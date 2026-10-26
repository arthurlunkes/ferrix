# 07. Glossary

Short definitions of the terms and abbreviations used in the README, the benchmarks and
the rest of this documentation.

## Performance and benchmarks

| Term | Meaning |
| --- | --- |
| **req/s** | Requests per second: how many HTTP requests the server completed per second during the test (throughput). Higher is better. |
| **Latency** | Time from sending a request until the full response arrives. Lower is better. |
| **p50** | 50th percentile (median) latency: half of the requests were faster than this, half slower. The "typical" request. |
| **p99** | 99th percentile latency: 99% of the requests were faster than this, and the slowest 1% took longer. It shows the "tail", the worst cases users actually hit. |
| **Tail latency** | The slow end of the latency distribution (p99 and above). |
| **wrk** | The HTTP load generator used by `benchmarks/run.sh`. It keeps a fixed number of connections open (256 here) and sends requests as fast as the server answers. |
| **Warm-up** | A short unmeasured run before each measurement, so caches and connection pools are filled before the numbers count. |
| **Upstream** | The backend server a proxy forwards requests to. In the benchmark it's an nginx returning a 20-byte body. |
| **Keep-alive** | Reusing one TCP connection for many HTTP requests instead of opening a new one each time. |
| **RSS** | Resident Set Size: memory pages of a process that are currently in RAM, including shared ones (program code, libraries). Summing RSS over several processes counts shared pages several times. |
| **PSS** | Proportional Set Size: like RSS, but each shared page is divided among the processes that share it. The fair way to compare one process (Ferrix) with a group of processes (nginx master + workers). |
| **MB / MiB** | Megabyte (10⁶ bytes) / mebibyte (2²⁰ = 1,048,576 bytes). `docker stats` reports MiB. |
| **Syscall** | System call: a request from a program to the operating system kernel (read a socket, open a file, read the clock). Each one has a fixed cost, so fast servers try to make fewer. |
| **TSC / HPET** | Hardware clocks. With a usable TSC (Time Stamp Counter), reading the time is almost free; machines that fall back to HPET (High Precision Event Timer) pay a slow syscall per clock read, which is why Ferrix caches the time. |
| **perf** | Linux profiler used to find where CPU time goes. |

## Server internals

| Term | Meaning |
| --- | --- |
| **Worker** | A thread (Ferrix) or process (nginx) that accepts and serves connections. Both use one per CPU core in the benchmark. |
| **Thread-per-core** | Each worker thread has its own event loop and its own listening socket, so a request never moves between threads. |
| **CPU affinity** | Pinning a thread to specific CPU cores so the scheduler doesn't move it around (`worker_cpu_affinity`). |
| **File descriptor / RLIMIT_NOFILE** | Every open socket or file is a numbered descriptor; `RLIMIT_NOFILE` is the per-process cap on how many can be open (`worker_rlimit_nofile`, `ulimit -n`). |
| **Listen backlog** | Kernel queue of connections that finished the TCP handshake but weren't accepted by the server yet. |
| **SO_REUSEPORT** | Socket option that lets each worker bind its own socket to the same port; the kernel spreads new connections across them. |
| **sendfile / zero-copy** | Linux syscall that sends a file to a socket directly from the kernel's page cache, without copying it through the program's memory. |
| **TCP_CORK / `tcp_nopush`** | Socket option that holds back partially filled packets, so a response header and the start of a file go out together in full packets. |
| **TCP_NODELAY** | Socket option that sends small writes immediately instead of waiting to combine them (disables Nagle's algorithm). |
| **Page cache** | The kernel's in-memory copy of recently read files. |
| **Allocator / mimalloc** | The library that hands out and frees memory inside the process. Ferrix uses Microsoft's mimalloc. |
| **LTO** | Link-Time Optimization: the compiler optimizes across the whole program when building the final binary. |
| **Future (Rust)** | The state machine behind an `async` function. Its size is memory held per connection while it waits. |
| **Tokio / hyper / axum** | Rust's async runtime, HTTP library and web framework that Ferrix is built on. |
| **Hot reload** | Applying a new configuration without restarting the process or dropping connections. |

## HTTP, TLS and features

| Term | Meaning |
| --- | --- |
| **Reverse proxy** | A server that receives client requests and forwards them to one or more upstreams. |
| **Load balancing** | Spreading requests across several upstream servers (round robin, least connections, hashing...). |
| **Consistent hashing / ketama** | Load balancing where the same key (client IP, URL...) always goes to the same server, and adding or removing a server only moves that server's keys. |
| **Virtual host (vhost)** | One site served by Ferrix, selected by the `Host` header and port. |
| **TLS / HTTPS** | Encryption for HTTP connections. |
| **SNI** | Server Name Indication: the hostname a client sends during the TLS handshake, used to pick the right certificate when several sites share one port. |
| **ACME / Let's Encrypt** | The protocol (and the free certificate authority) for issuing TLS certificates automatically. |
| **HTTP-01** | ACME challenge where the CA checks a token at `http://<domain>/.well-known/acme-challenge/...` to prove you control the domain. |
| **HTTP/2, HTTP/3, QUIC** | Newer HTTP versions: HTTP/2 multiplexes requests over one TCP connection; HTTP/3 does the same over QUIC (UDP). |
| **WebSocket** | A long-lived, two-way connection that starts as an HTTP request and is then "upgraded". |
| **Stream (TCP/UDP) proxy** | Forwarding raw TCP or UDP traffic (databases, DNS, MQTT) without understanding HTTP, like nginx's `stream {}` block. |
| **ETag / 304 Not Modified** | A fingerprint of a file; when the browser already has that version, the server answers `304` with no body. |
| **Range / 206 Partial Content** | Requesting only part of a file (resumable downloads, video seeking). |
| **gzip / brotli (br) / zstd** | Compression formats negotiated through the `Accept-Encoding` header. |
| **CORS** | Cross-Origin Resource Sharing: headers that tell browsers which other sites may call an API. |
| **Rate limiting / token bucket** | Capping how many requests a client may make per second; the token bucket allows short bursts. |
| **CIDR / ACL** | IP ranges written like `10.0.0.0/8` / access control lists that allow or deny them. |
| **SSE** | Server-Sent Events: a one-way stream from server to browser, used by the dashboard's live request view. |
| **Prometheus** | Monitoring system; Ferrix exposes metrics in its text format at `/metrics`. |
| **Control plane / admin API** | The REST API (port 8081 by default) used to read and change Ferrix's configuration at runtime. |
