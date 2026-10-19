# Cutroom

A small desktop video editor written in Rust.

- **Assets panel**: import video, images and audio (Import button, or drag files onto the window).
- **Player**: plays the timeline. Click a picture to select it, drag to move it, drag a corner to resize. Snaps to the frame center.
- **Adjustments panel**: position, scale, brightness, contrast, saturation, hue, value, opacity, volume. With nothing selected: project resolution and frame rate.
- **Timeline**: multiple tracks, drag clips (also between tracks), trim by dragging clip edges, split, delete, snapping, zoom, hide/mute tracks.
- Undo/redo, save/open projects (`.cutroom` files), export to MP4.

## Setup on Windows (one time)

1. **Rust**: download and run `rustup-init.exe` from https://rustup.rs. When it asks, let it install the
   **Visual Studio C++ Build Tools** (Rust needs their linker). Accept the defaults.
2. **FFmpeg**: open a terminal (PowerShell) and run:
   ```
   winget install Gyan.FFmpeg
   ```
   Close and reopen the terminal afterwards so `ffmpeg` is found.
3. Check both work:
   ```
   cargo --version
   ffmpeg -version
   ```

## Run it

In this folder:

```
cargo run --release
```

The first build downloads and compiles all libraries and takes several minutes. After that it's fast.
Use `--release` for testing playback: debug builds are noticeably slower.

Run the tests (includes a real export through FFmpeg):

```
cargo test
```

## Make a shareable app

```
cargo build --release
```

The program is `target\release\cutroom.exe`. To run it on a PC without FFmpeg installed,
put `ffmpeg.exe` and `ffprobe.exe` in the same folder as `cutroom.exe` (the app checks there first).

## Shortcuts

| Key | Action |
|---|---|
| Space | Play / pause |
| Left / Right | Previous / next frame (Shift: one second) |
| Home / End | Start / end |
| S | Split at playhead (selected clip, or all clips under the playhead) |
| Delete | Delete selected clip |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+S, Ctrl+O, Ctrl+I, Ctrl+E | Save, open, import, export |
| Ctrl + wheel | Zoom timeline at the cursor |
| Shift + wheel | Scroll timeline |

## How it's built

```
src/
  main.rs            window setup
  core/              the project as plain data (no UI, no video)
    model.rs           assets, tracks, clips, split/add/remove, save/load
    history.rs         undo/redo (snapshots)
  media/             reading files
    ffmpeg.rs          finds and launches ffmpeg/ffprobe
    probe.rs           file info, thumbnails, images, audio decoding
    video.rs           streams decoded frames for playback
    cache.rs           background loading (thumbnails, images, audio)
  engine/            turning the project into picture and sound
    color.rs           the color adjustments (shared by preview AND export)
    preview.rs         builds the player picture for the playhead time
    audio.rs           mixes and plays audio, master clock during playback
    export.rs          builds the ffmpeg export command, runs it with progress
  ui/                the window
    app.rs             main state, menus, shortcuts
    assets.rs  player.rs  inspector.rs  timeline.rs  export_dialog.rs
```

The key rule: **the UI only edits the project data, and the engine only reads it.**
Preview and export are two separate renderers of the same data.

### Adding a feature (the usual 3 steps)

Example: a "rotation" control.
1. `core/model.rs`: add `rotation: f32` to `Transform`.
2. `ui/inspector.rs`: add a slider for it.
3. `engine/preview.rs` + `ui/player.rs` (draw it rotated) and `engine/export.rs` (add ffmpeg's `rotate` filter).

Old project files need the new field to have a default: add `#[serde(default)]` above it.

### Values you can tune

| What | Where |
|---|---|
| Preview sharpness vs. CPU use | `PREVIEW_MAX_SIDE` in `engine/preview.rs` |
| Frames decoded ahead (smoothness vs. memory) | `QUEUE_FRAMES` in `media/video.rs` |
| Scrub responsiveness | `SCRUB_RESTART_INTERVAL` in `media/video.rs` |
| Timeline track height, snap distance, zoom limits, clip colors | top of `ui/timeline.rs` |
| Player handle size, center snap | top of `ui/player.rs` |
| Asset tile size | `TILE` in `ui/assets.rs` |
| Default image clip length | `DEFAULT_IMAGE_DURATION` in `core/model.rs` |
| Undo steps | `MAX_UNDO` in `core/history.rs` |
| Export quality presets | `QUALITIES` and `SPEEDS` in `ui/export_dialog.rs` |
| Resolution / fps presets | `RESOLUTIONS` and `FRAME_RATES` in `ui/inspector.rs` |

## Known limits of this first version

- Audio of each file is decoded fully into memory (about 11 MB per minute of audio).
- Each video layer runs its own ffmpeg process during playback. Many stacked 4K layers will stutter;
  proxies (low-res copies for editing) are the standard fix.
- No waveforms on audio clips, no text, transitions or keyframes yet.
- Very long timelines (hundreds of clips) can hit the Windows command-line length limit on export.
