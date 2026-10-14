//! A phone is narrower than the ribbon's tabs and the dialogs.

use egui::{Event, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, pos2};
use gridcraft_engine::Session;
use gridcraft_ui_egui::SheetApp;

fn phone() -> egui_kittest::Harness<'static, SheetApp> {
    let mut session = Session::new();
    session.new_workbook();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(390.0, 844.0)).build_ui_state(
        |ui, app: &mut SheetApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        SheetApp::new(session, Default::default()),
    );
    // egui lets a finger drag-scroll once it has seen a touch screen.
    h.input_mut().events.push(Event::Touch {
        device_id: TouchDeviceId(1),
        id: TouchId(1),
        phase: TouchPhase::Cancel,
        pos: pos2(1.0, 1.0),
        force: None,
    });
    h.run_steps(4);
    h
}

fn press(h: &mut egui_kittest::Harness<'static, SheetApp>, at: Pos2, pressed: bool) {
    h.input_mut()
        .events
        .extend([Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    h.step();
}

fn tap(h: &mut egui_kittest::Harness<'static, SheetApp>, at: Pos2) {
    press(h, at, true);
    press(h, at, false);
    h.run_steps(3);
}

#[test]
fn the_ribbon_tabs_swipe_sideways() {
    let mut h = phone();
    // The tab row's centre line; the last tabs are past the right edge.
    let (y, x) = (61.0, 310.0);
    tap(&mut h, pos2(x, y));
    let before = h.state().ui.ribbon_tab.clone();
    press(&mut h, pos2(330.0, y), true);
    for i in 1..=10 {
        h.input_mut().events.push(Event::PointerMoved(pos2(330.0 - 25.0 * i as f32, y)));
        h.step();
    }
    press(&mut h, pos2(80.0, y), false);
    h.run_steps(10);
    tap(&mut h, pos2(x, y));
    let after = h.state().ui.ribbon_tab.clone();
    assert_ne!(before, after, "the swipe moved a later tab under {x}");
}

#[test]
fn the_command_palette_fits() {
    let mut h = phone();
    h.state_mut().open_dialog("commandSearch", serde_json::json!({}));
    h.run_steps(4);
    let r = h.ctx.memory(|m| m.area_rect(egui::Id::new("dialog").with("commandSearch"))).expect("palette");
    assert!(r.left() >= 0.0 && r.right() <= 390.0, "{r:?}");
}
