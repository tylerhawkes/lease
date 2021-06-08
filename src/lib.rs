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
#![deny(unsafe_code, missing_docs)]
use core::cmp::Ordering;
use core::ops::{Deref, DerefMut};
use parking_lot::{Mutex, RwLock};
use std::iter::FromIterator;
use std::sync::Arc;

#[cfg(feature = "async")]
pub use async_lease::AsyncLease;
#[cfg(feature = "stream")]
pub use pool_stream::PoolStream;

/// A pool of objects of type `T` that can be leased out.
///
/// This struct implements [`std::iter::FromIterator`] so you can create it from an iterator
/// by calling [`std::iter::Iterator::collect()`]
///
/// There are some non-asynchronous locks used internally, but all of their critical code paths
/// are so short that the author doesn't consider them to be blocking.
#[must_use]
#[derive(Clone)]
pub struct Pool<T> {
  buffer: Arc<RwLock<Vec<Arc<Mutex<T>>>>>,
  #[cfg(feature = "async")]
  waiting_futures: Arc<Mutex<linked_hash_map::LinkedHashMap<usize, core::task::Waker>>>,
}

impl<T> Pool<T> {
  /// Creates a new `Pool` with an initial size of `pool_size` by calling `init` `pool_size` times.
  pub fn new(pool_size: usize, mut init: impl FnMut() -> T) -> Self {
    let buffer = (0..pool_size).map(|_| Arc::new(Mutex::new(init()))).collect::<Vec<_>>();
    Self {
      buffer: Arc::new(RwLock::new(buffer)),
      #[cfg(feature = "async")]
      waiting_futures: Arc::default(),
    }
  }

  /// Returns a future that resolves to a [`Lease`] when one is available
  #[cfg(feature = "async")]
  pub fn get_async(&self) -> async_lease::AsyncLease<T> {
    async_lease::AsyncLease::new(self)
  }

  /// Tries to get a [`Lease`] if one is available. This function does not block.
  #[allow(unused_mut)]
  #[must_use]
  pub fn get(&self) -> Option<Lease<T>> {
    self.buffer.read().iter().find_map(Lease::from_arc_mutex).map(|mut l| {
      #[cfg(feature = "async")]
      {
        l.waiting_futures = Some(self.waiting_futures.clone());
      }
      l
    })
  }

  /// Returns a struct that implements the [`futures_core::Stream`] trait.
  #[cfg(feature = "stream")]
  pub fn stream(&self) -> pool_stream::PoolStream<'_, T> {
    pool_stream::PoolStream::new(self)
  }

  /// Tries to get an existing [`Lease`] if available and if not returns a new one that has been added to the pool.
  ///
  /// Calling this method repeatedly can cause the pool size to increase without bound.
  #[allow(unused_mut)]
  pub fn get_or_new(&self, init: impl FnOnce() -> T) -> Lease<T> {
    self.get().map_or_else(
      || {
        let mut lease = Lease::from(init());
        self.append(&lease);
        #[cfg(feature = "async")]
        {
          lease.waiting_futures = Some(self.waiting_futures.clone());
        }
        lease
      },
      |t| t,
    )
  }

  /// Just like [`get_or_new()`](Self::get_or_new()) but caps the size of the pool. Once [`len()`](Self::len()) == `cap` then `None` is returned.
  #[allow(unused_mut)]
  pub fn get_or_new_with_cap(&self, cap: usize, init: impl FnOnce() -> T) -> Option<Lease<T>> {
    if let Some(t) = self.get() {
      Some(t)
    } else {
      if self.len() >= cap {
        return None;
      }
      let mut lease = Lease::from(init());
      self.append(&lease);
      #[cfg(feature = "async")]
      {
        lease.waiting_futures = Some(self.waiting_futures.clone());
      }
      Some(lease)
    }
  }

  /// Returns the size of the pool
  #[must_use]
  pub fn len(&self) -> usize {
    self.buffer.read().len()
  }

  /// Sets the size of the pool to zero
  ///
  /// This will disassociate all current [`Lease`]es and when they go out of scope the objects they're
  /// holding will be dropped
  pub fn clear(&self) {
    self.buffer.write().clear()
  }

  /// Resizes the pool to `pool_size`
  ///
  /// `init` is only called if the pool needs to grow.
  pub fn resize(&self, pool_size: usize, mut init: impl FnMut() -> T) {
    let mut vec = self.buffer.write();
    let buffer_len = vec.len();
    match pool_size.cmp(&buffer_len) {
      Ordering::Equal => {}
      Ordering::Less => vec.truncate(pool_size),
      Ordering::Greater => vec.extend((buffer_len..pool_size).map(|_| Arc::new(Mutex::new(init())))),
    }
  }

  fn append(&self, lease: &Lease<T>) {
    if self.buffer.read().iter().any(|a| Arc::ptr_eq(a, &lease.mutex)) {
      return;
    }
    self.buffer.write().push(lease.mutex.clone())
  }

  /// Returns the number of currently available [`Lease`]es. Even if the return is non-zero calling [`get()`](Self::get())
  /// immediately afterward can still fail if multiple.
  #[must_use]
  pub fn available(&self) -> usize {
    self.buffer.read().iter().filter(|b| !b.is_locked()).count()
  }

  /// Returns true if there are no items being stored.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.buffer.read().is_empty()
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
    self.resize(pool_size, T::default)
  }
}

impl<T> Default for Pool<T> {
  fn default() -> Self {
    Self::new(0, || unreachable!())
  }
}

impl<T> core::fmt::Debug for Pool<T> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    struct ListDebugger<I: Iterator<Item = bool> + Clone> {
      i: I,
    }

    impl<I: Iterator<Item = bool> + Clone> core::fmt::Debug for ListDebugger<I> {
      fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.i.clone()).finish()
      }
    }

    let mut s = f.debug_struct("Pool");
    s.field("len", &self.len()).field("available", &self.available());
    s.field(
      "availabilities",
      &ListDebugger {
        i: self.buffer.read().iter().map(|m| !m.is_locked()),
      },
    );
    s.finish()
  }
}

impl<T> FromIterator<T> for Pool<T> {
  fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
    Self {
      buffer: Arc::new(RwLock::new(iter.into_iter().map(Mutex::new).map(Arc::new).collect())),
      #[cfg(feature = "async")]
      waiting_futures: Arc::default(),
    }
  }
}

#[cfg(feature = "stream")]
mod pool_stream {
  use super::{async_lease::AsyncLease, Lease, Pool};
  use std::pin::Pin;
  use std::task::{Context, Poll};

  /// Implements the [`futures_core::Stream`] trait to return [`Lease`]es as they become available.
  ///
  /// This has a lifetime that is connected to the [`Pool`] it was created from.
  #[must_use]
  pub struct PoolStream<'a, T> {
    pool: &'a Pool<T>,
    async_lease: AsyncLease<'a, T>,
    _unpinned: core::marker::PhantomPinned,
  }

  impl<'a, T> PoolStream<'a, T> {
    pub(crate) fn new(pool: &'a Pool<T>) -> Self {
      Self {
        async_lease: pool.get_async(),
        pool,
        _unpinned: core::marker::PhantomPinned,
      }
    }
  }

  impl<'a, T> futures_core::Stream for PoolStream<'a, T> {
    type Item = Lease<T>;

    #[allow(unsafe_code)]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
      use core::future::Future;
      // # Safety
      // we never move pool so the references to it that are
      // inside of AsyncLease are always valid
      let this = unsafe { self.get_unchecked_mut() };
      let async_lease = Pin::new(&mut this.async_lease);
      match async_lease.poll(cx) {
        Poll::Ready(l) => {
          this.async_lease = this.pool.get_async();
          Poll::Ready(Some(l))
        }
        Poll::Pending => Poll::Pending,
      }
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
  waiting_futures: Option<Arc<Mutex<linked_hash_map::LinkedHashMap<usize, core::task::Waker>>>>,
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
    {
      if let Some(waiting_futures) = &self.waiting_futures {
        let mut guard = waiting_futures.lock();
        if let Some((_, waker)) = guard.pop_front() {
          waker.wake();
        }
      }
    }
  }
}

impl<T> Lease<T> {
  fn from_arc_mutex(arc: &Arc<Mutex<T>>) -> Option<Self> {
    #[allow(unsafe_code)]
    arc.try_lock().map(|guard| {
      std::mem::forget(guard);
      Self {
        mutex: arc.clone(),
        #[cfg(feature = "async")]
        waiting_futures: None,
      }
    })
  }
}

impl<T: core::fmt::Debug> core::fmt::Debug for Lease<T> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    self.deref().fmt(f)
  }
}

impl<T> From<T> for Lease<T> {
  fn from(t: T) -> Self {
    // The mutex will unlock successfully so this unwrap will never fail
    Self::from_arc_mutex(&Arc::new(Mutex::new(t))).unwrap()
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

impl<T, U> AsRef<U> for Lease<T>
where
  T: AsRef<U>,
{
  fn as_ref(&self) -> &U {
    self.deref().as_ref()
  }
}

impl<T, U> AsMut<U> for Lease<T>
where
  T: AsMut<U>,
{
  fn as_mut(&mut self) -> &mut U {
    self.deref_mut().as_mut()
  }
}

#[cfg(feature = "async")]
mod async_lease {
  use std::pin::Pin;
  use std::task::{Context, Poll};

  static ID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

  /// Implements the [`core::future::Future`] trait.
  ///
  /// This is returned by the [`Pool::get_async()`](super::Pool::get_async()) method and will resolve once a [`Lease`](super::Lease) is ready.
  #[must_use]
  pub struct AsyncLease<'a, T> {
    id: usize,
    pool: &'a super::Pool<T>,
    first: bool,
    removed: bool,
  }

  impl<'a, T> AsyncLease<'a, T> {
    pub(crate) fn new(pool: &'a super::Pool<T>) -> Self {
      Self {
        id: ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        pool,
        first: true,
        removed: true,
      }
    }
  }

  impl<'a, T> Drop for AsyncLease<'a, T> {
    fn drop(&mut self) {
      if !self.removed {
        self.pool.waiting_futures.lock().remove(&self.id);
      }
    }
  }

  impl<'a, T> core::future::Future for AsyncLease<'a, T> {
    type Output = super::Lease<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
      #[allow(clippy::single_match_else)]
      match self.pool.get() {
        Some(t) => {
          if !self.first {
            self.pool.waiting_futures.lock().remove(&self.id);
            self.removed = true;
          }
          Poll::Ready(t)
        }
        None => {
          self.first = false;
          self.pool.waiting_futures.lock().insert(self.id, cx.waker().clone());
          self.removed = false;
          Poll::Pending
        }
      }
    }
  }
}
