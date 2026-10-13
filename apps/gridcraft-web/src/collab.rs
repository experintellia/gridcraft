//! Shared editing for the webxdc build: everyone in a chat edits one workbook.
//!
//! Each command that changes the shared workbook ([`Session::edits`]) becomes an op appended to a
//! Yjs array (`yrs`). Peers exchange the array's updates through the messenger; Yjs gives every
//! peer the same order of ops, and every peer's workbook is that order replayed from a blank
//! workbook. When a collaborator's ops land before ops already applied here, the workbook rolls
//! back to a snapshot and replays from there.
//!
//! A command that reads session-local state (the clipboard, the format painter) wouldn't do the
//! same thing elsewhere, so its op carries the workbook it left instead, as do opening a file and
//! a command given a password (which never leaves this app).
//! Undo is shared too: it takes back the last change, whoever made it.

use std::sync::Arc;

use gridcraft_engine::model::Workbook;
use gridcraft_engine::{DocState, HistoryEntry, Selection, Session, UNDO_LIMIT};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use yrs::updates::decoder::Decode as _;
use yrs::{Any, Array as _, ArrayRef, Doc, Out, ReadTxn as _, Transact as _, Update};

/// Commands that read the clipboard or the format painter.
const LOCAL_STATE: &[&str] = &["edit.paste", "edit.pasteSpecial", "home.insertCopiedCells", "edit.formatPainter"];
/// A snapshot every this many ops, keeping the last `SNAPSHOTS`.
// ponytail: a reorder older than 256 ops replays the whole log; checkpoint the log into a
// workbook op if long-lived chats make startup or rollbacks slow.
const SNAPSHOT_EVERY: usize = 16;
const SNAPSHOTS: usize = 16;

#[derive(Serialize, Deserialize)]
struct Op {
    /// Command id and parameters.
    c: String,
    #[serde(default)]
    p: Json,
    /// The active sheet and selection it ran with, and who ran it.
    s: usize,
    sel: Selection,
    #[serde(default)]
    a: String,
    /// The workbook the command left, when the command can't run again elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wb: Option<Workbook>,
}

pub struct Collab {
    ydoc: Doc,
    ops: ArrayRef,
    /// The ops replayed into the shared workbook so far, in order.
    applied: Vec<Arc<str>>,
    /// The shared document before any op, and after every `SNAPSHOT_EVERY` ops.
    base: DocState,
    snapshots: Vec<(usize, DocState)>,
    /// [`DocState::uid`] of the shared document.
    uid: u64,
}

impl Collab {
    /// Opens the shared workbook (blank until ops arrive) and records edits from now on.
    /// `client_id` must be unique to this run of the app.
    pub fn new(s: &mut Session, client_id: u64) -> Option<Collab> {
        let i = s.new_workbook();
        let base = s.documents().get(i)?.clone();
        s.edits = Some(vec![]);
        let ydoc = Doc::with_client_id(client_id);
        let ops = ydoc.get_or_insert_array("ops");
        Some(Collab { ydoc, ops, applied: vec![], uid: base.uid, base, snapshots: vec![] })
    }

    /// [`DocState::uid`] of the shared document.
    pub fn doc(&self) -> u64 {
        self.uid
    }

    /// The edits made here since the last call, as an update for the collaborators.
    pub fn local(&mut self, s: &mut Session) -> Option<Vec<u8>> {
        let edits = s.edits.as_mut().map(std::mem::take).unwrap_or_default();
        let author = &s.prefs.user_name;
        let ops: Vec<Arc<str>> = edits
            .into_iter()
            .filter(|e| e.doc == self.uid)
            .filter_map(|e| {
                let p = gridcraft_engine::cmd::journal_params(&e.params);
                let wb = (LOCAL_STATE.contains(&e.id.as_str()) || p != e.params).then(|| Workbook::clone(&e.after));
                serde_json::to_string(&Op { c: e.id, p, s: e.sheet, sel: e.selection, a: author.clone(), wb }).ok()
            })
            .map(Arc::from)
            .collect();
        if ops.is_empty() {
            return None;
        }
        let before = self.applied.len();
        let update = self.append(&ops);
        // Already run here: the shared workbook is the ops so far.
        self.applied.extend(ops);
        if self.applied.len() / SNAPSHOT_EVERY > before / SNAPSHOT_EVERY {
            self.snapshot(s, self.applied.len());
        }
        Some(update)
    }

    /// Opens a workbook for everyone, replacing the shared one.
    pub fn open(&mut self, s: &mut Session, name: &str, wb: Workbook) -> Option<Vec<u8>> {
        let sel = Selection::at(wb.active().map(|sh| sh.view_active).unwrap_or_default());
        let op =
            Op { c: "file.open".into(), p: serde_json::json!({"name": name}), s: wb.active_sheet, sel, a: s.prefs.user_name.clone(), wb: Some(wb) };
        let update = self.append(&[serde_json::to_string(&op).ok()?.into()]);
        self.reconcile(s);
        Some(update)
    }

    /// Applies collaborators' updates (and this app's own, which are no-ops).
    pub fn remote(&mut self, s: &mut Session, updates: Vec<Vec<u8>>) {
        {
            let mut txn = self.ydoc.transact_mut();
            for bytes in updates {
                match Update::decode_v1(&bytes) {
                    Ok(update) => {
                        if let Err(e) = txn.apply_update(update) {
                            log::warn!("ignored a shared update: {e}");
                        }
                    }
                    Err(e) => log::warn!("ignored an unreadable shared update: {e}"),
                }
            }
        }
        self.reconcile(s);
    }

    fn append(&mut self, ops: &[Arc<str>]) -> Vec<u8> {
        let mut txn = self.ydoc.transact_mut();
        let sv = txn.state_vector();
        for op in ops {
            self.ops.push_back(&mut txn, op.as_ref());
        }
        txn.encode_state_as_update_v1(&sv)
    }

    /// Brings the shared workbook in line with the ops' order, keeping what is this user's own:
    /// the document and sheet they look at, their selection, clipboard and name.
    fn reconcile(&mut self, s: &mut Session) {
        let ops: Vec<Arc<str>> = {
            let txn = self.ydoc.transact();
            self.ops.iter(&txn).filter_map(|v| if let Out::Any(Any::String(op)) = v { Some(op) } else { None }).collect()
        };
        let same = self.applied.iter().zip(&ops).take_while(|(a, b)| a == b).count();
        if same == self.applied.len() && same == ops.len() {
            return;
        }
        let Some(i) = s.documents().iter().position(|d| d.uid == self.uid) else { return };
        let active = s.active_index();
        s.set_active(i);
        let Some((sheet, selection, revision)) = s.active().map(|d| (d.wb.active_sheet, d.selection.clone(), d.revision)) else { return };
        let (clipboard, painter, edits, author) = (s.clipboard.take(), s.format_painter.take(), s.edits.take(), s.prefs.user_name.clone());

        if same < self.applied.len() {
            self.snapshots.retain(|(n, _)| *n <= same);
            let (n, doc) = self.snapshots.last().cloned().unwrap_or_else(|| (0, self.base.clone()));
            if let Some(d) = s.active_mut() {
                *d = doc;
            }
            self.applied.truncate(n);
        }
        for op in ops.get(self.applied.len()..).unwrap_or_default() {
            replay(s, op);
            self.applied.push(op.clone());
            if self.applied.len().is_multiple_of(SNAPSHOT_EVERY) {
                self.snapshot(s, self.applied.len());
            }
        }

        if let Some(d) = s.active_mut() {
            let sheet = sheet.min(d.wb.sheets.len().saturating_sub(1));
            if d.wb.active_sheet != sheet {
                Arc::make_mut(&mut d.wb).active_sheet = sheet;
            }
            d.selection = selection;
            // A rollback can bring an older revision back; the UI must see a new one.
            d.revision = d.revision.max(revision + 1);
        }
        (s.clipboard, s.format_painter, s.edits, s.prefs.user_name) = (clipboard, painter, edits, author);
        s.take_ui_requests();
        s.set_active(active);
    }

    fn snapshot(&mut self, s: &Session, n: usize) {
        if let Some(d) = s.documents().iter().find(|d| d.uid == self.uid) {
            self.snapshots.push((n, d.clone()));
            if self.snapshots.len() > SNAPSHOTS {
                self.snapshots.remove(0);
            }
        }
    }
}

/// Runs one op on the active (shared) document, as its author ran it.
fn replay(s: &mut Session, json: &str) {
    let Ok(op) = serde_json::from_str::<Op>(json) else {
        log::warn!("skipped an unreadable shared op");
        return;
    };
    let Some(d) = s.active_mut() else { return };
    if op.s < d.wb.sheets.len() && d.wb.active_sheet != op.s {
        Arc::make_mut(&mut d.wb).active_sheet = op.s;
    }
    d.selection = op.sel;
    match op.wb {
        // As `Session::execute` records an undoable command.
        Some(wb) => {
            d.undo.push(HistoryEntry { label: op.c, wb: d.wb.clone(), selection: d.selection.clone() });
            if d.undo.len() > UNDO_LIMIT {
                d.undo.remove(0);
            }
            d.redo.clear();
            d.wb = Arc::new(wb);
            d.calc.rebuild(&d.wb);
            d.revision += 1;
        }
        None => {
            s.prefs.user_name = op.a;
            if let Err(e) = s.execute(&op.c, op.p) {
                log::info!("shared `{}` did nothing here: {e}", op.c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn peer(client: u64) -> (Session, Collab) {
        let mut s = Session::new();
        let c = Collab::new(&mut s, client).unwrap();
        (s, c)
    }

    fn run(s: &mut Session, id: &str, params: Json) {
        s.execute(id, params).unwrap();
    }

    fn value(s: &mut Session, cell: &str) -> Json {
        s.execute("cell.get", json!({"cell": cell})).unwrap()["value"].clone()
    }

    fn workbook(s: &Session) -> Json {
        serde_json::to_value(&*s.active().unwrap().wb).unwrap()
    }

    /// Delivers each peer's pending edits to everyone, itself included (as webxdc does).
    fn sync(peers: &mut [&mut (Session, Collab)]) {
        let updates: Vec<Vec<u8>> = peers.iter_mut().filter_map(|(s, c)| c.local(s)).collect();
        for (s, c) in peers.iter_mut() {
            c.remote(s, updates.clone());
        }
    }

    #[test]
    fn concurrent_edits_converge() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "A1", "input": "1"}));
        run(&mut b.0, "cell.set", json!({"cell": "A1", "input": "2"}));
        run(&mut b.0, "cell.set", json!({"cell": "B1", "input": "=A1*10"}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(workbook(&a.0), workbook(&b.0));
        let (a1, b1) = (value(&mut a.0, "A1"), value(&mut a.0, "B1"));
        assert_eq!(b1.as_f64(), a1.as_f64().map(|v| v * 10.0));
    }

    #[test]
    fn a_reorder_behind_snapshots_rolls_back_and_replays() {
        // Yjs orders concurrent ops by client id: either side may have to roll back.
        for (ida, idb) in [(1, 2), (2, 1)] {
            let (mut a, mut b) = (peer(ida), peer(idb));
            // B sees A's first edits, then A's next ones reach B late, after B made its own.
            let mut late = vec![];
            for r in 1..=40 {
                run(&mut a.0, "cell.set", json!({"cell": format!("A{r}"), "input": r.to_string()}));
                if r <= 20 {
                    sync(&mut [&mut a, &mut b]);
                } else {
                    late.extend(a.1.local(&mut a.0));
                }
            }
            run(&mut b.0, "cell.set", json!({"cell": "B1", "input": "=SUM(A1:A40)"}));
            run(&mut b.0, "selection.set", json!({"range": "A1:A40"}));
            run(&mut b.0, "home.bold", json!({}));
            sync(&mut [&mut a, &mut b]);
            b.1.remote(&mut b.0, late);
            assert_eq!(workbook(&a.0), workbook(&b.0));
            assert_eq!(value(&mut b.0, "B1").as_f64(), Some(820.0));
            // Each keeps its own selection.
            assert_eq!(b.0.active().unwrap().selection.ranges[0].a1(), "A1:A40");
        }
    }

    #[test]
    fn paste_ships_the_workbook_it_left() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "A1", "input": "7"}));
        run(&mut a.0, "edit.copy", json!({"range": "A1"}));
        run(&mut a.0, "edit.paste", json!({"at": "C3"}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut b.0, "C3").as_f64(), Some(7.0));
        assert_eq!(workbook(&a.0), workbook(&b.0));
        // The receiver's clipboard is its own.
        assert!(b.0.clipboard.is_none());
    }

    #[test]
    fn passwords_stay_here() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "review.protectSheet", json!({"password": "hunter2"}));
        let update = a.1.local(&mut a.0).unwrap();
        assert!(!String::from_utf8_lossy(&update).contains("hunter2"));
        b.1.remote(&mut b.0, vec![update]);
        assert_eq!(workbook(&a.0), workbook(&b.0));
    }

    #[test]
    fn undo_is_shared() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "A1", "input": "1"}));
        sync(&mut [&mut a, &mut b]);
        run(&mut b.0, "edit.undo", json!({}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut a.0, "A1"), Json::Null);
        assert_eq!(workbook(&a.0), workbook(&b.0));
    }

    #[test]
    fn opening_a_file_replaces_the_shared_workbook() {
        let (mut a, mut b) = (peer(1), peer(2));
        let mut wb = Workbook::new();
        wb.sheets.push(Arc::new(gridcraft_engine::model::Sheet::new("Data")));
        let update = a.1.open(&mut a.0, "data.xlsx", wb).unwrap();
        b.1.remote(&mut b.0, vec![update]);
        assert_eq!(b.0.active().unwrap().wb.sheets.len(), 2);
        assert_eq!(b.0.active().unwrap().uid, b.1.doc());
        assert_eq!(workbook(&a.0), workbook(&b.0));
    }

    #[test]
    fn garbage_updates_are_ignored() {
        let mut a = peer(1);
        a.1.remote(&mut a.0, vec![vec![], vec![0xff; 40]]);
        assert!(a.0.active().is_some());
    }
}
