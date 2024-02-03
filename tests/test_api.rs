use lease::*;

#[test]
fn test_pool() {
  let pool = (1..=2).collect::<Pool<u8>>();
  println!("{:?}", pool);
  assert_eq!(pool.available(), 2);
  assert_eq!(pool.len(), 2);
  let l1 = pool.try_get().unwrap();
  println!("{:?}, {:?}", pool, l1);
  assert!(*l1 & 0x3 > 0);
  assert_eq!(pool.available(), 1);
  let l2 = pool.try_get().unwrap();
  let l2_val = *l2;
  assert_ne!(*l2, *l1);
  assert_eq!(pool.available(), 0);
  assert!(pool.try_get().is_none());
  let l3 = pool.try_get_or_new(|| 3);
  assert_eq!(*l3, 3);
  assert_eq!(pool.available(), 0);
  assert_eq!(pool.len(), 3);
  assert!(pool.try_get().is_none());

  drop(l2);
  assert_eq!(pool.available(), 1);
  let l2 = pool.try_get().unwrap();
  assert_eq!(*l2, l2_val);
  assert_eq!(pool.available(), 0);
  assert!(pool.try_get().is_none());
  pool.resize(1, || unreachable!());
  assert_eq!(pool.available(), 0);
  assert_eq!(pool.len(), 1);

  // the following checks may be dependent on the order of checks in try_into_locked_pool()
  // let l2 = pool.get_or_default();
  // let (pool, error) = pool.try_into_locked_pool().unwrap_err();
  // assert_eq!(error, PoolConversionError::CheckedOutLeases { count: 2 });
  // drop(l2);
  // let (pool, error) = pool.try_into_locked_pool().unwrap_err();
  // assert_eq!(error, PoolConversionError::CheckedOutLeases { count: 1 });
  // // get rid of only checked out lease
  // drop(l1);
  let pool = pool.try_into_locked_pool().unwrap().try_into_pool().unwrap();
  let pool2 = pool.clone();
  let pool3 = pool.clone();
  let (pool, error) = pool.try_into_locked_pool().unwrap_err();
  assert_eq!(error, PoolConversionError::OtherCopies { count: 2 });
  // get rid of other copy
  drop(pool3);
  let (pool, error) = pool.try_into_locked_pool().unwrap_err();
  assert_eq!(error, PoolConversionError::OtherCopies { count: 1 });
  // get rid of other copy
  drop(pool2);
  // passes all checks
  let _ = pool.try_into_locked_pool().unwrap();
  let (_, error) = Pool::<()>::default().try_into_locked_pool().unwrap_err();
  assert_eq!(error, PoolConversionError::EmptyPool);
}

#[cfg(feature = "async")]
#[tokio::test]
async fn test_async_pool() {
  println!("staring test_async_pool");
  let pool = (1..=1).collect::<Pool<u8>>();
  println!("collected pool");
  assert_eq!(pool.available(), 1);
  assert_eq!(pool.len(), 1);
  println!("getting lease with pool.get()");
  let l1 = pool.get().await;
  println!("got lease from pool.get()");
  assert_eq!(*l1, 1);
  assert_eq!(pool.available(), 0);
  let start = std::time::Instant::now();
  tokio::spawn(async move {
    println!("waiting to drop original l1 {:?}", start.elapsed());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    println!("dropping original l1 {:?}", start.elapsed());
    drop(l1);
  });
  println!("awaiting new lease {:?}", start.elapsed());
  let _l1 = pool.get().await;
  println!("got new lease {:?}", start.elapsed());

  let sleep = tokio::time::sleep(std::time::Duration::from_millis(20));
  tokio::pin!(sleep);
  tokio::select! {
    _ = &mut sleep => {},
    _ = pool.get() => panic!("Pool should not be available!"),
  }
}

#[cfg(feature = "async")]
#[tokio::test]
async fn test_async_concurrent() {
  use futures::StreamExt;
  use std::collections::VecDeque;

  let pool = (1..=3).collect::<Pool<i32>>();
  let mut waiting1 = futures::stream::FuturesUnordered::new();
  let mut waiting2 = futures::stream::FuturesUnordered::new();
  let mut leases = VecDeque::new();
  while let Some(l) = pool.try_get() {
    leases.push_back(l);
  }

  for _ in 0..10 {
    waiting1.push(pool.get());
    waiting2.push(pool.get());
  }

  let stream = pool.stream();
  tokio::pin!(stream);
  let mut next = stream.next();
  let mut stream_count = 0_u8;
  // wait for stream and timeout simultaneously. If timed out then drop one from leases, otherwise push one onto leases. Go until waiting is empty.
  for i in 0_usize..60 {
    println!("running loop {i}");
    let sleep = tokio::time::sleep(std::time::Duration::from_millis(10));
    tokio::pin!(sleep);
    tokio::select! {
      biased;
      l = waiting1.next(), if !waiting1.is_empty() => {
        println!("got waiting1. pushing to leases");
        if let Some(l) = l {leases.push_back(l)}
      },
      l = waiting2.next(), if !waiting2.is_empty() => {
        println!("got waiting2. pushing to leases");
         if let Some(l) = l {leases.push_back(l)}
      },
      s = &mut next, if waiting1.is_empty() && waiting2.is_empty() => {
        println!("got next from stream: {s:?}");
        leases.push_back(s.unwrap());
        next = stream.next();
        stream_count += 1;
      },
      _ = &mut sleep => {
        let lease = leases.pop_front();
        println!("popping lease {lease:?}");
        if lease.is_none() {
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

pub fn test_send_sync_static() {
  fn send_sync<T: Send + Sync + 'static>() {}
  send_sync::<Pool<()>>();
  send_sync::<Lease<()>>();
}
