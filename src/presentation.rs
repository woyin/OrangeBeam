//! Rendering-independent presentation state and coordinate calculations.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    #[default]
    Spotlight,
    Laser,
    Magnify,
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

#[derive(Debug)]
pub struct Presentation {
    pub effect: Effect,
    pub radius: f64,
    pub shade: f64,
    pub zoom: f64,
    manual: bool,
    preview_until: Option<f64>,
    remote_held: bool,
    remote_suppressed: bool,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            effect: Effect::Spotlight,
            radius: 100.0,
            shade: 0.60,
            zoom: 2.0,
            manual: false,
            preview_until: None,
            remote_held: false,
            remote_suppressed: false,
        }
    }
}

impl Presentation {
    pub fn active(&self, now: f64) -> bool {
        self.manual
            || self.preview_until.is_some_and(|deadline| now < deadline)
            || (self.remote_held && !self.remote_suppressed)
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
        self.radius = (self.radius * factor).clamp(40.0, 300.0);
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
    fn radius_stays_in_usable_range() {
        let mut s = Presentation::default();
        s.resize(1000.0);
        assert_eq!(s.radius, 300.0);
        s.resize(0.0001);
        assert_eq!(s.radius, 40.0);
    }
}
