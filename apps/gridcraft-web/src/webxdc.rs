//! Running as a webxdc app (Delta Chat and other messengers): everyone in the chat edits one
//! workbook ([`crate::collab`]). Edits travel as `sendUpdate` payloads `{y: <base64 Yjs update>}`,
//! which the messenger stores and replays on every start, and also over the realtime channel
//! where the messenger has one, so collaborators online at the same time see them at once.
//! Opening a file replaces the shared workbook; Save and Export hand the file to the chat.

use std::cell::RefCell;
use std::rc::Rc;

use gridcraft_engine::Session;
use gridcraft_engine::io::{base64_decode, base64_encode, open_bytes};
use gridcraft_ui_egui::{Inbox, Services};
use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};

use crate::collab::Collab;

/// The realtime channel's limit per message; larger updates go by `sendUpdate` alone.
const REALTIME_MAX: usize = 128_000;

pub struct Webxdc {
    api: JsValue,
    channel: Option<JsValue>,
    collab: Collab,
    /// Updates received since the last frame.
    received: Rc<RefCell<Vec<Vec<u8>>>>,
}

/// `window.webxdc`, when the page runs as a webxdc app (`webxdc.js` loaded).
pub fn api() -> Option<JsValue> {
    let api = Reflect::get(&web_sys::window()?.into(), &"webxdc".into()).ok()?;
    api.is_object().then_some(api)
}

impl Webxdc {
    /// Opens the shared workbook in `session` and saves files to the chat.
    pub fn new(api: JsValue, session: &mut Session, services: &mut Services) -> Option<Webxdc> {
        if let Some(name) = Reflect::get(&api, &"selfName".into()).ok().and_then(|n| n.as_string()) {
            session.prefs.user_name = name;
        }
        // Unique to this run: the Yjs client id.
        let client = (js_sys::Math::random() * (1u64 << 53) as f64) as u64;
        let collab = Collab::new(session, client)?;
        let to_chat = api.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| {
            let name = std::path::Path::new(name).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| name.to_string());
            let file = object(&[("name", name.as_str().into()), ("base64", base64_encode(bytes).into())]);
            if let Err(e) = call(&to_chat, "sendToChat", &[object(&[("file", file)])]) {
                log::error!("couldn't send {name} to the chat: {e:?}");
            }
        }));
        services.open_url = None;
        Some(Webxdc { api, channel: None, collab, received: Rc::default() })
    }

    /// Starts hearing updates: the stored ones first, then live ones.
    pub fn listen(&mut self, ctx: egui::Context) {
        let received = self.received.clone();
        let repaint = ctx.clone();
        let on_update = Closure::<dyn FnMut(JsValue)>::new(move |update: JsValue| {
            let y = Reflect::get(&update, &"payload".into()).and_then(|p| Reflect::get(&p, &"y".into())).ok().and_then(|y| y.as_string());
            if let Some(bytes) = y.as_deref().and_then(base64_decode) {
                received.borrow_mut().push(bytes);
                repaint.request_repaint();
            }
        });
        match call(&self.api, "setUpdateListener", &[on_update.as_ref().clone(), 0.into()]) {
            // Lives as long as the page.
            Ok(_) => on_update.forget(),
            Err(e) => log::error!("couldn't listen for shared edits: {e:?}"),
        }
        let has_realtime = Reflect::get(&self.api, &"joinRealtimeChannel".into()).is_ok_and(|f| f.is_function());
        if let Some(channel) = has_realtime.then(|| call(&self.api, "joinRealtimeChannel", &[]).ok()).flatten() {
            let received = self.received.clone();
            let on_data = Closure::<dyn FnMut(JsValue)>::new(move |data: JsValue| {
                if let Some(data) = data.dyn_ref::<Uint8Array>() {
                    received.borrow_mut().push(data.to_vec());
                    ctx.request_repaint();
                }
            });
            if call(&channel, "setListener", &[on_data.as_ref().clone()]).is_ok() {
                on_data.forget();
                self.channel = Some(channel);
            }
        }
    }

    /// Before the frame: applies what collaborators sent, and opens picked or dropped files for
    /// everyone.
    pub fn before(&mut self, session: &mut Session, inbox: &Inbox) {
        let received = std::mem::take(&mut *self.received.borrow_mut());
        if !received.is_empty() {
            self.collab.remote(session, received);
        }
        let files = std::mem::take(&mut *inbox.lock().unwrap_or_else(|e| e.into_inner()));
        for (name, bytes) in files {
            match open_bytes(&name, &bytes) {
                Ok((wb, _)) => {
                    if let Some(update) = self.collab.open(session, &name, wb) {
                        self.send(&update);
                    }
                }
                // The app opens it as usual and says what's wrong.
                Err(_) => inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes)),
            }
        }
    }

    /// After the frame: sends the edits made in it.
    pub fn after(&mut self, session: &mut Session) {
        if let Some(update) = self.collab.local(session) {
            self.send(&update);
        }
    }

    fn send(&self, update: &[u8]) {
        let payload = object(&[("y", base64_encode(update).into())]);
        if let Err(e) = call(&self.api, "sendUpdate", &[object(&[("payload", payload)]), "".into()]) {
            log::error!("couldn't share an edit: {e:?}");
        }
        if let Some(channel) = &self.channel
            && update.len() <= REALTIME_MAX
            && let Err(e) = call(channel, "send", &[Uint8Array::from(update).into()])
        {
            log::warn!("realtime send failed: {e:?}");
        }
    }
}

fn call(this: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f: Function = Reflect::get(this, &method.into())?.dyn_into()?;
    f.apply(this, &args.iter().collect::<Array>())
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (key, value) in fields {
        let _ = Reflect::set(&o, &(*key).into(), value);
    }
    o.into()
}
