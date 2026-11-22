//! One-shot capture requests for the desk's stage confidence view.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Mutex,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;

pub(crate) const CAPTURE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_WIDTH: u32 = 480;
const MAX_HEIGHT: u32 = 320;

#[derive(Serialize)]
pub(crate) struct StageSnapshot {
    pub(crate) source_width: u32,
    pub(crate) source_height: u32,
    pub(crate) png_base64: String,
}

#[derive(Debug)]
pub(crate) struct RawStageFrame {
    pub(super) source_size: [u32; 2],
    pub(super) size: [u32; 2],
    pub(super) rgba: Vec<u8>,
}

impl RawStageFrame {
    pub(crate) fn encode(self) -> Result<StageSnapshot, String> {
        let [width, height] = self.size;
        let row_size = width as usize * 4;
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        // OpenGL reads from the bottom row first. Output alpha is not needed
        // for the monitor because the projector shows the finished RGB frame.
        for row in self.rgba.chunks_exact(row_size).rev() {
            for pixel in row.chunks_exact(4) {
                rgb.extend_from_slice(&pixel[..3]);
            }
        }
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_compression(png::Compression::Fast);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&rgb))
            .map_err(|error| format!("Could not encode the stage frame: {error}"))?;
        Ok(StageSnapshot {
            source_width: self.source_size[0],
            source_height: self.source_size[1],
            png_base64: STANDARD.encode(bytes),
        })
    }
}

pub(super) fn thumbnail_size(width: u32, height: u32) -> [u32; 2] {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let scale = (f64::from(MAX_WIDTH) / width)
        .min(f64::from(MAX_HEIGHT) / height)
        .min(1.0);
    [
        (width * scale).round().max(1.0) as u32,
        (height * scale).round().max(1.0) as u32,
    ]
}

pub(crate) type CaptureReceiver = Receiver<Result<RawStageFrame, String>>;
pub(super) type CaptureSender = SyncSender<Result<RawStageFrame, String>>;

#[derive(Default)]
pub(super) struct CaptureQueue {
    ready: AtomicBool,
    request: Mutex<Option<(Instant, CaptureSender)>>,
}

impl CaptureQueue {
    pub(super) fn request(&self, now: Instant) -> Result<CaptureReceiver, String> {
        let mut pending = self
            .request
            .lock()
            .map_err(|_| "Stage capture is unavailable")?;
        if pending
            .as_ref()
            .is_some_and(|(deadline, _)| *deadline > now)
        {
            return Err("A stage capture is already waiting for this output".to_owned());
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        *pending = Some((now + CAPTURE_TIMEOUT, sender));
        self.ready.store(true, Ordering::Release);
        Ok(receiver)
    }

    pub(super) fn pending(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub(super) fn take(&self, now: Instant) -> Option<(Instant, CaptureSender)> {
        if !self.pending() {
            return None;
        }
        let mut pending = self.request.lock().ok()?;
        self.ready.store(false, Ordering::Release);
        let (deadline, sender) = pending.take()?;
        (deadline > now).then_some((deadline, sender))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_thumbnail_keeps_aspect_without_upscaling() {
        assert_eq!(thumbnail_size(1920, 1080), [480, 270]);
        assert_eq!(thumbnail_size(1080, 1920), [180, 320]);
        assert_eq!(thumbnail_size(150, 100), [150, 100]);
        assert_eq!(thumbnail_size(1, 4000), [1, 320]);
    }

    #[test]
    fn stage_capture_queue_bounds_requests_and_discards_expired_work() {
        let now = Instant::now();
        let queue = CaptureQueue::default();
        let first = queue.request(now).unwrap();
        assert!(queue.pending());
        assert!(queue.request(now).is_err());
        assert!(queue.take(now + CAPTURE_TIMEOUT).is_none());
        assert!(!queue.pending());
        assert_eq!(
            first.try_recv().err(),
            Some(mpsc::TryRecvError::Disconnected)
        );
        let second = queue.request(now + CAPTURE_TIMEOUT).unwrap();
        let (deadline, reply) = queue.take(now + CAPTURE_TIMEOUT).unwrap();
        assert_eq!(deadline, now + CAPTURE_TIMEOUT * 2);
        reply.try_send(Err("Closed".into())).unwrap();
        assert_eq!(second.try_recv().unwrap().err().as_deref(), Some("Closed"));
    }

    #[test]
    fn png_picture_is_upright_and_uses_opaque_output_colors() {
        let shot = RawStageFrame {
            source_size: [2, 2],
            size: [2, 2],
            rgba: vec![0, 0, 255, 0, 0, 0, 255, 0, 255, 0, 0, 128, 255, 0, 0, 128],
        }
        .encode()
        .unwrap();
        let mut png = png::Decoder::new(std::io::Cursor::new(
            STANDARD.decode(shot.png_base64).unwrap(),
        ))
        .read_info()
        .unwrap();
        let mut bytes = vec![0; png.output_buffer_size()];
        png.next_frame(&mut bytes).unwrap();
        assert_eq!(bytes, [255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255]);
    }
}
