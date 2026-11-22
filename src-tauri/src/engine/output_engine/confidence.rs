//! One-shot still captures and a bounded live preview of finished stage frames.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use uuid::Uuid;

pub(crate) const CAPTURE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_WIDTH: u32 = 480;
const MAX_HEIGHT: u32 = 320;
const PREVIEW_INTERVAL: Duration = Duration::from_micros(41_667);
const PREVIEW_LEASE: Duration = Duration::from_secs(1);

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

    pub(crate) fn preview_bytes(&self, serial: u32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.rgba.len() + 20);
        for word in [serial, self.size[0], self.size[1], self.source_size[0], self.source_size[1]] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        for row in self.rgba.chunks_exact(self.size[0] as usize * 4).rev() {
            bytes.extend_from_slice(row);
        }
        for pixel in bytes[20..].chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        bytes
    }
}

pub(super) fn thumbnail_size(width: u32, height: u32) -> [u32; 2] {
    fit_frame(width, height, MAX_WIDTH, MAX_HEIGHT)
}

pub(super) fn preview_size(width: u32, height: u32) -> [u32; 2] {
    fit_frame(width, height, 320, 180)
}

fn fit_frame(width: u32, height: u32, max_width: u32, max_height: u32) -> [u32; 2] {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let scale = (f64::from(max_width) / width)
        .min(f64::from(max_height) / height)
        .min(1.0);
    [
        (width * scale).round().max(1.0) as u32,
        (height * scale).round().max(1.0) as u32,
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewOwner {
    pub(crate) workspace: Uuid,
    pub(crate) session: Uuid,
}

struct PreviewLease {
    owner: PreviewOwner,
    expires: Instant,
    next_capture: Instant,
    serial: u32,
    latest: Option<Arc<RawStageFrame>>,
    error: Option<String>,
}

#[derive(Default)]
pub(super) struct PreviewQueue {
    active: AtomicBool,
    lease: Mutex<Option<PreviewLease>>,
}

impl PreviewQueue {
    pub(super) fn start(&self, owner: PreviewOwner, now: Instant) -> Result<(), String> {
        let mut lease = self.lease.lock().map_err(|_| "Stage preview is unavailable")?;
        *lease = Some(PreviewLease {
            owner, expires: now + PREVIEW_LEASE, next_capture: now, serial: 0,
            latest: None, error: None,
        });
        self.active.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn stop(&self, owner: PreviewOwner) {
        if let Ok(mut lease) = self.lease.lock() {
            if lease.as_ref().is_some_and(|lease| lease.owner == owner) {
                *lease = None;
                self.active.store(false, Ordering::Release);
            }
        }
    }

    pub(super) fn latest(&self, owner: PreviewOwner, after: u32, now: Instant)
        -> Result<Option<(u32, Arc<RawStageFrame>)>, String>
    {
        let mut lease = self.lease.lock().map_err(|_| "Stage preview is unavailable")?;
        let lease = lease.as_mut().filter(|lease| lease.owner == owner && lease.expires > now)
            .ok_or("The stage preview ended. Start it again.")?;
        if let Some(error) = &lease.error { return Err(error.clone()); }
        lease.expires = now + PREVIEW_LEASE;
        Ok(lease.latest.as_ref().filter(|_| lease.serial != after)
            .map(|frame| (lease.serial, Arc::clone(frame))))
    }

    // The projector thread never waits for WebView IPC or a workspace command.
    fn with_live_lease<T>(&self, now: Instant, inspect: impl FnOnce(&mut PreviewLease) -> T) -> Option<T> {
        if !self.active.load(Ordering::Acquire) { return None; }
        let mut slot = self.lease.try_lock().ok()?;
        if slot.as_ref().is_some_and(|lease| lease.expires <= now) {
            *slot = None;
            self.active.store(false, Ordering::Release);
        }
        slot.as_mut().filter(|lease| lease.error.is_none()).map(inspect)
    }

    pub(super) fn needs_first_frame(&self, now: Instant) -> bool {
        self.with_live_lease(now, |lease| lease.latest.is_none() && lease.next_capture <= now).unwrap_or(false)
    }

    pub(super) fn reserve(&self, now: Instant) -> Option<PreviewOwner> {
        self.with_live_lease(now, |lease| {
            if lease.next_capture > now { return None; }
            lease.next_capture = now + PREVIEW_INTERVAL;
            Some(lease.owner)
        }).flatten()
    }

    pub(super) fn publish(&self, owner: PreviewOwner, frame: Result<RawStageFrame, String>, now: Instant) {
        self.with_live_lease(now, |lease| {
            if lease.owner != owner { return; }
            match frame {
                Ok(frame) => {
                    lease.serial = lease.serial.wrapping_add(1).max(1);
                    lease.latest = Some(Arc::new(frame));
                }
                Err(error) => lease.error = Some(error),
            }
        });
    }
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

    #[test]
    fn stage_preview_delivers_only_new_frames_and_retires_old_sessions() {
        let now = Instant::now();
        let queue = PreviewQueue::default();
        let first = PreviewOwner { workspace: Uuid::new_v4(), session: Uuid::new_v4() };
        let second = PreviewOwner { session: Uuid::new_v4(), ..first };
        let frame = || RawStageFrame {
            source_size: [16, 9],
            size: [1, 2],
            rgba: vec![0, 0, 255, 0, 255, 0, 0, 128],
        };
        queue.start(first, now).unwrap();
        assert!(queue.needs_first_frame(now));
        assert!(queue.reserve(now) == Some(first));
        assert!(queue.reserve(now + PREVIEW_INTERVAL / 2).is_none());
        queue.publish(first, Ok(frame()), now);
        let (id, picture) = queue.latest(first, 0, now).unwrap().unwrap();
        assert_eq!(&picture.preview_bytes(id)[20..], &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(queue.latest(first, id, now).unwrap().is_none());
        assert!(!queue.needs_first_frame(now + PREVIEW_INTERVAL));

        queue.start(second, now).unwrap();
        queue.publish(first, Ok(frame()), now);
        queue.stop(first);
        assert!(queue.latest(second, 0, now).unwrap().is_none());
        assert!(queue.latest(first, 0, now).is_err());
        assert!(queue.reserve(now + PREVIEW_LEASE).is_none());
        assert!(queue.latest(second, 0, now + PREVIEW_LEASE).is_err());
    }
}
