//! This module contains an initializing pool
//!
//! Several of the [`Pool`](crate::Pool) functions aren't available on it
//! because the purpose of this pool is that all items are guaranteed to be
//! initialized in the same way.
//!
//! The Init[Try]Fn[Async] structs are really just for convenience to not have
//! to specify so many things.

#![allow(clippy::module_name_repetitions)]
use crate::{Lease, Pool};
use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

/// A pool of objects of type `T` that can be leased out. Just like a [`Pool`](crate::Pool), but
/// it also carries an initializer that ensures each item is consistently initialized
pub struct InitPool<T, I: Init> {
  pool: Pool<T>,
  init: Arc<I>,
}

/// This trait is used to make constructing an [`InitPool`] easier and to
/// make it somewhat generic over what types are returned
pub trait Init {
  /// The type that call returns. This makes it so that if you need a mutable
  /// reference that you can take a lock
  type Output;

  /// Called internally by the library to get the output when returning it in a function
  fn call(&self) -> Self::Output;
}

/// Holds an initializing function to be called by [`InitPool`]
pub struct InitFn<T>(Box<dyn Fn() -> T + Send + Sync>);
impl<T> Init for InitFn<T> {
  type Output = T;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<F, T> From<F> for InitFn<T>
where
  F: Fn() -> T + Send + Sync + 'static,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

/// Holds an initializing function that returns a [`Future`]
pub struct InitFnAsync<T>(Box<dyn Fn() -> Pin<Box<dyn Future<Output = T> + Send + Sync + 'static>> + Send + Sync>);
impl<T> Init for InitFnAsync<T> {
  type Output = Pin<Box<dyn Future<Output = T> + Send + Sync + 'static>>;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<T, F> From<F> for InitFnAsync<T>
where
  F: Fn() -> Pin<Box<dyn Future<Output = T> + Send + Sync + 'static>> + Send + Sync + 'static,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

/// Holds an initializing function that can return an error
pub struct InitTryFn<T, E>(Box<dyn Fn() -> Result<T, E> + Send + Sync>);
impl<T, E> Init for InitTryFn<T, E> {
  type Output = Result<T, E>;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<F, T, E> From<F> for InitTryFn<T, E>
where
  F: Fn() -> Result<T, E> + Send + Sync + 'static,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

/// Holds an initializing function that return a [`Future`] that can error
#[allow(clippy::type_complexity)]
pub struct InitTryFnAsync<T, E>(Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + Sync + 'static>> + Send + Sync>);
impl<T, E> Init for InitTryFnAsync<T, E> {
  type Output = Pin<Box<dyn Future<Output = Result<T, E>>>>;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<T, E, F> From<F> for InitTryFnAsync<T, E>
where
  F: Fn() -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + Sync + 'static>> + Send + Sync + 'static,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

impl<T, I: Init> InitPool<T, I> {
  /// Creates a new empty `Pool`
  pub fn new(init: I) -> Self {
    Self {
      pool: Pool::default(),
      init: Arc::new(init),
    }
  }
  /// Creates a new empty `Pool` that will be initialized using `init`
  pub fn new_from<O: Into<I>>(init: O) -> Self {
    Self::new(init.into())
  }

  /// Returns the output of the initializer so that consumers can get an initialized value
  /// that isn't tied to the pool
  #[allow(clippy::must_use_candidate)]
  pub fn init(&self) -> I::Output {
    self.init.call()
  }

  /// Tries to get a [`Lease`] if one is available. This function does not block.
  ///
  /// For an asynchronous version that returns when one is available use [`get_async()`](Self::get_async())
  #[must_use]
  pub fn get(&self) -> Option<Lease<T>> {
    self.pool.get()
  }

  /// Returns a future that resolves to a [`Lease`] when one is available
  ///
  /// Reqires the `async` feature to be enabled because it requires extra memory
  #[cfg(feature = "async")]
  pub fn get_async(&self) -> crate::AsyncLease<T> {
    self.pool.get_async()
  }

  /// Returns a [`Stream`](futures_core::Stream) of `Lease`es
  #[cfg(feature = "async")]
  pub fn stream(&self) -> crate::PoolStream<T> {
    crate::PoolStream::new(&self.pool)
  }

  /// For the asynchronous version of this function see [`get_or_new_async()`](Self::get_or_new_async())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  pub fn get_or_new(&self) -> Lease<T>
  where
    I::Output: Into<T>,
  {
    self.pool.get_or_new(|| self.init.call().into())
  }

  /// Asynchronous version of [`get_or_new()`](Self::get_or_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  pub async fn get_or_new_async(&self) -> Lease<T>
  where
    I::Output: Future<Output = T>,
  {
    match self.pool.get() {
      Some(lease) => lease,
      None => self.pool.insert_with_lease(self.init.call().await),
    }
  }

  /// For the asynchronous version of this function see [`get_or_try_new_async()`](Self::get_or_try_new_async())
  ///
  /// Tries to get an existing [`Lease`] if available and if not tries to create a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  pub fn get_or_try_new<E>(&self) -> Result<Lease<T>, E>
  where
    I::Output: Into<Result<T, E>>,
  {
    self.pool.get_or_try_new(|| self.init.call().into())
  }

  /// Asynchronous version of [`get_or_try_new()`](Self::get_or_try_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not tries to create a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  pub async fn get_or_try_new_async<E>(&self) -> Result<Lease<T>, E>
  where
    I::Output: Future<Output = Result<T, E>>,
  {
    match self.get() {
      Some(lease) => Ok(lease),
      None => Ok(self.pool.insert_with_lease(self.init.call().await?)),
    }
  }

  /// For the asynchronous version of this function see [`get_or_new_with_cap_async()`](Self::get_or_new_with_cap_async())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  #[must_use]
  pub fn get_or_new_with_cap(&self, cap: usize) -> Option<Lease<T>>
  where
    I::Output: Into<T>,
  {
    self.pool.get_or_new_with_cap(cap, || self.init.call().into())
  }

  /// Asynchronous version of [`get_or_new_with_cap()`](Self::get_or_new_with_cap())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  pub async fn get_or_new_with_cap_async(&self, cap: usize) -> Option<Lease<T>>
  where
    I::Output: Future<Output = T>,
  {
    match self.pool.get_or_len() {
      Ok(t) => Some(t),
      Err(len) => {
        if len < cap {
          return None;
        }
        Some(self.pool.insert_with_lease(self.init.call().await))
      }
    }
  }

  /// For the asynchronous version of this function see [`get_or_try_new_with_cap_async()`](Self::get_or_try_new_with_cap_async())
  ///
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  pub fn get_or_try_new_with_cap<E>(&self, cap: usize) -> Result<Option<Lease<T>>, E>
  where
    I::Output: Into<Result<T, E>>,
  {
    self.pool.get_or_try_new_with_cap(cap, || self.init.call().into())
  }

  /// Asynchronous version of [`get_or_try_new_with_cap()`](Self::get_or_try_new_with_cap())
  ///
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  pub async fn get_or_try_new_with_cap_async<E>(&self, cap: usize) -> Result<Option<Lease<T>>, E>
  where
    I::Output: Future<Output = Result<T, E>>,
  {
    match self.pool.get_or_len() {
      Ok(t) => Ok(Some(t)),
      Err(len) => {
        if len >= cap {
          return Ok(None);
        }
        Ok(Some(self.pool.insert_with_lease(self.init.call().await?)))
      }
    }
  }

  /// Returns the size of the pool
  #[must_use]
  pub fn len(&self) -> usize {
    self.pool.len()
  }

  /// Sets the size of the pool to zero
  ///
  /// This will disassociate all current [`Lease`]es and when they go out of scope the objects they're
  /// holding will be dropped
  pub fn clear(&self) {
    self.pool.clear();
  }

  /// Returns the number of currently available [`Lease`]es. Even if the return is non-zero, calling [`get()`](Self::get())
  /// immediately afterward can still fail if multiple threads have access to this pool.
  #[must_use]
  pub fn available(&self) -> usize {
    self.pool.available()
  }

  /// Returns true if there are no items being stored.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.pool.is_empty()
  }

  /// Disassociates the [`Lease`] from this [`InitPool`]
  pub fn disassociate(&self, lease: &Lease<T>) {
    self.pool.disassociate(lease);
  }
}

impl<T, I: Init> Clone for InitPool<T, I> {
  fn clone(&self) -> Self {
    Self {
      pool: self.pool.clone(),
      init: self.init.clone(),
    }
  }
}

#[cfg(test)]
mod tests {

  use super::*;
  #[test]
  fn test_sync() {
    let init_pool = InitPool::<u8, InitFn<_>>::new_from(|| 42_u8);
    assert!(init_pool.get().is_none());
    assert_eq!(*init_pool.get_or_new(), 42);
  }

  #[tokio::test]
  async fn test_async() {
    let init_pool =
      InitPool::<u8, InitFnAsync<_>>::new_from(|| Box::pin(async { 42_u8 }) as Pin<Box<dyn Future<Output = u8> + Send + Sync + 'static>>);
    assert!(init_pool.get().is_none());
    assert_eq!(*init_pool.get_or_new_async().await, 42);
  }

  #[test]
  fn test_try_sync() {
    let init_pool = InitPool::<u8, InitTryFn<_, core::convert::Infallible>>::new_from(|| Ok(42_u8));
    assert!(init_pool.get().is_none());
    assert_eq!(*init_pool.get_or_try_new().unwrap(), 42);
  }

  #[tokio::test]
  async fn test_try_async() {
    let init_pool = InitPool::<u8, InitTryFnAsync<_, core::convert::Infallible>>::new_from(|| {
      Box::pin(async { Ok(42_u8) }) as Pin<Box<dyn Future<Output = _> + Send + Sync + 'static>>
    });
    assert!(init_pool.get().is_none());
    assert_eq!(*init_pool.get_or_try_new_async().await.unwrap(), 42);
  }

  #[tokio::test]
  async fn test_clone() {
    let init_pool = InitPool::<u8, InitTryFnAsync<_, core::convert::Infallible>>::new_from(|| {
      Box::pin(async { Ok(42_u8) }) as Pin<Box<dyn Future<Output = _> + Send + Sync + 'static>>
    });
    let init_pool_2 = init_pool.clone();
    assert!(init_pool.get().is_none());
    assert_eq!(init_pool.len(), 0);
    assert_eq!(init_pool_2.len(), 0);
    let value = init_pool.get_or_try_new_async().await.unwrap();
    assert_eq!(*value, 42);
    assert_eq!(init_pool.len(), 1);
    assert_eq!(init_pool_2.len(), 1);
    assert_eq!(init_pool.available(), 0);
    assert_eq!(init_pool_2.available(), 0);
    drop(value);
    assert_eq!(init_pool.available(), 1);
    assert_eq!(init_pool_2.available(), 1);
  }
}
