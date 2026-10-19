//! Reading file information (ffprobe), thumbnails, still images and audio.
//! All functions here are blocking; call them from a background thread.

use super::ffmpeg;
use crate::core::model::{AssetKind, ColorMatrix, VideoColor};
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;

pub struct ProbeInfo {
    pub kind: AssetKind,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    /// Video codec name, e.g. "h264". Empty for images and audio.
    pub codec: String,
    pub color: VideoColor,
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "webp", "gif"];

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn probe(path: &Path) -> Result<ProbeInfo, String> {
    if is_image(path) {
        let (w, h) = image::image_dimensions(path).map_err(|e| format!("Can't read image: {e}"))?;
        return Ok(ProbeInfo { kind: AssetKind::Image, duration: 0.0, width: w, height: h, has_audio: false, codec: String::new(), color: VideoColor::default() });
    }

    let out = ffmpeg::command("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Can't run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("Not a media file ({})", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let json: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let streams = json["streams"].as_array().cloned().unwrap_or_default();

    // Album art in mp3 files shows up as a video stream; skip it.
    let video = streams.iter().find(|s| {
        s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64().unwrap_or(0) == 0
    });
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");

    let duration = json["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse::<f64>().ok())
        .or_else(|| video.and_then(|v| v["duration"].as_str()?.parse().ok()))
        .unwrap_or(0.0);

    match video {
        Some(v) => {
            let mut w = v["width"].as_u64().unwrap_or(0) as u32;
            let mut h = v["height"].as_u64().unwrap_or(0) as u32;
            // Phone videos are often stored sideways with a rotation flag.
            // ffmpeg auto-rotates when decoding, so swap the reported size to match.
            let rotation = v["side_data_list"]
                .as_array()
                .and_then(|l| l.iter().find_map(|d| d["rotation"].as_f64()))
                .or_else(|| v["tags"]["rotate"].as_str().and_then(|r| r.parse().ok()))
                .unwrap_or(0.0);
            if (rotation.abs() as i64) % 180 == 90 {
                std::mem::swap(&mut w, &mut h);
            }
            if duration <= 0.0 {
                return Err("Video has no duration".into());
            }
            let codec = v["codec_name"].as_str().unwrap_or_default().to_string();
            let color = read_color(v);
            Ok(ProbeInfo { kind: AssetKind::Video, duration, width: w, height: h, has_audio, codec, color })
        }
        None if has_audio => Ok(ProbeInfo { kind: AssetKind::Audio, duration, width: 0, height: 0, has_audio, codec: String::new(), color: VideoColor::default() }),
        None => Err("No video or audio found".into()),
    }
}

/// Color standard from ffprobe's stream info.
fn read_color(v: &Value) -> VideoColor {
    let space = v["color_space"].as_str().unwrap_or("");
    let matrix = match space {
        "bt709" => ColorMatrix::Bt709,
        "smpte170m" | "bt470bg" | "fcc" | "smpte240m" => ColorMatrix::Bt601,
        "bt2020nc" | "bt2020c" => ColorMatrix::Bt2020,
        _ => ColorMatrix::Auto,
    };
    let pix = v["pix_fmt"].as_str().unwrap_or("");
    let full_range = v["color_range"].as_str() == Some("pc") || pix.starts_with("yuvj");
    VideoColor { matrix, full_range }
}

/// Decodes one frame at `t` seconds, scaled to exactly w x h, as RGBA bytes.
pub fn grab_frame(path: &Path, t: f64, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut child = ffmpeg::command("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{t:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-an", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let _ = child.wait();
    if buf.len() < (w * h * 4) as usize {
        return Err("No frame".into());
    }
    buf.truncate((w * h * 4) as usize);
    Ok(buf)
}

/// Loads an image file scaled to fit within max_w x max_h. Returns (w, h, rgba).
pub fn load_image(path: &Path, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let img = if img.width() > max_w || img.height() > max_h {
        img.resize(max_w, max_h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Sample rate of all decoded audio. Stereo, interleaved i16.
pub const AUDIO_RATE: u32 = 48_000;

/// Decodes the whole audio track to memory (about 11 MB per minute).
pub fn decode_audio(path: &Path) -> Result<Vec<i16>, String> {
    let mut child = ffmpeg::command("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "2", "-ar", &AUDIO_RATE.to_string(), "-f", "s16le", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let _ = child.wait();
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}
