# Changelog

Releases of the webxdc fork (`experintellia/gridcraft`, branch `webxdc`). Each release attaches `gridcraft.xdc`: send it to a Delta Chat chat and everyone who opens it edits the same workbook. Tags are the fork's own versions, independent of upstream's `Cargo.toml` version.

## Unreleased

- Synced with upstream GridCraft 0.10.0 (`storytold/gridcraft` `2ebdf16`): everything upstream shipped since 0.6.1, among it CSV/TSV open as text, Ctrl+Shift+= / Ctrl+- on whole rows and columns, right-to-left fixes and pivot table grand totals.
- One finger dragging on the sheet now scrolls it, as upstream does on every touch screen, including a drag from the active cell. To select a range, long-press a cell and drag on without lifting; a long press lifted without moving still opens the context menu (now when the finger lifts). A tap still selects.

### Shared workbook
- Zoom is each member's own: zooming no longer changes everyone's view, and others' edits keep your zoom ([#29](https://github.com/experintellia/gridcraft/pull/29)).

## v0.1.0 — 2026-10-21

### Shared workbook
- The chat's workbook takes the opened file's name: the messenger shows it next to the app, and every member's title bar follows it. Rename it for everyone on File › Info; undo takes the rename back ([#9](https://github.com/experintellia/gridcraft/pull/9)).
- Ctrl+O replaces the shared workbook (after the Replace Workbook warning); New, Close and the window commands are quiet no-ops instead of "not available" toasts.
- Password-protected files ask for their password and are decrypted on your device; the chat gets the workbook without its password, and the dialog says so. A file picked while someone else starts a workbook goes through the Replace Workbook warning ([#21](https://github.com/experintellia/gridcraft/pull/21)).
- File drops Save, Print and the AutoSave toggle, which have no meaning in a chat; Ctrl+P says to export a PDF ([#11](https://github.com/experintellia/gridcraft/pull/11), [#12](https://github.com/experintellia/gridcraft/pull/12)).

### Phones and tablets
- Type into cells on touch screens: a tap anywhere in the active cell, the Name Box or a dialog field opens the keyboard, and it stays up after Enter for continuous data entry, also with composing keyboards (Gboard suggestions, CJK) ([#7](https://github.com/experintellia/gridcraft/pull/7), [#19](https://github.com/experintellia/gridcraft/pull/19), [#23](https://github.com/experintellia/gridcraft/pull/23), [#24](https://github.com/experintellia/gridcraft/pull/24)).
- Drags and long presses on the active cell select and open the context menu instead of editing; no double-tap zoom; iOS's keyboard pan is undone when the edit ends ([#22](https://github.com/experintellia/gridcraft/pull/22)).
- The layout fits the phone's width: dialogs, the command palette and Insert Function no wider than the screen, a swipeable ribbon tab row, and a formula bar that keeps its text field ([#12](https://github.com/experintellia/gridcraft/pull/12), [#20](https://github.com/experintellia/gridcraft/pull/20)).

### Faster start
- WebGL2 rendering, a size-optimised wasm, and the wasm shipped pre-gzipped in the `.xdc` (Delta Chat delivers it ~7× faster); base64 decoding of shared updates is quicker. The browser console logs a startup timing report ([#8](https://github.com/experintellia/gridcraft/pull/8), [#10](https://github.com/experintellia/gridcraft/pull/10)).

## v0.0.1 — 2026-10-14

First demo: shared workbook editing in a chat ([#1](https://github.com/experintellia/gridcraft/pull/1)).
