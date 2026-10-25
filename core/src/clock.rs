//! Relógio em cache, com resolução de 1 ms.
//!
//! Ler o relógio do sistema parece de graça, mas não é: sem TSC confiável (máquinas
//! virtuais, alguns hosts com HPET), cada `Instant::now()`/`Utc::now()` vira uma syscall
//! que vai até o hardware. O Ferrix chegava a ~14 leituras por requisição de proxy, e no
//! benchmark elas custavam mais de 12% da CPU. O nginx resolve o mesmo problema guardando a
//! hora em cache (`ngx_time_update`); aqui, uma thread de fundo atualiza dois atômicos a cada
//! milissegundo e todo o resto só os lê.
//!
//! Serve para tudo que tolera erro de 1 ms: carimbo de data dos logs, RPS, health checks,
//! revalidação do cache de arquivos e prazos de timeout. A latência de cada requisição,
//! que precisa de precisão abaixo de 1 ms, continua usando `Instant::now()`.

use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Clock {
    /// Referência do relógio monotônico; `mono_ms` conta a partir dela.
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
        .expect("falha ao iniciar a thread do relógio");
    clock
});

fn read_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `Instant` aproximado (até 1 ms atrás do real), sem syscall.
pub fn now() -> Instant {
    let c = &*CLOCK;
    c.base + Duration::from_millis(c.mono_ms.load(Ordering::Relaxed))
}

/// Milissegundos monotônicos desde a partida do processo, sem syscall.
pub fn mono_ms() -> u64 {
    CLOCK.mono_ms.load(Ordering::Relaxed)
}

/// Hora atual em UTC com resolução de 1 ms, sem syscall.
pub fn utc_now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(CLOCK.unix_ms.load(Ordering::Relaxed) as i64)
        .unwrap_or_else(Utc::now)
}

/// Segundos Unix atuais, sem syscall.
pub fn unix_secs() -> u64 {
    CLOCK.unix_ms.load(Ordering::Relaxed) / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_real_clock() {
        let _ = mono_ms(); // inicializa a thread
        std::thread::sleep(Duration::from_millis(30));
        let drift = Instant::now().saturating_duration_since(now());
        assert!(drift < Duration::from_millis(20), "atraso de {:?}", drift);

        let real = Utc::now().timestamp_millis();
        let cached = utc_now().timestamp_millis();
        assert!((real - cached).abs() < 20, "diferença de {} ms", real - cached);
    }
}
