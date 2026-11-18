//! Classroom countdown state machine.
//!
//! Pure logic: the caller supplies `now` in seconds, so every transition and
//! every duration is decided here and can be tested without AppKit or a device.
//! See docs/COUNTDOWN.zh-CN.md.

/// Minutes offered the first time, and whenever no earlier value was saved.
pub const DEFAULT_MINUTES: u32 = 5;
/// Settable range; further presses at a boundary do nothing.
pub const MIN_MINUTES: u32 = 1;
pub const MAX_MINUTES: u32 = 99;
/// Setup starts by itself after this many seconds without a key press.
pub const SETUP_SECONDS: f64 = 3.0;
/// How long the "time is up" panel stays before the state returns to idle.
pub const FINISHED_SECONDS: f64 = 5.0;

/// Nearest settable minute count.
pub fn clamp_minutes(minutes: u32) -> u32 {
    minutes.clamp(MIN_MINUTES, MAX_MINUTES)
}

/// What a `tick` changed, so the caller can save settings, vibrate or hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Setup ran out and the countdown is running for this many minutes.
    Started(u32),
    /// The countdown reached zero.
    TimeUp,
    /// The "time is up" panel ran out.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Countdown {
    #[default]
    Idle,
    Setup {
        minutes: u32,
        /// When setup starts by itself.
        start_at: f64,
    },
    Running {
        deadline: f64,
    },
    Finished {
        until: f64,
    },
}

impl Countdown {
    /// Long press or menu item: idle starts setup with the last used minutes;
    /// setup, running and the "time is up" panel all return to idle.
    pub fn toggle(&mut self, now: f64, last_minutes: u32) {
        *self = match *self {
            Self::Idle => Self::Setup {
                minutes: clamp_minutes(last_minutes),
                start_at: now + SETUP_SECONDS,
            },
            _ => Self::Idle,
        };
    }

    pub fn cancel(&mut self) {
        *self = Self::Idle;
    }

    /// Adds (or subtracts) whole minutes during setup. Every adjustment also
    /// restarts the automatic start, so the last key press decides the moment.
    pub fn adjust(&mut self, delta: i32, now: f64) {
        if let Self::Setup { minutes, .. } = *self {
            let minutes = if delta >= 0 {
                minutes.saturating_add(delta.unsigned_abs())
            } else {
                minutes.saturating_sub(delta.unsigned_abs())
            };
            *self = Self::Setup {
                minutes: clamp_minutes(minutes),
                start_at: now + SETUP_SECONDS,
            };
        }
    }

    /// Advances the state machine; at most one change happens per call.
    pub fn tick(&mut self, now: f64) -> Option<Change> {
        match *self {
            Self::Setup { minutes, start_at } if now >= start_at => {
                *self = Self::Running {
                    deadline: start_at + f64::from(minutes) * 60.0,
                };
                Some(Change::Started(minutes))
            }
            Self::Running { deadline } if now >= deadline => {
                *self = Self::Finished {
                    until: deadline + FINISHED_SECONDS,
                };
                Some(Change::TimeUp)
            }
            Self::Finished { until } if now >= until => {
                *self = Self::Idle;
                Some(Change::Closed)
            }
            _ => None,
        }
    }

    /// Whole seconds left, rounded up, so the last second still shows 1 and the
    /// deadline itself shows 0. None while not running.
    pub fn remaining(&self, now: f64) -> Option<u32> {
        match *self {
            Self::Running { deadline } => Some((deadline - now).max(0.0).ceil() as u32),
            _ => None,
        }
    }

    /// Only setup needs the short page-turn keys; every other state leaves them
    /// to the system, so slides keep turning.
    pub fn wants_page_keys(&self) -> bool {
        matches!(self, Self::Setup { .. })
    }
}
