//! Fogbreak Light — WebGL/WASM host overlay.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
mod host_bridge;
mod theme;

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("fogbreak-light must be compiled and served for wasm32-unknown-unknown (via Trunk)");
}

#[cfg(target_arch = "wasm32")]
fn main() {
    wasm_bindgen_futures::spawn_local(async {
        if let Err(err) = web::start().await {
            web::show_error(&err);
            host_bridge::emit_failed(&err);
        }
    });
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::sync::{Arc, Mutex, mpsc};

    use lightcraft_engine::Session;
    use lightcraft_ui_egui::{LightcraftApp, Services, control, pick};
    use lightcraft_web::store::{Originals, content_hash};
    use serde_json::json;
    use wasm_bindgen::JsCast;

    use crate::{host_bridge, theme};

    pub struct FogbreakLightApp {
        app: LightcraftApp,
        bridge: host_bridge::SharedBridge,
        originals: Originals,
        dropped_pending: Arc<Mutex<Vec<String>>>,
        last_dark: bool,
        last_dirty: Option<(bool, Option<u64>, Option<String>)>,
        ready_emitted: bool,
    }

    pub async fn start() -> Result<(), String> {
        let doc = web_sys::window().and_then(|w| w.document()).ok_or("document unavailable")?;
        let canvas_el = doc.get_element_by_id(host_bridge::CANVAS_ID).ok_or("canvas element not found")?;
        let canvas: web_sys::HtmlCanvasElement = canvas_el.dyn_into().map_err(|_| "element is not a canvas".to_string())?;

        let dark = theme::initial_dark_mode();
        theme::sync_document_theme(dark);

        let originals = Originals::default();
        let (ctrl_tx, ctrl_rx) = mpsc::channel();
        let bridge = host_bridge::new_shared(originals.clone(), ctrl_tx, dark);
        host_bridge::install(&bridge)?;

        let options = eframe::WebOptions::default();
        let runner = eframe::WebRunner::new();
        let bridge_for_app = std::rc::Rc::clone(&bridge);
        let orig_for_app = originals.clone();

        runner
            .start(
                canvas,
                options,
                Box::new(move |cc| Ok(Box::new(build_app(&cc.egui_ctx, bridge_for_app, orig_for_app, ctrl_rx, dark)))),
            )
            .await
            .map_err(|e| format!("WebRunner start failed: {e:?}"))
    }

    fn build_app(
        ctx: &egui::Context,
        bridge: host_bridge::SharedBridge,
        originals: Originals,
        ctrl_rx: mpsc::Receiver<control::ControlRequest>,
        dark: bool,
    ) -> FogbreakLightApp {
        theme::apply_palette(ctx, dark);
        let mut session = if query_param("demo").is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes")) {
            Session::with_demo()
        } else {
            Session::new()
        };
        originals.install(&mut session);
        let services = build_services(originals.clone(), ctx.clone());
        let mut app = LightcraftApp::new(session, services).with_control(ctrl_rx);
        let theme_id = if dark { "midnight" } else { "paper" };
        let _ = app.run("view.theme", json!({ "theme": theme_id }));
        host_bridge::attach_ctx(&bridge, ctx, &app);
        FogbreakLightApp {
            app,
            bridge,
            originals,
            dropped_pending: Arc::new(Mutex::new(Vec::new())),
            last_dark: dark,
            last_dirty: None,
            ready_emitted: false,
        }
    }

    fn build_services(originals: Originals, ctx: egui::Context) -> Services {
        let orig_pick = originals.clone();
        let ctx_pick = ctx.clone();
        Services {
            picker: Some(Box::new(move |req: pick::PickRequest| {
                let (tx, rx) = mpsc::channel();
                let orig = orig_pick.clone();
                let ctx_c = ctx_pick.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let paths = run_async_picker(&req, &orig).await;
                    let _ = tx.send(paths);
                    ctx_c.request_repaint();
                });
                Ok(rx)
            })),
            write: Some(Box::new(|path: &str, bytes: &[u8]| trigger_browser_download(path, bytes))),
            png: Some(Box::new(host_bridge::encode_png_rgba)),
            open_url: Some(Box::new(open_browser_url)),
            ..Default::default()
        }
    }

    async fn run_async_picker(req: &pick::PickRequest, originals: &Originals) -> Vec<String> {
        let mut dlg = rfd::AsyncFileDialog::new();
        if let Some((label, exts)) = &req.filter {
            dlg = dlg.add_filter(label, exts);
        }
        match req.kind {
            pick::PickKind::Files => {
                let Some(handles) = dlg.pick_files().await else {
                    return Vec::new();
                };
                let mut paths = Vec::with_capacity(handles.len());
                for handle in handles {
                    let name = handle.file_name();
                    let bytes = handle.read().await;
                    let hash = content_hash(&bytes);
                    paths.push(originals.added(&name, &hash, Arc::from(bytes)));
                }
                paths
            }
            pick::PickKind::File => {
                let Some(handle) = dlg.pick_file().await else {
                    return Vec::new();
                };
                let name = handle.file_name();
                let bytes = handle.read().await;
                let hash = content_hash(&bytes);
                vec![originals.added(&name, &hash, Arc::from(bytes))]
            }
            pick::PickKind::Folder | pick::PickKind::Save => Vec::new(),
        }
    }

    fn open_browser_url(url: &str) -> Result<(), String> {
        let win = web_sys::window().ok_or("window unavailable")?;
        win.open_with_url_and_target(url, "_blank")
            .map(|_| ())
            .map_err(|_| "failed to open URL".to_string())
    }

    fn trigger_browser_download(path: &str, bytes: &[u8]) -> Result<(), String> {
        let doc = web_sys::window().and_then(|w| w.document()).ok_or("document unavailable")?;
        let arr = js_sys::Uint8Array::from(bytes);
        let parts = js_sys::Array::of1(&arr);
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type(lightcraft_web::store::mime_for(path));
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(|_| "blob creation failed".to_string())?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| "object URL creation failed".to_string())?;
        let el = doc.create_element("a").map_err(|_| "anchor creation failed".to_string())?;
        let anchor: web_sys::HtmlAnchorElement = el.dyn_into().map_err(|_| "element is not anchor".to_string())?;
        let filename = lightcraft_web::store::download_name(path);
        anchor.set_href(&url);
        anchor.set_download(filename);
        if let Some(body) = doc.body() {
            let _ = body.append_child(&anchor);
            anchor.click();
            let _ = body.remove_child(&anchor);
        }
        let _ = web_sys::Url::revoke_object_url(&url);
        host_bridge::emit_save(filename, None, bytes);
        Ok(())
    }

    impl eframe::App for FogbreakLightApp {
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.enqueue_dropped_files(ctx);
            let dark = host_bridge::prepare_frame(&self.bridge, &mut self.app, ctx);
            self.app.logic(ctx);
            host_bridge::finish_frame(&self.bridge, &self.app);
            if dark != self.last_dark {
                self.last_dark = dark;
                theme::sync_document_theme(dark);
                theme::apply_palette(ctx, dark);
                host_bridge::emit_theme(dark);
            }
            self.sync_dirty_event();
        }

        fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
            self.app.raw_input_hook(ctx, raw);
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.app.ui(ui);
            if !self.ready_emitted {
                self.ready_emitted = true;
                hide_loading();
                let rpc_key = self.bridge.borrow().rpc_key.clone();
                let count = self.bridge.borrow().commands.len();
                host_bridge::emit_ready(&rpc_key, count);
            }
        }
    }

    impl FogbreakLightApp {
        fn enqueue_dropped_files(&mut self, ctx: &egui::Context) {
            let dropped = ctx.input(|i| i.raw.dropped_files.clone());
            for file in dropped {
                let originals = self.originals.clone();
                let pending = Arc::clone(&self.dropped_pending);
                let ctx_c = ctx.clone();
                let name = egui::DroppedFile::path(file.as_ref())
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("dropped.png")
                    .to_string();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(bytes) = egui::DroppedFile::bytes_async(file.as_ref()).await {
                        let hash = content_hash(&bytes);
                        let path = originals.added(&name, &hash, Arc::from(bytes));
                        if let Ok(mut guard) = pending.lock() {
                            guard.push(path);
                        }
                        ctx_c.request_repaint();
                    }
                });
            }
            let ready_paths = self
                .dropped_pending
                .lock()
                .map(|mut guard| std::mem::take(&mut *guard))
                .unwrap_or_default();
            if !ready_paths.is_empty() {
                let _ = self.app.run("library.import", json!({ "paths": ready_paths }));
                ctx.request_repaint();
            }
        }

        fn sync_dirty_event(&mut self) {
            let active_id = self.app.session.active().map(|id| id.0);
            let dirty = self.app.session.unsaved().is_some();
            let title = active_id
                .and_then(|id| self.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)))
                .map(|p| p.file_name.clone());
            let state = (dirty, active_id, title.clone());
            if self.last_dirty.as_ref() != Some(&state) {
                self.last_dirty = Some(state);
                host_bridge::emit_dirty(dirty, active_id, title.as_deref());
            }
        }
    }

    fn query_param(key: &str) -> Option<String> {
        let win = web_sys::window()?;
        let search = win.location().search().ok()?;
        let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
        params.get(key)
    }

    fn hide_loading() {
        if let Some(doc) = web_sys::window().and_then(|w| w.document())
            && let Some(el) = doc.get_element_by_id("fogbreak_loading")
        {
            let _ = el.set_attribute("hidden", "true");
        }
    }

    pub fn show_error(msg: &str) {
        hide_loading();
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(txt) = doc.get_element_by_id("fogbreak_error_message") {
                txt.set_text_content(Some(msg));
            }
            if let Some(box_el) = doc.get_element_by_id("fogbreak_error") {
                let _ = box_el.remove_attribute("hidden");
            }
        }
    }
}
