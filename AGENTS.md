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
  inspector, timeline diamonds and undo work automatically. Keyframe times are **clip time**
  (`clip.local(t)` = seconds from the clip start). Footage position is `clip.source_time(t)`, which
  includes speed. Trimming and splitting go through `Clip::trim_start` so keyframes stay attached.
- Generated layers (color, gradient, text, effect) keep their settings in `clip.content`; each number
  there is an `Anim` addressed as `Param::Content(i)` (see `core/content.rs` `index`). Text is drawn as a
  coverage mask and colored in the shader. Effects are `DrawItem::Effect` passes that read the frame so
  far and write the other accumulation texture. Get on-screen outlines from `engine::scene::clip_quad`.
- Grading (white balance, wheels, HSL, chroma key) lives in `clip.grade` (`core/grade.rs`), addressed as
  `Param::Grade(i)` (see `grade::idx`). `Grade::frame` packs it for the shader; flag bits skip unused
  parts. Shader order: source, chroma key, white balance, basic color stages, wheels, HSL.
  Layer uniforms are padded to `UNIFORM_STRIDE` (768); grow it if `LayerUniform` grows past that.
- Transitions live on clip edges (`transition_in`, `transition_out`, see `core/transition.rs`).
  `transition::track_at` decides what a track shows at a time; `transition::clip_span` is the range a clip
  is drawn in (wider than the clip at cuts), so decoders must be opened for that range, not just the clip.
  The compositor draws each side into its own texture and mixes them in `fs_transition`.
- Motion blur is not a shader effect: `engine/scene.rs` rebuilds the stack below it at several times
  (`SceneItem::Samples`). Video frames and text masks stay at the frame's own time; items identical in every
  sample become the `prefix`, drawn once. Preview and export convert scenes with `scene::to_draws`, whose
  callback can run several times per clip in one frame, so it must not re-decode or re-upload needlessly.
- The preview skips rendering when `DrawItem::hash_into` (plus video frame times) is unchanged. New draw
  settings must be hashed there.
- Widgets in the inspector only count as edits through `user_edit()`: sliders report "changed" when
  they merely round a displayed value, which during playback would create a keyframe every frame.
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
