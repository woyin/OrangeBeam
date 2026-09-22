//! Read-only preflight for diagnostics; never changes the system's input policy.
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    // CarbonEventsCore.h: Boolean is an unsigned byte.
    fn IsSecureEventInputEnabled() -> u8;
}

pub fn secure_input_enabled() -> bool {
    // SAFETY: A no-argument, read-only Carbon API with no ownership requirements.
    unsafe { IsSecureEventInputEnabled() != 0 }
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IOHIDLib.h: IOHIDAccessType IOHIDCheckAccess(IOHIDRequestType).
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

const LISTEN_EVENT: u32 = 1; // kIOHIDRequestTypeListenEvent
const GRANTED: u32 = 0; // kIOHIDAccessTypeGranted

/// Input Monitoring, needed to read the remote's HID reports.
pub fn input_monitoring_granted() -> bool {
    // SAFETY: read-only TCC query with a documented enum argument.
    unsafe { IOHIDCheckAccess(LISTEN_EVENT) == GRANTED }
}

/// Shows the system prompt the first time; later calls only report state.
pub fn request_input_monitoring() -> bool {
    // SAFETY: documented TCC request; it never changes policy by itself.
    unsafe { IOHIDRequestAccess(LISTEN_EVENT) }
}
