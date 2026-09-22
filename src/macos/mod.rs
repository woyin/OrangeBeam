//! Native AppKit shell. All window and drawing work stays on the main thread.
mod capture;
mod hotkey;
pub mod input_access;
mod remote;
pub mod render_qa;
use capture::Capture;
use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::*;
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess,
    CGRequestPostEventAccess, CGRequestScreenCaptureAccess,
};
use objc2_foundation::*;
use spotlight_rs::controls::{format_unrestored, parse_unrestored, Reporting};
use spotlight_rs::presentation::{BoxView, Effect, Point, Presentation, Rect};
use spotlight_rs::settings::{
    parse_minutes, HoldAction, PresentationTimer, ScreenReminder, Settings, EFFECTS,
};
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
struct ViewData {
    effect: Cell<Effect>,
    center: Cell<NSPoint>,
    radius: Cell<f64>,
    shade: Cell<f64>,
    zoom: Cell<f64>,
    image: RefCell<Option<Retained<NSImage>>>,
    sequence: Cell<u64>,
    black: Cell<bool>,
    /// Box effect content in view coordinates.
    box_draw: Cell<BoxDraw>,
}

/// How long a double-click switch shows the newly selected effect.
const SWITCH_FLASH_SECONDS: f64 = 1.2;

#[derive(Default, Clone, Copy, PartialEq)]
enum BoxDraw {
    #[default]
    None,
    Aim(NSPoint),
    Anchor(NSPoint),
    Rect(NSRect),
}

/// Crosshair marking a box corner; `fixed` draws the confirmed start corner.
fn draw_corner_marker(point: NSPoint, fixed: bool) {
    let arm = 18.0;
    for (width, color) in [
        (
            5.0,
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.6),
        ),
        (
            2.0,
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.95),
        ),
    ] {
        let path = NSBezierPath::bezierPath();
        path.moveToPoint(NSPoint::new(point.x - arm, point.y));
        path.lineToPoint(NSPoint::new(point.x + arm, point.y));
        path.moveToPoint(NSPoint::new(point.x, point.y - arm));
        path.lineToPoint(NSPoint::new(point.x, point.y + arm));
        path.setLineWidth(width);
        color.setStroke();
        path.stroke();
    }
    let r = if fixed { 6.0 } else { 9.0 };
    let dot = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
        NSPoint::new(point.x - r, point.y - r),
        NSSize::new(r * 2.0, r * 2.0),
    ));
    dot.setLineWidth(2.0);
    if fixed {
        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.8, 0.1, 1.0).setFill();
        dot.fill();
    }
    NSColor::whiteColor().setStroke();
    dot.stroke();
}

define_class!(
    // SAFETY: NSView subclass is confined to AppKit's main thread.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewData]
    struct OverlayView;
    unsafe impl NSObjectProtocol for OverlayView {}
    impl OverlayView {
        #[unsafe(method(isOpaque))]
        fn opaque(&self) -> bool { false }
        #[unsafe(method(drawRect:))]
        fn draw(&self, _dirty: NSRect) {
            let data = self.ivars();
            let bounds = self.bounds();
            let center = data.center.get();
            let radius = data.radius.get();
            let circle = NSRect::new(NSPoint::new(center.x - radius, center.y - radius), NSSize::new(radius * 2.0, radius * 2.0));
            // Clear the previous frame explicitly to avoid trails in a transparent window.
            NSColor::clearColor().setFill();
            NSRectFillUsingOperation(bounds, NSCompositingOperation::Copy);
            if data.black.get() {
                NSColor::blackColor().setFill();
                NSRectFillUsingOperation(bounds, NSCompositingOperation::Copy);
                return;
            }
            match data.effect.get() {
                Effect::Spotlight | Effect::Magnify => {
                    let mask = NSBezierPath::bezierPathWithRect(bounds);
                    mask.appendBezierPathWithOvalInRect(circle);
                    mask.setWindingRule(NSWindingRule::EvenOdd);
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, data.shade.get()).setFill();
                    mask.fill();
                    if data.effect.get() == Effect::Magnify {
                        if let Some(image) = data.image.borrow().as_ref() {
                            NSGraphicsContext::saveGraphicsState_class();
                            NSBezierPath::bezierPathWithOvalInRect(circle).addClip();
                            let zoom = data.zoom.get();
                            let destination = NSRect::new(NSPoint::new(center.x * (1.0-zoom), center.y * (1.0-zoom)), NSSize::new(bounds.size.width * zoom, bounds.size.height * zoom));
                            image.drawInRect_fromRect_operation_fraction(destination, NSRect::ZERO, NSCompositingOperation::SourceOver, 1.0);
                            NSGraphicsContext::restoreGraphicsState_class();
                        }
                    }
                    let border = NSBezierPath::bezierPathWithOvalInRect(circle);
                    border.setLineWidth(2.0);
                    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.85).setStroke();
                    border.stroke();
                }
                Effect::Box => match data.box_draw.get() {
                    BoxDraw::None => {}
                    BoxDraw::Aim(point) => draw_corner_marker(point, false),
                    BoxDraw::Anchor(point) => draw_corner_marker(point, true),
                    BoxDraw::Rect(rect) => {
                        let mask = NSBezierPath::bezierPathWithRect(bounds);
                        mask.appendBezierPathWithRect(rect);
                        mask.setWindingRule(NSWindingRule::EvenOdd);
                        NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, data.shade.get()).setFill();
                        mask.fill();
                        let border = NSBezierPath::bezierPathWithRect(rect);
                        border.setLineWidth(2.0);
                        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.85).setStroke();
                        border.stroke();
                    }
                },
                Effect::Laser => {
                    for (r, alpha) in [(17.0, 0.15), (11.0, 0.25), (6.0, 1.0)] {
                        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.18, 0.20, alpha).setFill();
                        NSBezierPath::bezierPathWithOvalInRect(NSRect::new(NSPoint::new(center.x-r, center.y-r), NSSize::new(r*2.0, r*2.0))).fill();
                    }
                    NSColor::whiteColor().setFill();
                    NSBezierPath::bezierPathWithOvalInRect(NSRect::new(NSPoint::new(center.x-2.0, center.y-2.0), NSSize::new(4.0, 4.0))).fill();
                }
            }
        }
    }
);

define_class!(
    // SAFETY: Non-activating panel overrides have the documented BOOL signatures.
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    struct OverlayPanel;
    unsafe impl NSObjectProtocol for OverlayPanel {}
    impl OverlayPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_key(&self) -> bool { false }
        #[unsafe(method(canBecomeMainWindow))]
        fn can_main(&self) -> bool { false }
    }
);

struct Overlay {
    panel: Retained<OverlayPanel>,
    view: Retained<OverlayView>,
    frame: NSRect,
    display_id: u32,
    scale: f64,
}

fn display_id(screen: &NSScreen) -> u32 {
    screen
        .deviceDescription()
        .objectForKey(&NSString::from_str("NSScreenNumber"))
        .and_then(|value| value.downcast::<NSNumber>().ok())
        .map(|number| number.unsignedIntValue())
        .unwrap_or(0)
}

impl Overlay {
    fn new(mtm: MainThreadMarker, screen: &NSScreen) -> Self {
        let frame = screen.frame();
        let display_id = display_id(screen);
        // SAFETY: AppKit calls use the main thread. Panel ownership is retained here,
        // and automatic release-on-close is disabled before the panel can be shown.
        unsafe {
            let panel: Retained<OverlayPanel> = msg_send![OverlayPanel::alloc(mtm), initWithContentRect: frame, styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel, backing: NSBackingStoreType::Buffered, defer: false];
            panel.setReleasedWhenClosed(false);
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setHasShadow(false);
            panel.setIgnoresMouseEvents(true);
            panel.setHidesOnDeactivate(false);
            panel.setLevel(NSFloatingWindowLevel);
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
            panel.setTitle(ns_string!("Spotlight RS Overlay"));
            let view = OverlayView::alloc(mtm).set_ivars(ViewData::default());
            let view: Retained<OverlayView> =
                msg_send![super(view), initWithFrame: NSRect::new(NSPoint::ZERO, frame.size)];
            panel.setContentView(Some(&view));
            Self {
                panel,
                view,
                frame,
                display_id,
                scale: screen.backingScaleFactor(),
            }
        }
    }
    fn hide(&self) {
        self.panel.orderOut(None);
        self.view.ivars().image.replace(None);
    }
    fn render_blackout(&self) {
        // Above the menu bar and Dock, which sit over the floating level.
        self.panel.setLevel(NSScreenSaverWindowLevel);
        let data = self.view.ivars();
        data.image.replace(None);
        if !data.black.replace(true) || !self.panel.isVisible() {
            self.view.setNeedsDisplay(true);
        }
        if !self.panel.isVisible() {
            self.panel.orderFrontRegardless();
        }
    }
    fn global_frame(&self) -> Rect {
        Rect {
            x: self.frame.origin.x,
            y: self.frame.origin.y,
            width: self.frame.size.width,
            height: self.frame.size.height,
        }
    }
    /// Box effect: a rectangle shades every display it touches; corner
    /// markers appear only on the display that contains them.
    fn render_box(&self, view: BoxView, state: &Presentation) {
        let screen = self.global_frame();
        let local_point = |p: Point| screen.local_point(p).map(|l| NSPoint::new(l.x, l.y));
        let draw = match view {
            BoxView::Aim(p) => local_point(p).map(BoxDraw::Aim),
            BoxView::Anchor(p) => local_point(p).map(BoxDraw::Anchor),
            BoxView::Rect(rect) => screen.intersects(rect).then(|| {
                BoxDraw::Rect(NSRect::new(
                    NSPoint::new(rect.x - screen.x, rect.y - screen.y),
                    NSSize::new(rect.width, rect.height),
                ))
            }),
        };
        let Some(draw) = draw else {
            self.hide();
            return;
        };
        self.panel.setLevel(NSFloatingWindowLevel);
        let data = self.view.ivars();
        data.image.replace(None);
        let changed = data.black.replace(false)
            || data.effect.replace(Effect::Box) != Effect::Box
            || data.box_draw.replace(draw) != draw
            || data.shade.replace(state.shade) != state.shade;
        if changed || !self.panel.isVisible() {
            self.view.setNeedsDisplay(true);
        }
        if !self.panel.isVisible() {
            self.panel.orderFrontRegardless();
        }
    }
    fn render(&self, pointer: NSPoint, state: &Presentation, capture: &Capture) {
        let rect = Rect {
            x: self.frame.origin.x,
            y: self.frame.origin.y,
            width: self.frame.size.width,
            height: self.frame.size.height,
        };
        let Some(local) = rect.local_point(Point {
            x: pointer.x,
            y: pointer.y,
        }) else {
            self.hide();
            return;
        };
        let data = self.view.ivars();
        let mut fresh_image = false;
        if state.effect == Effect::Magnify {
            let Some((sequence, image)) = capture.frame() else {
                self.hide();
                return;
            };
            if sequence != data.sequence.get() || data.image.borrow().is_none() {
                data.image.replace(Some(NSImage::initWithCGImage_size(
                    NSImage::alloc(),
                    &image,
                    self.frame.size,
                )));
                data.sequence.set(sequence);
                fresh_image = true;
            }
        } else {
            data.image.replace(None);
        }
        self.panel.setLevel(NSFloatingWindowLevel);
        let center = NSPoint::new(local.x, local.y);
        let changed = data.black.replace(false)
            || fresh_image
            || data.zoom.get() != state.zoom
            || data.center.get() != center
            || data.effect.get() != state.effect
            || data.radius.get() != state.radius
            || data.shade.get() != state.shade;
        data.center.set(center);
        data.effect.set(state.effect);
        data.radius.set(state.radius);
        data.shade.set(state.shade);
        data.zoom.set(state.zoom);
        if changed || !self.panel.isVisible() {
            self.view.setNeedsDisplay(true);
        }
        if !self.panel.isVisible() {
            self.panel.orderFrontRegardless();
        }
    }
}

struct AppData {
    start: Instant,
    presentation: RefCell<Presentation>,
    overlays: RefCell<Vec<Overlay>>,
    status: RefCell<Option<Retained<NSStatusItem>>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    fast_timer: Cell<bool>,
    screen_check: Cell<f64>,
    exit_after: Option<f64>,
    initial_demo: Option<(Effect, f64)>,
    effect_items: RefCell<Vec<Retained<NSMenuItem>>>,
    capture: Retained<Capture>,
    controls: RefCell<Option<Retained<NSWindow>>>,
    status_label: RefCell<Option<Retained<NSTextField>>>,
    message: RefCell<String>,
    hotkeys: RefCell<Option<hotkey::HotKeys>>,
    capture_allowed: Cell<bool>,
    remote: RefCell<Option<remote::Remote>>,
    remote_epoch: Cell<u64>,
    // Keep a main-thread owner until after the worker joins. MainThreadBound's
    // final drop must never synchronously dispatch to a main thread joining it.
    remote_target: RefCell<Option<Arc<MainThreadBound<Retained<Delegate>>>>>,
    remote_button: RefCell<Option<Retained<NSButton>>>,
    remote_item: RefCell<Option<Retained<NSMenuItem>>>,
    remote_label: RefCell<Option<Retained<NSTextField>>>,
    remote_message: RefCell<String>,
    signals: crate::stop_signals::StopSignals,
    closing: Cell<bool>,
    settings: RefCell<Settings>,
    talk_timer: RefCell<PresentationTimer>,
    cycle_boxes: RefCell<Vec<Retained<NSButton>>>,
    hold_popups: RefCell<Vec<Retained<NSPopUpButton>>>,
    timer_fields: RefCell<Vec<Retained<NSTextField>>>,
    timer_button: RefCell<Option<Retained<NSButton>>>,
    timer_item: RefCell<Option<Retained<NSMenuItem>>>,
    auto_reconnect: Cell<bool>,
    retry_at: Cell<f64>,
    toast: RefCell<Option<(Retained<OverlayPanel>, Retained<NSTextField>)>>,
    toast_until: Cell<f64>,
    reminder_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    unrestored: RefCell<remote::Unrestored>,
}

define_class!(
    // SAFETY: AppKit delegate and its state are used only on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppData]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn launched(&self, _notification: &NSNotification) {
            self.setup_menu();
            // SAFETY: the delegate owns the registration and unregisters on termination.
            match unsafe { hotkey::HotKeys::new((self as *const Self).cast_mut().cast(), hotkey_action) } {
                Ok(keys) => { self.ivars().hotkeys.replace(Some(keys)); }
                Err(error) => self.set_message(&error),
            }
            self.refresh_screens();
            if let Some((effect, seconds)) = self.ivars().initial_demo {
                let mut state = self.ivars().presentation.borrow_mut();
                state.effect = effect;
                state.preview(self.now(), seconds);
            }
            if self.ivars().initial_demo.is_none() { self.show_controls(); self.start_remote(); }
            self.install_timer(false);
            self.tick();
        }
        #[unsafe(method(applicationWillTerminate:))]
        fn terminate(&self, _notification: &NSNotification) {
            self.ivars().closing.set(true);
            if let Some(timer) = self.ivars().timer.borrow_mut().take() { timer.invalidate(); }
            self.ivars().hotkeys.replace(None);
            self.ivars().capture.stop();
            for overlay in self.ivars().overlays.borrow().iter() { overlay.hide(); }
            let _ = self.stop_remote(); // errors are logged inside
        }
    }
    impl Delegate {
        #[unsafe(method(tick:))]
        fn on_timer(&self, _timer: &NSTimer) { self.tick(); }
        #[unsafe(method(toggle:))]
        fn toggle(&self, _sender: Option<&AnyObject>) {
            self.ivars().presentation.borrow_mut().toggle(self.now());
            self.tick();
        }
        #[unsafe(method(preview:))]
        fn preview(&self, _sender: Option<&AnyObject>) {
            self.ivars().presentation.borrow_mut().preview(self.now(), 10.0);
            self.tick();
        }
        #[unsafe(method(hide:))]
        fn hide(&self, _sender: Option<&AnyObject>) {
            self.ivars().presentation.borrow_mut().hide();
            self.tick();
            self.set_message("效果已隐藏。");
        }
        #[unsafe(method(selectEffect:))]
        fn select_effect(&self, sender: &NSMenuItem) {
            let effect = match sender.tag() { 1 => Effect::Laser, 2 => Effect::Magnify, 3 => Effect::Box, _ => Effect::Spotlight };
            if !self.allow_effect(effect) { return; }
            self.set_effect(effect);
            self.tick();
        }
        #[unsafe(method(showControls:))]
        fn controls(&self, _sender: Option<&AnyObject>) { self.show_controls(); }
        #[unsafe(method(previewSpotlight:))]
        fn preview_spotlight(&self, _sender: Option<&AnyObject>) { self.preview_effect(Effect::Spotlight); }
        #[unsafe(method(previewLaser:))]
        fn preview_laser(&self, _sender: Option<&AnyObject>) { self.preview_effect(Effect::Laser); }
        #[unsafe(method(previewMagnify:))]
        fn preview_magnify(&self, _sender: Option<&AnyObject>) { self.preview_effect(Effect::Magnify); }
        #[unsafe(method(smaller:))]
        fn smaller(&self, _sender: Option<&AnyObject>) { self.resize_effect(0.8); }
        #[unsafe(method(larger:))]
        fn larger(&self, _sender: Option<&AnyObject>) { self.resize_effect(1.25); }
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) { NSApplication::sharedApplication(self.mtm()).terminate(None); }
        #[unsafe(method(cycleChanged:))]
        fn cycle_changed(&self, sender: &NSButton) {
            let slot = sender.tag() as usize;
            let on = sender.state() == 1;
            if on && EFFECTS[slot] == Effect::Magnify {
                // Prompt now; the effect stays selected but is skipped until allowed.
                self.allow_effect(Effect::Magnify);
            }
            self.ivars().settings.borrow_mut().cycle[slot] = on;
            self.save_settings();
        }
        #[unsafe(method(holdChanged:))]
        fn hold_changed(&self, sender: &NSPopUpButton) {
            let action = HoldAction::ALL[sender.indexOfSelectedItem().clamp(0, 3) as usize];
            {
                let mut settings = self.ivars().settings.borrow_mut();
                if sender.tag() == 0 { settings.next_hold = action; } else { settings.back_hold = action; }
            }
            if action != HoldAction::BlackScreen && action != HoldAction::None { self.ensure_post_access(); }
            self.save_settings();
        }
        #[unsafe(method(reminderChanged:))]
        fn reminder_changed(&self, sender: &NSPopUpButton) {
            let mode = ScreenReminder::ALL[sender.indexOfSelectedItem().clamp(0, 2) as usize];
            self.ivars().settings.borrow_mut().screen_reminder = mode;
            self.save_settings();
            if mode != ScreenReminder::Off {
                self.show_toast("⏱ 屏幕提醒示例：显示 5 秒");
            }
        }
        #[unsafe(method(toggleTimer:))]
        fn toggle_timer(&self, _sender: Option<&AnyObject>) { self.toggle_talk_timer(); }
        #[unsafe(method(toggleBlackout:))]
        fn toggle_blackout(&self, _sender: Option<&AnyObject>) {
            let mut state = self.ivars().presentation.borrow_mut();
            state.blackout = !state.blackout;
            drop(state);
            self.tick();
        }
        #[unsafe(method(toggleRemote:))]
        fn toggle_remote(&self, _sender: Option<&AnyObject>) {
            if self.ivars().remote.borrow().is_some() {
                // A deliberate disconnect also stops automatic reconnection.
                self.ivars().auto_reconnect.set(false);
                let _ = self.stop_remote(); // errors are logged inside
                self.set_remote_message("遥控器已断开，不会自动重连。点击连接可重新使用。");
            } else {
                self.ivars().auto_reconnect.set(true);
                self.start_remote();
            }
            self.tick();
        }
    }
);

impl Delegate {
    fn set_remote_message(&self, text: &str) {
        self.ivars().remote_message.replace(text.to_string());
        if let Some(label) = self.ivars().remote_label.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(text));
        }
        let title = NSString::from_str(if self.ivars().remote.borrow().is_some() {
            "断开遥控器"
        } else {
            "连接遥控器"
        });
        if let Some(button) = self.ivars().remote_button.borrow().as_ref() {
            button.setTitle(&title);
        }
        if let Some(item) = self.ivars().remote_item.borrow().as_ref() {
            item.setTitle(&title);
        }
    }
    fn start_remote(&self) {
        if self.ivars().remote.borrow().is_some() || self.ivars().closing.get() {
            return;
        }
        let epoch = self.ivars().remote_epoch.get().wrapping_add(1);
        self.ivars().remote_epoch.set(epoch);
        // SAFETY: the delegate is live on main. The extra retain is kept on main
        // through worker shutdown and every queued delivery checks its epoch.
        let retained =
            unsafe { Retained::retain(self as *const Self as *mut Self) }.expect("live delegate");
        let target = Arc::new(MainThreadBound::new(retained, self.mtm()));
        self.ivars().remote_target.replace(Some(target.clone()));
        let unrestored = self.ivars().unrestored.borrow().clone();
        let started = remote::Remote::start(self.mtm(), unrestored, move |event| {
            let target = target.clone();
            DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new().expect("main queue");
                target.get(mtm).remote_event(epoch, event);
            });
        });
        match started {
            Ok(remote) => {
                self.ivars().remote.replace(Some(remote));
                self.set_remote_message("正在连接 Spotlight…");
            }
            Err(error) => {
                self.ivars().remote_target.replace(None);
                self.schedule_reconnect(3.0);
                self.set_remote_message(&if self.ivars().auto_reconnect.get() {
                    format!("等待遥控器：{error}（每 3 秒自动重试）")
                } else {
                    format!("无法连接遥控器：{error}")
                });
            }
        }
    }
    fn schedule_reconnect(&self, delay: f64) {
        self.ivars().retry_at.set(self.now() + delay);
    }
    /// Called from tick: reconnect after a disconnect, sleep or failed start.
    fn maybe_reconnect(&self) {
        if self.ivars().auto_reconnect.get()
            && !self.ivars().closing.get()
            && self.ivars().initial_demo.is_none()
            && self.ivars().remote.borrow().is_none()
            && self.now() >= self.ivars().retry_at.get()
        {
            self.start_remote();
        }
    }
    /// Joins the worker and keeps any controls it could not restore, so the
    /// next session can recognise and adopt its own leftover diversion.
    fn stop_remote(&self) -> std::result::Result<(), String> {
        self.ivars()
            .remote_epoch
            .set(self.ivars().remote_epoch.get().wrapping_add(1));
        self.ivars()
            .presentation
            .borrow_mut()
            .set_remote_held(false);
        let remote = self.ivars().remote.borrow_mut().take();
        let Some(mut worker) = remote else {
            return Ok(());
        };
        let (result, unrestored) = worker.stop();
        self.ivars().remote_target.replace(None);
        if !unrestored.is_empty() {
            eprintln!("REMOTE UNRESTORED {unrestored:?}");
        }
        Self::save_unrestored(&unrestored);
        *self.ivars().unrestored.borrow_mut() = unrestored;
        if let Err(error) = &result {
            eprintln!("REMOTE STOP ERROR: {error}");
        }
        result
    }
    fn remote_event(&self, epoch: u64, event: remote::Event) {
        if self.ivars().closing.get() || epoch != self.ivars().remote_epoch.get() {
            return;
        }
        match event {
            remote::Event::Connected(transport, warning, leased) => {
                // Record what a crash would leave behind; normal stop rewrites it.
                let mut pending = self.ivars().unrestored.borrow().clone();
                pending.retain(|(cid, _)| !leased.iter().any(|(c, _)| c == cid));
                pending.extend(leased);
                Self::save_unrestored(&pending);
                self.set_remote_message(&format!(
                    "Spotlight 已通过{transport}连接。按住顶键显示，双击顶键切换特效。{}",
                    warning.unwrap_or_default()
                ));
            }
            remote::Event::Gesture(gesture) => {
                eprintln!("REMOTE GESTURE {gesture:?}");
                match gesture {
                    remote::Gesture::DoubleClick => self.cycle_effect(),
                    remote::Gesture::NextHold => {
                        let action = self.ivars().settings.borrow().next_hold;
                        self.perform_hold(action);
                    }
                    remote::Gesture::BackHold => {
                        let action = self.ivars().settings.borrow().back_hold;
                        self.perform_hold(action);
                    }
                }
            }
            remote::Event::Message(text) => self.set_message(&text),
            remote::Event::PageTurn => {
                if self.ivars().presentation.borrow().has_box() {
                    self.ivars().presentation.borrow_mut().clear_box();
                    self.tick();
                }
            }
            remote::Event::VibrationFailed(reminder) => {
                eprintln!("TIMER vibration failed for {reminder}");
                // "Always" already showed it when the timer fired.
                if self.ivars().settings.borrow().screen_reminder
                    == ScreenReminder::OnVibrationFailure
                {
                    self.show_toast(&reminder);
                }
                self.set_message(&format!("{reminder}（振动失败）"));
            }
            remote::Event::Hold(down) => {
                let mouse = NSEvent::mouseLocation();
                let pointer = Point {
                    x: mouse.x,
                    y: mouse.y,
                };
                {
                    let mut state = self.ivars().presentation.borrow_mut();
                    if down {
                        state.set_remote_held(true);
                        state.begin_box(pointer);
                    } else {
                        state.finish_box(pointer);
                        state.set_remote_held(false);
                    }
                }
                self.tick();
                let visible = self
                    .ivars()
                    .overlays
                    .borrow()
                    .iter()
                    .filter(|o| o.panel.isVisible())
                    .count();
                eprintln!(
                    "REMOTE TOP {} overlay_windows_visible={visible}",
                    if down { "pressed" } else { "released" }
                );
            }
            remote::Event::Stopped => {
                // Final notification is queued after restoration. Joining also
                // completes destruction of the worker's main-bound callback.
                let result = self.stop_remote();
                if self.ivars().auto_reconnect.get() {
                    self.schedule_reconnect(2.0);
                    self.set_remote_message(match result {
                        Ok(()) => "遥控器连接已结束，正在自动重连…",
                        Err(_) => "遥控器连接中断（可能已休眠或超出范围），正在自动重连…",
                    });
                } else if let Err(error) = result {
                    self.set_remote_message(&format!("遥控器已停止：{error}"));
                }
                self.tick();
            }
        }
    }
    fn now(&self) -> f64 {
        self.ivars().start.elapsed().as_secs_f64()
    }
    fn settings_path() -> Option<std::path::PathBuf> {
        std::env::var_os("HOME").map(|home| {
            std::path::PathBuf::from(home)
                .join("Library/Application Support/Spotlight RS/settings.conf")
        })
    }
    fn unrestored_path() -> Option<std::path::PathBuf> {
        Self::settings_path().map(|path| path.with_file_name("diverted-controls.txt"))
    }
    fn load_unrestored() -> remote::Unrestored {
        Self::unrestored_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse_unrestored(&text))
            .unwrap_or_default()
    }
    fn save_unrestored(items: &[(u16, Reporting)]) {
        let Some(path) = Self::unrestored_path() else {
            return;
        };
        let result = if items.is_empty() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        } else {
            path.parent()
                .map(std::fs::create_dir_all)
                .unwrap_or(Ok(()))
                .and_then(|()| std::fs::write(&path, format_unrestored(items)))
        };
        if let Err(error) = result {
            eprintln!("REMOTE could not record diverted controls: {error}");
        }
    }
    fn load_settings() -> Settings {
        Self::settings_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Settings::parse(&text))
            .unwrap_or_default()
    }
    fn save_settings(&self) {
        let Some(path) = Self::settings_path() else {
            return;
        };
        let text = self.ivars().settings.borrow().serialize();
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(&path, text));
        if let Err(error) = result {
            self.set_message(&format!("设置未能保存：{error}"));
        }
    }
    fn resize_effect(&self, factor: f64) {
        let radius = {
            let mut state = self.ivars().presentation.borrow_mut();
            state.resize(factor);
            state.radius
        };
        self.ivars().settings.borrow_mut().radius = radius;
        self.save_settings();
        self.set_message(&format!("聚光直径：{} 点", (radius * 2.0).round()));
        self.tick();
    }
    fn effect_name(effect: Effect) -> &'static str {
        match effect {
            Effect::Spotlight => "聚光",
            Effect::Laser => "数字激光",
            Effect::Magnify => "实时放大",
            Effect::Box => "方框高亮",
        }
    }
    /// Every effect change drops a kept rectangle; it belonged to Box mode.
    fn set_effect(&self, effect: Effect) {
        let mut state = self.ivars().presentation.borrow_mut();
        if state.effect != effect {
            state.clear_box();
        }
        state.effect = effect;
    }
    fn show_toast(&self, text: &str) {
        let mtm = self.mtm();
        // The primary display (with the menu bar) is usually the presenter's
        // laptop in extended mode; in mirrored mode the audience sees it too.
        let Some(screen) = NSScreen::screens(mtm).firstObject() else {
            return;
        };
        let size = NSSize::new(360.0, 64.0);
        let frame = screen.frame();
        let origin = NSPoint::new(
            frame.origin.x + frame.size.width - size.width - 24.0,
            frame.origin.y + frame.size.height - size.height - 24.0,
        );
        if self.ivars().toast.borrow().is_none() {
            // SAFETY: AppKit calls stay on main; the panel is retained by AppData.
            let (panel, label) = unsafe {
                let panel: Retained<OverlayPanel> = msg_send![OverlayPanel::alloc(mtm), initWithContentRect: NSRect::new(origin, size), styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel, backing: NSBackingStoreType::Buffered, defer: false];
                panel.setReleasedWhenClosed(false);
                panel.setOpaque(false);
                panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                    0.1, 0.1, 0.1, 0.85,
                )));
                panel.setHasShadow(true);
                panel.setIgnoresMouseEvents(true);
                panel.setHidesOnDeactivate(false);
                // Above the black screen too, so a reminder is never hidden.
                panel.setLevel(NSScreenSaverWindowLevel + 1);
                panel.setCollectionBehavior(
                    NSWindowCollectionBehavior::CanJoinAllSpaces
                        | NSWindowCollectionBehavior::FullScreenAuxiliary
                        | NSWindowCollectionBehavior::Stationary
                        | NSWindowCollectionBehavior::IgnoresCycle,
                );
                let label = NSTextField::labelWithString(ns_string!(""), mtm);
                label.setFont(Some(&NSFont::boldSystemFontOfSize(22.0)));
                label.setTextColor(Some(&NSColor::whiteColor()));
                label.setAlignment(NSTextAlignment::Center);
                label.setFrame(NSRect::new(
                    NSPoint::new(12.0, 16.0),
                    NSSize::new(size.width - 24.0, 32.0),
                ));
                panel
                    .contentView()
                    .expect("panel content view")
                    .addSubview(&label);
                (panel, label)
            };
            self.ivars().toast.replace(Some((panel, label)));
        }
        if let Some((panel, label)) = self.ivars().toast.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(text));
            panel.setFrameOrigin(origin);
            panel.orderFrontRegardless();
        }
        self.ivars().toast_until.set(self.now() + 5.0);
    }
    fn cycle_effect(&self) {
        let current = self.ivars().presentation.borrow().effect;
        let next = self
            .ivars()
            .settings
            .borrow()
            .next_effect(current, self.ivars().capture_allowed.get());
        match next {
            Some(effect) => {
                self.set_effect(effect);
                self.ivars()
                    .presentation
                    .borrow_mut()
                    .flash(self.now(), SWITCH_FLASH_SECONDS);
                self.set_message(&format!("双击切换：{}", Self::effect_name(effect)));
                self.tick();
            }
            None => self.set_message(
                "双击切换没有可用特效：请在控制面板勾选；实时放大还需要屏幕录制权限。",
            ),
        }
    }
    /// Posting keys to another app needs Accessibility permission.
    fn ensure_post_access(&self) -> bool {
        if CGPreflightPostEventAccess() || CGRequestPostEventAccess() {
            return true;
        }
        self.set_message("长按翻页键发送快捷键需要“辅助功能”权限：系统设置 → 隐私与安全性 → 辅助功能，允许 Spotlight RS 后重新打开程序。");
        false
    }
    fn perform_hold(&self, action: HoldAction) {
        match action {
            HoldAction::None => {}
            HoldAction::BlackScreen => {
                let black = {
                    let mut state = self.ivars().presentation.borrow_mut();
                    state.blackout = !state.blackout;
                    state.blackout
                };
                self.set_message(if black {
                    "屏幕已变黑；再次长按或 ⌃⌥⌘H 恢复。"
                } else {
                    "屏幕已恢复。"
                });
                self.tick();
            }
            HoldAction::PlayFromCurrent | HoldAction::EndShow => {
                let Some(app) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
                    return;
                };
                let bundle = app
                    .bundleIdentifier()
                    .map(|b| b.to_string())
                    .unwrap_or_default();
                // kVK codes: P=0x23, Return=0x24, Escape=0x35.
                let shortcut = match (action, bundle.as_str()) {
                    (HoldAction::PlayFromCurrent, "com.apple.iWork.Keynote") => Some((
                        0x23,
                        CGEventFlags::MaskAlternate | CGEventFlags::MaskCommand,
                    )),
                    (HoldAction::PlayFromCurrent, "com.microsoft.Powerpoint") => {
                        Some((0x24, CGEventFlags::MaskShift | CGEventFlags::MaskCommand))
                    }
                    (HoldAction::EndShow, _) => Some((0x35, CGEventFlags::empty())),
                    _ => None,
                };
                let Some((key, flags)) = shortcut else {
                    self.set_message(
                        "“从当前页播放”目前支持 Keynote 和 PowerPoint；请先切换到演示文稿窗口。",
                    );
                    return;
                };
                if !self.ensure_post_access() {
                    return;
                }
                // Delivered directly to the frontmost app's event queue.
                for down in [true, false] {
                    if let Some(event) = CGEvent::new_keyboard_event(None, key, down) {
                        CGEvent::set_flags(Some(&event), flags);
                        CGEvent::post_to_pid(app.processIdentifier(), Some(&event));
                    }
                }
                eprintln!("REMOTE ACTION {action:?} sent to {bundle}");
                self.set_message(&format!("{}：已发送到 {bundle}", action.label()));
            }
        }
    }
    fn toggle_talk_timer(&self) {
        if self
            .ivars()
            .talk_timer
            .borrow()
            .elapsed(self.now())
            .is_some()
        {
            self.ivars().talk_timer.borrow_mut().stop();
            self.set_message("计时已停止。");
        } else {
            // The panel fields are authoritative when the panel has been opened.
            let fields = self.ivars().timer_fields.borrow();
            if !fields.is_empty() {
                let mut timers = [None; 3];
                for (slot, field) in timers.iter_mut().zip(fields.iter()) {
                    match parse_minutes(&field.stringValue().to_string()) {
                        Ok(value) => *slot = value,
                        Err(error) => {
                            self.set_message(&error);
                            return;
                        }
                    }
                }
                self.ivars().settings.borrow_mut().timers = timers;
            }
            drop(fields);
            self.save_settings();
            let timers = self.ivars().settings.borrow().timers;
            if timers.iter().all(Option::is_none) {
                self.set_message("请至少设定一个定时（分钟）。");
                self.show_controls();
                return;
            }
            self.ivars().talk_timer.borrow_mut().start(self.now());
            let list: Vec<_> = timers
                .iter()
                .flatten()
                .map(|m| format!("{m} 分钟"))
                .collect();
            self.set_message(&format!("计时开始：{} 时振动提醒。", list.join("、")));
            if self.ivars().remote.borrow().is_none() {
                self.set_message("计时开始，但遥控器未连接：到点只能在屏幕上提示。");
            }
        }
        self.update_timer_ui();
    }
    fn update_timer_ui(&self) {
        let elapsed = self.ivars().talk_timer.borrow().elapsed(self.now());
        let title = NSString::from_str(if elapsed.is_some() {
            "停止计时"
        } else {
            "开始计时"
        });
        if let Some(button) = self.ivars().timer_button.borrow().as_ref() {
            button.setTitle(&title);
        }
        if let Some(item) = self.ivars().timer_item.borrow().as_ref() {
            item.setTitle(&title);
        }
        let status = match elapsed {
            Some(seconds) => {
                let seconds = seconds as u64;
                format!("◎ {:02}:{:02}", seconds / 60, seconds % 60)
            }
            None => "◎".into(),
        };
        if let Some(item) = self.ivars().status.borrow().as_ref() {
            if let Some(button) = item.button(self.mtm()) {
                if button.title().to_string() != status {
                    button.setTitle(&NSString::from_str(&status));
                }
            }
        }
    }
    fn check_timers(&self) {
        let timers = self.ivars().settings.borrow().timers;
        let due = self
            .ivars()
            .talk_timer
            .borrow_mut()
            .due(self.now(), &timers);
        for slot in due {
            let minutes = timers[slot].unwrap_or_default();
            let reminder = format!("⏱ 第 {} 个定时：{minutes} 分钟", slot + 1);
            let vibrated = self
                .ivars()
                .remote
                .borrow()
                .as_ref()
                .is_some_and(|remote| remote.vibrate(reminder.clone()));
            eprintln!("TIMER {} ({minutes} min) due vibrated={vibrated}", slot + 1);
            if self
                .ivars()
                .settings
                .borrow()
                .screen_reminder
                .shows(vibrated)
            {
                self.show_toast(&reminder);
            }
            self.set_message(&format!(
                "第 {} 个定时到：{minutes} 分钟{}",
                slot + 1,
                if vibrated {
                    "，已振动。"
                } else {
                    "（遥控器未连接，未振动）。"
                }
            ));
        }
    }
    fn set_message(&self, text: &str) {
        if *self.ivars().message.borrow() == text {
            return;
        }
        self.ivars().message.replace(text.to_string());
        if let Some(label) = self.ivars().status_label.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(text));
        }
    }
    fn allow_effect(&self, effect: Effect) -> bool {
        if effect != Effect::Magnify {
            return true;
        }
        let allowed = CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess();
        self.ivars().capture_allowed.set(allowed);
        if allowed {
            return true;
        }
        self.set_message("请在系统设置 → 隐私与安全性 → 屏幕录制中允许 Spotlight RS，然后重新打开程序。聚光与激光无需此权限。");
        self.show_controls();
        false
    }
    fn preview_effect(&self, effect: Effect) {
        if !self.allow_effect(effect) {
            return;
        }
        self.set_effect(effect);
        self.ivars()
            .presentation
            .borrow_mut()
            .preview(self.now(), 10.0);
        self.set_message("移动鼠标查看效果；预览会在 10 秒后自动结束。");
        self.tick();
    }
    fn show_controls(&self) {
        if self.ivars().controls.borrow().is_none() {
            let mtm = self.mtm();
            // SAFETY: Native controls and target/action references live on main.
            unsafe {
                let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::ZERO, NSSize::new(480.0, 620.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable,
                    NSBackingStoreType::Buffered,
                    false,
                );
                window.setReleasedWhenClosed(false);
                window.setTitle(ns_string!("Spotlight RS"));
                window.center();
                let view = window.contentView().expect("window content view");
                let title = NSTextField::labelWithString(ns_string!("Spotlight RS"), mtm);
                title.setFont(Some(&NSFont::boldSystemFontOfSize(26.0)));
                title.setFrame(NSRect::new(
                    NSPoint::new(28.0, 555.0),
                    NSSize::new(424.0, 36.0),
                ));
                view.addSubview(&title);
                let subtitle =
                    NSTextField::labelWithString(ns_string!("用光线，把注意力留在重点。"), mtm);
                subtitle.setTextColor(Some(&NSColor::secondaryLabelColor()));
                subtitle.setFrame(NSRect::new(
                    NSPoint::new(28.0, 524.0),
                    NSSize::new(424.0, 24.0),
                ));
                view.addSubview(&subtitle);
                let remote_label = NSTextField::wrappingLabelWithString(
                    &NSString::from_str(&self.ivars().remote_message.borrow()),
                    mtm,
                );
                remote_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
                remote_label.setFrame(NSRect::new(
                    NSPoint::new(28.0, 444.0),
                    NSSize::new(424.0, 70.0),
                ));
                view.addSubview(&remote_label);
                self.ivars().remote_label.replace(Some(remote_label));
                for (index, (text, action)) in [
                    ("预览聚光", sel!(previewSpotlight:)),
                    ("预览激光", sel!(previewLaser:)),
                    ("预览放大", sel!(previewMagnify:)),
                ]
                .iter()
                .enumerate()
                {
                    let button = NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(text),
                        Some(self),
                        Some(*action),
                        mtm,
                    );
                    button.setFrame(NSRect::new(
                        NSPoint::new(24.0 + index as f64 * 145.0, 400.0),
                        NSSize::new(138.0, 36.0),
                    ));
                    view.addSubview(&button);
                }
                let hide = NSButton::buttonWithTitle_target_action(
                    ns_string!("立即隐藏"),
                    Some(self),
                    Some(sel!(hide:)),
                    mtm,
                );
                hide.setFrame(NSRect::new(
                    NSPoint::new(24.0, 352.0),
                    NSSize::new(138.0, 32.0),
                ));
                view.addSubview(&hide);
                let connect = NSButton::buttonWithTitle_target_action(
                    ns_string!("连接遥控器"),
                    Some(self),
                    Some(sel!(toggleRemote:)),
                    mtm,
                );
                connect.setFrame(NSRect::new(
                    NSPoint::new(169.0, 352.0),
                    NSSize::new(138.0, 32.0),
                ));
                view.addSubview(&connect);
                self.ivars().remote_button.replace(Some(connect));
                let quit = NSButton::buttonWithTitle_target_action(
                    ns_string!("退出"),
                    Some(self),
                    Some(sel!(quit:)),
                    mtm,
                );
                quit.setFrame(NSRect::new(
                    NSPoint::new(314.0, 352.0),
                    NSSize::new(138.0, 32.0),
                ));
                view.addSubview(&quit);
                self.add_settings_controls(&view);
                let label = NSTextField::wrappingLabelWithString(ns_string!("预览持续 10 秒。⌃⌥⌘H 立即隐藏（含黑屏）；关闭窗口后，从菜单栏 ◎ 继续控制。"), mtm);
                label.setTextColor(Some(&NSColor::secondaryLabelColor()));
                label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
                label.setFrame(NSRect::new(
                    NSPoint::new(28.0, 12.0),
                    NSSize::new(424.0, 76.0),
                ));
                if !self.ivars().message.borrow().is_empty() {
                    label.setStringValue(&NSString::from_str(&self.ivars().message.borrow()));
                }
                view.addSubview(&label);
                self.ivars().status_label.replace(Some(label));
                self.ivars().controls.replace(Some(window));
            }
        }
        if let Some(window) = self.ivars().controls.borrow().as_ref() {
            window.makeKeyAndOrderFront(None);
        }
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
    }
    fn add_settings_controls(&self, view: &NSView) {
        let mtm = self.mtm();
        let frame =
            |x: f64, y: f64, w: f64, h: f64| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        let heading = |text: &str, y: f64| {
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            label.setFrame(frame(28.0, y, 424.0, 20.0));
            view.addSubview(&label);
        };
        let settings = self.ivars().settings.borrow().clone();
        heading("双击顶键在以下特效间切换", 316.0);
        let mut boxes = Vec::new();
        for (slot, effect) in EFFECTS.iter().enumerate() {
            // SAFETY: target/action refer to this main-thread delegate's selector.
            let button = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &NSString::from_str(Self::effect_name(*effect)),
                    Some(self),
                    Some(sel!(cycleChanged:)),
                    mtm,
                )
            };
            button.setTag(slot as isize);
            button.setState(if settings.cycle[slot] { 1 } else { 0 });
            button.setFrame(frame(28.0 + slot as f64 * 106.0, 288.0, 104.0, 24.0));
            view.addSubview(&button);
            boxes.push(button);
        }
        self.ivars().cycle_boxes.replace(boxes);
        let mut popups = Vec::new();
        for (tag, (text, action, y)) in [
            ("长按下一页", settings.next_hold, 244.0),
            ("长按上一页", settings.back_hold, 208.0),
        ]
        .into_iter()
        .enumerate()
        {
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFrame(frame(28.0, y + 4.0, 100.0, 20.0));
            view.addSubview(&label);
            let popup = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                frame(130.0, y, 220.0, 28.0),
                false,
            );
            for choice in HoldAction::ALL {
                popup.addItemWithTitle(&NSString::from_str(choice.label()));
            }
            let selected = HoldAction::ALL
                .iter()
                .position(|a| *a == action)
                .unwrap_or(0);
            popup.selectItemAtIndex(selected as isize);
            popup.setTag(tag as isize);
            // SAFETY: target/action refer to this main-thread delegate's selector.
            unsafe {
                popup.setTarget(Some(self));
                popup.setAction(Some(sel!(holdChanged:)));
            }
            view.addSubview(&popup);
            popups.push(popup);
        }
        self.ivars().hold_popups.replace(popups);
        heading("计时器（分钟，留空不用；到点遥控器振动）", 168.0);
        let mut fields = Vec::new();
        for (slot, minutes) in settings.timers.iter().enumerate() {
            let field = NSTextField::textFieldWithString(
                &NSString::from_str(&minutes.map(|m| m.to_string()).unwrap_or_default()),
                mtm,
            );
            field.setPlaceholderString(Some(&NSString::from_str(&format!("定时 {}", slot + 1))));
            field.setFrame(frame(28.0 + slot as f64 * 80.0, 134.0, 70.0, 24.0));
            view.addSubview(&field);
            fields.push(field);
        }
        self.ivars().timer_fields.replace(fields);
        // SAFETY: target/action refer to this main-thread delegate's selector.
        let start = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("开始计时"),
                Some(self),
                Some(sel!(toggleTimer:)),
                mtm,
            )
        };
        start.setFrame(frame(280.0, 128.0, 172.0, 32.0));
        view.addSubview(&start);
        self.ivars().timer_button.replace(Some(start));
        let label = NSTextField::labelWithString(ns_string!("屏幕提醒"), mtm);
        label.setFrame(frame(28.0, 100.0, 100.0, 20.0));
        view.addSubview(&label);
        let popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            frame(130.0, 96.0, 220.0, 28.0),
            false,
        );
        for mode in ScreenReminder::ALL {
            popup.addItemWithTitle(&NSString::from_str(mode.label()));
        }
        let selected = ScreenReminder::ALL
            .iter()
            .position(|m| *m == settings.screen_reminder)
            .unwrap_or(1);
        popup.selectItemAtIndex(selected as isize);
        // SAFETY: target/action refer to this main-thread delegate's selector.
        unsafe {
            popup.setTarget(Some(self));
            popup.setAction(Some(sel!(reminderChanged:)));
        }
        view.addSubview(&popup);
        self.ivars().reminder_popup.replace(Some(popup));
        self.update_timer_ui();
    }
    fn item(
        &self,
        menu: &NSMenu,
        title: &str,
        action: objc2::runtime::Sel,
        key: &str,
    ) -> Retained<NSMenuItem> {
        // SAFETY: All menu selectors are defined above with AppKit action signatures.
        unsafe {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            );
            item.setTarget(Some(self));
            menu.addItem(&item);
            item
        }
    }
    fn setup_menu(&self) {
        let menu = NSMenu::new(self.mtm());
        menu.setAutoenablesItems(false);
        for (index, title) in ["聚光", "数字激光", "实时放大", "方框高亮"]
            .iter()
            .enumerate()
        {
            let item = self.item(&menu, title, sel!(selectEffect:), "");
            item.setTag(index as isize);
            self.ivars().effect_items.borrow_mut().push(item);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        self.item(&menu, "显示 / 隐藏", sel!(toggle:), "");
        self.item(&menu, "预览 10 秒", sel!(preview:), "");
        self.item(&menu, "立即隐藏    ⌃⌥⌘H", sel!(hide:), "");
        self.item(&menu, "屏幕变黑 / 恢复", sel!(toggleBlackout:), "");
        let timer = self.item(&menu, "开始计时", sel!(toggleTimer:), "");
        self.ivars().timer_item.replace(Some(timer));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        self.item(&menu, "缩小范围", sel!(smaller:), "-");
        self.item(&menu, "放大范围", sel!(larger:), "+");
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        let remote = self.item(&menu, "连接遥控器", sel!(toggleRemote:), "");
        self.ivars().remote_item.replace(Some(remote));
        self.item(&menu, "打开控制面板…", sel!(showControls:), "");
        self.item(&menu, "退出 Spotlight RS", sel!(quit:), "q");
        let status =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status.button(self.mtm()) {
            button.setTitle(ns_string!("◎"));
            button.setToolTip(Some(ns_string!("Spotlight RS")));
        }
        status.setMenu(Some(&menu));
        self.ivars().status.replace(Some(status));
    }
    fn install_timer(&self, fast: bool) {
        if let Some(timer) = self.ivars().timer.borrow_mut().take() {
            timer.invalidate();
        }
        // SAFETY: self is kept alive for the app lifetime; the timer is invalidated on quit.
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                if fast { 1.0 / 60.0 } else { 0.25 },
                self,
                sel!(tick:),
                None,
                true,
            )
        };
        unsafe {
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        self.ivars().timer.replace(Some(timer));
        self.ivars().fast_timer.set(fast);
    }
    fn refresh_screens(&self) {
        let screens = NSScreen::screens(self.mtm());
        let frames: Vec<_> = screens
            .iter()
            .map(|screen| {
                (
                    screen.frame(),
                    display_id(&screen),
                    screen.backingScaleFactor(),
                )
            })
            .collect();
        if frames
            == self
                .ivars()
                .overlays
                .borrow()
                .iter()
                .map(|o| (o.frame, o.display_id, o.scale))
                .collect::<Vec<_>>()
        {
            return;
        }
        let mut overlays = self.ivars().overlays.borrow_mut();
        for overlay in overlays.iter() {
            overlay.hide();
        }
        self.ivars().capture.stop();
        *overlays = screens
            .iter()
            .map(|screen| Overlay::new(self.mtm(), &screen))
            .collect();
    }
    fn tick(&self) {
        let now = self.now();
        if self.ivars().signals.received() != 0
            || self
                .ivars()
                .exit_after
                .is_some_and(|seconds| now >= seconds)
        {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
            return;
        }
        self.maybe_reconnect();
        if now - self.ivars().screen_check.get() >= 1.0 {
            self.refresh_screens();
            self.ivars().screen_check.set(now);
        }
        if let Some(error) = self.ivars().capture.error() {
            self.ivars().presentation.borrow_mut().hide();
            self.ivars().capture.stop();
            self.set_message(&format!("放大镜已停止：{error}"));
        }
        self.check_timers();
        self.update_timer_ui();
        if let Some((panel, _)) = self.ivars().toast.borrow().as_ref() {
            if panel.isVisible() && now >= self.ivars().toast_until.get() {
                panel.orderOut(None);
            }
        }
        let state = self.ivars().presentation.borrow();
        let active = state.active(now);
        if active != self.ivars().fast_timer.get() {
            self.install_timer(active);
        }
        let pointer = NSEvent::mouseLocation();
        if active && state.effect == Effect::Magnify && self.ivars().capture_allowed.get() {
            for overlay in self.ivars().overlays.borrow().iter() {
                let r = overlay.frame;
                if pointer.x >= r.origin.x
                    && pointer.y >= r.origin.y
                    && pointer.x < r.origin.x + r.size.width
                    && pointer.y < r.origin.y + r.size.height
                {
                    self.ivars().capture.start(
                        overlay.display_id,
                        (r.size.width * overlay.scale).round() as usize,
                        (r.size.height * overlay.scale).round() as usize,
                    );
                }
            }
        } else if self.ivars().capture.frame().is_some()
            || state.effect != Effect::Magnify
            || !active
        {
            self.ivars().capture.stop();
        }
        let pointer_point = Point {
            x: pointer.x,
            y: pointer.y,
        };
        for overlay in self.ivars().overlays.borrow().iter() {
            if state.blackout {
                overlay.render_blackout();
            } else if active && state.effect == Effect::Box {
                match state.box_view(pointer_point, now) {
                    Some(view) => overlay.render_box(view, &state),
                    None => overlay.hide(),
                }
            } else if active {
                overlay.render(pointer, &state, &self.ivars().capture);
            } else if overlay.panel.isVisible() {
                overlay.hide();
            }
        }
        if let Some(error) = self.ivars().capture.error() {
            self.set_message(&format!("放大镜已停止：{error}"));
        } else if active
            && state.effect == Effect::Magnify
            && self.ivars().capture.frame().is_none()
        {
            self.set_message(if self.ivars().capture_allowed.get() {
                "正在启动屏幕采集…"
            } else {
                "实时放大需要屏幕录制权限；聚光和激光仍可使用。"
            });
        }
        let selected = match state.effect {
            Effect::Spotlight => 0,
            Effect::Laser => 1,
            Effect::Magnify => 2,
            Effect::Box => 3,
        };
        for item in self.ivars().effect_items.borrow().iter() {
            item.setState(if item.tag() == selected { 1 } else { 0 });
        }
    }
}

unsafe fn hotkey_action(target: *mut std::ffi::c_void, action: u32) {
    // SAFETY: Carbon callback is on main and the delegate outlives registration.
    let delegate = unsafe { &*(target as *const Delegate) };
    // Only the emergency hide is registered; no other global shortcut is taken.
    if action != 2 {
        return;
    }
    delegate.ivars().presentation.borrow_mut().hide();
    delegate.tick();
    delegate.set_message("效果已隐藏。");
}

pub fn run(demo: Option<(Effect, f64)>) -> Result<(), Box<dyn std::error::Error>> {
    let mtm = MainThreadMarker::new().ok_or("AppKit must start on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let settings = Delegate::load_settings();
    let delegate = Delegate::alloc(mtm).set_ivars(AppData {
        start: Instant::now(),
        presentation: RefCell::new({
            let mut state = Presentation::default();
            state.radius = settings.radius;
            state
        }),
        overlays: RefCell::new(Vec::new()),
        status: RefCell::new(None),
        timer: RefCell::new(None),
        fast_timer: Cell::new(false),
        screen_check: Cell::new(0.0),
        exit_after: demo.map(|(_, seconds)| seconds),
        initial_demo: demo,
        effect_items: RefCell::new(Vec::new()),
        capture: Capture::new(mtm),
        controls: RefCell::new(None),
        status_label: RefCell::new(None),
        message: RefCell::new(String::new()),
        hotkeys: RefCell::new(None),
        capture_allowed: Cell::new(CGPreflightScreenCaptureAccess()),
        remote: RefCell::new(None),
        remote_epoch: Cell::new(0),
        remote_target: RefCell::new(None),
        remote_button: RefCell::new(None),
        remote_item: RefCell::new(None),
        remote_label: RefCell::new(None),
        remote_message: RefCell::new("遥控器未连接。".into()),
        signals: crate::stop_signals::StopSignals::install()?,
        closing: Cell::new(false),
        settings: RefCell::new(settings),
        talk_timer: RefCell::new(PresentationTimer::default()),
        cycle_boxes: RefCell::new(Vec::new()),
        hold_popups: RefCell::new(Vec::new()),
        timer_fields: RefCell::new(Vec::new()),
        timer_button: RefCell::new(None),
        timer_item: RefCell::new(None),
        auto_reconnect: Cell::new(true),
        retry_at: Cell::new(0.0),
        toast: RefCell::new(None),
        toast_until: Cell::new(0.0),
        reminder_popup: RefCell::new(None),
        unrestored: RefCell::new(Delegate::load_unrestored()),
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
    Ok(())
}
