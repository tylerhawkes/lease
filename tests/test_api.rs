use lease::*;

#[test]
fn test_pool() {
  let pool = (1..=2).collect::<Pool<u8>>();
  println!("{:?}", pool);
  assert_eq!(pool.available(), 2);
  assert_eq!(pool.len(), 2);
  let l1 = pool.get().unwrap();
  println!("{:?}", pool);
  assert_eq!(*l1, 1);
  assert_eq!(pool.available(), 1);
  let l2 = pool.get().unwrap();
  assert_eq!(*l2, 2);
  assert_eq!(pool.available(), 0);
  assert!(pool.get().is_none());
  let l3 = pool.get_or_new(|| 3);
  assert_eq!(*l3, 3);
  assert_eq!(pool.available(), 0);
  assert_eq!(pool.len(), 3);
  assert!(pool.get().is_none());

  drop(l2);
  assert_eq!(pool.available(), 1);
  let l2 = pool.get().unwrap();
  assert_eq!(*l2, 2);
  assert_eq!(pool.available(), 0);
  assert!(pool.get().is_none());
  pool.resize(1, || unreachable!());
  assert_eq!(pool.available(), 0);
  assert_eq!(pool.len(), 1);
}

#[cfg(feature = "async")]
#[tokio::test]
async fn test_async_pool() {
  let pool = (1..=1).collect::<Pool<u8>>();
  assert_eq!(pool.available(), 1);
  assert_eq!(pool.len(), 1);
  let l1 = pool.get_async().await;
  assert_eq!(*l1, 1);
  assert_eq!(pool.available(), 0);
  let sleep = tokio::time::sleep(std::time::Duration::from_millis(20));
  tokio::pin!(sleep);
  tokio::select! {
    _ = &mut sleep => {},
    _ = pool.get_async() => panic!("Pool should not be available!"),
  }
}

#[cfg(feature = "async")]
#[tokio::test]
async fn test_async_pool_concurrent() {
  use futures::StreamExt;
  use std::collections::VecDeque;

  let pool = (1..=3).collect::<Pool<i32>>();
  let mut waiting1 = futures::stream::FuturesUnordered::new();
  let mut waiting2 = futures::stream::FuturesUnordered::new();
  let mut leases = VecDeque::new();
  while let Some(l) = pool.get() {
    leases.push_back(l);
  }

  for _ in 0..10 {
    waiting1.push(pool.get_async());
    waiting2.push(pool.get_async());
  }

  let mut stream = Box::pin(pool.stream());
  let mut next = stream.next();
  let mut stream_count = 0_u8;
  // wait for stream and timeout simultaneously. If timed out then drop one from leases, otherwise push one onto leases. Go until waiting is empty.
  for i in 0_usize.. {
    if i > 60 {
      break;
    }
    let sleep = tokio::time::sleep(std::time::Duration::from_millis(10));
    tokio::pin!(sleep);
    tokio::select! {
      biased;
      l = waiting1.next(), if !waiting1.is_empty() => match l {
        Some(l) => leases.push_back(l),
        None => {},
      },
      l = waiting2.next(), if !waiting2.is_empty() => match l {
        Some(l) => leases.push_back(l),
        None => {},
      },
      s = &mut next, if waiting1.is_empty() && waiting2.is_empty() => {
        leases.push_back(s.unwrap());
        next = stream.next();
        stream_count += 1;
      },
      _ = &mut sleep => {
        if leases.pop_front().is_none() {
          break;
        }
      }
    }
  }
  dbg!(&waiting1.len());
  dbg!(&waiting2.len());
  dbg!(&pool);
  assert!(waiting1.is_empty());
  assert!(waiting2.is_empty());
  assert_eq!(stream_count, 10);
}
