use super::{Lease, Pool};
use core::task::Waker;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct WaitingFutures {
  wakers: lockfree::map::Map<usize, Waker>,
  order: lockfree::queue::Queue<usize>,
}

impl WaitingFutures {
  pub fn new() -> Self {
    Self {
      wakers: lockfree::map::Map::new(),
      order: lockfree::queue::Queue::new(),
    }
  }
  pub fn insert(&self, id: usize, waker: Waker) {
    self.order.push(id);
    self.wakers.insert(id, waker);
  }
  pub fn remove(&self, id: usize) -> Option<()> {
    self.wakers.remove(&id).map(|_| ())
  }
  pub fn wake_next(&self) {
    while let Some(id) = self.order.pop() {
      if let Some(r) = self.wakers.remove(&id) {
        r.val().wake_by_ref();
        break;
      }
    }
  }
}

/// Implements the [`core::future::Future`] trait.
///
/// This is returned by the [`Pool::get_async()`](super::Pool::get_async()) method and will resolve once a [`Lease`](super::Lease) is ready.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub struct AsyncLease<T> {
  id: usize,
  pool: super::Pool<T>,
  first: bool,
  removed: bool,
}

impl<T> AsyncLease<T> {
  pub(crate) fn new(pool: &super::Pool<T>) -> Self {
    static ID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    Self {
      id: ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
      pool: pool.clone(),
      first: true,
      removed: true,
    }
  }
}

impl<T> Drop for AsyncLease<T> {
  fn drop(&mut self) {
    if !self.removed {
      self.pool.inner.waiting_futures.remove(self.id);
    }
  }
}

impl<'a, T> core::future::Future for AsyncLease<T> {
  type Output = super::Lease<T>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    #[allow(clippy::single_match_else)]
    match self.pool.get() {
      Some(t) => {
        if !self.first {
          self.pool.inner.waiting_futures.remove(self.id);
          self.removed = true;
        }
        Poll::Ready(t)
      }
      None => {
        self.first = false;
        self.pool.inner.waiting_futures.insert(self.id, cx.waker().clone());
        self.removed = false;
        Poll::Pending
      }
    }
  }
}

/// Implements the [`futures_core::Stream`] trait to return [`Lease`]es as they become available.
#[must_use]
pub struct PoolStream<T> {
  pool: Pool<T>,
  async_lease: AsyncLease<T>,
}

impl<T> PoolStream<T> {
  pub(crate) fn new(pool: &Pool<T>) -> Self {
    Self {
      pool: pool.clone(),
      async_lease: pool.get_async(),
    }
  }
}

impl<T> futures_core::Stream for PoolStream<T> {
  type Item = Lease<T>;

  #[allow(unsafe_code)]
  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    use core::future::Future;
    // # Safety
    // we never move pool so the references to it that are
    // inside of AsyncLease are always valid
    let this = self.get_mut();
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
