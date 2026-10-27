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
    std::io::copy(&mut f, &mut h).map_err(|e| e.to_string())?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The ONNX Runtime environment (only one is allowed per process).
fn environment() -> Result<&'static ort::environment::Environment, String> {
    static ENV: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| ort::init().with_name("dotFilm").with_telemetry(false).build().map_err(|e| format!("ONNX Runtime failed to start: {e}")))
        .as_ref()
        .map_err(|e| e.clone())
}

/// A loaded model, ready to run on frames.
pub struct Segmenter {
    session: Session,
    /// "GPU (DirectML)" or "CPU", for the UI.
    pub device: &'static str,
    input: Vec<f32>,
}

impl Segmenter {
    /// Loads the model on the GPU if possible, else on the CPU.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let env = environment()?;
        let size = (MODEL.size * MODEL.size * 3) as usize;
        #[cfg(windows)]
        {
            // DirectML needs sequential execution and no memory pattern optimization.
            let gpu = Session::builder(env)
                .map_err(|e| e.to_string())
                .and_then(|b| b.with_parallel_execution(false).map_err(|e| e.to_string()))
                .and_then(|b| b.with_memory_pattern(false).map_err(|e| e.to_string()))
                .and_then(|b| b.with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()]).map_err(|e| e.to_string()))
                .and_then(|mut b| b.commit_from_file(path).map_err(|e| e.to_string()));
            match gpu {
                Ok(session) => return Ok(Self { session, device: "GPU (DirectML)", input: vec![0.0; size] }),
                Err(e) => eprintln!("DirectML unavailable, using the CPU: {e}"),
            }
        }
        let session = Session::builder(env)
            .map_err(|e| e.to_string())?
            .commit_from_file(path)
            .map_err(|e| format!("Can't load the model: {e}"))?;
        Ok(Self { session, device: "CPU", input: vec![0.0; size] })
    }

    /// Runs the model on one `MODEL.size` square RGB24 picture. Returns the subject
    /// coverage per pixel, 0..1, same size.
    pub fn run(&mut self, rgb: &[u8]) -> Result<Vec<f32>, String> {
        let n = (MODEL.size * MODEL.size) as usize;
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
        let s = MODEL.size as usize;
        let tensor = Tensor::from_array(([1usize, 3, s, s], self.input.clone())).map_err(|e| e.to_string())?;
        let out = self.session.run(ort::inputs![MODEL.input => tensor]).map_err(|e| e.to_string())?;
        let (_, data) = out[MODEL.output].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        if data.len() < n {
            return Err("unexpected model output size".into());
        }
        Ok(data[..n].iter().map(|&v| if MODEL.logits { 1.0 / (1.0 + (-v).exp()) } else { v.clamp(0.0, 1.0) }).collect())
    }
}
