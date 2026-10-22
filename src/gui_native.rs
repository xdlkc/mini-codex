//! `mcodex app`: Native macOS desktop window (WebKit via `wry` + `tao`) connected to `appserver`
//! over an in-process bidirectional JSON-RPC channel, with Chat, Work and Codex workspaces.

use super::app_html;
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

#[cfg(target_os = "macos")]
#[path = "native_shell.rs"]
mod native_shell;

#[cfg(target_os = "macos")]
#[path = "native_integration.rs"]
mod native_integration;

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tao::{
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    window::{Window, WindowId},
};
use wry::WebView;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowKind {
    Main,
    Mini,
    Popout,
}
impl WindowKind {
    fn name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Mini => "mini",
            Self::Popout => "popout",
        }
    }
}
struct NativeWindow {
    webview: WebView,
    window: Window,
    kind: WindowKind,
    ready: std::cell::Cell<bool>,
    pending: std::cell::RefCell<Vec<(String, Value)>>,
}
enum UserEvent {
    ServerMsg(String),
    WindowCommand(WindowId, Value),
    DocumentTitle(WindowId, String),
    CaptureResult(WindowId, Result<Value, String>),
    RequestQuit,
    BackendStopped,
    #[cfg(target_os = "macos")]
    GlobalShortcut(native_integration::ShortcutAction),
    #[cfg(target_os = "macos")]
    NativeAction(native_shell::Action),
}

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
        let msg_send_bytes: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *const u8,
            usize,
        ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());

        let ns_data_cls = objc_getClass(c"NSData".as_ptr());
        let sel_data = sel_registerName(c"dataWithBytes:length:".as_ptr());
        let data = msg_send_bytes(
            ns_data_cls,
            sel_data,
            APP_ICON_PNG.as_ptr(),
            APP_ICON_PNG.len(),
        );
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

fn make_window(
    kind: WindowKind,
    thread_id: Option<&str>,
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: &EventLoopProxy<UserEvent>,
    to_server: &tokio::sync::mpsc::UnboundedSender<Value>,
    url: &str,
    status: &Value,
) -> Result<NativeWindow> {
    let (width, height) = match kind {
        WindowKind::Main => (1280.0, 820.0),
        WindowKind::Mini => (560.0, 440.0),
        WindowKind::Popout => (680.0, 760.0),
    };
    let mut builder = WindowBuilder::new()
        .with_title(match kind {
            WindowKind::Main => "Mini ChatGPT",
            WindowKind::Mini => "Mini — Quick Chat",
            WindowKind::Popout => "Mini ChatGPT — Popout",
        })
        .with_inner_size(LogicalSize::new(width, height))
        .with_min_inner_size(LogicalSize::new(
            if kind == WindowKind::Main {
                760.0
            } else {
                440.0
            },
            if kind == WindowKind::Main {
                520.0
            } else {
                340.0
            },
        ))
        .with_always_on_top(kind == WindowKind::Mini);
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
            .with_visible_on_all_workspaces(kind == WindowKind::Mini);
    }
    let window = builder.build(target)?;
    let id = window.id();
    let events = proxy.clone();
    let ipc_origin = url.to_owned();
    let server = to_server.clone();
    let document_url = url.to_owned();
    let ipc_token = format!("{:032x}", rand::random::<u128>());
    let initialization=format!("Object.defineProperty(window,'__mcodexNative',{{value:true}});window.__mcodexWindow={};window.__nativeHotkeys={};window.__nativeToken={};",serde_json::json!({"kind":kind.name(),"threadId":thread_id}),status,serde_json::to_string(&ipc_token).unwrap());
    let downloads = Arc::new(std::sync::Mutex::new(HashMap::<String, String>::new()));
    let started = downloads.clone();
    let download_proxy = proxy.clone();
    let title_proxy = proxy.clone();
    let webview=WebViewBuilder::new().with_initialization_script(&initialization).with_url(url)
        .with_document_title_changed_handler(move|title|{let _=title_proxy.send_event(UserEvent::DocumentTitle(id,title));})
        .with_navigation_handler(move|url|{if url==document_url||url.starts_with(&format!("{document_url}#")){return true;}open_external_link(&url);false})
        // Wry routes subframe navigation through this callback on macOS too.
        // Only local sandbox frames may load here. Trusted main-frame links use
        // an authenticated command instead of opening URLs from frame scripts.
        .with_new_window_req_handler(|url|matches!(url.as_str(),"about:blank"|"about:srcdoc"))
        .with_download_started_handler(move|url,suggested|{
            if !url.starts_with("blob:"){return false;}
            let Some(directory)=std::env::var_os("HOME").map(|home|std::path::PathBuf::from(home).join("Downloads"))else{return false;};
            if std::fs::create_dir_all(&directory).is_err(){return false;}
            let name=suggested.file_name().unwrap_or_else(||std::ffi::OsStr::new("document"));
            *suggested=directory.join(format!("{:08x}-{}",rand::random::<u64>(),name.to_string_lossy()));
            started.lock().unwrap().insert(url,suggested.display().to_string());true
        })
        .with_download_completed_handler(move|url,_,success|{let path=downloads.lock().unwrap().remove(&url);let notification=serde_json::json!({"method":"desktop/download/completed","params":{"success":success,"path":path}});let _=download_proxy.send_event(UserEvent::ServerMsg(notification.to_string()));})
        .with_ipc_handler(move|request|{let source=request.uri().to_string();if source!=ipc_origin&&!source.starts_with(&format!("{ipc_origin}#")){return;}if let Ok(mut value)=serde_json::from_str::<Value>(request.body()){if value["_nativeToken"].as_str()!=Some(ipc_token.as_str()){return;}if let Some(object)=value.as_object_mut(){object.remove("_nativeToken");}if value["__window"].is_string(){let _=events.send_event(UserEvent::WindowCommand(id,value));}else{let _=server.send(value);}}})
        .build(&window)?;
    Ok(NativeWindow {
        webview,
        window,
        kind,
        ready: std::cell::Cell::new(false),
        pending: std::cell::RefCell::new(Vec::new()),
    })
}
fn show_window(window: &NativeWindow) {
    #[cfg(target_os = "macos")]
    native_shell::activate_application();
    window.window.set_visible(true);
    window.window.set_minimized(false);
    window.window.set_focus();
    let _ = window
        .webview
        .evaluate_script("document.getElementById('composer-input')?.focus();");
}
fn invoke(window: &NativeWindow, action: &str, params: Value) {
    if !window.ready.get() {
        window
            .pending
            .borrow_mut()
            .push((action.to_owned(), params));
        return;
    }
    let script=format!("(async()=>{{try{{if(typeof nativeControlsReady!=='undefined')await nativeControlsReady;await window.__nativeInvoke?.({},{});}}catch(error){{if(typeof showError==='function')showError(error);}}}})();",serde_json::to_string(action).unwrap(),params);
    let _ = window.webview.evaluate_script(&script);
}
fn request_close(window: &NativeWindow) -> bool {
    window.webview.evaluate_script("(async()=>{window.__nativeCloseFocus=document.activeElement;document.body.inert=true;try{if(typeof nativeControlsReady!=='undefined')await nativeControlsReady;else if(typeof desktopReady!=='undefined')await desktopReady;if(typeof window.__requestClose!=='function')throw new Error('The window is still starting.');await window.__requestClose();}catch(error){document.body.inert=false;window.ipc.postMessage(JSON.stringify({__window:'closeCancelled',_nativeToken:window.__nativeToken}));if(typeof showError==='function')showError(error);}})();").is_ok()
}
fn capture(window: WindowId, proxy: EventLoopProxy<UserEvent>, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let result = (|| -> Result<Value, String> {
            #[cfg(not(target_os = "macos"))]
            {
                return Err("Screenshot capture is currently available on macOS".into());
            }
            #[cfg(target_os = "macos")]
            {
                use std::os::unix::fs::DirBuilderExt;
                let directory = std::env::temp_dir()
                    .join(format!("mcodex-capture-{:032x}", rand::random::<u128>()));
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .map_err(|_| "Could not prepare screenshot capture")?;
                let path = directory.join("capture.png");
                let captured = (|| -> Result<Value, String> {
                    let mut child = std::process::Command::new("/usr/sbin/screencapture")
                        .args(["-i", "-x", "-t", "png"])
                        .arg(&path)
                        .spawn()
                        .map_err(|_| "Could not start screenshot selection")?;
                    loop {
                        if cancel.load(Ordering::SeqCst) {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Ok(Value::Null);
                        }
                        match child.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                            Err(_) => {
                                let _ = child.kill();
                                let _ = child.wait();
                                return Err("Screenshot selection failed".into());
                            }
                        }
                    }
                    if !path.is_file() {
                        return Ok(Value::Null);
                    }
                    let bytes = std::fs::read(&path).map_err(|_| "Could not read screenshot")?;
                    crate::attachments::import_generated_image(
                        "Screenshot.png",
                        "image/png",
                        &bytes,
                    )
                })();
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_dir(&directory);
                captured
            }
        })();
        let _ = proxy.send_event(UserEvent::CaptureResult(window, result));
    });
}

pub fn launch(shared: Shared) -> Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (to_server, from_ui) = unbounded_channel::<Value>();
    let (to_ui, mut from_server) = unbounded_channel::<Value>();
    let messages = proxy.clone();
    let stopped = proxy.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let mut shutdown_tx = Some(shutdown_tx);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            tokio::spawn(async move {
                while let Some(value) = from_server.recv().await {
                    let _ = messages.send_event(UserEvent::ServerMsg(value.to_string()));
                }
            });
            appserver::run_channel_until(shared, from_ui, to_ui, async {
                let _ = shutdown_rx.await;
            })
            .await;
        });
        runtime.shutdown_timeout(std::time::Duration::from_secs(5));
        let _ = stopped.send_event(UserEvent::BackendStopped);
    });
    #[cfg(target_os = "macos")]
    let native_shell = {
        let menu = proxy.clone();
        let quit = proxy.clone();
        native_shell::NativeShell::install(
            move |action| {
                let _ = menu.send_event(UserEvent::NativeAction(action));
            },
            move || quit.send_event(UserEvent::RequestQuit).is_ok(),
        )?
    };
    #[cfg(target_os = "macos")]
    let mut shortcuts = {
        let events = proxy.clone();
        native_integration::GlobalShortcuts::install(move |action| {
            let _ = events.send_event(UserEvent::GlobalShortcut(action));
        })
    };
    #[cfg(target_os = "macos")]
    let initial_status = shortcuts.status.clone();
    #[cfg(not(target_os = "macos"))]
    let initial_status = Value::Null;
    let origin = crate::preview::native_origin::serve(app_html())?;
    let main = make_window(
        WindowKind::Main,
        None,
        &event_loop,
        &proxy,
        &to_server,
        origin.url(),
        &initial_status,
    )?;
    let main_id = main.window.id();
    #[cfg(target_os = "macos")]
    set_macos_app_icon();
    let mut windows = HashMap::from([(main_id, main)]);
    let mut active = main_id;
    let mut closing = HashSet::new();
    let mut quit_pending: Option<HashSet<WindowId>> = None;
    let mut quit_deferred = false;
    let mut cancelled = HashSet::new();
    let capture_cancel = Arc::new(AtomicBool::new(false));
    let mut capture_running = false;
    let mut handoffs = HashMap::<String, (WindowId, WindowId)>::new();
    event_loop.run(move|event,target,flow|{
        let _=&origin;
        #[cfg(target_os="macos")]
        let _=&native_shell;
        *flow=ControlFlow::Wait;
        let mut command:Option<(WindowId,Value)>=None;
        match event {
            Event::WindowEvent{window_id,event:WindowEvent::Focused(true),..}=>{active=window_id;}
            Event::WindowEvent{window_id,event:WindowEvent::CloseRequested,..}=>{command=Some((window_id,serde_json::json!({"__window":"requestClose"})));}
            Event::Reopen{..}=>{if let Some(window)=windows.get(&main_id){show_window(window);}active=main_id;}
            Event::UserEvent(UserEvent::WindowCommand(id,value))=>command=Some((id,value)),
            Event::UserEvent(UserEvent::DocumentTitle(id,title))=>{if let Some(window)=windows.get(&id){window.window.set_title(&title);}}
            Event::UserEvent(UserEvent::BackendStopped)=>{if quit_pending.is_some(){#[cfg(target_os="macos")]native_shell::reply_to_termination(true);*flow=ControlFlow::Exit;}},
            Event::UserEvent(UserEvent::RequestQuit)=>{command=Some((active,serde_json::json!({"__window":"quit"})));}
            #[cfg(target_os="macos")]
            Event::UserEvent(UserEvent::GlobalShortcut(action))=>{let action=match action{native_integration::ShortcutAction::Mini=>"mini",native_integration::ShortcutAction::Dictation=>"dictation",native_integration::ShortcutAction::Capture=>"capture"};command=Some((active,serde_json::json!({"__window":action})));}
            #[cfg(target_os="macos")]
            Event::UserEvent(UserEvent::NativeAction(action))=>{
                use native_shell::Action;
                let name=match action{Action::Close=>Some("requestClose"),Action::Quit=>Some("quit"),Action::Mini=>Some("mini"),Action::ShowMain=>Some("main"),_=>None};
                if let Some(name)=name{command=Some((active,serde_json::json!({"__window":name})));}
                else if let Some(window)=windows.get(&active).filter(|_|quit_pending.is_none()&&!closing.contains(&active)) {if let Some(script)=action.script(){show_window(window);let script=format!("(async()=>{{try{{if(typeof nativeControlsReady!=='undefined')await nativeControlsReady;{script}}}catch(error){{if(typeof showError==='function')showError(error);}}}})();");let _=window.webview.evaluate_script(&script);}}
            }
            Event::UserEvent(UserEvent::CaptureResult(id,result))=>{capture_running=false;if quit_pending.is_none(){if let Some(window)=windows.get(&id){show_window(window);match result{Ok(attachment) if !attachment.is_null()=>{invoke(window,"capture",serde_json::json!({"attachment":attachment}));},Err(error)=>{let _=window.webview.evaluate_script(&format!("showError({});",serde_json::to_string(&error).unwrap()));},_=>{}}}}}
            Event::UserEvent(UserEvent::ServerMsg(message))=>{for window in windows.values(){let _=window.webview.evaluate_script(&format!("window.__onServerMessage?.({message});"));}}
            Event::LoopDestroyed=>{capture_cancel.store(true,Ordering::SeqCst);}
            _=>{}
        }
        let Some((id,value))=command else{return;};let name=value["__window"].as_str().unwrap_or("");
        match name {
            "external"=>{if let Some(url)=value["url"].as_str(){open_external_link(url);}}
            "ready"=>{if let Some(window)=windows.get(&id){window.ready.set(true);let pending=std::mem::take(&mut *window.pending.borrow_mut());for (action,params) in pending{invoke(window,&action,params);}}}
            "drag"=>{if let Some(window)=windows.get(&id){let _=window.window.drag_window();}}
            "requestClose"=>{if quit_pending.is_none()&&closing.insert(id){if !windows.get(&id).is_some_and(request_close){closing.remove(&id);}}}
            "quit"=>{
                if quit_pending.is_some(){return;}if !closing.is_empty(){quit_deferred=true;return;}quit_deferred=false;
                let pending:HashSet<_>=windows.keys().copied().collect();quit_pending=Some(pending.clone());
                for id in pending{closing.insert(id);if !request_close(&windows[&id]){command_error(&windows[&id],"Could not save this window before quitting");quit_pending=None;closing.clear();for window in windows.values(){let _=window.webview.evaluate_script("document.body.inert=false;");}#[cfg(target_os="macos")]native_shell::reply_to_termination(false);break;}}
            }
            "close"|"closeCancelled"=>{
                closing.remove(&id);if cancelled.remove(&id){return;}
                if name=="closeCancelled"{quit_deferred=false;
                    if let Some(pending)=quit_pending.take(){cancelled.extend(pending.into_iter().filter(|other|*other!=id));}
                    for window in windows.values(){let _=window.webview.evaluate_script("document.body.inert=false;");}
                    if let Some(window)=windows.get(&id){show_window(window);let _=window.webview.evaluate_script("window.__nativeCloseFocus?.focus?.();");}
                    #[cfg(target_os="macos")]native_shell::reply_to_termination(false);
                }else if let Some(pending)=quit_pending.as_mut(){pending.remove(&id);if pending.is_empty(){capture_cancel.store(true,Ordering::SeqCst);if let Some(stop)=shutdown_tx.take(){let _=stop.send(());}}}
                else if let Some(window)=windows.get(&id){let _=window.webview.evaluate_script("document.body.inert=false;");window.window.set_visible(false);}
            }
            "print"=>{if let Some(window)=windows.get(&id).filter(|_|quit_pending.is_none()&&!closing.contains(&id)){if let Err(error)=window.webview.print(){let _=window.webview.evaluate_script(&format!("window.__documentPrintFailed?.({});",serde_json::to_string(&error.to_string()).unwrap()));}}}
            "mini"|"popout"|"dictation"=>{
                if quit_pending.is_some(){return;}
                let kind=if name=="popout"{WindowKind::Popout}else{WindowKind::Mini};
                let existing=windows.iter().find(|(_,window)|window.kind==kind).map(|(id,_)|*id);let thread=value["threadId"].as_str();
                let selected=if let Some(existing)=existing{existing}else{
                    #[cfg(target_os="macos")]let status=&shortcuts.status;
                    #[cfg(not(target_os="macos"))]let status=&Value::Null;
                    match make_window(kind,thread,target,&proxy,&to_server,origin.url(),status){Ok(window)=>{let id=window.window.id();windows.insert(id,window);id},Err(error)=>{if let Some(window)=windows.get(&id){command_error(window,&error.to_string());}return;}}
                };
                let window=&windows[&selected];
                if name=="mini"&&existing.is_some()&&window.window.is_visible()&&active==selected {if closing.insert(selected){request_close(window);}return;}
                active=selected;show_window(window);
                if existing.is_some()&&thread.is_some(){invoke(window,"thread",serde_json::json!({"threadId":thread}));}
                if name=="popout"&&value["draft"].is_object()&&value["ticket"].as_str().is_some_and(|ticket|ticket.len()<=128)&&handoffs.len()<20{handoffs.insert(value["ticket"].as_str().unwrap().to_owned(),(id,selected));invoke(window,"handoff",serde_json::json!({"ticket":value["ticket"],"threadId":value["threadId"],"draft":value["draft"]}));}
                else if name=="popout"&&thread.is_none()&&value["mode"].is_string(){invoke(window,"mode",serde_json::json!({"mode":value["mode"]}));}
                if name=="dictation"{invoke(window,"dictation",serde_json::json!({}));}
            }
            "main"=>{if let Some(window)=windows.get(&main_id){show_window(window);active=main_id;if value["draft"].is_object()&&value["ticket"].as_str().is_some_and(|ticket|ticket.len()<=128)&&handoffs.len()<20 {handoffs.insert(value["ticket"].as_str().unwrap().to_owned(),(id,main_id));invoke(window,"handoff",serde_json::json!({"threadId":value["threadId"],"ticket":value["ticket"],"draft":value["draft"]}));}else if value["threadId"].is_string(){invoke(window,"thread",serde_json::json!({"threadId":value["threadId"]}));}else if value["mode"].is_string(){invoke(window,"mode",serde_json::json!({"mode":value["mode"]}));}}}
            "handoffComplete"=>{if let Some(ticket)=value["ticket"].as_str(){if handoffs.get(ticket).is_some_and(|(_,target)|*target==id){if let Some((origin,_))=handoffs.remove(ticket){if let Some(window)=windows.get(&origin){invoke(window,"handoffComplete",serde_json::json!({"ticket":value["ticket"],"threadId":value["threadId"]}));}}}}}
            "alwaysOnTop"=>{if let Some(window)=windows.get(&id){let pinned=!window.window.is_always_on_top();window.window.set_always_on_top(pinned);let _=window.webview.evaluate_script(&format!("document.querySelector('[aria-label=\"Always on top\"]')?.setAttribute('aria-pressed','{pinned}');"));}}
            "capture"=>{if !capture_running&&quit_pending.is_none()&&!closing.contains(&id){
                #[cfg(target_os="macos")]
                if !native_integration::request_screen_capture_access(){if let Some(window)=windows.get(&id){command_error(window,"Allow screen recording for Mini ChatGPT in System Settings → Privacy & Security → Screen & System Audio Recording, then restart the app.");}return;}
                capture_running=true;capture_cancel.store(false,Ordering::SeqCst);if let Some(window)=windows.get(&id){invoke(window,"captureStarted",serde_json::json!({}));window.window.set_visible(false);}capture(id,proxy.clone(),capture_cancel.clone());}}
            #[cfg(target_os="macos")]
            "shortcuts"=>{let preferences=crate::desktop::handle("desktop/settings/read",&serde_json::json!({})).and_then(Result::ok).unwrap_or_default();shortcuts.configure(&preferences["settings"]["preferences"]);for window in windows.values(){let _=window.webview.evaluate_script(&format!("window.__nativeHotkeysUpdated?.({});",shortcuts.status));}}
            _=>{}
        }
        if quit_deferred&&closing.is_empty()&&quit_pending.is_none(){quit_deferred=false;let _=proxy.send_event(UserEvent::RequestQuit);}
    });
}
fn command_error(window: &NativeWindow, message: &str) {
    let _ = window.webview.evaluate_script(&format!(
        "if(typeof showError==='function')showError({});",
        serde_json::to_string(message).unwrap()
    ));
}

fn open_external_link(url: &str) {
    if ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
    {
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
        }
    }
}
