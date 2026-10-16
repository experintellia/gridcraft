//! A finger tap on the active cell (or the formula bar) edits it, and the frames around it keep the platform text
//! input (the on-screen keyboard) up: both are keyboard zones, and editing asks for IME from its first frame.

use egui::{Event, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, vec2};
use gridcraft_engine::Session;
use gridcraft_engine::core::CellRef;
use gridcraft_ui_egui::{SheetApp, grid::Geo};
use serde_json::json;

fn harness() -> egui_kittest::Harness<'static, SheetApp> {
    let mut session = Session::new();
    session.new_workbook();
    session.execute("selection.set", json!({"cell": "C3"})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_step_dt(1.0 / 60.0).with_size(vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut SheetApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        SheetApp::new(session, Default::default()),
    );
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
    assert!(!zone.contains(cell.right_bottom() - egui::vec2(2.0, 2.0)));
}
