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
  parts. Shader order: source, chroma key, white balance, basic color stages, wheels, HSL, tone, LUT.
  Layer uniforms are padded to `UNIFORM_STRIDE` (2304); grow it if `LayerUniform` grows past that.
- Levels, shadows / highlights and curves live in `clip.tone` (`core/tone.rs`, `Param::Tone(i)`).
  `Tone::frame` bakes them on the CPU into a 64-entry table in the uniform; the shader only looks it up
  (`apply_tone`). LUTs are 3D textures in bind group 3, cached by path hash (`engine/lut.rs`, compositor
  `upload_lut` / `retain_luts`); layers without a LUT bind a blank one. LUT export renders a lattice image
  through the real compositor (`Compositor::sibling`), so it always matches the shader.
- Per-clip effects (`clip.effects`, `Param::ClipEffect(effect, i)`) draw the layer alone into the `iso`
  textures, run the effect passes there, then composite the result (`encode_isolated`). Effect shaders
  work on unpremultiplied color for color effects and premultiplied RGBA for spatial ones, so they're
  correct on transparent layers.
- A clip's own frame rate (`clip.frame_rate`) goes through `Clip::chop`; use it wherever a clip's time is
  sampled for drawing (scene, `clip_quad`, player hit-testing).
- Transitions live on clip edges (`transition_in`, `transition_out`, see `core/transition.rs`). At a cut
  they work like CapCut: the incoming clip **overlaps** the previous one by the transition length
  (B.start = A.end - d), and the window is that overlap. Change lengths only through `transition::set_start`
  / `set_end`, which ripple later clips; `Project::is_free` and `next_start` allow exactly that overlap.
  An end transition at a cut belongs to the next clip's start. `transition::track_at` decides what a track
  shows at a time, `audio_fades` gives the matching audio crossfade, and `sync_durations` fixes lengths after
  timeline drags. Project version 4 converted the old centered cuts (`upgrade_centered_cuts`).
  The compositor draws each side into its own texture and mixes them in `fs_transition`.
- Motion blur is not a shader effect: `engine/scene.rs` rebuilds the stack below it at several times
  (`SceneItem::Samples`). Video frames and text masks stay at the frame's own time; items identical in every
  sample become the `prefix`, drawn once. Preview and export convert scenes with `scene::to_draws`, whose
  callback can run several times per clip in one frame, so it must not re-decode or re-upload needlessly.
- The preview skips rendering when `DrawItem::hash_into` (plus video frame times) is unchanged. New draw
  settings must be hashed there.
- Masks live in `clip.masks` (`core/mask.rs`), addressed as `Param::Mask(mask, i)`. They're in the layer's
  own pixels (effect layers: project pixels), so the shader evaluates them on the layer's UV. `mask::distance`
  is the CPU twin of the shader's `mask_distance`; a GPU test compares them, keep them in step. The player
  edits masks through `Quad::homography`, the same projective map the GPU uses. `MaskKind::ColorRange`
  isn't a shape: it reuses the value slots (see `mask::idx` aliases) and keys on the clip's own source color
  (on effect layers, the backdrop); `mask::coverage_full` is its CPU twin.
- Markers (`project.markers`) and the beat grid (`project.beat_grid`, an endless `BeatGrid`, never stored
  as single markers) live in `core/marker.rs` and are part of undo snapshots. Timeline snapping includes them.
- Effects get an `EffectFrame` (core/geometry.rs): the area they treat as the picture. For a clip's own
  effects the compositor builds it from the clip's quad, its mask bounds (`LayerDraw::region`, from
  `mask::bounds`) and the AI matte's bounds (found on upload). Shaders reach it through `to_frame` /
  `from_frame` / `frame_px`; any effect with a center or edges must use those, not the raw frame uv.
- Export renders at `ExportSettings` size and fps (the compositor's canvas stays the project size).
- The timeline's keyframe table (`ui/keyframes.rs`) lists every animated `Param` of the selected clip from
  `Clip::params()`; anything that edits keys on the timeline must use `params()`, not `Param::ALL`.
- The asset preview (`app.source`) is a separate one-clip `Project` rendered by the same preview renderer.
  Transport functions (`seek`, `toggle_play`, ...) act on whatever `view_project` / `view_time` return.
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
