//! Host bridge for Fogbreak Light (`fogbreak-light`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use base64::Engine as Base64Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use lightcraft_engine::export::{ExportFormat, ExportOptions, export_photo};
use lightcraft_raster::Rgba8;
use lightcraft_ui_egui::{LightcraftApp, control};
use lightcraft_web::store::{Originals, content_hash};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

pub const ENGINE_ID: &str = "fogbreak-light";
pub const DEFAULT_HOST_KEY: &str = "fogbreakLightHost";
pub const CANVAS_ID: &str = "fogbreak_canvas";

struct PendingRpc {
    method: String,
    params: Value,
    resolve: js_sys::Function,
    reject: js_sys::Function,
}

struct WaitingControl {
    rx: Receiver<Value>,
    resolve: js_sys::Function,
    reject: js_sys::Function,
}

enum SyncAction {
    OpenBytes { name: String, bytes: Vec<u8> },
    SetTheme { dark: bool },
}

pub struct HostBridge {
    pub rpc_key: String,
    pub dark: bool,
    pub commands: Vec<Value>,
    originals: Originals,
    ctrl_tx: Sender<control::ControlRequest>,
    pending_rpcs: VecDeque<PendingRpc>,
    waiting_ctrl: Vec<WaitingControl>,
    pending_sync: VecDeque<SyncAction>,
    egui_ctx: Option<egui::Context>,
    no_arg_ret: Vec<Closure<dyn FnMut() -> JsValue>>,
    one_arg_ret: Vec<Closure<dyn FnMut(JsValue) -> JsValue>>,
    one_arg_promise: Vec<Closure<dyn FnMut(JsValue) -> js_sys::Promise>>,
    two_arg_ret: Vec<Closure<dyn FnMut(JsValue, JsValue) -> JsValue>>,
    two_arg_promise: Vec<Closure<dyn FnMut(JsValue, JsValue) -> js_sys::Promise>>,
}

pub type SharedBridge = Rc<RefCell<HostBridge>>;

pub fn new_shared(originals: Originals, ctrl_tx: Sender<control::ControlRequest>, dark: bool) -> SharedBridge {
    let rpc_key = detect_rpc_key();
    Rc::new(RefCell::new(HostBridge {
        rpc_key,
        dark,
        commands: Vec::new(),
        originals,
        ctrl_tx,
        pending_rpcs: VecDeque::new(),
        waiting_ctrl: Vec::new(),
        pending_sync: VecDeque::new(),
        egui_ctx: None,
        no_arg_ret: Vec::new(),
        one_arg_ret: Vec::new(),
        one_arg_promise: Vec::new(),
        two_arg_ret: Vec::new(),
        two_arg_promise: Vec::new(),
    }))
}

pub fn attach_ctx(bridge: &SharedBridge, ctx: &egui::Context, app: &LightcraftApp) {
    let mut b = bridge.borrow_mut();
    b.egui_ctx = Some(ctx.clone());
    refresh_commands_inner(&mut b, app);
}

pub fn refresh_commands(bridge: &SharedBridge, app: &LightcraftApp) {
    let mut b = bridge.borrow_mut();
    refresh_commands_inner(&mut b, app);
}

fn refresh_commands_inner(b: &mut HostBridge, app: &LightcraftApp) {
    if let Value::Array(items) = control::all_commands(app) {
        b.commands = items;
    }
}

pub fn detect_rpc_key() -> String {
    let fallback = DEFAULT_HOST_KEY.to_string();
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return fallback;
    };
    if let Some(el) = doc.get_element_by_id(CANVAS_ID)
        && let Some(key) = el.get_attribute("data-rpc-key")
    {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    fallback
}

pub fn install(bridge: &SharedBridge) -> Result<(), String> {
    let win = web_sys::window().ok_or_else(|| "window missing".to_string())?;
    let host_obj = js_sys::Object::new();

    set_prop(&host_obj, "engine", &JsValue::from_str(ENGINE_ID))?;
    set_prop(&host_obj, "rpcKey", &JsValue::from_str(&bridge.borrow().rpc_key))?;

    install_command_methods(bridge, &host_obj)?;
    install_async_methods(bridge, &host_obj)?;
    install_lifecycle_methods(bridge, &host_obj)?;
    install_theme_and_io(bridge, &host_obj)?;

    let key = bridge.borrow().rpc_key.clone();
    set_prop(&win, &key, &host_obj)?;
    Ok(())
}

fn install_command_methods(bridge: &SharedBridge, host_obj: &js_sys::Object) -> Result<(), String> {
    let b1 = Rc::clone(bridge);
    let commands_fn = Closure::wrap(Box::new(move || to_js(&Value::Array(b1.borrow().commands.clone()))) as Box<dyn FnMut() -> JsValue>);
    set_prop(host_obj, "commands", commands_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().no_arg_ret.push(commands_fn);

    let b2 = Rc::clone(bridge);
    let list_fn = Closure::wrap(Box::new(move |query: JsValue| {
        let q = query.as_string().unwrap_or_default();
        to_js(&filter_commands(&b2.borrow().commands, &q))
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set_prop(host_obj, "listCommands", list_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().one_arg_ret.push(list_fn);

    let b3 = Rc::clone(bridge);
    let desc_fn = Closure::wrap(Box::new(move |id: JsValue| {
        let wanted = id.as_string().unwrap_or_default();
        let val = find_command(&b3.borrow().commands, &wanted).unwrap_or(Value::Null);
        to_js(&val)
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set_prop(host_obj, "describeCommand", desc_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().one_arg_ret.push(desc_fn);
    Ok(())
}

fn install_async_methods(bridge: &SharedBridge, host_obj: &js_sys::Object) -> Result<(), String> {
    register_two_arg_promise(bridge, host_obj, "execute", |method_js, params_js| {
        let cmd = method_js.as_string().unwrap_or_default();
        let params = from_js(&params_js);
        ("engine.execute".to_string(), json!({ "command": cmd, "params": params }))
    })?;
    register_two_arg_promise(bridge, host_obj, "executeCommand", |method_js, params_js| {
        let cmd = method_js.as_string().unwrap_or_default();
        let params = from_js(&params_js);
        ("engine.execute".to_string(), json!({ "command": cmd, "params": params }))
    })?;
    register_two_arg_promise(bridge, host_obj, "request", |method_js, params_js| {
        let method = method_js.as_string().unwrap_or_default();
        let params = from_js(&params_js);
        (method, params)
    })?;

    for (js_name, rpc_name) in [
        ("docInspect", "document.inspect"),
        ("uiInspect", "ui.inspect"),
        ("inspect", "fogbreak.inspect"),
        ("screenshot", "fogbreak.screenshot"),
    ] {
        let b = Rc::clone(bridge);
        let rpc = rpc_name.to_string();
        let cb = Closure::wrap(Box::new(move |params_js: JsValue| enqueue_promise(&b, &rpc, from_js(&params_js))) as Box<dyn FnMut(JsValue) -> js_sys::Promise>);
        set_prop(host_obj, js_name, cb.as_ref().unchecked_ref())?;
        bridge.borrow_mut().one_arg_promise.push(cb);
    }
    Ok(())
}

fn install_lifecycle_methods(bridge: &SharedBridge, host_obj: &js_sys::Object) -> Result<(), String> {
    let b_open = Rc::clone(bridge);
    let open_fn = Closure::wrap(Box::new(move |name_js: JsValue, bytes_js: JsValue| {
        let name = name_js.as_string().unwrap_or_else(|| "photo.png".to_string());
        match decode_bytes(&bytes_js) {
            Ok(bytes) => {
                let len = bytes.len();
                let mut guard = b_open.borrow_mut();
                guard.pending_sync.push_back(SyncAction::OpenBytes { name: name.clone(), bytes });
                if let Some(ctx) = &guard.egui_ctx {
                    ctx.request_repaint();
                }
                drop(guard);
                emit_open(&name, len);
                to_js(&json!({ "ok": true, "name": name, "bytes": len }))
            }
            Err(err) => to_js(&json!({ "ok": false, "error": err })),
        }
    }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
    set_prop(host_obj, "open", open_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().two_arg_ret.push(open_fn);

    let handoff_fn = Closure::wrap(Box::new(move |target_js: JsValue, payload_js: JsValue| {
        let target = target_js.as_string().unwrap_or_default();
        let payload = from_js(&payload_js);
        emit_handoff(&target, &payload);
        to_js(&json!({ "ok": true, "target": target }))
    }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
    set_prop(host_obj, "emitHandoff", handoff_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().two_arg_ret.push(handoff_fn);

    let b_theme = Rc::clone(bridge);
    let theme_fn = Closure::wrap(Box::new(move |mode_js: JsValue| {
        let dark = set_theme_sync(&b_theme, mode_js.as_string().as_deref());
        to_js(&json!({ "theme": if dark { "dark" } else { "light" }, "dark": dark }))
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set_prop(host_obj, "setTheme", theme_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().one_arg_ret.push(theme_fn);
    Ok(())
}

fn install_theme_and_io(bridge: &SharedBridge, host_obj: &js_sys::Object) -> Result<(), String> {
    let theme_obj = js_sys::Object::new();
    let bt1 = Rc::clone(bridge);
    let theme_get = Closure::wrap(Box::new(move || {
        let dark = bt1.borrow().dark;
        to_js(&json!({ "theme": if dark { "dark" } else { "light" }, "dark": dark }))
    }) as Box<dyn FnMut() -> JsValue>);
    set_prop(&theme_obj, "get", theme_get.as_ref().unchecked_ref())?;
    bridge.borrow_mut().no_arg_ret.push(theme_get);

    let bt2 = Rc::clone(bridge);
    let theme_set = Closure::wrap(Box::new(move |mode_js: JsValue| {
        let dark = set_theme_sync(&bt2, mode_js.as_string().as_deref());
        to_js(&json!({ "theme": if dark { "dark" } else { "light" }, "dark": dark }))
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set_prop(&theme_obj, "set", theme_set.as_ref().unchecked_ref())?;
    bridge.borrow_mut().one_arg_ret.push(theme_set);
    set_prop(host_obj, "theme", &theme_obj)?;

    let io_obj = js_sys::Object::new();
    let formats_fn = Closure::wrap(Box::new(move || to_js(&io_formats_value())) as Box<dyn FnMut() -> JsValue>);
    set_prop(&io_obj, "formats", formats_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().no_arg_ret.push(formats_fn);

    register_two_arg_promise(bridge, &io_obj, "import", |name_js, bytes_js| {
        let name = name_js.as_string().unwrap_or_else(|| "photo.png".to_string());
        match decode_bytes(&bytes_js) {
            Ok(bytes) => ("io.import".to_string(), json!({ "name": name, "base64": BASE64.encode(bytes) })),
            Err(err) => ("io.importError".to_string(), json!({ "error": err })),
        }
    })?;

    let b_exp = Rc::clone(bridge);
    let export_fn = Closure::wrap(Box::new(move |opts_js: JsValue| enqueue_promise(&b_exp, "io.export", from_js(&opts_js))) as Box<dyn FnMut(JsValue) -> js_sys::Promise>);
    set_prop(&io_obj, "export", export_fn.as_ref().unchecked_ref())?;
    bridge.borrow_mut().one_arg_promise.push(export_fn);

    set_prop(host_obj, "io", &io_obj)?;
    Ok(())
}

fn set_theme_sync(bridge: &SharedBridge, mode: Option<&str>) -> bool {
    let current = bridge.borrow().dark;
    let dark = match mode.map(str::trim) {
        Some(m) if m.eq_ignore_ascii_case("dark") => true,
        Some(m) if m.eq_ignore_ascii_case("light") => false,
        Some(m) if m.eq_ignore_ascii_case("toggle") || m.is_empty() => !current,
        _ => current,
    };
    {
        let mut guard = bridge.borrow_mut();
        guard.dark = dark;
        guard.pending_sync.push_back(SyncAction::SetTheme { dark });
        if let Some(ctx) = &guard.egui_ctx {
            ctx.request_repaint();
        }
    }
    crate::theme::sync_document_theme(dark);
    emit_theme(dark);
    dark
}

fn enqueue_promise(bridge: &SharedBridge, method: &str, params: Value) -> js_sys::Promise {
    let b = Rc::clone(bridge);
    let m = method.to_string();
    js_sys::Promise::new(&mut |resolve, reject| {
        let mut guard = b.borrow_mut();
        guard.pending_rpcs.push_back(PendingRpc { method: m.clone(), params: params.clone(), resolve, reject });
        if let Some(ctx) = &guard.egui_ctx {
            ctx.request_repaint();
        }
    })
}

fn register_two_arg_promise<F>(bridge: &SharedBridge, target: &js_sys::Object, prop: &str, map_args: F) -> Result<(), String>
where
    F: Fn(JsValue, JsValue) -> (String, Value) + 'static,
{
    let b = Rc::clone(bridge);
    let cb = Closure::wrap(Box::new(move |a: JsValue, c: JsValue| {
        let (method, params) = map_args(a, c);
        enqueue_promise(&b, &method, params)
    }) as Box<dyn FnMut(JsValue, JsValue) -> js_sys::Promise>);
    set_prop(target, prop, cb.as_ref().unchecked_ref())?;
    bridge.borrow_mut().two_arg_promise.push(cb);
    Ok(())
}

/// Apply pending sync updates and forward async bridge requests before `LightcraftApp::logic`.
pub fn prepare_frame(bridge: &SharedBridge, app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    let (sync_actions, rpcs) = {
        let mut guard = bridge.borrow_mut();
        guard.egui_ctx = Some(ctx.clone());
        let syncs: Vec<_> = guard.pending_sync.drain(..).collect();
        let rpcs: Vec<_> = guard.pending_rpcs.drain(..).collect();
        (syncs, rpcs)
    };
    let mut theme_override = None;
    for action in sync_actions {
        match action {
            SyncAction::OpenBytes { name, bytes } => {
                let _ = import_photo_bytes(&bridge.borrow().originals, app, &name, bytes);
            }
            SyncAction::SetTheme { dark } => {
                apply_theme_to_app(app, ctx, dark);
                theme_override = Some(dark);
            }
        }
    }
    for rpc in rpcs {
        handle_rpc(bridge, app, ctx, rpc);
    }
    theme_override.unwrap_or_else(|| app.ui.settings.theme(app.system_theme(ctx)).is_dark())
}

/// Poll async control replies and refresh command list after `LightcraftApp::logic`.
pub fn finish_frame(bridge: &SharedBridge, app: &LightcraftApp) {
    let mut waiting = {
        let mut guard = bridge.borrow_mut();
        std::mem::take(&mut guard.waiting_ctrl)
    };
    waiting.retain(|item| match item.rx.try_recv() {
        Ok(val) => {
            if val.get("ok").and_then(Value::as_bool) == Some(false) {
                let err = val.get("error").and_then(Value::as_str).unwrap_or("control call failed");
                let _ = item.reject.call1(&JsValue::NULL, &JsValue::from_str(err));
            } else {
                let payload = val.get("result").cloned().unwrap_or(val);
                let _ = item.resolve.call1(&JsValue::NULL, &to_js(&payload));
            }
            false
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => true,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            let _ = item.reject.call1(&JsValue::NULL, &JsValue::from_str("control channel closed"));
            false
        }
    });
    let mut guard = bridge.borrow_mut();
    guard.waiting_ctrl.extend(waiting);
    refresh_commands_inner(&mut guard, app);
}

fn apply_theme_to_app(app: &mut LightcraftApp, ctx: &egui::Context, dark: bool) {
    let theme_id = if dark { "midnight" } else { "paper" };
    let _ = app.run("view.theme", json!({ "theme": theme_id }));
    crate::theme::sync_document_theme(dark);
    crate::theme::apply_palette(ctx, dark);
    ctx.request_repaint();
}

fn handle_rpc(bridge: &SharedBridge, app: &mut LightcraftApp, ctx: &egui::Context, rpc: PendingRpc) {
    let method = rpc.method.as_str();
    let params = rpc.params;
    let resolve = rpc.resolve;
    let reject = rpc.reject;

    if should_forward_to_control(method) {
        let (req, rx) = control::ControlRequest::new(method, params);
        let tx = bridge.borrow().ctrl_tx.clone();
        if tx.send(req).is_ok() {
            bridge.borrow_mut().waiting_ctrl.push(WaitingControl { rx, resolve, reject });
            ctx.request_repaint();
        } else {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("control channel unavailable"));
        }
        return;
    }

    let result = dispatch_sync(bridge, app, ctx, method, params);
    match result {
        Ok(val) => {
            let _ = resolve.call1(&JsValue::NULL, &to_js(&val));
        }
        Err(msg) => {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&msg));
        }
    }
}

fn should_forward_to_control(method: &str) -> bool {
    matches!(method, "ui.menu.list" | "ui.menu.tree" | "ui.widgets")
        || method.starts_with("ui.click")
        || method.starts_with("ui.drag")
        || method.starts_with("ui.hover")
        || method.starts_with("ui.move")
        || method.starts_with("ui.pointer")
        || method.starts_with("ui.key")
        || method.starts_with("ui.text")
        || method.starts_with("ui.clipboard")
        || method.starts_with("ui.scroll")
        || method.starts_with("ui.zoom")
        || method.starts_with("ui.dialog")
        || method.starts_with("ui.set")
        || method.starts_with("ui.dropFiles")
        || method.starts_with("ui.resize")
}

fn dispatch_sync(bridge: &SharedBridge, app: &mut LightcraftApp, ctx: &egui::Context, method: &str, params: Value) -> Result<Value, String> {
    match method {
        "commands.list" | "engine.commands" => Ok(Value::Array(control::all_commands(app).as_array().cloned().unwrap_or_default())),
        "commands.describe" => {
            let id = params.get("id").or_else(|| params.get("command")).and_then(Value::as_str).unwrap_or("");
            Ok(find_command(&bridge.borrow().commands, id).unwrap_or(Value::Null))
        }
        "theme.get" => {
            let dark = app.ui.settings.theme(app.system_theme(ctx)).is_dark();
            Ok(json!({ "theme": if dark { "dark" } else { "light" }, "dark": dark }))
        }
        "theme.set" => {
            let mode = params.get("theme").or_else(|| params.get("mode")).and_then(Value::as_str);
            let dark = set_theme_sync(bridge, mode);
            apply_theme_to_app(app, ctx, dark);
            Ok(json!({ "theme": if dark { "dark" } else { "light" }, "dark": dark }))
        }
        "io.formats" => Ok(io_formats_value()),
        "io.importError" => Err(params.get("error").and_then(Value::as_str).unwrap_or("invalid byte payload").to_string()),
        "io.import" => handle_io_import(bridge, app, &params),
        "io.export" => handle_io_export(app, &params),
        "handoff.emit" => {
            let target = params.get("target").and_then(Value::as_str).unwrap_or("");
            let payload = params.get("payload").cloned().unwrap_or(Value::Null);
            emit_handoff(target, &payload);
            Ok(json!({ "ok": true, "target": target }))
        }
        "document.inspect" => inspect_catalog_or_photo(app, &params),
        "ui.inspect" => Ok(control::inspect(app, ctx)),
        "fogbreak.inspect" => Ok(json!({
            "engine": ENGINE_ID,
            "document": inspect_catalog_or_photo(app, &json!({})).ok(),
            "ui": control::inspect(app, ctx),
        })),
        "fogbreak.screenshot" | "ui.screenshot" => capture_canvas_png(),
        "engine.execute" | "command" => {
            let id = params.get("command").or_else(|| params.get("id")).and_then(Value::as_str).ok_or("missing `command`")?;
            let args = params.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
            let res = app.run(id, args)?;
            ctx.request_repaint();
            Ok(res)
        }
        other => {
            let args = if params.is_null() { json!({}) } else { params };
            let res = app.run(other, args)?;
            ctx.request_repaint();
            Ok(res)
        }
    }
}

fn inspect_catalog_or_photo(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if app.session.active().is_some() || params.get("id").is_some() {
        let mut out = app.run("photo.inspect", params.clone())?;
        if let Some(obj) = out.as_object_mut() {
            obj.insert("photoCount".into(), json!(app.session.catalog.len()));
            if let Ok(state) = app.run("library.state", json!({})) {
                obj.insert("library".into(), state);
            }
        }
        return Ok(out);
    }
    let state = app.run("library.state", json!({})).unwrap_or_else(|_| json!({}));
    Ok(json!({
        "photoCount": app.session.catalog.len(),
        "active": null,
        "library": state,
    }))
}

fn handle_io_import(bridge: &SharedBridge, app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("photo.png");
    let b64 = params.get("base64").and_then(Value::as_str).ok_or("missing `base64`")?;
    let bytes = BASE64.decode(b64.trim()).map_err(|e| format!("invalid base64: {e}"))?;
    let len = bytes.len();
    let result = import_photo_bytes(&bridge.borrow().originals, app, name, bytes)?;
    emit_open(name, len);
    Ok(json!({ "ok": true, "name": name, "bytes": len, "result": result }))
}

fn import_photo_bytes(originals: &Originals, app: &mut LightcraftApp, name: &str, bytes: Vec<u8>) -> Result<Value, String> {
    let hash = content_hash(&bytes);
    let path = originals.added(name, &hash, Arc::from(bytes));
    let imported = app.run("library.import", json!({ "paths": [path] }))?;
    if let Some(id) = imported.get("imported").and_then(Value::as_array).and_then(|arr| arr.first()).and_then(Value::as_u64) {
        let _ = app.run("library.select", json!({ "ids": [id] }));
    }
    Ok(imported)
}

fn handle_io_export(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    let id = params
        .get("id")
        .and_then(Value::as_u64)
        .map(lightcraft_catalog::PhotoId)
        .or_else(|| app.session.active())
        .ok_or("no active or selected photo to export")?;
    let fmt_str = params.get("format").and_then(Value::as_str).unwrap_or("jpeg");
    let fmt = ExportFormat::parse(fmt_str).ok_or_else(|| format!("unsupported export format `{fmt_str}`"))?;
    let mut opts = ExportOptions { format: fmt, ..ExportOptions::default() };
    if let Some(q) = params.get("quality").and_then(Value::as_u64) {
        opts.quality = q.clamp(1, 100) as u8;
    }
    if let Some(hdr) = params.get("hdr").and_then(Value::as_bool) {
        opts.hdr = hdr;
    }
    let exported = export_photo(&mut app.session, id, &opts, 1)?;
    let mime = lightcraft_web::store::mime_for(&exported.file_name);
    let b64 = BASE64.encode(&exported.bytes);
    emit_save(&exported.file_name, Some(fmt_str), &exported.bytes);
    Ok(json!({
        "name": exported.file_name,
        "format": fmt_str,
        "mimeType": mime,
        "width": exported.width,
        "height": exported.height,
        "bytes": exported.bytes.len(),
        "base64": b64,
    }))
}

pub fn encode_png_rgba(img: &Rgba8) -> Vec<u8> {
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
}

fn capture_canvas_png() -> Result<Value, String> {
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("document unavailable")?;
    let el = doc.get_element_by_id(CANVAS_ID).ok_or("canvas element not found")?;
    let canvas: web_sys::HtmlCanvasElement = el.dyn_into().map_err(|_| "element is not a canvas".to_string())?;
    let url = canvas.to_data_url_with_type("image/png").map_err(|_| "canvas capture failed".to_string())?;
    Ok(json!({
        "dataUrl": url,
        "mimeType": "image/png",
        "width": canvas.width(),
        "height": canvas.height(),
    }))
}

fn io_formats_value() -> Value {
    json!({
        "import": ["jpg", "jpeg", "png", "tif", "tiff", "webp", "avif", "jxl", "heic", "heif", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "pef", "srw", "psd", "gif"],
        "export": ["jpeg", "jpg", "png", "tiff", "tif", "psd", "webp", "avif", "jxl", "original", "dng", "rendereddng"],
    })
}

fn filter_commands(commands: &[Value], query: &str) -> Value {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Value::Array(commands.to_vec());
    }
    let filtered: Vec<Value> = commands
        .iter()
        .filter(|entry| {
            ["id", "label", "shortcut"].iter().any(|field| {
                entry.get(*field).and_then(Value::as_str).is_some_and(|v| v.to_ascii_lowercase().contains(&needle))
            })
        })
        .cloned()
        .collect();
    Value::Array(filtered)
}

fn find_command(commands: &[Value], wanted: &str) -> Option<Value> {
    let w = wanted.trim();
    if w.is_empty() {
        return None;
    }
    commands
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(w))
        .cloned()
}

fn decode_bytes(val: &JsValue) -> Result<Vec<u8>, String> {
    if let Some(s) = val.as_string() {
        return BASE64.decode(s.trim()).map_err(|e| format!("invalid base64: {e}"));
    }
    if val.is_instance_of::<js_sys::Uint8Array>() || val.is_instance_of::<js_sys::ArrayBuffer>() {
        return Ok(js_sys::Uint8Array::new(val).to_vec());
    }
    if js_sys::Array::is_array(val) {
        let arr = js_sys::Array::from(val);
        let mut out = Vec::with_capacity(arr.length() as usize);
        for item in arr.iter() {
            let n = item.as_f64().ok_or("byte array element must be numeric")?;
            out.push((n as i64).clamp(0, 255) as u8);
        }
        return Ok(out);
    }
    Err("expected Uint8Array, ArrayBuffer, number[], or base64 string".to_string())
}

pub fn emit_ready(rpc_key: &str, command_count: usize) {
    emit_event(
        "ready",
        &json!({
            "engine": ENGINE_ID,
            "rpcKey": rpc_key,
            "commandCount": command_count,
            "version": env!("CARGO_PKG_VERSION"),
        }),
    );
}

pub fn emit_failed(error: &str) {
    emit_event("failed", &json!({ "engine": ENGINE_ID, "error": error }));
}

pub fn emit_dirty(dirty: bool, active_id: Option<u64>, title: Option<&str>) {
    emit_event(
        "dirty",
        &json!({
            "engine": ENGINE_ID,
            "dirty": dirty,
            "activePhoto": active_id,
            "title": title,
        }),
    );
}

pub fn emit_save(name: &str, format: Option<&str>, bytes: &[u8]) {
    emit_event(
        "save",
        &json!({
            "engine": ENGINE_ID,
            "name": name,
            "format": format,
            "bytes": bytes.len(),
            "base64": BASE64.encode(bytes),
        }),
    );
}

pub fn emit_open(name: &str, byte_count: usize) {
    emit_event("open", &json!({ "engine": ENGINE_ID, "name": name, "bytes": byte_count }));
}

pub fn emit_handoff(target: &str, payload: &Value) {
    emit_event("handoff", &json!({ "engine": ENGINE_ID, "target": target, "payload": payload }));
}

pub fn emit_theme(dark: bool) {
    emit_event(
        "theme",
        &json!({
            "engine": ENGINE_ID,
            "theme": if dark { "dark" } else { "light" },
            "dark": dark,
        }),
    );
}

fn emit_event(suffix: &str, detail: &Value) {
    let Some(win) = web_sys::window() else {
        return;
    };
    let name = format!("{ENGINE_ID}:{suffix}");
    let init = web_sys::CustomEventInit::new();
    init.set_bubbles(true);
    init.set_detail(&to_js(detail));
    let Ok(ev) = web_sys::CustomEvent::new_with_event_init_dict(&name, &init) else {
        return;
    };
    let _ = win.dispatch_event(&ev);
    if let Some(canvas) = win.document().and_then(|d| d.get_element_by_id(CANVAS_ID)) {
        let _ = canvas.dispatch_event(&ev);
    }
}

fn to_js(val: &Value) -> JsValue {
    let s = serde_json::to_string(val).unwrap_or_else(|_| "null".to_string());
    js_sys::JSON::parse(&s).unwrap_or(JsValue::NULL)
}

fn from_js(val: &JsValue) -> Value {
    if val.is_null() || val.is_undefined() {
        return Value::Null;
    }
    let Ok(s) = js_sys::JSON::stringify(val) else {
        return Value::Null;
    };
    s.as_string()
        .and_then(|txt| serde_json::from_str::<Value>(&txt).ok())
        .unwrap_or(Value::Null)
}

fn set_prop(target: &JsValue, key: &str, val: &JsValue) -> Result<(), String> {
    js_sys::Reflect::set(target, &JsValue::from_str(key), val).map(|_| ()).map_err(|_| format!("failed to set host property `{key}`"))
}
