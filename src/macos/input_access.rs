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
