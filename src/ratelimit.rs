//! Provider rate limiting: a shared cooldown gate per host, full-jitter
//! request backoff, and bounded concurrency lanes. Clock access goes
//! through the tokio timer, so these types are used from async code (or
//! tokio tests) — the judge pass is the consumer.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::DtError;

const BACKOFF_CAP_SECS: f64 = 90.0;
const ATTEMPT_CAP: u32 = 32;
const COOLDOWN_CAP: Duration = Duration::from_secs(3600);

pub trait Rng {
    fn next_f64(&mut self) -> f64;
}

pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self { state: seed }
    }

    #[must_use]
    pub fn from_entropy() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                since
                    .as_secs()
                    .wrapping_mul(1_000_000_000)
                    .wrapping_add(u64::from(since.subsec_nanos()))
            });
        Self::seeded(nanos ^ u64::from(std::process::id()))
    }
}

impl Rng for SplitMix64 {
    fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = self.state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^= mixed >> 31;
        f64::from_bits((mixed >> 11) | 0x3ff0_0000_0000_0000) - 1.0
    }
}

#[must_use]
pub fn backoff_wait(attempt: u32, base: Duration, rng: &mut dyn Rng) -> Duration {
    let bound = (base.as_secs_f64() * 2.0_f64.powf(f64::from(attempt.min(ATTEMPT_CAP))))
        .clamp(0.0, BACKOFF_CAP_SECS);
    Duration::from_secs_f64((rng.next_f64() * bound).max(0.0))
}

#[must_use]
pub fn cooldown_duration(retry_after: Duration, jitter: f64, rng: &mut dyn Rng) -> Duration {
    let scale = 1.0 + rng.next_f64() * jitter.max(0.0);
    let scaled = retry_after.as_secs_f64() * scale;
    Duration::from_secs_f64(scaled.clamp(0.0, COOLDOWN_CAP.as_secs_f64()))
}

pub struct CooldownGate {
    jitter: f64,
    base: Duration,
    rng: Mutex<SplitMix64>,
    deadlines: Mutex<BTreeMap<String, tokio::time::Instant>>,
    streaks: Mutex<BTreeMap<String, u32>>,
}

impl CooldownGate {
    #[must_use]
    pub fn new(jitter: f64, base: Duration) -> Self {
        Self {
            jitter,
            base,
            rng: Mutex::new(SplitMix64::from_entropy()),
            deadlines: Mutex::new(BTreeMap::new()),
            streaks: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn trip(&self, host: &str, retry_after: Option<Duration>) {
        let now = tokio::time::Instant::now();
        {
            let mut deadlines = self
                .deadlines
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let lapsed: Vec<String> = deadlines
                .iter()
                .filter(|(_, deadline)| **deadline <= now)
                .map(|(lapsed_host, _)| lapsed_host.clone())
                .collect();
            if !lapsed.is_empty() {
                deadlines.retain(|_, deadline| *deadline > now);
                let mut streaks = self.streaks.lock().unwrap_or_else(PoisonError::into_inner);
                for lapsed_host in lapsed {
                    streaks.remove(&lapsed_host);
                }
            }
        }
        let streak = {
            let mut streaks = self.streaks.lock().unwrap_or_else(PoisonError::into_inner);
            let entry = streaks.entry(host.to_owned()).or_insert(0);
            if retry_after.is_some() {
                *entry = 0;
                0
            } else {
                *entry = entry.saturating_add(1);
                *entry
            }
        };
        let duration = {
            let mut rng = self.rng.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(after) = retry_after {
                cooldown_duration(after, self.jitter, &mut *rng)
            } else {
                let raw = self
                    .base
                    .saturating_mul(2u32.saturating_pow(streak.saturating_sub(1).min(8)));
                cooldown_duration(raw.min(COOLDOWN_CAP), self.jitter, &mut *rng)
            }
        };
        let mut deadlines = self
            .deadlines
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let earliest = deadlines
            .get(host)
            .map_or(now, |existing| (*existing).max(now));
        let farthest = now.checked_add(COOLDOWN_CAP).unwrap_or(now);
        let deadline = earliest.checked_add(duration).unwrap_or(farthest);
        let _unused = deadlines.insert(host.to_owned(), deadline);
    }

    pub async fn wait(&self, host: &str) {
        loop {
            let remaining = self.remaining(host);
            if remaining.is_zero() {
                return;
            }
            tokio::time::sleep(remaining).await;
        }
    }

    #[must_use]
    pub fn remaining(&self, host: &str) -> Duration {
        let deadlines = self
            .deadlines
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        deadlines.get(host).map_or(Duration::ZERO, |deadline| {
            deadline.saturating_duration_since(tokio::time::Instant::now())
        })
    }
}

pub struct Lanes {
    per_host: usize,
    host_permits: Mutex<BTreeMap<String, Arc<Semaphore>>>,
    global: Arc<Semaphore>,
}

#[must_use]
pub struct LaneGuard {
    _host: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

impl Lanes {
    #[must_use]
    pub fn new(per_host: usize, global: usize) -> Self {
        Self {
            per_host,
            host_permits: Mutex::new(BTreeMap::new()),
            global: Arc::new(Semaphore::new(global)),
        }
    }

    pub async fn acquire(&self, host: &str) -> Result<LaneGuard, DtError> {
        let host_semaphore = self.host_semaphore(host);
        let host = host_semaphore
            .acquire_owned()
            .await
            .map_err(|err| DtError::Provider(format!("a host rate lane closed: {err}")))?;
        let global = Arc::clone(&self.global)
            .acquire_owned()
            .await
            .map_err(|err| DtError::Provider(format!("the global rate pool closed: {err}")))?;
        Ok(LaneGuard {
            _host: host,
            _global: global,
        })
    }

    fn host_semaphore(&self, host: &str) -> Arc<Semaphore> {
        let mut permits = self
            .host_permits
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let fresh = Arc::new(Semaphore::new(self.per_host));
        Arc::clone(permits.entry(host.to_owned()).or_insert(fresh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[tokio::test]
    async fn a_raised_global_pool_admits_that_many_requests_at_once() -> Result<(), DtError> {
        // Per-host 20 under global 20: twenty acquires on one host
        // all proceed — the single-provider panel shape.
        let lanes = Lanes::new(20, 20);
        let mut held = Vec::new();
        for _ in 0..20 {
            held.push(lanes.acquire("zai").await?);
        }
        drop(held);
        Ok(())
    }

    #[tokio::test]
    async fn the_global_pool_caps_concurrency_across_hosts() -> Result<(), DtError> {
        // Per-host 4 each under global 5: the sixth acquire waits, so
        // the global is the real ceiling.
        let lanes = Lanes::new(4, 5);
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(lanes.acquire("zai").await?);
        }
        held.push(lanes.acquire("anthropic").await?);
        let sixth = lanes.acquire("anthropic");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), sixth)
                .await
                .is_err(),
            "the sixth acquire waits — the global pool of 5 binds, not the host pool of 4"
        );
        drop(held);
        Ok(())
    }

    #[test]
    fn header_less_trips_escalate_with_the_streak() {
        let gate = CooldownGate::new(0.0, Duration::from_secs(5));
        gate.trip("zai", None);
        let first = gate.remaining("zai");
        assert!(
            first >= Duration::from_millis(4900) && first <= Duration::from_secs(6),
            "the first header-less trip adds one base interval: {first:?}"
        );
        gate.trip("zai", None);
        gate.trip("zai", None);
        let third = gate.remaining("zai");
        assert!(
            third >= Duration::from_millis(34_900),
            "consecutive header-less trips double per streak, so a quota window can actually pass: {third:?}"
        );
        gate.trip("zai", Some(Duration::from_secs(2)));
        let after_hint = gate.remaining("zai");
        gate.trip("zai", None);
        let reset = gate.remaining("zai");
        let added = reset.checked_sub(after_hint).unwrap_or(Duration::ZERO);
        assert!(
            added >= Duration::from_millis(4900) && added <= Duration::from_secs(6),
            "a hinted trip resets the streak, so the next header-less trip starts at one base again: {after_hint:?} -> {reset:?} (+{added:?})"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_streak_resets_once_the_deadline_lapses() {
        let gate = CooldownGate::new(0.0, Duration::from_secs(5));
        gate.trip("zai", None);
        gate.trip("zai", None);
        tokio::time::advance(Duration::from_secs(60)).await;
        let before = gate.remaining("zai");
        assert!(before.is_zero(), "the deadline lapsed");
        gate.trip("zai", None);
        let restarted = gate.remaining("zai");
        assert!(
            restarted >= Duration::from_millis(4900) && restarted <= Duration::from_secs(6),
            "a fresh trip after the deadline elapsed starts a new streak, not a doubling one: {restarted:?}"
        );
    }

    #[test]
    fn backoff_is_bounded_by_the_capped_exponential_and_jittered() {
        let mut rng = SplitMix64::seeded(7);
        let base = Duration::from_secs(5);
        let mut drawn: HashSet<u64> = HashSet::new();
        for attempt in 0..12u32 {
            for _ in 0..16 {
                let wait = backoff_wait(attempt, base, &mut rng);
                let bound =
                    (5.0 * 2.0_f64.powf(f64::from(attempt.min(ATTEMPT_CAP)))).min(BACKOFF_CAP_SECS);
                assert!(
                    wait.as_secs_f64() <= bound + 1e-9,
                    "every draw sits under the capped exponential: {wait:?} > {bound}s"
                );
                let _unused = drawn.insert(wait.as_secs_f64().to_bits());
            }
        }
        assert!(
            drawn.len() > 1,
            "full jitter spreads the waits, never a lockstep constant"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_tripped_host_blocks_all_its_waiters_not_other_hosts() {
        let gate = Arc::new(CooldownGate::new(0.0, Duration::from_secs(5)));
        gate.trip("api.z.ai", Some(Duration::from_secs(30)));
        let blocked = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move { gate.wait("api.z.ai").await })
        };
        tokio::task::yield_now().await;
        assert!(
            !blocked.is_finished(),
            "the tripped host's waiter is still paused at the deadline"
        );
        gate.wait("api.anthropic.com").await;
        assert!(
            gate.remaining("api.z.ai") >= Duration::from_secs(29),
            "the other host's pass leaves the tripped cooldown untouched"
        );
        tokio::time::advance(Duration::from_secs(30)).await;
        let _unused = blocked.await;
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_scales_the_cooldown() {
        let gate = CooldownGate::new(0.3, Duration::from_secs(5));
        gate.trip("api.z.ai", Some(Duration::from_secs(10)));
        let remaining = gate.remaining("api.z.ai");
        assert!(
            remaining >= Duration::from_secs(10)
                && remaining <= Duration::from_secs(13) + Duration::from_nanos(1),
            "the cooldown is retry-after scaled by at most the jitter: {remaining:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_trip_without_retry_after_still_cools_the_host() {
        let gate = CooldownGate::new(0.3, Duration::from_secs(5));
        gate.trip("api.z.ai", None);
        let remaining = gate.remaining("api.z.ai");
        assert!(
            remaining >= Duration::from_secs(5) && remaining <= Duration::from_secs(7),
            "a header-less trip starts at one base interval, scaled up to 1+jitter: {remaining:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_extreme_retry_after_clamps_into_a_bounded_cooldown() {
        let mut rng = SplitMix64::seeded(11);
        let direct = cooldown_duration(Duration::from_secs(4_000_000_000), 5.0, &mut rng);
        assert_eq!(
            direct, COOLDOWN_CAP,
            "a server-stated wait past the cap clamps to the cap, never panics"
        );
        let infinite = cooldown_duration(Duration::from_secs(10), f64::INFINITY, &mut rng);
        assert_eq!(
            infinite, COOLDOWN_CAP,
            "even an infinite jitter draw lands on the cap instead of a poisoned duration"
        );
        let gate = CooldownGate::new(1.0, Duration::from_secs(5));
        gate.trip("api.z.ai", Some(Duration::new(u64::MAX, 0)));
        assert_eq!(
            gate.remaining("api.z.ai"),
            COOLDOWN_CAP,
            "a Retry-After near the u64 ceiling cools the host for exactly the cap"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lanes_admit_their_per_host_bound_and_no_more_independently() -> Result<(), DtError> {
        let lanes = Lanes::new(2, 8);
        let first = lanes.acquire("zai").await?;
        let second = lanes.acquire("zai").await?;
        let fifth = lanes.acquire("zai");
        tokio::pin!(fifth);
        assert!(
            tokio::time::timeout(Duration::ZERO, fifth.as_mut())
                .await
                .is_err(),
            "the third zai call pends at its own lane bound"
        );
        let other = lanes.acquire("anthropic").await?;
        drop(other);
        drop(first);
        let _unused = tokio::time::timeout(Duration::ZERO, fifth.as_mut()).await;
        drop(second);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn the_global_cap_bounds_all_hosts_together() -> Result<(), DtError> {
        let lanes = Lanes::new(4, 8);
        let held: Vec<LaneGuard> = {
            let mut held = Vec::new();
            for _ in 0..4 {
                held.push(lanes.acquire("zai").await?);
                held.push(lanes.acquire("anthropic").await?);
            }
            held
        };
        let ninth = lanes.acquire("openai");
        tokio::pin!(ninth);
        assert!(
            tokio::time::timeout(Duration::ZERO, ninth.as_mut())
                .await
                .is_err(),
            "eight lanes are held globally, so a ninth anywhere pends"
        );
        drop(held);
        let _unused = tokio::time::timeout(Duration::ZERO, ninth.as_mut()).await;
        Ok(())
    }
}
