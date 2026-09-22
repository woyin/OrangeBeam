//! User settings for remote gestures and presentation timers.
//! Platform-independent; the app stores them as a small key=value file.
use crate::presentation::{clamp_radius, Effect, DEFAULT_RADIUS};

/// Effects in cycle order; index matches `Settings::cycle`.
pub const EFFECTS: [Effect; 4] = [
    Effect::Spotlight,
    Effect::Laser,
    Effect::Magnify,
    Effect::Box,
];
const EFFECT_KEYS: [&str; 4] = ["spotlight", "laser", "magnify", "box"];

/// When timer reminders also appear on screen (the audience may see them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenReminder {
    Off,
    OnVibrationFailure,
    Always,
}

impl ScreenReminder {
    pub const ALL: [ScreenReminder; 3] = [
        ScreenReminder::Off,
        ScreenReminder::OnVibrationFailure,
        ScreenReminder::Always,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ScreenReminder::Off => "关闭（只振动）",
            ScreenReminder::OnVibrationFailure => "仅在振动失败时",
            ScreenReminder::Always => "始终显示",
        }
    }
    fn key(self) -> &'static str {
        match self {
            ScreenReminder::Off => "off",
            ScreenReminder::OnVibrationFailure => "on-vibration-failure",
            ScreenReminder::Always => "always",
        }
    }
    /// Whether a reminder is shown, given whether the vibration was delivered.
    pub fn shows(self, vibrated: bool) -> bool {
        match self {
            ScreenReminder::Off => false,
            ScreenReminder::OnVibrationFailure => !vibrated,
            ScreenReminder::Always => true,
        }
    }
}
pub const MAX_TIMERS: usize = 3;
pub const MAX_TIMER_MINUTES: u32 = 600;
pub const ZOOM_LEVELS: [f64; 4] = [1.5, 2.0, 3.0, 4.0];
pub const DEFAULT_SHADE: f64 = 0.6;

/// Dimming outside the spotlight/box; kept visible but never fully black.
pub fn clamp_shade(shade: f64) -> f64 {
    if shade.is_finite() {
        shade.clamp(0.2, 0.9)
    } else {
        DEFAULT_SHADE
    }
}

/// Nearest supported magnifier level.
pub fn nearest_zoom(zoom: f64) -> f64 {
    ZOOM_LEVELS
        .into_iter()
        .min_by(|a, b| (a - zoom).abs().total_cmp(&(b - zoom).abs()))
        .unwrap_or(2.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldAction {
    None,
    PlayFromCurrent,
    BlackScreen,
    EndShow,
}

impl HoldAction {
    pub const ALL: [HoldAction; 4] = [
        HoldAction::None,
        HoldAction::PlayFromCurrent,
        HoldAction::BlackScreen,
        HoldAction::EndShow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HoldAction::None => "不执行",
            HoldAction::PlayFromCurrent => "从当前页播放",
            HoldAction::BlackScreen => "屏幕变黑 / 恢复",
            HoldAction::EndShow => "结束放映",
        }
    }

    fn key(self) -> &'static str {
        match self {
            HoldAction::None => "none",
            HoldAction::PlayFromCurrent => "play-from-current",
            HoldAction::BlackScreen => "black-screen",
            HoldAction::EndShow => "end-show",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Effects included in the double-click cycle, in `EFFECTS` order.
    pub cycle: [bool; 4],
    pub next_hold: HoldAction,
    pub back_hold: HoldAction,
    /// Minutes after the timer starts; each set timer vibrates once.
    pub timers: [Option<u32>; MAX_TIMERS],
    /// Spotlight/magnifier radius in points.
    pub radius: f64,
    pub shade: f64,
    pub zoom: f64,
    pub screen_reminder: ScreenReminder,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cycle: [true, true, false, false],
            next_hold: HoldAction::PlayFromCurrent,
            back_hold: HoldAction::BlackScreen,
            timers: [None; MAX_TIMERS],
            radius: DEFAULT_RADIUS,
            shade: DEFAULT_SHADE,
            zoom: 2.0,
            screen_reminder: ScreenReminder::OnVibrationFailure,
        }
    }
}

/// Timer field text: empty means unused; otherwise 1..=MAX_TIMER_MINUTES.
pub fn parse_minutes(text: &str) -> Result<Option<u32>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<u32>() {
        Ok(m) if (1..=MAX_TIMER_MINUTES).contains(&m) => Ok(Some(m)),
        _ => Err(format!("定时须为 1–{MAX_TIMER_MINUTES} 的整数分钟：{text}")),
    }
}

impl Settings {
    /// Lenient: unknown keys and invalid values keep their defaults, so a
    /// damaged or older file never prevents the app from starting.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "cycle" => {
                    let mut cycle = [false; 4];
                    for name in value.split(',').map(str::trim) {
                        if let Some(slot) = EFFECT_KEYS.iter().position(|k| *k == name) {
                            cycle[slot] = true;
                        }
                    }
                    settings.cycle = cycle;
                }
                "next-hold" => {
                    if let Some(action) = HoldAction::from_key(value) {
                        settings.next_hold = action;
                    }
                }
                "back-hold" => {
                    if let Some(action) = HoldAction::from_key(value) {
                        settings.back_hold = action;
                    }
                }
                "timers" => {
                    let mut timers = [None; MAX_TIMERS];
                    for (slot, item) in timers.iter_mut().zip(value.split(',')) {
                        *slot = parse_minutes(item).ok().flatten();
                    }
                    settings.timers = timers;
                }
                "screen-reminder" => {
                    if let Some(mode) = ScreenReminder::ALL.into_iter().find(|m| m.key() == value) {
                        settings.screen_reminder = mode;
                    }
                }
                "shade" => {
                    if let Ok(shade) = value.parse::<f64>() {
                        settings.shade = clamp_shade(shade);
                    }
                }
                "zoom" => {
                    if let Ok(zoom) = value.parse::<f64>() {
                        if zoom.is_finite() {
                            settings.zoom = nearest_zoom(zoom);
                        }
                    }
                }
                "radius" => {
                    if let Ok(radius) = value.parse::<f64>() {
                        settings.radius = clamp_radius(radius);
                    }
                }
                _ => {}
            }
        }
        settings
    }

    pub fn serialize(&self) -> String {
        let cycle: Vec<_> = EFFECT_KEYS
            .iter()
            .zip(self.cycle)
            .filter(|(_, on)| *on)
            .map(|(name, _)| *name)
            .collect();
        let timers: Vec<_> = self
            .timers
            .iter()
            .map(|t| t.map(|m| m.to_string()).unwrap_or_default())
            .collect();
        format!(
            "cycle={}\nnext-hold={}\nback-hold={}\ntimers={}\nradius={}\nshade={:.2}\nzoom={}\nscreen-reminder={}\n",
            cycle.join(","),
            self.next_hold.key(),
            self.back_hold.key(),
            timers.join(","),
            self.radius.round(),
            self.shade,
            self.zoom,
            self.screen_reminder.key()
        )
    }

    /// Next effect after `current` among selected ones. Magnify is skipped
    /// without screen-recording permission. None when nothing is selectable.
    pub fn next_effect(&self, current: Effect, magnify_allowed: bool) -> Option<Effect> {
        let candidates: Vec<_> = EFFECTS
            .iter()
            .zip(self.cycle)
            .filter(|(e, on)| *on && (**e != Effect::Magnify || magnify_allowed))
            .map(|(e, _)| *e)
            .collect();
        let position = candidates.iter().position(|e| *e == current);
        match position {
            Some(i) => Some(candidates[(i + 1) % candidates.len()]),
            None => candidates.first().copied(),
        }
    }
}

/// Elapsed-time timer; each configured minute mark fires once per run.
#[derive(Debug, Default)]
pub struct PresentationTimer {
    started: Option<f64>,
    fired: [bool; MAX_TIMERS],
}

impl PresentationTimer {
    pub fn start(&mut self, now: f64) {
        self.started = Some(now);
        self.fired = [false; MAX_TIMERS];
    }
    pub fn stop(&mut self) {
        self.started = None;
    }
    pub fn elapsed(&self, now: f64) -> Option<f64> {
        self.started.map(|start| (now - start).max(0.0))
    }
    /// Timer slots that became due since the last call.
    pub fn due(&mut self, now: f64, timers: &[Option<u32>; MAX_TIMERS]) -> Vec<usize> {
        let Some(elapsed) = self.elapsed(now) else {
            return Vec::new();
        };
        let mut due = Vec::new();
        for (slot, minutes) in timers.iter().enumerate() {
            if let Some(minutes) = minutes {
                if !self.fired[slot] && elapsed >= f64::from(*minutes) * 60.0 {
                    self.fired[slot] = true;
                    due.push(slot);
                }
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_requested_behaviour() {
        let s = Settings::default();
        assert_eq!(s.cycle, [true, true, false, false]);
        assert_eq!(s.screen_reminder, ScreenReminder::OnVibrationFailure);
        assert_eq!(s.next_hold, HoldAction::PlayFromCurrent);
        assert_eq!(s.back_hold, HoldAction::BlackScreen);
        assert_eq!(s.timers, [None; 3]);
    }

    #[test]
    fn settings_round_trip_and_tolerate_bad_input() {
        let s = Settings {
            cycle: [false, true, true, true],
            next_hold: HoldAction::EndShow,
            back_hold: HoldAction::None,
            timers: [Some(15), None, Some(30)],
            radius: 220.0,
            shade: 0.45,
            zoom: 3.0,
            screen_reminder: ScreenReminder::Always,
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
        let damaged = Settings::parse("junk\nnext-hold=explode\ntimers=0,abc,601\ncycle=\n");
        assert_eq!(damaged.next_hold, HoldAction::PlayFromCurrent);
        assert_eq!(damaged.timers, [None; 3]);
        assert_eq!(damaged.cycle, [false; 4]);
        assert_eq!(Settings::parse("radius=9999\n").radius, 500.0);
        assert_eq!(Settings::parse("radius=NaN\n").radius, DEFAULT_RADIUS);
        assert_eq!(Settings::parse("shade=5\n").shade, 0.9);
        assert_eq!(Settings::parse("shade=inf\n").shade, DEFAULT_SHADE);
        assert_eq!(Settings::parse("zoom=2.4\n").zoom, 2.0);
        assert_eq!(Settings::parse("zoom=99\n").zoom, 4.0);
    }

    #[test]
    fn minute_fields_are_validated() {
        assert_eq!(parse_minutes(" "), Ok(None));
        assert_eq!(parse_minutes(" 25 "), Ok(Some(25)));
        for bad in ["0", "601", "-1", "1.5", "x"] {
            assert!(parse_minutes(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn double_click_cycles_only_selected_effects() {
        let mut s = Settings::default();
        assert_eq!(s.next_effect(Effect::Spotlight, true), Some(Effect::Laser));
        assert_eq!(s.next_effect(Effect::Laser, true), Some(Effect::Spotlight));
        // Current effect outside the cycle jumps to the first selected one.
        assert_eq!(
            s.next_effect(Effect::Magnify, true),
            Some(Effect::Spotlight)
        );
        s.cycle = [true, false, true, false];
        assert_eq!(
            s.next_effect(Effect::Spotlight, true),
            Some(Effect::Magnify)
        );
        assert_eq!(
            s.next_effect(Effect::Spotlight, false),
            Some(Effect::Spotlight)
        );
        s.cycle = [false, false, true, false];
        assert_eq!(s.next_effect(Effect::Spotlight, false), None);
        s.cycle = [false; 4];
        assert_eq!(s.next_effect(Effect::Laser, true), None);
        s.cycle = [true, false, false, true];
        assert_eq!(s.next_effect(Effect::Spotlight, false), Some(Effect::Box));
        assert_eq!(s.next_effect(Effect::Box, false), Some(Effect::Spotlight));
    }

    #[test]
    fn screen_reminder_modes() {
        assert!(!ScreenReminder::Off.shows(false));
        assert!(ScreenReminder::OnVibrationFailure.shows(false));
        assert!(!ScreenReminder::OnVibrationFailure.shows(true));
        assert!(ScreenReminder::Always.shows(true));
    }

    #[test]
    fn each_timer_fires_once_at_its_minute_mark() {
        let timers = [Some(1), None, Some(2)];
        let mut t = PresentationTimer::default();
        assert!(t.due(1000.0, &timers).is_empty(), "not started");
        t.start(100.0);
        assert!(t.due(159.9, &timers).is_empty());
        assert_eq!(t.due(160.0, &timers), vec![0]);
        assert!(t.due(200.0, &timers).is_empty());
        assert_eq!(t.due(400.0, &timers), vec![2]);
        assert!(t.due(10_000.0, &timers).is_empty());
        t.start(500.0);
        assert_eq!(t.due(700.0, &timers), vec![0, 2], "restart re-arms timers");
        t.stop();
        assert!(t.due(10_000.0, &timers).is_empty());
        assert_eq!(t.elapsed(1.0), None);
    }
}
