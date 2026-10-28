# dotFilm

A desktop video editor written in Rust, rendered on the GPU.

- **Assets panel**: import video, images and audio (Import button, or drag files onto the window).
  **Click** an asset to preview it in the player (with sound, its own playhead), **double-click** to add
  it at the playhead, or drag it onto the timeline.
  Heavy videos (4K, HEVC, ProRes...) automatically get a fast **proxy** for editing.
  **New** makes solid colors, gradients (2 or 3 colors, any angle), text and effects.
- **Text**: any installed font, bold/italic, alignment, size, line spacing, fill and outline color,
  outline width. Proper shaping for Arabic, Urdu and other complex scripts. All keyframeable.
- **Effect layers**, in groups:
  - Blur: Gaussian, lens (round bokeh), radial (zoom), spin, **motion blur**.
  - Retro: **VHS** (scanlines, line count, noise, line jitter, color bleed, CRT curve, tracking band,
    softness, each adjustable), **jitter** (random shake every few frames), **pixelate**, **dither** (few
    colors, ordered or noise pattern, mono or color), **halftone** (black or CMY dots), film grain.
  - Distort: **bulge / pinch** and **fisheye** with a center you drag in the player, **CRT bulge** (curved
    glass, rounded corners, darker edges), **liquify** (push pixels with a brush in the player).
  - Light: bloom, **light rays** (streaks from a light source you place), **star sparkles** on highlights.
  - Look: vignette, chromatic aberration, double exposure.
  An effect clip is its own layer on a track and changes everything on the tracks below it. Effects with a
  center show a yellow crosshair on the player to drag.
  Motion blur blurs anything that moves by keyframes (position, scale, rotation...): it draws the layers
  below at several moments within the shutter and averages them. Shutter angle and sample count are adjustable.
- **Masks** (up to 4 per clip): rectangle (rounded corners), ellipse, linear, mirror (a band) and freeform
  shapes you click out point by point. Feather, expansion, opacity, invert, and add / subtract / intersect.
  Everything keyframeable. Masks stick to the clip as it moves, scales and turns in 3D. Edit them right in
  the player: drag the center, corners, edges, the round rotation handle, the inside, or shape points.
  On an **effect layer** a mask limits the effect to that area (blur just a face, glow just the sky).
  **Color range** masks select by color instead of shape: click **Pick** and then a color in the player, then
  widen the hue range, minimum saturation, brightness range and softness. Blur only the blue sky, desaturate
  everything but the red coat.
- **Transitions**: crossfade, fade through black or white, wipe (any direction, soft edge), push, slide,
  zoom, spin, blur, glow and light leak,
  with adjustable length and easing. Set them on a clip's start or end (Adjustments panel, or right-click
  the clip). Between two touching clips it works like CapCut: the next clip slides back over the end of
  the previous one by the transition length and starts playing from its first frame when the transition
  starts. Everything after it on the track moves back too, so the timeline gets shorter. The audio
  crossfades over the same overlap. On a free edge the clip fades, wipes or pushes in from (or out to)
  what's below.
- **Per-clip effects**: any effect (except motion blur) can be added to one clip in its **Effects** section.
  It changes only that clip, with a Mix amount, on/off switch and order. Effect layers on the timeline
  still change every track below them. When the clip has masks or a removed background, its effects treat
  the visible part as the picture: a vignette darkens towards the mask's edges, radial blurs and distortions
  center on it, and it all follows the clip's position, scale and 3D rotation. Masks on an effect layer do
  the same for that effect.
- **Frame rate per clip**: tick **Own** next to Frame rate in Timing to play a clip at e.g. 12 fps for a
  choppy, stop-motion look. Keyframed movement steps at that rate too.
- **Levels, shadows / highlights, curves**: input and output black / white points and gamma; shadows and
  highlights recovery; curves for RGB together, R, G, B or brightness only (click to add a point, drag it,
  double-click to remove it). All keyframeable except the curve shapes.
- **LUTs**: load any 3D `.cube` LUT on a clip with an intensity slider, and **export** a clip's
  whole color grade (adjustments, white balance, wheels, HSL, levels, curves, LUT) as a `.cube` file to use
  on other clips or in other programs.
- **Player**: plays the timeline. Click a picture to select it, drag to move it, drag a corner to resize it.
  Hit-testing follows the real 3D outline of rotated layers. Snaps to the frame center.
  **Ctrl + wheel** zooms in at the cursor (up to 32x; the preview renders sharper while zoomed), the middle
  mouse button pans, the zoom button goes back to Fit. **Full screen** (F11) shows only the player.
- **Adjustments panel**: position, scale, opacity, **3D rotation (pitch, yaw, roll)**, brightness, contrast,
  saturation, hue, value, volume, **speed** (0.1x to 10x, with or without changing audio pitch).
  **Every value can be keyframed**, with an easing curve editor (linear, ease in/out, hold, or a
  custom bezier with overshoot). Exact start and length fields.
- **Color grading** on every picture clip: white balance (temperature, tint), three **color wheels**
  (shadows, midtones, highlights: tint plus luminance), and **HSL by color** (hue, saturation and
  luminance for red, orange, yellow, green, blue, indigo, violet). All keyframeable.
- **Remove background (AI)**: no green screen needed. Select a video clip, open **Remove background (AI)**
  in the Adjustments panel and click **Remove BG (high quality)** or **Remove BG (fast)**. The video is analyzed
  once in the background (progress bar, cancel button) on your dedicated GPU through DirectML (any DirectX 12
  card), or on the CPU if that fails. After that it plays in real time. **Fast** analyzes every other frame at
  half resolution, about 6 to 8 times faster, with softer edges. Both results can be cached at once; switch
  between them any time. **Erode**, **Dilate** and **Feather** clean up the edge live and are keyframeable;
  **Keep the background instead** inverts it. The clip's **masks** then isolate the subject further (only what's
  inside an Add mask stays). Turn on **Masks bring back the background** to have masks combine with the cut-out
  instead, so an Add mask shows the original picture inside it. The first use downloads the AI model (ISNet,
  about 180 MB) into the cache folder.
- **Chroma key**: remove a green (or any) screen. Click **Pick** and then the background in the player,
  or set the color by hand. Similarity, softness and spill removal. Runs on the GPU like everything else.
- **Copy and paste clips** (Ctrl+C / Ctrl+V or right-click): pastes at the playhead with all its
  settings and keyframes, on a track above if the spot is taken.
- **Timeline**: multiple tracks (Track 1 is the bottom layer), **blend mode per track** (18 modes: multiply,
  screen, overlay, soft light, difference, hue, color, luminosity...), drag/trim/split clips, snapping, zoom,
  hide/mute tracks, keyframe diamonds you can drag, click or right-click.
- **Compound clips**: select clips (Ctrl+click for more) and press **Compound** (or right-click > Make
  compound clip). They become one clip that moves, scales, fades, takes masks, effects and transitions as one
  picture. Double-click it (or right-click > Open compound) to edit inside, **Back** (or Esc with nothing
  selected) to return. Right-click > Release compound puts the clips back. Compounds can contain compounds.
- **Audio**: volume (keyframeable), fade in / out (fields, or drag the small circles at a selected clip's top
  corners on the timeline), pitch in semitones without changing the timing, speed with or without pitch.
  **Separate audio** (right-click a video, or the Audio section) moves a video's sound onto its own clip on the
  lowest free track (a new bottom track if needed), so it can be cut, moved and faded on its own. Waveforms
  show on clips with sound.
- **Markers**: press M (or the Marker button) to drop a marker at the playhead. Drag them on the ruler,
  click to jump, double-click to type an exact time (e.g. 4.35 s), right-click to name, color or delete.
  **Markers > Beat markers from BPM** lays an endless beat (or bar) grid from a start time on; it stays
  in place. Clips and the playhead snap to clip edges, markers and beats while Snap is on.
- **Keyframe table**: the **Keyframes** button swaps the tracks for the selected clip's animated values,
  one row each. Drag keys sideways to retime, and edit the curve of the chosen row in the graph below:
  drag points up and down for the value, and the white handles for bezier easing.
- **Project**: any resolution up to 8192 x 8192 and any frame rate from 1 to 240 fps.
- **Export**: MP4 rendered by the same GPU code as the preview, at the project's size and frame rate or
  any other (e.g. edit at 1080p 60 fps, export 480p 15 fps). Constant quality or target bitrate. Renders on
  the dedicated graphics card and uses its video encoder (NVIDIA, AMD, Intel) when ffmpeg has one; the
  dialog says which GPU and encoder are used.
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
on a PC without FFmpeg installed. Always ship **`DirectML.dll`** (also in `target\release`) next to the exe:
dotFilm doesn't start without it.

## Keyframes

1. Select a clip and move the playhead.
2. Click the diamond next to a value: it turns gold, there's now a keyframe here.
3. Move the playhead and change the value: a new keyframe is added automatically.
4. On a keyframe, the **Keyframe easing** section shapes the motion to the next keyframe.
   Drag the two white handles for a custom curve.
5. On the timeline, the selected clip's keyframes show as diamonds:
   drag to retime, click to jump there, right-click for easing presets or delete.
6. For more control, click **Keyframes** in the timeline toolbar: every animated value gets its own row,
   and the graph shows the selected row's curve with bezier handles.

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
| M | Add a marker at the playhead |
| F11 | Full screen player on / off |
| Esc | Leave full screen / back from an asset preview / stop picking a color / stop editing a mask / deselect / leave a compound clip |
| Ctrl+click (timeline) | Select more clips (for a compound clip or deleting several) |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste clip at the playhead |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+S, Ctrl+O, Ctrl+I, Ctrl+E | Save, open, import, export |
| Ctrl + wheel | Zoom timeline at the cursor |
| Shift + wheel | Scroll timeline |
| Ctrl + wheel on the player | Zoom the player at the cursor (middle mouse button pans) |

## How it's built

```
src/
  main.rs            window setup
  core/              the project as plain data (no UI, no video, no GPU)
    model.rs           assets, tracks, clips, blend modes, parameters, save/load
    anim.rs            keyframes and easing curves
    content.rs         colors, gradients, text and effect settings of generated layers
    grade.rs           white balance, color wheels, HSL per color, chroma key settings
    transition.rs      transitions on clip edges, the overlap at cuts, and where they play on the timeline
    tone.rs            levels, shadows / highlights, curves, and .cube LUT reading / writing
    marker.rs          timeline markers and the beat grid
    compound.rs        compound clips: grouping, releasing, their sound
    warp.rs            liquify grid and brush
    mask.rs            mask shapes, their math, and packing for the GPU
    matte.rs           AI background removal settings (quality, erode/dilate/feather, mask mode)
    geometry.rs        3D rotation and perspective of a layer
    history.rs         undo/redo
  media/             reading files
    ffmpeg.rs          finds and launches ffmpeg/ffprobe
    probe.rs           file info, thumbnails, images, audio decoding
    video.rs           decoding video frames (YUV) for preview and export
    proxy.rs           building and caching proxy files
    segment.rs         the AI background removal model: download, GPU/CPU inference
    matte.rs           analyzing videos into cached mattes (background jobs)
    cache.rs           background loading
  engine/            turning the project into picture and sound
    scene.rs           what to draw at a given time (shared by preview and export)
    gpu/shaders.wgsl   all per-pixel work: YUV to RGB, color, blend modes, YUV packing
    gpu/compositor.rs  runs the shaders, manages GPU textures
    color.rs           color adjustments as matrices
    lut.rs             loads LUT files for the GPU, bakes a clip's grade into a LUT
    preview.rs         the player picture
    text.rs            text layout and rasterizing (cosmic-text, system fonts)
    audio.rs           audio mixing and playback (also used for export)
    stretch.rs         time-stretching: speed changes without pitch changes (WSOLA)
    export.rs          renders every frame and streams it into the encoder
  ui/                the window: app.rs, assets, player (+ effect_tools: effect centers, liquify brush),
                     inspector (+ grading, tone, clip_effects, matte, masks, transitions, audio),
                     timeline (+ markers, keyframes), export_dialog
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
| Export defaults (quality, bitrate), resolution and frame rate presets | top of `ui/export_dialog.rs` |
| Frames rendered ahead of the encoder | `FRAMES_AHEAD` in `engine/export.rs` |
| Which GPU renders | automatic (dedicated card first, `pick_adapter` in `engine/gpu/mod.rs`); set `DOTFILM_GPU=nvidia` (part of the name) to force one |
| Mask bounds precision for clip effects | `BOUNDS_GRID`, `BOUNDS_THRESHOLD` in `core/mask.rs`, `MATTE_BOUNDS_THRESHOLD` in `engine/gpu/compositor.rs` |
| Hardware decoding on export, audio bitrate | top of `engine/export.rs` |
| Resolution / fps presets | `RESOLUTIONS` and `FRAME_RATES` in `ui/inspector.rs` |
| Undo steps | `MAX_UNDO` in `core/history.rs` |
| Grading ranges and defaults (temperature, wheels, HSL, key) | `info` in `core/grade.rs` |
| HSL color centers | `COLORS` in `core/grade.rs` and `HSL_CENTERS` in `gpu/shaders.wgsl` (keep in step) |
| Color wheel strength | `WHEEL_TINT`, `WHEEL_LUM` in `gpu/shaders.wgsl` |
| Chroma key reach (how far similarity 1.0 goes) | `KEY_REACH` in `gpu/shaders.wgsl` |
| Color wheel size, key color picker area | `WHEEL_SIZE` in `ui/grading.rs`, `PICK_RADIUS` in `engine/preview.rs` |
| Transition default length, shortest, what counts as touching clips | `DEFAULT_DURATION`, `MIN_DURATION`, `CUT_TOLERANCE` in `core/transition.rs` |
| Transition length limit in the UI, timeline marker color | `MAX_DURATION`, `OVERLAY` in `ui/transitions.rs` |
| Zoom transition strength | `ZOOM_AMOUNT` in `gpu/shaders.wgsl` |
| Blur quality (taps per pass) | `BLUR_TAPS`, `BLOOM_TAPS`, `LENS_MIN_TAPS`, `LENS_MAX_TAPS`, `RADIAL_TAPS` in `gpu/shaders.wgsl` |
| Most motion blur samples | `MAX_MOTION_SAMPLES` in `engine/scene.rs` (the slider range is in `core/content.rs`) |
| Masks per clip, shape points per clip | `MAX_MASKS`, `MAX_POINTS` in `core/mask.rs` (the shader has room for exactly this) |
| Mask value ranges and defaults | `info` and `Mask::new` in `core/mask.rs` |
| Mask handle size, grab distance, outline color | top of `ui/masks.rs` |
| AI matte size, flicker smoothing, analysis frame rate cap | `MATTE_MAX_SIDE`, `SMOOTHING`, `MOTION_THRESHOLD`, `MAX_FPS` in `media/matte.rs` |
| Fast mode: model size, frames skipped | `FAST_MODEL_SIZE` (512, or 768 for closer to High quality), `FAST_FRAME_STEP` in `media/matte.rs` |
| Which AI model removes backgrounds | `MODEL` in `media/segment.rs` (set `DOTFILM_MATTE_MODEL` to a local .onnx to skip the download) |
| Which GPU runs the AI | automatic (Windows' high performance GPU); set `DOTFILM_DML_DEVICE=1` (or 0, 2...) to force one |
| Erode / dilate / feather ranges | `info` in `core/matte.rs`; largest radius `MAX_RADIUS` there and `MATTE_MAX_RADIUS` in `gpu/shaders.wgsl` (keep in step) |
| Effect parameter ranges and defaults | `EffectKind::params` in `core/content.rs` |
| Default text, colors, gradient | `Content::text`, `Content::color`, `Content::gradient` in `core/content.rs` |
| Text sharpness limits | `MAX_RASTER_PX`, `MIN_RASTER_PX` in `engine/text.rs` |
| Speed range | `SPEED_RANGE` in `core/model.rs` |
| Time-stretch quality (chunk size, alignment search) | `WINDOW`, `SEEK` in `engine/stretch.rs` |
| Effect setting ranges and defaults (VHS, dither, bulge...) | `EffectKind::params` in `core/content.rs` |
| Light rays taps, sparkle taps | the `19u` / `20u` cases of `fs_effect` in `gpu/shaders.wgsl` (`n`, `taps`) |
| Glow / blur transition reach | `30.0` / `45.0` in the `6u, 7u` case of `fs_transition` in `gpu/shaders.wgsl` |
| Liquify grid detail | `GRID` in `core/warp.rs` (more = finer, bigger project files) |
| Liquify brush size range, default strength, restore speed | `BRUSH_SIZE`, `BRUSH_STRENGTH`, `RESTORE_RATE` in `ui/effect_tools.rs` |
| Effect center handle size, grab distance, color | `CENTER_HANDLE`, `CENTER_GRAB`, `HANDLE_COLOR` in `ui/effect_tools.rs` |
| Pitch range, longest fade | `PITCH_RANGE`, `MAX_FADE` in `core/model.rs` |
| Waveform detail and color, fade shading | `PEAKS_PER_SECOND`, `WAVE_STEP_PX`, `WAVE_COLOR`, `FADE_SHADE` in `ui/audio.rs` |
| Fade handle grab distance | `FADE_GRAB_PX` in `ui/timeline.rs` |
| Levels / shadows / highlights ranges and defaults | `info` in `core/tone.rs` |
| Shadows / highlights strength | `SH_STRENGTH` in `core/tone.rs` |
| Curve editor size, grab distance, most points | `CURVE_EDITOR_SIZE`, `GRAB_PX`, `MAX_POINTS` in `ui/tone.rs` |
| Biggest LUT accepted, size of exported LUTs | `MAX_LUT_SIZE` in `core/tone.rs`, `EXPORT_SIZE` in `engine/lut.rs` (33 is standard, 65 is finer) |
| Color range mask: how wide hue range 1.0 is | `HUE_SPAN` in `core/mask.rs` and `MASK_HUE_SPAN` in `gpu/shaders.wgsl` (keep in step) |
| Player zoom limits and Ctrl+wheel speed | `ZOOM_RANGE`, `ZOOM_STEP` in `ui/player.rs` |
| Marker colors | `COLORS` in `core/marker.rs` |
| Marker grab distance, beat tick spacing, marker time precision | `GRAB_PX`, `MIN_BEAT_PX`, `TIME_DECIMALS` in `ui/markers.rs` |
| Keyframe table row height, table vs graph split, key size, snapping | `ROW_H`, `ROWS_SHARE`, `KEY_R`, `SNAP_PX` in `ui/keyframes.rs` |

## Known limits

- Audio of each file is decoded fully into memory (about 11 MB per minute of audio).
- The first proxy of a long 4K file takes a while to build; you can edit meanwhile (from the original).
- Motion blur blurs keyframed movement only. Movement inside the footage itself (a car driving past in
  the video) is not detected.
- Curve shapes aren't keyframeable (their strength can be faded with keyframed Levels or a second clip).
- Exported LUTs capture color only: per-clip effects, masks and chroma key are not part of a LUT.
- A clip's own frame rate holds frames; it doesn't blend or interpolate them.
- 1D `.cube` LUTs are not supported yet (3D ones are, which is nearly all of them).
- Pitch shift is per clip (not keyframeable). With speed changing the pitch and keyframed speed, the pitch
  shift adds on top of the original pitch, not the sped-up one.
- Liquify is not keyframeable (its Amount is), and its grid follows the clip's visible area.
- Ctrl+click picks clips one by one; there's no box selection yet.
- Odd frame sizes get one black pixel row/column on MP4 export (H.264 requires even sizes).
- AI background removal works on video clips (not still images yet). It finds the most prominent subject;
  with several people it may pick the wrong one or all of them, so use Intersect or Subtract masks.
  Low-contrast edges (white feathers on snow) can come out partly see-through; an Add mask fixes them.
  The matte is stored at up to 1024 px (Fast: 512 px), so very fine hair on 4K footage is softer than the picture.
