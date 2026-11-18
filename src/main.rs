mod diagnostics;
use hidapi::{DeviceInfo, HidApi, HidDevice};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos;
#[cfg(unix)]
mod stop_signals;
use orange_beam::controls::notification_address;
use orange_beam::presentation::Effect;
use orange_beam::{feature_index, feature_request, is_spotlight, Report, ResponseKind};
use std::{
    error::Error,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HELP: &str = "orange-beam — Orange Beam (橙现), a macOS companion for the Logitech Spotlight

USAGE:
  orange-beam app
  orange-beam demo [spotlight|laser] [SECONDS]
  orange-beam render-fixtures DIRECTORY
  orange-beam list
  orange-beam probe DEVICE [INDEX]
  orange-beam inspect DEVICE [INDEX]
  orange-beam capture DEVICE [SECONDS]
  orange-beam watch-top DEVICE [SECONDS] [INDEX]
  orange-beam watch-controls DEVICE [SECONDS] [INDEX]
  orange-beam watch-page-keys DEVICE [SECONDS] [INDEX]
  orange-beam vibrate DEVICE [INTENSITY] [LENGTH] [INDEX]
  orange-beam recover-top-default DEVICE [INDEX]
  orange-beam recover-controls-default DEVICE [INDEX]

DEVICE is an interface number from 'list', not a receiver slot.
INDEX is the HID++ receiver slot (default 1, allowed 1..6).
SECONDS defaults to 15 (allowed 1..300).

list: enumerate only, without opening devices.
probe: query feature availability; does not configure or remap the device.
inspect: read battery and control mappings; does not change device settings.
capture: read raw input from one Spotlight interface; does not write reports.
watch-top: temporarily enable top-hold notifications, then restore and verify.
It ends after one press/release cycle, a signal, or SECONDS (default 30).
Raw XY, persistent settings, mappings and other controls are not modified.
watch-controls: temporarily divert top hold (00d8), double click (00df), next
hold (00da) and back hold (00dc); log decoded events for SECONDS (default 60),
then restore and verify each control. Short presses (00d9/00db) stay native.
watch-page-keys: temporarily divert only the short page keys (00d9/00db), log
decoded press/release events and any keyboard report (ID 01) for SECONDS
(default 60), then restore and verify. A KEYBOARD line means the device still
sends arrow keys, so a slide would also turn. Nothing else is diverted.
TAKEOVER ms= and RELEASE ms= report how long each side took; a long press
followed at once by a short press may still turn one slide above ~300 ms.
vibrate: send one PRESENTER_CONTROL vibration (INTENSITY 0..255, default 128;
LENGTH 0..10, default 0). No device settings are changed.
recover-top-default: recover an interrupted session only when its saved original
top flags were 0000, mapping 00d8, and other device-control apps are stopped.
On macOS, unlock the desktop and leave password fields before diagnostics.
recover-controls-default: the same explicit recovery for 00d8, 00df, 00da,
00dc, 00d9 and 00db, each accepted only if its documented original was 0000
mapped to itself.
Secure Input blocks diagnostics; capture stops if it becomes active.
Interface numbers can change after reconnecting. Run list again first.
For probe choose a vendor usage page (usually ff00); for capture also try
the mouse/keyboard interface. Spotlight 2 is not supported in this prototype.
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    App,
    RenderQa {
        directory: std::path::PathBuf,
    },
    Demo {
        effect: Effect,
        seconds: u64,
    },
    Help,
    List,
    Probe {
        device: usize,
        index: u8,
    },
    Inspect {
        device: usize,
        index: u8,
    },
    RecoverTop {
        device: usize,
        index: u8,
    },
    RecoverControls {
        device: usize,
        index: u8,
    },
    Capture {
        device: usize,
        seconds: u64,
    },
    WatchTop {
        device: usize,
        seconds: u64,
        index: u8,
    },
    WatchControls {
        device: usize,
        seconds: u64,
        index: u8,
    },
    WatchPageKeys {
        device: usize,
        seconds: u64,
        index: u8,
    },
    Vibrate {
        device: usize,
        intensity: u8,
        length: u8,
        index: u8,
    },
}

impl Command {
    /// Interface number from `list` that the command operates on. Read access
    /// needs the Input Monitoring permission and breaks under Secure Input.
    fn device(&self) -> Option<usize> {
        match self {
            Command::Probe { device, .. }
            | Command::Inspect { device, .. }
            | Command::Capture { device, .. }
            | Command::WatchTop { device, .. }
            | Command::WatchControls { device, .. }
            | Command::WatchPageKeys { device, .. }
            | Command::Vibrate { device, .. }
            | Command::RecoverTop { device, .. }
            | Command::RecoverControls { device, .. } => Some(*device),
            Command::App
            | Command::RenderQa { .. }
            | Command::Demo { .. }
            | Command::Help
            | Command::List => None,
        }
    }

    /// Commands that call the device's HID++ feature API instead of leaving
    /// navigation to the system, so they need the vendor-defined interface.
    fn needs_vendor_interface(&self) -> bool {
        match self {
            Command::Probe { .. }
            | Command::Inspect { .. }
            | Command::WatchTop { .. }
            | Command::WatchControls { .. }
            | Command::WatchPageKeys { .. }
            | Command::Vibrate { .. }
            | Command::RecoverTop { .. }
            | Command::RecoverControls { .. } => true,
            Command::App
            | Command::RenderQa { .. }
            | Command::Demo { .. }
            | Command::Help
            | Command::List
            | Command::Capture { .. } => false,
        }
    }
}

fn parse_args(args: &[String]) -> Result<Command> {
    match args.first().map(String::as_str) {
        None => Ok(Command::App),
        Some("app") if args.len() == 1 => Ok(Command::App),
        Some("render-fixtures") if args.len() == 2 => Ok(Command::RenderQa {
            directory: args[1].clone().into(),
        }),
        Some("demo") if args.len() <= 3 => {
            let effect = match args.get(1).map(String::as_str).unwrap_or("spotlight") {
                "spotlight" => Effect::Spotlight,
                "laser" => Effect::Laser,
                _ => return Err("Demo effect must be spotlight or laser".into()),
            };
            let seconds = args
                .get(2)
                .map(|s| s.parse::<u64>())
                .transpose()?
                .unwrap_or(10);
            if !(1..=300).contains(&seconds) {
                return Err("SECONDS must be 1..300".into());
            }
            Ok(Command::Demo { effect, seconds })
        }
        Some("--help" | "-h" | "help") if args.len() == 1 => Ok(Command::Help),
        Some("list") if args.len() == 1 => Ok(Command::List),
        Some("probe" | "inspect" | "recover-top-default" | "recover-controls-default")
            if (2..=3).contains(&args.len()) =>
        {
            let device = args[1].parse()?;
            let index = args
                .get(2)
                .map(|s| s.parse::<u8>())
                .transpose()?
                .unwrap_or(1);
            if !(1..=6).contains(&index) {
                return Err("INDEX must be 1..6".into());
            }
            Ok(if args[0] == "recover-top-default" {
                Command::RecoverTop { device, index }
            } else if args[0] == "recover-controls-default" {
                Command::RecoverControls { device, index }
            } else if args[0] == "inspect" {
                Command::Inspect { device, index }
            } else {
                Command::Probe { device, index }
            })
        }
        Some("vibrate") if (2..=5).contains(&args.len()) => {
            let device = args[1].parse()?;
            let intensity = args
                .get(2)
                .map(|s| s.parse::<u8>())
                .transpose()?
                .unwrap_or(128);
            let length = args
                .get(3)
                .map(|s| s.parse::<u8>())
                .transpose()?
                .unwrap_or(0);
            let index = args
                .get(4)
                .map(|s| s.parse::<u8>())
                .transpose()?
                .unwrap_or(1);
            if length > 10 {
                return Err("LENGTH must be 0..10".into());
            }
            if !(1..=6).contains(&index) {
                return Err("INDEX must be 1..6".into());
            }
            Ok(Command::Vibrate {
                device,
                intensity,
                length,
                index,
            })
        }
        Some("capture" | "watch-top" | "watch-controls" | "watch-page-keys")
            if (2..=if args[0] == "capture" { 3 } else { 4 }).contains(&args.len()) =>
        {
            let device = args[1].parse()?;
            let seconds = args
                .get(2)
                .map(|s| s.parse::<u64>())
                .transpose()?
                .unwrap_or(match args[0].as_str() {
                    "watch-top" => 30,
                    "watch-controls" | "watch-page-keys" => 60,
                    _ => 15,
                });
            if !(1..=300).contains(&seconds) {
                return Err("SECONDS must be 1..300".into());
            }
            if args[0] != "capture" {
                let index = args
                    .get(3)
                    .map(|s| s.parse::<u8>())
                    .transpose()?
                    .unwrap_or(1);
                if !(1..=6).contains(&index) {
                    return Err("INDEX must be 1..6".into());
                }
                let command = match args[0].as_str() {
                    "watch-top" => Command::WatchTop {
                        device,
                        seconds,
                        index,
                    },
                    "watch-controls" => Command::WatchControls {
                        device,
                        seconds,
                        index,
                    },
                    _ => Command::WatchPageKeys {
                        device,
                        seconds,
                        index,
                    },
                };
                Ok(command)
            } else {
                Ok(Command::Capture { device, seconds })
            }
        }
        _ => Err("Invalid arguments. Run orange-beam --help".into()),
    }
}

fn interfaces(api: &HidApi) -> Vec<&DeviceInfo> {
    let mut devices: Vec<_> = api
        .device_list()
        .filter(|d| is_spotlight(d.vendor_id(), d.product_id()))
        .collect();
    devices.sort_by(|a, b| {
        a.path()
            .to_bytes()
            .cmp(b.path().to_bytes())
            .then(a.usage_page().cmp(&b.usage_page()))
            .then(a.usage().cmp(&b.usage()))
    });
    devices
}

fn ensure_input_available() -> Result<()> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if macos::input_access::secure_input_enabled() {
        return Err("macOS Secure Input is active; Spotlight diagnostics cannot read reports. Unlock this Mac and leave password fields, then retry. Input Monitoring permission alone does not resolve this state.".into());
    }
    Ok(())
}

fn transact(device: &HidDevice, request: &Report) -> Result<Report> {
    transact_observing(device, request, |_| {})
}

/// Like transact, but hands every other HID++ report read while waiting to
/// `other`, so button notifications are not lost during a live session.
fn transact_observing(
    device: &HidDevice,
    request: &Report,
    mut other: impl FnMut(&Report),
) -> Result<Report> {
    ensure_input_available()?;
    // A single request is outstanding. A hard deadline still applies if input is busy.
    let deadline = Instant::now() + Duration::from_millis(1200);
    let written = device.write(request.bytes())?;
    if written != request.bytes().len() {
        return Err("Incomplete HID write".into());
    }
    let mut buffer = [0; 64];
    loop {
        ensure_input_available()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                "HID++ request timed out; wake the remote, check interface/slot and permissions"
                    .into(),
            );
        }
        let timeout = remaining.as_millis().clamp(1, 100) as i32;
        let count = device.read_timeout(&mut buffer, timeout)?;
        if count == 0 {
            continue;
        }
        let Ok(report) = Report::parse(&buffer[..count]) else {
            continue;
        };
        match report.response_to(request) {
            ResponseKind::Reply => return Ok(report),
            ResponseKind::Error { legacy, code } => {
                return Err(format!(
                    "Device returned {} error 0x{code:02x}",
                    if legacy { "HID++ 1.0" } else { "HID++ 2.0" }
                )
                .into())
            }
            ResponseKind::Unrelated => other(&report),
        }
    }
}

fn resolve_feature(device: &HidDevice, index: u8, code: u16) -> Result<Option<u8>> {
    let request = feature_request(index, code);
    Ok(feature_index(&transact(device, &request)?, &request)?)
}

fn run() -> Result<()> {
    let command = parse_args(&std::env::args().skip(1).collect::<Vec<_>>())?;
    if command == Command::Help {
        print!("{HELP}");
        return Ok(());
    }
    if matches!(command, Command::App | Command::Demo { .. }) {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return macos::run(match command {
            Command::Demo { effect, seconds } => Some((effect, seconds as f64)),
            _ => None,
        });
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err("The native app currently requires macOS Apple Silicon".into());
    }
    if let Command::RenderQa { directory } = &command {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return macos::render_qa::run(directory);
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err("Render fixtures require macOS Apple Silicon".into());
    }
    if command.device().is_some() {
        ensure_input_available()?;
    }
    let api = HidApi::new()?;
    // hidapi defaults to seizing devices unless macos-shared-device is enabled.
    // Keep normal slide navigation available while observing Spotlight reports.
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);
    let devices = interfaces(&api);
    if command == Command::List {
        diagnostics::list(&devices);
        return Ok(());
    }
    let number = command
        .device()
        .expect("commands without a device returned above");
    let info = devices
        .get(number)
        .ok_or("No such Spotlight interface. Connect the device and run list again.")?;
    if command.needs_vendor_interface() && info.usage_page() < 0xff00 {
        return Err(
            "probe/inspect/watch-top/watch-controls/watch-page-keys/vibrate requires a vendor-defined HID interface; choose its DEVICE number from list"
                .into(),
        );
    }
    let device = info.open_device(&api).map_err(|e| format!("Cannot open Spotlight: {e}. Check Input Monitoring permission on macOS or hidraw access on Linux."))?;
    let address =
        |index| notification_address(matches!(info.bus_type(), hidapi::BusType::Bluetooth), index);
    match command {
        Command::Probe { index, .. } => diagnostics::probe(&device, index),
        Command::Inspect { index, .. } => diagnostics::inspect(&device, index),
        Command::Capture { seconds, .. } => diagnostics::capture(&device, seconds),
        Command::WatchTop { seconds, index, .. } => {
            diagnostics::watch_top(&device, index, address(index), seconds)
        }
        Command::WatchControls { seconds, index, .. } => {
            diagnostics::watch_controls(&device, index, address(index), seconds)
        }
        Command::WatchPageKeys { seconds, index, .. } => {
            diagnostics::watch_page_keys(&device, index, address(index), seconds)
        }
        Command::Vibrate {
            intensity,
            length,
            index,
            ..
        } => diagnostics::vibrate(&device, index, intensity, length),
        Command::RecoverControls { index, .. } => diagnostics::recover_controls(&device, index),
        Command::RecoverTop { index, .. } => diagnostics::recover_top(&device, index),
        _ => unreachable!(),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("orange-beam: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Command> {
        parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn input_limits_prevent_accidental_unbounded_capture_or_wrong_slots() {
        for args in [
            vec!["capture", "0", "0"],
            vec!["capture", "0", "301"],
            vec!["probe", "0", "0"],
            vec!["probe", "0", "255"],
            vec!["inspect", "0", "0"],
            vec!["inspect", "0", "255"],
            vec!["inspect", "-1"],
            vec!["inspect", "0", "1", "extra"],
            vec!["list", "extra"],
            vec!["probe"],
            vec!["capture", "-1"],
            vec!["watch-top", "4", "0"],
            vec!["watch-top", "4", "301"],
            vec!["watch-top", "4", "30", "0"],
            vec!["watch-top", "4", "30", "7"],
            vec!["watch-top", "4", "30", "1", "extra"],
            vec!["watch-controls", "4", "0"],
            vec!["watch-controls", "4", "60", "7"],
            vec!["watch-page-keys", "4", "0"],
            vec!["watch-page-keys", "4", "301"],
            vec!["watch-page-keys", "4", "60", "7"],
            vec!["watch-page-keys", "4", "60", "1", "extra"],
            vec!["vibrate", "4", "256"],
            vec!["vibrate", "4", "128", "11"],
            vec!["vibrate", "4", "128", "0", "0"],
            vec!["unknown"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        assert_eq!(
            parse(&["probe", "2"]).unwrap(),
            Command::Probe {
                device: 2,
                index: 1
            }
        );
        assert_eq!(
            parse(&["capture", "1"]).unwrap(),
            Command::Capture {
                device: 1,
                seconds: 15
            }
        );
        assert_eq!(parse(&[]).unwrap(), Command::App);
        assert_eq!(
            parse(&["watch-top", "4"]).unwrap(),
            Command::WatchTop {
                device: 4,
                seconds: 30,
                index: 1
            }
        );
        assert_eq!(
            parse(&["watch-controls", "4"]).unwrap(),
            Command::WatchControls {
                device: 4,
                seconds: 60,
                index: 1
            }
        );
        assert_eq!(
            parse(&["watch-page-keys", "4"]).unwrap(),
            Command::WatchPageKeys {
                device: 4,
                seconds: 60,
                index: 1
            }
        );
        assert_eq!(
            parse(&["vibrate", "4"]).unwrap(),
            Command::Vibrate {
                device: 4,
                intensity: 128,
                length: 0,
                index: 1
            }
        );
        assert_eq!(
            parse(&["inspect", "4"]).unwrap(),
            Command::Inspect {
                device: 4,
                index: 1
            }
        );
    }
}
