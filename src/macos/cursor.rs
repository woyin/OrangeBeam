//! Hides the system pointer while a remote effect stands in for it.
//!
//! A background (accessory) app may only hide the cursor after setting the
//! private WindowServer connection property "SetsCursorInBackground", which
//! presentation and cursor utilities commonly use. If that call fails (for
//! example after an OS change), the pointer simply stays visible.
use objc2_core_graphics::{CGDisplayHideCursor, CGDisplayShowCursor, CGMainDisplayID};
use objc2_foundation::{NSNumber, NSString};
use std::ffi::c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    // Re-exported from SkyLight; not in the public SDK headers.
    fn CGSMainConnectionID() -> i32;
    fn CGSSetConnectionProperty(
        connection: i32,
        target: i32,
        key: *const c_void,
        value: *const c_void,
    ) -> i32;
}

#[derive(Default)]
pub struct CursorHider {
    /// None until first needed; then whether background hiding is allowed.
    allowed: Option<bool>,
    hidden: bool,
}

impl CursorHider {
    fn allowed(&mut self) -> bool {
        *self.allowed.get_or_insert_with(|| {
            let key = NSString::from_str("SetsCursorInBackground");
            let value = NSNumber::new_bool(true);
            // SAFETY: NSString/NSNumber(bool) are toll-free bridged to the
            // CFString key and CFBoolean value; both outlive the call.
            let status = unsafe {
                let connection = CGSMainConnectionID();
                CGSSetConnectionProperty(
                    connection,
                    connection,
                    (&*key as *const NSString).cast(),
                    (&*value as *const NSNumber).cast(),
                )
            };
            if status != 0 {
                eprintln!("CURSOR background hiding unavailable (CGSSetConnectionProperty={status}); pointer stays visible");
            }
            status == 0
        })
    }

    /// Hide or show; calls are balanced so the system hide count never drifts.
    pub fn set_hidden(&mut self, hide: bool) {
        if hide == self.hidden || (hide && !self.allowed()) {
            return;
        }
        let display = CGMainDisplayID();
        let status = if hide {
            CGDisplayHideCursor(display)
        } else {
            CGDisplayShowCursor(display)
        };
        if status.0 == 0 {
            self.hidden = hide;
        } else {
            eprintln!(
                "CURSOR {} failed: {}",
                if hide { "hide" } else { "show" },
                status.0
            );
        }
    }
}

impl Drop for CursorHider {
    fn drop(&mut self) {
        self.set_hidden(false);
    }
}
