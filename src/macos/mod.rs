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
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
use objc2_foundation::*;
use spotlight_rs::presentation::{Effect, Point, Presentation, Rect};
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
        let center = NSPoint::new(local.x, local.y);
        let changed = fresh_image
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
            self.stop_remote();
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
            self.set_message("效果已隐藏。⌥⌘P 可再次显示。");
        }
        #[unsafe(method(selectEffect:))]
        fn select_effect(&self, sender: &NSMenuItem) {
            let effect = match sender.tag() { 1 => Effect::Laser, 2 => Effect::Magnify, _ => Effect::Spotlight };
            if !self.allow_effect(effect) { return; }
            self.ivars().presentation.borrow_mut().effect = effect;
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
        fn smaller(&self, _sender: Option<&AnyObject>) { self.ivars().presentation.borrow_mut().resize(0.8); self.tick(); }
        #[unsafe(method(larger:))]
        fn larger(&self, _sender: Option<&AnyObject>) { self.ivars().presentation.borrow_mut().resize(1.25); self.tick(); }
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) { NSApplication::sharedApplication(self.mtm()).terminate(None); }
        #[unsafe(method(toggleRemote:))]
        fn toggle_remote(&self, _sender: Option<&AnyObject>) {
            if self.ivars().remote.borrow().is_some() { self.stop_remote(); }
            else { self.start_remote(); }
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
        let started = remote::Remote::start(self.mtm(), move |event| {
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
                self.set_remote_message(&format!("无法连接遥控器：{error}"));
            }
        }
    }
    fn stop_remote(&self) {
        self.ivars()
            .remote_epoch
            .set(self.ivars().remote_epoch.get().wrapping_add(1));
        self.ivars()
            .presentation
            .borrow_mut()
            .set_remote_held(false);
        let remote = self.ivars().remote.borrow_mut().take();
        let result = remote.map(|mut worker| worker.stop()).unwrap_or(Ok(()));
        self.ivars().remote_target.replace(None);
        match result {
            Ok(()) => self.set_remote_message("遥控器已断开。点击连接可重新使用。"),
            Err(error) => {
                eprintln!("REMOTE STOP ERROR: {error}");
                self.set_remote_message(&format!("遥控器已停止：{error}"));
            }
        }
    }
    fn remote_event(&self, epoch: u64, event: remote::Event) {
        if self.ivars().closing.get() || epoch != self.ivars().remote_epoch.get() {
            return;
        }
        match event {
            remote::Event::Connected(transport) => self.set_remote_message(&format!(
                "Spotlight 已通过{transport}连接。按住顶键显示，松开隐藏。"
            )),
            remote::Event::Hold(down) => {
                self.ivars().presentation.borrow_mut().set_remote_held(down);
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
            remote::Event::Stopped(result) => {
                // Final notification is queued after restoration. Joining also
                // completes destruction of the worker's main-bound callback.
                self.stop_remote();
                if let Err(error) = result {
                    self.set_remote_message(&format!("遥控器已停止：{error}"));
                }
                self.tick();
            }
        }
    }
    fn now(&self) -> f64 {
        self.ivars().start.elapsed().as_secs_f64()
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
        {
            let mut state = self.ivars().presentation.borrow_mut();
            state.effect = effect;
            state.preview(self.now(), 10.0);
        }
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
                    NSRect::new(NSPoint::ZERO, NSSize::new(480.0, 366.0)),
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
                    NSPoint::new(28.0, 301.0),
                    NSSize::new(424.0, 36.0),
                ));
                view.addSubview(&title);
                let subtitle =
                    NSTextField::labelWithString(ns_string!("用光线，把注意力留在重点。"), mtm);
                subtitle.setTextColor(Some(&NSColor::secondaryLabelColor()));
                subtitle.setFrame(NSRect::new(
                    NSPoint::new(28.0, 270.0),
                    NSSize::new(424.0, 24.0),
                ));
                view.addSubview(&subtitle);
                let remote_label = NSTextField::wrappingLabelWithString(
                    &NSString::from_str(&self.ivars().remote_message.borrow()),
                    mtm,
                );
                remote_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
                remote_label.setFrame(NSRect::new(
                    NSPoint::new(28.0, 190.0),
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
                        NSPoint::new(24.0 + index as f64 * 145.0, 146.0),
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
                    NSPoint::new(24.0, 98.0),
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
                    NSPoint::new(169.0, 98.0),
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
                    NSPoint::new(314.0, 98.0),
                    NSSize::new(138.0, 32.0),
                ));
                view.addSubview(&quit);
                let label = NSTextField::wrappingLabelWithString(ns_string!("预览持续 10 秒。⌥⌘P 显示 / 隐藏，⌃⌥⌘H 立即隐藏；关闭窗口后，从菜单栏 ◎ 继续控制。"), mtm);
                label.setTextColor(Some(&NSColor::secondaryLabelColor()));
                label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
                label.setFrame(NSRect::new(
                    NSPoint::new(28.0, 20.0),
                    NSSize::new(424.0, 60.0),
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
        for (index, title) in ["聚光", "数字激光", "实时放大"].iter().enumerate() {
            let item = self.item(&menu, title, sel!(selectEffect:), "");
            item.setTag(index as isize);
            self.ivars().effect_items.borrow_mut().push(item);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        self.item(&menu, "显示 / 隐藏    ⌥⌘P", sel!(toggle:), "");
        self.item(&menu, "预览 10 秒", sel!(preview:), "");
        self.item(&menu, "立即隐藏    ⌃⌥⌘H", sel!(hide:), "");
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
        if now - self.ivars().screen_check.get() >= 1.0 {
            self.refresh_screens();
            self.ivars().screen_check.set(now);
        }
        if let Some(error) = self.ivars().capture.error() {
            self.ivars().presentation.borrow_mut().hide();
            self.ivars().capture.stop();
            self.set_message(&format!("放大镜已停止：{error}"));
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
        for overlay in self.ivars().overlays.borrow().iter() {
            if active {
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
        };
        for item in self.ivars().effect_items.borrow().iter() {
            item.setState(if item.tag() == selected { 1 } else { 0 });
        }
    }
}

unsafe fn hotkey_action(target: *mut std::ffi::c_void, action: u32) {
    // SAFETY: Carbon callback is on main and the delegate outlives registration.
    let delegate = unsafe { &*(target as *const Delegate) };
    match action {
        1 => delegate
            .ivars()
            .presentation
            .borrow_mut()
            .toggle(delegate.now()),
        2 => delegate.ivars().presentation.borrow_mut().hide(),
        _ => return,
    }
    delegate.tick();
    delegate.set_message(
        if delegate
            .ivars()
            .presentation
            .borrow()
            .active(delegate.now())
        {
            "效果已显示；⌃⌥⌘H 可立即隐藏。"
        } else {
            "效果已隐藏。⌥⌘P 可再次显示。"
        },
    );
}

pub fn run(demo: Option<(Effect, f64)>) -> Result<(), Box<dyn std::error::Error>> {
    let mtm = MainThreadMarker::new().ok_or("AppKit must start on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = Delegate::alloc(mtm).set_ivars(AppData {
        start: Instant::now(),
        presentation: RefCell::new(Presentation::default()),
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
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
    Ok(())
}
