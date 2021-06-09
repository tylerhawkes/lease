use core::cmp::Ordering;
use core::hash::Hasher;
use core::ops::Deref;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

// Internal struct used to make the lockfree Set happy about Hash and Ord
#[must_use]
pub(crate) struct Wrapper<T>(Mutex<T>, usize);

impl<T> Deref for Wrapper<T> {
  type Target = Mutex<T>;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<T> Wrapper<T> {
  pub(crate) fn new(t: T) -> Self {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    Self(Mutex::new(t), COUNT.fetch_add(1, Relaxed))
  }
}

impl<T> core::hash::Hash for Wrapper<T> {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.1.hash(state)
  }
}

impl<T> core::cmp::PartialEq for Wrapper<T> {
  fn eq(&self, other: &Self) -> bool {
    self.1 == other.1
  }
}

impl<T> core::cmp::Eq for Wrapper<T> {}

impl<T> core::cmp::PartialOrd for Wrapper<T> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    self.1.partial_cmp(&other.1)
  }
}

impl<T> core::cmp::Ord for Wrapper<T> {
  fn cmp(&self, other: &Self) -> Ordering {
    self.1.cmp(&other.1)
  }
}
