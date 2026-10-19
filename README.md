# dotFilm

A desktop video editor written in Rust, rendered on the GPU.

- **Assets panel**: import video, images and audio (Import button, or drag files onto the window).
  Heavy videos (4K, HEVC, ProRes...) automatically get a fast **proxy** for editing.
  **New** makes solid colors, gradients (2 or 3 colors, any angle), text and effects.
- **Text**: any installed font, bold/italic, alignment, size, line spacing, fill and outline color,
  outline width. Proper shaping for Arabic, Urdu and other complex scripts. All keyframeable.
- **Effect layers**: Gaussian blur, vignette, film grain (new pattern every frame), chromatic aberration.
  An effect clip is its own layer on a track and changes everything on the tracks below it.
- **Player**: plays the timeline. Click a picture to select it, drag to move it, drag a corner to resize it.
  Hit-testing follows the real 3D outline of rotated layers. Snaps to the frame center.
- **Adjustments panel**: position, scale, opacity, **3D rotation (pitch, yaw, roll)**, brightness, contrast,
  saturation, hue, value, volume, **speed** (0.1x to 10x, with or without changing audio pitch).
  **Every value can be keyframed**, with an easing curve editor (linear, ease in/out, hold, or a
  custom bezier with overshoot). Exact start and length fields.
- **Copy and paste clips** (Ctrl+C / Ctrl+V or right-click): pastes at the playhead with all its
  settings and keyframes, on a track above if the spot is taken.
- **Timeline**: multiple tracks (Track 1 is the bottom layer), **blend mode per track** (18 modes: multiply,
  screen, overlay, soft light, difference, hue, color, luminosity...), drag/trim/split clips, snapping, zoom,
  hide/mute tracks, keyframe diamonds you can drag, click or right-click.
- **Project**: any resolution up to 8192 x 8192 and any frame rate from 1 to 240 fps.
- **Export**: MP4 rendered by the same GPU code as the preview. Constant quality or target bitrate.
  Uses your GPU's video encoder (NVIDIA, AMD, Intel) when available.
- Undo/redo, save/open projects (`.dotfilm` files; old `.cutroom` files still open).

## Setup on Windows (one time)

1. **Rust**: download and run `rustup-init.exe` from https://rustup.rs. When it asks, let it install the
   **Visual Studio C++ Build Tools** (Rust needs their linker). Accept the defaults.
2. **FFmpeg**: open a terminal (PowerShell) and run:
   ```
   winget install Gyan.FFmpeg
   ```
   Close and reopen the terminal afterwards so `ffmpeg` is found.

Or skip building: every push to GitHub builds `dotfilm.exe` with FFmpeg included.
Open the repo's **Actions** tab, pick the latest "Windows build" run, and download `dotfilm-windows`.

## Run it

```
cargo run --release
```

The first build takes several minutes. Use `--release` for testing playback.

```
cargo test
```

runs the tests, including GPU rendering checks and real exports through FFmpeg.

## Make a shareable app

```
cargo build --release
```

The program is `target\release\dotfilm.exe`. Put `ffmpeg.exe` and `ffprobe.exe` next to it to run it
on a PC without FFmpeg installed.

## Keyframes

1. Select a clip and move the playhead.
2. Click the diamond next to a value: it turns gold, there's now a keyframe here.
3. Move the playhead and change the value: a new keyframe is added automatically.
4. On a keyframe, the **Keyframe easing** section shapes the motion to the next keyframe.
   Drag the two white handles for a custom curve.
5. On the timeline, the selected clip's keyframes show as diamonds:
   drag to retime, click to jump there, right-click for easing presets or delete.

Keyframes are stored relative to the clip's start, so they move with it; trimming and splitting keep them
on the same moment of the footage. Right-click a clip, or use **Clear** in the Adjustments panel, to remove all its keyframes.

## Shortcuts

| Key | Action |
|---|---|
| Space | Play / pause |
| Left / Right | Previous / next frame (Shift: one second) |
| Home / End | Start / last frame |
| S | Split at playhead (selected clip, or all clips under the playhead) |
| Delete | Delete selected clip |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste clip at the playhead |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+S, Ctrl+O, Ctrl+I, Ctrl+E | Save, open, import, export |
| Ctrl + wheel | Zoom timeline at the cursor |
| Shift + wheel | Scroll timeline |

## How it's built

```
src/
  main.rs            window setup
  core/              the project as plain data (no UI, no video, no GPU)
    model.rs           assets, tracks, clips, blend modes, parameters, save/load
    anim.rs            keyframes and easing curves
    content.rs         colors, gradients, text and effect settings of generated layers
    geometry.rs        3D rotation and perspective of a layer
    history.rs         undo/redo
  media/             reading files
    ffmpeg.rs          finds and launches ffmpeg/ffprobe
    probe.rs           file info, thumbnails, images, audio decoding
    video.rs           decoding video frames (YUV) for preview and export
    proxy.rs           building and caching proxy files
    cache.rs           background loading
  engine/            turning the project into picture and sound
    scene.rs           what to draw at a given time (shared by preview and export)
    gpu/shaders.wgsl   all per-pixel work: YUV to RGB, color, blend modes, YUV packing
    gpu/compositor.rs  runs the shaders, manages GPU textures
    color.rs           color adjustments as matrices
    preview.rs         the player picture
    text.rs            text layout and rasterizing (cosmic-text, system fonts)
    audio.rs           audio mixing and playback (also used for export)
    stretch.rs         time-stretching: speed changes without pitch changes (WSOLA)
    export.rs          renders every frame and streams it into the encoder
  ui/                the window: app.rs, assets, player, inspector, timeline, export_dialog
```

The key rule: **the UI only edits the project data; preview and export both render it through
`engine/scene.rs` and the same GPU compositor.** That's why what you see is exactly what you export.

### Speed tricks

- Video decodes as YUV (1.5 bytes per pixel instead of 4), and the GPU converts it to RGB.
- The preview decodes each video only at the size it appears on screen.
- Proxies: heavy files get a small H.264 copy with frequent keyframes, so scrubbing is instant.
  They're cached in `%LOCALAPPDATA%\dotFilm\proxies` and reused next time.
- Clips start decoding one second before they appear.
- When nothing on screen changed, the preview does no GPU work at all.
- Export converts frames to YUV on the GPU before handing them to the encoder, and uses
  hardware video encoders and decoders when available.

### Adding a feature (the usual steps)

Example: a new animatable parameter "blur radius".
1. `core/model.rs`: add an `Anim` field with `#[serde(default)]`, a `Param` variant, and wire it into
   `param`, `param_mut` and `values_at`. It's keyframeable from then on.
2. `ui/inspector.rs`: add the `Param` to a `param_grid` call and give it a range in `widget_for`.
3. `engine/scene.rs` + `engine/gpu/`: pass the value to the shader and use it there.
   Preview and export both pick it up automatically.

### Values you can tune

| What | Where |
|---|---|
| Preview resolution options | `QUALITIES` in `engine/preview.rs` (also a dropdown in the player) |
| Proxy size, keyframe interval, quality | `PROXY_MAX`, `PROXY_GOP`, `PROXY_CRF` in `media/proxy.rs` |
| Which files get proxies | `needs_proxy` and `LIGHT_CODECS` in `media/proxy.rs` |
| Frames decoded ahead / scrub responsiveness | `QUEUE_FRAMES`, `SCRUB_RESTART_INTERVAL` in `media/video.rs` |
| 3D perspective strength | `CAMERA_DISTANCE` in `core/geometry.rs` |
| Timeline track height, snap distance, zoom, colors | top of `ui/timeline.rs` |
| Easing editor size and range | `CURVE_SIZE`, `CURVE_Y` in `ui/inspector.rs` |
| Player handle size, center snap | top of `ui/player.rs` |
| Export defaults (quality, bitrate) | top of `ui/export_dialog.rs` |
| Hardware decoding on export, audio bitrate | top of `engine/export.rs` |
| Resolution / fps presets | `RESOLUTIONS` and `FRAME_RATES` in `ui/inspector.rs` |
| Undo steps | `MAX_UNDO` in `core/history.rs` |
| Effect parameter ranges and defaults | `EffectKind::params` in `core/content.rs` |
| Default text, colors, gradient | `Content::text`, `Content::color`, `Content::gradient` in `core/content.rs` |
| Text sharpness limits | `MAX_RASTER_PX`, `MIN_RASTER_PX` in `engine/text.rs` |
| Speed range | `SPEED_RANGE` in `core/model.rs` |
| Time-stretch quality (chunk size, alignment search) | `WINDOW`, `SEEK` in `engine/stretch.rs` |

## Known limits

- Audio of each file is decoded fully into memory (about 11 MB per minute of audio).
- The first proxy of a long 4K file takes a while to build; you can edit meanwhile (from the original).
- Odd frame sizes get one black pixel row/column on MP4 export (H.264 requires even sizes).
