use core::cmp::Ordering;
use core::hash::Hasher;
use core::ops::Deref;
use parking_lot::Mutex;
use std::sync::Arc;

// Internal struct used to make the lockfree Set happy about Hash and Ord
#[must_use]
#[repr(transparent)]
pub(crate) struct Wrapper<T>(pub(crate) Arc<Mutex<T>>);

impl<T> Clone for Wrapper<T> {
  fn clone(&self) -> Self {
    Self(Arc::clone(&self.0))
  }
}

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
  #[inline]
  fn as_usize(&self) -> usize {
    Arc::as_ptr(&self.0) as usize
  }
}

impl<T> core::hash::Hash for Wrapper<T> {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.as_usize().hash(state);
  }
}

impl<T> core::cmp::PartialEq for Wrapper<T> {
  fn eq(&self, other: &Self) -> bool {
    self.as_usize() == other.as_usize()
  }
}

impl<T> core::cmp::Eq for Wrapper<T> {}

impl<T> core::cmp::PartialOrd for Wrapper<T> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<T> core::cmp::Ord for Wrapper<T> {
  fn cmp(&self, other: &Self) -> Ordering {
    self.as_usize().cmp(&other.as_usize())
  }
}
