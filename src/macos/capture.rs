//! ScreenCaptureKit capture, with own-process windows excluded from the stream.
//! Frame delivery and mutable state are confined to the main dispatch queue.
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_core_graphics::CGImage;
use objc2_core_image::{CIContext, CIImage};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::*;
use objc2_screen_capture_kit::*;
use std::cell::{Cell, RefCell};

/// Extra logical points around the magnifier source circle so small pointer
/// movements on a static screen reuse the same rasterized CGImage.
const TILE_MARGIN_POINTS: f64 = 128.0;
/// Extra logical points beyond `radius / zoom` to cover circle antialiasing.
const NEED_MARGIN_POINTS: f64 = 2.0;

/// The last rasterized tile, keyed by frame so a still screen rasterizes once.
struct CachedTile {
    sequence: u64,
    point_rect: NSRect,
    image: Retained<CGImage>,
}

#[derive(Default)]
pub(super) struct CaptureData {
    epoch: Cell<u64>,
    display: Cell<Option<u32>>,
    width: Cell<usize>,
    height: Cell<usize>,
    stream: RefCell<Option<Retained<SCStream>>>,
    context: RefCell<Option<Retained<CIContext>>>,
    /// Latest captured frame, kept lazy: the output callback must not rasterize.
    source: RefCell<Option<Retained<CIImage>>>,
    tile: RefCell<Option<CachedTile>>,
    sequence: Cell<u64>,
    error: RefCell<Option<String>>,
}

define_class!(
    // SAFETY: Every ivar access occurs on the main thread. ScreenCaptureKit's
    // non-main completion/delegate callbacks only enqueue selectors through NSObject.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = CaptureData]
    pub(super) struct Capture;
    unsafe impl NSObjectProtocol for Capture {}
    unsafe impl SCStreamOutput for Capture {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn output(&self, stream: &SCStream, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            // The output queue is explicitly DispatchQueue::main(). Ignore callbacks
            // from a stopped/replaced stream, including late frames during switching.
            if kind != SCStreamOutputType::Screen || !self.is_current(stream) { return; }
            let Some(pixel) = buffer.image_buffer() else { return; };
            // Only the lazy source is stored. Rasterizing a whole 4K/5K screen
            // here stalled the main queue up to 30 times per second; the tile is
            // now cropped on demand in `tile`.
            let image = CIImage::imageWithCVPixelBuffer(&pixel);
            self.ivars().source.replace(Some(image));
            self.ivars().sequence.set(self.ivars().sequence.get().wrapping_add(1));
        }
    }
    unsafe impl SCStreamDelegate for Capture {
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn stopped(&self, stream: &SCStream, error: &NSError) {
            // ScreenCaptureKit does not promise this callback's thread. No ivars or
            // AppKit are touched; NSObject retains the arguments until main delivery.
            let args = NSArray::<AnyObject>::from_slice(&[stream.as_ref(), error.as_ref()]);
            self.performSelectorOnMainThread_withObject_waitUntilDone(sel!(streamFailed:), Some(&args), false);
        }
    }
    impl Capture {
        #[unsafe(method(acceptContent:))]
        fn accept_content(&self, args: &NSArray<AnyObject>) {
            let Some(epoch) = args.objectAtIndex(0).downcast_ref::<NSNumber>().map(|n| n.unsignedLongLongValue()) else { return; };
            if epoch != self.ivars().epoch.get() { return; }
            let result = args.objectAtIndex(1);
            if let Some(error) = result.downcast_ref::<NSError>() {
                self.ivars().error.replace(Some(error.localizedDescription().to_string()));
                return;
            }
            let Some(content) = result.downcast_ref::<SCShareableContent>() else { return; };
            // SAFETY: Immutable shareable-content metadata is consumed on the main
            // thread; the stream and output delegate are retained for the session.
            unsafe {
                let Some(display) = content.displays().iter().find(|d| Some(d.displayID()) == self.ivars().display.get()) else {
                    self.ivars().error.replace(Some("目标显示器已断开".to_string()));
                    return;
                };
                let own_apps: Vec<_> = content.applications().iter().filter(|a| a.processID() == std::process::id() as i32).collect();
                if own_apps.is_empty() {
                    self.ivars().error.replace(Some("无法从采集中排除本程序，放大镜已停止".to_string()));
                    return;
                }
                let excluded = NSArray::from_retained_slice(&own_apps);
                let filter = SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(SCContentFilter::alloc(), &display, &excluded, &NSArray::new());
                let config = SCStreamConfiguration::new();
                config.setWidth(self.ivars().width.get());
                config.setHeight(self.ivars().height.get());
                config.setMinimumFrameInterval(CMTime { value: 1, timescale: 30, flags: CMTimeFlags::Valid, epoch: 0 });
                config.setQueueDepth(3);
                config.setShowsCursor(false);
                config.setCapturesAudio(false);
                let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(ProtocolObject::from_ref(self)));
                if let Err(error) = stream.addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(self), SCStreamOutputType::Screen, Some(DispatchQueue::main())) {
                    self.ivars().error.replace(Some(error.localizedDescription().to_string()));
                    return;
                }
                self.ivars().stream.replace(Some(stream.clone()));
                let target = Retained::retain(self as *const Self as *mut Self).expect("live capture controller");
                let block = RcBlock::new(move |error: *mut NSError| {
                    if let Some(error) = error.as_ref() {
                        let number = NSNumber::new_u64(epoch);
                        let args = NSArray::<AnyObject>::from_slice(&[number.as_ref(), error.as_ref()]);
                        target.performSelectorOnMainThread_withObject_waitUntilDone(sel!(acceptContent:), Some(&args), false);
                    }
                });
                stream.startCaptureWithCompletionHandler(Some(&block));
            }
        }
        #[unsafe(method(streamFailed:))]
        fn stream_failed(&self, args: &NSArray<AnyObject>) {
            let object = args.objectAtIndex(0);
            let Some(stream) = object.downcast_ref::<SCStream>() else { return; };
            if !self.is_current(stream) { return; }
            let error = args.objectAtIndex(1);
            if let Some(error) = error.downcast_ref::<NSError>() {
                self.ivars().error.replace(Some(error.localizedDescription().to_string()));
            }
            self.ivars().source.replace(None);
            self.ivars().tile.replace(None);
        }
    }
);

impl Capture {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CaptureData::default());
        unsafe { msg_send![super(this), init] }
    }
    fn is_current(&self, stream: &SCStream) -> bool {
        self.ivars()
            .stream
            .borrow()
            .as_ref()
            .is_some_and(|current| std::ptr::eq(&**current, stream))
    }
    pub fn start(&self, display_id: u32, width: usize, height: usize) {
        if self.ivars().display.get() == Some(display_id) {
            return;
        }
        self.stop();
        self.ivars().display.set(Some(display_id));
        self.ivars().width.set(width);
        self.ivars().height.set(height);
        let epoch = self.ivars().epoch.get();
        // SAFETY: Completion may run off main. It only retains immutable metadata
        // and schedules the actual work on main. The app owns this controller until
        // process termination, so releasing a completion cannot deallocate it off main.
        unsafe {
            let target = Retained::retain(self as *const Self as *mut Self)
                .expect("live capture controller");
            let block = RcBlock::new(
                move |content: *mut SCShareableContent, error: *mut NSError| {
                    let result: Option<&AnyObject> = if let Some(error) = error.as_ref() {
                        Some(error.as_ref())
                    } else {
                        content.as_ref().map(|c| c.as_ref())
                    };
                    if let Some(result) = result {
                        let number = NSNumber::new_u64(epoch);
                        let args = NSArray::<AnyObject>::from_slice(&[number.as_ref(), result]);
                        target.performSelectorOnMainThread_withObject_waitUntilDone(
                            sel!(acceptContent:),
                            Some(&args),
                            false,
                        );
                    }
                },
            );
            SCShareableContent::getShareableContentWithCompletionHandler(&block);
        }
    }
    pub fn stop(&self) {
        self.ivars()
            .epoch
            .set(self.ivars().epoch.get().wrapping_add(1));
        self.ivars().display.set(None);
        if let Some(stream) = self.ivars().stream.borrow_mut().take() {
            unsafe {
                stream.stopCaptureWithCompletionHandler(None);
            }
        }
        self.ivars().source.replace(None);
        self.ivars().tile.replace(None);
        self.ivars().context.replace(None);
        self.ivars().error.replace(None);
    }
    /// Rasterizes only the padded tile around `center` (view points), reusing
    /// the cached one while the frame is unchanged and the magnifier circle
    /// still fits inside it. Returns the frame sequence, the point rectangle the
    /// image covers and the image itself.
    pub fn tile(
        &self,
        center: NSPoint,
        radius: f64,
        zoom: f64,
        scale: f64,
        view_size: NSSize,
    ) -> Option<(u64, NSRect, Retained<CGImage>)> {
        // Cloned rather than borrowed: rasterizing must not make a callback in
        // the same turn fail with a nested borrow.
        let source = self.ivars().source.borrow().clone()?;
        let sequence = self.ivars().sequence.get();
        // A circle of `radius` drawn at `zoom` samples this much of the screen.
        let need_half = radius / zoom + NEED_MARGIN_POINTS;
        let nx0 = (center.x - need_half).clamp(0.0, view_size.width);
        let ny0 = (center.y - need_half).clamp(0.0, view_size.height);
        let nx1 = (center.x + need_half).clamp(0.0, view_size.width);
        let ny1 = (center.y + need_half).clamp(0.0, view_size.height);
        let needed = NSRect::new(NSPoint::new(nx0, ny0), NSSize::new(nx1 - nx0, ny1 - ny0));
        if let Some(cached) = self.ivars().tile.borrow().as_ref() {
            if cached.sequence == sequence && rect_contains(cached.point_rect, needed) {
                return Some((sequence, cached.point_rect, cached.image.clone()));
            }
        }
        // SAFETY: The extent is read-only metadata of a retained image, and the
        // crop runs on the main thread with a context owned by this controller.
        let (pixel_rect, point_rect) =
            tile_bounds(unsafe { source.extent() }, center, radius, zoom, scale)?;
        let mut context = self.ivars().context.borrow_mut();
        let context = context.get_or_insert_with(|| unsafe { CIContext::contextWithOptions(None) });
        let image = unsafe { context.createCGImage_fromRect(&source, pixel_rect) }?;
        self.ivars().tile.replace(Some(CachedTile {
            sequence,
            point_rect,
            image: image.clone(),
        }));
        Some((sequence, point_rect, image))
    }
    /// Whether a frame is available, without cloning the image to check it.
    pub fn has_frame(&self) -> bool {
        self.ivars().source.borrow().is_some()
    }
    pub fn error(&self) -> Option<String> {
        self.ivars().error.borrow().clone()
    }
}

/// Computes integer-aligned `(pixel_rect, point_rect)` for a padded tile
/// around `center` (in view points), clamped to `extent` (in pixels).
pub(super) fn tile_bounds(
    extent: NSRect,
    center: NSPoint,
    radius: f64,
    zoom: f64,
    scale: f64,
) -> Option<(NSRect, NSRect)> {
    let half = radius / zoom + TILE_MARGIN_POINTS;
    let max_x = extent.origin.x + extent.size.width;
    let max_y = extent.origin.y + extent.size.height;
    let x0 = (extent.origin.x + (center.x - half) * scale)
        .floor()
        .clamp(extent.origin.x, max_x);
    let y0 = (extent.origin.y + (center.y - half) * scale)
        .floor()
        .clamp(extent.origin.y, max_y);
    let x1 = (extent.origin.x + (center.x + half) * scale)
        .ceil()
        .clamp(extent.origin.x, max_x);
    let y1 = (extent.origin.y + (center.y + half) * scale)
        .ceil()
        .clamp(extent.origin.y, max_y);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let pixel_rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0));
    let point_rect = NSRect::new(
        NSPoint::new(
            (x0 - extent.origin.x) / scale,
            (y0 - extent.origin.y) / scale,
        ),
        NSSize::new((x1 - x0) / scale, (y1 - y0) / scale),
    );
    Some((pixel_rect, point_rect))
}

/// Whether a cached tile still covers everything the magnifier circle samples.
/// An empty need (the pointer is off screen) is always covered.
fn rect_contains(outer: NSRect, inner: NSRect) -> bool {
    const TOLERANCE: f64 = 1e-6;
    inner.size.width <= 0.0
        || inner.size.height <= 0.0
        || (inner.origin.x >= outer.origin.x - TOLERANCE
            && inner.origin.y >= outer.origin.y - TOLERANCE
            && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + TOLERANCE
            && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + TOLERANCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    #[test]
    fn tile_bounds_are_pixel_aligned_and_cover_the_circle() {
        // A 3840x2160 pixel display is 1920x1080 points at scale 2. radius 60
        // at zoom 2 is 30 pt; with the 128 pt margin the half size is 158 pt,
        // i.e. a 632 px tile, 812 px for a 150 pt radius.
        let extent = rect(0.0, 0.0, 3840.0, 2160.0);
        let (pixel, point) =
            tile_bounds(extent, NSPoint::new(960.0, 540.0), 60.0, 2.0, 2.0).expect("tile");
        assert_eq!(pixel, rect(1604.0, 764.0, 632.0, 632.0));
        assert_eq!(point, rect(802.0, 382.0, 316.0, 316.0));
        assert_eq!(pixel.size.width, point.size.width * 2.0);
        // The cached tile always covers the sampled circle, which is what lets
        // a static pointer move without a new rasterization.
        assert!(rect_contains(
            point,
            rect(960.0 - 32.0, 540.0 - 32.0, 64.0, 64.0)
        ));
        // A fractional centre is expanded outwards to whole pixels, never in.
        let (pixel, point) =
            tile_bounds(extent, NSPoint::new(1920.3, 1080.7), 60.0, 2.0, 1.0).expect("tile");
        assert_eq!(pixel, rect(1762.0, 922.0, 317.0, 317.0));
        assert_eq!(point, pixel);
    }

    #[test]
    fn tile_bounds_clamp_at_screen_edges_and_reject_off_screen() {
        let extent = rect(0.0, 0.0, 800.0, 450.0);
        // Top left corner: both edges clip, so no pixel outside the screen.
        let (pixel, point) =
            tile_bounds(extent, NSPoint::new(90.0, 70.0), 60.0, 2.0, 1.0).expect("tile");
        assert_eq!(pixel, rect(0.0, 0.0, 248.0, 228.0));
        assert_eq!(point, pixel);
        // Bottom right corner clips to the remaining 168 pt.
        let (pixel, _) =
            tile_bounds(extent, NSPoint::new(790.0, 440.0), 60.0, 2.0, 1.0).expect("tile");
        assert_eq!(pixel, rect(632.0, 282.0, 168.0, 168.0));
        // A pointer outside the screen leaves an empty tile instead of a panic.
        assert_eq!(
            tile_bounds(extent, NSPoint::new(2000.0, 2000.0), 60.0, 2.0, 1.0),
            None
        );
    }

    #[test]
    fn cached_tile_covers_small_pointer_moves_only() {
        let tile = rect(242.0, 67.0, 316.0, 316.0);
        // Staying inside the 128 pt margin reuses the cached image.
        assert!(rect_contains(tile, rect(368.0, 193.0, 64.0, 64.0)));
        assert!(rect_contains(tile, rect(468.0, 293.0, 64.0, 64.0)));
        // Moving past the margin has to rasterize again.
        assert!(!rect_contains(tile, rect(600.0, 400.0, 64.0, 64.0)));
        // An empty need is covered by definition.
        assert!(rect_contains(tile, rect(1000.0, 1000.0, 0.0, 0.0)));
    }
}
