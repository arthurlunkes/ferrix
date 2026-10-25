# 01. Overview and Comparison

## 📌 What is Ferrix?

**Ferrix** is an asynchronous web server and reverse proxy designed to combine the efficiency and robustness of **Nginx** with the memory safety of **Rust** and the ease-of-use of a modern **React Web Dashboard**.

In traditional environments, tools like Nginx require manual editing of plaintext configuration files (`nginx.conf`), process signal reloads (`nginx -s reload`), and depend on third-party modules or external tools for real-time observability.

Ferrix solves this complexity by integrating into a single lightweight executable:

1. **High-Concurrency Network Engine**: Built on Tokio, Hyper, and Axum, processing tens of thousands of requests per second non-blockingly.
2. **Embedded React Web Dashboard**: A sophisticated administrative interface with dark mode, live traffic charts, latency percentiles, and real-time request inspection.
3. **RESTful Management API**: Full programmatic control for CI/CD pipelines, automation scripts, and cluster orchestrators.
4. **Zero-Downtime In-Memory Hot Reloading**: Route, upstream, and security updates are applied atomically via pointer swaps (`ArcSwap`), without dropping a single active connection or closing listening sockets.

---

## ⚖️ Comparison: Ferrix vs. Nginx vs. Caddy

| Feature                              | Nginx                                        | Caddy                  | Ferrix                                             |
| ------------------------------------ | -------------------------------------------- | ---------------------- | -------------------------------------------------- |
| **Base Language**                    | C                                            | Go                     | **Rust** (Tokio / Hyper / Axum)                    |
| **Memory Safety**                    | Vulnerable to buffer overflows / leaks       | Safe (with GC pauses)  | **Zero-Cost Abstractions / No Garbage Collector**  |
| **Native Web Dashboard**             | ❌ No (requires separate Nginx Proxy Manager)| ❌ No                  | ✅ **Yes (React SPA embedded in binary)**          |
| **REST API for Dynamic Config**      | ⚠️ Paid Nginx Plus only                      | ✅ Yes                 | ✅ **Yes (Native and open-source)**                |
| **Real-time Live Request Telemetry** | ⚠️ Requires exporter + Prometheus/Grafana    | ⚠️ Requires exporters  | ✅ **Yes (Real-time SSE stream built-in)**         |
| **WebSocket Proxying**               | ✅ Yes (`proxy_set_header Upgrade`)          | ✅ Yes                 | ✅ **Yes (Transparent full-duplex tunnel)**       |
| **Load Balancing & Failover**        | ✅ Yes                                       | ✅ Yes                 | ✅ **Yes (5 algorithms + automated failover)**     |
| **Active Health Checks**             | ⚠️ Paid Nginx Plus only                      | ✅ Yes                 | ✅ **Yes (Native background health checker)**      |
| **SPA Fallback (`try_files`)**       | ✅ Yes                                       | ✅ Yes                 | ✅ **Yes (Native directive)**                      |
| **Distribution Format**              | Binary + dynamic modules                     | Single binary          | **Single standalone binary (with bundled UI)**     |

---

## 🛡️ Advantages of Building in Rust

1. **Memory Safety without a Garbage Collector (GC)**:
   - Unlike Go (used in Caddy and Traefik), Rust has no runtime garbage collection pauses (*stop-the-world*). This guarantees rock-solid p99 tail latencies and predictable memory usage under heavy load.
2. **Compile-Time Concurrency Safety**:
   - The Rust compiler guarantees thread safety and prevents *data races* through its strict ownership and borrowing system.
3. **Zero-Copy Asynchronous I/O with Tokio**:
   - Data streams and static file transfers operate efficiently without unnecessary memory allocations or buffer copying.
