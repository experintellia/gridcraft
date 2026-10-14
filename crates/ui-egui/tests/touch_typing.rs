//! On a touch screen a tap on the active cell edits it, and the frames around it keep the platform text input
//! (the on-screen keyboard) up: the active cell is a keyboard zone, the editor asks for IME from its first frame.

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

#[test]
fn a_tap_on_the_active_cell_edits_it_with_the_keyboard_up() {
    let mut h = harness();
    let c3 = center(&h, CellRef::new(2, 2));
    assert!(gridcraft_ui_egui::keyboard_zones(&h.ctx).iter().any(|z| z.contains(c3)), "the active cell opens the keyboard");

    // A tap elsewhere only selects.
    let d5 = center(&h, CellRef::new(4, 3));
    tap(&mut h, d5, true);
    h.run_steps(30); // Past the double-click interval.
    assert!(h.state().editor.is_none());
    assert_eq!(h.state().session.active().unwrap().selection.a1(), "D5");

    tap(&mut h, d5, true);
    assert_eq!(h.state().editor.as_ref().map(|e| e.cell), Some(CellRef::new(4, 3)));
    assert!(h.output().platform_output.ime.is_some(), "the keyboard stays up the frame editing starts");
    h.step();
    assert!(h.output().platform_output.ime.is_some());
}

#[test]
fn a_mouse_click_on_the_active_cell_does_not_edit() {
    let mut h = harness();
    let c3 = center(&h, CellRef::new(2, 2));
    h.run_steps(30);
    tap(&mut h, c3, false);
    h.step();
    assert!(h.state().editor.is_none());
}
