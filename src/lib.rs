//! # Lease
//! This crate provides a [`Pool`] struct that allows taking [`Lease`]es and using them.
//! When a [`Lease`] is dropped it is automatically returned to the pool.
//!
//! One nice thing about this api is that the lifetime of a [`Lease`] is not connected to the lifetime
//! of a [`Pool`] so they can be sent across threads.
//!
//! There is also an `InitPool` that ensures that all new leases are created the same way
//!
//! A `LockedPool` only allows calling functions that do not add or remove any `Lease`s
//!
//! ## Features
//! * `async`
//!   - Enables the [`Pool::get()`] function. Async brings a little bit of overhead to getting
//!     leases so it is behind a feature.
//!   - Enables the [`Pool::stream()`] function that allows getting a stream of leases as they become available

#![cfg_attr(not(test), deny(warnings, clippy::all, clippy::pedantic, clippy::cargo))]
#![allow(clippy::single_match_else)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]
use self::wrapper::Wrapper;
use core::future::Future;
use core::iter::FromIterator;
use core::ops::{Deref, DerefMut};
use std::sync::Arc;

#[cfg(feature = "async")]
pub use async_::{AsyncLease, PoolStream};
pub use init::InitPool;
use parking_lot::lock_api::ArcMutexGuard;
use parking_lot::RawMutex;

#[cfg(feature = "async")]
mod async_;
pub mod init;
mod wrapper;

/// A pool of objects of type `T` that can be leased out.
///
/// This struct implements [`std::iter::FromIterator`] so you can create it from an iterator
/// by calling [`std::iter::Iterator::collect()`]
#[must_use]
pub struct Pool<T> {
  inner: Arc<PoolInner<T>>,
}

struct PoolInner<T> {
  buffer: lockfree::set::Set<Wrapper<T>>,
  #[cfg(feature = "async")]
  waiting_futures: Arc<async_::WaitingFutures<T>>,
}

impl<T> Default for PoolInner<T> {
  #[inline]
  fn default() -> Self {
    Self {
      buffer: lockfree::set::Set::default(),
      #[cfg(feature = "async")]
      waiting_futures: Arc::default(),
    }
  }
}

impl<T: Send + Sync + 'static> Pool<T> {
  /// Turns this pool into an [`InitPool`] using the provided initialization function
  pub fn into_init_pool<I: init::Init>(self, init: I) -> InitPool<T, I> {
    InitPool::new_from_pool(self, init)
  }
}

impl<T> Pool<T> {
  /// Turns this [`Pool`] into a [`LockedPool`]
  ///
  /// # Errors
  /// Will return an error if any of the variants in [`PoolConversionError`] apply
  pub fn try_into_locked_pool(mut self) -> Result<LockedPool<T>, (Pool<T>, PoolConversionError)> {
    let len = self.len();
    if len == 0 {
      return Err((self, PoolConversionError::EmptyPool));
    }
    // let available = self.available();
    // if available != len {
    //   return Err((self, PoolConversionError::CheckedOutLeases { count: len - available }));
    // }
    let Some(_) = Arc::get_mut(&mut self.inner) else {
      let count = Arc::strong_count(&self.inner) - 1;
      return Err((self, PoolConversionError::OtherCopies { count }));
    };

    Ok(LockedPool { pool: self, len })
  }

  /// Creates a new empty [`Pool`]
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Creates a new [`Pool`] with an initial size of `pool_size` by calling `init` `pool_size` times.
  #[inline]
  pub fn with_initial_size(pool_size: usize, mut init: impl FnMut() -> T) -> Self {
    (0..pool_size).map(|_| init()).collect()
  }

  /// Creates a new [`Pool`] with an initial size of `pool_size` by calling `init` `pool_size` times.
  ///
  /// # Errors
  /// This returns the very first error returned by `init`
  #[inline]
  pub fn try_with_initial_size<E>(pool_size: usize, mut init: impl FnMut() -> Result<T, E>) -> Result<Self, E> {
    let buffer = lockfree::set::Set::new();
    for _ in 0..pool_size {
      buffer
        .insert(Wrapper::new(init()?))
        .unwrap_or_else(|_| unreachable!("Each new wrapper should be unique"));
    }
    Ok(Self {
      inner: Arc::new(PoolInner {
        buffer,
        #[cfg(feature = "async")]
        waiting_futures: Arc::default(),
      }),
    })
  }

  /// Tries to get a [`Lease`] if one is available. This function does not block.
  ///
  /// For an asynchronous version that returns when one is available use [`get()`](Self::get())
  #[must_use]
  #[inline]
  pub fn try_get(&self) -> Option<Lease<T>> {
    self.inner.buffer.iter().find_map(|wrapper| Lease::from_arc_mutex(&wrapper, self))
  }

  #[inline]
  fn try_get_or_len(&self) -> Result<Lease<T>, usize> {
    let mut count = 0;
    let lease = self
      .inner
      .buffer
      .iter()
      .inspect(|_| count += 1)
      .find_map(|wrapper| Lease::from_arc_mutex(&wrapper, self));
    lease.ok_or(count)
  }

  /// Returns a future that resolves to a [`Lease`] when one is available
  ///
  /// Reqires the `async` feature to be enabled because it requires extra memory
  #[cfg(feature = "async")]
  #[inline]
  pub async fn get(&self) -> Lease<T> {
    self.get_async().await
  }

  // for internal use when we need to know the type of a future
  #[cfg(feature = "async")]
  #[inline]
  fn get_async(&self) -> AsyncLease<T> {
    let (sender, receiver) = futures_channel::oneshot::channel();
    self.inner.waiting_futures.insert(sender);
    // call this after inserting the sender to avoid a race condition
    if let Some(lease) = self.try_get() {
      self.inner.waiting_futures.wake_next(lease);
    }
    AsyncLease::<T>::new(receiver)
  }

  /// Returns a [`Stream`](futures_core::Stream) of [`Lease`]es
  #[cfg(feature = "async")]
  #[inline]
  pub fn stream(&self) -> impl futures_core::Stream<Item = Lease<T>> {
    PoolStream::new(self)
  }

  /// For the asynchronous version of this function see [`get_or_new()`](Self::get_or_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  #[inline]
  pub fn try_get_or_new(&self, init: impl FnOnce() -> T) -> Lease<T> {
    self.try_get().unwrap_or_else(|| self.insert_with_lease(init()))
  }

  /// Asynchronous version of [`try_get_or_new()`](Self::get_or_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  #[inline]
  pub async fn get_or_new<FUT: Future<Output = T>, FN: FnOnce() -> FUT>(&self, init: FN) -> Lease<T> {
    match self.try_get() {
      Some(lease) => lease,
      None => self.insert_with_lease(init().await),
    }
  }

  /// For the asynchronous version of this function see [`get_or_try_new()`](Self::get_or_try_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not tries to create a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  ///
  /// # Errors
  /// Returns an error if `init` errors
  #[inline]
  pub fn try_get_or_try_new<E>(&self, init: impl FnOnce() -> Result<T, E>) -> Result<Lease<T>, E> {
    match self.try_get() {
      Some(l) => Ok(l),
      None => Ok(self.insert_with_lease(init()?)),
    }
  }

  /// Asynchronous version of [`get_or_try_new()`](Self::get_or_try_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not tries to create a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  ///
  /// # Errors
  /// Returns an error if `init` errors
  #[inline]
  pub async fn get_or_try_new<E, FUT: Future<Output = Result<T, E>>, FN: FnOnce() -> FUT>(&self, init: FN) -> Result<Lease<T>, E> {
    match self.try_get() {
      None => Ok(self.insert_with_lease(init().await?)),
      Some(l) => Ok(l),
    }
  }

  /// For the asynchronous version of this function see [`get_or_new_with_cap()`](Self::get_or_new_with_cap())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  #[inline]
  pub fn try_get_or_new_with_cap(&self, cap: usize, init: impl FnOnce() -> T) -> Option<Lease<T>> {
    match self.try_get_or_len() {
      Ok(t) => Some(t),
      Err(len) => (len < cap).then(|| self.insert_with_lease(init())),
    }
  }

  /// Asynchronous version of [`get_or_new_with_cap()`](Self::get_or_new_with_cap())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then it waits for an open lease.
  #[cfg(feature = "async")]
  #[inline]
  pub async fn get_or_new_with_cap<FUT: Future<Output = T>, FN: FnOnce() -> FUT>(&self, cap: usize, init: FN) -> Lease<T> {
    match self.try_get_or_len() {
      Ok(t) => t,
      Err(len) => {
        if len >= cap {
          return self.get().await;
        }
        self.insert_with_lease(init().await)
      }
    }
  }

  /// For the asynchronous version of this function see [`get_or_try_new_with_cap()`](Self::get_or_try_new_with_cap())
  ///
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  ///
  /// # Errors
  /// Returns an error if `init` errors
  #[inline]
  pub fn try_get_or_try_new_with_cap<E>(&self, cap: usize, init: impl FnOnce() -> Result<T, E>) -> Result<Option<Lease<T>>, E> {
    match self.try_get_or_len() {
      Ok(t) => Ok(Some(t)),
      Err(len) => {
        if len >= cap {
          return Ok(None);
        }
        Ok(Some(self.insert_with_lease(init()?)))
      }
    }
  }

  /// Asynchronous version of [`get_or_try_new_with_cap()`](Self::get_or_try_new_with_cap())
  ///
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then it waits for an open lease.
  ///
  /// # Errors
  /// Returns an error if `init` errors
  #[cfg(feature = "async")]
  #[inline]
  pub async fn get_or_try_new_with_cap<E, FUT: Future<Output = Result<T, E>>, FN: FnOnce() -> FUT>(
    &self,
    cap: usize,
    init: FN,
  ) -> Result<Lease<T>, E> {
    match self.try_get_or_len() {
      Ok(t) => Ok(t),
      Err(len) => {
        if len >= cap {
          return Ok(self.get().await);
        }
        Ok(self.insert_with_lease(init().await?))
      }
    }
  }

  /// Returns the size of the pool
  #[inline]
  #[must_use]
  pub fn len(&self) -> usize {
    self.inner.buffer.iter().count()
  }

  /// Sets the size of the pool to zero
  ///
  /// This will disassociate all current [`Lease`]es and when they go out of scope the objects they're
  /// holding will be dropped
  #[inline]
  pub fn clear(&self) {
    self.inner.buffer.iter().for_each(|g| {
      let wrapper: &Wrapper<_> = &g;
      self.inner.buffer.remove(wrapper);
    });
  }

  /// Resizes the pool to `pool_size`
  ///
  /// `init` is only called if the pool needs to grow.
  #[inline]
  pub fn resize(&self, pool_size: usize, mut init: impl FnMut() -> T) {
    let set = &self.inner.buffer;
    self.inner.buffer.iter().skip(pool_size).for_each(|g| {
      self.inner.buffer.remove(&*g);
    });
    set.extend((self.len()..pool_size).map(|_| Wrapper::new(init())));
  }

  /// Resizes the pool to `pool_size`
  ///
  /// `init` is only called if the pool needs to grow.
  ///
  /// # Errors
  /// This returns the very first error returned by `init`
  #[inline]
  pub fn try_resize<E>(&self, pool_size: usize, mut init: impl FnMut() -> Result<T, E>) -> Result<(), E> {
    let set = &self.inner.buffer;
    set.iter().skip(pool_size).for_each(|g| {
      set.remove(&*g);
    });
    for _ in self.len()..pool_size {
      set
        .insert(Wrapper::new(init()?))
        .unwrap_or_else(|_| unreachable!("Each new wrapper should be unique"));
    }
    Ok(())
  }

  /// Just like the [`Extend`] trait but doesn't require self to be mutable
  #[inline]
  pub fn extend<I: IntoIterator<Item = T>>(&self, iter: I) {
    self.inner.buffer.extend(iter.into_iter().map(Wrapper::new));
  }

  /// Adds new item to this [`Pool`]
  /// Use [`insert_with_lease()`](Self::insert_with_lease()) if you need a [`Lease`] back
  #[inline]
  pub fn insert(&self, t: T) {
    let lease = self.insert_with_lease(t);
    self.notify(lease);
  }

  /// Adds new item to this [`Pool`] and returns a [`Lease`] that is ready to be used.
  /// Use [`insert()`](Self::insert()) if you don't need a [`Lease`] back
  #[inline]
  pub fn insert_with_lease(&self, t: T) -> Lease<T> {
    let wrapper = Wrapper::new(t);
    let lease = Lease::from_arc_mutex(&wrapper, self).unwrap_or_else(|| unreachable!("Wrapper is unlocked when new"));
    self
      .inner
      .buffer
      .insert(wrapper)
      .unwrap_or_else(|_| unreachable!("Each new wrapper should be unique"));
    lease
  }

  /// Returns the number of currently available [`Lease`]es. Even if the return is non-zero, calling [`get()`](Self::get())
  /// immediately afterward can still fail if multiple threads have access to this pool.
  #[must_use]
  #[inline]
  pub fn available(&self) -> usize {
    self.inner.buffer.iter().filter(|b| !b.is_locked()).count()
  }

  /// Returns true if there are no items being stored.
  #[must_use]
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.inner.buffer.iter().next().is_none()
  }

  /// Disassociates the [`Lease`] from this [`Pool`]
  #[inline]
  pub fn disassociate(&self, lease: &Lease<T>) {
    // This is the one unfortunate place where wrapping the arc is more costly.
    // Since this function shouldn't be called in a tight loop, the clone should be fine
    self.inner.buffer.remove(&Wrapper(ArcMutexGuard::mutex(lease.guard()).clone()));
  }

  #[cfg_attr(not(feature = "async"), allow(clippy::unused_self))]
  #[inline]
  fn notify(&self, lease: Lease<T>) {
    #[cfg(feature = "async")]
    self.inner.waiting_futures.wake_next(lease);
    #[cfg(not(feature = "async"))]
    drop(lease);
  }
}

impl<T: Default + Send + Sync + 'static> Pool<T> {
  /// Just like [`get_or_new()`](Self::get_or_new()) but uses [`Default::default()`] as the `init` function
  #[inline]
  pub fn get_or_default(&self) -> Lease<T> {
    self.try_get_or_new(T::default)
  }

  /// Just like [`get_or_new_with_cap()`](Self::get_or_new_with_cap()) but uses [`Default::default()`] as the `init` function
  #[must_use]
  #[inline]
  pub fn get_or_default_with_cap(&self, cap: usize) -> Option<Lease<T>> {
    self.try_get_or_new_with_cap(cap, T::default)
  }

  /// Just like [`resize()`](Self::resize()) but uses [`Default::default()`] as the `init` function
  #[inline]
  pub fn resize_default(&self, pool_size: usize) {
    self.resize(pool_size, T::default);
  }
}

impl<T> Default for Pool<T> {
  fn default() -> Self {
    Self {
      inner: Arc::new(PoolInner::default()),
    }
  }
}

impl<T> core::fmt::Debug for Pool<T> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    struct ListDebugger<'a, T> {
      set: &'a lockfree::set::Set<Wrapper<T>>,
    }

    impl<T> core::fmt::Debug for ListDebugger<'_, T> {
      fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.set.iter().map(|m| !m.is_locked())).finish()
      }
    }

    let mut s = f.debug_struct("Pool");
    s.field("len", &self.len())
      .field("available", &self.available())
      .field("availabilities", &ListDebugger { set: &self.inner.buffer })
      .finish()
  }
}

impl<T> Clone for Pool<T> {
  fn clone(&self) -> Self {
    Self { inner: self.inner.clone() }
  }
}

impl<T> FromIterator<T> for Pool<T> {
  fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
    Self {
      inner: Arc::new(PoolInner {
        buffer: iter.into_iter().map(Wrapper::new).collect(),
        #[cfg(feature = "async")]
        waiting_futures: Arc::default(),
      }),
    }
  }
}

/// A pool of objects of type `T` that can be leased out.
///
/// This is a wrapper of pool that once created doesn't allow
/// any changes to the number of [`Lease`]s that the [`Pool`] contains
#[derive(Default, Clone)]
pub struct LockedPool<T> {
  pool: Pool<T>,
  len: usize,
}

impl<T> LockedPool<T> {
  /// Turns this [`Pool`] into a [`LockedPool`]
  ///
  /// # Errors
  /// Will return an error if there are other copies of this [`LockedPool`]
  pub fn try_into_pool(mut self) -> Result<Pool<T>, (LockedPool<T>, PoolConversionError)> {
    let Some(_) = Arc::get_mut(&mut self.pool.inner) else {
      let count = Arc::strong_count(&self.pool.inner) - 1;
      return Err((self, PoolConversionError::OtherCopies { count }));
    };

    Ok(self.pool)
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
    self.pool.stream()
  }

  /// Returns the size of the pool
  #[inline]
  #[must_use]
  pub fn len(&self) -> usize {
    self.len
  }

  /// Returns true if there are no items being stored.
  #[must_use]
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.len == 0
  }

  /// Returns the number of currently available [`Lease`]es. Even if the return is non-zero, calling [`get()`](Self::get())
  /// immediately afterward can still fail if multiple threads have access to this pool.
  #[must_use]
  #[inline]
  pub fn available(&self) -> usize {
    self.pool.available()
  }
}

impl<T> core::fmt::Debug for LockedPool<T> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    self.pool.fmt(f)
  }
}

/// A set of errors giving more context into why a [`LockedPool`] couldn't be created
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolConversionError {
  /// The Pool is empty and therefore useless since the number of [`Lease`]s can't be changed
  EmptyPool,
  // /// Any of the leases are checked out
  // CheckedOutLeases {
  //   /// The number of active [`Lease`]s held by the original [`Pool`]
  //   count: usize,
  // },
  /// The [`Pool`] has other copies that could still mutate the original [`Pool`] that would be held by [`LockedPool`]
  OtherCopies {
    /// The number of other active clones of the original [`Pool`]
    count: usize,
  },
}

impl core::fmt::Display for PoolConversionError {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::EmptyPool => f.write_str("Pool is empty"),
      Self::OtherCopies { count } => write!(f, "Pool has {count} other copies"),
      // Self::CheckedOutLeases { count } => write!(f, "Pool has {count} checked out leases"),
    }
  }
}

impl std::error::Error for PoolConversionError {}

/// Represents a lease from a [`Pool`]
///
/// When the lease is dropped it is returned to the pool for re-use
///
/// This struct implements [`Deref`] and [`DerefMut`] so those traits can be used
/// to get access to the underlying data.
///
/// It also implements [`AsRef`] and [`AsMut`] for all types that the underlying type
/// does so those can also be used to get access to the underlying data.  
#[must_use]
pub struct Lease<T> {
  // this needs to be an option so we can move ownership in drop if needed.
  guard: Option<ArcMutexGuard<RawMutex, T>>,
  #[cfg(feature = "async")]
  waiting_futures: Arc<async_::WaitingFutures<T>>,
}

impl<T> Drop for Lease<T> {
  fn drop(&mut self) {
    #[cfg(feature = "async")]
    {
      if let Some(guard) = self.guard.take() {
        let lease = Self {
          guard: Some(guard),
          waiting_futures: self.waiting_futures.clone(),
        };
        self.waiting_futures.wake_next(lease);
      }
    }
    #[cfg(not(feature = "async"))]
    {
      self.guard.take();
    }
  }
}

impl<T> Lease<T> {
  #[inline]
  fn from_arc_mutex(arc: &Wrapper<T>, #[allow(unused)] pool: &Pool<T>) -> Option<Self> {
    arc.0.try_lock_arc().map(|guard| Self {
      guard: Some(guard),
      #[cfg(feature = "async")]
      waiting_futures: pool.inner.waiting_futures.clone(),
    })
  }
  fn guard(&self) -> &ArcMutexGuard<RawMutex, T> {
    self.guard.as_ref().unwrap()
  }
  fn guard_mut(&mut self) -> &mut ArcMutexGuard<RawMutex, T> {
    self.guard.as_mut().unwrap()
  }
  #[cfg(feature = "async")]
  fn drop_without_recursion(mut self) {
    self.guard.take();
  }
}

impl<T: core::fmt::Debug> core::fmt::Debug for Lease<T> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    self.deref().fmt(f)
  }
}

impl<T> Deref for Lease<T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    self.guard()
  }
}

impl<T> DerefMut for Lease<T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    self.guard_mut()
  }
}

impl<T, U: ?Sized> AsRef<U> for Lease<T>
where
  T: AsRef<U>,
{
  fn as_ref(&self) -> &U {
    self.deref().as_ref()
  }
}

impl<T, U: ?Sized> AsMut<U> for Lease<T>
where
  T: AsMut<U>,
{
  fn as_mut(&mut self) -> &mut U {
    self.deref_mut().as_mut()
  }
}

#[allow(unused)]
fn asserts() {
  fn bytes<B: AsRef<[u8]> + AsMut<[u8]>>() {}
  fn send_sync_static_clone<F: Send + 'static + Clone>() {}
  bytes::<Lease<Vec<u8>>>();
  send_sync_static_clone::<Pool<u8>>();
  send_sync_static_clone::<init::InitPool<u8, init::InitFn<u8>>>();
}
