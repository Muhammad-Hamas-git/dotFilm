//! The main window: owns all state and lays out the panels.

use super::{assets, export_dialog::ExportDialog, inspector, player, timeline};
use crate::core::history::History;
use crate::core::model::{Asset, AssetId, AssetKind, ClipId, Project};
use crate::engine::audio::{AudioClip, AudioOut};
use crate::engine::preview::PreviewRenderer;
use crate::media::cache::{Loaded, MediaCache};
use crate::media::ffmpeg;
use egui::{Key, Modifiers};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const PROJECT_EXT: &str = "cutroom";

pub struct EditorApp {
    pub project: Project,
    pub history: History,
    pub media: MediaCache,
    pub preview: PreviewRenderer,
    pub audio: AudioOut,
    pub playhead: f64,
    pub playing: bool,
    pub selected: Option<ClipId>,
    /// Asset currently being dragged from the assets panel.
    pub dragged_asset: Option<AssetId>,
    pub timeline: timeline::TimelineState,
    pub player: player::PlayerState,
    pub inspector: inspector::InspectorState,
    pub export: ExportDialog,
    project_path: Option<PathBuf>,
    status: Option<(String, Instant)>,
    ffmpeg_ok: bool,
    show_shortcuts: bool,
    audio_signature: u64,
    window_title: String,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        ctx.set_theme(egui::Theme::Dark);
        let mut app = Self {
            project: Project::default(),
            history: History::default(),
            media: MediaCache::new(ctx),
            preview: PreviewRenderer::default(),
            audio: AudioOut::new(),
            playhead: 0.0,
            playing: false,
            selected: None,
            dragged_asset: None,
            timeline: Default::default(),
            player: Default::default(),
            inspector: Default::default(),
            export: Default::default(),
            project_path: None,
            status: None,
            ffmpeg_ok: ffmpeg::available(),
            show_shortcuts: false,
            audio_signature: 0,
            window_title: String::new(),
        };
        for f in files {
            if f.extension().is_some_and(|e| e == PROJECT_EXT) {
                match Project::load(&f) {
                    Ok(p) => app.replace_project(p, Some(f)),
                    Err(e) => app.set_status(format!("Can't open project: {e}")),
                }
            } else {
                app.media.import(f);
            }
        }
        app
    }

    // ---------- actions used by all panels ----------

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    pub fn seek(&mut self, t: f64) {
        self.playhead = t.max(0.0);
        self.audio.seek(self.playhead);
    }

    pub fn toggle_play(&mut self) {
        if self.playing {
            self.audio.pause();
            self.playing = false;
            self.playhead = self.audio.time();
        } else {
            if self.playhead >= self.project.duration() - 0.01 {
                self.seek(0.0);
            }
            self.audio.play(self.playhead);
            self.playing = true;
        }
    }

    pub fn frame_step(&mut self, frames: i32) {
        if self.playing {
            self.toggle_play();
        }
        let f = 1.0 / self.project.fps as f64;
        // Round to the frame grid first so steps stay aligned.
        let t = ((self.playhead / f).round() + frames as f64) * f;
        self.seek(t);
    }

    pub fn import_dialog(&mut self) {
        let files = rfd::FileDialog::new()
            .add_filter("Media", &["mp4", "mov", "mkv", "webm", "avi", "m4v", "png", "jpg", "jpeg", "bmp", "webp", "gif", "mp3", "wav", "m4a", "aac", "flac", "ogg"])
            .add_filter("All files", &["*"])
            .pick_files();
        for f in files.unwrap_or_default() {
            self.media.import(f);
        }
    }

    pub fn add_asset_to_timeline(&mut self, asset: AssetId, track: usize, start: f64) {
        self.history.checkpoint(&self.project);
        if let Some(id) = self.project.add_clip(asset, track, start) {
            self.selected = Some(id);
        }
    }

    pub fn split_at_playhead(&mut self) {
        let t = self.playhead;
        let targets: Vec<ClipId> = match self.selected.and_then(|id| self.project.clip(id)) {
            Some(c) if c.contains(t) => vec![c.id],
            _ => self.project.clips.iter().filter(|c| c.contains(t)).map(|c| c.id).collect(),
        };
        if targets.is_empty() {
            return;
        }
        self.history.checkpoint(&self.project);
        for id in targets {
            self.project.split(id, t);
        }
    }

    pub fn delete_selected(&mut self) {
        if let Some(id) = self.selected.take() {
            self.history.checkpoint(&self.project);
            self.project.remove_clip(id);
        }
    }

    pub fn undo(&mut self) {
        if self.history.undo(&mut self.project) {
            self.drop_selection_if_gone();
        }
    }

    pub fn redo(&mut self) {
        if self.history.redo(&mut self.project) {
            self.drop_selection_if_gone();
        }
    }

    pub fn drop_selection_if_gone(&mut self) {
        if self.selected.is_some_and(|id| self.project.clip(id).is_none()) {
            self.selected = None;
        }
    }

    pub fn remove_asset(&mut self, id: AssetId) {
        self.history.checkpoint(&self.project);
        self.project.remove_asset(id);
        self.history.clear(); // Undo could bring back clips whose asset is gone.
        self.drop_selection_if_gone();
    }

    fn replace_project(&mut self, project: Project, path: Option<PathBuf>) {
        if self.playing {
            self.toggle_play();
        }
        self.project = project;
        self.project_path = path;
        self.history.clear();
        self.media.clear();
        self.preview.reset();
        self.selected = None;
        self.seek(0.0);
        let mut missing = 0;
        for asset in self.project.assets.clone() {
            if asset.path.exists() {
                self.media.load(&asset);
            } else {
                missing += 1;
            }
        }
        if missing > 0 {
            self.set_status(format!("{missing} media file(s) are missing"));
        }
    }

    fn new_project(&mut self) {
        self.replace_project(Project::default(), None);
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Cutroom project", &[PROJECT_EXT]).pick_file() {
            match Project::load(&path) {
                Ok(p) => self.replace_project(p, Some(path)),
                Err(e) => self.set_status(format!("Can't open project: {e}")),
            }
        }
    }

    fn save(&mut self, save_as: bool) {
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("Cutroom project", &[PROJECT_EXT])
                .set_file_name(format!("project.{PROJECT_EXT}"))
                .save_file(),
        };
        if let Some(path) = path {
            match self.project.save(&path) {
                Ok(()) => {
                    self.set_status(format!("Saved {}", path.display()));
                    self.project_path = Some(path);
                }
                Err(e) => self.set_status(format!("Save failed: {e}")),
            }
        }
    }

    // ---------- per-frame work ----------

    fn handle_media_events(&mut self) {
        for msg in self.media.poll() {
            match msg {
                Loaded::Probed { path, result: Ok(info) } => {
                    let id = AssetId(self.project.alloc_id());
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let asset = Asset {
                        id,
                        path,
                        name,
                        kind: info.kind,
                        duration: info.duration,
                        width: info.width,
                        height: info.height,
                        has_audio: info.has_audio,
                    };
                    self.media.load(&asset);
                    self.project.assets.push(asset);
                }
                Loaded::Probed { path, result: Err(e) } => {
                    self.set_status(format!("Can't import {}: {e}", path.display()));
                }
                Loaded::Failed { what } => self.set_status(format!("Load error: {what}")),
                _ => {}
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for f in files {
            self.media.import(f);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let cmd = Modifiers::COMMAND;
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));

        if pressed(shift_cmd, Key::Z) || pressed(cmd, Key::Y) {
            self.redo();
        }
        if pressed(cmd, Key::Z) {
            self.undo();
        }
        if pressed(shift_cmd, Key::S) {
            self.save(true);
        }
        if pressed(cmd, Key::S) {
            self.save(false);
        }
        if pressed(cmd, Key::O) {
            self.open_dialog();
        }
        if pressed(cmd, Key::I) {
            self.import_dialog();
        }
        if pressed(cmd, Key::E) {
            self.export.open = true;
        }
        if pressed(Modifiers::NONE, Key::Space) {
            self.toggle_play();
        }
        if pressed(Modifiers::NONE, Key::S) {
            self.split_at_playhead();
        }
        if pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace) {
            self.delete_selected();
        }
        if pressed(Modifiers::NONE, Key::ArrowLeft) {
            self.frame_step(-1);
        }
        if pressed(Modifiers::NONE, Key::ArrowRight) {
            self.frame_step(1);
        }
        if pressed(Modifiers::SHIFT, Key::ArrowLeft) {
            self.frame_step(-(self.project.fps as i32));
        }
        if pressed(Modifiers::SHIFT, Key::ArrowRight) {
            self.frame_step(self.project.fps as i32);
        }
        if pressed(Modifiers::NONE, Key::Home) {
            self.seek(0.0);
        }
        if pressed(Modifiers::NONE, Key::End) {
            self.seek(self.project.duration());
        }
        if pressed(Modifiers::NONE, Key::Escape) {
            self.selected = None;
        }
        if pressed(Modifiers::NONE, Key::Plus) || pressed(Modifiers::NONE, Key::Equals) {
            self.timeline.zoom(1.25);
        }
        if pressed(Modifiers::NONE, Key::Minus) {
            self.timeline.zoom(0.8);
        }
    }

    /// Keeps the playhead moving and stops at the end.
    fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.playing {
            return;
        }
        self.playhead = self.audio.time();
        let end = self.project.duration();
        if self.playhead >= end {
            self.audio.pause();
            self.playing = false;
            self.seek(end);
        }
        ctx.request_repaint();
    }

    /// Sends the audible clips to the mixer when anything relevant changed.
    fn sync_audio(&mut self) {
        let mut h = DefaultHasher::new();
        for c in &self.project.clips {
            (c.id.0, c.track, c.start.to_bits(), c.source_in.to_bits(), c.duration.to_bits(), c.volume.to_bits()).hash(&mut h);
        }
        for t in &self.project.tracks {
            t.muted.hash(&mut h);
        }
        self.media.audio.len().hash(&mut h);
        let sig = h.finish();
        if sig == self.audio_signature {
            return;
        }
        self.audio_signature = sig;
        let clips = self
            .project
            .clips
            .iter()
            .filter(|c| self.project.tracks.get(c.track).is_some_and(|t| !t.muted))
            .filter_map(|c| {
                Some(AudioClip {
                    start: c.start,
                    source_in: c.source_in,
                    duration: c.duration,
                    volume: c.volume,
                    pcm: self.media.audio.get(&c.asset)?.clone(),
                })
            })
            .collect();
        self.audio.set_clips(clips);
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self
            .project_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        let title = format!("{name} - Cutroom");
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New project").clicked() {
                    self.new_project();
                }
                if ui.button("Open project...      Ctrl+O").clicked() {
                    self.open_dialog();
                }
                if ui.button("Save                 Ctrl+S").clicked() {
                    self.save(false);
                }
                if ui.button("Save as...     Ctrl+Shift+S").clicked() {
                    self.save(true);
                }
                ui.separator();
                if ui.button("Import media...      Ctrl+I").clicked() {
                    self.import_dialog();
                }
                if ui.button("Export video...      Ctrl+E").clicked() {
                    self.export.open = true;
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo     Ctrl+Z")).clicked() {
                    self.undo();
                }
                if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo     Ctrl+Y")).clicked() {
                    self.redo();
                }
                ui.separator();
                if ui.button("Split at playhead     S").clicked() {
                    self.split_at_playhead();
                }
                if ui.add_enabled(self.selected.is_some(), egui::Button::new("Delete clip     Del")).clicked() {
                    self.delete_selected();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    self.show_shortcuts = true;
                }
            });

            if let Some((msg, at)) = &self.status {
                if at.elapsed() < Duration::from_secs(6) {
                    ui.separator();
                    ui.label(egui::RichText::new(msg).color(egui::Color32::from_rgb(240, 200, 120)));
                    ui.ctx().request_repaint_after(Duration::from_secs(1));
                }
            }
            if self.media.pending > 0 {
                ui.separator();
                ui.spinner();
                ui.label(format!("Loading media ({})", self.media.pending));
            }
        });
    }

    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Keyboard shortcuts").open(&mut self.show_shortcuts).resizable(false).show(ctx, |ui| {
            egui::Grid::new("keys").striped(true).show(ui, |ui| {
                for (k, v) in [
                    ("Space", "Play / pause"),
                    ("Left / Right", "Previous / next frame"),
                    ("Shift + Left / Right", "Jump one second"),
                    ("Home / End", "Go to start / end"),
                    ("S", "Split at playhead"),
                    ("Delete", "Delete selected clip"),
                    ("Ctrl + Z / Ctrl + Y", "Undo / redo"),
                    ("+ / -", "Zoom timeline"),
                    ("Ctrl + mouse wheel", "Zoom timeline at cursor"),
                    ("Shift + mouse wheel", "Scroll timeline"),
                    ("Esc", "Deselect"),
                ] {
                    ui.label(egui::RichText::new(k).strong());
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
    }

    fn ffmpeg_warning(&mut self, ctx: &egui::Context) {
        egui::Window::new("FFmpeg not found").collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label("Cutroom needs ffmpeg.exe and ffprobe.exe to read and export video.");
            ui.label("Install it with this command in a terminal, then restart Cutroom:");
            ui.code("winget install Gyan.FFmpeg");
            ui.label("Or copy ffmpeg.exe and ffprobe.exe into the same folder as cutroom.exe.");
            if ui.button("Check again").clicked() {
                self.ffmpeg_ok = ffmpeg::available();
            }
        });
    }
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_media_events();
        self.handle_dropped_files(&ctx);
        self.handle_shortcuts(&ctx);
        self.advance_playback(&ctx);
        self.sync_audio();
        self.update_title(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("timeline").resizable(true).default_size(300.0).min_size(160.0).show(ui, |ui| {
            timeline::show(self, ui);
        });
        egui::Panel::left("assets").resizable(true).default_size(280.0).min_size(180.0).show(ui, |ui| {
            assets::show(self, ui);
        });
        egui::Panel::right("inspector").resizable(true).default_size(290.0).min_size(220.0).show(ui, |ui| {
            inspector::show(self, ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            player::show(self, ui);
        });

        // Floating label that follows the cursor while dragging an asset.
        if let Some(id) = self.dragged_asset {
            if let (Some(pos), Some(asset)) = (ctx.pointer_latest_pos(), self.project.asset(id)) {
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("drag-ghost")));
                let text = match asset.kind {
                    AssetKind::Audio => format!("Audio: {}", asset.name),
                    _ => asset.name.clone(),
                };
                painter.text(pos + egui::vec2(14.0, 8.0), egui::Align2::LEFT_TOP, text, egui::FontId::proportional(13.0), egui::Color32::WHITE);
            }
            if ctx.input(|i| i.pointer.any_released()) {
                self.dragged_asset = None;
            }
        }

        if let Some(msg) = self.export.show(&ctx, &self.project) {
            self.set_status(msg);
        }
        self.shortcuts_window(&ctx);
        if !self.ffmpeg_ok {
            self.ffmpeg_warning(&ctx);
        }
    }
}
