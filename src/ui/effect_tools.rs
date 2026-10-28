//! Effect tools on the player: drag the center of radial blurs, bulges, fisheyes
//! and light rays, and paint liquify with a brush. Positions are in the effect's
//! own frame (see core::geometry::EffectFrame), so they follow the clip and its masks.

use super::app::EditorApp;
use crate::core::content::{Content, EffectKind};
use crate::core::geometry::{self, EffectFrame};
use crate::core::mask;
use crate::core::model::{ClipId, Param};
use crate::core::warp::WarpGrid;
use crate::engine::scene;
use egui::{vec2, Align2, Color32, CursorIcon, FontId, Painter, Pos2, Response, Stroke, Ui};

// ----- Values you can tune -----
/// Size of a center handle, screen pixels.
const CENTER_HANDLE: f32 = 9.0;
/// How close (pixels) a press must be to grab a center handle.
const CENTER_GRAB: f32 = 12.0;
/// Center handle color.
const HANDLE_COLOR: Color32 = Color32::from_rgb(255, 200, 60);
/// Brush size range (fraction of the frame width) and default.
pub const BRUSH_SIZE: (f32, f32, f32) = (0.01, 0.6, 0.12);
/// Brush strength default (0..1).
pub const BRUSH_STRENGTH: f32 = 0.6;
/// How fast Restore undoes the warp per brush step (0..1).
const RESTORE_RATE: f32 = 0.15;

/// An effect: one of a clip's own effects, or (effect = None) an effect layer clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub clip: ClipId,
    pub effect: Option<usize>,
}

#[derive(Clone, Copy)]
enum Drag {
    Center { target: Target, x: u8, y: u8 },
    Paint { last: [f32; 2] },
}

pub struct EffectTools {
    /// Liquify effect being painted on the player.
    pub brush: Option<Target>,
    pub size: f32,
    pub strength: f32,
    pub restore: bool,
    drag: Option<Drag>,
}

impl Default for EffectTools {
    fn default() -> Self {
        Self { brush: None, size: BRUSH_SIZE.2, strength: BRUSH_STRENGTH, restore: false, drag: None }
    }
}

/// The effect's settings: kind and the clip that holds it.
fn kind_of(app: &EditorApp, t: Target) -> Option<EffectKind> {
    let c = app.project.clip(t.clip)?;
    match t.effect {
        Some(i) => c.effects.get(i).filter(|e| e.enabled).map(|e| e.effect.kind),
        None => match &c.content {
            Some(Content::Effect(e)) => Some(e.kind),
            _ => None,
        },
    }
}

/// The area the effect treats as the picture at timeline time `t` (what the GPU uses).
pub fn effect_frame(app: &mut EditorApp, target: Target, t: f64) -> Option<EffectFrame> {
    let clip = app.project.clip(target.clip)?.clone();
    let canvas = (app.project.width as f32, app.project.height as f32);
    let local = clip.local(clip.chop(t.clamp(clip.start, clip.end() - 1e-6)));
    match target.effect {
        None => {
            let rect = mask::bounds(&clip.masks, local, canvas).unwrap_or(geometry::FULL_RECT);
            Some(EffectFrame::new(&scene::full_quad(canvas), rect, canvas))
        }
        Some(_) => {
            let frame_scale = app.preview.frame_scale(&app.project);
            let (size, quad) = scene::layer_space(&app.project, &clip, t, &mut app.preview.text, frame_scale)?;
            let masks = mask::bounds(&clip.masks, local, size);
            let matte_on = clip.matte.as_ref().is_some_and(|m| m.enabled && !m.invert);
            let matte = if matte_on { app.preview.matte_bounds(clip.id) } else { None };
            let restore = clip.matte.as_ref().is_some_and(|m| m.restore);
            Some(EffectFrame::new(&quad, geometry::visible_rect(masks, matte, restore), canvas))
        }
    }
}

/// Liquify brush controls, shown under a liquify effect.
pub fn liquify_controls(ui: &mut Ui, tools: &mut EffectTools, target: Target, warp: &mut Option<WarpGrid>, changed: &mut bool) {
    ui.horizontal(|ui| {
        let on = tools.brush == Some(target);
        if ui.selectable_label(on, if on { "Painting (Esc stops)" } else { "Paint in player" }).on_hover_text("Drag in the player to push pixels").clicked() {
            tools.brush = if on { None } else { Some(target) };
        }
        if ui.add_enabled(warp.as_ref().is_some_and(|w| !w.is_empty()), egui::Button::new("Reset")).on_hover_text("Undo all liquify painting").clicked() {
            *warp = None;
            *changed = true;
        }
    });
    egui::Grid::new(("liquify brush", target.clip.0, target.effect)).num_columns(2).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        ui.label("Brush size");
        ui.add(egui::Slider::new(&mut tools.size, BRUSH_SIZE.0..=BRUSH_SIZE.1).fixed_decimals(2));
        ui.end_row();
        ui.label("Strength");
        ui.add(egui::Slider::new(&mut tools.strength, 0.05..=1.0).fixed_decimals(2));
        ui.end_row();
        ui.label("Brush");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut tools.restore, false, "Push");
            ui.selectable_value(&mut tools.restore, true, "Restore");
        });
        ui.end_row();
    });
}

/// The warp grid of a liquify target.
fn warp_mut(app: &mut EditorApp, t: Target) -> Option<&mut Option<WarpGrid>> {
    let c = app.project.clip_mut(t.clip)?;
    match t.effect {
        Some(i) => c.effects.get_mut(i).map(|e| &mut e.effect.warp),
        None => match &mut c.content {
            Some(Content::Effect(e)) => Some(&mut e.warp),
            _ => None,
        },
    }
}

/// Draws center handles and the liquify brush and handles their input. Returns true
/// if it took over the player's input this frame.
pub(super) fn player(app: &mut EditorApp, ui: &Ui, response: &Response, painter: &Painter, to_screen: &dyn Fn(f32, f32) -> Pos2, to_project: &dyn Fn(Pos2) -> (f32, f32)) -> bool {
    let t = app.playhead;
    let (pw, ph) = (app.project.width as f32, app.project.height as f32);
    let screen_of = |f: &EffectFrame, u: f32, v: f32| {
        let c = f.from_local(u, v);
        to_screen(c[0] * pw, c[1] * ph)
    };
    let frame_of = |f: &EffectFrame, p: Pos2| {
        let (x, y) = to_project(p);
        f.to_local(x / pw, y / ph)
    };

    // ---- liquify brush ----
    if let Some(target) = app.player.effects.brush {
        let valid = app.selected == Some(target.clip) && kind_of(app, target) == Some(EffectKind::Liquify);
        if !valid || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.player.effects.brush = None;
            app.player.effects.drag = None;
        } else if let Some(f) = effect_frame(app, target, t) {
            let tools = &app.player.effects;
            let (size, strength, restore) = (tools.size, tools.strength, tools.restore);
            let aspect = f.size[0] / f.size[1].max(1.0);
            // Brush outline: `size` of the frame's width, round on screen.
            if let Some(p) = response.hover_pos() {
                let a = screen_of(&f, 0.0, 0.5);
                let b = screen_of(&f, 1.0, 0.5);
                let r = a.distance(b) * size;
                painter.circle_stroke(p, r, Stroke::new(1.5, Color32::WHITE));
                painter.circle_stroke(p, r, Stroke::new(0.5, Color32::BLACK));
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
            painter.text(to_screen(pw / 2.0, 0.0) + vec2(0.0, 14.0), Align2::CENTER_TOP, if restore { "Liquify: drag to restore (Esc stops)" } else { "Liquify: drag to push pixels (Esc stops)" }, FontId::proportional(14.0), Color32::WHITE);
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(o) = ui.input(|i| i.pointer.press_origin()) {
                    app.history.checkpoint(&app.project);
                    app.player.effects.drag = Some(Drag::Paint { last: frame_of(&f, o) });
                }
            }
            if let (Some(Drag::Paint { last }), true, Some(p)) = (app.player.effects.drag, response.dragged_by(egui::PointerButton::Primary), response.interact_pointer_pos()) {
                let now = frame_of(&f, p);
                if let Some(w) = warp_mut(app, target) {
                    let grid = w.get_or_insert_with(WarpGrid::default);
                    if restore {
                        grid.restore(now, size, strength * RESTORE_RATE, aspect);
                    } else {
                        grid.push(now, [now[0] - last[0], now[1] - last[1]], size, strength, aspect);
                    }
                }
                app.player.effects.drag = Some(Drag::Paint { last: now });
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                app.player.effects.drag = None;
            }
            return true;
        }
    }

    // ---- center handles of the selected clip's effects ----
    let Some(id) = app.selected else { return false };
    let Some(clip) = app.project.clip(id) else { return false };
    let mut targets: Vec<Target> = Vec::new();
    if clip.content.as_ref().is_some_and(|c| c.is_effect()) {
        targets.push(Target { clip: id, effect: None });
    }
    targets.extend((0..clip.effects.len()).map(|i| Target { clip: id, effect: Some(i) }));
    let local = clip.local(clip.chop(t.clamp(clip.start, clip.end() - 1e-6)));
    let mut handles: Vec<(Target, u8, u8, Pos2, &'static str)> = Vec::new();
    for target in targets {
        let Some(kind) = kind_of(app, target) else { continue };
        let Some((px, py)) = kind.center_params() else { continue };
        let Some(f) = effect_frame(app, target, t) else { continue };
        let c = app.project.clip(id).unwrap();
        let param = |i: u8| match target.effect {
            Some(e) => Param::ClipEffect(e as u8, i),
            None => Param::Content(i),
        };
        let (u, v) = (c.param(param(px)).eval(local), c.param(param(py)).eval(local));
        handles.push((target, px, py, screen_of(&f, u, v), kind.label()));
    }
    if handles.is_empty() && !matches!(app.player.effects.drag, Some(Drag::Center { .. })) {
        return false;
    }
    for (_, _, _, p, label) in &handles {
        painter.circle_stroke(*p, CENTER_HANDLE, Stroke::new(2.0, HANDLE_COLOR));
        painter.line_segment([*p - vec2(CENTER_HANDLE * 1.6, 0.0), *p + vec2(CENTER_HANDLE * 1.6, 0.0)], Stroke::new(1.0, HANDLE_COLOR));
        painter.line_segment([*p - vec2(0.0, CENTER_HANDLE * 1.6), *p + vec2(0.0, CENTER_HANDLE * 1.6)], Stroke::new(1.0, HANDLE_COLOR));
        painter.text(*p + vec2(CENTER_HANDLE + 4.0, -CENTER_HANDLE - 4.0), Align2::LEFT_BOTTOM, *label, FontId::proportional(11.0), HANDLE_COLOR);
    }
    let hit = |p: Pos2| handles.iter().filter(|h| h.3.distance(p) <= CENTER_GRAB).min_by(|a, b| a.3.distance(p).total_cmp(&b.3.distance(p))).map(|h| (h.0, h.1, h.2));
    if response.hover_pos().and_then(hit).is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Move);
    }
    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some((target, x, y)) = ui.input(|i| i.pointer.press_origin()).and_then(hit) {
            app.history.checkpoint(&app.project);
            app.player.effects.drag = Some(Drag::Center { target, x, y });
        }
    }
    if let Some(Drag::Center { target, x, y }) = app.player.effects.drag {
        if response.dragged_by(egui::PointerButton::Primary) {
            if let (Some(p), Some(f)) = (response.interact_pointer_pos(), effect_frame(app, target, t)) {
                let uv = frame_of(&f, p);
                let tol = app.project.key_tolerance();
                if let Some(c) = app.project.clip_mut(target.clip) {
                    let param = |i: u8| match target.effect {
                        Some(e) => Param::ClipEffect(e as u8, i),
                        None => Param::Content(i),
                    };
                    c.param_mut(param(x)).set(local, uv[0].clamp(0.0, 1.0), tol);
                    c.param_mut(param(y)).set(local, uv[1].clamp(0.0, 1.0), tol);
                }
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            app.player.effects.drag = None;
        }
        return true;
    }
    false
}

/// Effects offered in menus, grouped (motion blur only as a layer: `layer_only`).
pub fn effect_menu(ui: &mut Ui, layer: bool) -> Option<EffectKind> {
    let mut picked = None;
    for g in crate::core::content::EffectGroup::ALL {
        ui.menu_button(g.label(), |ui| {
            for k in EffectKind::ALL.into_iter().filter(|k| k.group() == g && (layer || *k != EffectKind::MotionBlur)) {
                if ui.button(k.label()).clicked() {
                    picked = Some(k);
                    ui.close();
                }
            }
        });
    }
    picked
}
