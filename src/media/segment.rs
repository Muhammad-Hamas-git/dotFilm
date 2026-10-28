//! The AI model that finds the subject of a picture, for background removal.
//!
//! The model is an ONNX file run by ONNX Runtime. On Windows it runs on the GPU
//! through DirectML (any DirectX 12 card: NVIDIA, AMD, Intel) and falls back to the
//! CPU when that fails. It isn't shipped with dotFilm: the first "Remove BG" downloads
//! it once into the cache folder and checks its SHA-256.
//!
//! Model: ISNet "general use" (Apache 2.0, by Xuebin Qin et al.), converted to ONNX by
//! the rembg project. It's a plain convolutional net, so it needs far less memory than
//! transformer models like BiRefNet (about 1.5 GB of RAM on the CPU vs. over 6 GB),
//! at somewhat softer detail on hair.

use super::ffmpeg;
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Everything needed to fetch and run one segmentation model.
pub struct ModelInfo {
    /// Shown in the UI and part of the matte cache key (a new model makes new mattes).
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Download size, for the progress bar.
    pub bytes: u64,
    /// Square input size the graph was exported with.
    pub size: u32,
    pub input: &'static str,
    pub output: &'static str,
    /// Input normalization: (rgb / 255 - mean) / std.
    pub mean: [f32; 3],
    pub std: [f32; 3],
    /// True if the output is raw logits (needs a sigmoid), false if already 0..1.
    pub logits: bool,
}

/// The model "Remove BG" uses.
pub const MODEL: ModelInfo = ModelInfo {
    name: "ISNet general",
    file: "isnet-general-use.onnx",
    url: "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-general-use.onnx",
    sha256: "60920e99c45464f2ba57bee2ad08c919a52bbf852739e96947fbb4358c0d964a",
    bytes: 178_648_008,
    size: 1024,
    input: "input_image",
    output: "output_image",
    mean: [0.5, 0.5, 0.5],
    std: [1.0, 1.0, 1.0],
    logits: false,
};

/// Set this to a local .onnx file to skip the download (tests, offline machines).
const MODEL_PATH_ENV: &str = "DOTFILM_MATTE_MODEL";
/// How often the download progress is checked.
const DOWNLOAD_POLL: Duration = Duration::from_millis(200);

pub fn models_dir() -> PathBuf {
    super::proxy::cache_root().join("models")
}

/// Where the model file lives (or will after the download).
pub fn model_path() -> PathBuf {
    match std::env::var_os(MODEL_PATH_ENV) {
        Some(p) => PathBuf::from(p),
        None => models_dir().join(MODEL.file),
    }
}

/// True if the model is on disk (no download needed).
pub fn model_ready() -> bool {
    model_path().is_file()
}

/// Downloads the model if it isn't there yet. `progress` gets 0..1.
/// Uses the system's curl (part of Windows 10 and later), so the download goes
/// through the system's certificates and proxy settings.
pub fn ensure_model(progress: &mut dyn FnMut(f32), cancel: &AtomicBool) -> Result<PathBuf, String> {
    let path = model_path();
    if path.is_file() {
        return Ok(path);
    }
    if std::env::var_os(MODEL_PATH_ENV).is_some() {
        return Err(format!("{MODEL_PATH_ENV} points to a missing file: {}", path.display()));
    }
    std::fs::create_dir_all(models_dir()).map_err(|e| e.to_string())?;
    let part = path.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let manual = format!("You can also download it yourself from {} and save it as {}", MODEL.url, path.display());
    let mut child = ffmpeg::command("curl")
        .args(["-L", "--fail", "--silent", "--show-error", "--retry", "2", "-o"])
        .arg(&part)
        .arg(MODEL.url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Can't start curl to download the model ({e}). {manual}"))?;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&part);
            return Err("Cancelled".into());
        }
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        let got = std::fs::metadata(&part).map_or(0, |m| m.len());
        progress((got as f64 / MODEL.bytes as f64).min(1.0) as f32);
        std::thread::sleep(DOWNLOAD_POLL);
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut e, &mut err);
        }
        let _ = std::fs::remove_file(&part);
        return Err(format!("Model download failed: {}. {manual}", err.trim()));
    }
    let digest = sha256_file(&part)?;
    if digest != MODEL.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("The downloaded model is damaged (checksum mismatch). Try again. {manual}"));
    }
    std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    progress(1.0);
    Ok(path)
}

fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The ONNX Runtime environment (only one is allowed per process).
fn environment() -> Result<&'static ort::environment::Environment, String> {
    static ENV: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| ort::init().with_name("dotFilm").with_telemetry(false).build().map_err(|e| format!("ONNX Runtime failed to start: {e}")))
        .as_ref()
        .map_err(|e| e.clone())
}

/// Set this to a number to force a DirectML device (0 = first adapter Windows lists),
/// e.g. when the automatic choice picks the wrong GPU.
#[cfg(windows)]
const DEVICE_ENV: &str = "DOTFILM_DML_DEVICE";

/// A loaded model, ready to run on frames.
pub struct Segmenter {
    session: Session,
    /// Where it runs, for the UI, e.g. "NVIDIA GeForce RTX 4060 (DirectML)" or "CPU".
    pub device: String,
    /// Square input size the model runs at.
    pub size: u32,
    input: Vec<f32>,
}

impl Segmenter {
    /// Loads the model at `size` x `size` (MODEL.size, or half of it for fast mode),
    /// on the dedicated GPU if there is one, else any GPU, else the CPU.
    pub fn load(path: &std::path::Path, size: u32) -> Result<Self, String> {
        let env = environment()?;
        let mut bytes = std::fs::read(path).map_err(|e| format!("Can't read the model: {e}"))?;
        if size != MODEL.size {
            patch_input_size(&mut bytes, size).map_err(|e| format!("Can't prepare the model for {size} px: {e}"))?;
        }
        let input = vec![0.0; (size * size * 3) as usize];
        #[cfg(windows)]
        {
            use ort::ep::directml::{DeviceFilter, PerformancePreference};
            // High performance = the dedicated card on laptops and PCs with two GPUs
            // (Windows' own GPU preference order), instead of the first adapter listed,
            // which is often the integrated one.
            let forced = std::env::var(DEVICE_ENV).ok().and_then(|v| v.trim().parse::<i32>().ok());
            let dml = match forced {
                Some(id) => ort::ep::DirectML::default().with_device_id(id),
                None => ort::ep::DirectML::default().with_device_filter(DeviceFilter::Gpu).with_performance_preference(PerformancePreference::HighPerformance),
            };
            // DirectML needs sequential execution and no memory pattern optimization.
            let gpu = Session::builder(env)
                .map_err(|e| e.to_string())
                .and_then(|b| b.with_parallel_execution(false).map_err(|e| e.to_string()))
                .and_then(|b| b.with_memory_pattern(false).map_err(|e| e.to_string()))
                .and_then(|b| b.with_execution_providers([dml.build().error_on_failure()]).map_err(|e| e.to_string()))
                .and_then(|mut b| b.commit_from_memory(&bytes).map_err(|e| e.to_string()));
            match gpu {
                Ok(session) => {
                    let name = match forced {
                        Some(id) => format!("GPU {id}"),
                        None => high_performance_gpu_name().unwrap_or_else(|| "GPU".into()),
                    };
                    return Ok(Self { session, device: format!("{name} (DirectML)"), size, input });
                }
                Err(e) => eprintln!("DirectML unavailable, using the CPU: {e}"),
            }
        }
        let session = Session::builder(env)
            .map_err(|e| e.to_string())?
            .commit_from_memory(&bytes)
            .map_err(|e| format!("Can't load the model: {e}"))?;
        Ok(Self { session, device: "CPU".into(), size, input })
    }

    /// Runs the model on one `size` x `size` RGB24 picture. Returns the subject
    /// coverage per pixel, 0..1, same size.
    pub fn run(&mut self, rgb: &[u8]) -> Result<Vec<f32>, String> {
        let n = (self.size * self.size) as usize;
        if rgb.len() < n * 3 {
            return Err("frame too small".into());
        }
        // Planar, normalized input (NCHW).
        let inv = MODEL.std.map(|s| 1.0 / (255.0 * s));
        let off = [0, 1, 2].map(|c| MODEL.mean[c] / MODEL.std[c]);
        for (i, px) in rgb[..n * 3].chunks_exact(3).enumerate() {
            for c in 0..3 {
                self.input[c * n + i] = px[c] as f32 * inv[c] - off[c];
            }
        }
        let s = self.size as usize;
        let tensor = Tensor::from_array(([1usize, 3, s, s], self.input.clone())).map_err(|e| e.to_string())?;
        let out = self.session.run(ort::inputs![MODEL.input => tensor]).map_err(|e| e.to_string())?;
        let (_, data) = out[MODEL.output].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        if data.len() < n {
            return Err("unexpected model output size".into());
        }
        Ok(data[..n].iter().map(|&v| if MODEL.logits { 1.0 / (1.0 + (-v).exp()) } else { v.clamp(0.0, 1.0) }).collect())
    }
}

/// Name of the GPU Windows considers "high performance" (what DirectML picks).
#[cfg(windows)]
fn high_performance_gpu_name() -> Option<String> {
    use eframe::wgpu;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let opts = wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    };
    pollster::block_on(instance.request_adapter(&opts)).ok().map(|a| a.get_info().name)
}

// ---------------------------------------------------------------- model resizing
//
// The ISNet ONNX export has its 1024 x 1024 input size baked in: the graph input is
// declared 1024 x 1024 and every upsampling step (Resize) gets its target size from a
// constant. To run it at another size, those numbers are rewritten in place in the
// protobuf bytes. Only same-length edits are made (16-byte int64 pairs and 2-byte
// varints), so nothing else in the file moves.

/// Protobuf field: number, wire type, and its payload range (varint: its bytes).
struct Field {
    num: u64,
    wire: u8,
    start: usize,
    end: usize,
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos).ok_or("truncated varint")?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("bad varint".into())
}

/// The fields of the message in `buf[range]`.
fn fields(buf: &[u8], range: std::ops::Range<usize>) -> Result<Vec<Field>, String> {
    let mut out = Vec::new();
    let mut pos = range.start;
    while pos < range.end {
        let key = read_varint(buf, &mut pos)?;
        let (num, wire) = (key >> 3, (key & 7) as u8);
        let start = pos;
        match wire {
            0 => {
                read_varint(buf, &mut pos)?;
            }
            1 => pos += 8,
            2 => {
                let len = read_varint(buf, &mut pos)? as usize;
                let s = pos;
                pos += len;
                out.push(Field { num, wire, start: s, end: pos });
                continue;
            }
            5 => pos += 4,
            _ => return Err(format!("unsupported wire type {wire}")),
        }
        out.push(Field { num, wire, start, end: pos });
    }
    if pos != range.end {
        return Err("message overruns its length".into());
    }
    Ok(out)
}

/// Payload ranges of the length-delimited fields numbered `num`.
fn sub(fs: &[Field], num: u64) -> Vec<std::ops::Range<usize>> {
    fs.iter().filter(|f| f.num == num && f.wire == 2).map(|f| f.start..f.end).collect()
}

fn text(buf: &[u8], r: &std::ops::Range<usize>) -> String {
    String::from_utf8_lossy(&buf[r.clone()]).into_owned()
}

/// Rewrites a 2-byte varint in place (values 128..16383 all take two bytes).
fn put_varint2(buf: &mut [u8], f: &Field, v: u64) -> Result<(), String> {
    if f.end - f.start != 2 || !(128..16384).contains(&v) {
        return Err(format!("size {v} can't be written in place"));
    }
    buf[f.start] = (v & 0x7f) as u8 | 0x80;
    buf[f.start + 1] = (v >> 7) as u8;
    Ok(())
}

/// Sets the spatial dims (index 2 and 3) of the graph input/output named `name`.
fn patch_value_info(buf: &mut [u8], graph: &[Field], field: u64, name: &str, size: u64) -> Result<(), String> {
    for vi in sub(graph, field) {
        let vf = fields(buf, vi)?;
        if sub(&vf, 1).first().map(|r| text(buf, r)).as_deref() != Some(name) {
            continue;
        }
        let ty = sub(&vf, 2).into_iter().next().ok_or("no type")?;
        let tensor = sub(&fields(buf, ty)?, 1).into_iter().next().ok_or("no tensor type")?;
        let shape = sub(&fields(buf, tensor)?, 2).into_iter().next().ok_or("no shape")?;
        let dims = sub(&fields(buf, shape)?, 1);
        if dims.len() != 4 {
            return Err(format!("{name} is not 4D"));
        }
        for d in &dims[2..] {
            let df = fields(buf, d.clone())?;
            let v = df.iter().find(|f| f.num == 1 && f.wire == 0).ok_or("dynamic dim")?;
            let (s, e) = (v.start, v.end);
            put_varint2(buf, &Field { num: 1, wire: 0, start: s, end: e }, size)?;
        }
        return Ok(());
    }
    Err(format!("{name} not found"))
}

/// Makes the model take `size` x `size` input instead of MODEL.size. Edits `model` in place.
pub fn patch_input_size(model: &mut [u8], size: u32) -> Result<(), String> {
    let (from, to) = (MODEL.size as i64, size as i64);
    let top = fields(model, 0..model.len())?;
    let graph_range = sub(&top, 7).into_iter().next().ok_or("no graph")?;
    let graph = fields(model, graph_range)?;

    // Nodes: inputs, outputs, type, and for 2-element int64 constants their raw bytes.
    struct Node {
        inputs: Vec<String>,
        op: String,
        pair: Option<std::ops::Range<usize>>,
    }
    let mut nodes = Vec::new();
    let mut producer = std::collections::HashMap::new();
    for n in sub(&graph, 1) {
        let nf = fields(model, n)?;
        let inputs: Vec<String> = sub(&nf, 1).iter().map(|r| text(model, r)).collect();
        let op = sub(&nf, 4).first().map(|r| text(model, r)).unwrap_or_default();
        let mut pair = None;
        if op == "Constant" {
            for a in sub(&nf, 5) {
                let af = fields(model, a)?;
                if let Some(t) = sub(&af, 5).into_iter().next() {
                    let tf = fields(model, t)?;
                    let raw = sub(&tf, 9);
                    let dtype_int64 = tf.iter().any(|f| f.num == 2 && f.wire == 0 && f.end - f.start == 1 && model[f.start] == 7);
                    if dtype_int64 && raw.len() == 1 && raw[0].len() == 16 {
                        pair = Some(raw[0].clone());
                    }
                }
            }
        }
        for o in sub(&nf, 2) {
            producer.insert(text(model, &o), nodes.len());
        }
        nodes.push(Node { inputs, op, pair });
    }

    // Resize target sizes: Resize(sizes = Concat(shape slice, Constant[h, w])).
    let mut targets = std::collections::BTreeSet::new();
    for n in nodes.iter().filter(|n| n.op == "Resize") {
        let cat = n.inputs.get(3).and_then(|s| producer.get(s)).map(|&i| &nodes[i]).ok_or("Resize without sizes")?;
        for i in &cat.inputs {
            if let Some(r) = producer.get(i).and_then(|&k| nodes[k].pair.clone()) {
                targets.insert((r.start, r.end));
            }
        }
    }
    if targets.is_empty() {
        return Err("no resize sizes found (unexpected model)".into());
    }
    for (s, _) in targets {
        for k in 0..2 {
            let at = s + k * 8;
            let v = i64::from_le_bytes(model[at..at + 8].try_into().unwrap());
            if (v * to) % from != 0 {
                return Err(format!("{size} px doesn't divide the model's sizes evenly"));
            }
            model[at..at + 8].copy_from_slice(&(v * to / from).to_le_bytes());
        }
    }
    let graph = fields(model, sub(&top, 7)[0].clone())?;
    patch_value_info(model, &graph, 11, MODEL.input, size as u64)?;
    patch_value_info(model, &graph, 12, MODEL.output, size as u64)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        let mut buf = vec![0u8; 2];
        let f = Field { num: 1, wire: 0, start: 0, end: 2 };
        put_varint2(&mut buf, &f, 512).unwrap();
        assert_eq!(read_varint(&buf, &mut 0).unwrap(), 512);
        assert!(put_varint2(&mut buf, &f, 64).is_err(), "would change the length");
    }

    /// Patches the real model to 512 and runs it. Needs DOTFILM_MATTE_MODEL and ORT_DYLIB_PATH.
    #[test]
    fn half_size_model_runs() {
        let Some(path) = std::env::var_os("DOTFILM_MATTE_MODEL") else {
            eprintln!("DOTFILM_MATTE_MODEL not set, skipping");
            return;
        };
        let mut seg = Segmenter::load(std::path::Path::new(&path), MODEL.size / 2).expect("load at 512");
        let px = vec![128u8; 512 * 512 * 3];
        assert_eq!(seg.run(&px).unwrap().len(), 512 * 512);
    }
}
