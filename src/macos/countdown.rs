//! Audience-facing classroom countdown windows and page-key takeover wiring.
use super::*;
use orange_beam::countdown::{Change, Countdown};

/// Whether the setup panel may promise that the page keys adjust minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdjustHint {
    /// Takeover asked for, device has not answered: promise nothing yet.
    Pending,
    Available,
    /// Refused by the device or another client; setup still runs.
    Unavailable,
}

/// What every display shows while a countdown is being set, running or up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CountdownLook {
    /// Centred and large, with the key hints for adjustment.
    Setup { minutes: u32, adjust: AdjustHint },
    /// Top right and small; red in the last ten seconds.
    Running { seconds: u32 },
    /// Centred and large for FINISHED_SECONDS.
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountdownMode {
    Setup,
    Running,
    Up,
}

impl CountdownLook {
    fn mode(self) -> CountdownMode {
        match self {
            Self::Setup { .. } => CountdownMode::Setup,
            Self::Running { .. } => CountdownMode::Running,
            Self::Up => CountdownMode::Up,
        }
    }
    fn text(self) -> String {
        match self {
            Self::Setup { minutes, .. } => format!("{minutes} 分钟"),
            Self::Running { seconds } => format!("{:02}:{:02}", seconds / 60, seconds % 60),
            Self::Up => "时间到".to_string(),
        }
    }
    /// Second line of the setup panel; it follows whether the device accepted
    /// the takeover of the short page keys. A takeover in progress must not
    /// look like a failure, so it only promises the automatic start.
    fn hint(self) -> Option<String> {
        match self {
            Self::Setup {
                adjust: AdjustHint::Available,
                ..
            } => Some("下一页 +1 分钟 · 上一页 −1 分钟 · 3 秒后开始".to_string()),
            Self::Setup {
                minutes,
                adjust: AdjustHint::Unavailable,
            } => Some(format!("无法用翻页键调整，3 秒后按 {minutes} 分钟开始")),
            Self::Setup {
                adjust: AdjustHint::Pending,
                ..
            } => Some("3 秒后开始".to_string()),
            _ => None,
        }
    }
    fn red(self) -> bool {
        matches!(self, Self::Running { seconds } if seconds <= 10)
    }
}

/// Audience-facing countdown panel for one display: borderless, never key,
/// click-through, and one level above the black screen (as the toast is).
pub(super) struct CountdownWindow {
    panel: Retained<OverlayPanel>,
    time: Retained<NSTextField>,
    hint: Retained<NSTextField>,
    frame: NSRect,
    /// Applied layout, so a minute or second change only updates text.
    mode: Cell<Option<CountdownMode>>,
    shown: Cell<bool>,
    red: Cell<bool>,
}

impl CountdownWindow {
    pub(super) fn new(mtm: MainThreadMarker, screen: &NSScreen) -> Self {
        Self::new_for_frame(mtm, screen.frame())
    }
    /// Same panel for an explicit screen rectangle, so the layout rules can be
    /// checked on a runner without displays.
    pub(super) fn new_for_frame(mtm: MainThreadMarker, frame: NSRect) -> Self {
        // SAFETY: AppKit calls stay on main; the panel is retained here and
        // release-on-close is disabled before it can be shown.
        unsafe {
            let panel: Retained<OverlayPanel> = msg_send![OverlayPanel::alloc(mtm), initWithContentRect: NSRect::new(NSPoint::ZERO, NSSize::new(10.0, 10.0)), styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel, backing: NSBackingStoreType::Buffered, defer: true];
            panel.setReleasedWhenClosed(false);
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.08, 0.08, 0.08, 0.9,
            )));
            panel.setHasShadow(true);
            // Clicks belong to the app below, so a running countdown never
            // interrupts the presentation.
            panel.setIgnoresMouseEvents(true);
            panel.setHidesOnDeactivate(false);
            panel.setLevel(NSScreenSaverWindowLevel + 1);
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
            panel.setTitle(ns_string!("Orange Beam Countdown"));
            let time = NSTextField::labelWithString(ns_string!(""), mtm);
            time.setTextColor(Some(&NSColor::whiteColor()));
            time.setAlignment(NSTextAlignment::Center);
            let hint = NSTextField::labelWithString(ns_string!(""), mtm);
            hint.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                0.85, 0.85, 0.85, 1.0,
            )));
            hint.setAlignment(NSTextAlignment::Center);
            let view = panel.contentView().expect("panel content view");
            view.addSubview(&time);
            view.addSubview(&hint);
            panel.orderOut(None);
            Self {
                panel,
                time,
                hint,
                frame,
                mode: Cell::new(None),
                shown: Cell::new(false),
                red: Cell::new(false),
            }
        }
    }
    fn layout(&self, mode: CountdownMode) {
        let screen = self.frame;
        let (size, origin) = match mode {
            CountdownMode::Running => {
                let size = NSSize::new(320.0, 104.0);
                (
                    size,
                    NSPoint::new(
                        screen.origin.x + screen.size.width - size.width - 32.0,
                        screen.origin.y + screen.size.height - size.height - 32.0,
                    ),
                )
            }
            CountdownMode::Setup | CountdownMode::Up => {
                let width = (screen.size.width - 160.0).clamp(420.0, 760.0);
                let size = NSSize::new(width, 240.0);
                (
                    size,
                    NSPoint::new(
                        screen.origin.x + (screen.size.width - width) / 2.0,
                        screen.origin.y + (screen.size.height - size.height) / 2.0,
                    ),
                )
            }
        };
        let target = NSRect::new(origin, size);
        if self.panel.frame() != target {
            self.panel.setFrame_display(target, false);
        }
        let (font, time_frame) = match mode {
            CountdownMode::Running => (
                48.0,
                NSRect::new(NSPoint::new(0.0, 26.0), NSSize::new(size.width, 60.0)),
            ),
            CountdownMode::Setup => (
                96.0,
                NSRect::new(NSPoint::new(0.0, 110.0), NSSize::new(size.width, 120.0)),
            ),
            CountdownMode::Up => (
                96.0,
                NSRect::new(NSPoint::new(0.0, 62.0), NSSize::new(size.width, 120.0)),
            ),
        };
        self.time.setFont(Some(&NSFont::boldSystemFontOfSize(font)));
        self.time.setFrame(time_frame);
        self.hint.setFont(Some(&NSFont::systemFontOfSize(20.0)));
        self.hint.setFrame(NSRect::new(
            NSPoint::new(0.0, 56.0),
            NSSize::new(size.width, 28.0),
        ));
        self.hint.setHidden(mode != CountdownMode::Setup);
    }
    pub(super) fn render(&self, look: Option<CountdownLook>) {
        let Some(look) = look else {
            self.mode.set(None);
            if self.shown.replace(false) {
                self.panel.orderOut(None);
            }
            return;
        };
        if self.mode.get() != Some(look.mode()) {
            self.layout(look.mode());
            self.mode.set(Some(look.mode()));
        }
        set_field_text_if_changed(&self.time, &look.text());
        if let Some(hint) = look.hint() {
            set_field_text_if_changed(&self.hint, &hint);
        }
        if self.red.get() != look.red() {
            self.red.set(look.red());
            let color = if look.red() {
                NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.32, 0.28, 1.0)
            } else {
                NSColor::whiteColor()
            };
            self.time.setTextColor(Some(&color));
        }
        if !self.shown.replace(true) {
            self.panel.orderFrontRegardless();
        }
    }
    pub(super) fn retire(&self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
    /// Read-only accessors for the offline checks in `render_qa`.
    pub(super) fn qa_panel(&self) -> &OverlayPanel {
        &self.panel
    }
    pub(super) fn qa_time_text(&self) -> String {
        self.time.stringValue().to_string()
    }
    pub(super) fn qa_hint_text(&self) -> String {
        self.hint.stringValue().to_string()
    }
    pub(super) fn qa_is_red(&self) -> bool {
        self.red.get()
    }
}

/// How far the takeover of the short page keys has got. It decides the setup
/// hint and whether a command still has to be sent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PageKeysState {
    /// Undiverted: the device sends arrow keys natively.
    Native,
    /// Command queued, no answer yet.
    Requesting,
    /// Diverted by this session; short presses adjust the minutes.
    Diverted,
    /// The device or another client refused; setup runs without adjustment.
    Refused,
}

impl Delegate {
    /// Long press configured as "课堂倒计时" and the menu item share this entry:
    /// idle starts setup, anything else returns to idle.
    pub(super) fn toggle_countdown(&self) {
        let now = self.now();
        let last = self.ivars().settings.borrow().countdown_minutes;
        let before = self.ivars().countdown.borrow().wants_page_keys();
        let after = {
            let mut countdown = self.ivars().countdown.borrow_mut();
            countdown.toggle(now, last);
            countdown.wants_page_keys()
        };
        if !before && after && self.ivars().page_keys.get() == PageKeysState::Refused {
            // A new setup retries a takeover the device refused last time.
            self.ivars().page_keys.set(PageKeysState::Native);
        }
        self.sync_page_keys();
        self.tick();
    }

    /// Short page-key press during setup. Every other state ignores it, so the
    /// same keys keep turning slides once the countdown runs.
    pub(super) fn adjust_countdown(&self, delta: i32) {
        self.ivars()
            .countdown
            .borrow_mut()
            .adjust(delta, self.now());
        self.tick();
    }

    /// ⌃⌥⌘H also ends setup or a running countdown, so the page keys stop
    /// being diverted at the same moment.
    pub(super) fn cancel_countdown(&self) {
        self.ivars().countdown.borrow_mut().cancel();
        self.sync_page_keys();
    }

    /// The device thread diverts the short page keys exactly while setup asks
    /// for them, so one change of `wants_page_keys` means one command.
    fn sync_page_keys(&self) {
        let want = self.ivars().countdown.borrow().wants_page_keys();
        match (want, self.ivars().page_keys.get()) {
            (true, PageKeysState::Native) => {
                let queued = self
                    .ivars()
                    .remote
                    .borrow()
                    .as_ref()
                    .is_some_and(|remote| remote.page_keys(true));
                // Without a remote the setup still runs; only adjustment is lost.
                self.ivars().page_keys.set(if queued {
                    PageKeysState::Requesting
                } else {
                    PageKeysState::Refused
                });
            }
            (false, PageKeysState::Requesting | PageKeysState::Diverted) => {
                if let Some(remote) = self.ivars().remote.borrow().as_ref() {
                    remote.page_keys(false);
                }
                self.ivars().page_keys.set(PageKeysState::Native);
            }
            (false, PageKeysState::Refused) => self.ivars().page_keys.set(PageKeysState::Native),
            _ => {}
        }
    }

    /// What every display shows now; None hides the countdown panels.
    fn countdown_look(&self, now: f64) -> Option<CountdownLook> {
        let state = *self.ivars().countdown.borrow();
        match state {
            Countdown::Idle => None,
            Countdown::Setup { minutes, .. } => Some(CountdownLook::Setup {
                minutes,
                adjust: match self.ivars().page_keys.get() {
                    PageKeysState::Diverted => AdjustHint::Available,
                    PageKeysState::Refused => AdjustHint::Unavailable,
                    PageKeysState::Native | PageKeysState::Requesting => AdjustHint::Pending,
                },
            }),
            Countdown::Running { .. } => Some(CountdownLook::Running {
                seconds: state.remaining(now).unwrap_or(0),
            }),
            Countdown::Finished { .. } => Some(CountdownLook::Up),
        }
    }

    /// Advances the countdown, keeps the takeover in step and repaints the
    /// panels. An idle countdown only compares one value per frame.
    pub(super) fn update_countdown(&self, now: f64) {
        let change = self.ivars().countdown.borrow_mut().tick(now);
        if let Some(change) = change {
            self.countdown_changed(change);
        }
        self.sync_page_keys();
        let look = self.countdown_look(now);
        for window in self.ivars().countdown_windows.borrow().iter() {
            window.render(look);
        }
    }

    fn countdown_changed(&self, change: Change) {
        match change {
            Change::Started(minutes) => {
                eprintln!("COUNTDOWN started minutes={minutes}");
                let changed = {
                    let mut settings = self.ivars().settings.borrow_mut();
                    let changed = settings.countdown_minutes != minutes;
                    settings.countdown_minutes = minutes;
                    changed
                };
                if changed {
                    self.save_settings();
                }
            }
            Change::TimeUp => {
                let vibrated = self
                    .ivars()
                    .remote
                    .borrow()
                    .as_ref()
                    .is_some_and(|remote| remote.vibrate("⏰ 课堂倒计时：时间到".to_string()));
                eprintln!("COUNTDOWN time up vibrated={vibrated}");
                // The panel itself is the screen notice; a missing vibration
                // only changes this status line.
                self.set_message(if vibrated {
                    "课堂倒计时：时间到（已振动）。"
                } else {
                    "课堂倒计时：时间到（遥控器未连接，未振动）。"
                });
            }
            Change::Closed => {}
        }
    }

    /// While the countdown is being set, the other hold key must do nothing: a
    /// stray black screen or a closed show would surprise the presenter.
    pub(super) fn hold_allowed(&self, action: HoldAction) -> bool {
        action == HoldAction::Countdown || !self.ivars().countdown.borrow().wants_page_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_hint_follows_takeover_state_and_last_ten_seconds_turn_red() {
        let pending = CountdownLook::Setup {
            minutes: 5,
            adjust: AdjustHint::Pending,
        };
        assert_eq!(pending.text(), "5 分钟");
        assert_eq!(pending.hint().as_deref(), Some("3 秒后开始"));
        assert!(!pending.red());

        let available = CountdownLook::Setup {
            minutes: 5,
            adjust: AdjustHint::Available,
        };
        assert_eq!(
            available.hint().as_deref(),
            Some("下一页 +1 分钟 · 上一页 −1 分钟 · 3 秒后开始")
        );

        let unavailable = CountdownLook::Setup {
            minutes: 5,
            adjust: AdjustHint::Unavailable,
        };
        assert_eq!(
            unavailable.hint().as_deref(),
            Some("无法用翻页键调整，3 秒后按 5 分钟开始")
        );

        let running_11 = CountdownLook::Running { seconds: 11 };
        assert_eq!(running_11.text(), "00:11");
        assert_eq!(running_11.hint(), None);
        assert!(!running_11.red());

        let running_10 = CountdownLook::Running { seconds: 10 };
        assert_eq!(running_10.text(), "00:10");
        assert!(running_10.red());

        assert_eq!(CountdownLook::Up.text(), "时间到");
        assert!(!CountdownLook::Up.red());
    }
}
