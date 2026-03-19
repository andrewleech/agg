#[cfg(feature = "ffmpeg")]
use std::path::Path;

#[cfg(feature = "ffmpeg")]
use anyhow::{anyhow, Result};

#[cfg(feature = "ffmpeg")]
use ffmpeg_next as ffmpeg;
#[cfg(feature = "ffmpeg")]
use ffmpeg::{
    codec, format,
    format::Pixel,
    frame,
    software::scaling,
    Dictionary, Rational,
};
#[cfg(feature = "ffmpeg")]
use rgb::ComponentBytes;

#[cfg(feature = "ffmpeg")]
use super::{Encoder, FrameData};

#[cfg(feature = "ffmpeg")]
pub struct FfmpegEncoder {
    output_ctx: format::context::Output,
    encoder: ffmpeg::codec::encoder::video::Encoder,
    scaler: scaling::Context,
    stream_index: usize,
    stream_time_base: Rational,
    width: u32,
    height: u32,
    frame_count: u64,
    frames_encoded: u64,
    show_progress: bool,
}

#[cfg(feature = "ffmpeg")]
impl FfmpegEncoder {
    pub fn new(
        path: &Path,
        width: usize,
        height: usize,
        no_loop: bool,
        frame_count: u64,
        show_progress: bool,
    ) -> Result<Self> {
        ffmpeg::init().map_err(|e| anyhow!("ffmpeg init failed: {}", e))?;

        let mut output_ctx = format::output(path)
            .map_err(|e| anyhow!("failed to open output '{}': {}", path.display(), e))?;

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        // Round up to even for codecs that require it (H.264, VP9).
        let enc_width = if matches!(ext.as_str(), "mp4" | "webm") {
            ((width + 1) / 2) * 2
        } else {
            width
        } as u32;

        let enc_height = if matches!(ext.as_str(), "mp4" | "webm") {
            ((height + 1) / 2) * 2
        } else {
            height
        } as u32;

        // Determine codec id from format or extension fallback.
        let fmt_output = output_ctx.format();
        let guessed_id = fmt_output.codec(path, ffmpeg::media::Type::Video);

        let codec_id = if guessed_id != codec::Id::None {
            guessed_id
        } else {
            match ext.as_str() {
                "mp4" => codec::Id::H264,
                "webm" => codec::Id::VP9,
                "gif" => codec::Id::GIF,
                "apng" | "png" => codec::Id::APNG,
                "webp" => codec::Id::WEBP,
                _ => {
                    return Err(anyhow!(
                        "cannot determine video codec for extension '{}'",
                        ext
                    ))
                }
            }
        };

        // Find an encoder for the codec id. For H.264, prefer libx264 over the
        // built-in encoder because it produces a playable stream without extra work.
        let codec = match codec_id {
            codec::Id::H264 => {
                ffmpeg::encoder::find_by_name("libx264")
                    .or_else(|| ffmpeg::encoder::find(codec_id))
                    .ok_or_else(|| anyhow!("no encoder found for H.264 (install libx264)"))?
            }
            codec::Id::VP9 => {
                ffmpeg::encoder::find_by_name("libvpx-vp9")
                    .or_else(|| ffmpeg::encoder::find(codec_id))
                    .ok_or_else(|| anyhow!("no encoder found for VP9 (install libvpx)"))?
            }
            _ => ffmpeg::encoder::find(codec_id)
                .ok_or_else(|| anyhow!("no encoder found for codec {:?}", codec_id))?,
        };

        // Choose pixel format: YUV420P for video, RGB8 for GIF, RGBA for APNG, YUVA420P for WebP.
        let pix_fmt = match codec_id {
            codec::Id::H264 | codec::Id::VP9 => Pixel::YUV420P,
            codec::Id::GIF => Pixel::RGB8,
            codec::Id::APNG => Pixel::RGBA,
            codec::Id::WEBP => Pixel::YUVA420P,
            _ => Pixel::RGBA,
        };

        // Add a stream to the output using the codec so the muxer knows about it.
        let mut stream = output_ctx
            .add_stream(codec)
            .map_err(|e| anyhow!("failed to add stream: {}", e))?;
        let stream_index = stream.index();

        // Build and configure the encoder context.
        let mut encoder_ctx = codec::context::Context::new_with_codec(codec);

        // time_base: 1/10000 (0.1ms resolution) is enough for smooth animation.
        let time_base = Rational(1, 10000);
        encoder_ctx.set_time_base(time_base);

        let mut video_enc = encoder_ctx
            .encoder()
            .video()
            .map_err(|e| anyhow!("failed to get video encoder: {}", e))?;

        video_enc.set_width(enc_width);
        video_enc.set_height(enc_height);
        video_enc.set_format(pix_fmt);
        video_enc.set_time_base(time_base);
        video_enc.set_max_b_frames(0);
        video_enc.set_gop(10);

        let opened = match codec_id {
            codec::Id::H264 => {
                let mut opts = Dictionary::new();
                opts.set("preset", "fast");
                opts.set("crf", "23");
                video_enc
                    .open_as_with(codec, opts)
                    .map_err(|e| anyhow!("failed to open H.264 encoder: {}", e))?
            }
            codec::Id::VP9 => {
                let mut opts = Dictionary::new();
                opts.set("crf", "33");
                opts.set("b", "0");
                video_enc
                    .open_as_with(codec, opts)
                    .map_err(|e| anyhow!("failed to open VP9 encoder: {}", e))?
            }
            _ => video_enc
                .open_as(codec)
                .map_err(|e| anyhow!("failed to open encoder: {}", e))?,
        };

        // Copy encoder parameters back to the stream so the muxer has them.
        let params = codec::Parameters::from(&opened);
        stream.set_parameters(params);

        // GIF looping: the GIF muxer reads a "loop" metadata entry.
        if codec_id == codec::Id::GIF && !no_loop {
            let mut meta = Dictionary::new();
            meta.set("loop", "0");
            output_ctx.set_metadata(meta);
        }

        // Write file header.
        output_ctx
            .write_header()
            .map_err(|e| anyhow!("failed to write header: {}", e))?;

        // Read back the stream time base that the muxer may have adjusted.
        let stream_time_base = output_ctx
            .stream(stream_index)
            .ok_or_else(|| anyhow!("stream {} not found after write_header", stream_index))?
            .time_base();

        // Build scaler: RGBA source -> encoder pixel format destination.
        let scaler = scaling::Context::get(
            Pixel::RGBA,
            width as u32,
            height as u32,
            pix_fmt,
            enc_width,
            enc_height,
            scaling::Flags::BILINEAR,
        )
        .map_err(|e| anyhow!("failed to create scaler: {}", e))?;

        Ok(Self {
            output_ctx,
            encoder: opened,
            scaler,
            stream_index,
            stream_time_base,
            width: width as u32,
            height: height as u32,
            frame_count,
            frames_encoded: 0,
            show_progress,
        })
    }

    fn drain_packets(&mut self) -> Result<()> {
        let mut packet = ffmpeg::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    packet.set_stream(self.stream_index);
                    packet.rescale_ts(Rational(1, 10000), self.stream_time_base);
                    packet
                        .write_interleaved(&mut self.output_ctx)
                        .map_err(|e| anyhow!("failed to write packet: {}", e))?;
                }
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => return Err(anyhow!("receive_packet error: {}", e)),
            }
        }
        Ok(())
    }
}

#[cfg(feature = "ffmpeg")]
impl Encoder for FfmpegEncoder {
    fn add_frame(&mut self, frame_data: FrameData) -> Result<()> {
        // Build RGBA input frame from the raw pixel data.
        let mut src_frame = frame::Video::new(Pixel::RGBA, self.width, self.height);

        // Copy pixels row by row, respecting the frame's stride (linesize).
        let stride = src_frame.stride(0);
        let row_bytes = self.width as usize * 4; // 4 bytes per RGBA pixel
        let pixel_bytes: &[u8] = frame_data.rgba_pixels.as_bytes();
        let src_plane = src_frame.data_mut(0);

        for row in 0..self.height as usize {
            let src_start = row * row_bytes;
            let dst_start = row * stride;
            src_plane[dst_start..dst_start + row_bytes]
                .copy_from_slice(&pixel_bytes[src_start..src_start + row_bytes]);
        }

        // Scale to the encoder's pixel format and dimensions.
        let mut dst_frame = frame::Video::empty();
        self.scaler
            .run(&src_frame, &mut dst_frame)
            .map_err(|e| anyhow!("scaler error: {}", e))?;

        // Set PTS: absolute time in units of 1/10000 s.
        let pts = (frame_data.time * 10000.0).round() as i64;
        dst_frame.set_pts(Some(pts));

        // Send frame to encoder.
        self.encoder
            .send_frame(&*dst_frame)
            .map_err(|e| anyhow!("send_frame error: {}", e))?;

        self.drain_packets()?;

        self.frames_encoded += 1;

        if self.show_progress {
            eprint!(
                "\r{}/{} frames encoded",
                self.frames_encoded, self.frame_count
            );
        }

        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.encoder
            .send_eof()
            .map_err(|e| anyhow!("send_eof error: {}", e))?;

        self.drain_packets()?;

        self.output_ctx
            .write_trailer()
            .map_err(|e| anyhow!("write_trailer error: {}", e))?;

        if self.show_progress {
            eprintln!();
        }

        Ok(())
    }
}
