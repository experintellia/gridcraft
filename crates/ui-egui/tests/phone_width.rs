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
    // Opened once on a wider screen first: it shrinks again on the narrower one.
    h.set_size(egui::vec2(900.0, 844.0));
    h.state_mut().open_dialog("commandSearch", serde_json::json!({}));
    h.run_steps(4);
    h.state_mut().dialog = None;
    h.set_size(egui::vec2(390.0, 844.0));
    h.run_steps(2);
    h.state_mut().open_dialog("commandSearch", serde_json::json!({}));
    h.run_steps(4);
    let r = h.ctx.memory(|m| m.area_rect(egui::Id::new("dialog").with("commandSearch"))).expect("palette");
    assert!(r.left() >= 0.0 && r.right() <= 390.0, "{r:?}");
}

/// The formula bar's text field (its keyboard zone) on a phone, at the given interface size.
fn formula_field(width: f32, percent: u32) -> egui::Rect {
    let mut h = phone();
    h.state_mut().run("view.interfaceScale", serde_json::json!({ "percent": percent })).unwrap();
    h.set_size(egui::vec2(width, 844.0));
    h.run_steps(4);
    let grid_top = h.state().grid.rect.unwrap().top();
    gridcraft_ui_egui::keyboard_zones(&h.ctx).into_iter().find(|z| z.bottom() <= grid_top).expect("formula bar zone")
}

#[test]
fn the_formula_field_keeps_its_width() {
    // 390 px at 100%, at 170% (229 points), and the narrowest a phone gets (a 390 px canvas at 200%: 195 points).
    // Also each width where a control comes back (at 100%): 232 points (✕ ✓ only), 360 (Name Box), 388 (and fx).
    for (width, percent) in [(390.0, 100), (390.0, 170), (390.0, 200), (232.0, 100), (264.0, 100), (360.0, 100), (388.0, 100)] {
        let field = formula_field(width, percent);
        let points = width / (percent as f32 / 100.0);
        // 120 points wherever the bar has them; narrower, only ✕ ✓ and the chevron are left beside the field.
        let min = if points >= 232.0 { 120.0 } else { 80.0 };
        assert!(field.width() >= min && field.right() <= points, "{width} px at {percent}%: {field:?}");
    }
}

#[test]
fn fx_goes_before_the_name_box() {
    // At 300 points the Name Box is hidden, and fx with it: the field gets about 188 points (about 158 with fx).
    let field = formula_field(300.0, 100);
    assert!(field.width() >= 180.0, "{field:?}");
}

#[test]
fn a_hidden_name_box_forgets_what_was_typed() {
    let mut h = phone();
    h.set_size(egui::vec2(900.0, 844.0));
    h.run_steps(2);
    h.ctx.memory_mut(|m| m.request_focus(egui::Id::new("gridcraft.name_box")));
    h.run_steps(2);
    h.input_mut().events.push(Event::Text("Q9".into()));
    h.run_steps(2);
    assert!(h.state().name_box.is_some());
    // Narrowed (a phone turned upright) until the Name Box goes.
    h.set_size(egui::vec2(300.0, 844.0));
    h.run_steps(2);
    assert!(h.state().name_box.is_none(), "{:?}", h.state().name_box);
}

#[test]
fn a_formula_bar_with_no_room_does_not_panic() {
    let mut h = phone();
    h.state_mut().run("view.interfaceScale", serde_json::json!({ "percent": 200 })).unwrap();
    h.set_size(egui::vec2(120.0, 844.0));
    h.run_steps(2);
    h.state_mut().begin_edit(None, true);
    h.run_steps(2);
    assert!(h.state().editor.is_some());
}
