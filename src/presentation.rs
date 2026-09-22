//! Rendering-independent presentation state and coordinate calculations.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    #[default]
    Spotlight,
    Laser,
    Magnify,
    /// Two holds: the first aims and fixes the start corner on release, the
    /// second drags to the opposite corner. The rectangle then stays until a
    /// page turn, a new rectangle, an effect switch or hide.
    Box,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// Rectangle spanned by two opposite corners, in either order.
    pub fn spanning(a: Point, b: Point) -> Self {
        Self {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            width: (a.x - b.x).abs(),
            height: (a.y - b.y).abs(),
        }
    }
    pub fn intersects(self, other: Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
    pub fn local_point(self, point: Point) -> Option<Point> {
        (point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height)
            .then_some(Point {
                x: point.x - self.x,
                y: point.y - self.y,
            })
    }
}

pub const DEFAULT_RADIUS: f64 = 150.0;
pub const MIN_RADIUS: f64 = 40.0;
pub const MAX_RADIUS: f64 = 500.0;

pub fn clamp_radius(radius: f64) -> f64 {
    if radius.is_finite() {
        radius.clamp(MIN_RADIUS, MAX_RADIUS)
    } else {
        DEFAULT_RADIUS
    }
}

#[derive(Debug)]
pub struct Presentation {
    pub effect: Effect,
    pub radius: f64,
    pub shade: f64,
    pub zoom: f64,
    /// Full black screen on every display, independent of pointer effects.
    pub blackout: bool,
    manual: bool,
    preview_until: Option<f64>,
    /// Brief confirmation after switching effects, independent of manual mode.
    flash_until: Option<f64>,
    remote_held: bool,
    remote_suppressed: bool,
    box_aiming: bool,
    box_anchor: Option<Point>,
    box_rect: Option<Rect>,
}

/// What the Box effect currently shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoxView {
    /// First hold: the pointer is choosing the start corner.
    Aim(Point),
    /// Start corner fixed, waiting for the second hold.
    Anchor(Point),
    /// Being dragged (second hold) or kept after release.
    Rect(Rect),
}

/// Smaller drags are treated as an accidental press, not a rectangle.
pub const MIN_BOX_SIDE: f64 = 8.0;
/// Width and height of the rectangle shown when switching to Box.
const SAMPLE_BOX: (f64, f64) = (240.0, 150.0);

impl Default for Presentation {
    fn default() -> Self {
        Self {
            effect: Effect::Spotlight,
            radius: DEFAULT_RADIUS,
            shade: 0.60,
            zoom: 2.0,
            blackout: false,
            manual: false,
            preview_until: None,
            flash_until: None,
            remote_held: false,
            remote_suppressed: false,
            box_aiming: false,
            box_anchor: None,
            box_rect: None,
        }
    }
}

impl Presentation {
    pub fn active(&self, now: f64) -> bool {
        self.manual
            || self.preview_until.is_some_and(|deadline| now < deadline)
            || self.flashing(now)
            || (self.remote_held && !self.remote_suppressed)
            || (self.effect == Effect::Box
                && (self.box_rect.is_some() || self.box_anchor.is_some()))
    }
    fn previewing(&self, now: f64) -> bool {
        self.preview_until.is_some_and(|deadline| now < deadline)
    }
    fn flashing(&self, now: f64) -> bool {
        self.flash_until.is_some_and(|deadline| now < deadline)
    }
    /// Show the current effect briefly so a switch is visible.
    pub fn flash(&mut self, now: f64, seconds: f64) {
        self.flash_until = Some(now + seconds);
    }
    /// Top button pressed. Without a start corner this begins aiming (and
    /// drops any kept rectangle); with one it begins dragging the rectangle.
    pub fn begin_box(&mut self, _pointer: Point) {
        if self.effect != Effect::Box {
            return;
        }
        if self.box_anchor.is_none() {
            self.box_rect = None;
            self.box_aiming = true;
        }
    }
    /// Top button released: fix the start corner, or finish the rectangle.
    /// A click-sized rectangle cancels instead of leaving a sliver.
    pub fn finish_box(&mut self, pointer: Point) {
        if self.effect != Effect::Box {
            return;
        }
        if std::mem::take(&mut self.box_aiming) {
            self.box_anchor = Some(pointer);
        } else if let Some(anchor) = self.box_anchor.take() {
            let rect = Rect::spanning(anchor, pointer);
            if rect.width >= MIN_BOX_SIDE && rect.height >= MIN_BOX_SIDE {
                self.box_rect = Some(rect);
            }
        }
    }
    pub fn clear_box(&mut self) {
        self.box_aiming = false;
        self.box_anchor = None;
        self.box_rect = None;
    }
    pub fn has_box(&self) -> bool {
        self.box_rect.is_some() || self.box_anchor.is_some()
    }
    pub fn box_view(&self, pointer: Point, now: f64) -> Option<BoxView> {
        let held = self.remote_held && !self.remote_suppressed;
        match (self.box_aiming, self.box_anchor, self.box_rect) {
            (true, _, _) if held => Some(BoxView::Aim(pointer)),
            (_, Some(anchor), _) if held => Some(BoxView::Rect(Rect::spanning(anchor, pointer))),
            (_, Some(anchor), _) => Some(BoxView::Anchor(anchor)),
            (_, _, Some(rect)) => Some(BoxView::Rect(rect)),
            // Nothing drawn yet: a sample rectangle confirms a switch or preview.
            _ if self.flashing(now) || self.previewing(now) => Some(BoxView::Rect(Rect {
                x: pointer.x - SAMPLE_BOX.0 / 2.0,
                y: pointer.y - SAMPLE_BOX.1 / 2.0,
                width: SAMPLE_BOX.0,
                height: SAMPLE_BOX.1,
            })),
            _ => None,
        }
    }
    pub fn toggle(&mut self, now: f64) {
        if self.active(now) {
            self.hide();
        } else {
            self.preview_until = None;
            self.manual = true;
        }
    }
    pub fn preview(&mut self, now: f64, seconds: f64) {
        self.manual = false;
        self.remote_suppressed = self.remote_held;
        self.preview_until = Some(now + seconds);
    }
    pub fn hide(&mut self) {
        self.flash_until = None;
        self.clear_box();
        self.blackout = false;
        self.manual = false;
        self.preview_until = None;
        self.remote_suppressed = self.remote_held;
    }
    pub fn set_remote_held(&mut self, held: bool) {
        self.remote_held = held;
        if !held {
            self.remote_suppressed = false;
        }
    }
    pub fn resize(&mut self, factor: f64) {
        self.radius = clamp_radius(self.radius * factor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn displays_above_and_left_use_local_coordinates_without_dpi_scaling() {
        let left = Rect {
            x: -1920.0,
            y: 200.0,
            width: 1920.0,
            height: 1080.0,
        };
        assert_eq!(
            left.local_point(Point {
                x: -960.0,
                y: 740.0
            }),
            Some(Point { x: 960.0, y: 540.0 })
        );
        assert_eq!(left.local_point(Point { x: 0.0, y: 740.0 }), None);
        assert_eq!(
            left.local_point(Point {
                x: -960.0,
                y: 199.0
            }),
            None
        );
    }
    #[test]
    fn shared_display_edge_belongs_to_one_screen() {
        let first = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let second = Rect { x: 100.0, ..first };
        let edge = Point { x: 100.0, y: 50.0 };
        assert!(first.local_point(edge).is_none());
        assert!(second.local_point(edge).is_some());
    }
    #[test]
    fn preview_expires_and_hide_cancels_every_activation() {
        let mut s = Presentation::default();
        s.preview(5.0, 10.0);
        assert!(s.active(14.99));
        assert!(!s.active(15.0));
        s.toggle(16.0);
        assert!(s.active(1000.0));
        s.hide();
        assert!(!s.active(1000.0));
        s.preview(1000.0, 10.0);
        s.toggle(1001.0);
        assert!(!s.active(1002.0));
    }
    #[test]
    fn box_takes_two_holds_so_the_start_corner_can_be_aimed() {
        let mut s = Presentation {
            effect: Effect::Box,
            ..Default::default()
        };
        let aim = Point { x: 50.0, y: 60.0 };
        let start = Point { x: 300.0, y: 400.0 };
        let end = Point { x: 100.0, y: 250.0 };
        // First hold: pointer moves freely; release fixes the start corner.
        s.set_remote_held(true);
        s.begin_box(aim);
        assert_eq!(s.box_view(aim, 0.0), Some(BoxView::Aim(aim)));
        assert_eq!(s.box_view(start, 0.0), Some(BoxView::Aim(start)));
        s.finish_box(start);
        s.set_remote_held(false);
        assert_eq!(s.box_view(end, 0.0), Some(BoxView::Anchor(start)));
        assert!(s.active(1.0), "the start marker stays visible");
        // Second hold drags live from the fixed corner; release keeps it.
        s.set_remote_held(true);
        s.begin_box(start);
        let dragged = Rect {
            x: 100.0,
            y: 250.0,
            width: 200.0,
            height: 150.0,
        };
        assert_eq!(s.box_view(end, 0.0), Some(BoxView::Rect(dragged)));
        s.finish_box(end);
        s.set_remote_held(false);
        assert_eq!(s.box_view(aim, 0.0), Some(BoxView::Rect(dragged)));
        assert!(s.active(1.0));
        // The next hold aims a new rectangle and drops the kept one.
        s.set_remote_held(true);
        s.begin_box(aim);
        assert_eq!(s.box_view(aim, 0.0), Some(BoxView::Aim(aim)));
        s.finish_box(aim);
        s.set_remote_held(false);
        // A click-sized second drag cancels instead of leaving a sliver.
        s.set_remote_held(true);
        s.begin_box(aim);
        s.finish_box(Point { x: 53.0, y: 62.0 });
        s.set_remote_held(false);
        assert_eq!(s.box_view(aim, 0.0), None);
        assert!(!s.active(1.0));
    }
    #[test]
    fn box_is_cleared_by_hide_and_ignored_outside_box_mode() {
        let mut s = Presentation {
            effect: Effect::Box,
            ..Default::default()
        };
        s.set_remote_held(true);
        s.begin_box(Point { x: 0.0, y: 0.0 });
        s.finish_box(Point { x: 0.0, y: 0.0 });
        s.set_remote_held(false);
        assert!(s.has_box(), "start corner fixed");
        s.set_remote_held(true);
        s.begin_box(Point { x: 0.0, y: 0.0 });
        s.hide(); // emergency hide during the drag
        s.finish_box(Point { x: 200.0, y: 200.0 });
        s.set_remote_held(false);
        assert!(!s.has_box());
        s.effect = Effect::Spotlight;
        s.set_remote_held(true);
        s.begin_box(Point { x: 0.0, y: 0.0 });
        s.finish_box(Point { x: 200.0, y: 200.0 });
        assert!(!s.has_box(), "spotlight mode never creates a rectangle");
    }
    #[test]
    fn switching_flashes_the_effect_without_touching_manual_mode() {
        let mut s = Presentation::default();
        s.flash(10.0, 1.2);
        assert!(s.active(10.5));
        assert!(!s.active(11.2));
        s.toggle(20.0); // manual on
        s.flash(20.0, 1.2);
        assert!(s.active(30.0), "flash expiry keeps manual mode");
        s.hide();
        s.flash(40.0, 1.2);
        s.hide();
        assert!(!s.active(40.1), "hide cancels a flash");
        // Box mode shows a sample rectangle only while flashing.
        let mut b = Presentation {
            effect: Effect::Box,
            ..Default::default()
        };
        let p = Point { x: 500.0, y: 300.0 };
        b.flash(1.0, 1.2);
        assert_eq!(
            b.box_view(p, 1.5),
            Some(BoxView::Rect(Rect {
                x: 380.0,
                y: 225.0,
                width: 240.0,
                height: 150.0
            }))
        );
        assert_eq!(b.box_view(p, 2.5), None);
        b.preview(3.0, 10.0);
        assert!(matches!(b.box_view(p, 4.0), Some(BoxView::Rect(_))));
    }
    #[test]
    fn rectangles_intersect_only_when_overlapping() {
        let screen = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let right = Rect { x: 100.0, ..screen };
        assert!(!screen.intersects(right));
        assert!(screen.intersects(Rect { x: 99.0, ..screen }));
    }
    #[test]
    fn immediate_hide_also_ends_black_screen() {
        let mut s = Presentation {
            blackout: true,
            ..Default::default()
        };
        s.set_remote_held(false);
        assert!(
            s.blackout,
            "releasing the top button does not end black screen"
        );
        s.hide();
        assert!(!s.blackout);
    }
    #[test]
    fn radius_stays_in_usable_range() {
        let mut s = Presentation::default();
        s.resize(1000.0);
        assert_eq!(s.radius, 500.0);
        s.resize(0.0001);
        assert_eq!(s.radius, 40.0);
    }
}
