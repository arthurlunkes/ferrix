//! Event loop plumbing, like nginx's `src/event/`: the cached clock and timers
//! (`ngx_times` + `ngx_event_timer`) and the accept loop (`ngx_event_accept`).

pub mod accept;
pub mod clock;
