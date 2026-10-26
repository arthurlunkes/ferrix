use chrono::{DateTime, Utc};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
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
// source), where every clock read is a slow syscall.
//
// Each proxy worker thread owns its timers outright (no locks, nothing shared with other
// threads) and a small task on its runtime fires the expired ones every few
// milliseconds. Threads that never called `enable_local_timers` (the shared runtime that
// runs the admin API and health checks) fall back to ordinary tokio timers.
// ---------------------------------------------------------------------------

const TIMER_TICK: Duration = Duration::from_millis(5);

#[derive(Default)]
struct LocalWheel {
    next_id: u64,
    entries: BTreeMap<(u64, u64), Waker>,
}

thread_local! {
    static WHEEL: RefCell<Option<LocalWheel>> = const { RefCell::new(None) };
}

/// Turns on per-thread coarse timers for the current (single-threaded) runtime. Must be
/// called from inside that runtime.
pub fn enable_local_timers() {
    let fresh = WHEEL.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_some() {
            return false;
        }
        *w = Some(LocalWheel::default());
        true
    });
    if !fresh {
        return;
    }
    tokio::spawn(async {
        let mut ticker = tokio::time::interval(TIMER_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let now = mono_ms();
            let due: Vec<Waker> = WHEEL.with(|w| {
                let mut w = w.borrow_mut();
                let Some(wheel) = w.as_mut() else {
                    return Vec::new();
                };
                let mut due = Vec::new();
                while let Some(entry) = wheel.entries.first_entry() {
                    if entry.key().0 > now {
                        break;
                    }
                    due.push(entry.remove());
                }
                due
            });
            for w in due {
                w.wake();
            }
        }
    });
}

enum Registration {
    None,
    Local(std::thread::ThreadId, (u64, u64)),
    Tokio(Pin<Box<tokio::time::Sleep>>),
}

/// A sleep that resolves once the cached clock reaches `deadline_ms` (see
/// [`mono_ms`]). It only registers a timer if it is actually polled while pending.
pub struct CoarseSleep {
    deadline_ms: u64,
    reg: Registration,
}

impl CoarseSleep {
    pub fn until_ms(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            reg: Registration::None,
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
        if let Registration::Local(owner, key) =
            std::mem::replace(&mut self.reg, Registration::None)
        {
            if owner == std::thread::current().id() {
                WHEEL.with(|w| {
                    if let Some(wheel) = w.borrow_mut().as_mut() {
                        wheel.entries.remove(&key);
                    }
                });
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
        let this = &mut *self;
        if let Registration::Tokio(sleep) = &mut this.reg {
            return sleep.as_mut().poll(cx);
        }
        let registered = WHEEL.with(|w| {
            let mut w = w.borrow_mut();
            let wheel = w.as_mut()?;
            match &this.reg {
                Registration::Local(_, key) => match wheel.entries.get_mut(key) {
                    Some(waker) => {
                        if !waker.will_wake(cx.waker()) {
                            *waker = cx.waker().clone();
                        }
                    }
                    // Already fired between our clock check and now; re-arm.
                    None => {
                        wheel.entries.insert(*key, cx.waker().clone());
                    }
                },
                _ => {
                    wheel.next_id += 1;
                    let key = (deadline, wheel.next_id);
                    wheel.entries.insert(key, cx.waker().clone());
                    this.reg = Registration::Local(std::thread::current().id(), key);
                }
            }
            Some(())
        });
        if registered.is_none() {
            let at = tokio::time::Instant::from_std(CLOCK.base + Duration::from_millis(deadline));
            let mut sleep = Box::pin(tokio::time::sleep_until(at));
            let ready = sleep.as_mut().poll(cx).is_ready();
            this.reg = Registration::Tokio(sleep);
            if ready {
                return Poll::Ready(());
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

    #[tokio::test]
    async fn local_wheel_fires_and_deregisters() {
        enable_local_timers();
        let r = timeout(Duration::from_millis(20), std::future::pending::<()>()).await;
        assert_eq!(r, Err(Elapsed));

        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut sleep = CoarseSleep::after(Duration::from_secs(60));
        assert!(Pin::new(&mut sleep).poll(&mut cx).is_pending());
        let Registration::Local(_, key) = sleep.reg else {
            panic!("should use the local wheel");
        };
        drop(sleep);
        let still_there = WHEEL.with(|w| w.borrow().as_ref().unwrap().entries.contains_key(&key));
        assert!(!still_there);
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
