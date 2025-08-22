use criterion::{Criterion, criterion_group, criterion_main};
use saa::Lock;

fn exclusive_unlock(c: &mut Criterion) {
    c.bench_function("lock-exclusive-unlock", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.lock_sync();
            assert!(lock.release_lock());
        });
    });
}

fn shared_shared_unlock_unlock(c: &mut Criterion) {
    c.bench_function("share-shared-unlock-unlock", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.share_sync();
            lock.share_sync();
            assert!(lock.release_share());
            assert!(lock.release_share());
        });
    });
}

criterion_group!(lock, exclusive_unlock, shared_shared_unlock_unlock);
criterion_main!(lock);
