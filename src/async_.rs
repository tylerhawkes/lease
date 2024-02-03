use super::{Lease, Pool};
use futures_channel::oneshot::{Receiver, Sender};
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct WaitingFutures<T: Send + Sync + 'static> {
  order: lockfree::queue::Queue<Sender<Lease<T>>>,
}

impl<T: Send + Sync + 'static> WaitingFutures<T> {
  pub fn new() -> Self {
    Self {
      order: lockfree::queue::Queue::new(),
    }
  }
  pub fn insert(&self, sender: Sender<Lease<T>>) {
    // Push to wakers first so that it is available when returned by order.pop()
    self.order.push(sender);
  }
  pub fn wake_next(&self, lease: Lease<T>) {
    let mut lease = Some(lease);
    while let Some(sender) = self.order.pop() {
      if let Err(l) = sender.send(lease.take().unwrap()) {
        lease = Some(l);
        continue;
      }
      break;
    }
    if let Some(lease) = lease {
      lease.drop_without_recursion();
    }
  }
}

impl<T: Send + Sync + 'static> Default for WaitingFutures<T> {
  fn default() -> Self {
    Self::new()
  }
}

/// Implements the [`core::future::Future`] trait.
///
/// This is returned by the [`Pool::get()`](super::Pool::get()) method and will resolve once a [`Lease`] is ready.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub struct AsyncLease<T: Send + Sync + 'static> {
  receiver: Receiver<Lease<T>>,
}

impl<T: Send + Sync + 'static> AsyncLease<T> {
  pub(crate) fn new(receiver: Receiver<Lease<T>>) -> Self {
    Self { receiver }
  }
}

impl<T: Send + Sync + 'static> core::future::Future for AsyncLease<T> {
  type Output = super::Lease<T>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let receiver = &mut self.get_mut().receiver;

    match std::future::Future::poll(core::pin::Pin::new(receiver), cx) {
      Poll::Pending => Poll::Pending,
      Poll::Ready(r) => Poll::Ready(r.expect("Sender is never dropped until it has sent a message")),
    }
  }
}

/// Implements the [`futures_core::Stream`] trait to return [`Lease`]es as they become available.
#[must_use]
pub struct PoolStream<T: Send + Sync + 'static> {
  pool: Pool<T>,
  async_lease: AsyncLease<T>,
}

impl<T: Send + Sync + 'static> PoolStream<T> {
  pub(crate) fn new(pool: &Pool<T>) -> Self {
    Self {
      pool: pool.clone(),
      async_lease: pool.get_async(),
    }
  }
}

#[allow(unused)]
fn pool_stream_is_unpin() {
  fn unpin<T: core::marker::Unpin>() {}
  unpin::<PoolStream<usize>>();
}

impl<T: Send + Sync + 'static> futures_core::Stream for PoolStream<T> {
  type Item = Lease<T>;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    use core::future::Future;

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
