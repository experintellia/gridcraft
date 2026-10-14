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
}
