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
//!
//! Undo and Redo take back only this member's own changes, on top of what everyone did since.
//! Each member keeps their own steps (the workbook before and after each change they made, opening
//! a file included). Undoing one appends an op carrying what the change did, as patches over the
//! workbook's JSON; every member puts the earlier values back wherever the workbook still holds the
//! values the change left. If anyone has changed one of the same things since (a cell, a
//! property, a whole list such as the merges or the sheets), the undo is refused as a whole and
//! the workbook stays as it is; the step moves to the redo list all the same. Formula results
//! aren't compared (they're recalculated), and the style table only grows, so the style ids an
//! undo puts back still name the same styles. Redo is the same patches the other way.
//!
//! An empty log means nobody has started the workbook yet; starting a blank one is an op that
//! changes nothing ([`Collab::create`]).

use std::sync::Arc;

use gridcraft_engine::core::{CellRef, MAX_COLS, MAX_ROWS, Value};
use gridcraft_engine::model::{CellStore, Workbook};
use gridcraft_engine::{DocState, HistoryEntry, Selection, Session, UNDO_LIMIT, find_command};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use yrs::updates::decoder::Decode as _;
use yrs::{Any, Array as _, ArrayRef, Doc, Out, ReadTxn as _, Transact as _, Update};

/// The op that starts a blank shared workbook: it changes nothing, but the chat has a workbook.
const NEW: &str = "file.new";
/// Commands that read the clipboard or the format painter.
const LOCAL_STATE: &[&str] = &["edit.paste", "edit.pasteSpecial", "home.insertCopiedCells", "edit.formatPainter"];
/// A snapshot every this many ops, keeping the last `SNAPSHOTS`.
// ponytail: a reorder older than 256 ops replays the whole log; checkpoint the log into a
// workbook op if long-lived chats make startup or rollbacks slow.
const SNAPSHOT_EVERY: usize = 16;
const SNAPSHOTS: usize = 16;
/// Deepest path a patch may have; a remote op with a deeper one is refused.
const MAX_PATH: usize = 32;
/// Longest document title a remote `file.open` may set.
const MAX_TITLE: usize = 255;

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
    /// Undo and redo: what to change back ([`patched`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    d: Vec<Patch>,
}

/// Where the workbook's JSON ([`tree`]) holds `f` at path `p`, put `t`. A value is `[value]`;
/// none means the key isn't there.
#[derive(Clone, Serialize, Deserialize)]
struct Patch {
    p: Vec<Json>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    f: Option<[Json; 1]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    t: Option<[Json; 1]>,
}

/// One of this member's changes, for their undo and redo.
struct Step {
    label: String,
    selection: Selection,
    before: Arc<Workbook>,
    after: Arc<Workbook>,
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
    /// This member's own undo and redo steps.
    undo: Vec<Step>,
    redo: Vec<Step>,
}

impl Collab {
    /// Opens the shared workbook (blank until ops arrive) and records edits from now on.
    /// `client_id` must be unique to this run of the app.
    pub fn new(s: &mut Session, client_id: u64) -> Option<Collab> {
        let i = s.new_workbook();
        let base = s.documents().get(i)?.clone();
        s.edits = Some(vec![]);
        s.host_undo = Some(base.uid);
        let ydoc = Doc::with_client_id(client_id);
        let ops = ydoc.get_or_insert_array("ops");
        Some(Collab { ydoc, ops, applied: vec![], uid: base.uid, base, snapshots: vec![], undo: vec![], redo: vec![] })
    }

    /// [`DocState::uid`] of the shared document.
    pub fn doc(&self) -> u64 {
        self.uid
    }

    /// Whether nobody has started the shared workbook: no ops yet.
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }

    /// Starts the shared workbook, blank, for everyone.
    pub fn create(&mut self, s: &mut Session) -> Option<Vec<u8>> {
        let op =
            Op { c: NEW.into(), p: Json::Null, s: 0, sel: Selection::at(Default::default()), a: s.prefs.user_name.clone(), wb: None, d: Vec::new() };
        let update = self.append(&[serde_json::to_string(&op).ok()?.into()]);
        self.reconcile(s, usize::MAX);
        Some(update)
    }

    /// The edits made here since the last call, as an update for the collaborators.
    pub fn local(&mut self, s: &mut Session) -> Option<Vec<u8>> {
        let edits: Vec<_> = s.edits.as_mut().map(std::mem::take).unwrap_or_default().into_iter().filter(|e| e.doc == self.uid).collect();
        if edits.is_empty() {
            return None;
        }
        let author = s.prefs.user_name.clone();
        let mut ops: Vec<Arc<str>> = vec![];
        // The first undo or redo: it hasn't changed the workbook here yet.
        let mut stale = None;
        for e in edits {
            let undo = e.id == "edit.undo";
            if undo || e.id == "edit.redo" {
                let steps = e.params.get("steps").and_then(Json::as_u64).unwrap_or(1).max(1);
                for _ in 0..steps {
                    let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
                    let Some(step) = from.pop() else { break };
                    let d = if undo { changes(&step.after, &step.before) } else { changes(&step.before, &step.after) };
                    push(to, step);
                    let op = Op { c: e.id.clone(), p: Json::Null, s: e.sheet, sel: e.selection.clone(), a: author.clone(), wb: None, d };
                    if let Some(op) = (!op.d.is_empty()).then(|| serde_json::to_string(&op).ok()).flatten() {
                        stale.get_or_insert(ops.len());
                        ops.push(op.into());
                    }
                }
                continue;
            }
            let label = gridcraft_engine::find_command(&e.id).map_or_else(|| e.id.clone(), |c| c.label.trim_end_matches('…').to_string());
            push(&mut self.undo, Step { label, selection: e.selection.clone(), before: e.before, after: e.after.clone() });
            self.redo.clear();
            let p = gridcraft_engine::cmd::journal_params(&e.params);
            let wb = (LOCAL_STATE.contains(&e.id.as_str()) || p != e.params).then(|| Workbook::clone(&e.after));
            if let Ok(op) = serde_json::to_string(&Op { c: e.id, p, s: e.sheet, sel: e.selection, a: author.clone(), wb, d: vec![] }) {
                ops.push(op.into());
            }
        }
        if ops.is_empty() {
            self.mirror(s);
            return None;
        }
        let before = self.applied.len();
        let update = self.append(&ops);
        self.applied.extend(ops);
        match stale {
            // Undo and redo only change the workbook when replayed: replay from the first one.
            Some(k) => self.reconcile(s, before + k),
            // Already run here: the shared workbook is the ops so far.
            None => {
                if self.applied.len() / SNAPSHOT_EVERY > before / SNAPSHOT_EVERY {
                    self.snapshot(s, self.applied.len());
                }
                self.mirror(s);
            }
        }
        Some(update)
    }

    /// Opens a workbook for everyone, replacing the shared one.
    pub fn open(&mut self, s: &mut Session, name: &str, wb: Workbook) -> Option<Vec<u8>> {
        let sel = Selection::at(wb.active().map(|sh| sh.view_active).unwrap_or_default());
        let before = s.documents().iter().find(|d| d.uid == self.uid)?.wb.clone();
        let after = Arc::new(wb.clone());
        let op = Op {
            c: "file.open".into(),
            p: serde_json::json!({"name": name}),
            s: wb.active_sheet,
            sel: sel.clone(),
            a: s.prefs.user_name.clone(),
            wb: Some(wb),
            d: vec![],
        };
        let update = self.append(&[serde_json::to_string(&op).ok()?.into()]);
        push(&mut self.undo, Step { label: "Open".into(), selection: sel, before, after });
        self.redo.clear();
        self.reconcile(s, usize::MAX);
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
        self.reconcile(s, usize::MAX);
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
    /// the document and sheet they look at, their selection, clipboard and name, and their undo
    /// and redo. Only the first `valid` ops applied are taken as run here.
    fn reconcile(&mut self, s: &mut Session, valid: usize) {
        let ops: Vec<Arc<str>> = {
            let txn = self.ydoc.transact();
            self.ops.iter(&txn).filter_map(|v| if let Out::Any(Any::String(op)) = v { Some(op) } else { None }).collect()
        };
        let same = self.applied.iter().zip(&ops).take_while(|(a, b)| a == b).count().min(valid);
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
        self.mirror(s);
    }

    /// Shows this member's own steps as the shared document's undo and redo lists, which the
    /// Undo and Redo buttons, menus and `edit.undo`'s enabled state read.
    fn mirror(&self, s: &mut Session) {
        let history = |steps: &[Step]| -> Vec<HistoryEntry> {
            steps.iter().map(|st| HistoryEntry { label: st.label.clone(), wb: st.before.clone(), selection: st.selection.clone() }).collect()
        };
        let active = s.active_index();
        let Some(i) = s.documents().iter().position(|d| d.uid == self.uid) else { return };
        s.set_active(i);
        if let Some(d) = s.active_mut() {
            d.undo = history(&self.undo);
            d.redo = history(&self.redo);
        }
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
    if op.c == NEW && op.wb.is_none() {
        return;
    }
    // Hostile input: only what this host records (undoable commands, or a workbook or patches it
    // carries), on a sheet that still exists, with a selection inside the grid.
    let runs = op.wb.as_ref().map_or(!op.d.is_empty() || find_command(&op.c).is_some_and(|c| c.undoable), |wb| !wb.sheets.is_empty());
    let Some(d) = s.active_mut() else { return };
    if !runs || op.s >= d.wb.sheets.len() || !valid(&op.sel) {
        log::warn!("skipped a shared `{}` this app can't run", op.c);
        return;
    }
    if d.wb.active_sheet != op.s {
        Arc::make_mut(&mut d.wb).active_sheet = op.s;
    }
    d.selection = op.sel;
    if !op.d.is_empty() {
        match patched(&d.wb, &op.d) {
            Some(mut wb) => {
                d.calc.recalc_all(&mut wb);
                d.wb = Arc::new(wb);
                d.revision += 1;
            }
            None => log::info!("a shared `{}` did nothing: the same things changed since", op.c),
        }
        return;
    }
    match op.wb {
        // As `Session::execute` records an undoable command.
        Some(wb) => {
            // Everyone's title is the opened file's name (hostile input: a bare, bounded name).
            if op.c == "file.open"
                && let Some(name) = op.p.get("name").and_then(Json::as_str).and_then(|n| std::path::Path::new(n).file_name())
            {
                d.title = name.to_string_lossy().chars().take(MAX_TITLE).collect();
            }
            d.undo.push(HistoryEntry { label: op.c, wb: d.wb.clone(), selection: d.selection.clone() });
            if d.undo.len() > UNDO_LIMIT {
                d.undo.remove(0);
            }
            d.redo.clear();
            let mut wb = wb;
            // Spilled values aren't stored: recalculate them.
            d.calc.recalc_all(&mut wb);
            d.wb = Arc::new(wb);
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

/// A selection inside the grid: some ranges, each ordered, and the active cell and anchor in it.
pub fn valid(sel: &Selection) -> bool {
    let cell = |c: &CellRef| c.row < MAX_ROWS && c.col < MAX_COLS;
    !sel.ranges.is_empty()
        && cell(&sel.active)
        && cell(&sel.anchor)
        && sel.ranges.iter().all(|r| cell(&r.end) && r.start.row <= r.end.row && r.start.col <= r.end.col)
}

fn push(steps: &mut Vec<Step>, step: Step) {
    steps.push(step);
    if steps.len() > UNDO_LIMIT {
        steps.remove(0);
    }
}

/// The workbook as JSON, with each sheet's cells as one object keyed `row:col` and without formula
/// results (recalculated). The active sheet is each member's own.
fn tree(wb: &Workbook) -> Option<Json> {
    let mut bare = wb.clone();
    bare.active_sheet = 0;
    for sheet in &mut bare.sheets {
        Arc::make_mut(sheet).cells = CellStore::new();
    }
    let mut t = serde_json::to_value(&bare).ok()?;
    let empty = serde_json::to_value(Value::Empty).ok()?;
    for (sheet, s) in t.get_mut("sheets")?.as_array_mut()?.iter_mut().zip(&wb.sheets) {
        let mut cells = serde_json::Map::new();
        for (at, cell) in s.cells.iter() {
            let mut c = serde_json::to_value(cell).ok()?;
            if cell.formula.is_some() {
                c.as_object_mut()?.insert("value".into(), empty.clone());
            }
            cells.insert(format!("{}:{}", at.row, at.col), c);
        }
        sheet.as_object_mut()?.insert("cells".into(), Json::Object(cells));
    }
    Some(t)
}

/// Patches that make `a` into `b`. The style table only grows, so it is left out unless one
/// table isn't the start of the other (a file was opened).
// ponytail: serializes both whole workbooks on each Undo press; diff only the sheets whose `Arc`
// differs if big shared workbooks make Undo slow.
fn changes(a: &Workbook, b: &Workbook) -> Vec<Patch> {
    let (Some(ta), Some(tb)) = (tree(a), tree(b)) else { return vec![] };
    let mut out = vec![];
    diff(&mut vec![], Some(&ta), Some(&tb), &mut out);
    let styles = Json::from("styles");
    out.retain(|p| p.p.first() != Some(&styles));
    if !a.styles.iter().zip(b.styles.iter()).all(|((_, x), (_, y))| x == y) {
        out.push(Patch { p: vec![styles], f: ta.get("styles").map(|v| [v.clone()]), t: tb.get("styles").map(|v| [v.clone()]) });
    }
    out
}

/// Object members and same-length arrays are compared item by item; anything else that differs
/// is one patch.
fn diff(path: &mut Vec<Json>, a: Option<&Json>, b: Option<&Json>, out: &mut Vec<Patch>) {
    match (a, b) {
        (Some(Json::Object(x)), Some(Json::Object(y))) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                path.push(k.as_str().into());
                diff(path, x.get(k), y.get(k), out);
                path.pop();
            }
        }
        (Some(Json::Array(x)), Some(Json::Array(y))) if x.len() == y.len() => {
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                path.push(i.into());
                diff(path, Some(u), Some(v), out);
                path.pop();
            }
        }
        _ if a != b => out.push(Patch { p: path.clone(), f: a.map(|v| [v.clone()]), t: b.map(|v| [v.clone()]) }),
        _ => {}
    }
}

/// `wb` with `patches` applied, if every one still finds what it expects; `None` when someone
/// changed one of the same things since (or the patches are malformed).
fn patched(wb: &Workbook, patches: &[Patch]) -> Option<Workbook> {
    let mut t = tree(wb)?;
    if !patches.iter().all(|p| p.p.len() <= MAX_PATH && at(&t, &p.p) == p.f.as_ref().map(|[v]| v)) {
        return None;
    }
    for p in patches {
        put(&mut t, &p.p, p.t.as_ref().map(|[v]| v.clone()))?;
    }
    let blank = serde_json::to_value(CellStore::new()).ok()?;
    let cells: Vec<Json> =
        t.get_mut("sheets")?.as_array_mut()?.iter_mut().map(|s| s.as_object_mut()?.insert("cells".into(), blank.clone())).collect::<Option<_>>()?;
    let mut out: Workbook = serde_json::from_value(t).ok()?;
    out.active_sheet = wb.active_sheet.min(out.sheets.len().saturating_sub(1));
    out.preserved = wb.preserved.clone();
    out.source_dir = wb.source_dir.clone();
    for (i, (sheet, cells)) in out.sheets.iter_mut().zip(cells).enumerate() {
        let sheet = Arc::make_mut(sheet);
        for (at, cell) in cells.as_object()? {
            let (row, col) = at.split_once(':')?;
            let (row, col) = (row.parse().ok().filter(|r| *r < MAX_ROWS)?, col.parse().ok().filter(|c| *c < MAX_COLS)?);
            sheet.cells.set(CellRef::new(row, col), serde_json::from_value(cell.clone()).ok()?);
        }
        if let Some(old) = wb.sheets.get(i).filter(|old| old.name == sheet.name) {
            sheet.preserved = old.preserved.clone();
        }
    }
    Some(out)
}

fn index(k: &Json) -> Option<usize> {
    usize::try_from(k.as_u64()?).ok()
}

fn at<'a>(t: &'a Json, path: &[Json]) -> Option<&'a Json> {
    path.iter().try_fold(t, |v, k| match k {
        Json::String(k) => v.as_object()?.get(k),
        _ => v.as_array()?.get(index(k)?),
    })
}

/// Sets the value at `path` (adding missing objects on the way), or with `None` removes it.
fn put(t: &mut Json, path: &[Json], v: Option<Json>) -> Option<()> {
    let (last, parents) = path.split_last()?;
    let mut cur = t;
    for k in parents {
        cur = match (k, v.is_some()) {
            (Json::String(k), true) => cur.as_object_mut()?.entry(k.clone()).or_insert_with(|| Json::Object(Default::default())),
            (Json::String(k), false) => match cur.as_object_mut()?.get_mut(k) {
                Some(next) => next,
                None => return Some(()),
            },
            _ => cur.as_array_mut()?.get_mut(index(k)?)?,
        };
    }
    match (last, v) {
        (Json::String(k), Some(v)) => drop(cur.as_object_mut()?.insert(k.clone(), v)),
        (Json::String(k), None) => drop(cur.as_object_mut()?.remove(k)),
        (k, Some(v)) => *cur.as_array_mut()?.get_mut(index(k)?)? = v,
        (_, None) => return None,
    }
    Some(())
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
    fn undo_takes_back_only_your_own_changes() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "B1", "input": "1"}));
        sync(&mut [&mut a, &mut b]);
        run(&mut b.0, "cell.set", json!({"cell": "C1", "input": "=B1+1"}));
        sync(&mut [&mut a, &mut b]);
        // Each sees only their own step.
        assert_eq!(a.0.active().unwrap().undo.len(), 1);
        assert_eq!(b.0.active().unwrap().undo.len(), 1);
        run(&mut a.0, "edit.undo", json!({}));
        sync(&mut [&mut a, &mut b]);
        for p in [&mut a, &mut b] {
            assert_eq!(value(&mut p.0, "B1"), Json::Null);
            // B's formula stays, recalculated.
            assert_eq!(value(&mut p.0, "C1").as_f64(), Some(1.0));
        }
        assert_eq!(workbook(&a.0), workbook(&b.0));
        assert!(a.0.active().unwrap().undo.is_empty());
        assert_eq!(a.0.active().unwrap().redo.len(), 1);
        assert_eq!(b.0.active().unwrap().undo.len(), 1);
    }

    #[test]
    fn redo_after_undo() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "B1", "input": "1"}));
        run(&mut a.0, "home.bold", json!({"range": "B1"}));
        sync(&mut [&mut a, &mut b]);
        run(&mut b.0, "cell.set", json!({"cell": "C1", "input": "2"}));
        run(&mut a.0, "edit.undo", json!({"steps": 2}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut b.0, "B1"), Json::Null);
        assert_eq!(workbook(&a.0), workbook(&b.0));
        run(&mut a.0, "edit.redo", json!({}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut b.0, "B1").as_f64(), Some(1.0));
        assert_eq!(value(&mut b.0, "C1").as_f64(), Some(2.0));
        assert_eq!(workbook(&a.0), workbook(&b.0));
        run(&mut a.0, "edit.redo", json!({}));
        sync(&mut [&mut a, &mut b]);
        let d = b.0.active().unwrap();
        let style = d.wb.sheets[0].cell(gridcraft_engine::cell(0, 1)).unwrap().style;
        assert!(d.wb.styles.get(style).font.bold);
        assert_eq!(workbook(&a.0), workbook(&b.0));
        assert!(a.0.active().unwrap().redo.is_empty());
    }

    #[test]
    fn undo_is_refused_when_someone_changed_the_same_cell_since() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "B1", "input": "1"}));
        run(&mut a.0, "cell.set", json!({"cell": "B2", "input": "1"}));
        sync(&mut [&mut a, &mut b]);
        run(&mut b.0, "cell.set", json!({"cell": "B2", "input": "2"}));
        sync(&mut [&mut a, &mut b]);
        // B2 is B's now: A's undo of it does nothing, the next one takes back B1.
        run(&mut a.0, "edit.undo", json!({}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut b.0, "B2").as_f64(), Some(2.0));
        assert_eq!(value(&mut b.0, "B1").as_f64(), Some(1.0));
        run(&mut a.0, "edit.undo", json!({}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut b.0, "B1"), Json::Null);
        assert_eq!(workbook(&a.0), workbook(&b.0));
    }

    #[test]
    fn undo_with_nothing_of_your_own_does_nothing() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "A1", "input": "1"}));
        sync(&mut [&mut a, &mut b]);
        assert!(b.0.execute("edit.undo", json!({})).is_err());
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut b.0, "A1").as_f64(), Some(1.0));
        assert_eq!(workbook(&a.0), workbook(&b.0));
    }

    #[test]
    fn undo_converges_when_ops_arrive_late() {
        for (ida, idb) in [(1, 2), (2, 1)] {
            let (mut a, mut b) = (peer(ida), peer(idb));
            run(&mut b.0, "cell.set", json!({"cell": "B1", "input": "5"}));
            run(&mut b.0, "home.insertRows", json!({"rows": "1:1"}));
            sync(&mut [&mut a, &mut b]);
            // A's edits reach B only after B's undo.
            run(&mut a.0, "cell.set", json!({"cell": "A1", "input": "1"}));
            run(&mut a.0, "cell.set", json!({"cell": "C9", "input": "=SUM(B1:B5)"}));
            let late = a.1.local(&mut a.0).unwrap();
            run(&mut b.0, "edit.undo", json!({}));
            let undo = b.1.local(&mut b.0).unwrap();
            a.1.remote(&mut a.0, vec![undo.clone()]);
            b.1.remote(&mut b.0, vec![late.clone(), undo]);
            a.1.remote(&mut a.0, vec![late]);
            assert_eq!(workbook(&a.0), workbook(&b.0));
            assert_eq!(value(&mut a.0, "A1").as_f64(), Some(1.0));
            assert_eq!(value(&mut a.0, "B1").as_f64(), Some(5.0));
        }
    }

    #[test]
    fn undoing_an_open_puts_the_workbook_back() {
        let (mut a, mut b) = (peer(1), peer(2));
        run(&mut a.0, "cell.set", json!({"cell": "A1", "input": "1"}));
        sync(&mut [&mut a, &mut b]);
        let mut wb = Workbook::new();
        wb.sheets = vec![Arc::new(gridcraft_engine::model::Sheet::new("Data"))];
        let update = a.1.open(&mut a.0, "data.xlsx", wb).unwrap();
        b.1.remote(&mut b.0, vec![update]);
        run(&mut b.0, "cell.set", json!({"cell": "D4", "input": "4"}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(value(&mut b.0, "A1"), Json::Null);
        run(&mut a.0, "edit.undo", json!({}));
        sync(&mut [&mut a, &mut b]);
        assert_eq!(b.0.active().unwrap().wb.sheets[0].name, "Sheet1");
        assert_eq!(value(&mut b.0, "A1").as_f64(), Some(1.0));
        assert_eq!(value(&mut b.0, "D4").as_f64(), Some(4.0));
        assert_eq!(workbook(&a.0), workbook(&b.0));
    }

    #[test]
    fn hostile_patches_are_refused() {
        let wb = Workbook::new();
        let deep = Patch { p: vec![json!("x"); 1000], f: None, t: Some([json!(1)]) };
        assert!(patched(&wb, &[deep]).is_none());
        let bad = Patch { p: vec![json!("sheets"), json!(7), json!("name")], f: None, t: Some([json!("X")]) };
        assert!(patched(&wb, &[bad]).is_none());
        let wrong_type = Patch { p: vec![json!("sheets"), json!(0), json!("name")], f: Some([json!("Sheet1")]), t: Some([json!(3)]) };
        assert!(patched(&wb, &[wrong_type]).is_none());
    }

    #[test]
    fn hostile_ops_are_skipped() {
        let (mut evil, mut b) = (peer(1), peer(2));
        let sel = json!({"active": {"row": 0, "col": 0}, "anchor": {"row": 0, "col": 0}, "ranges": [{"start": {"row": 0, "col": 0}, "end": {"row": 0, "col": 0}}]});
        let reversed = json!({"active": {"row": 0, "col": 0}, "anchor": {"row": 0, "col": 0}, "ranges": [{"start": {"row": 4_000_000_000u32, "col": 0}, "end": {"row": 2, "col": 0}}]});
        let ops: Vec<Arc<str>> = [
            json!({"c": "file.close", "p": {"force": true}, "s": 0, "sel": sel}),
            json!({"c": "view.newWindow", "s": 0, "sel": sel}),
            json!({"c": "home.bold", "s": 0, "sel": reversed}),
            json!({"c": "cell.set", "p": {"cell": "A1", "input": "elsewhere"}, "s": 9, "sel": sel}),
            json!({"c": "cell.set", "p": {"cell": "A1", "input": "fine"}, "s": 0, "sel": sel}),
        ]
        .iter()
        .map(|op| op.to_string().into())
        .collect();
        let update = evil.1.append(&ops);
        b.1.remote(&mut b.0, vec![update]);
        assert_eq!(b.0.documents().len(), 1);
        assert_eq!(b.0.active().unwrap().uid, b.1.doc());
        assert_eq!(value(&mut b.0, "A1"), json!("fine"));
        // The engine refuses other documents at any depth, too.
        assert!(b.0.execute("file.close", json!({"force": true})).is_err());
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
        assert_eq!(a.0.active().unwrap().display_title(), "data.xlsx");
        assert_eq!(b.0.active().unwrap().display_title(), "data.xlsx");
    }

    #[test]
    fn garbage_updates_are_ignored() {
        let mut a = peer(1);
        a.1.remote(&mut a.0, vec![vec![], vec![0xff; 40]]);
        assert!(a.0.active().is_some());
        assert!(a.1.is_empty());
    }

    #[test]
    fn a_new_chat_has_no_workbook_until_someone_starts_one() {
        let (mut a, mut b) = (peer(1), peer(2));
        assert!(a.1.is_empty() && b.1.is_empty());
        // A blank workbook: an op that changes nothing, so the others leave their start screen.
        let update = a.1.create(&mut a.0).unwrap();
        assert!(!a.1.is_empty());
        b.1.remote(&mut b.0, vec![update]);
        assert!(!b.1.is_empty());
        assert_eq!(b.0.documents().len(), 1, "no second document");
        assert!(b.0.active().unwrap().undo.is_empty(), "nothing to undo");
        assert_eq!(workbook(&a.0), workbook(&b.0));
        // An edit or an opened file starts it too.
        let (mut c, mut d) = (peer(3), peer(4));
        run(&mut c.0, "cell.set", json!({"cell": "A1", "input": "1"}));
        sync(&mut [&mut c, &mut d]);
        assert!(!c.1.is_empty() && !d.1.is_empty());
        let mut e = peer(5);
        e.1.open(&mut e.0, "data.xlsx", Workbook::new()).unwrap();
        assert!(!e.1.is_empty());
    }
}
