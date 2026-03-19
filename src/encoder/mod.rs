mod gifski_encoder;
#[cfg(feature = "ffmpeg")]
mod ffmpeg;

pub use gifski_encoder::GifskiEncoder;
#[cfg(feature = "ffmpeg")]
pub use ffmpeg::FfmpegEncoder;

use anyhow::Result;
use rgb::RGBA8;
use std::path::Path;

pub struct FrameData {
    pub index: usize,
    pub rgba_pixels: Vec<RGBA8>,
    pub width: usize,
    pub height: usize,
    pub time: f64,
}

pub trait Encoder {
    fn add_frame(&mut self, frame: FrameData) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

#[derive(Clone, Debug, clap::ArgEnum)]
pub enum EncoderBackend {
    Gifski,
    #[cfg(feature = "ffmpeg")]
    Ffmpeg,
}

/// Infer backend from output file extension. Returns error for non-GIF when ffmpeg not available.
pub fn infer_backend(path: &Path, explicit: Option<EncoderBackend>) -> Result<EncoderBackend> {
    if let Some(backend) = explicit {
        return Ok(backend);
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("gif") | None => Ok(EncoderBackend::Gifski),
        #[cfg(feature = "ffmpeg")]
        Some("mp4" | "webm" | "webp" | "apng" | "png") => Ok(EncoderBackend::Ffmpeg),
        #[cfg(feature = "ffmpeg")]
        Some(ext) => anyhow::bail!(
            "output format '.{}' is not supported by the ffmpeg encoder",
            ext
        ),
        #[cfg(not(feature = "ffmpeg"))]
        Some(ext) => anyhow::bail!(
            "output format '.{}' requires the 'ffmpeg' feature (build with --features ffmpeg)",
            ext
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn infer_gif_extension() {
        let backend = infer_backend(Path::new("out.gif"), None).unwrap();
        assert!(matches!(backend, EncoderBackend::Gifski));
    }

    #[test]
    fn infer_no_extension() {
        let backend = infer_backend(Path::new("output"), None).unwrap();
        assert!(matches!(backend, EncoderBackend::Gifski));
    }

    #[test]
    fn explicit_override() {
        let backend =
            infer_backend(Path::new("out.mp4"), Some(EncoderBackend::Gifski)).unwrap();
        assert!(matches!(backend, EncoderBackend::Gifski));
    }

    #[test]
    #[cfg(not(feature = "ffmpeg"))]
    fn non_gif_without_ffmpeg_feature() {
        let result = infer_backend(Path::new("out.mp4"), None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ffmpeg"));
    }

    #[test]
    #[cfg(feature = "ffmpeg")]
    fn infer_mp4_with_ffmpeg() {
        let backend = infer_backend(Path::new("out.mp4"), None).unwrap();
        assert!(matches!(backend, EncoderBackend::Ffmpeg));
    }

    #[test]
    #[cfg(feature = "ffmpeg")]
    fn infer_webm_with_ffmpeg() {
        let backend = infer_backend(Path::new("out.webm"), None).unwrap();
        assert!(matches!(backend, EncoderBackend::Ffmpeg));
    }
}

pub fn create(
    backend: EncoderBackend,
    path: &Path,
    width: usize,
    height: usize,
    no_loop: bool,
    frame_count: u64,
    show_progress: bool,
) -> Result<Box<dyn Encoder>> {
    match backend {
        EncoderBackend::Gifski => Ok(Box::new(GifskiEncoder::new(
            path,
            width,
            height,
            no_loop,
            frame_count,
            show_progress,
        )?)),
        #[cfg(feature = "ffmpeg")]
        EncoderBackend::Ffmpeg => Ok(Box::new(FfmpegEncoder::new(
            path,
            width,
            height,
            no_loop,
            frame_count,
            show_progress,
        )?)),
    }
}
