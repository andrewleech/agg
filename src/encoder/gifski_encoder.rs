use std::fs::File;
use std::path::Path;
use std::thread;

use anyhow::Result;
use imgref::Img;

use super::{Encoder, FrameData};

pub struct GifskiEncoder {
    collector: Option<gifski::Collector>,
    writer_thread: Option<thread::JoinHandle<gifski::CatResult<()>>>,
}

impl GifskiEncoder {
    pub fn new(
        path: &Path,
        width: usize,
        height: usize,
        no_loop: bool,
        frame_count: u64,
        show_progress: bool,
    ) -> Result<Self> {
        let repeat = if no_loop {
            gifski::Repeat::Finite(0)
        } else {
            gifski::Repeat::Infinite
        };

        let settings = gifski::Settings {
            width: Some(width as u32),
            height: Some(height as u32),
            fast: true,
            repeat,
            ..Default::default()
        };

        let (collector, writer) = gifski::new(settings)?;
        let output = File::create(path)?;

        let writer_thread = thread::spawn(move || {
            if show_progress {
                let mut pr = gifski::progress::ProgressBar::new(frame_count);
                let result = writer.write(output, &mut pr);
                pr.finish();
                println!();
                result
            } else {
                let mut pr = gifski::progress::NoProgress {};
                writer.write(output, &mut pr)
            }
        });

        Ok(Self {
            collector: Some(collector),
            writer_thread: Some(writer_thread),
        })
    }
}

impl Drop for GifskiEncoder {
    fn drop(&mut self) {
        // Ensure the writer thread is joined even on early error return.
        // Drop the collector first to signal end-of-stream.
        self.collector.take();
        if let Some(handle) = self.writer_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Encoder for GifskiEncoder {
    fn add_frame(&mut self, frame: FrameData) -> Result<()> {
        let collector = self.collector.as_ref().expect("add_frame called after finish");
        let image = Img::new(frame.rgba_pixels, frame.width, frame.height);
        collector
            .add_frame_rgba(frame.index, image, frame.time)
            .map_err(|e| anyhow::anyhow!("{}", e))
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        // Drop collector to signal end-of-stream to the writer thread.
        drop(self.collector.take());

        let result = self
            .writer_thread
            .take()
            .expect("finish called more than once")
            .join()
            .expect("gifski writer thread panicked");

        result.map_err(|e| anyhow::anyhow!("{}", e))
    }
}
