# Notes for AI coding assistants

Read README.md first for the folder layout.

## Architecture rules

- `core/` is plain data and math. It must never import `egui`, `eframe`, `wgpu`, `cpal` or anything
  from `media/`, `engine/` or `ui/`.
- **One renderer for everything.** Preview and export both call `engine::scene::layers_at` and draw with
  `engine::gpu::compositor::Compositor`. Never add a visual feature to only one of them, and never
  render pictures with ffmpeg filters (ffmpeg only decodes and encodes).
- Per-pixel work goes in `engine/gpu/shaders.wgsl`. Keep `color::apply` (CPU reference) in sync with the
  shader's color math; tests compare them.
- Layer positions are in **project pixels**. The compositor maps them to its output size via `set_canvas`.
- Every user-adjustable number on a clip is an `Anim` (keyframeable). Add new ones to `Param` so the
  inspector, timeline diamonds and undo work automatically. Keyframe times are **source time**
  (`clip.local(t)`), not timeline time.
- The UI changes the project only through `core::model::Project` methods or direct field edits.
  **Before any edit the user can undo, call `app.history.checkpoint(&app.project)` once.**
  For drags, checkpoint at drag start, not on every frame.
- Never block the UI thread on ffmpeg. Long work runs on a thread and reports back
  through a channel (see `media/cache.rs`) or shared state (see `engine/export.rs`, `media/proxy.rs`).
- Never lock or allocate in a way that can stall inside the audio callback (`engine/audio.rs`).
- New fields in saved structs need `#[serde(default)]` so older project files still open.
  Breaking changes bump `PROJECT_VERSION` and get an upgrade step in `Project::upgrade`.
- Child processes are created only through `media::ffmpeg::command` (hides console windows on Windows).

## Conventions

- Tunable numbers are named constants at the top of the file, with a comment.
- Times are `f64` seconds. Positions inside the frame are `f32` project pixels.
- **Track 0 is the bottom layer** and is drawn first; the timeline shows it as the bottom row.

## Checking your work

- `cargo build` must have no errors and no warnings.
- `cargo test` must pass. GPU tests skip without a GPU unless `DOTFILM_REQUIRE_GPU=1` (CI sets it).
- Add a test in `core/` for new editing operations, and a GPU test in `engine/gpu/mod.rs` for new shader work.
