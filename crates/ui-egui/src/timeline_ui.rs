//! Window › Timeline: a panel that creates and drives the document timeline. It docks along the
//! bottom of the window, under the canvas, or floats over it. All actions run the engine
//! `timeline.*` and `animation.*` commands, so the control channel drives it the same way.

use egui::{RichText, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Timeline panel view state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TimelineUi {
    pub open: bool,
    /// Floats over the canvas instead of docking along the bottom of the window.
    #[serde(default)]
    pub floating: bool,
    /// Playing back (auto-advancing the playhead at the frame rate).
    #[serde(default)]
    pub playing: bool,
    /// egui time (seconds) of the last auto-advance.
    #[serde(default, skip)]
    pub last_time: f64,
    /// Frame animation: passes through the frames played so far, for a finite loop count.
    #[serde(default, skip)]
    pub plays_done: u32,
    /// Frames copied with ⌘C, for ⌘V to paste after the selected frame.
    #[serde(default, skip)]
    pub clipboard: Vec<photocraft_doc::AnimFrame>,
    /// Where the panel was drawn last frame: the timeline's keys work while the pointer is over it.
    #[serde(default, skip)]
    pub panel_rect: Option<egui::Rect>,
}

/// Menu checkmark for Window › Timeline.
pub fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    (id == "window.panel.timeline").then_some(app.ui.timeline.open)
}

/// Ids this module owns (for parity / enablement).
pub fn handles(id: &str) -> bool {
    id == "window.panel.timeline"
}

/// Toggle the Timeline panel.
pub fn menu(app: &mut PhotocraftApp, id: &str, _params: &Value) -> Option<Result<Value, String>> {
    if id == "window.panel.timeline" {
        app.ui.timeline.open = !app.ui.timeline.open;
        return Some(Ok(json!({ "open": app.ui.timeline.open })));
    }
    None
}

/// Docked height of the panel, in points: room for the frame strip and its bar.
const DOCK_HEIGHT: f32 = 168.0;

/// What the panel's contents asked for this frame.
#[derive(Default)]
struct Out {
    acts: Vec<crate::frame_strip_ui::Act>,
    act: Option<(&'static str, Value)>,
    close: bool,
    toggle_play: bool,
    toggle_float: bool,
    rect: Option<egui::Rect>,
}

/// The panel's contents, the same whether it is docked or floating.
fn contents(app: &PhotocraftApp, ui: &mut egui::Ui, ctx: &egui::Context, out: &mut Out) {
    let t = Tokens::get(ctx);
    out.rect = Some(ui.max_rect());
    let playing = app.ui.timeline.playing;
    let floating = app.ui.timeline.floating;
    let tl = app.session.active().and_then(|d| d.doc.timeline.clone());
    let anim = app.session.active().and_then(|d| d.doc.frame_animation.clone());
    let doc = app.session.active().map(|d| (d.doc.clone(), d.revision));

    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Timeline")).strong().color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::icons::button(ui, "x", 20.0, false, tl!("Close")).clicked() {
                out.close = true;
            }
            let (icon, tip) = if floating { ("panels-top-left", tl!("Dock Panel")) } else { ("app-window", tl!("Float Panel")) };
            if crate::icons::button(ui, icon, 20.0, false, tip).clicked() {
                out.toggle_float = true;
            }
        });
    });
    ui.separator();

    if doc.is_none() {
        ui.weak(tl!("Open a document to use the timeline."));
        return;
    }
    if let (Some(a), Some((d, revision))) = (&anim, &doc) {
        crate::frame_strip_ui::body(
            ui,
            ctx,
            crate::frame_strip_ui::Strip { doc: d, revision: *revision, animation: a, playing, acts: &mut out.acts, toggle_play: &mut out.toggle_play },
        );
        return;
    }
    match &tl {
        None => crate::frame_strip_ui::empty(ui, &mut out.acts),
        Some(tl) => {
            ui.horizontal(|ui| {
                let play_icon = if playing { "pause" } else { "play" };
                if crate::icons::button(ui, play_icon, 22.0, playing, if playing { tl!("Pause") } else { tl!("Play") }).clicked() {
                    out.toggle_play = true;
                }
                if crate::icons::button(ui, "chevron-left", 22.0, false, tl!("Previous frame")).clicked() {
                    out.act = Some(("timeline.previousFrame", json!({})));
                }
                ui.label(
                    RichText::new(crate::i18n::fmt(
                        crate::i18n::t("Frame {current} / {total}"),
                        &[("current", &(tl.current + 1).to_string()), ("total", &tl.duration.to_string())],
                    ))
                    .color(t.text)
                    .monospace(),
                );
                if crate::icons::button(ui, "chevron-right", 22.0, false, tl!("Next frame")).clicked() {
                    out.act = Some(("timeline.nextFrame", json!({})));
                }
                ui.separator();
                ui.label(RichText::new(format!("{:.2}s", tl.time())).color(t.text_dim).monospace());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::icons::button(ui, "trash", 20.0, false, tl!("Delete Timeline")).clicked() {
                        out.act = Some(("timeline.delete", json!({})));
                    }
                    ui.label(RichText::new(format!("{:.0} fps", tl.fps)).color(t.text_dim));
                });
            });
            ui.add_space(6.0);
            // Scrubber: frame playhead across the duration.
            if tl.duration > 1 {
                let mut frame = tl.current as f64;
                let resp = ui.add(egui::Slider::new(&mut frame, 0.0..=(tl.duration - 1) as f64).integer().show_value(false).text("playhead"));
                if resp.changed() {
                    out.act = Some(("timeline.setFrame", json!({ "frame": frame as u64 })));
                }
            }
        }
    }
}

/// Carries out what the contents asked for.
fn apply(app: &mut PhotocraftApp, ctx: &egui::Context, out: Out) {
    app.ui.timeline.panel_rect = out.rect;
    if out.close {
        app.ui.timeline.open = false;
        app.ui.timeline.playing = false;
    }
    if out.toggle_float {
        app.ui.timeline.floating = !app.ui.timeline.floating;
    }
    if out.toggle_play {
        toggle_play(app, ctx);
    }
    if let Some((cmd, p)) = out.act {
        let _ = app.run(cmd, p);
    }
    for (cmd, p) in out.acts {
        let _ = app.run(cmd, p);
        crate::frame_strip_ui::ran(app, ctx, cmd);
    }
}

fn toggle_play(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.ui.timeline.playing = !app.ui.timeline.playing;
    app.ui.timeline.last_time = ctx.input(|i| i.time);
    app.ui.timeline.plays_done = 0;
}

/// Runs a frame-animation command from a key, keeping the thumbnails in step.
fn act(app: &mut PhotocraftApp, ctx: &egui::Context, cmd: &'static str, params: Value) {
    let _ = app.run(cmd, params);
    crate::frame_strip_ui::ran(app, ctx, cmd);
}

/// Is the pointer over the Timeline panel? Its keys only work there, so Space, the arrows and ⌘C
/// keep their usual meaning on the canvas and in the other panels.
fn pointer_over(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    app.ui.timeline.open && app.ui.timeline.panel_rect.is_some_and(|r| ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| r.contains(p)))
}

/// Keys for the Timeline, while the pointer is over it (and no text field has the keyboard):
/// Space plays or pauses; ← → step the selected frame, Home / End jump to the first / last;
/// ⌥← ⌥→ move the selected frame; ⌘C copies the selected frames and ⌘V pastes them after the
/// shown frame; Delete removes the selected frames. True when a key was taken.
pub fn keys(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    use egui::{Key, Modifiers};
    if !pointer_over(app, ctx) {
        return false;
    }
    let Some((has_timeline, anim)) = app.session.active().map(|d| (d.doc.timeline.is_some(), d.doc.frame_animation.clone())) else {
        return false;
    };
    let press = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    if (has_timeline || anim.is_some()) && press(Modifiers::NONE, Key::Space) {
        toggle_play(app, ctx);
        return true;
    }
    let Some(a) = anim else { return false };
    let last = a.frames.len().saturating_sub(1);
    // Copy and Paste reach egui as clipboard events, not key presses; take both forms.
    let clip = |ctx: &egui::Context, copy: bool| {
        ctx.input_mut(|i| {
            let n = i.events.len();
            i.events.retain(|e| if copy { !matches!(e, egui::Event::Copy) } else { !matches!(e, egui::Event::Paste(_)) });
            n != i.events.len()
        })
    };
    if clip(ctx, true) | press(Modifiers::COMMAND, Key::C) {
        app.ui.timeline.clipboard = a.selection().into_iter().filter_map(|i| a.frames.get(i).cloned()).collect();
        return true;
    }
    if clip(ctx, false) | press(Modifiers::COMMAND, Key::V) {
        let frames: Vec<Value> =
            app.ui.timeline.clipboard.iter().map(|f| json!({ "delayMs": f.delay_ms, "hidden": f.hidden.iter().map(|l| l.0).collect::<Vec<_>>() })).collect();
        if !frames.is_empty() {
            act(app, ctx, "animation.pasteFrames", json!({ "frames": frames }));
        }
        return true;
    }
    if press(Modifiers::ALT, Key::ArrowLeft) {
        if a.current > 0 {
            act(app, ctx, "animation.moveFrame", json!({ "from": a.current, "to": a.current - 1 }));
        }
        return true;
    }
    if press(Modifiers::ALT, Key::ArrowRight) {
        if a.current < last {
            act(app, ctx, "animation.moveFrame", json!({ "from": a.current, "to": a.current + 1 }));
        }
        return true;
    }
    let target = if press(Modifiers::NONE, Key::ArrowLeft) {
        Some(a.current.saturating_sub(1))
    } else if press(Modifiers::NONE, Key::ArrowRight) {
        Some((a.current + 1).min(last))
    } else if press(Modifiers::NONE, Key::Home) {
        Some(0)
    } else if press(Modifiers::NONE, Key::End) {
        Some(last)
    } else {
        None
    };
    if let Some(frame) = target {
        act(app, ctx, "animation.selectFrame", json!({ "frame": frame }));
        return true;
    }
    if (press(Modifiers::NONE, Key::Delete) || press(Modifiers::NONE, Key::Backspace)) && a.frames.len() > a.selection().len() {
        act(app, ctx, "animation.deleteFrame", json!({}));
        return true;
    }
    false
}

/// The docked panel, along the bottom of the window under the canvas. Add it after the side
/// panels so it spans only the canvas column, and before the central panel.
pub fn dock(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if !app.ui.timeline.open || app.ui.timeline.floating {
        return;
    }
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let mut out = Out::default();
    egui::Panel::bottom("timeline_dock")
        .resizable(true)
        .default_size(DOCK_HEIGHT)
        .size_range(110.0..=420.0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 6)))
        .show(ui, |ui| {
            // The separator is on the canvas side.
            let r = ui.max_rect();
            ui.painter().line_segment([pos2(r.left() - 10.0, r.top() - 6.0), pos2(r.right() + 10.0, r.top() - 6.0)], egui::Stroke::new(1.0, t.separator));
            contents(app, ui, &ctx, &mut out);
        });
    apply(app, &ctx, out);
}

/// The floating panel, when the Timeline is set to float, and playback in either mode.
pub fn windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.timeline.open {
        return;
    }
    if app.ui.timeline.floating {
        let mut out = Out::default();
        crate::analysis_ui::panel_window(app, ctx, "timeline", tl!("Timeline"), vec2(0.0, 520.0), 660.0, |ui| contents(app, ui, ctx, &mut out));
        apply(app, ctx, out);
    }
    playback(app, ctx);
}

/// Playback: a frame animation shows each frame for its own delay, then the loop count decides;
/// a video timeline advances at its frame rate and loops.
fn playback(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // Frame animation playback.
    if app.ui.timeline.playing
        && let Some(a) = app.session.active().and_then(|d| d.doc.frame_animation.clone())
    {
        let now = ctx.input(|i| i.time);
        let period = crate::frame_strip_ui::playback_period(a.frames.get(a.current).map_or(0, |f| f.delay_ms));
        if now - app.ui.timeline.last_time >= period {
            app.ui.timeline.last_time = now;
            match crate::frame_strip_ui::next_playback_frame(a.current, a.frames.len(), a.loops, app.ui.timeline.plays_done) {
                Some((next, plays)) => {
                    app.ui.timeline.plays_done = plays;
                    let _ = app.run("animation.selectFrame", json!({ "frame": next }));
                    crate::frame_strip_ui::ran(app, ctx, "animation.selectFrame");
                }
                None => app.ui.timeline.playing = false,
            }
        }
        ctx.request_repaint();
        return;
    }
    // Playback: auto-advance the playhead at the frame rate (loops).
    if app.ui.timeline.playing {
        if let Some(t) = app.session.active().and_then(|d| d.doc.timeline.clone()) {
            let now = ctx.input(|i| i.time);
            let period = 1.0 / f64::from(t.fps.max(1.0));
            if now - app.ui.timeline.last_time >= period {
                app.ui.timeline.last_time = now;
                let next = (t.current + 1) % t.duration.max(1);
                let _ = app.run("timeline.setFrame", json!({ "frame": next as u64 }));
            }
            ctx.request_repaint();
        } else {
            app.ui.timeline.playing = false;
        }
    }
}
