//! AppKit menus and termination integration for the single-window desktop shell.
//! Edit and Window actions remain native selectors handled by WebKit/AppKit.

use anyhow::{anyhow, Result};
use muda::{
    accelerator::{Accelerator, Code, Modifiers},
    AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem as Native, Submenu,
};
use std::ffi::{c_char, c_void};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    NewChat,
    NewDocument,
    PrintDocument,
    Settings,
    Chat,
    Work,
    Codex,
    Sidebar,
    Search,
    Files,
    Terminal,
    Review,
    Scheduled,
    Library,
    Shortcuts,
    Capabilities,
    Mini,
    Popout,
    ShowMain,
    Capture,
    Close,
    Quit,
}

impl Action {
    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "new-chat" => Self::NewChat,
            "new-document" => Self::NewDocument,
            "print-document" => Self::PrintDocument,
            "settings" => Self::Settings,
            "mode-chat" => Self::Chat,
            "mode-work" => Self::Work,
            "mode-codex" => Self::Codex,
            "sidebar" => Self::Sidebar,
            "search" => Self::Search,
            "files" => Self::Files,
            "terminal" => Self::Terminal,
            "review" => Self::Review,
            "scheduled" => Self::Scheduled,
            "library" => Self::Library,
            "shortcuts" => Self::Shortcuts,
            "capabilities" => Self::Capabilities,
            "mini" => Self::Mini,
            "popout" => Self::Popout,
            "show-main" => Self::ShowMain,
            "capture" => Self::Capture,
            "close" => Self::Close,
            "quit" => Self::Quit,
            _ => return None,
        })
    }
    pub(super) fn closes(self) -> bool {
        matches!(self, Self::Close | Self::Quit)
    }
    pub(super) fn script(self) -> Option<&'static str> {
        Some(match self {
            Self::NewChat => "await flushDocument();closeSettingsView();await newThread();",
            Self::NewDocument => "await switchMode('work');await createDocument();",
            Self::PrintDocument => "await docsPrint();",
            Self::Settings => "await openSettingsView('general');",
            Self::Chat => "await switchMode('chat');",
            Self::Work => "await switchMode('work');",
            Self::Codex => "await switchMode('codex');",
            Self::Sidebar => "toggleSidebar();",
            Self::Search => "await openSearch();",
            Self::Files => "await flushDocument();toggleDrawer('files');",
            Self::Terminal => "await flushDocument();toggleDrawer('terminal');",
            Self::Review => "await flushDocument();toggleDrawer('diff');",
            Self::Scheduled => "await openScheduledView();",
            Self::Library => {
                "if(desktop.mode!=='work')await switchMode('work');await showLibrary();"
            }
            Self::Shortcuts => "await openSettingsView('shortcuts');",
            Self::Capabilities => "await openSettingsView('capabilities');",
            Self::Popout => "await nativePopout();",
            Self::Capture => "nativeWindowRequest('capture');",
            Self::Close | Self::Quit | Self::Mini | Self::ShowMain => return None,
        })
    }
}

pub(super) struct NativeShell {
    _menu: Menu,
}

impl NativeShell {
    pub(super) fn install(
        handler: impl Fn(Action) + Send + Sync + 'static,
        quit: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        let menu = Menu::new();
        let command = |id: &str, label: &str, key: Option<Code>, shift: bool| {
            MenuItem::with_id(
                id,
                label,
                true,
                key.map(|key| {
                    Accelerator::new(
                        if shift {
                            Modifiers::META | Modifiers::SHIFT
                        } else {
                            Modifiers::META
                        },
                        key,
                    )
                }),
            )
        };
        let app = Submenu::with_items(
            "Mini ChatGPT",
            true,
            &[
                &Native::about(
                    Some("About Mini ChatGPT"),
                    Some(AboutMetadata {
                        name: Some("Mini ChatGPT".into()),
                        version: Some(env!("CARGO_PKG_VERSION").into()),
                        ..Default::default()
                    }),
                ),
                &Native::separator(),
                &command("settings", "Settings…", Some(Code::Comma), false),
                &Native::separator(),
                &Native::services(None),
                &Native::separator(),
                &Native::hide(Some("Hide Mini ChatGPT")),
                &Native::hide_others(None),
                &Native::show_all(None),
                &Native::separator(),
                &command("quit", "Quit Mini ChatGPT", Some(Code::KeyQ), false),
            ],
        )?;
        let file = Submenu::with_items(
            "File",
            true,
            &[
                &command("new-chat", "New Chat", Some(Code::KeyN), false),
                &command("new-document", "New Document", None, false),
                &command(
                    "print-document",
                    "Print / Save as PDF…",
                    Some(Code::KeyP),
                    false,
                ),
                &command("mini", "Show Mini", None, false),
                &command("popout", "Pop Out Chat", None, false),
                &command("capture", "Capture Screenshot…", None, false),
                &Native::separator(),
                &command("close", "Close Window", Some(Code::KeyW), false),
            ],
        )?;
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &Native::undo(None),
                &Native::redo(None),
                &Native::separator(),
                &Native::cut(None),
                &Native::copy(None),
                &Native::paste(None),
                &Native::paste_and_match_style(None),
                &Native::separator(),
                &Native::select_all(None),
                &Native::separator(),
                &Native::emoji_and_symbols(None),
            ],
        )?;
        let modes = Submenu::with_items(
            "Workspace",
            true,
            &[
                &command("mode-chat", "Chat", Some(Code::Digit1), false),
                &command("mode-work", "Work", Some(Code::Digit2), false),
                &command("mode-codex", "Codex", Some(Code::Digit3), false),
            ],
        )?;
        let view = Submenu::with_items(
            "View",
            true,
            &[
                &modes,
                &Native::separator(),
                &command("sidebar", "Toggle Sidebar", Some(Code::KeyS), false),
                &command("search", "Search…", Some(Code::KeyK), false),
                &Native::separator(),
                &command("library", "Library", None, false),
                &command("scheduled", "Scheduled", None, false),
                &Native::separator(),
                &command("files", "Files", Some(Code::KeyF), true),
                &command("terminal", "Terminal", Some(Code::KeyT), true),
                &command("review", "Review Changes", Some(Code::KeyD), true),
                &Native::separator(),
                &Native::fullscreen(None),
            ],
        )?;
        let window = Submenu::with_items(
            "Window",
            true,
            &[
                &Native::minimize(None),
                &command("show-main", "Show Main Window", None, false),
                &Native::zoom(None),
                &Native::separator(),
                &Native::bring_all_to_front(None),
            ],
        )?;
        let help = Submenu::with_items(
            "Help",
            true,
            &[
                &command("shortcuts", "Keyboard Shortcuts", None, false),
                &command("capabilities", "Features & Connections", None, false),
            ],
        )?;
        menu.append_items(&[&app, &file, &edit, &view, &window, &help])?;
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(action) = Action::from_id(event.id.0.as_str()) {
                handler(action);
            }
        }));
        install_termination_handler(quit)?;
        Ok(Self { _menu: menu })
    }
}

static QUIT_REQUEST: OnceLock<Box<dyn Fn() -> bool + Send + Sync>> = OnceLock::new();
static TERMINATION_PENDING: AtomicBool = AtomicBool::new(false);
static ORIGINAL_SEND_EVENT: OnceLock<usize> = OnceLock::new();

extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn object_getClass(object: *mut c_void) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn class_addMethod(
        class: *mut c_void,
        selector: *mut c_void,
        implementation: unsafe extern "C" fn(),
        types: *const c_char,
    ) -> i8;
    fn class_getMethodImplementation(class: *mut c_void, selector: *mut c_void) -> *mut c_void;
    fn class_replaceMethod(
        class: *mut c_void,
        selector: *mut c_void,
        implementation: unsafe extern "C" fn(),
        types: *const c_char,
    ) -> *mut c_void;
    fn objc_msgSend();
}

unsafe fn send(object: *mut c_void, selector: &std::ffi::CStr) -> *mut c_void {
    let send: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        std::mem::transmute(objc_msgSend as *const ());
    send(object, sel_registerName(selector.as_ptr()))
}

unsafe extern "C" fn should_terminate(
    _delegate: *mut c_void,
    _selector: *mut c_void,
    _application: *mut c_void,
) -> usize {
    // NSTerminateLater lets AppKit wait for the same durable save handshake as Cmd-W.
    if TERMINATION_PENDING.swap(true, Ordering::SeqCst) {
        return 2;
    }
    let queued = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        QUIT_REQUEST.get().is_some_and(|request| request())
    }))
    .unwrap_or(false);
    if queued {
        2
    } else {
        TERMINATION_PENDING.store(false, Ordering::SeqCst);
        0
    }
}

unsafe extern "C" fn send_event_guard(
    application: *mut c_void,
    selector: *mut c_void,
    event: *mut c_void,
) {
    if event.is_null() {
        return;
    }
    let integer: unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize =
        std::mem::transmute(objc_msgSend as *const ());
    let is_command_up = integer(event, sel_registerName(c"type".as_ptr())) == 11
        && integer(event, sel_registerName(c"modifierFlags".as_ptr())) & (1 << 20) != 0;
    let implementation = if is_command_up && send(application, c"keyWindow").is_null() {
        // Tao 0.31 forwards Command-keyUp to a nil keyWindow after Cmd-M. Its old
        // objc macro creates a null Rust reference. AppKit itself accepts this state.
        class_getMethodImplementation(objc_getClass(c"NSApplication".as_ptr()), selector) as usize
    } else {
        ORIGINAL_SEND_EVENT.get().copied().unwrap_or(0)
    };
    if implementation != 0 {
        let original: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(implementation);
        original(application, selector, event);
    }
}

unsafe fn install_event_guard(application: *mut c_void) -> Result<()> {
    let class = object_getClass(application);
    let selector = sel_registerName(c"sendEvent:".as_ptr());
    let original = class_getMethodImplementation(class, selector);
    if original.is_null() {
        return Err(anyhow!("AppKit event dispatcher is unavailable"));
    }
    ORIGINAL_SEND_EVENT
        .set(original as usize)
        .map_err(|_| anyhow!("Native event guard is already installed"))?;
    let implementation: unsafe extern "C" fn() = std::mem::transmute(send_event_guard as *const ());
    class_replaceMethod(class, selector, implementation, c"v@:@".as_ptr());
    Ok(())
}

fn install_termination_handler(quit: impl Fn() -> bool + Send + Sync + 'static) -> Result<()> {
    QUIT_REQUEST
        .set(Box::new(quit))
        .map_err(|_| anyhow!("Native quit handler is already installed"))?;
    unsafe {
        let application = send(
            objc_getClass(c"NSApplication".as_ptr()),
            c"sharedApplication",
        );
        let delegate = send(application, c"delegate");
        if application.is_null() || delegate.is_null() {
            return Err(anyhow!("AppKit application delegate is unavailable"));
        }
        install_event_guard(application)?;
        let implementation: unsafe extern "C" fn() =
            std::mem::transmute(should_terminate as *const ());
        if class_addMethod(
            object_getClass(delegate),
            sel_registerName(c"applicationShouldTerminate:".as_ptr()),
            implementation,
            c"Q@:@".as_ptr(),
        ) == 0
        {
            return Err(anyhow!(
                "Cannot install the native save-before-quit handler"
            ));
        }
        // A debug executable has no bundle display name. Keep its native app menu unambiguous too.
        let process = send(objc_getClass(c"NSProcessInfo".as_ptr()), c"processInfo");
        let string: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let name = string(
            objc_getClass(c"NSString".as_ptr()),
            sel_registerName(c"stringWithUTF8String:".as_ptr()),
            c"Mini ChatGPT".as_ptr(),
        );
        let set: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        set(process, sel_registerName(c"setProcessName:".as_ptr()), name);
    }
    Ok(())
}

pub(super) fn reply_to_termination(approved: bool) {
    if !TERMINATION_PENDING.swap(false, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let application = send(
            objc_getClass(c"NSApplication".as_ptr()),
            c"sharedApplication",
        );
        let reply: unsafe extern "C" fn(*mut c_void, *mut c_void, i8) =
            std::mem::transmute(objc_msgSend as *const ());
        reply(
            application,
            sel_registerName(c"replyToApplicationShouldTerminate:".as_ptr()),
            i8::from(approved),
        );
    }
}

pub(super) fn activate_application() {
    unsafe {
        let application = send(
            objc_getClass(c"NSApplication".as_ptr()),
            c"sharedApplication",
        );
        let action: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        action(
            application,
            sel_registerName(c"unhide:".as_ptr()),
            std::ptr::null_mut(),
        );
        let activate: unsafe extern "C" fn(*mut c_void, *mut c_void, i8) =
            std::mem::transmute(objc_msgSend as *const ());
        activate(
            application,
            sel_registerName(c"activateIgnoringOtherApps:".as_ptr()),
            1,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_and_quit_cannot_bypass_the_save_handshake() {
        for id in ["close", "quit"] {
            let action = Action::from_id(id).unwrap();
            assert!(action.closes());
            assert!(action.script().is_none());
        }
        for id in [
            "new-chat",
            "settings",
            "mode-chat",
            "mode-work",
            "mode-codex",
            "sidebar",
            "search",
            "files",
            "terminal",
            "review",
        ] {
            let action = Action::from_id(id).unwrap();
            assert!(!action.closes());
            assert!(action.script().is_some());
        }
        assert!(Action::from_id("untrusted-script").is_none());
    }
}
