//! Assets panel: imported files as a grid of thumbnails.
//! Import with the button or by dropping files onto the window.
//! Drag an item onto the timeline (or the player), or double-click to add it at the playhead.

use super::app::EditorApp;
use super::timeline::format_time;
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
                match app.media.thumbs.get(&asset.id) {
                    Some(tex) => {
                        let s = tex.size_vec2();
                        let k = (thumb_area.width() / s.x).min(thumb_area.height() / s.y);
                        let r = Rect::from_center_size(thumb_area.center(), s * k);
                        painter.image(tex.id(), r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => {
                        let label = match asset.kind {
                            AssetKind::Audio => "AUDIO",
                            AssetKind::Image => "IMAGE",
                            AssetKind::Video => "...",
                        };
                        painter.text(thumb_area.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), Color32::from_gray(140));
                    }
                }
                // Duration badge.
                if asset.kind != AssetKind::Image {
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
                if !asset.path.exists() {
                    painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(2.0, Color32::RED), StrokeKind::Inside);
                }

                if response.drag_started() {
                    app.dragged_asset = Some(asset.id);
                }
                if response.double_clicked() {
                    add = Some(asset.id);
                }
                let response = response.on_hover_text(format!(
                    "{}\n{}x{}  {}{}",
                    asset.path.display(),
                    asset.width,
                    asset.height,
                    format_time(asset.duration),
                    if asset.has_audio { "  with audio" } else { "" }
                ));
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
