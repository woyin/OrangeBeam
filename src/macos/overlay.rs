//! Non-activating overlay panels, incremental dirty-rect computation and NSView drawing.
use super::capture::Capture;
use objc2::rc::Retained;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;
use orange_beam::presentation::{BoxView, Effect, Point, Presentation, Rect};
use std::cell::{Cell, RefCell};

#[derive(Default)]
pub(super) struct ViewData {
    pub(super) effect: Cell<Effect>,
    pub(super) center: Cell<NSPoint>,
    pub(super) radius: Cell<f64>,
    pub(super) shade: Cell<f64>,
    pub(super) zoom: Cell<f64>,
    pub(super) image: RefCell<Option<Retained<NSImage>>>,
    pub(super) sequence: Cell<u64>,
    pub(super) black: Cell<bool>,
    /// Box effect content in view coordinates.
    pub(super) box_draw: Cell<BoxDraw>,
}

#[derive(Default, Clone, Copy, PartialEq)]
pub(super) enum BoxDraw {
    #[default]
    None,
    Aim(NSPoint),
    Anchor(NSPoint),
    Rect(NSRect),
}

/// Everything that changes the uniform background, versus the small region
/// whose pixels differ from it. Only a background change needs a full repaint.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Look {
    black: bool,
    effect: Effect,
    shade: f64,
    zoom: f64,
    /// Outside the feature is dimmed (spotlight, magnifier, box) or clear.
    shaded: bool,
    feature: Option<NSRect>,
}

/// Margin covering the 2 pt border stroke and antialiasing.
const REPAINT_MARGIN: f64 = 4.0;

fn union_rect(a: NSRect, b: NSRect) -> NSRect {
    let x = a.origin.x.min(b.origin.x);
    let y = a.origin.y.min(b.origin.y);
    let right = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let top = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    NSRect::new(NSPoint::new(x, y), NSSize::new(right - x, top - y))
}

/// Logical (points) rectangle of an AppKit frame, in the global space the
/// presentation code works in.
pub(super) fn screen_rect(frame: NSRect) -> Rect {
    Rect {
        x: frame.origin.x,
        y: frame.origin.y,
        width: frame.size.width,
        height: frame.size.height,
    }
}

/// The one rule for which display owns a global point: the half-open
/// rectangle in logical points, so a shared edge belongs to exactly one.
pub(super) fn contains(rect: Rect, point: NSPoint) -> bool {
    rect.local_point(Point {
        x: point.x,
        y: point.y,
    })
    .is_some()
}

impl ViewData {
    pub(super) fn look(&self) -> Look {
        let around = |p: NSPoint, r: f64| {
            let r = r + REPAINT_MARGIN;
            NSRect::new(
                NSPoint::new(p.x - r, p.y - r),
                NSSize::new(r * 2.0, r * 2.0),
            )
        };
        let (shaded, feature) = match self.effect.get() {
            Effect::Spotlight | Effect::Magnify => {
                (true, Some(around(self.center.get(), self.radius.get())))
            }
            // Outer glow radius of the laser dot.
            Effect::Laser => (false, Some(around(self.center.get(), 17.0))),
            Effect::Box => match self.box_draw.get() {
                BoxDraw::None => (false, None),
                // Crosshair arm length plus the marker's outline.
                BoxDraw::Aim(p) | BoxDraw::Anchor(p) => (false, Some(around(p, 20.0))),
                BoxDraw::Rect(r) => (
                    true,
                    Some(NSRect::new(
                        NSPoint::new(r.origin.x - REPAINT_MARGIN, r.origin.y - REPAINT_MARGIN),
                        NSSize::new(
                            r.size.width + REPAINT_MARGIN * 2.0,
                            r.size.height + REPAINT_MARGIN * 2.0,
                        ),
                    )),
                ),
            },
        };
        Look {
            black: self.black.get(),
            effect: self.effect.get(),
            shade: self.shade.get(),
            zoom: self.zoom.get(),
            shaded,
            feature,
        }
    }
}

/// The rectangle to repaint after a change from `before` to `after`, or
/// None for a full repaint. Moving a spotlight only alters its old and new
/// circles; the dimmed screen around them stays identical.
pub(super) fn repaint_region(
    before: Look,
    after: Look,
    content_changed: bool,
) -> Option<Option<NSRect>> {
    let background = |l: Look| (l.black, l.effect, l.shade, l.zoom, l.shaded);
    if background(before) != background(after) {
        return None;
    }
    if before.feature == after.feature && !content_changed {
        return Some(None);
    }
    Some(match (before.feature, after.feature) {
        (Some(a), Some(b)) => Some(union_rect(a, b)),
        (a, b) => a.or(b),
    })
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
    pub(super) struct OverlayView;
    unsafe impl NSObjectProtocol for OverlayView {}
    impl OverlayView {
        #[unsafe(method(isOpaque))]
        fn opaque(&self) -> bool { false }
        #[unsafe(method(drawRect:))]
        fn draw(&self, dirty: NSRect) {
            let data = self.ivars();
            let bounds = self.bounds();
            let center = data.center.get();
            let radius = data.radius.get();
            let circle = NSRect::new(NSPoint::new(center.x - radius, center.y - radius), NSSize::new(radius * 2.0, radius * 2.0));
            // Clear the previous frame explicitly to avoid trails in a transparent window.
            NSColor::clearColor().setFill();
            NSRectFillUsingOperation(dirty, NSCompositingOperation::Copy);
            if data.black.get() {
                NSColor::blackColor().setFill();
                NSRectFillUsingOperation(dirty, NSCompositingOperation::Copy);
                return;
            }
            match data.effect.get() {
                Effect::Spotlight | Effect::Magnify => {
                    let mask = NSBezierPath::bezierPathWithRect(dirty);
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
                        let mask = NSBezierPath::bezierPathWithRect(dirty);
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
    pub(super) struct OverlayPanel;
    unsafe impl NSObjectProtocol for OverlayPanel {}
    impl OverlayPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_key(&self) -> bool { false }
        #[unsafe(method(canBecomeMainWindow))]
        fn can_main(&self) -> bool { false }
    }
);

pub(super) struct Overlay {
    panel: Retained<OverlayPanel>,
    view: Retained<OverlayView>,
    /// Panels stay ordered in; "hidden" means fully transparent, so showing
    /// needs neither a new backing store nor a full-screen repaint.
    pub(super) shown: Cell<bool>,
    pub(super) frame: NSRect,
    pub(super) display_id: u32,
    pub(super) scale: f64,
}

pub(super) fn display_id(screen: &NSScreen) -> u32 {
    screen
        .deviceDescription()
        .objectForKey(ns_string!("NSScreenNumber"))
        .and_then(|value| value.downcast::<NSNumber>().ok())
        .map(|number| number.unsignedIntValue())
        .unwrap_or(0)
}

impl Overlay {
    pub(super) fn new(mtm: MainThreadMarker, screen: &NSScreen, state: &Presentation) -> Self {
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
            panel.setTitle(ns_string!("Orange Beam Overlay"));
            let view = OverlayView::alloc(mtm).set_ivars(ViewData::default());
            let view: Retained<OverlayView> =
                msg_send![super(view), initWithFrame: NSRect::new(NSPoint::ZERO, frame.size)];
            panel.setContentView(Some(&view));
            // Pre-render the current effect centred on the screen while fully
            // transparent, so the first press only repaints the moved circle.
            let data = view.ivars();
            data.effect.set(state.effect);
            data.radius.set(state.radius);
            data.shade.set(state.shade);
            data.zoom.set(state.zoom);
            data.center.set(NSPoint::new(
                frame.size.width / 2.0,
                frame.size.height / 2.0,
            ));
            panel.setAlphaValue(0.0);
            panel.orderFrontRegardless();
            view.display();
            Self {
                panel,
                view,
                shown: Cell::new(false),
                frame,
                display_id,
                scale: screen.backingScaleFactor(),
            }
        }
    }
    pub(super) fn hide(&self) {
        if self.shown.replace(false) {
            self.panel.setAlphaValue(0.0);
        }
        self.view.ivars().image.replace(None);
    }
    /// Remove the panel for good (display configuration changed).
    pub(super) fn retire(&self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
    fn set_level(&self, level: NSWindowLevel) {
        if self.panel.level() != level {
            self.panel.setLevel(level);
        }
    }
    pub(super) fn render_blackout(&self) {
        // Above the menu bar and Dock, which sit over the floating level.
        self.set_level(NSScreenSaverWindowLevel);
        let data = self.view.ivars();
        let before = data.look();
        data.image.replace(None);
        data.black.set(true);
        self.repaint(before, false);
    }
    /// Invalidate only what changed since `before`. The transparent panel
    /// keeps its last frame, so revealing it is incremental too; it is drawn
    /// synchronously first so no stale frame is ever visible.
    fn repaint(&self, before: Look, content_changed: bool) {
        match repaint_region(before, self.view.ivars().look(), content_changed) {
            None => self.view.setNeedsDisplay(true),
            Some(Some(dirty)) => self.view.setNeedsDisplayInRect(dirty),
            Some(None) => {}
        }
        if !self.shown.replace(true) {
            self.view.displayIfNeeded();
            self.panel.setAlphaValue(1.0);
        }
    }
    pub(super) fn global_frame(&self) -> Rect {
        screen_rect(self.frame)
    }
    /// Box effect: a rectangle shades every display it touches; corner
    /// markers appear only on the display that contains them.
    pub(super) fn render_box(&self, view: BoxView, state: &Presentation) {
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
        self.set_level(NSFloatingWindowLevel);
        let data = self.view.ivars();
        let before = data.look();
        data.image.replace(None);
        data.black.set(false);
        data.effect.set(Effect::Box);
        data.box_draw.set(draw);
        data.shade.set(state.shade);
        self.repaint(before, false);
    }
    pub(super) fn render(&self, pointer: NSPoint, state: &Presentation, capture: &Capture) {
        let Some(local) = self.global_frame().local_point(Point {
            x: pointer.x,
            y: pointer.y,
        }) else {
            self.hide();
            return;
        };
        let data = self.view.ivars();
        let before = data.look();
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
        self.set_level(NSFloatingWindowLevel);
        data.black.set(false);
        data.center.set(NSPoint::new(local.x, local.y));
        data.effect.set(state.effect);
        data.radius.set(state.radius);
        data.shade.set(state.shade);
        data.zoom.set(state.zoom);
        // A new magnifier frame changes the circle's content in place.
        self.repaint(before, fresh_image);
    }
}
