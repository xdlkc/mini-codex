//! App-owned global shortcuts. Carbon registration does not read keyboard input.
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug)]
pub(super) enum ShortcutAction {
    Mini,
    Dictation,
    Capture,
}
static HANDLER: OnceLock<Box<dyn Fn(ShortcutAction) + Send + Sync>> = OnceLock::new();
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}
pub(super) fn request_screen_capture_access() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() }
}
#[repr(C)]
struct EventType {
    class: u32,
    kind: u32,
}
#[repr(C)]
#[derive(Default)]
struct HotkeyId {
    signature: u32,
    id: u32,
}
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
        count: u32,
        kinds: *const EventType,
        data: *mut c_void,
        result: *mut *mut c_void,
    ) -> i32;
    fn RemoveEventHandler(handler: *mut c_void) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        kind: u32,
        actual: *mut u32,
        size: u32,
        actual_size: *mut u32,
        value: *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        code: u32,
        modifiers: u32,
        id: HotkeyId,
        target: *mut c_void,
        options: u32,
        result: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(key: *mut c_void) -> i32;
}
unsafe extern "C" fn pressed(_: *mut c_void, event: *mut c_void, _: *mut c_void) -> i32 {
    let mut id = HotkeyId::default();
    if GetEventParameter(
        event,
        u32::from_be_bytes(*b"----"),
        u32::from_be_bytes(*b"hkid"),
        std::ptr::null_mut(),
        std::mem::size_of::<HotkeyId>() as u32,
        std::ptr::null_mut(),
        &mut id as *mut _ as *mut c_void,
    ) != 0
        || id.signature != u32::from_be_bytes(*b"MCDX")
    {
        return -9874;
    }
    let action = match id.id {
        1 => ShortcutAction::Mini,
        2 => ShortcutAction::Dictation,
        3 => ShortcutAction::Capture,
        _ => return -9874,
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(handler) = HANDLER.get() {
            handler(action);
        }
    }));
    0
}
pub(super) struct GlobalShortcuts {
    handler: *mut c_void,
    keys: Vec<*mut c_void>,
    pub status: Value,
}
impl GlobalShortcuts {
    pub fn install(handler: impl Fn(ShortcutAction) + Send + Sync + 'static) -> Self {
        let _ = HANDLER.set(Box::new(handler));
        let mut result = Self {
            handler: std::ptr::null_mut(),
            keys: Vec::new(),
            status: json!({}),
        };
        unsafe {
            let kind = EventType {
                class: u32::from_be_bytes(*b"keyb"),
                kind: 5,
            };
            let status = InstallEventHandler(
                GetApplicationEventTarget(),
                pressed,
                1,
                &kind,
                std::ptr::null_mut(),
                &mut result.handler,
            );
            if status != 0 {
                result.status =
                    json!({"error":format!("Global shortcuts could not start ({status})")});
                return result;
            }
        }
        let preferences = crate::desktop::handle("desktop/settings/read", &json!({}))
            .and_then(Result::ok)
            .unwrap_or_default();
        result.configure(&preferences["settings"]["preferences"]);
        result
    }
    pub fn configure(&mut self, preferences: &Value) {
        for key in self.keys.drain(..) {
            unsafe {
                UnregisterEventHotKey(key);
            }
        }
        let mut status = json!({});
        for (key, id, default) in [
            ("globalMiniShortcut", 1, "option-space"),
            ("globalDictationShortcut", 2, "control-option-space"),
            ("globalCaptureShortcut", 3, "option-shift-space"),
        ] {
            let requested = preferences[key].as_str().unwrap_or(default);
            let modifiers = match requested {
                "option-space" => 2048,
                "control-option-space" => 4096 | 2048,
                "command-shift-space" => 256 | 512,
                "option-shift-space" => 2048 | 512,
                "disabled" => {
                    status[key] = json!({"requested":requested,"registered":false,"disabled":true});
                    continue;
                }
                _ => {
                    status[key] = json!({"requested":requested,"registered":false,"error":"Unsupported shortcut"});
                    continue;
                }
            };
            let mut handle = std::ptr::null_mut();
            let result = unsafe {
                RegisterEventHotKey(
                    49,
                    modifiers,
                    HotkeyId {
                        signature: u32::from_be_bytes(*b"MCDX"),
                        id,
                    },
                    GetApplicationEventTarget(),
                    1,
                    &mut handle,
                )
            };
            if result == 0 {
                self.keys.push(handle);
                status[key] = json!({"requested":requested,"registered":true});
            } else {
                status[key] = json!({"requested":requested,"registered":false,"error":format!("Shortcut is unavailable or used by another app ({result}). Choose a different shortcut.")});
            }
        }
        self.status = status;
    }
}
impl Drop for GlobalShortcuts {
    fn drop(&mut self) {
        for key in self.keys.drain(..) {
            unsafe {
                UnregisterEventHotKey(key);
            }
        }
        if !self.handler.is_null() {
            unsafe {
                RemoveEventHandler(self.handler);
            }
        }
    }
}
