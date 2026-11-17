use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const SLOTS: usize = 32;

#[repr(align(64))]
struct Slot(AtomicIsize);

static BUSY: [Slot; SLOTS] = [const { Slot(AtomicIsize::new(0)) }; SLOTS];
static DRAINING: AtomicBool = AtomicBool::new(false);

fn slot() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static INDEX: usize = NEXT.fetch_add(1, Ordering::Relaxed) % SLOTS;
    }
    INDEX.with(|i| *i)
}

pub struct Busy(usize);

pub fn begin() -> Busy {
    let i = slot();
    BUSY[i].0.fetch_add(1, Ordering::Relaxed);
    Busy(i)
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY[self.0].0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn busy() -> isize {
    BUSY.iter().map(|s| s.0.load(Ordering::Relaxed)).sum()
}

#[inline]
pub fn draining() -> bool {
    DRAINING.load(Ordering::Relaxed)
}

pub async fn drain(timeout: Duration) -> isize {
    DRAINING.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + timeout;
    loop {
        let left = busy();
        if left <= 0 || Instant::now() >= deadline {
            return left.max(0);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_are_counted() {
        let before = busy();
        let a = begin();
        let b = begin();
        assert_eq!(busy(), before + 2);
        drop(a);
        drop(b);
        assert_eq!(busy(), before);
    }
}
