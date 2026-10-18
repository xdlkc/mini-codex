//! `mcodex app`: Native macOS desktop window (WebKit via `wry` + `tao`) connected to `appserver`
//! over an in-process bidirectional JSON-RPC channel, styled and structured 1:1 after `ChatGPT.app` (`com.openai.codex`).

use crate::appserver::{self, Shared};
use anyhow::Result;
use serde_json::Value;
#[cfg(target_os = "macos")]
use tao::platform::macos::WindowBuilderExtMacOS;
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use tokio::sync::mpsc::unbounded_channel;
use wry::WebViewBuilder;

enum UserEvent {
    ServerMsg(String),
    DragWindow,
}

const APP_HTML: &str = include_str!("gui_app.html");
#[cfg(target_os = "macos")]
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon_256.png");

#[cfg(target_os = "macos")]
fn set_macos_app_icon() {
    use std::ffi::c_void;
    use std::os::raw::c_char;
    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }
    unsafe {
        let msg_send_0: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let msg_send_1: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let msg_send_bytes: unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, usize) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());

        let ns_data_cls = objc_getClass(c"NSData".as_ptr());
        let sel_data = sel_registerName(c"dataWithBytes:length:".as_ptr());
        let data = msg_send_bytes(ns_data_cls, sel_data, APP_ICON_PNG.as_ptr(), APP_ICON_PNG.len());
        if data.is_null() {
            return;
        }
        let ns_image_cls = objc_getClass(c"NSImage".as_ptr());
        let sel_alloc = sel_registerName(c"alloc".as_ptr());
        let sel_init_data = sel_registerName(c"initWithData:".as_ptr());
        let img_alloc = msg_send_0(ns_image_cls, sel_alloc);
        let img = msg_send_1(img_alloc, sel_init_data, data);
        if img.is_null() {
            return;
        }
        let ns_app_cls = objc_getClass(c"NSApplication".as_ptr());
        let sel_shared = sel_registerName(c"sharedApplication".as_ptr());
        let sel_set_icon = sel_registerName(c"setApplicationIconImage:".as_ptr());
        let app = msg_send_0(ns_app_cls, sel_shared);
        if !app.is_null() {
            let _ = msg_send_1(app, sel_set_icon, img);
        }
    }
}

pub fn launch(shared: Shared) -> Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let ipc_proxy = event_loop.create_proxy();

    let (to_srv, from_ui) = unbounded_channel::<Value>();
    let (to_ui, mut from_srv) = unbounded_channel::<Value>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let p = proxy.clone();
            tokio::spawn(async move {
                while let Some(v) = from_srv.recv().await {
                    let _ = p.send_event(UserEvent::ServerMsg(v.to_string()));
                }
            });
            appserver::run_channel(shared, from_ui, to_ui).await;
        });
    });

    let mut wb = WindowBuilder::new()
        .with_title("Codex")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(760.0, 520.0));

    #[cfg(target_os = "macos")]
    {
        wb = wb
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true);
    }

    let window = wb.build(&event_loop)?;
    #[cfg(target_os = "macos")]
    set_macos_app_icon();

    let webview = WebViewBuilder::new()
        .with_html(APP_HTML)
        .with_ipc_handler(move |req| {
            if let Ok(v) = serde_json::from_str::<Value>(req.body()) {
                if v.get("__window").and_then(|w| w.as_str()) == Some("drag") {
                    let _ = ipc_proxy.send_event(UserEvent::DragWindow);
                    return;
                }
                let _ = to_srv.send(v);
            }
        })
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::DragWindow) => {
                let _ = window.drag_window();
            }
            Event::UserEvent(UserEvent::ServerMsg(json_line)) => {
                let script = format!("window.__onServerMessage && window.__onServerMessage({json_line});");
                let _ = webview.evaluate_script(&script);
            }
            _ => {}
        }
    });
}
