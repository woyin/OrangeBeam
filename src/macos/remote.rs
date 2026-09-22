//! Enumeration stays on AppKit's enduring main run loop; device I/O runs in a
//! worker. Only semantic state changes reach the UI. macOS handles cursor motion.
use hidapi::{BusType, DeviceInfo, HidApi};
use objc2::MainThreadMarker;
use spotlight_rs::controls::{notification_address, top_button_state, TemporaryTopButton};
use spotlight_rs::Report;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};

pub enum Event {
    Connected(&'static str),
    Hold(bool),
    Stopped(std::result::Result<(), String>),
}

pub struct Remote {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<std::result::Result<(), String>>>,
}

impl Remote {
    pub fn start(
        _mtm: MainThreadMarker,
        notify: impl Fn(Event) + Send + 'static,
    ) -> crate::Result<Self> {
        crate::ensure_input_available()?;
        // hidapi never deinitializes its process-global IOHIDManager. Its first
        // CFRunLoop must outlive every reconnect, so initialize/enumerate on main.
        // Initializing it on a disposable worker leaves a dangling run-loop target.
        let api = HidApi::new()?;
        api.set_open_exclusive(false);
        let interfaces = crate::interfaces(&api);
        let info = interfaces
            .iter()
            .copied()
            .filter(|d| d.usage_page() >= 0xff00)
            .min_by_key(|d| {
                if matches!(d.bus_type(), BusType::Bluetooth) {
                    0
                } else {
                    1
                }
            })
            .ok_or("未找到第一代 Spotlight；连接蓝牙或 USB 接收器后，点击连接遥控器。")?
            .clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("spotlight-hid".into())
            .spawn(move || {
                // The lease's Drop attempts restoration during unwinding. Report a
                // failure and clear UI hold even if transport code unexpectedly panics.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(api, info, &stopping, &notify).map_err(|e| e.to_string())
                }))
                .unwrap_or_else(|_| Err("遥控器读取线程异常退出；请检查恢复日志。".into()));
                notify(Event::Hold(false));
                notify(Event::Stopped(result.clone()));
                result
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }

    pub fn stop(&mut self) -> std::result::Result<(), String> {
        self.stop.store(true, Ordering::Relaxed);
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .unwrap_or_else(|_| Err("遥控器线程未正常退出。".into())),
            None => Ok(()),
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("REMOTE cleanup: {error}");
        }
    }
}

fn run(
    api: HidApi,
    info: DeviceInfo,
    stop: &AtomicBool,
    notify: &impl Fn(Event),
) -> crate::Result<()> {
    crate::ensure_input_available()?;
    let bluetooth = matches!(info.bus_type(), BusType::Bluetooth);
    let device = info.open_device(&api)?;
    let index = 1;
    let feature =
        crate::resolve_feature(&device, index, 0x1b04)?.ok_or("遥控器未提供顶键控制功能。")?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let lease = TemporaryTopButton::start(|r| crate::transact(&device, r), index, feature)?;
    let address = notification_address(bluetooth, index);
    eprintln!("REMOTE READY transport={} query={index:02x} notifications={address:02x} feature={feature:02x}", if bluetooth { "Bluetooth" } else { "USB" });
    notify(Event::Connected(if bluetooth { "蓝牙" } else { "USB" }));
    let captured: crate::Result<()> = (|| {
        let mut buffer = [0; 64];
        let mut held = false;
        while !stop.load(Ordering::Relaxed) {
            crate::ensure_input_available()?;
            let count = device.read_timeout(&mut buffer, 100)?;
            if count == 0 {
                continue;
            }
            crate::ensure_input_available()?;
            if let Some(down) = Report::parse(&buffer[..count])
                .ok()
                .and_then(|r| top_button_state(&r, address, feature))
            {
                if down != held {
                    held = down;
                    notify(Event::Hold(down));
                }
            }
        }
        Ok(())
    })();
    // Hide promptly on read/disconnection failure, before cleanup transactions.
    notify(Event::Hold(false));
    let restored = lease.finish();
    if restored.is_ok() {
        eprintln!("REMOTE RESTORED and verified top configuration");
    }
    match (captured, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(format!("{error}；顶键原值已恢复并校验。请重新连接。").into()),
        (Ok(()), Err(error)) => Err(format!("RESTORE FAILED: {error}").into()),
        (Err(error), Err(restore)) => Err(format!("{error}; RESTORE FAILED: {restore}").into()),
    }
}
