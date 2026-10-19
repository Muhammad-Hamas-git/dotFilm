# Notes for AI coding assistants

Read README.md first for the folder layout.

## Architecture rules

- `core/` is plain data. It must never import `egui`, `eframe`, `cpal` or anything from `media/`, `engine/` or `ui/`.
- The UI changes the project only through `core::model::Project` methods or direct field edits.
  **Before any edit the user can undo, call `app.history.checkpoint(&app.project)` once.**
  For drags, checkpoint at drag start, not on every frame.
- Anything visual must be implemented twice: in the preview (`engine/preview.rs`, `ui/player.rs`)
  and in export (`engine/export.rs`). They must produce the same picture.
  Color work goes through `engine/color.rs`, which serves both.
- Never block the UI thread on ffmpeg. Long work runs on a thread and reports back
  through a channel (see `media/cache.rs`) or shared state (see `engine/export.rs`).
- Never lock or allocate in a way that can stall inside the audio callback (`engine/audio.rs`).
- New fields in saved structs need `#[serde(default)]` so older project files still open.
- Child processes are created only through `media::ffmpeg::command` (hides console windows on Windows).

## Conventions

- Tunable numbers are named constants at the top of the file, with a comment.
- Times are `f64` seconds. Positions inside the frame are `f32` project pixels.
- Track 0 is the top row and is drawn on top.

## Checking your work

- `cargo build` must have no errors and no warnings.
- `cargo test` must pass (the export test needs ffmpeg on PATH).
- Add a test in `core/model.rs` for any new editing operation.
