use std::sync::Arc;
use std::thread;

use saa::gate::{Error, State};
use saa::{Gate, Lock, Semaphore};

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

    let mut cnt = 0;
    while cnt != 8 {
        if let Ok(n) = gate.permit() {
            cnt += n;
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
