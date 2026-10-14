//! The browser shell: web `Services`, drag-and-drop, and the eframe web runner.

use gridcraft_engine::Session;
use gridcraft_ui_egui::{Inbox, Services, SheetApp};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

const CANVAS_ID: &str = "gridcraft_canvas";
const LOADING_ID: &str = "gridcraft_loading";

pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    crate::perf::mark("wasm compiled and started");
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        let options = eframe::WebOptions::default();
        // `?host=parent`: a same-origin page framing GridCraft (such as a Nextcloud app) opens
        // workbooks in it and stores what it saves (`host`).
        let bridge = if query_param("host").as_deref() == Some("parent") { crate::host::Bridge::new() } else { None };
        let app_bridge = bridge.clone();
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    crate::perf::mark("WebGL2 context ready");
                    keyboard_on_tap(cc.egui_ctx.clone());
                    let inbox: Inbox = Inbox::default();
                    let mut session = Session::new();
                    let mut services = services(inbox.clone(), cc.egui_ctx.clone());
                    // As a webxdc app the chat shares one workbook.
                    let mut webxdc = crate::webxdc::api().and_then(|api| crate::webxdc::Webxdc::new(api, &mut session, &mut services));
                    // In host mode the parent sends the workbook to edit; no sample meanwhile.
                    let q = if app_bridge.is_some() || webxdc.is_some() { String::new() } else { query() };
                    if let Some(name) = q.split(['?', '&']).find_map(|kv| kv.strip_prefix("sample=")) {
                        let _ = session.execute("file.new", serde_json::json!({"sample": name}));
                    } else if q.contains("sample") {
                        let _ = session.execute("file.new", serde_json::json!({"sample": "sales"}));
                    }
                    if let Some(bridge) = &app_bridge {
                        bridge.connect(&mut services, inbox.clone(), cc.egui_ctx.clone());
                    }
                    crate::perf::mark("session ready");
                    let mut app = SheetApp::new(session, services);
                    crate::perf::mark("app ready");
                    app.ui.system_theme = true;
                    // Follow the browser's language, except a CJK one: the web build has no system
                    // CJK fonts (none are bundled), so that interface would render as tofu.
                    let browser = gridcraft_ui_egui::i18n::Language::system();
                    if !matches!(
                        browser,
                        gridcraft_ui_egui::i18n::Language::Zh | gridcraft_ui_egui::i18n::Language::Ja | gridcraft_ui_egui::i18n::Language::Ko
                    ) {
                        app.ui.language = browser;
                    }
                    SheetApp::setup_context_for_language(&cc.egui_ctx, false, app.ui.language);
                    crate::perf::mark("fonts and language set up");
                    if let Some(bridge) = &app_bridge {
                        if let Some(author) = query_param("author") {
                            crate::host::Bridge::set_author(&mut app, &author);
                        }
                        bridge.post_ready();
                    }
                    if let Some(webxdc) = &mut webxdc {
                        webxdc.listen(cc.egui_ctx.clone());
                        crate::perf::mark("listening for shared edits");
                    }
                    Ok(Box::new(WebShell { app, inbox, bridge: app_bridge, webxdc, frames: 0, shown: None }))
                }),
            )
            .await;
        if let (Err(e), Some(bridge)) = (&result, &bridge) {
            bridge.post_failed(&format!("{e:?}"));
        }
        if let Some(el) = document.get_element_by_id(LOADING_ID) {
            match result {
                Ok(()) => el.remove(),
                Err(e) => el.set_inner_html(&format!("<p>GridCraft failed to start: {e:?}</p><p>A browser with WebGL2 is required.</p>")),
            }
        }
    });
}

/// iOS opens the on-screen keyboard only for a text field focused inside a touch handler, and eframe focuses its text
/// input a frame later. So on touch screens eframe's (transparent) text input lies over the active cell, where a tap
/// focuses it natively and edits the cell ([`gridcraft_ui_egui::cell_zone`]); a tap on another
/// [`gridcraft_ui_egui::keyboard_zones`] rect (the formula bar) focuses it from the touch handler. eframe takes the
/// focus back if the frame starts no editing.
fn keyboard_on_tap(ctx: egui::Context) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    let touch_screen = web_sys::window().and_then(|w| w.match_media("(pointer: coarse)").ok().flatten()).is_some_and(|m| m.matches());
    if touch_screen {
        ctx.on_end_pass("gridcraft.cell_input", std::sync::Arc::new(|ui: &mut egui::Ui| place_cell_input(ui.ctx())));
    }
    // Where and when the touch began, and whether a second finger joined it.
    let start = std::rc::Rc::new(std::cell::Cell::new((0, 0, 0.0, false)));
    let start_set = start.clone();
    let on_start = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(move |e: web_sys::TouchEvent| {
        if let Some(t) = e.changed_touches().get(0) {
            start_set.set((t.client_x(), t.client_y(), e.time_stamp(), e.touches().length() > 1));
        }
    });
    let on_end = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(move |e: web_sys::TouchEvent| {
        // One finger, lifted soon and near where it went down: a tap, not a scroll, a pinch or a long press.
        let Some(t) = e.changed_touches().get(0).filter(|_| e.touches().length() == 0) else { return };
        let (x, y, at, multi) = start.get();
        if multi || e.time_stamp() - at > 500.0 || (t.client_x() - x).abs() > 10 || (t.client_y() - y).abs() > 10 {
            return;
        }
        let Some((canvas, input)) = canvas_and_input() else { return };
        let target = e.target();
        if target.as_ref() == Some(input.as_ref()) {
            gridcraft_ui_egui::tap_cell_zone(&ctx);
            return;
        }
        if target.as_ref() != Some(canvas.as_ref()) {
            return;
        }
        let r = canvas.get_bounding_client_rect();
        let zoom = ctx.zoom_factor();
        let p = egui::pos2((f64::from(t.client_x()) - r.left()) as f32 / zoom, (f64::from(t.client_y()) - r.top()) as f32 / zoom);
        if gridcraft_ui_egui::tap_opens_keyboard(&ctx, p) {
            let options = web_sys::FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = input.focus_with_options(&options);
        }
    });
    // Capturing: eframe stops touches on the canvas from bubbling.
    let _ = document.add_event_listener_with_callback_and_bool("touchstart", on_start.as_ref().unchecked_ref(), true);
    let _ = document.add_event_listener_with_callback_and_bool("touchend", on_end.as_ref().unchecked_ref(), true);
    // Listening for the page's lifetime.
    on_start.forget();
    on_end.forget();
}

/// The canvas, and eframe's text input (which eframe puts right after it).
fn canvas_and_input() -> Option<(web_sys::HtmlCanvasElement, web_sys::HtmlInputElement)> {
    let canvas = web_sys::window()?.document()?.get_element_by_id(CANVAS_ID)?.dyn_into::<web_sys::HtmlCanvasElement>().ok()?;
    let input = canvas.next_element_sibling()?.dyn_into::<web_sys::HtmlInputElement>().ok()?;
    Some((canvas, input))
}

/// Lays eframe's text input over the active cell while it takes typing; otherwise shrinks it back to the 1px field
/// eframe moves to the text cursor.
fn place_cell_input(ctx: &egui::Context) {
    let Some((canvas, input)) = canvas_and_input() else { return };
    let style = input.style();
    let size = match gridcraft_ui_egui::cell_zone(ctx) {
        Some(z) => {
            let zoom = ctx.zoom_factor();
            let _ = style.set_property("left", &format!("{}px", canvas.offset_left() as f32 + z.left() * zoom));
            let _ = style.set_property("top", &format!("{}px", canvas.offset_top() as f32 + z.top() * zoom));
            (z.width() * zoom, z.height() * zoom)
        }
        None => (1.0, 1.0),
    };
    let _ = style.set_property("width", &format!("{}px", size.0));
    let _ = style.set_property("height", &format!("{}px", size.1));
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

fn query_param(key: &str) -> Option<String> {
    web_sys::UrlSearchParams::new_with_str(&query()).ok()?.get(key)
}

struct WebShell {
    app: SheetApp,
    inbox: Inbox,
    bridge: Option<crate::host::Bridge>,
    webxdc: Option<crate::webxdc::Webxdc>,
    /// Frames run, and the one that first showed the workbook or the start screen, for the
    /// startup report ([`crate::perf`]).
    frames: u64,
    shown: Option<u64>,
}

impl eframe::App for WebShell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        if self.frames == 2 {
            crate::perf::mark("first frame painted");
        }
        if self.shown.is_some_and(|f| self.frames == f + 1) {
            crate::perf::mark("… and painted");
            crate::perf::report();
        }
        let dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        for f in dropped {
            // In a chat a dropped file replaces the workbook for everyone: webxdc asks first.
            let inbox = self.webxdc.as_ref().map_or_else(|| self.inbox.clone(), |w| w.dropped.clone());
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let name = f.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "dropped.xlsx".into());
                match f.bytes_async().await {
                    Ok(bytes) => {
                        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
                        ctx.request_repaint();
                    }
                    Err(e) => log::error!("couldn't read dropped file {name}: {e}"),
                }
            });
        }
        if let Some(webxdc) = &mut self.webxdc {
            webxdc.before(&mut self.app, &self.inbox);
        }
        self.app.logic(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.frames == 1 {
            crate::perf::mark("first frame: input and shared edits handled");
        }
        self.app.ui(ui);
        if self.frames == 1 {
            crate::perf::mark("first frame laid out");
        }
        if let Some(webxdc) = &mut self.webxdc {
            webxdc.after(&mut self.app);
        }
        if self.shown.is_none() && self.app.start != gridcraft_ui_egui::Start::Loading {
            self.shown = Some(self.frames);
            let what = if self.app.start == gridcraft_ui_egui::Start::Off { "workbook" } else { "start screen" };
            crate::perf::mark(format!("{what} laid out (frame {})", self.frames));
            // Paint once more for the report even when nothing else asks for a frame.
            ui.ctx().request_repaint();
        }
        if let Some(bridge) = &self.bridge {
            bridge.sync(&mut self.app);
        }
    }
}

fn services(inbox: Inbox, ctx: egui::Context) -> Services {
    let open_inbox = inbox.clone();
    let picture_ctx = ctx.clone();
    Services {
        copy_html: Some(Box::new(copy_html)),
        open_async: Some(Box::new(move || {
            let inbox = open_inbox.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let Some(file) = rfd::AsyncFileDialog::new()
                    .add_filter("Spreadsheets", &["xlsx", "xlsm", "xls", "xlsb", "ods", "csv", "tsv", "txt", "json"])
                    .pick_file()
                    .await
                else {
                    return;
                };
                let bytes = file.read().await;
                inbox.lock().unwrap_or_else(|e| e.into_inner()).push((file.file_name(), bytes));
                ctx.request_repaint();
            });
        })),
        pick_picture_async: Some(Box::new(move |inbox| {
            let ctx = picture_ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let Some(file) = rfd::AsyncFileDialog::new().add_filter("Pictures (PNG, JPEG)", &["png", "jpg", "jpeg"]).pick_file().await else {
                    return;
                };
                let bytes = file.read().await;
                inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((file.file_name(), bytes));
                ctx.request_repaint();
            });
        })),
        download: Some(Box::new(|name: &str, bytes: &[u8]| {
            if let Err(e) = download(name, bytes) {
                log::error!("download of {name} failed: {e}");
            }
        })),
        print: Some(Box::new(|bytes: &[u8]| {
            if let Err(e) = print_pdf(bytes) {
                log::error!("print failed: {e}");
            }
        })),
        open_url: Some(Box::new(|url: &str| {
            if let Some(w) = web_sys::window()
                && let gridcraft_engine::links::LinkKind::External(url) = gridcraft_engine::links::classify(url)
            {
                let _ = w.open_with_url_and_target(&url, "_blank");
            }
        })),
        inbox: Some(inbox),
        ..Default::default()
    }
}

/// Start inside the copy gesture; deferring `write` can lose browser user activation.
fn copy_html(html: &str, text: &str) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let clipboard = window.navigator().clipboard();
    if clipboard.is_undefined() || clipboard.is_null() {
        return Err("browser clipboard is unavailable (a secure context is required)".into());
    }
    if !js_sys::Reflect::get(&clipboard, &"write".into()).map_err(js)?.is_function() {
        return Err("browser does not support rich clipboard writes".into());
    }
    let data = js_sys::Object::new();
    for (mime, value) in [("text/html", html), ("text/plain", text)] {
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type(mime);
        let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(value));
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts).map_err(js)?;
        js_sys::Reflect::set(&data, &mime.into(), &blob).map_err(js)?;
    }
    let item = web_sys::ClipboardItem::new_with_record_from_str_to_blob_promise(&data).map_err(js)?;
    let promise = clipboard.write(&js_sys::Array::of1(&item));
    let text = text.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = wasm_bindgen_futures::JsFuture::from(promise).await {
            log::warn!("Rich clipboard write failed; trying plain text: {e:?}");
            if let Err(e) = wasm_bindgen_futures::JsFuture::from(clipboard.write_text(&text)).await {
                log::error!("Plain-text clipboard fallback also failed: {e:?}");
            }
        }
    });
    Ok(())
}

/// Opens `bytes` (a PDF) as an object URL in a hidden iframe and calls the iframe's own
/// `print()` once it has finished loading — the standard way to drive the browser's native print
/// dialog on a PDF without a download or a popup window the browser might block. The blob URL and
/// the iframe are both cleaned up once the print dialog closes, so repeated prints don't leak
/// them.
fn print_pdf(bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;

    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/pdf");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(js)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;

    let iframe: web_sys::HtmlIFrameElement = document.create_element("iframe").map_err(js)?.dyn_into().map_err(|_| "not an iframe")?;
    iframe.style().set_property("display", "none").map_err(js)?;
    document.body().ok_or("no body")?.append_child(&iframe).map_err(js)?;

    // `onload` fires once the PDF has actually rendered inside the iframe;
    // printing before that would show a blank page. Set the handler BEFORE
    // `src` so a fast/cached load can't fire before we're listening.
    let iframe_for_load = iframe.clone();
    let url_for_cleanup = url.clone();
    let on_load = wasm_bindgen::closure::Closure::once_into_js(move || {
        let Some(content_window) = iframe_for_load.content_window() else { return };
        let _ = content_window.print();
        // `afterprint` fires once the print dialog (or print preview) closes, whether the user
        // printed or cancelled — the right moment to release the blob URL and drop the iframe.
        let iframe_for_cleanup = iframe_for_load.clone();
        let on_after_print = wasm_bindgen::closure::Closure::once_into_js(move || {
            web_sys::Url::revoke_object_url(&url_for_cleanup).ok();
            iframe_for_cleanup.remove();
        });
        let _ = content_window.add_event_listener_with_callback("afterprint", on_after_print.unchecked_ref());
    });
    iframe.set_onload(Some(on_load.unchecked_ref()));
    iframe.set_src(&url);
    Ok(())
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("xlsm") => "application/vnd.ms-excel.sheet.macroEnabled.12",
        Some("xlsb") => "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        Some("ods") => "application/vnd.oasis.opendocument.spreadsheet",
        Some("csv") => "text/csv",
        Some("tsv" | "txt") => "text/plain",
        Some("pdf") => "application/pdf",
        Some("html" | "htm") => "text/html",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

/// Trigger a browser save dialog or download of `bytes` named after `name`.

fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;

    let show_picker_fn: Option<js_sys::Function> =
        js_sys::Reflect::get(&window, &JsValue::from_str("showSaveFilePicker")).ok().and_then(|v| v.dyn_into().ok());

    if let Some(picker_fn) = show_picker_fn {
        let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("xlsx").to_ascii_lowercase();
        let dot_ext = format!(".{ext}");
        let mime = mime_for(name);

        let accept_obj = js_sys::Object::new();
        let exts_arr = js_sys::Array::new();
        exts_arr.push(&JsValue::from_str(&dot_ext));
        let _ = js_sys::Reflect::set(&accept_obj, &JsValue::from_str(mime), &exts_arr);

        let type_desc = match ext.as_str() {
            "xlsx" => "Excel Workbook (*.xlsx)",
            "xlsm" => "Excel Macro-Enabled Workbook (*.xlsm)",
            "xlsb" => "Excel Binary Workbook (*.xlsb)",
            "ods" => "OpenDocument Spreadsheet (*.ods)",
            "csv" => "CSV (Comma delimited) (*.csv)",
            "tsv" => "Tab Delimited Text (*.tsv)",
            "txt" => "Plain Text (*.txt)",
            "pdf" => "PDF Document (*.pdf)",
            "html" | "htm" => "HTML Document (*.html)",
            "json" => "JSON Document (*.json)",
            "png" => "PNG Image (*.png)",
            "jpg" | "jpeg" => "JPEG Image (*.jpeg)",
            _ => "Spreadsheet",
        };

        let type_obj = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&type_obj, &JsValue::from_str("description"), &JsValue::from_str(type_desc));
        let _ = js_sys::Reflect::set(&type_obj, &JsValue::from_str("accept"), &accept_obj);

        let types_arr = js_sys::Array::new();
        types_arr.push(&type_obj);

        let picker_opts = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&picker_opts, &JsValue::from_str("suggestedName"), &JsValue::from_str(name));
        let _ = js_sys::Reflect::set(&picker_opts, &JsValue::from_str("types"), &types_arr);

        let prom_val = picker_fn.call1(&window, &picker_opts).map_err(js)?;
        let prom: js_sys::Promise = prom_val.dyn_into().map_err(|_| "not a promise".to_string())?;

        let bytes_vec = bytes.to_vec();
        let name_owned = name.to_string();
        // Anything but the user cancelling (no user gesture, a denied permission, a failed write)
        // falls back to an ordinary download, so a save never fails silently.
        let fallback = move |bytes: &[u8]| {
            if let Some(w) = web_sys::window()
                && let Err(e) = trigger_anchor_download(&w, &name_owned, bytes)
            {
                log::error!("download fallback failed: {e}");
            }
        };
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(prom).await {
                Ok(handle_val) => {
                    if let Err(e) = write_file_handle(&handle_val, &bytes_vec).await {
                        log::error!("failed writing to file handle: {e}");
                        fallback(&bytes_vec);
                    } else {
                        log::info!("Successfully saved file through save file picker");
                    }
                }
                Err(e) => {
                    let err_name = js_sys::Reflect::get(&e, &JsValue::from_str("name")).ok().and_then(|v| v.as_string()).unwrap_or_default();
                    if err_name == "AbortError" {
                        log::info!("Save file dialog cancelled by user");
                    } else {
                        log::warn!("showSaveFilePicker refused ({err_name}); downloading instead");
                        fallback(&bytes_vec);
                    }
                }
            }
        });
        return Ok(());
    }

    // Fallback for browsers without showSaveFilePicker (e.g. Firefox, Safari):
    trigger_anchor_download(&window, name, bytes)
}

async fn write_file_handle(handle: &JsValue, bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let create_fn: js_sys::Function = js_sys::Reflect::get(handle, &JsValue::from_str("createWritable"))
        .map_err(js)?
        .dyn_into()
        .map_err(|_| "createWritable is not a function".to_string())?;
    let stream_prom: js_sys::Promise = create_fn.call0(handle).map_err(js)?.dyn_into().map_err(|_| "not a promise".to_string())?;
    let stream = wasm_bindgen_futures::JsFuture::from(stream_prom).await.map_err(js)?;

    let write_fn: js_sys::Function =
        js_sys::Reflect::get(&stream, &JsValue::from_str("write")).map_err(js)?.dyn_into().map_err(|_| "write is not a function".to_string())?;
    let uint8 = js_sys::Uint8Array::from(bytes);
    let write_prom: js_sys::Promise = write_fn.call1(&stream, &uint8).map_err(js)?.dyn_into().map_err(|_| "not a promise".to_string())?;
    wasm_bindgen_futures::JsFuture::from(write_prom).await.map_err(js)?;

    let close_fn: js_sys::Function =
        js_sys::Reflect::get(&stream, &JsValue::from_str("close")).map_err(js)?.dyn_into().map_err(|_| "close is not a function".to_string())?;
    let close_prom: js_sys::Promise = close_fn.call0(&stream).map_err(js)?.dyn_into().map_err(|_| "not a promise".to_string())?;
    wasm_bindgen_futures::JsFuture::from(close_prom).await.map_err(js)?;
    Ok(())
}

fn trigger_anchor_download(window: &web_sys::Window, name: &str, bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let document = window.document().ok_or("no document")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime_for(name));
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(js)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a").map_err(js)?.dyn_into().map_err(|_| "not an anchor".to_string())?;
    a.set_href(&url);
    a.set_download(name);
    a.style().set_property("display", "none").map_err(js)?;
    let body = document.body().ok_or("no body")?;
    body.append_child(&a).map_err(js)?;
    a.click();
    a.remove();
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        web_sys::Url::revoke_object_url(&url).ok();
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000).map_err(js)?;
    Ok(())
}
