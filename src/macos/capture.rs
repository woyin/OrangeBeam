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

#[derive(Default)]
pub(super) struct CaptureData {
    epoch: Cell<u64>,
    display: Cell<Option<u32>>,
    width: Cell<usize>,
    height: Cell<usize>,
    stream: RefCell<Option<Retained<SCStream>>>,
    context: RefCell<Option<Retained<CIContext>>>,
    image: RefCell<Option<Retained<CGImage>>>,
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
            let image = CIImage::imageWithCVPixelBuffer(&pixel);
            let mut context = self.ivars().context.borrow_mut();
            let context = context.get_or_insert_with(|| CIContext::contextWithOptions(None));
            if let Some(frame) = context.createCGImage_fromRect(&image, image.extent()) {
                self.ivars().image.replace(Some(frame));
                self.ivars().sequence.set(self.ivars().sequence.get().wrapping_add(1));
            }
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
            self.ivars().image.replace(None);
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
        self.ivars().image.replace(None);
        self.ivars().context.replace(None);
        self.ivars().error.replace(None);
    }
    pub fn frame(&self) -> Option<(u64, Retained<CGImage>)> {
        self.ivars()
            .image
            .borrow()
            .as_ref()
            .map(|image| (self.ivars().sequence.get(), image.clone()))
    }
    pub fn error(&self) -> Option<String> {
        self.ivars().error.borrow().clone()
    }
}
