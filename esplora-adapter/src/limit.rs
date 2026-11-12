//! Per-IP token bucket.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

pub struct Limiter {
    per_sec: f64,
    burst: f64,
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
}

impl Limiter {
    pub fn new(per_sec: u32, burst: u32) -> Self {
        Self {
            per_sec: per_sec as f64,
            burst: burst.max(1) as f64,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Ok if allowed; Err(seconds to wait) if not.
    pub fn check(&self, ip: IpAddr) -> Result<(), u64> {
        if self.per_sec <= 0.0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut m = self.buckets.lock().unwrap();
        if m.len() > 50_000 {
            // drop idle entries so the map cannot grow without bound
            m.retain(|_, (_, at)| now.duration_since(*at).as_secs() < 60);
        }
        let (tokens, at) = m.entry(ip).or_insert((self.burst, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * self.per_sec).min(self.burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - *tokens) / self.per_sec).ceil().max(1.0) as u64)
        }
    }
}
