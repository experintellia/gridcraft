//! Running as a webxdc app (Delta Chat and other messengers): everyone in the chat edits one
//! workbook ([`crate::collab`]). Edits travel as `sendUpdate` payloads `{y: <base64 Yjs update>}`,
//! which the messenger stores and replays on every start, and also over the realtime channel
//! where the messenger has one, so collaborators online at the same time see them at once.
//! Opening a file replaces the shared workbook (after a warning, once there is one); its name,
//! or one given on File › Info (`file.rename`), titles the workbook for everyone and names the
//! instance in the chat (`sendUpdate`'s `document`). Save and Export hand the file to the chat. The app is that one workbook: no New, Open, samples or window
//! commands ([`Services::single_document`]). Until someone starts it, a start screen offers a
//! blank workbook or a file.
//!
//! The realtime channel also carries each member's sheet and selection (presence), drawn on the
//! others' grids; it isn't stored, so it needs a messenger with realtime channels.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gridcraft_engine::io::{base64_decode, base64_encode, open_bytes};
use gridcraft_engine::{Selection, Session};
use gridcraft_ui_egui::{Inbox, Peer, Services, SheetApp, Start};
use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};

use crate::collab::Collab;

/// The realtime channel's limit per message; larger updates go by `sendUpdate` alone.
const REALTIME_MAX: usize = 128_000;
/// Realtime messages start with one of these.
const TAG_UPDATE: u8 = 0;
const TAG_PRESENCE: u8 = 1;
/// A moving selection is sent at most this often; a still one is repeated every `HEARTBEAT_MS`,
/// and a member not heard from for `GONE_MS` disappears.
const PRESENCE_EVERY_MS: f64 = 100.0;
const HEARTBEAT_MS: f64 = 5_000.0;
const GONE_MS: f64 = 15_000.0;
/// Caps on what one member's presence can make every frame draw.
const MAX_PEERS: usize = 50;
const MAX_NAME: usize = 40;
const MAX_RANGES: usize = 64;

/// Where a member is: the shared workbook's sheet and their selection.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Presence {
    /// `selfAddr`: who it is.
    id: String,
    name: String,
    sheet: usize,
    sel: Selection,
}

pub struct Webxdc {
    api: JsValue,
    channel: Option<JsValue>,
    collab: Collab,
    ctx: Option<egui::Context>,
    me: String,
    /// Received since the last frame.
    received: Rc<RefCell<Vec<Vec<u8>>>>,
    /// The messenger has handed over the updates it stored: an empty log now means nobody has
    /// started the workbook.
    replayed: Rc<Cell<bool>>,
    /// Files dropped on the window: once the workbook exists, each asks before replacing it.
    pub dropped: Inbox,
    heard: Rc<RefCell<Vec<Presence>>>,
    /// The other members online, by `selfAddr`, with when they were last heard.
    peers: HashMap<String, (Presence, f64)>,
    /// What was last sent, and when.
    sent: Option<(Presence, f64)>,
    /// Someone new is here: tell them where this member is.
    announce: bool,
    /// The workbook's name as last sent to the messenger.
    named: String,
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
        // The messenger's sandbox has no print dialog.
        services.print = None;
        services.single_document = true;
        let me = Reflect::get(&api, &"selfAddr".into()).ok().and_then(|a| a.as_string()).unwrap_or_default();
        let named = collab.title(session);
        Some(Webxdc {
            named,
            api,
            channel: None,
            collab,
            ctx: None,
            me,
            received: Rc::default(),
            replayed: Rc::default(),
            dropped: Inbox::default(),
            heard: Rc::default(),
            peers: HashMap::new(),
            sent: None,
            announce: false,
        })
    }

    /// Starts hearing updates: the stored ones first, then live ones.
    pub fn listen(&mut self, ctx: egui::Context) {
        self.ctx = Some(ctx.clone());
        let received = self.received.clone();
        let repaint = ctx.clone();
        // Stored updates handed over, and their bytes, for the startup report.
        let stored = Rc::new(Cell::new((0usize, 0usize)));
        let count = stored.clone();
        let on_update = Closure::<dyn FnMut(JsValue)>::new(move |update: JsValue| {
            let y = Reflect::get(&update, &"payload".into()).and_then(|p| Reflect::get(&p, &"y".into())).ok().and_then(|y| y.as_string());
            if let Some(bytes) = y.as_deref().and_then(base64_decode) {
                let (n, size) = count.get();
                count.set((n + 1, size + bytes.len()));
                received.borrow_mut().push(bytes);
                repaint.request_repaint();
            }
        });
        match call(&self.api, "setUpdateListener", &[on_update.as_ref().clone(), 0.into()]) {
            Ok(done) => {
                // Lives as long as the page.
                on_update.forget();
                // The promise resolves once the stored updates are in; a messenger without one
                // has handed them over already.
                let (replayed, repaint) = (self.replayed.clone(), ctx.clone());
                match done.dyn_into::<js_sys::Promise>() {
                    Ok(done) => wasm_bindgen_futures::spawn_local(async move {
                        let _ = wasm_bindgen_futures::JsFuture::from(done).await;
                        let (n, size) = stored.get();
                        crate::perf::mark(format!("messenger handed over {n} stored updates ({} KB)", size / 1000));
                        replayed.set(true);
                        repaint.request_repaint();
                    }),
                    Err(_) => replayed.set(true),
                }
            }
            Err(e) => {
                log::error!("couldn't listen for shared edits: {e:?}");
                self.replayed.set(true);
            }
        }
        let has_realtime = Reflect::get(&self.api, &"joinRealtimeChannel".into()).is_ok_and(|f| f.is_function());
        if let Some(channel) = has_realtime.then(|| call(&self.api, "joinRealtimeChannel", &[]).ok()).flatten() {
            let (received, heard) = (self.received.clone(), self.heard.clone());
            let on_data = Closure::<dyn FnMut(JsValue)>::new(move |data: JsValue| {
                let Some(data) = data.dyn_ref::<Uint8Array>().map(Uint8Array::to_vec) else { return };
                match data.split_first() {
                    Some((&TAG_UPDATE, update)) => received.borrow_mut().push(update.to_vec()),
                    Some((&TAG_PRESENCE, json)) => match serde_json::from_slice(json) {
                        Ok(presence) => heard.borrow_mut().push(presence),
                        Err(_) => return,
                    },
                    _ => return,
                }
                ctx.request_repaint();
            });
            if call(&channel, "setListener", &[on_data.as_ref().clone()]).is_ok() {
                on_data.forget();
                self.channel = Some(channel);
            }
        }
    }

    /// Before the frame: applies what collaborators sent, and opens picked files (and dropped
    /// ones, once confirmed) for everyone.
    pub fn before(&mut self, app: &mut SheetApp, inbox: &Inbox) {
        self.hear(app);
        let received = std::mem::take(&mut *self.received.borrow_mut());
        if !received.is_empty() {
            let (n, size, t) = (received.len(), received.iter().map(Vec::len).sum::<usize>(), crate::perf::now());
            self.collab.remote(&mut app.session, received);
            crate::perf::mark(format!("applied {n} updates ({} KB) in {:.0} ms", size / 1000, crate::perf::now() - t));
        }
        let dropped = std::mem::take(&mut *self.dropped.lock().unwrap_or_else(|e| e.into_inner()));
        let mut files = std::mem::take(&mut *inbox.lock().unwrap_or_else(|e| e.into_inner()));
        for (name, bytes) in dropped {
            // Before the chat's updates are in, a workbook may well exist: ask.
            if self.collab.is_empty() && self.replayed.get() {
                files.push((name, bytes));
            } else {
                app.open_dialog("replaceWorkbook", serde_json::json!({"name": name, "base64": base64_encode(&bytes)}));
            }
        }
        for (name, bytes) in files {
            match open_bytes(&name, &bytes) {
                Ok((wb, _)) => {
                    if let Some(update) = self.collab.open(&mut app.session, &name, wb) {
                        self.send(&app.session, &update);
                    }
                    app.backstage = None;
                }
                // ponytail: a password-protected file isn't asked for its password here.
                Err(e) => app.message = Some(("GridCraft".into(), gridcraft_ui_egui::clean_error(&e.to_string()))),
            }
        }
        self.start(app);
    }

    /// After the frame: sends the edits made in it, and where this member is.
    pub fn after(&mut self, app: &mut SheetApp) {
        if let Some(update) = self.collab.local(&mut app.session) {
            self.send(&app.session, &update);
        }
        self.start(app);
        self.tell(&app.session);
    }

    /// The start screen shows while nobody has started the shared workbook; a blank one chosen
    /// there starts it for everyone.
    fn start(&mut self, app: &mut SheetApp) {
        app.start = if !self.collab.is_empty() {
            Start::Off
        } else if !self.replayed.get() {
            Start::Loading
        } else if app.start == Start::Blank {
            if let Some(update) = self.collab.create(&mut app.session) {
                self.send(&app.session, &update);
            }
            Start::Off
        } else {
            Start::Shown
        };
    }

    /// Takes in where the others are, and drops those gone quiet.
    fn hear(&mut self, app: &mut SheetApp) {
        let now = js_sys::Date::now();
        for presence in std::mem::take(&mut *self.heard.borrow_mut()) {
            // Hostile input: bound what one member can make every frame draw.
            if presence.id != self.me
                && crate::collab::valid(&presence.sel)
                && (self.peers.len() < MAX_PEERS || self.peers.contains_key(&presence.id))
            {
                let mut presence = presence;
                presence.name = presence.name.chars().take(MAX_NAME).collect();
                presence.sel.ranges.truncate(MAX_RANGES);
                self.announce |= !self.peers.contains_key(&presence.id);
                self.peers.insert(presence.id.clone(), (presence, now));
            }
        }
        self.peers.retain(|_, (_, seen)| now - *seen < GONE_MS);
        let doc = self.collab.doc();
        app.peers = self
            .peers
            .values()
            .map(|(p, _)| Peer {
                name: p.name.clone(),
                doc,
                sheet: p.sheet,
                selection: p.sel.clone(),
                color: p.id.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)),
            })
            .collect();
    }

    /// Sends this member's sheet and selection when they moved (throttled), when someone new
    /// arrived, and as a heartbeat.
    fn tell(&mut self, session: &Session) {
        let (Some(channel), Some(ctx)) = (&self.channel, &self.ctx) else { return };
        // Wake for the heartbeat and to drop members who left.
        ctx.request_repaint_after(std::time::Duration::from_millis(1_000));
        let Some(d) = session.active().filter(|d| d.uid == self.collab.doc()) else { return };
        let here = Presence { id: self.me.clone(), name: session.prefs.user_name.clone(), sheet: d.wb.active_sheet, sel: d.selection.clone() };
        let now = js_sys::Date::now();
        let due = match &self.sent {
            None => true,
            Some((sent, at)) => self.announce || now - at >= HEARTBEAT_MS || (*sent != here && now - at >= PRESENCE_EVERY_MS),
        };
        if !due {
            if self.sent.as_ref().is_some_and(|(sent, _)| *sent != here) {
                ctx.request_repaint_after(std::time::Duration::from_millis(PRESENCE_EVERY_MS as u64));
            }
            return;
        }
        let Ok(json) = serde_json::to_vec(&here) else { return };
        let message = Uint8Array::from([&[TAG_PRESENCE], json.as_slice()].concat().as_slice());
        match call(channel, "send", &[message.into()]) {
            Ok(_) => {
                self.sent = Some((here, now));
                self.announce = false;
            }
            Err(e) => log::warn!("couldn't share the selection: {e:?}"),
        }
    }

    /// Sends `update`, and the workbook's name when it changed (opened or renamed): the messenger
    /// shows it next to the app's (`document`; the latest one wins).
    fn send(&mut self, s: &Session, update: &[u8]) {
        let payload = object(&[("y", base64_encode(update).into())]);
        let mut fields = vec![("payload", payload)];
        let title = self.collab.title(s);
        if title != self.named {
            self.named = title;
            let stem = std::path::Path::new(&self.named).file_stem().map_or_else(|| self.named.clone(), |s| s.to_string_lossy().to_string());
            fields.push(("document", stem.into()));
        }
        if let Err(e) = call(&self.api, "sendUpdate", &[object(&fields), "".into()]) {
            log::error!("couldn't share an edit: {e:?}");
        }
        if let Some(channel) = &self.channel
            && update.len() < REALTIME_MAX
            && let Err(e) = call(channel, "send", &[Uint8Array::from([&[TAG_UPDATE], update].concat().as_slice()).into()])
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
