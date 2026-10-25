use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Clock {
    base: Instant,
    mono_ms: AtomicU64,
    unix_ms: AtomicU64,
}

static CLOCK: LazyLock<Clock> = LazyLock::new(|| {
    let clock = Clock {
        base: Instant::now(),
        mono_ms: AtomicU64::new(0),
        unix_ms: AtomicU64::new(read_unix_ms()),
    };
    std::thread::Builder::new()
        .name("ferrix-clock".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_millis(1));
            let c = &*CLOCK;
            c.mono_ms
                .store(c.base.elapsed().as_millis() as u64, Ordering::Relaxed);
            c.unix_ms.store(read_unix_ms(), Ordering::Relaxed);
        })
        .expect("failed to start clock thread");
    clock
});

fn read_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn now() -> Instant {
    let c = &*CLOCK;
    c.base + Duration::from_millis(c.mono_ms.load(Ordering::Relaxed))
}

pub fn mono_ms() -> u64 {
    CLOCK.mono_ms.load(Ordering::Relaxed)
}

pub fn utc_now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(CLOCK.unix_ms.load(Ordering::Relaxed) as i64)
        .unwrap_or_else(Utc::now)
}

pub fn unix_secs() -> u64 {
    CLOCK.unix_ms.load(Ordering::Relaxed) / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_real_clock() {
        let _ = mono_ms();
        std::thread::sleep(Duration::from_millis(30));
        let drift = Instant::now().saturating_duration_since(now());
        assert!(drift < Duration::from_millis(20), "atraso de {:?}", drift);

        let real = Utc::now().timestamp_millis();
        let cached = utc_now().timestamp_millis();
        assert!(
            (real - cached).abs() < 20,
            "diferença de {} ms",
            real - cached
        );
    }
}
