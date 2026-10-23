//! Public AppKit window actions for custom titlebar hit regions.
use std::{
    cell::RefCell,
    ffi::{c_char, c_void, CStr},
};
use tao::{platform::macos::WindowExtMacOS, window::Window};
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
struct Point {
    x: f64,
    y: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
struct Size {
    width: f64,
    height: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
struct Rect {
    origin: Point,
    size: Size,
}
#[derive(Debug, PartialEq)]
enum Action {
    Fill,
    Zoom,
    Minimize,
    None,
}
fn action(value: &str, legacy_minimize: bool) -> Action {
    match value {
        "Fill" => Action::Fill,
        "Minimize" => Action::Minimize,
        "None" => Action::None,
        "Maximize" | "Zoom" => Action::Zoom,
        _ if legacy_minimize => Action::Minimize,
        _ => Action::Zoom,
    }
}
fn same_rect(a: Rect, b: Rect) -> bool {
    [
        a.origin.x - b.origin.x,
        a.origin.y - b.origin.y,
        a.size.width - b.size.width,
        a.size.height - b.size.height,
    ]
    .iter()
    .all(|v| v.abs() < 0.5)
}
fn fill_target(current: Rect, visible: Rect, saved: &mut Option<Rect>) -> Rect {
    if same_rect(current, visible) {
        if let Some(frame) = saved.take() {
            return frame;
        }
    }
    *saved = Some(current);
    visible
}
#[derive(Default)]
pub(super) struct Actions {
    restore: RefCell<Option<Rect>>,
}
extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}
#[cfg(target_arch = "x86_64")]
extern "C" {
    fn objc_msgSend_stret();
}
unsafe fn object(receiver: *mut c_void, selector: &CStr) -> *mut c_void {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        std::mem::transmute(objc_msgSend as *const ());
    f(receiver, sel_registerName(selector.as_ptr()))
}
unsafe fn rect(receiver: *mut c_void, selector: &CStr) -> Rect {
    #[cfg(target_arch = "x86_64")]
    {
        let f: unsafe extern "C" fn(*mut Rect, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend_stret as *const ());
        let mut out = std::mem::MaybeUninit::<Rect>::uninit();
        f(
            out.as_mut_ptr(),
            receiver,
            sel_registerName(selector.as_ptr()),
        );
        out.assume_init()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> Rect =
            std::mem::transmute(objc_msgSend as *const ());
        f(receiver, sel_registerName(selector.as_ptr()))
    }
}
unsafe fn send_action(window: *mut c_void, selector: &CStr) {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
        std::mem::transmute(objc_msgSend as *const ());
    f(
        window,
        sel_registerName(selector.as_ptr()),
        std::ptr::null_mut(),
    );
}
unsafe fn preference() -> Action {
    let string: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> *mut c_void =
        std::mem::transmute(objc_msgSend as *const ());
    let defaults = object(
        objc_getClass(c"NSUserDefaults".as_ptr()),
        c"standardUserDefaults",
    );
    let key = string(
        objc_getClass(c"NSString".as_ptr()),
        sel_registerName(c"stringWithUTF8String:".as_ptr()),
        c"AppleActionOnDoubleClick".as_ptr(),
    );
    let read: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
        std::mem::transmute(objc_msgSend as *const ());
    let value = read(defaults, sel_registerName(c"stringForKey:".as_ptr()), key);
    if !value.is_null() {
        let value = object(value, c"UTF8String") as *const c_char;
        if !value.is_null() {
            return action(&CStr::from_ptr(value).to_string_lossy(), false);
        }
    }
    let key = string(
        objc_getClass(c"NSString".as_ptr()),
        sel_registerName(c"stringWithUTF8String:".as_ptr()),
        c"AppleMiniaturizeOnDoubleClick".as_ptr(),
    );
    let read: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool =
        std::mem::transmute(objc_msgSend as *const ());
    action(
        "",
        read(defaults, sel_registerName(c"boolForKey:".as_ptr()), key),
    )
}
impl Actions {
    pub(super) fn double_click(&self, window: &Window) {
        if window.fullscreen().is_some() {
            return;
        }
        unsafe {
            let native = window.ns_window();
            match preference() {
                Action::None => {}
                Action::Minimize => send_action(native, c"performMiniaturize:"),
                Action::Zoom => send_action(native, c"performZoom:"),
                Action::Fill => {
                    let screen = object(native, c"screen");
                    if screen.is_null() {
                        send_action(native, c"performZoom:");
                        return;
                    }
                    let visible = rect(screen, c"visibleFrame");
                    if visible.size.width <= 0.0 || visible.size.height <= 0.0 {
                        return;
                    }
                    let current = rect(native, c"frame");
                    if same_rect(current, visible) && self.restore.borrow().is_none() {
                        send_action(native, c"performZoom:");
                        return;
                    }
                    let target = fill_target(current, visible, &mut self.restore.borrow_mut());
                    let set: unsafe extern "C" fn(*mut c_void, *mut c_void, Rect, bool, bool) =
                        std::mem::transmute(objc_msgSend as *const ());
                    set(
                        native,
                        sel_registerName(c"setFrame:display:animate:".as_ptr()),
                        target,
                        true,
                        true,
                    );
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect {
            origin: Point { x, y },
            size: Size {
                width: w,
                height: h,
            },
        }
    }
    #[test]
    fn follows_double_click_preferences() {
        assert_eq!(action("Fill", false), Action::Fill);
        assert_eq!(action("Minimize", false), Action::Minimize);
        assert_eq!(action("None", false), Action::None);
        assert_eq!(action("Maximize", false), Action::Zoom);
        assert_eq!(action("", true), Action::Minimize);
        assert_eq!(action("", false), Action::Zoom);
    }
    #[test]
    fn fill_restores_previous_frame_and_replaces_it_after_manual_resize() {
        let original = r(80., 90., 900., 600.);
        let visible = r(0., 40., 1440., 860.);
        let mut saved = None;
        assert_eq!(fill_target(original, visible, &mut saved), visible);
        assert_eq!(fill_target(visible, visible, &mut saved), original);
        let resized = r(120., 100., 800., 550.);
        fill_target(original, visible, &mut saved);
        assert_eq!(fill_target(resized, visible, &mut saved), visible);
        assert_eq!(fill_target(visible, visible, &mut saved), resized);
    }
}
