# Protocol research

Research date: 2026-09-21. This document separates implementation evidence from
hardware verification. Initial research preceded hardware access; see
VALIDATION.md for subsequent Bluetooth query results.

## Sources

- Logitech's [feature overview](https://support.logi.com/hc/en-us/articles/360023265514-Use-Logitech-Presentation-Software-with-the-Spotlight-Presentation-Remote)
  describes highlight, magnify, digital laser, button customization, battery and timers.
- Logitech's [plug-and-play behavior](https://support.logi.com/hc/en-us/articles/360023195374-Using-the-Spotlight-Presentation-Remote-without-the-Logi-Options-App)
  documents cursor movement without the companion application.
- Logitech's [public protocol repository](https://github.com/Logitech/cpg-docs)
  is a source for further HID++ specifications; it was not used as evidence that
  every Spotlight-specific feature is publicly specified.
- Projecteur, pinned revision
  [`5a174f6790f82abda3360916940b66060cc2358b`](https://github.com/gbin/Projecteur/tree/5a174f6790f82abda3360916940b66060cc2358b), MIT:
  [device IDs](https://github.com/gbin/Projecteur/blob/5a174f6790f82abda3360916940b66060cc2358b/src/devicescan.cc),
  [feature codes](https://github.com/gbin/Projecteur/blob/5a174f6790f82abda3360916940b66060cc2358b/src/hidpp.h),
  [message matching](https://github.com/gbin/Projecteur/blob/5a174f6790f82abda3360916940b66060cc2358b/src/hidpp.cc),
  [transport and initialization](https://github.com/gbin/Projecteur/blob/5a174f6790f82abda3360916940b66060cc2358b/src/device-hidpp.cc).
- [hidapi-rs 2.6.7](https://docs.rs/hidapi/2.6.7/hidapi/)
  provides the HID transport. Its default Cargo features do not enable shared
  macOS access; the application explicitly calls `set_open_exclusive(false)`.
- Solaar's [HID++ implementation](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/hidpp20.py)
  and [control IDs](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/special_keys.py)
  were consulted for read-only REPROG_CONTROLS_V4 function/field definitions.
  The `inspect` implementation independently encodes getCount (0),
  getControlInfo (1), getCidReporting (2), and BATTERY_STATUS function 0.
  It does not issue setCidReporting (3).
- Logitech's [x1b04 version 2 specification, mirrored by Lekensteyn](https://lekensteyn.nl/files/logitech/x1b04_specialkeysmsebuttons.html)
  defines temporary diversion and button-state notifications used by `watch-top`.
- Apple [SCContentFilter](https://developer.apple.com/documentation/screencapturekit/sccontentfilter)
  is the proposed way to exclude the application's own windows from magnifier capture.

## Implemented subset

HID++ short reports are 7 bytes with report ID `10`; long reports are 20 bytes
with report ID `11`. Byte 1 is the device index, byte 2 the feature index, and
byte 3 combines function (high nibble) with software ID (low nibble).

The encoder emits long reports for both transports. Projecteur explicitly
converts Bluetooth requests to this format. Software ID `a` distinguishes this
client's requests from notifications (software ID zero); it is not a globally
reserved ID. Only one request is outstanding. A transaction timeout terminates
the command, preventing a late reply from being reused for the next ROOT query.

ROOT function zero receives the feature code in big-endian order; reply payload
byte zero is its runtime index. Index zero means unavailable. Runtime indices
must not be assumed to equal stable feature codes.

The decoder correlates device, feature, function and software ID. Error markers
`8f` (HID++ 1.0) and `fe` (HID++ 2.0) carry the original feature/address and error
code at bytes 3/4/5. Other replies and notifications are ignored by the current
probe, not misreported as success.

First-generation device IDs are `046d:c53e` (receiver) and `046d:b503` (Bluetooth).
Slot 1 was initially reference-based and has since returned successful Bluetooth
feature queries on the user's unit (2026-09-22); USB remains unverified.
The CLI permits slots 1–6 and requires explicit selection of an interface.

## Not implemented or verified

- Top-button notifications and basic Bluetooth spotlight activation are verified;
  gestures, laser/magnifier integration and broader presentation scenarios remain unverified.
- Vibration and remapping. Battery status is read by `inspect`, but not yet shown in the GUI.
- Hotplug, sleep/resume, firmware differences and coexistence with Options+.
- Spotlight 2 discovery through a shared Bolt receiver or Bluetooth.
- Full-screen/multi-display acceptance of the implemented device integration.

## Native app implementation (0.2.0)

The native Rust implementation uses objc2 0.6.4 and Apple framework bindings
0.3.2. NSPanel overlays cannot become key/main windows and ignore mouse input.
Screen coordinates stay in AppKit logical points; capture resolution uses each
screen backing scale. ScreenCaptureKit excludes this process by application
identity, with a generation counter to reject late callbacks during switching.
Capture stops when the effect is hidden. The capture output queue is main, and
non-main completion callbacks schedule their state updates onto main.

Carbon RegisterEventHotKey registers exact shortcuts, without a global event tap.
Declarations were checked against the local macOS SDK CarbonEvents headers.
Rendering fixtures use the real NSView drawing with a synthetic source; they do
not imply successful live ScreenCaptureKit acceptance.

The app now includes overlay rendering, a control panel, a permissions fallback,
and an ad-hoc signed Apple Silicon bundle. See VALIDATION.md for the current
manual-test limitations.

Capture logs must be labelled with model, OS, transport, interface usage and
action sequence before they become regression fixtures. Synthetic protocol
tests in this repository are not presented as captured device traffic.

## 2026-09-22 live diagnostic limitation

The first Bluetooth capture returned zero reports. System diagnostics showed
the console locked and Secure Input active, with the session's secure-input PID
pointing to loginwindow. That PID alone does not prove the actual owner. IORegistry showed
no seized client for this device; running Options+, BetterMouse and Karabiner
processes alone do not establish an ownership conflict. Apple's
[IOHIDLibUserClient implementation](https://github.com/apple-oss-distributions/IOHIDFamily/blob/main/IOHIDFamily/IOHIDLibUserClient.cpp)
checks secure-console access for keyboard devices; this Spotlight's primary
usage is keyboard even though it also exposes mouse and HID++ collections.

The CLI now checks the public Carbon `IsSecureEventInputEnabled` API before
opening diagnostic interfaces and while reading. It reports the blocked state
instead of starting a misleading empty capture. It never disables Secure Input.
After the user rebooted, Secure Input became inactive and all seven feature
queries completed successfully. Later software isolation yielded a RightArrow
press/release pair and ordinary mouse movement; see VALIDATION.md.

## Temporary top-button notification diagnostic (2026-09-22)

`watch-top` dynamically finds 0x1b04 and checks that CID 0x00d8 supports diversion.
It refuses preexisting diversion/persistent/raw-XY flags. Function 3 sends
`00 d8 03 00 00` to enable temporary diversion; cleanup sends
`00 d8 02 00 00`. Only DVALID is set; other flags and mappings remain unchanged.
Function 2 readback must match the expected full reporting state after each write.
Restoration uses a different software ID to reject delayed enable replies.

The tested device reports feature version 3 and returns an all-zero payload
for function 3, rather than the v2 specification's echo. Both forms are accepted
only with subsequent readback verification. The initial strict-echo attempt
failed its check, issued restoration, and an independent query confirmed zero
flags. Its [raw replies](qa/bluetooth-top-empty-config-ack.hex) are configuration
traffic, not user input. [Successful restoration tests](qa/bluetooth-top-restoration.json)
cover timeout, SIGINT, SIGTERM and SIGHUP on the Bluetooth device.

Event 0 contains four big-endian active CIDs. The decoder requires matching
device/feature and software ID zero; CID 0x00d8 means held. An explicit snapshot
without it means released. Mouse silence never implies release. Event 1 is raw
motion, not button state; this diagnostic leaves raw-XY diversion disabled.

Cleanup also runs on capture errors and Rust unwinding. SIGKILL, abort, power
loss or unavailable device access can prevent restoration. Temporary diversion
resets on HID++ configuration reset, not necessarily on application exit or
disconnection. No configuration-reset, persistent-setting or firmware request
is sent. `recover-top-default` is an explicit recovery command, never invoked
automatically. It requires a documented default baseline and rejects unrelated
reporting flags or a nondefault mapping before clearing temporary diversion.

## Bluetooth notifications and native integration (2026-09-22)

The [captured hold/release pair](qa/bluetooth-top-diverted-hold.hex) uses address
`ff`, whereas matched query replies on the same Bluetooth device use `01`.
The earlier diagnostic captured these events but rejected them as unrelated;
it was explicitly stopped by SIGTERM and restored its settings. The corrected
decoder takes a transport-specific notification address. Bluetooth uses `ff`;
USB retains the selected receiver slot (hardware verification pending).
It still requires the dynamic feature index and software ID zero. Native
mouse report 2 remains active without enabling raw-XY diversion.

The GUI now leases only top-hold diversion. Device reads and bounded HID++
transactions run on a worker; only button/state changes are queued to AppKit.
Each session has an epoch, so callbacks from a stopped session cannot reactivate
the overlay. Cleanup is joined on explicit disconnect, normal quit and ordinary
termination signals. A main-thread owner keeps the callback target alive during
join, avoiding synchronous main-queue destruction from the worker.

hidapi 2.6.7 initializes a process-global IOHIDManager on the first calling run
loop and never deinitializes it. Creating it on an expendable read worker caused
a real reconnect crash in CFRunLoopAddSource. Initialization/enumeration now run
on AppKit's enduring main loop; each worker receives copied device metadata and
opens only the selected Spotlight. Same-process UI reconnect was then verified.
Physical hotplug and sleep/resume remain untested; reconnect is currently manual.

Presentation tracks remote hold separately from manual/preview activation.
Motion silence does not expire it. Immediate hide suppresses that hold until
release; release or a read failure clears only the remote activation. AppKit
continues to use the system pointer for overlay placement.
