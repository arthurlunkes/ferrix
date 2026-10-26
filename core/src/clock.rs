use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::task::{Context, Poll, Waker};
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
        .spawn(|| {
            let mut ticks: u32 = 0;
            loop {
                std::thread::sleep(Duration::from_millis(1));
                let c = &*CLOCK;
                let now = c.base.elapsed().as_millis() as u64;
                c.mono_ms.store(now, Ordering::Relaxed);
                c.unix_ms.store(read_unix_ms(), Ordering::Relaxed);
                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(TIMER_SCAN_EVERY_MS) {
                    fire_expired(now);
                }
            }
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

/// Milliseconds on the cached monotonic scale for an `Instant`.
pub fn instant_ms(at: Instant) -> u64 {
    at.saturating_duration_since(CLOCK.base).as_millis() as u64
}

// ---------------------------------------------------------------------------
// Coarse timers.
//
// Like nginx's timers (and Cloudflare's pingora-timeout), deadlines are checked against
// the cached clock instead of reading the system clock: creating, polling or dropping a
// timer never makes a syscall. That matters on hosts without a usable TSC (HPET clock
// source), where every clock read is a slow syscall. The clock thread wakes expired
// timers every few milliseconds, which is plenty for network timeouts.
// ---------------------------------------------------------------------------

const TIMER_SCAN_EVERY_MS: u32 = 5;
const TIMER_SHARDS: usize = 16;

#[derive(Default)]
struct TimerShard {
    next_id: u64,
    entries: HashMap<u64, (u64, Waker)>,
}

static TIMERS: LazyLock<Box<[Mutex<TimerShard>]>> =
    LazyLock::new(|| (0..TIMER_SHARDS).map(|_| Mutex::default()).collect());

fn timer_shard() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static SHARD: usize = NEXT.fetch_add(1, Ordering::Relaxed) % TIMER_SHARDS;
    }
    SHARD.with(|s| *s)
}

fn fire_expired(now_ms: u64) {
    let mut due: Vec<Waker> = Vec::new();
    for shard in TIMERS.iter() {
        let Ok(mut s) = shard.lock() else { continue };
        if s.entries.is_empty() {
            continue;
        }
        s.entries.retain(|_, (deadline, waker)| {
            if *deadline <= now_ms {
                due.push(waker.clone());
                false
            } else {
                true
            }
        });
    }
    for w in due {
        w.wake();
    }
}

/// A sleep that resolves once the cached clock reaches `deadline_ms` (see
/// [`mono_ms`]). It only registers with the clock thread if it is actually polled
/// while still pending.
pub struct CoarseSleep {
    deadline_ms: u64,
    registered: Option<(usize, u64)>,
}

impl CoarseSleep {
    pub fn until_ms(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            registered: None,
        }
    }

    pub fn after(dur: Duration) -> Self {
        Self::until_ms(mono_ms().saturating_add(dur.as_millis() as u64))
    }

    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    pub fn is_elapsed(&self) -> bool {
        mono_ms() >= self.deadline_ms
    }

    fn deregister(&mut self) {
        if let Some((shard, id)) = self.registered.take() {
            if let Ok(mut s) = TIMERS[shard].lock() {
                s.entries.remove(&id);
            }
        }
    }
}

impl Future for CoarseSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if mono_ms() >= self.deadline_ms {
            self.deregister();
            return Poll::Ready(());
        }
        let deadline = self.deadline_ms;
        match self.registered {
            Some((shard, id)) => {
                if let Ok(mut s) = TIMERS[shard].lock() {
                    match s.entries.get_mut(&id) {
                        Some(entry) if entry.1.will_wake(cx.waker()) => {}
                        Some(entry) => entry.1 = cx.waker().clone(),
                        // Fired between our clock check and now: poll again shortly.
                        None => {
                            s.entries.insert(id, (deadline, cx.waker().clone()));
                        }
                    }
                }
            }
            None => {
                let shard = timer_shard();
                if let Ok(mut s) = TIMERS[shard].lock() {
                    s.next_id += 1;
                    let id = s.next_id;
                    s.entries.insert(id, (deadline, cx.waker().clone()));
                    self.registered = Some((shard, id));
                }
            }
        }
        Poll::Pending
    }
}

impl Drop for CoarseSleep {
    fn drop(&mut self) {
        self.deregister();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

/// Runs `fut` until the cached clock reaches `deadline_ms`.
pub async fn timeout_at_ms<F: Future>(deadline_ms: u64, fut: F) -> Result<F::Output, Elapsed> {
    let mut fut = std::pin::pin!(fut);
    let mut sleep = CoarseSleep::until_ms(deadline_ms);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(v) = fut.as_mut().poll(cx) {
            return Poll::Ready(Ok(v));
        }
        Pin::new(&mut sleep).poll(cx).map(|_| Err(Elapsed))
    })
    .await
}

pub async fn timeout<F: Future>(dur: Duration, fut: F) -> Result<F::Output, Elapsed> {
    timeout_at_ms(mono_ms().saturating_add(dur.as_millis() as u64), fut).await
}

/// hyper timer backed by the coarse timers, so header-read and keep-alive timeouts
/// don't read the system clock on every request.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoarseTimer;

struct HyperSleep(CoarseSleep);

impl Future for HyperSleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl hyper::rt::Sleep for HyperSleep {}

impl hyper::rt::Timer for CoarseTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(HyperSleep(CoarseSleep::after(duration)))
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(HyperSleep(CoarseSleep::until_ms(instant_ms(deadline))))
    }

    fn now(&self) -> Instant {
        now()
    }
}

/// `Date` header value for the current second, formatted once per second per thread.
pub fn http_date() -> axum::http::HeaderValue {
    use std::cell::RefCell;
    thread_local! {
        static CACHE: RefCell<(u64, Option<axum::http::HeaderValue>)> = const { RefCell::new((0, None)) };
    }
    let secs = unix_secs();
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.0 != secs || c.1.is_none() {
            let dt = DateTime::from_timestamp(secs as i64, 0).unwrap_or_else(Utc::now);
            c.1 = axum::http::HeaderValue::from_str(
                &dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
            )
            .ok();
            c.0 = secs;
        }
        c.1.clone().unwrap_or_else(|| {
            axum::http::HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coarse_timeout_fires_and_passes_through() {
        let r = timeout(Duration::from_millis(30), std::future::pending::<()>()).await;
        assert_eq!(r, Err(Elapsed));
        let r = timeout(Duration::from_millis(30), async { 7 }).await;
        assert_eq!(r, Ok(7));
    }

    #[test]
    fn dropped_timers_are_deregistered() {
        let mut sleep = CoarseSleep::after(Duration::from_secs(60));
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut sleep).poll(&mut cx).is_pending());
        let (shard, id) = sleep.registered.expect("should be registered");
        drop(sleep);
        assert!(!TIMERS[shard].lock().unwrap().entries.contains_key(&id));
    }

    #[test]
    fn date_header_format() {
        let d = http_date();
        assert!(d.to_str().unwrap().ends_with(" GMT"));
        assert_eq!(d.len(), 29);
    }

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
