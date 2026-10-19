//! A finger tap on the active cell (or the formula bar) edits it, and the frames around it keep the platform text
//! input (the on-screen keyboard) up: both are keyboard zones, and editing asks for IME from its first frame. A tap on
//! the web shell's text input over a text field (the name box, a dialog's) focuses that field.

use egui::{Event, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, vec2};
use gridcraft_engine::Session;
use gridcraft_engine::core::CellRef;
use gridcraft_ui_egui::{SheetApp, grid::Geo};
use serde_json::json;

fn harness() -> egui_kittest::Harness<'static, SheetApp> {
    harness_of_size(vec2(1200.0, 800.0))
}

fn harness_of_size(size: egui::Vec2) -> egui_kittest::Harness<'static, SheetApp> {
    let mut session = Session::new();
    session.new_workbook();
    session.execute("selection.set", json!({"cell": "C3"})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_step_dt(1.0 / 60.0).with_size(size).build_ui_state(
        |ui, app: &mut SheetApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        SheetApp::new(session, Default::default()),
    );
    gridcraft_ui_egui::set_touch_screen(&h.ctx, true);
    h.run_steps(4);
    h
}

fn center(h: &egui_kittest::Harness<'_, SheetApp>, cell: CellRef) -> Pos2 {
    let app = h.state();
    let sheet = app.session.active().unwrap().wb.active().unwrap();
    let geo = Geo::new(sheet, app.grid.rect.unwrap(), app.view().scroll);
    geo.screen_cell_rect(sheet, cell).center()
}

/// A finger tap: touch events plus the pointer press and release eframe sends with them.
fn tap(h: &mut egui_kittest::Harness<'_, SheetApp>, pos: Pos2, touch: bool) {
    for pressed in [true, false] {
        let phase = if pressed { TouchPhase::Start } else { TouchPhase::End };
        if touch {
            h.input_mut().events.push(Event::Touch { device_id: TouchDeviceId(0), id: TouchId(1), phase, pos, force: None });
        }
        h.input_mut()
            .events
            .extend([Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        h.step();
    }
}

fn ime(h: &egui_kittest::Harness<'_, SheetApp>) -> bool {
    h.output().platform_output.ime.is_some()
}

fn focused(h: &egui_kittest::Harness<'_, SheetApp>) -> Option<egui::Id> {
    h.ctx.memory(|m| m.focused())
}

/// The text zones as the web shell reads them, at the end of the pass (with the pass's IME output).
fn text_zones(h: &mut egui_kittest::Harness<'_, SheetApp>) -> Vec<egui::Rect> {
    let id = egui::Id::new("test.text_zones");
    h.ctx.on_end_pass(
        "test.text_zones",
        std::sync::Arc::new(move |ui: &mut egui::Ui| {
            let zones = gridcraft_ui_egui::text_zones(ui.ctx());
            ui.ctx().data_mut(|d| d.insert_temp(id, zones));
        }),
    );
    h.step();
    h.ctx.data(|d| d.get_temp(id)).unwrap_or_default()
}

#[test]
fn a_tap_on_the_active_cell_edits_it_with_the_keyboard_up() {
    let mut h = harness();
    let c3 = center(&h, CellRef::new(2, 2));
    assert!(gridcraft_ui_egui::tap_opens_keyboard(&h.ctx, c3), "the active cell opens the keyboard");
    let d5 = center(&h, CellRef::new(4, 3));
    assert!(!gridcraft_ui_egui::tap_opens_keyboard(&h.ctx, d5));

    // C3 is active but was never clicked, so this is no double-click.
    tap(&mut h, c3, true);
    assert_eq!(h.state().editor.as_ref().map(|e| e.cell), Some(CellRef::new(2, 2)));
    assert!(ime(&h), "the keyboard stays up the frame editing starts");
    h.step();
    assert!(ime(&h));
}

#[test]
fn a_tap_elsewhere_only_selects() {
    let mut h = harness();
    let p = center(&h, CellRef::new(4, 3));
    tap(&mut h, p, true);
    h.step();
    assert!(h.state().editor.is_none());
    assert_eq!(h.state().session.active().unwrap().selection.a1(), "D5");
}

#[test]
fn a_tap_on_the_formula_bar_keeps_the_keyboard_up() {
    let mut h = harness();
    let grid_top = h.state().grid.rect.unwrap().top();
    let bar = gridcraft_ui_egui::keyboard_zones(&h.ctx).into_iter().find(|z| z.bottom() <= grid_top).expect("formula bar zone");
    tap(&mut h, bar.center(), true);
    assert!(h.state().editor.as_ref().is_some_and(|e| e.from_formula_bar));
    assert!(ime(&h), "the keyboard stays up the frame editing starts");
    h.step();
    assert!(ime(&h));
}

#[test]
fn a_mouse_click_on_the_active_cell_does_not_edit_even_after_a_touch() {
    let mut h = harness();
    let p = center(&h, CellRef::new(4, 3));
    tap(&mut h, p, true); // A touch screen, from now on.
    // F2 becomes active without a click, so a click on it is no double-click.
    h.state_mut().session.execute("selection.set", json!({"cell": "F2"})).unwrap();
    h.step();
    let p = center(&h, CellRef::new(1, 5));
    tap(&mut h, p, false);
    h.step();
    assert!(h.state().editor.is_none());
    assert!(!ime(&h), "no on-screen keyboard (or desktop IME) without editing");
}

#[test]
fn a_tap_on_the_text_input_over_the_active_cell_edits_it() {
    let mut h = harness();
    let c3 = center(&h, CellRef::new(2, 2));
    assert!(gridcraft_ui_egui::cell_zone(&h.ctx).is_some_and(|z| z.contains(c3)), "the web shell's text input lies over C3");

    gridcraft_ui_egui::tap_cell_zone(&h.ctx);
    h.step();
    assert_eq!(h.state().editor.as_ref().map(|e| e.cell), Some(CellRef::new(2, 2)));
    assert!(ime(&h), "the keyboard stays up the frame editing starts");
    h.step();
    assert!(ime(&h));
    assert!(gridcraft_ui_egui::cell_zone(&h.ctx).is_none(), "while editing the input shrinks back to the text cursor");
}

#[test]
fn no_text_input_over_an_active_cell_scrolled_out_of_view() {
    let mut h = harness();
    let grid = h.state().grid.rect.unwrap();
    h.input_mut().events.extend([
        Event::PointerMoved(grid.center()),
        Event::MouseWheel { unit: egui::MouseWheelUnit::Point, phase: TouchPhase::Move, delta: vec2(0.0, -3000.0), modifiers: Modifiers::NONE },
    ]);
    h.run_steps(4);
    assert!(h.state().view().scroll.y > 200.0, "the grid scrolled past C3");
    assert!(gridcraft_ui_egui::cell_zone(&h.ctx).is_none(), "C3 is out of view: a tap there must not edit it");
}

#[test]
fn checkbox_cells_keep_their_tap() {
    let mut h = harness();
    h.state_mut().session.execute("insert.checkbox", json!({"range": "C3"})).unwrap();
    h.run_steps(2);
    assert!(gridcraft_ui_egui::cell_zone(&h.ctx).is_none());
}

#[test]
fn a_tap_reported_while_editing_does_not_edit_again_afterwards() {
    let mut h = harness();
    gridcraft_ui_egui::tap_cell_zone(&h.ctx);
    h.step();
    assert!(h.state().editor.is_some());
    // eframe focusing its input itself while the edit runs.
    gridcraft_ui_egui::tap_cell_zone(&h.ctx);
    h.step();
    h.state_mut().cancel_edit();
    h.run_steps(2);
    assert!(h.state().editor.is_none());
}

fn type_and_enter(h: &mut egui_kittest::Harness<'_, SheetApp>, text: &str) {
    h.input_mut().events.push(Event::Text(text.into()));
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
}

fn input(h: &egui_kittest::Harness<'_, SheetApp>, cell: &str) -> String {
    let sheet = h.state().session.active().unwrap().wb.active().unwrap();
    sheet.cell(CellRef::parse(cell).unwrap()).map(|c| c.input_text()).unwrap_or_default()
}

#[test]
fn enter_after_a_tap_started_edit_keeps_the_keyboard_up_for_the_next_cell() {
    let mut h = harness();
    let c3 = center(&h, CellRef::new(2, 2));
    tap(&mut h, c3, true);
    h.step();
    type_and_enter(&mut h, "5");
    assert!(h.state().editor.is_none());
    assert_eq!(h.state().session.active().unwrap().selection.a1(), "C4");
    for _ in 0..3 {
        h.step();
        assert!(ime(&h), "the keyboard stays up after Enter");
    }
    let c4 = center(&h, CellRef::new(3, 2));
    assert!(gridcraft_ui_egui::cell_zone(&h.ctx).is_some_and(|z| z.contains(c4)), "the web shell's text input lies over C4");

    // The next keystroke edits C4, and Enter goes on to C5.
    type_and_enter(&mut h, "7");
    h.step();
    assert!(ime(&h));
    assert_eq!((input(&h, "C3"), input(&h, "C4")), ("5".to_string(), "7".to_string()));
    assert_eq!(h.state().session.active().unwrap().selection.a1(), "C5");
}

#[test]
fn a_tap_elsewhere_lets_the_kept_keyboard_go() {
    let mut h = harness();
    gridcraft_ui_egui::tap_cell_zone(&h.ctx);
    h.step();
    type_and_enter(&mut h, "5");
    h.step();
    assert!(ime(&h));
    let p = center(&h, CellRef::new(4, 3));
    tap(&mut h, p, true);
    h.step();
    assert!(h.state().editor.is_none());
    assert!(!ime(&h), "the keyboard goes down");
}

#[test]
fn enter_without_touch_lets_the_ime_go() {
    let mut h = harness();
    type_and_enter(&mut h, "5");
    h.step();
    assert_eq!(input(&h, "C3"), "5");
    assert!(!ime(&h), "no touch: no IME once the edit ends");
}

#[test]
fn a_tap_on_the_text_input_over_the_name_box_focuses_it() {
    let mut h = harness();
    let grid_top = h.state().grid.rect.unwrap().top();
    let zones = text_zones(&mut h);
    let c3 = center(&h, CellRef::new(2, 2));
    assert!(zones.iter().any(|z| z.contains(c3)), "the input lies over the active cell");
    let name_box = zones.into_iter().find(|z| z.bottom() <= grid_top).expect("the input lies over the name box too");

    assert!(gridcraft_ui_egui::tap_text_zone(&h.ctx, name_box.center()).is_some());
    h.step();
    assert_eq!(focused(&h), Some(egui::Id::new("gridcraft.name_box")));
    assert!(h.state().editor.is_none(), "no cell edit");
    assert!(ime(&h), "the keyboard stays up");
    assert!(text_zones(&mut h).is_empty(), "while typing the input follows the text cursor");
    h.input_mut().events.push(Event::Text("E9".into()));
    h.step();
    assert_eq!(h.state().name_box.as_deref(), Some("E9"), "typing replaces the reference");
}

#[test]
fn a_tap_on_the_text_input_over_a_dialog_field_focuses_it() {
    let mut h = harness();
    h.state_mut().open_dialog("find", json!({"replace": true}));
    h.run_steps(4);
    let grid_top = h.state().grid.rect.unwrap().top();
    let fields: Vec<_> = text_zones(&mut h).into_iter().filter(|z| z.top() > grid_top).collect();
    assert_eq!(fields.len(), 2, "Find what and Replace with, and no active cell under the dialog");
    // Between the fields the tap reaches the dialog.
    assert!(gridcraft_ui_egui::tap_text_zone(&h.ctx, fields[0].center_bottom() + vec2(0.0, 2.0)).is_none());

    let with = fields[1];
    assert_eq!(gridcraft_ui_egui::tap_text_zone(&h.ctx, with.center()), Some(with));
    h.step();
    assert!(ime(&h), "the Replace with field takes typing");
    let id = focused(&h).expect("a focused field");
    h.input_mut().events.push(Event::Text("x".into()));
    h.step();
    assert_eq!(focused(&h), Some(id));
    assert_eq!(h.state().dialog.as_ref().and_then(|d| d.values.get("with").cloned()), Some(json!("x")));
}

#[test]
fn no_text_input_over_a_field_a_dialog_covers() {
    // A phone: the dialog spans the width, over the name box.
    let mut h = harness_of_size(vec2(390.0, 330.0));
    let grid_top = h.state().grid.rect.unwrap().top();
    assert!(text_zones(&mut h).iter().any(|z| z.bottom() <= grid_top), "the name box takes a tap");
    h.state_mut().open_dialog("find", json!({"replace": true}));
    h.run_steps(4);
    let zones = text_zones(&mut h);
    assert_eq!(zones.len(), 2, "only the dialog's fields: {zones:?}");
    let dialog = h.ctx.memory(|m| m.area_rect(egui::Id::new("dialog").with("find"))).unwrap();
    assert!(zones.iter().all(|z| dialog.contains_rect(*z)));
}

#[test]
fn no_text_input_over_a_field_a_popup_covers_in_part() {
    let mut h = harness();
    let grid_top = h.state().grid.rect.unwrap().top();
    let name_box = text_zones(&mut h).into_iter().find(|z| z.bottom() <= grid_top).expect("the name box takes a tap");
    // A popup over the left of the box, clear of its centre and corners.
    let strip = egui::Rect::from_min_size(name_box.left_center() + vec2(4.0, -2.0), vec2(12.0, 4.0));
    h.ctx.on_end_pass(
        "test.popup",
        std::sync::Arc::new(move |ui: &mut egui::Ui| {
            egui::Area::new(egui::Id::new("test.popup")).order(egui::Order::Foreground).fixed_pos(strip.min).show(ui.ctx(), |ui| {
                ui.allocate_exact_size(strip.size(), egui::Sense::click());
            });
        }),
    );
    h.run_steps(2);
    assert!(!text_zones(&mut h).iter().any(|z| z.bottom() <= grid_top), "a tap on the popup reaches it");
}

#[test]
fn a_tap_on_the_text_input_over_the_active_cell_still_edits_it() {
    let mut h = harness();
    let c3 = center(&h, CellRef::new(2, 2));
    assert!(gridcraft_ui_egui::tap_text_zone(&h.ctx, c3).is_some());
    h.step();
    assert_eq!(h.state().editor.as_ref().map(|e| e.cell), Some(CellRef::new(2, 2)));
}

#[test]
fn the_text_input_covers_most_of_the_active_cell() {
    let h = harness();
    let app = h.state();
    let sheet = app.session.active().unwrap().wb.active().unwrap();
    let cell = Geo::new(sheet, app.grid.rect.unwrap(), app.view().scroll).screen_cell_rect(sheet, CellRef::new(2, 2));
    let zone = gridcraft_ui_egui::cell_zone(&h.ctx).expect("a zone over C3");
    // A tap near the top or bottom of the cell must still land on the input, or iOS keeps the keyboard down.
    assert!(zone.height() >= cell.height() - 4.0, "{zone:?} in {cell:?}");
    // The fill handle at the bottom-right corner stays on the grid.
    assert!(!zone.intersects(egui::Rect::from_center_size(cell.right_bottom(), egui::vec2(9.0, 9.0))));
}

#[test]
fn a_filter_header_keeps_its_tap() {
    let mut h = harness();
    h.state_mut().session.execute("data.filter", json!({"range": "C3:D6"})).unwrap();
    h.state_mut().session.execute("selection.set", json!({"cell": "C3"})).unwrap();
    h.run_steps(2);
    assert!(gridcraft_ui_egui::cell_zone(&h.ctx).is_none(), "a tap on C3's filter button opens the filter menu");
}

/// A frame with the events the app queued for itself (what eframe's `raw_input_hook` hands it).
fn step_with_queued_events(h: &mut egui_kittest::Harness<'_, SheetApp>) {
    let mut raw = std::mem::take(h.input_mut());
    h.state_mut().raw_input_hook(&mut raw);
    *h.input_mut() = raw;
    h.step();
}

#[test]
fn enter_after_a_formula_keeps_the_keyboard_up() {
    let mut h = harness();
    let c3 = center(&h, CellRef::new(2, 2));
    tap(&mut h, c3, true);
    h.step();
    // The argument hint shows under the editor, over C4, until Enter closes it.
    type_and_enter(&mut h, "=SUM(A1:A3");
    assert_eq!(h.state().session.active().unwrap().selection.a1(), "C4");
    for _ in 0..3 {
        h.step();
        assert!(ime(&h), "the keyboard stays up after Enter on a formula");
    }
    type_and_enter(&mut h, "7");
    assert_eq!(input(&h, "C4"), "7");
}

#[test]
fn a_composing_keyboard_types_into_the_next_cell() {
    let mut h = harness();
    gridcraft_ui_egui::tap_cell_zone(&h.ctx);
    h.step();
    type_and_enter(&mut h, "5");
    h.step();
    assert!(ime(&h));
    // Gboard with suggestions, or a CJK keyboard: the word is composed first, then committed.
    h.input_mut().events.push(Event::Ime(egui::ImeEvent::Preedit { text: "ap".into(), active_range_chars: None }));
    h.step();
    assert_eq!(h.state().editor.as_ref().map(|e| e.cell), Some(CellRef::new(3, 2)));
    step_with_queued_events(&mut h);
    assert_eq!(h.state().editor.as_ref().map(|e| e.text.as_str()), Some("ap"), "the editor composes the word");
    assert!(ime(&h));
    h.input_mut().events.push(Event::Ime(egui::ImeEvent::Preedit { text: "apple".into(), active_range_chars: None }));
    h.step();
    h.input_mut().events.push(Event::Ime(egui::ImeEvent::Commit("apple".into())));
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    assert_eq!(input(&h, "C4"), "apple");
    // A word committed at once (iOS Korean, a picked suggestion) edits the next cell too.
    h.step();
    h.input_mut().events.push(Event::Ime(egui::ImeEvent::Commit("pear".into())));
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    assert_eq!(input(&h, "C5"), "pear");
}

#[test]
fn the_text_input_stays_over_the_next_cell_while_the_keyboard_is_kept_up() {
    let mut h = harness();
    gridcraft_ui_egui::tap_cell_zone(&h.ctx);
    h.step();
    type_and_enter(&mut h, "5");
    h.step();
    let zones = text_zones(&mut h);
    assert!(ime(&h), "the keyboard is kept up");
    let c4 = center(&h, CellRef::new(3, 2));
    assert!(zones.len() == 1 && zones[0].contains(c4), "only the next cell, not the name box: {zones:?}");
    // A tap on the still-focused input there edits it.
    assert!(gridcraft_ui_egui::tap_text_zone(&h.ctx, c4).is_some());
    h.step();
    assert_eq!(h.state().editor.as_ref().map(|e| e.cell), Some(CellRef::new(3, 2)));
}

#[test]
fn a_desktop_touchscreen_lets_the_ime_go_after_enter() {
    let mut h = harness();
    gridcraft_ui_egui::set_touch_screen(&h.ctx, false);
    let c3 = center(&h, CellRef::new(2, 2));
    tap(&mut h, c3, true);
    h.step();
    type_and_enter(&mut h, "5");
    h.step();
    assert_eq!(input(&h, "C3"), "5");
    assert!(!ime(&h), "no platform IME without a text field on the desktop");
}
