//! Window › Timeline in frame-animation mode: create and drive the document's [`FrameAnimation`]
//! (the frame strip). Each frame holds a delay and which layers it shows; selecting a frame shows
//! it on the layers. Navigation adds no history step; every change to the frames does one.
//! Visibility edits made while a frame is selected change that frame (see `Session::edit`).
//!
//! Several frames can be selected (Shift-click a range, ⌘-click one more, Select All Frames);
//! `current` is the one the layers show. Delete, duplicate, delay and reverse act on them all.

use std::sync::Arc;

use photocraft_doc::{
    AnimFrame, Document, FrameAnimation, LoopCount,
    frame_animation::{MAX_DELAY_MS, MAX_FRAMES},
};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

fn has_animation(s: &Session) -> std::result::Result<(), String> {
    match s.active() {
        Some(d) if d.doc.frame_animation.is_some() => Ok(()),
        Some(_) => Err("no frame animation (create one first)".into()),
        None => Err("no document".into()),
    }
}

fn bad(cmd: &str, msg: String) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg }
}

fn animation<'a>(doc: &'a mut Document, cmd: &str) -> Result<&'a mut FrameAnimation> {
    doc.frame_animation.as_mut().ok_or_else(|| bad(cmd, "no frame animation (create one first)".into()))
}

/// `frame` (or the selected frame when absent) as an index into `a`.
fn frame_param(cmd: &str, p: &Value, key: &str, a: &FrameAnimation) -> Result<usize> {
    let Some(v) = p.get(key).filter(|v| !v.is_null()) else { return Ok(a.current) };
    v.as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n < a.frames.len())
        .ok_or_else(|| bad(cmd, format!("`{key}` must be a frame from 0 to {}, got {v}", a.frames.len() - 1)))
}

/// A delay in milliseconds from `delayMs` or `seconds`, if either is given.
fn delay_param(cmd: &str, p: &Value) -> Result<Option<u32>> {
    let ms = match (p.get("delayMs").filter(|v| !v.is_null()), p.get("seconds").filter(|v| !v.is_null())) {
        (Some(v), _) => v.as_f64().ok_or_else(|| bad(cmd, format!("`delayMs` must be a number, got {v}")))?,
        (None, Some(v)) => v.as_f64().ok_or_else(|| bad(cmd, format!("`seconds` must be a number, got {v}")))? * 1000.0,
        (None, None) => return Ok(None),
    };
    if !ms.is_finite() || !(0.0..=f64::from(MAX_DELAY_MS)).contains(&ms) {
        return Err(bad(cmd, format!("the delay must be from 0 to {} seconds", MAX_DELAY_MS / 1000)));
    }
    Ok(Some(ms.round() as u32))
}

/// Run `f` on the document without a history step (frame navigation), keeping a clean document
/// clean as selecting a layer does.
fn navigate(s: &mut Session, f: impl FnOnce(&mut Document) -> Result<()>) -> Result<Value> {
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    let mut doc = (*st.doc).clone();
    f(&mut doc)?;
    let clean = st.saved_revision == st.revision;
    st.doc = Arc::new(doc);
    st.revision += 1;
    if clean {
        st.saved_revision = st.revision;
    }
    st.last_damage = None;
    info(s)
}

fn create(s: &mut Session, _p: &Value) -> Result<Value> {
    if s.active().ok_or(EngineError::NoDocument)?.doc.frame_animation.is_some() {
        return Err(EngineError::Other("the document already has a frame animation".into()));
    }
    s.edit("Create Frame Animation", |doc, _| {
        doc.frame_animation = Some(FrameAnimation::new(&doc.layers));
        Ok(())
    })?;
    info(s)
}

fn delete(s: &mut Session, _p: &Value) -> Result<Value> {
    s.edit("Delete Frame Animation", |doc, _| {
        // The layers keep what the selected frame showed.
        doc.frame_animation = None;
        Ok(())
    })?;
    info(s)
}

/// Make Frames From Layers: one frame per top-level layer, each showing that layer alone, the
/// bottom layer first.
fn make_frames_from_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let delay = delay_param("animation.makeFramesFromLayers", p)?.unwrap_or(0);
    let n = s.active().ok_or(EngineError::NoDocument)?.doc.layers.len();
    if n == 0 {
        return Err(EngineError::Other("the document has no layers to make frames from".into()));
    }
    if n > MAX_FRAMES {
        return Err(EngineError::Other(format!("{n} layers is more than the {MAX_FRAMES} frames an animation holds")));
    }
    s.edit("Make Frames From Layers", |doc, _| {
        let mut a = doc.frame_animation.take().unwrap_or_else(|| FrameAnimation::new(&doc.layers));
        a.frames = (0..n)
            .map(|shown| {
                let mut hidden: Vec<_> = doc.layers.iter().enumerate().filter(|(i, _)| *i != shown).map(|(_, l)| l.id).collect();
                hidden.sort();
                AnimFrame { delay_ms: delay, hidden }
            })
            .collect();
        a.current = 0;
        a.apply(0, &mut doc.layers);
        doc.frame_animation = Some(a);
        Ok(())
    })?;
    info(s)
}

/// New Frame (Duplicate Selected Frames): copies of the selected frames after the last of
/// them, and the copies selected.
fn new_frame(s: &mut Session, _p: &Value) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.frame_animation.as_ref().is_some_and(|a| a.frames.len() + a.selection().len() > MAX_FRAMES) {
        return Err(EngineError::Other(format!("an animation holds at most {MAX_FRAMES} frames")));
    }
    s.edit("New Frame", |doc, _| {
        let a = animation(doc, "animation.newFrame")?;
        a.clamp();
        let sel = a.selection();
        let at = sel[sel.len() - 1] + 1;
        let copies: Vec<AnimFrame> = sel.iter().map(|&i| a.frames[i].clone()).collect();
        a.frames.splice(at..at, copies);
        a.current = at + sel.iter().position(|&i| i == a.current).unwrap_or(0);
        a.selected = if sel.len() > 1 { (at..at + sel.len()).collect() } else { Vec::new() };
        Ok(())
    })?;
    info(s)
}

/// Delete Frame: frame `frame`, else every selected frame. One frame always stays.
fn delete_frame(s: &mut Session, p: &Value) -> Result<Value> {
    s.edit("Delete Frame", |doc, _| {
        let a = animation(doc, "animation.deleteFrame")?;
        a.clamp();
        let doomed = match p.get("frame").filter(|v| !v.is_null()) {
            Some(_) => vec![frame_param("animation.deleteFrame", p, "frame", a)?],
            None => a.selection(),
        };
        if doomed.len() >= a.frames.len() {
            return Err(EngineError::Other("an animation keeps at least one frame; delete the animation instead".into()));
        }
        let (cur, first) = (a.current, doomed[0]);
        for &i in doomed.iter().rev() {
            a.frames.remove(i);
        }
        // The selected frame stays selected if it survives, else the frame now where the first
        // deleted one was (or the new last frame).
        let cur = if doomed.binary_search(&cur).is_err() { cur - doomed.iter().filter(|&&i| i < cur).count() } else { first.min(a.frames.len() - 1) };
        doc.select_animation_frame(cur);
        Ok(())
    })?;
    info(s)
}

/// Select Frame: `frame` alone; with `extend` "range" every frame from the selected one to it
/// (Shift-click), or "toggle" adds it to the selection or takes it out (⌘-click).
fn select_frame(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "animation.selectFrame";
    let extend = match p.get("extend") {
        None | Some(Value::Null) => None,
        Some(Value::String(w)) if w == "range" || w == "toggle" => Some(w.clone()),
        Some(v) => return Err(bad(CMD, format!("`extend` must be \"range\" or \"toggle\", got {v}"))),
    };
    navigate(s, |doc| {
        let a = animation(doc, CMD)?;
        a.clamp();
        let v = p.get("frame").ok_or_else(|| bad(CMD, "need `frame`".into()))?;
        let at = v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < a.frames.len())
            .ok_or_else(|| bad(CMD, format!("`frame` must be from 0 to {}, got {v}", a.frames.len() - 1)))?;
        match extend.as_deref() {
            Some("range") => {
                // The selected frame is the anchor: it stays shown, so another Shift-click
                // re-spans from it.
                a.selected = (a.current.min(at)..=a.current.max(at)).collect();
                a.clamp();
            }
            Some("toggle") if a.is_selected(at) => {
                let mut sel = a.selection();
                if sel.len() > 1 {
                    sel.retain(|&i| i != at);
                    // Taking out the shown frame shows the next selected one (else the last).
                    let show = if at == a.current { sel[sel.partition_point(|&i| i < at).min(sel.len() - 1)] } else { a.current };
                    a.current = show;
                    a.selected = sel;
                    a.clamp();
                    let a = a.clone();
                    a.apply(show, &mut doc.layers);
                }
            }
            Some(_) => {
                a.selected = a.selection();
                a.selected.push(at);
                a.current = at;
                a.clamp();
                let a = a.clone();
                a.apply(at, &mut doc.layers);
            }
            None => {
                doc.select_animation_frame(at);
            }
        }
        Ok(())
    })
}

/// Select All Frames; the shown frame stays as it is.
fn select_all_frames(s: &mut Session) -> Result<Value> {
    navigate(s, |doc| {
        let a = animation(doc, "animation.selectAllFrames")?;
        a.clamp();
        a.selected = (0..a.frames.len()).collect();
        a.clamp();
        Ok(())
    })
}

/// Step the selected frame by `delta`; `wrap` goes round the ends (playback), else it stops there.
fn step(s: &mut Session, delta: i64, wrap: bool) -> Result<Value> {
    navigate(s, |doc| {
        let a = animation(doc, "animation.nextFrame")?;
        a.clamp();
        let n = a.frames.len() as i64;
        let to = a.current as i64 + delta;
        let to = if wrap { to.rem_euclid(n) } else { to.clamp(0, n - 1) };
        doc.select_animation_frame(to as usize);
        Ok(())
    })
}

fn set_delay(s: &mut Session, p: &Value) -> Result<Value> {
    let delay = delay_param("animation.setDelay", p)?.ok_or_else(|| bad("animation.setDelay", "need `seconds` or `delayMs`".into()))?;
    s.edit("Frame Delay", |doc, _| {
        let a = animation(doc, "animation.setDelay")?;
        a.clamp();
        let targets: Vec<usize> = match p.get("frames") {
            None | Some(Value::Null) => a.selection(),
            Some(Value::String(w)) if w == "all" => (0..a.frames.len()).collect(),
            Some(Value::Array(v)) => {
                let mut out = Vec::new();
                for i in v {
                    let i = i
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n < a.frames.len())
                        .ok_or_else(|| bad("animation.setDelay", format!("`frames` holds {i}, which is not a frame from 0 to {}", a.frames.len() - 1)))?;
                    out.push(i);
                }
                out
            }
            Some(v) => return Err(bad("animation.setDelay", format!("`frames` must be \"all\" or a list of frames, got {v}"))),
        };
        for i in targets {
            a.frames[i].delay_ms = delay;
        }
        Ok(())
    })?;
    info(s)
}

fn set_loop(s: &mut Session, p: &Value) -> Result<Value> {
    let loops = match p.get("loops") {
        Some(Value::String(w)) if w == "forever" => LoopCount::Forever,
        Some(Value::String(w)) if w == "once" => LoopCount::Once,
        Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) => LoopCount::from_plays(n),
            None => return Err(bad("animation.setLoop", format!("`loops` must be \"once\", \"forever\" or a number of plays, got {v}"))),
        },
        None => return Err(bad("animation.setLoop", "need `loops`".into())),
    };
    s.edit("Loop Count", |doc, _| {
        animation(doc, "animation.setLoop")?.loops = loops;
        Ok(())
    })?;
    info(s)
}

/// Move frame `from` to position `to`; the selection narrows to the frame it was on.
fn move_frame(s: &mut Session, p: &Value) -> Result<Value> {
    s.edit("Move Frame", |doc, _| {
        let a = animation(doc, "animation.moveFrame")?;
        a.clamp();
        a.selected.clear();
        let from = frame_param("animation.moveFrame", p, "from", a)?;
        let to = frame_param("animation.moveFrame", p, "to", a)?;
        let f = a.frames.remove(from);
        a.frames.insert(to, f);
        if a.current == from {
            a.current = to;
        } else if from < a.current && a.current <= to {
            a.current -= 1;
        } else if to <= a.current && a.current < from {
            a.current += 1;
        }
        Ok(())
    })?;
    info(s)
}

/// Paste Frames: copies of `frames` ({delayMs, hidden:[layer ids]}) inserted after frame `after`
/// (the selected one when absent); the last pasted frame is selected. Ids of layers the document
/// does not have are dropped, so a frame copied from another document still pastes. Several
/// pasted frames stay selected.
fn paste_frames(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "animation.pasteFrames";
    let list = p.get("frames").and_then(Value::as_array).filter(|l| !l.is_empty()).ok_or_else(|| bad(CMD, "`frames` must be a non-empty list".into()))?;
    let mut pasted = Vec::with_capacity(list.len());
    for f in list {
        let delay_ms = delay_param(CMD, f)?.unwrap_or(0);
        let ids = match f.get("hidden") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => v
                .as_array()
                .and_then(|l| l.iter().map(Value::as_u64).collect::<Option<Vec<_>>>())
                .ok_or_else(|| bad(CMD, format!("`hidden` must be a list of layer ids, got {v}")))?,
        };
        pasted.push((delay_ms, ids));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.frame_animation.as_ref().is_some_and(|a| a.frames.len() + pasted.len() > MAX_FRAMES) {
        return Err(EngineError::Other(format!("an animation holds at most {MAX_FRAMES} frames")));
    }
    s.edit("Paste Frames", |doc, _| {
        let known = |id: u64| doc.layer(photocraft_doc::LayerId(id)).is_some();
        let frames: Vec<AnimFrame> = pasted
            .iter()
            .map(|(delay_ms, ids)| {
                let mut hidden: Vec<_> = ids.iter().copied().filter(|id| known(*id)).map(photocraft_doc::LayerId).collect();
                hidden.sort();
                hidden.dedup();
                AnimFrame { delay_ms: *delay_ms, hidden }
            })
            .collect();
        let a = animation(doc, CMD)?;
        a.clamp();
        let after = frame_param(CMD, p, "after", a)?;
        let at = after + 1;
        let n = frames.len();
        a.frames.splice(at..at, frames);
        a.current = at + n - 1;
        a.selected = if n > 1 { (at..at + n).collect() } else { Vec::new() };
        let cur = a.current;
        let a = a.clone();
        a.apply(cur, &mut doc.layers);
        Ok(())
    })?;
    info(s)
}

/// Reverse Frames: the selected frames among themselves when several are selected (they keep
/// their places in the strip), else every frame.
fn reverse_frames(s: &mut Session, _p: &Value) -> Result<Value> {
    s.edit("Reverse Frames", |doc, _| {
        let a = animation(doc, "animation.reverseFrames")?;
        a.clamp();
        let slots = if a.selected.len() > 1 { a.selected.clone() } else { (0..a.frames.len()).collect() };
        let picked: Vec<AnimFrame> = slots.iter().rev().map(|&i| a.frames[i].clone()).collect();
        for (&i, f) in slots.iter().zip(picked) {
            a.frames[i] = f;
        }
        if let Ok(k) = slots.binary_search(&a.current) {
            a.current = slots[slots.len() - 1 - k];
        }
        Ok(())
    })?;
    info(s)
}

pub(crate) fn info_of(doc: &Document) -> Value {
    match &doc.frame_animation {
        Some(a) => json!({"animation": {
            "current": a.current,
            "selected": a.selection(),
            "loops": match a.loops { LoopCount::Once => json!("once"), LoopCount::Forever => json!("forever"), LoopCount::Times(n) => json!(n) },
            "durationMs": a.duration_ms(),
            "frames": a.frames.iter().map(|f| json!({"delayMs": f.delay_ms, "hidden": f.hidden.iter().map(|l| l.0).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        }}),
        None => json!({"animation": null}),
    }
}

fn info(s: &mut Session) -> Result<Value> {
    Ok(info_of(&s.active().ok_or(EngineError::NoDocument)?.doc))
}

/// The Timeline panel menu's commands (Photoshop lists them under Panel Menus › Timeline): a
/// shortcut set imports onto them by label, and Edit › Keyboard Shortcuts lists them under
/// Window › Timeline.
pub const PANEL_MENU: &[&str] = &[
    "animation.newFrame",
    "animation.deleteFrame",
    "animation.delete",
    "animation.selectAllFrames",
    "animation.nextFrame",
    "animation.previousFrame",
    "animation.reverseFrames",
    "animation.makeFramesFromLayers",
    "animation.create",
];

macro_rules! spec {
    ($id:expr, $label:expr, $params:expr, $en:expr, $journal:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: $en, journal: $journal, run: $run }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!("animation.create", "Create Frame Animation", "{} → {animation}", has_doc, true, |s, p| create(s, p)),
        spec!("animation.delete", "Delete Frame Animation", "{} → {animation:null}", has_animation, true, |s, p| delete(s, p)),
        spec!("animation.makeFramesFromLayers", "Make Frames From Layers", r#"{seconds?:0, delayMs?} → {animation}"#, has_doc, true, |s, p| {
            make_frames_from_layers(s, p)
        }),
        spec!("animation.newFrame", "New Frame", "{} → {animation}: duplicates the selected frames", has_animation, true, |s, p| new_frame(s, p)),
        spec!("animation.deleteFrame", "Delete Frame", r#"{frame?:selected frames} → {animation}"#, has_animation, true, |s, p| delete_frame(s, p)),
        spec!("animation.selectFrame", "Select Frame", r#"{frame, extend?:"range"|"toggle"} → {animation}"#, has_animation, false, |s, p| select_frame(s, p)),
        spec!("animation.selectAllFrames", "Select All Frames", "{} → {animation}", has_animation, false, |s, _| select_all_frames(s)),
        spec!("animation.nextFrame", "Next Frame", "{} → {animation}", has_animation, false, |s, _| step(s, 1, false)),
        spec!("animation.previousFrame", "Previous Frame", "{} → {animation}", has_animation, false, |s, _| step(s, -1, false)),
        spec!(
            "animation.setDelay",
            "Frame Delay",
            r#"{seconds | delayMs, frames?:[i..] | "all" = selected frames} → {animation}"#,
            has_animation,
            true,
            |s, p| { set_delay(s, p) }
        ),
        spec!("animation.setLoop", "Loop Count", r#"{loops:"once"|"forever"|plays} → {animation}"#, has_animation, true, |s, p| set_loop(s, p)),
        spec!("animation.moveFrame", "Move Frame", r#"{from, to} → {animation}"#, has_animation, true, |s, p| move_frame(s, p)),
        spec!(
            "animation.pasteFrames",
            "Paste Frames",
            r#"{frames:[{delayMs, hidden:[layerId]}], after?:selected} → {animation}"#,
            has_animation,
            true,
            |s, p| { paste_frames(s, p) }
        ),
        spec!("animation.reverseFrames", "Reverse Frames", "{} → {animation}: the selected frames, or all", has_animation, true, |s, p| reverse_frames(s, p)),
        spec!("animation.info", "Frame Animation Info", "{} → {animation}", has_doc, false, |s, _| info(s)),
    ]
}

#[cfg(test)]
mod tests;
