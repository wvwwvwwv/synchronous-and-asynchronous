use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::thread;

use saa::gate::{Error, State};
use saa::lock::Mode;
use saa::{Barrier, Gate, Lock, Pager, Semaphore};

#[test]
fn lock() {
    let lock = Lock::default();

    lock.lock_sync();

    assert!(!lock.try_lock());
    assert!(!lock.try_share());

    assert!(!lock.release_share());
    assert!(lock.release_lock());
}

#[test]
fn barrier() {
    let barrier = Arc::new(Barrier::with_count(8));

    let mut threads = Vec::new();

    for _ in 0..8 {
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            for _ in 0..4 {
                barrier.wait_sync();
            }
        }));
    }

    for thread in threads {
        thread.join().unwrap();
    }

    assert_eq!(barrier.count(Relaxed), 8);
}

#[test]
fn semaphore() {
    let semaphore = Semaphore::default();

    semaphore.acquire_many_sync(Semaphore::MAX_PERMITS - 1);

    assert!(semaphore.try_acquire());
    assert!(!semaphore.try_acquire());

    assert!(semaphore.release());
    assert!(!semaphore.release_many(Semaphore::MAX_PERMITS));
    assert!(semaphore.release_many(Semaphore::MAX_PERMITS - 1));
}

#[test]
fn gate() {
    let gate = Arc::new(Gate::default());

    let mut threads = Vec::new();

    for _ in 0..8 {
        let gate = gate.clone();
        threads.push(thread::spawn(move || {
            assert_eq!(gate.enter_sync(), Ok(State::Controlled));
        }));
    }

    let mut count = 0;
    while count != 8 {
        if let Ok(n) = gate.permit() {
            count += n;
        }
    }

    for thread in threads {
        thread.join().unwrap();
    }

    gate.open();
    assert_eq!(gate.enter_sync(), Ok(State::Open));

    gate.seal();
    assert_eq!(gate.enter_sync(), Err(Error::Sealed));
}

#[test]
fn pager() {
    let lock = Lock::default();

    let mut pinned_pager = pin!(Pager::default());
    assert!(!pinned_pager.is_registered());

    lock.lock_sync();
    lock.register_pager(&mut pinned_pager, Mode::Shared, true);

    assert!(pinned_pager.try_poll().is_err());

    assert!(lock.release_lock());
    assert!(pinned_pager.poll_sync().is_ok());
}
