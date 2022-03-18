//! # Lease
//! This crate provides a [`Pool`] struct that allows taking [`Lease`]es and using them.
//! When a [`Lease`] is dropped it is automatically returned to the pool.
//!
//! One nice thing about this api is that the lifetime of a [`Lease`] is not connected to the lifetime
//! of a [`Pool`] so they can be sent across threads.
//!
//! ## Features
//! * `async`
//!   - Enables the [`Pool::get_async()`] function. Async brings a little bit of overhead to getting
//!     leases so it is behind a feature.
//! * `stream`
//!   - Enables the `async` feature and adds the [`Pool::stream()`] function for creating a stream
//!     of leases that resolve anytime there is an available [`Lease`]

#![cfg_attr(not(test), deny(warnings, clippy::all, clippy::pedantic, clippy::cargo))]
#![allow(clippy::single_match_else)]
#![deny(unsafe_code, missing_docs)]
use self::wrapper::Wrapper;
use core::future::Future;
use core::iter::FromIterator;
use core::ops::{Deref, DerefMut};
use parking_lot::Mutex;
use std::sync::Arc;

#[cfg(feature = "async")]
pub use async_::{AsyncLease, PoolStream};
pub use init::InitPool;

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
  waiting_futures: async_::WaitingFutures,
}

impl<T> Default for PoolInner<T> {
  fn default() -> Self {
    Self {
      buffer: lockfree::set::Set::default(),
      #[cfg(feature = "async")]
      waiting_futures: async_::WaitingFutures::default(),
    }
  }
}

impl<T> Pool<T> {
  /// Creates a new empty `Pool`
  pub fn new() -> Self {
    Self::default()
  }

  /// Creates a new `Pool` with an initial size of `pool_size` by calling `init` `pool_size` times.
  pub fn with_initial_size(pool_size: usize, mut init: impl FnMut() -> T) -> Self {
    (0..pool_size).map(|_| init()).collect()
  }

  /// Creates a new `Pool` with an initial size of `pool_size` by calling `init` `pool_size` times.
  ///
  /// # Errors
  /// This returns the very first error returned by `init`
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
        waiting_futures: async_::WaitingFutures::new(),
      }),
    })
  }

  /// Tries to get a [`Lease`] if one is available. This function does not block.
  ///
  /// For an asynchronous version that returns when one is available use [`get_async()`](Self::get_async())
  #[must_use]
  pub fn get(&self) -> Option<Lease<T>> {
    self.inner.buffer.iter().find_map(|wrapper| Lease::from_arc_mutex(&wrapper, self))
  }

  fn get_or_len(&self) -> Result<Lease<T>, usize> {
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
  pub fn get_async(&self) -> AsyncLease<T> {
    AsyncLease::new(self)
  }

  /// Returns a [`Stream`](futures_core::Stream) of `Lease`es
  #[cfg(feature = "async")]
  pub fn stream(&self) -> PoolStream<T> {
    PoolStream::new(self)
  }

  /// For the asynchronous version of this function see [`get_or_new_async()`](Self::get_or_new_async())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  pub fn get_or_new(&self, init: impl FnOnce() -> T) -> Lease<T> {
    self.get().map_or_else(|| self.insert_with_lease(init()), |t| t)
  }

  /// Asynchronous version of [`get_or_new()`](Self::get_or_new())
  ///
  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  // TODO: make this take a FnOnce() -> Future
  pub async fn get_or_new_async(&self, init: impl Future<Output = T>) -> Lease<T> {
    match self.get() {
      Some(lease) => lease,
      None => self.insert_with_lease(init.await),
    }
  }

  /// For the asynchronous version of this function see [`get_or_try_new_async()`](Self::get_or_try_new_async())
  ///
  /// Tries to get an existing [`Lease`] if available and if not tries to create a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  ///
  /// # Errors
  /// Returns an error if `init` errors
  pub fn get_or_try_new<E>(&self, init: impl FnOnce() -> Result<T, E>) -> Result<Lease<T>, E> {
    match self.get() {
      None => Ok(self.insert_with_lease(init()?)),
      Some(l) => Ok(l),
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
  // TODO: make this take a FnOnce() -> Future
  pub async fn get_or_try_new_async<E>(&self, init: impl Future<Output = Result<T, E>>) -> Result<Lease<T>, E> {
    match self.get() {
      None => Ok(self.insert_with_lease(init.await?)),
      Some(l) => Ok(l),
    }
  }

  /// For the asynchronous version of this function see [`get_or_new_with_cap_async()`](Self::get_or_new_with_cap_async())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  pub fn get_or_new_with_cap(&self, cap: usize, init: impl FnOnce() -> T) -> Option<Lease<T>> {
    match self.get_or_len() {
      Ok(t) => Some(t),
      Err(len) => (len < cap).then(|| self.insert_with_lease(init())),
    }
  }

  /// Asynchronous version of [`get_or_new_with_cap()`](Self::get_or_new_with_cap())
  ///
  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  // TODO: make this take a FnOnce() -> Future
  pub async fn get_or_new_with_cap_async(&self, cap: usize, init: impl Future<Output = T>) -> Option<Lease<T>> {
    match self.get_or_len() {
      Ok(t) => Some(t),
      Err(len) => {
        if len < cap {
          return None;
        }
        Some(self.insert_with_lease(init.await))
      }
    }
  }

  /// For the asynchronous version of this function see [`get_or_try_new_with_cap_async()`](Self::get_or_try_new_with_cap_async())
  ///
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  ///
  /// # Errors
  /// Returns an error if `init` errors
  pub fn get_or_try_new_with_cap<E>(&self, cap: usize, init: impl FnOnce() -> Result<T, E>) -> Result<Option<Lease<T>>, E> {
    match self.get_or_len() {
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
  /// Just like [`get_or_try_new()`](Self::get_or_try_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  ///
  /// # Errors
  /// Returns an error if `init` errors
  // TODO: make this take a FnOnce() -> Future
  pub async fn get_or_try_new_with_cap_async<E>(
    &self,
    cap: usize,
    init: impl Future<Output = Result<T, E>>,
  ) -> Result<Option<Lease<T>>, E> {
    match self.get_or_len() {
      Ok(t) => Ok(Some(t)),
      Err(len) => {
        if len >= cap {
          return Ok(None);
        }
        Ok(Some(self.insert_with_lease(init.await?)))
      }
    }
  }

  /// Returns the size of the pool
  #[must_use]
  pub fn len(&self) -> usize {
    self.inner.buffer.iter().count()
  }

  /// Sets the size of the pool to zero
  ///
  /// This will disassociate all current [`Lease`]es and when they go out of scope the objects they're
  /// holding will be dropped
  pub fn clear(&self) {
    self.inner.buffer.iter().for_each(|g| {
      let wrapper: &Wrapper<_> = &g;
      self.inner.buffer.remove(wrapper);
    });
  }

  /// Resizes the pool to `pool_size`
  ///
  /// `init` is only called if the pool needs to grow.
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

  /// Just like the [`Extend`](core::iter::Extend) trait but doesn't require self to be mutable
  pub fn extend<I: IntoIterator<Item = T>>(&self, iter: I) {
    self.inner.buffer.extend(iter.into_iter().map(Wrapper::new));
  }

  /// Adds new item to this [`Pool`]
  /// Use [`insert_with_lease()`](Self::insert_with_lease()) if you need a [`Lease`] back
  pub fn insert(&self, t: T) {
    let wrapper = Wrapper::new(t);
    self
      .inner
      .buffer
      .insert(wrapper)
      .unwrap_or_else(|_| unreachable!("Each new wrapper should be unique"));
    self.notify();
  }

  /// Adds new item to this [`Pool`] and returns a [`Lease`] that is ready to be used.
  /// Use [`insert()`](Self::insert()) if you don't need a [`Lease`] back
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
  pub fn available(&self) -> usize {
    self.inner.buffer.iter().filter(|b| !b.is_locked()).count()
  }

  /// Returns true if there are no items being stored.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.inner.buffer.iter().next().is_none()
  }

  /// Disassociates the [`Lease`] from this [`Pool`]
  pub fn disassociate(&self, lease: &Lease<T>) {
    // This is the one unfortunate place where wrapping the arc is more costly.
    // Since this function shouldn't be called in a tight loop, the clone should be fine
    self.inner.buffer.remove(&Wrapper(lease.mutex.clone()));
  }

  #[cfg_attr(not(feature = "async"), allow(clippy::unused_self))]
  fn notify(&self) {
    #[cfg(feature = "async")]
    self.inner.waiting_futures.wake_next();
  }
}

impl<T: Default> Pool<T> {
  /// Just like [`get_or_new()`](Self::get_or_new()) but uses [`Default::default()`] as the `init` function
  pub fn get_or_default(&self) -> Lease<T> {
    self.get_or_new(T::default)
  }

  /// Just like [`get_or_new_with_cap()`](Self::get_or_new_with_cap()) but uses [`Default::default()`] as the `init` function
  #[must_use]
  pub fn get_or_default_with_cap(&self, cap: usize) -> Option<Lease<T>> {
    self.get_or_new_with_cap(cap, T::default)
  }

  /// Just like [`resize()`](Self::resize()) but uses [`Default::default()`] as the `init` function
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
        waiting_futures: async_::WaitingFutures::new(),
      }),
    }
  }
}

#[allow(unused)]
fn assert_lease_is_static() {
  fn is_static<T: 'static>() {};
  is_static::<Lease<()>>();
}

/// Represents a lease from a [`Pool`]
///
/// When the lease is dropped it is returned to the pool for re-use
///
/// This struct implements [`core::ops::Deref`] and [`core::ops::DerefMut`] so those traits can be used
/// to get access to the underlying data.
///
/// It also implements [`core::convert::AsRef`] and [`core::convert::AsMut`] for all types that the underlying type
/// does so those can also be used to get access to the underlying data.  
#[must_use]
pub struct Lease<T> {
  mutex: Arc<Mutex<T>>,
  #[cfg(feature = "async")]
  pool: Pool<T>,
}

impl<T> Drop for Lease<T> {
  fn drop(&mut self) {
    #[allow(unsafe_code)]
    unsafe {
      // # Safety
      // We had a guard when we created this Lease, so now we must force_unlock it.
      self.mutex.force_unlock();
    }
    #[cfg(feature = "async")]
    self.pool.notify();
  }
}

impl<T> Lease<T> {
  fn from_arc_mutex(arc: &Arc<Mutex<T>>, #[allow(unused)] pool: &Pool<T>) -> Option<Self> {
    arc.try_lock().map(|guard| {
      std::mem::forget(guard);
      Self {
        mutex: arc.clone(),
        #[cfg(feature = "async")]
        pool: pool.clone(),
      }
    })
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
    debug_assert!(self.mutex.is_locked());
    #[allow(unsafe_code)]
    // # Safety
    // We had a guard that we forgot when we created this Lease, so it is ok to get a reference to the underlying data.
    unsafe {
      &*self.mutex.data_ptr()
    }
  }
}

impl<T> DerefMut for Lease<T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    debug_assert!(self.mutex.is_locked());
    #[allow(unsafe_code)]
    // # Safety
    // We had a guard that we forgot when we created this Lease, so it is ok to get a reference to the underlying data.
    unsafe {
      &mut *self.mutex.data_ptr()
    }
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
  fn bytes<B: AsRef<[u8]>>() {}
  bytes::<Lease<Vec<u8>>>();
  fn send_sync_static_clone<F: Send + Sync + 'static + Clone>() {}
  send_sync_static_clone::<Pool<u8>>();
  send_sync_static_clone::<init::InitPool<u8, init::InitFn<u8>>>();
}
