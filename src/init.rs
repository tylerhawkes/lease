//! This module contains an initializing pool
//!
//! Several of the [`Pool`] functions aren't available on it
//! because the purpose of this pool is that all items are guaranteed to be
//! initialized in the same way.
//!
//! The Init\[Try\]Fn\[Async\] structs are really just for convenience to not have
//! to specify so many things.

#![allow(clippy::module_name_repetitions)]
use crate::{Lease, Pool};
use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

/// A pool of objects of type `T` that can be leased out. Just like a [`Pool`], but
/// it also carries an initializer that ensures each item is consistently initialized
pub struct InitPool<T: Send + Sync + 'static, I: Init> {
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
pub struct InitFn<T>(Box<dyn Fn() -> T + Send + Sync + 'static>);
impl<T> Init for InitFn<T> {
  type Output = T;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<F, T> From<F> for InitFn<T>
where
  F: Send + Sync + 'static + Fn() -> T,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

/// Holds an initializing function that returns a [`Future`]
pub struct InitFnAsync<T>(Box<dyn Fn() -> Pin<Box<dyn Future<Output = T> + Send + 'static>> + Send>);
impl<T> Init for InitFnAsync<T> {
  type Output = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<T, F> From<F> for InitFnAsync<T>
where
  F: Fn() -> Pin<Box<dyn Future<Output = T> + Send + 'static>> + Send + 'static,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

/// Holds an initializing function that can return an error
pub struct InitTryFn<T, E>(Box<dyn Fn() -> Result<T, E> + Send>);
impl<T, E> Init for InitTryFn<T, E> {
  type Output = Result<T, E>;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<F, T, E> From<F> for InitTryFn<T, E>
where
  F: Fn() -> Result<T, E> + Send + 'static,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

/// Holds an initializing function that return a [`Future`] that can error
#[allow(clippy::type_complexity)]
pub struct InitTryFnAsync<T, E>(Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'static>> + Send>);
impl<T, E> Init for InitTryFnAsync<T, E> {
  type Output = Pin<Box<dyn Future<Output = Result<T, E>>>>;

  fn call(&self) -> Self::Output {
    (self.0)()
  }
}

impl<T, E, F> From<F> for InitTryFnAsync<T, E>
where
  F: Fn() -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'static>> + Send + 'static,
{
  fn from(f: F) -> Self {
    Self(Box::new(f))
  }
}

impl<T: Send + Sync + 'static, I: Init> InitPool<T, I> {
  /// Turns this [`Pool`] into a [`LockedPool`](super::LockedPool)
  ///
  /// # Errors
  /// Will return an error if any of the variants in [`PoolConversionError`](super::PoolConversionError) apply
  pub fn try_into_locked_pool(self) -> Result<super::LockedPool<T>, (Pool<T>, super::PoolConversionError)> {
    self.pool.try_into_locked_pool()
  }

  /// Creates a new empty [`Pool`]
  #[inline]
  pub fn new(init: I) -> Self {
    Self {
      pool: Pool::default(),
      init: Arc::new(init),
    }
  }

  /// Creates a new empty [`Pool`] that will be initialized using `init`
  #[inline]
  pub fn new_from<O: Into<I>>(init: O) -> Self {
    Self::new(init.into())
  }

  pub(super) fn new_from_pool(pool: Pool<T>, init: I) -> Self {
    Self {
      pool,
      init: Arc::new(init),
    }
  }

  /// Returns the output of the initializer so that consumers can get an initialized value
  /// that isn't tied to the pool
  #[allow(clippy::must_use_candidate)]
  #[inline]
  pub fn init(&self) -> I::Output {
    self.init.call()
  }

  /// Tries to get a [`Lease`] if one is available. This function does not block.
  ///
  /// For an asynchronous version that returns when one is available use [`get()`](Self::get())
  #[must_use]
  #[inline]
  pub fn try_get(&self) -> Option<Lease<T>> {
    self.pool.try_get()
  }

  /// Returns a future that resolves to a [`Lease`] when one is available
  ///
  /// Reqires the `async` feature to be enabled because it requires extra memory
  #[cfg(feature = "async")]
  #[inline]
  pub async fn get(&self) -> Lease<T> {
    self.pool.get().await
  }

  /// Returns a [`Stream`](futures_core::Stream) of [`Lease`]es
  #[cfg(feature = "async")]
  #[inline]
  pub fn stream(&self) -> impl futures_core::Stream<Item = Lease<T>> {
    crate::PoolStream::new(&self.pool)
  }

  /// For the asynchronous version of this function see [`get_or_new()`](Self::get_or_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  #[inline]
  pub fn try_get_or_new(&self) -> Lease<T>
  where
    I::Output: Into<T>,
  {
    self.pool.try_get_or_new(|| self.init.call().into())
  }

  /// Asynchronous version of [`try_get_or_new()`](Self::try_get_or_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  #[inline]
  pub async fn get_or_new(&self) -> Lease<T>
  where
    I::Output: Future<Output = T>,
  {
    self.pool.get_or_new(|| self.init.call()).await
  }

  /// For the asynchronous version of this function see [`get_or_try_new()`](Self::get_or_try_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not tries to create a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  #[inline]
  pub fn try_get_or_try_new<E>(&self) -> Result<Lease<T>, E>
  where
    I::Output: Into<Result<T, E>>,
  {
    self.pool.try_get_or_try_new(|| self.init.call().into())
  }

  /// Asynchronous version of [`try_get_or_try_new()`](Self::try_get_or_try_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not tries to create a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  #[inline]
  pub async fn get_or_try_new<E>(&self) -> Result<Lease<T>, E>
  where
    I::Output: Future<Output = Result<T, E>>,
  {
    self.pool.get_or_try_new(|| self.init.call()).await
  }

  /// For the asynchronous version of this function see [`get_or_new_with_cap()`](Self::get_or_new_with_cap())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  #[must_use]
  pub fn try_get_or_new_with_cap(&self, cap: usize) -> Option<Lease<T>>
  where
    I::Output: Into<T>,
  {
    self.pool.try_get_or_new_with_cap(cap, || self.init.call().into())
  }

  /// Asynchronous version of [`get_or_new_with_cap()`](Self::get_or_new_with_cap())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then it waits for an open lease.
  #[inline]
  #[cfg(feature = "async")]
  pub async fn get_or_new_with_cap(&self, cap: usize) -> Lease<T>
  where
    I::Output: Future<Output = T>,
  {
    self.pool.get_or_new_with_cap(cap, || self.init.call()).await
  }

  /// For the asynchronous version of this function see [`get_or_try_new_with_cap()`](Self::get_or_try_new_with_cap())
  ///
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  #[inline]
  pub fn try_get_or_try_new_with_cap<E>(&self, cap: usize) -> Result<Option<Lease<T>>, E>
  where
    I::Output: Into<Result<T, E>>,
  {
    self.pool.try_get_or_try_new_with_cap(cap, || self.init.call().into())
  }

  /// Asynchronous version of [`get_or_try_new_with_cap()`](Self::get_or_try_new_with_cap())
  ///
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then it waits for an open lease.
  ///
  /// # Errors
  /// Returns an error if the stored initializer errors
  #[inline]
  #[cfg(feature = "async")]
  pub async fn get_or_try_new_with_cap<E>(&self, cap: usize) -> Result<Lease<T>, E>
  where
    I::Output: Future<Output = Result<T, E>>,
  {
    self.pool.get_or_try_new_with_cap(cap, || self.init.call()).await
  }

  /// Returns the size of the pool
  #[must_use]
  #[inline]
  pub fn len(&self) -> usize {
    self.pool.len()
  }

  /// Sets the size of the pool to zero
  ///
  /// This will disassociate all current [`Lease`]es and when they go out of scope the objects they're
  /// holding will be dropped
  #[inline]
  pub fn clear(&self) {
    self.pool.clear();
  }

  /// Returns the number of currently available [`Lease`]es. Even if the return is non-zero, calling [`get()`](Self::get())
  /// immediately afterward can still fail if multiple threads have access to this pool.
  #[must_use]
  #[inline]
  pub fn available(&self) -> usize {
    self.pool.available()
  }

  /// Returns true if there are no items being stored.
  #[must_use]
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.pool.is_empty()
  }

  /// Disassociates the [`Lease`] from this [`InitPool`]
  #[inline]
  pub fn disassociate(&self, lease: &Lease<T>) {
    self.pool.disassociate(lease);
  }
}

impl<T: Send + Sync + 'static, I: Init> Clone for InitPool<T, I> {
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
  use futures::*;

  #[test]
  fn test_sync() {
    let init_pool = InitPool::<u8, InitFn<_>>::new_from(|| 42_u8);
    assert!(init_pool.try_get().is_none());
    let lease = init_pool.try_get_or_new();
    assert_eq!(*lease, 42);
  }

  #[tokio::test]
  async fn test() {
    let init_pool = InitPool::<u8, InitFnAsync<_>>::new_from(|| async { 42_u8 }.boxed());
    assert!(init_pool.try_get().is_none());
    assert_eq!(*init_pool.get_or_new().await, 42);
  }

  #[test]
  fn test_try_sync() {
    let init_pool = InitPool::<u8, InitTryFn<_, core::convert::Infallible>>::new_from(|| Ok(42_u8));
    assert!(init_pool.try_get().is_none());
    assert_eq!(*init_pool.try_get_or_try_new().unwrap(), 42);
  }

  #[tokio::test]
  async fn test_try_async() {
    let init_pool = InitPool::<u8, InitTryFnAsync<_, core::convert::Infallible>>::new_from(|| async { Ok(42_u8) }.boxed());
    assert!(init_pool.try_get().is_none());
    assert_eq!(*init_pool.get_or_try_new().await.unwrap(), 42);
  }

  #[tokio::test]
  async fn test_clone() {
    let init_pool = InitPool::<u8, InitTryFnAsync<_, core::convert::Infallible>>::new_from(|| {
      async { Ok::<_, core::convert::Infallible>(42_u8) }.boxed()
    });
    let init_pool_2 = init_pool.clone();
    assert!(init_pool.try_get().is_none());
    assert_eq!(init_pool.len(), 0);
    assert_eq!(init_pool_2.len(), 0);
    let value = init_pool.get_or_try_new().await.unwrap();
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
