use chrono::{DateTime, Utc};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

struct Clock {
    base: Instant,
    base_ns: u64,
    slack_ms: u64,
}

static CLOCK: LazyLock<Clock> = LazyLock::new(|| {
    let base = Instant::now();
    Clock {
        base,
        base_ns: raw::precise_ns(),
        slack_ms: raw::resolution_ms(),
    }
});

#[cfg(any(target_os = "linux", target_os = "android"))]
mod raw {
    fn read(clock: libc::clockid_t) -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(clock, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    pub fn precise_ns() -> u64 {
        read(libc::CLOCK_MONOTONIC)
    }

    pub fn mono_ns() -> u64 {
        read(libc::CLOCK_MONOTONIC_COARSE)
    }

    pub fn unix_ms() -> u64 {
        read(libc::CLOCK_REALTIME_COARSE) / 1_000_000
    }

    pub fn resolution_ms() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_getres(libc::CLOCK_MONOTONIC_COARSE, &mut ts) };
        (ts.tv_nsec as u64).div_ceil(1_000_000).clamp(1, 20)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod raw {
    use std::sync::LazyLock;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    static START: LazyLock<Instant> = LazyLock::new(Instant::now);

    pub fn precise_ns() -> u64 {
        START.elapsed().as_nanos() as u64
    }

    pub fn mono_ns() -> u64 {
        START.elapsed().as_nanos() as u64
    }

    pub fn unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn resolution_ms() -> u64 {
        1
    }
}

pub fn now() -> Instant {
    CLOCK.base + Duration::from_millis(mono_ms())
}

pub fn mono_ms() -> u64 {
    raw::mono_ns().saturating_sub(CLOCK.base_ns) / 1_000_000
}

pub fn utc_now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(raw::unix_ms() as i64).unwrap_or_else(Utc::now)
}

pub fn unix_secs() -> u64 {
    raw::unix_ms() / 1000
}

pub fn instant_ms(at: Instant) -> u64 {
    at.saturating_duration_since(CLOCK.base).as_millis() as u64
}

struct LocalWheel {
    next_id: u64,
    armed: u64,
    notify: Arc<tokio::sync::Notify>,
    entries: BTreeMap<(u64, u64), Waker>,
}

impl LocalWheel {
    fn insert(&mut self, key: (u64, u64), waker: Waker) {
        self.entries.insert(key, waker);
        if key.0 < self.armed {
            self.armed = key.0;
            self.notify.notify_one();
        }
    }
}

thread_local! {
    static LAST_ACTIVE_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static WHEEL: RefCell<Option<LocalWheel>> = const { RefCell::new(None) };
}

const IDLE_TRIM_AFTER_MS: u64 = 1000;

pub fn touch() {
    let was = LAST_ACTIVE_MS.with(|l| l.replace(mono_ms().max(1)));
    if was == 0 {
        WHEEL.with(|w| {
            if let Ok(w) = w.try_borrow() {
                if let Some(wheel) = w.as_ref() {
                    wheel.notify.notify_one();
                }
            }
        });
    }
}

fn trim_if_idle(now: u64) {
    let idle = LAST_ACTIVE_MS.with(|l| {
        let last = l.get();
        if last != 0 && now.saturating_sub(last) >= IDLE_TRIM_AFTER_MS {
            l.set(0);
            true
        } else {
            false
        }
    });
    if idle {
        crate::http::upstream::h1::sweep_idle();
        crate::os::trim_allocator();
    }
}

pub fn enable_local_timers() {
    let notify = WHEEL.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_some() {
            return None;
        }
        let notify = Arc::new(tokio::sync::Notify::new());
        *w = Some(LocalWheel {
            next_id: 0,
            armed: u64::MAX,
            notify: notify.clone(),
            entries: BTreeMap::new(),
        });
        Some(notify)
    });
    let Some(notify) = notify else {
        return;
    };
    tokio::spawn(async move {
        loop {
            let now = mono_ms();
            let (due, next) = WHEEL.with(|w| {
                let mut w = w.borrow_mut();
                let Some(wheel) = w.as_mut() else {
                    return (Vec::new(), u64::MAX);
                };
                let mut due = Vec::new();
                while let Some(entry) = wheel.entries.first_entry() {
                    if entry.key().0 > now {
                        break;
                    }
                    due.push(entry.remove());
                }
                let mut next = wheel.entries.keys().next().map_or(u64::MAX, |k| k.0);
                let last = LAST_ACTIVE_MS.with(|l| l.get());
                if last != 0 {
                    next = next.min(last + IDLE_TRIM_AFTER_MS);
                }
                wheel.armed = next;
                (due, next)
            });
            for w in due {
                w.wake();
            }
            trim_if_idle(now);
            if next == u64::MAX {
                notify.notified().await;
                continue;
            }
            let wake_ms = next.max(now + 1) + CLOCK.slack_ms;
            let at = tokio::time::Instant::from_std(CLOCK.base + Duration::from_millis(wake_ms));
            tokio::select! {
                _ = tokio::time::sleep_until(at) => {}
                _ = notify.notified() => {}
            }
        }
    });
}

enum Registration {
    None,
    Local(std::thread::ThreadId, (u64, u64)),
    Tokio(Pin<Box<tokio::time::Sleep>>),
}

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
                    None => {
                        wheel.insert(*key, cx.waker().clone());
                    }
                },
                _ => {
                    wheel.next_id += 1;
                    let key = (deadline, wheel.next_id);
                    wheel.insert(key, cx.waker().clone());
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
