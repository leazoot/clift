//! A fixed-window request counter, per client address.
//!
//! Deliberately the simplest thing that works. A relay's job is to hold a few
//! megabytes for a few minutes; the threat this addresses is somebody filling
//! it up or hammering it, not a distributed attack, and a token bucket with
//! burst allowances would be more machinery than the problem deserves.
//!
//! Counting is by IP address, which is what the socket gives us. Behind a
//! reverse proxy every request appears to come from the proxy -- so the limit
//! is documented as "per source the relay can see", and an operator who needs
//! per-user limits puts them in the proxy, where the information is.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

pub struct RateLimiter {
    windows: Mutex<HashMap<IpAddr, Window>>,
    per_minute: u32,
}

struct Window {
    started: Instant,
    count: u32,
}

impl RateLimiter {
    #[must_use]
    pub fn new(per_minute: u32) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            per_minute,
        }
    }

    /// Records a request and says whether it is within the limit.
    ///
    /// A limit of zero means no limiting at all, which is the right reading of
    /// "unset" for an operator running a relay on a private network.
    #[must_use]
    pub fn allow(&self, address: IpAddr) -> bool {
        if self.per_minute == 0 {
            return true;
        }
        let now = Instant::now();
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Bounded by sweeping the whole map when it grows: the alternative is a
        // table that an attacker can grow one address at a time.
        if windows.len() > 10_000 {
            windows.retain(|_, window| now.duration_since(window.started) < WINDOW);
        }

        let window = windows.entry(address).or_insert(Window {
            started: now,
            count: 0,
        });
        if now.duration_since(window.started) >= WINDOW {
            window.started = now;
            window.count = 0;
        }
        window.count += 1;
        window.count <= self.per_minute
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn address(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    #[test]
    fn requests_up_to_the_limit_are_allowed_and_the_next_one_is_not() {
        let limiter = RateLimiter::new(3);
        for attempt in 1..=3 {
            assert!(limiter.allow(address(1)), "request {attempt} was refused");
        }
        assert!(!limiter.allow(address(1)));
    }

    #[test]
    fn one_busy_client_does_not_shut_out_another() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.allow(address(1)));
        assert!(limiter.allow(address(1)));
        assert!(!limiter.allow(address(1)));
        assert!(limiter.allow(address(2)));
    }

    #[test]
    fn a_limit_of_zero_means_no_limit() {
        let limiter = RateLimiter::new(0);
        for _ in 0..1000 {
            assert!(limiter.allow(address(1)));
        }
    }
}
