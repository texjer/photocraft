use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use crate::PhotocraftApp;

/// An app with a three-layer document that has a frame animation (one frame per layer).
fn harness() -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
        s.execute("layer.new.layer", json!({"name": "A"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "B"})).unwrap();
        s.execute("animation.makeFramesFromLayers", json!({"seconds": 0.2})).unwrap();
        PhotocraftApp::new(s, Default::default())
    });
    h.state_mut().ui.timeline.open = true;
    h.run_steps(6);
    h
}

#[test]
fn the_timeline_docks_along_the_bottom_by_default() {
    let h = harness();
    assert!(!h.state().ui.timeline.floating);
    // Docked, the header offers to float it; the strip shows each frame's delay.
    assert!(h.query_by_label("Float Panel").is_some());
    assert!(h.query_by_label("Dock Panel").is_none());
    assert_eq!(h.query_all_by_label_contains("0.2 sec.").count(), 3);
}

#[test]
fn the_dock_button_floats_the_panel_and_back() {
    let mut h = harness();
    h.get_by_label("Float Panel").click();
    h.run_steps(6);
    assert!(h.state().ui.timeline.floating);
    assert!(h.query_by_label("Dock Panel").is_some(), "a floating panel offers to dock");
    assert_eq!(h.query_all_by_label_contains("0.2 sec.").count(), 3, "the strip is the same floating");
    h.get_by_label("Dock Panel").click();
    h.run_steps(6);
    assert!(!h.state().ui.timeline.floating);
}

#[test]
fn clicking_a_frame_selects_it_when_docked() {
    let mut h = harness();
    h.state_mut().run("animation.selectFrame", json!({"frame": 0})).unwrap();
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().doc.frame_animation.as_ref().unwrap().current, 0);
    h.get_by_label("Next frame").click();
    h.run_steps(3);
    assert_eq!(h.state().session.active().unwrap().doc.frame_animation.as_ref().unwrap().current, 1);
}

#[test]
fn the_window_menu_hides_and_shows_the_docked_timeline() {
    let mut h = harness();
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "window.panel.timeline", json!({})).unwrap();
    h.run_steps(4);
    assert!(!h.state().ui.timeline.open);
    assert!(h.query_by_label("Float Panel").is_none());
    crate::menus::invoke(h.state_mut(), &ctx, "window.panel.timeline", json!({})).unwrap();
    h.run_steps(6);
    assert!(h.query_by_label("Float Panel").is_some(), "it comes back docked");
}

fn anim(h: &Harness<'static, PhotocraftApp>) -> photocraft_doc::FrameAnimation {
    h.state().session.active().unwrap().doc.frame_animation.clone().unwrap()
}

/// Puts the pointer over the panel, as the timeline's keys need.
fn hover_panel(h: &mut Harness<'static, PhotocraftApp>) -> egui::Rect {
    let r = h.state().ui.timeline.panel_rect.expect("the panel was drawn");
    h.event(egui::Event::PointerMoved(r.center()));
    h.run_steps(2);
    r
}

#[test]
fn space_plays_and_pauses_while_the_pointer_is_over_the_timeline() {
    let mut h = harness();
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert!(!h.state().ui.timeline.playing, "Space elsewhere is not the timeline's");
    hover_panel(&mut h);
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert!(h.state().ui.timeline.playing);
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert!(!h.state().ui.timeline.playing);
}

#[test]
fn arrows_home_and_end_step_through_the_frames() {
    let mut h = harness();
    hover_panel(&mut h);
    h.state_mut().run("animation.selectFrame", json!({"frame": 1})).unwrap();
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(anim(&h).current, 2);
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(anim(&h).current, 2, "stops at the last frame");
    h.key_press(egui::Key::Home);
    h.run_steps(2);
    assert_eq!(anim(&h).current, 0);
    h.key_press(egui::Key::End);
    h.run_steps(2);
    assert_eq!(anim(&h).current, 2);
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(anim(&h).current, 1);
}

#[test]
fn copy_and_paste_duplicate_a_frame_with_its_delay_and_layers() {
    let mut h = harness();
    hover_panel(&mut h);
    h.state_mut().run("animation.selectFrame", json!({"frame": 1})).unwrap();
    h.state_mut().run("animation.setDelay", json!({"seconds": 1.5})).unwrap();
    let copied = anim(&h).frames[1].clone();
    h.event(egui::Event::Copy);
    h.run_steps(2);
    assert_eq!(h.state().ui.timeline.clipboard, std::slice::from_ref(&copied));
    h.state_mut().run("animation.selectFrame", json!({"frame": 2})).unwrap();
    h.event(egui::Event::Paste(String::new()));
    h.run_steps(3);
    let a = anim(&h);
    assert_eq!(a.frames.len(), 4);
    assert_eq!(a.frames[3], copied, "pasted after the selected frame");
    assert_eq!(a.current, 3);
}

#[test]
fn alt_arrows_move_the_selected_frame() {
    let mut h = harness();
    hover_panel(&mut h);
    h.state_mut().run("animation.selectFrame", json!({"frame": 0})).unwrap();
    h.state_mut().run("animation.setDelay", json!({"seconds": 3})).unwrap();
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowRight);
    h.run_steps(2);
    let a = anim(&h);
    assert_eq!((a.current, a.frames[1].delay_ms), (1, 3000));
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(anim(&h).frames[0].delay_ms, 3000);
}

#[test]
fn dragging_a_frame_reorders_it() {
    let mut h = harness();
    let r = hover_panel(&mut h);
    h.state_mut().run("animation.setDelay", json!({"seconds": 3, "frames": [0]})).unwrap();
    h.run_steps(2);
    let y = r.top() + 62.0;
    let cell = |i: usize| egui::pos2(r.left() + 42.0 + i as f32 * 90.0, y);
    h.event(egui::Event::PointerMoved(cell(0)));
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: cell(0), button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.run_steps(1);
    for step in 1..=8 {
        let x = cell(0).x + (cell(2).x + 30.0 - cell(0).x) * step as f32 / 8.0;
        h.event(egui::Event::PointerMoved(egui::pos2(x, y)));
        h.run_steps(1);
    }
    h.event(egui::Event::PointerButton {
        pos: egui::pos2(cell(2).x + 30.0, y),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    h.run_steps(3);
    let a = anim(&h);
    assert_eq!(a.frames.iter().map(|f| f.delay_ms).collect::<Vec<_>>(), [200, 200, 3000], "frame 1 was dropped after the last one");
    assert_eq!(a.current, 2, "the selection travelled with it");
}

/// The delay menu's "Other…" field, which starts at the frame's 0.2 seconds.
fn other_field<'a>(h: &'a Harness<'static, PhotocraftApp>) -> egui_kittest::Node<'a> {
    h.query_all_by_role(egui::accesskit::Role::TextInput).find(|n| n.value().as_deref() == Some("0.2")).expect("the Other… field")
}

#[test]
fn clicking_the_other_field_keeps_the_delay_menu_open_for_typing() {
    let mut h = harness();
    h.query_all_by_label_contains("0.2 sec.").next().unwrap().click();
    h.run_steps(4);
    assert!(h.query_by_label("Other…").is_some(), "the menu opened");
    other_field(&h).click();
    h.run_steps(4);
    assert!(h.query_by_label("Other…").is_some(), "a click in the field leaves the menu open");
    other_field(&h).type_text("1.5");
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert_eq!(anim(&h).frames[0].delay_ms, 1500);
    assert!(h.query_by_label("Other…").is_none(), "Enter applies it and closes the menu");
}

/// A click on frame cell `i` with `mods` held.
fn click_cell(h: &mut Harness<'static, PhotocraftApp>, r: egui::Rect, i: usize, mods: egui::Modifiers) {
    let at = egui::pos2(r.left() + 42.0 + i as f32 * 90.0, r.top() + 62.0);
    h.event(egui::Event::ModifiersChanged(mods));
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: mods });
        h.run_steps(1);
    }
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run_steps(2);
}

#[test]
fn shift_and_command_clicks_select_several_frames() {
    let mut h = harness();
    let r = hover_panel(&mut h);
    click_cell(&mut h, r, 0, egui::Modifiers::NONE);
    assert_eq!(anim(&h).selection(), [0]);
    click_cell(&mut h, r, 2, egui::Modifiers::SHIFT);
    assert_eq!(anim(&h).selection(), [0, 1, 2]);
    click_cell(&mut h, r, 1, egui::Modifiers::COMMAND);
    assert_eq!(anim(&h).selection(), [0, 2]);
    click_cell(&mut h, r, 1, egui::Modifiers::NONE);
    assert_eq!(anim(&h).selection(), [1], "a plain click selects one frame");
}

#[test]
fn a_selected_frames_delay_menu_sets_every_selected_frame() {
    let mut h = harness();
    hover_panel(&mut h);
    h.state_mut().run("animation.selectFrame", json!({"frame": 1})).unwrap();
    h.state_mut().run("animation.selectFrame", json!({"frame": 2, "extend": "range"})).unwrap();
    h.run_steps(2);
    h.get_all_by_label_contains("0.2 sec.").nth(1).unwrap().click();
    h.run_steps(3);
    h.get_by_label("1 sec.").click();
    h.run_steps(3);
    let delays: Vec<u32> = anim(&h).frames.iter().map(|f| f.delay_ms).collect();
    assert_eq!(delays, [200, 1000, 1000]);
}

#[test]
fn copy_takes_every_selected_frame_and_delete_removes_them() {
    let mut h = harness();
    hover_panel(&mut h);
    h.state_mut().run("animation.selectFrame", json!({"frame": 0})).unwrap();
    h.state_mut().run("animation.selectFrame", json!({"frame": 1, "extend": "range"})).unwrap();
    h.event(egui::Event::Copy);
    h.run_steps(2);
    assert_eq!(h.state().ui.timeline.clipboard.len(), 2);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(anim(&h).frames.len(), 1);
    h.state_mut().run("animation.selectAllFrames", json!({})).unwrap();
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(h.state().session.active().unwrap().doc.frame_animation.is_some(), "Delete never takes the last frame");
}
