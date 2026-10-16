//! VNX timestamps use signed microseconds since the Unix epoch.
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
pub fn now_micros() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(value) => value.as_micros().min(i64::MAX as u128) as i64,
        Err(value) => -(value.duration().as_micros().min(i64::MAX as u128) as i64),
    }
}
pub fn now_millis() -> i64 {
    now_micros() / 1000
}
pub fn now_seconds() -> i64 {
    now_micros() / 1_000_000
}
/// An independently controlled clock for replay/simulation. Rate 0 pauses it.
#[derive(Clone)]
pub struct Clock {
    state: Arc<RwLock<State>>,
}
struct State {
    base: i64,
    origin: Instant,
    rate: f64,
}
impl Clock {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(State {
                base: now_micros(),
                origin: Instant::now(),
                rate: 1.0,
            })),
        }
    }
    pub fn now_micros(&self) -> i64 {
        let state = self.state.read().unwrap();
        current(&state)
    }
    pub fn set(&self, timestamp: i64) {
        let mut state = self.state.write().unwrap();
        state.base = timestamp;
        state.origin = Instant::now();
    }
    pub fn set_rate(&self, rate: f64) -> crate::Result<()> {
        if !rate.is_finite() || rate < 0.0 {
            return Err(crate::error::invalid(
                "clock rate must be finite and nonnegative",
            ));
        }
        let mut state = self.state.write().unwrap();
        state.base = current(&state);
        state.origin = Instant::now();
        state.rate = rate;
        Ok(())
    }
    pub fn rate(&self) -> f64 {
        self.state.read().unwrap().rate
    }
}
fn current(state: &State) -> i64 {
    state.base.saturating_add(
        (state.origin.elapsed().as_micros() as f64 * state.rate).min(i64::MAX as f64) as i64,
    )
}
impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}
