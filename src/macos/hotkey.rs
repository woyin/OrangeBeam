//! Carbon hot keys register only our exact shortcuts; no global keyboard monitor.
use std::ffi::c_void;

#[repr(C)]
struct EventType {
    class: u32,
    kind: u32,
}
#[repr(C)]
#[derive(Default)]
struct HotKeyId {
    signature: u32,
    id: u32,
}
type Handle = *mut c_void;
type Handler = unsafe extern "C" fn(Handle, Handle, Handle) -> i32;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> Handle;
    fn InstallEventHandler(
        target: Handle,
        handler: Handler,
        count: u32,
        types: *const EventType,
        context: Handle,
        result: *mut Handle,
    ) -> i32;
    fn RemoveEventHandler(handler: Handle) -> i32;
    fn RegisterEventHotKey(
        code: u32,
        modifiers: u32,
        id: HotKeyId,
        target: Handle,
        options: u32,
        result: *mut Handle,
    ) -> i32;
    fn UnregisterEventHotKey(key: Handle) -> i32;
    fn GetEventParameter(
        event: Handle,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        size: u32,
        actual_size: *mut u32,
        data: Handle,
    ) -> i32;
}

struct Context {
    target: Handle,
    callback: unsafe fn(Handle, u32),
}
pub struct HotKeys {
    handler: Handle,
    keys: Vec<Handle>,
    _context: Box<Context>,
}

unsafe extern "C" fn handle(_call: Handle, event: Handle, context: Handle) -> i32 {
    let mut id = HotKeyId::default();
    // SAFETY: Carbon supplies an event and our live boxed Context. Parameter memory
    // is sized for the documented EventHotKeyID C struct; callbacks run on main.
    let result = unsafe {
        GetEventParameter(
            event,
            u32::from_be_bytes(*b"----"),
            u32::from_be_bytes(*b"hkid"),
            std::ptr::null_mut(),
            std::mem::size_of::<HotKeyId>() as u32,
            std::ptr::null_mut(),
            (&mut id as *mut HotKeyId).cast(),
        )
    };
    if result != 0 || id.signature != u32::from_be_bytes(*b"SpRS") {
        return -9874;
    }
    let context = unsafe { &*(context as *const Context) };
    unsafe {
        (context.callback)(context.target, id.id);
    }
    0
}

impl HotKeys {
    /// SAFETY: target must outlive this object and callback must accept it on main.
    pub unsafe fn new(target: Handle, callback: unsafe fn(Handle, u32)) -> Result<Self, String> {
        let context = Box::new(Context { target, callback });
        let mut result = Self {
            handler: std::ptr::null_mut(),
            keys: Vec::new(),
            _context: context,
        };
        let event_type = EventType {
            class: u32::from_be_bytes(*b"keyb"),
            kind: 6,
        };
        let event_target = unsafe { GetApplicationEventTarget() };
        let status = unsafe {
            InstallEventHandler(
                event_target,
                handle,
                1,
                &event_type,
                (&mut *result._context as *mut Context).cast(),
                &mut result.handler,
            )
        };
        if status != 0 {
            return Err(format!("快捷键初始化失败：{status}"));
        }
        // Only an emergency hide: ANSI H with Control+Option+Command, avoiding
        // macOS's standard Command+Option+H (Hide Others). The app takes no
        // other global shortcut, so presentation apps keep theirs (e.g. Keynote
        // uses Command+Option+P to play).
        let mut key = std::ptr::null_mut();
        let status = unsafe {
            RegisterEventHotKey(
                4,
                0x1900,
                HotKeyId {
                    signature: u32::from_be_bytes(*b"SpRS"),
                    id: 2,
                },
                event_target,
                0,
                &mut key,
            )
        };
        if status != 0 {
            return Err(format!(
                "快捷键被占用或无法注册：{status}。仍可使用菜单栏。"
            ));
        }
        result.keys.push(key);
        Ok(result)
    }
}

impl Drop for HotKeys {
    fn drop(&mut self) {
        // SAFETY: Handles are registered by this object and released exactly once,
        // before dropping the callback context or its target delegate.
        unsafe {
            for key in self.keys.drain(..) {
                UnregisterEventHotKey(key);
            }
            if !self.handler.is_null() {
                RemoveEventHandler(self.handler);
            }
        }
    }
}
