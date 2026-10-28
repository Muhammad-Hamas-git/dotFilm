//! Assets panel: imported files as a grid of thumbnails.
//! Import with the button or by dropping files onto the window.
//! Click an item to preview it in the player, double-click to add it at the playhead,
//! or drag it onto the timeline (or the player).

use super::app::EditorApp;
use super::timeline::format_time;
use crate::core::content::Content;
use crate::core::model::{AssetId, AssetKind};
use crate::media::proxy::{self, ProxyState};
use egui::{vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui};

/// Size of one tile in the grid.
const TILE: egui::Vec2 = vec2(124.0, 104.0);

pub fn show(app: &mut EditorApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.heading("Assets");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Import...").clicked() {
                app.import_dialog();
            }
            ui.menu_button("New", |ui| {
                let mut made = None;
                if ui.button("Color").clicked() {
                    made = Some(Content::color());
                }
                if ui.button("Gradient").clicked() {
                    made = Some(Content::gradient());
                }
                if ui.button("Text").clicked() {
                    made = Some(Content::text());
                }
                ui.menu_button("Effect", |ui| {
                    if let Some(k) = super::effect_tools::effect_menu(ui, true) {
                        made = Some(Content::effect(k));
                    }
                });
                if let Some(content) = made {
                    app.project.add_generated_asset(content);
                    ui.close();
                }
            });
        });
    });
    ui.separator();

    let files_hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());

    if app.project.assets.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Drop video, image or\naudio files here").size(15.0).weak());
            ui.add_space(8.0);
            ui.label(egui::RichText::new("or click Import").weak());
        });
    }

    let mut add: Option<AssetId> = None;
    let mut remove: Option<AssetId> = None;
    let mut preview: Option<AssetId> = None;
    let previewing = app.source.as_ref().map(|s| s.asset);

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for asset in &app.project.assets {
                let (rect, response) = ui.allocate_exact_size(TILE, Sense::click_and_drag());
                let painter = ui.painter_at(rect);
                let hovered = response.hovered();
                painter.rect_filled(rect, CornerRadius::same(6), if hovered { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().faint_bg_color });

                // Thumbnail area.
                let thumb_area = Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(TILE.x - 8.0, TILE.y - 36.0));
                painter.rect_filled(thumb_area, CornerRadius::same(4), Color32::from_gray(20));
                if let Some(g) = &asset.generator {
                    generated_thumb(&painter, thumb_area, g);
                }
                match app.media.thumbs.get(&asset.id).filter(|_| asset.generator.is_none()) {
                    Some(tex) => {
                        let s = tex.size_vec2();
                        let k = (thumb_area.width() / s.x).min(thumb_area.height() / s.y);
                        let r = Rect::from_center_size(thumb_area.center(), s * k);
                        painter.image(tex.id(), r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => {
                        let label = match asset.kind {
                            _ if asset.generator.is_some() => "",
                            AssetKind::Audio => "AUDIO",
                            AssetKind::Image => "IMAGE",
                            AssetKind::Video => "...",
                            AssetKind::Generated => "",
                        };
                        painter.text(thumb_area.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), Color32::from_gray(140));
                    }
                }
                // Duration badge.
                if asset.kind.is_timed() {
                    painter.text(
                        thumb_area.right_bottom() - vec2(4.0, 3.0),
                        Align2::RIGHT_BOTTOM,
                        format_time(asset.duration),
                        FontId::monospace(10.0),
                        Color32::WHITE,
                    );
                }
                // File name (clipped to the tile).
                let name_painter = painter.with_clip_rect(rect.shrink(4.0));
                name_painter.text(
                    egui::pos2(rect.left() + 6.0, thumb_area.bottom() + 6.0),
                    Align2::LEFT_TOP,
                    &asset.name,
                    FontId::proportional(12.0),
                    ui.visuals().text_color(),
                );
                // Proxy status badge.
                let badge = match app.proxies.state(asset.id) {
                    Some(ProxyState::Queued) => Some(("Proxy queued".to_string(), Color32::from_gray(160))),
                    Some(ProxyState::Building(p)) => Some((format!("Proxy {:.0}%", p * 100.0), Color32::from_rgb(240, 200, 120))),
                    Some(ProxyState::Ready(_)) if app.proxies.enabled => Some(("Proxy".to_string(), Color32::from_rgb(120, 210, 140))),
                    Some(ProxyState::Failed(_)) => Some(("Proxy failed".to_string(), Color32::LIGHT_RED)),
                    _ => None,
                };
                if let Some((text, color)) = badge {
                    let pos = thumb_area.left_top() + vec2(4.0, 3.0);
                    let galley = painter.layout_no_wrap(text, FontId::proportional(10.0), color);
                    painter.rect_filled(Rect::from_min_size(pos - vec2(2.0, 1.0), galley.size() + vec2(4.0, 2.0)), 3.0, Color32::from_black_alpha(180));
                    painter.galley(pos, galley, color);
                }
                if asset.kind != AssetKind::Generated && !asset.path.exists() {
                    painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(2.0, Color32::RED), StrokeKind::Inside);
                }

                if response.drag_started() {
                    app.dragged_asset = Some(asset.id);
                }
                if response.double_clicked() {
                    add = Some(asset.id);
                } else if response.clicked() {
                    preview = Some(asset.id);
                }
                if previewing == Some(asset.id) {
                    painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(2.0, Color32::from_rgb(255, 196, 64)), StrokeKind::Inside);
                }
                let tip = if asset.generator.is_some() {
                    "Click to preview. Double-click to add at the playhead, or drag onto the timeline.".to_string()
                } else {
                    format!(
                        "{}\n{}x{}  {}{}\nClick to preview, double-click to add at the playhead.",
                        asset.path.display(),
                        asset.width,
                        asset.height,
                        format_time(asset.duration),
                        if asset.has_audio { "  with audio" } else { "" }
                    )
                };
                let response = response.on_hover_text(tip);
                response.context_menu(|ui| {
                    if ui.button("Add at playhead").clicked() {
                        add = Some(asset.id);
                        ui.close();
                    }
                    if proxy::needs_proxy(asset) && ui.button("Rebuild proxy").clicked() {
                        app.proxies.rebuild(asset);
                        ui.close();
                    }
                    if ui.button("Remove from project").clicked() {
                        remove = Some(asset.id);
                        ui.close();
                    }
                });
            }
        });
    });

    if let Some(id) = add {
        app.add_asset_to_timeline(id, 0, app.playhead);
    } else if let Some(id) = preview {
        app.open_source(id);
    }
    if let Some(id) = remove {
        app.remove_asset(id);
    }

    // Highlight the panel while files are dragged over the window.
    if files_hovering {
        let r = ui.max_rect();
        ui.painter().rect_stroke(r, 6.0, Stroke::new(2.0, ui.visuals().selection.bg_fill), StrokeKind::Inside);
    }
}

fn color32(c: [f32; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, (c[3] * 255.0) as u8)
}

/// Preview tile for colors, gradients, text and effects.
fn generated_thumb(painter: &egui::Painter, r: Rect, content: &Content) {
    match content {
        Content::Compound(c) => {
            painter.rect_filled(r, CornerRadius::same(4), Color32::from_rgb(40, 60, 90));
            // Stacked bars: one per inner track.
            let n = c.project.tracks.len().clamp(1, 4);
            for i in 0..n {
                let y = r.bottom() - 10.0 - i as f32 * 9.0;
                painter.rect_filled(Rect::from_min_max(egui::pos2(r.left() + 10.0 + i as f32 * 6.0, y - 6.0), egui::pos2(r.right() - 10.0, y)), 2.0, Color32::from_rgb(90, 140, 210));
            }
            painter.text(egui::pos2(r.center().x, r.top() + 14.0), egui::Align2::CENTER_CENTER, format!("{} clips", c.project.clips.len()), egui::FontId::proportional(11.0), Color32::WHITE);
        }
        Content::Color(c) => {
            painter.rect_filled(r, CornerRadius::same(4), color32(c.eval(0.0)));
        }
        Content::Gradient(g) => {
            // Left-to-right bands through the stops (enough for a thumbnail).
            let n = g.stops.clamp(2, 3) as usize;
            let mut mesh = egui::Mesh::default();
            for i in 0..n {
                let x = r.left() + r.width() * i as f32 / (n - 1) as f32;
                let c = color32(g.colors[i].eval(0.0));
                mesh.colored_vertex(egui::pos2(x, r.top()), c);
                mesh.colored_vertex(egui::pos2(x, r.bottom()), c);
            }
            for i in 0..(n - 1) as u32 {
                let b = i * 2;
                mesh.add_triangle(b, b + 1, b + 2);
                mesh.add_triangle(b + 1, b + 3, b + 2);
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        Content::Text(t) => {
            let preview: String = t.text.chars().take(10).collect();
            painter.text(r.center(), Align2::CENTER_CENTER, if preview.is_empty() { "T".into() } else { preview }, FontId::proportional(18.0), color32(t.fill.eval(0.0)));
        }
        Content::Effect(e) => {
            painter.text(r.center() - vec2(0.0, 8.0), Align2::CENTER_CENTER, "FX", FontId::proportional(20.0), Color32::from_rgb(240, 170, 80));
            painter.text(r.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER, e.kind.label(), FontId::proportional(10.0), Color32::from_gray(180));
        }
    }
}
