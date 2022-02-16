use core::cmp::Ordering;
use core::hash::Hasher;
use core::ops::Deref;
use parking_lot::Mutex;
use std::sync::Arc;

// Internal struct used to make the lockfree Set happy about Hash and Ord
#[must_use]
pub(crate) struct Wrapper<T>(pub(crate) Arc<Mutex<T>>);

impl<T> Deref for Wrapper<T> {
  type Target = Arc<Mutex<T>>;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<T> Wrapper<T> {
  pub(crate) fn new(t: T) -> Self {
    Self(Arc::new(Mutex::new(t)))
  }
}

impl<T> core::hash::Hash for Wrapper<T> {
  fn hash<H: Hasher>(&self, state: &mut H) {
    Arc::as_ptr(&self.0).hash(state);
  }
}

impl<T> core::cmp::PartialEq for Wrapper<T> {
  fn eq(&self, other: &Self) -> bool {
    Arc::as_ptr(&self.0) == Arc::as_ptr(&other.0)
  }
}

impl<T> core::cmp::Eq for Wrapper<T> {}

impl<T> core::cmp::PartialOrd for Wrapper<T> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Arc::as_ptr(&self.0).partial_cmp(&Arc::as_ptr(&other.0))
  }
}

impl<T> core::cmp::Ord for Wrapper<T> {
  fn cmp(&self, other: &Self) -> Ordering {
    Arc::as_ptr(&self.0).cmp(&Arc::as_ptr(&other.0))
  }
}
