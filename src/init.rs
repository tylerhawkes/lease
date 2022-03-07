#![allow(missing_docs)]
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

/// This trait is used to make constructing an InitPool easier and to
/// make it somewhat generic over what types are returned
pub trait Init {
  /// The type that call returns. This makes it so that if you need a mutable
  /// reference that you can get a lock
  type Output;

  /// Called internally by the library to get the output when returning it in a function
  fn call(&self) -> Self::Output;
}

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

  pub fn get(&self) -> Option<Lease<T>> {
    self.pool.get()
  }

  #[cfg(feature = "async")]
  pub async fn get_async(&self) -> Lease<T> {
    self.pool.get_async().await
  }

  /// Returns a [`Stream`](futures_core::Stream) of `Lease`es
  #[cfg(feature = "async")]
  pub fn stream(&self) -> crate::PoolStream<T> {
    crate::PoolStream::new(&self.pool)
  }

  pub fn get_or_new(&self) -> Lease<T>
  where
    I::Output: Into<T>,
  {
    self.pool.get_or_new(|| self.init.call().into())
  }

  pub async fn get_or_new_async(&self) -> Lease<T>
  where
    I::Output: Future<Output = T>,
  {
    match self.pool.get() {
      Some(lease) => lease,
      None => self.pool.insert_with_lease(self.init.call().await),
    }
  }

  pub fn get_or_try_new<E>(&self) -> Result<Lease<T>, E>
  where
    I::Output: Into<Result<T, E>>,
  {
    self.pool.get_or_try_new(|| self.init.call().into())
  }

  pub async fn get_or_try_new_async<E>(&self) -> Result<Lease<T>, E>
  where
    I::Output: Future<Output = Result<T, E>>,
  {
    match self.get() {
      Some(lease) => Ok(lease),
      None => Ok(self.pool.insert_with_lease(self.init.call().await?)),
    }
  }

  pub fn get_or_new_with_cap(&self, cap: usize) -> Option<Lease<T>>
  where
    I::Output: Into<T>,
  {
    self.pool.get_or_new_with_cap(cap, || self.init.call().into())
  }

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

  pub fn get_or_try_new_with_cap<E>(&self, cap: usize) -> Result<Option<Lease<T>>, E>
  where
    I::Output: Into<Result<T, E>>,
  {
    self.pool.get_or_try_new_with_cap(cap, || self.init.call().into())
  }

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

  pub fn len(&self) -> usize {
    self.pool.len()
  }

  pub fn clear(&self) {
    self.pool.clear();
  }

  pub fn is_empty(&self) -> bool {
    self.pool.is_empty()
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
}
