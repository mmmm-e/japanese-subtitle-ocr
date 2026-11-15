use crate::{capture, command, Segment, Settings, Video};
use anyhow::{Context, Result};
use rodio::{OutputStream, Sink, Source};
use std::{
    collections::VecDeque,
    io::Read,
    process::{Child, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
const FPS: f64 = 30.;
const RATE: u32 = 48000;
const CHANNELS: u16 = 2;

pub fn subtitle_text(rows: &[Segment], time: f64) -> String {
    rows.iter()
        .filter(|r| r.start <= time && time < r.end)
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn subtitle_text_lane(rows: &[Segment], time: f64, lane: crate::SubtitleLane) -> String {
    rows.iter()
        .filter(|r| r.lane == lane && r.start <= time && time < r.end)
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
pub struct DisplayFrame {
    pub time: f64,
    pub image: image::RgbaImage,
}
pub enum PlaybackEvent {
    Ready { audio: bool },
    Ended,
    Error(String),
    Notice(String),
}
type FrameSlot = Arc<Mutex<Option<(u64, DisplayFrame)>>>;
enum Control {
    Load {
        video: Video,
        settings: Box<Settings>,
        start: f64,
        paused: bool,
        volume: f32,
        generation: u64,
        cancel: Arc<AtomicBool>,
    },
    Pause(bool),
    Volume(f32),
    Stop,
    Quit,
}
pub struct Player {
    tx: mpsc::Sender<Control>,
    events: Receiver<(u64, PlaybackEvent)>,
    frame: FrameSlot,
    generation: u64,
    cancel: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}
impl Player {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let (events, output) = mpsc::channel();
        let frame = Arc::new(Mutex::new(None));
        let slot = frame.clone();
        let worker = thread::spawn(move || {
            let mut session: Option<Session> = None;
            loop {
                match rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(Control::Load {
                        video,
                        settings,
                        start,
                        paused,
                        volume,
                        generation,
                        cancel,
                    }) => {
                        session = None;
                        match Session::start(&video, &settings, start, paused, volume, cancel) {
                            Ok(s) => session = Some(s),
                            Err(e) => {
                                let _ = events
                                    .send((generation, PlaybackEvent::Error(format!("{e:#}"))));
                                continue;
                            }
                        }
                        if let Some(s) = session.as_mut() {
                            s.generation = generation;
                        }
                    }
                    Ok(Control::Pause(paused)) => {
                        if let Some(s) = session.as_mut() {
                            s.pause(paused);
                        }
                    }
                    Ok(Control::Volume(volume)) => {
                        if let Some(s) = session.as_mut() {
                            if let Some((_, sink)) = &s.audio {
                                sink.set_volume(volume);
                            }
                        }
                    }
                    Ok(Control::Stop) => session = None,
                    Ok(Control::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if let Some(s) = session.as_mut() {
                    match s.tick(&slot, &events) {
                        Ok(true) => {}
                        Ok(false) => {
                            let _ = events.send((s.generation, PlaybackEvent::Ended));
                            session = None;
                        }
                        Err(e) => {
                            let _ =
                                events.send((s.generation, PlaybackEvent::Error(format!("{e:#}"))));
                            session = None;
                        }
                    }
                }
            }
        });
        Self {
            tx,
            events: output,
            frame,
            generation: 0,
            worker: Some(worker),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn load(&mut self, v: Video, s: Settings, start: f64, paused: bool, volume: f32) {
        self.cancel.store(true, Ordering::Relaxed);
        self.generation += 1;
        self.cancel = Arc::new(AtomicBool::new(false));
        let _ = self.tx.send(Control::Load {
            video: v,
            settings: Box::new(s),
            start,
            paused,
            volume,
            generation: self.generation,
            cancel: self.cancel.clone(),
        });
    }
    pub fn pause(&self, paused: bool) {
        let _ = self.tx.send(Control::Pause(paused));
    }
    pub fn volume(&self, value: f32) {
        let _ = self.tx.send(Control::Volume(value));
    }
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.generation += 1;
        let _ = self.tx.send(Control::Stop);
    }
    pub fn poll(&self) -> (Option<DisplayFrame>, Vec<PlaybackEvent>) {
        let frame = self
            .frame
            .lock()
            .unwrap()
            .take()
            .and_then(|(g, f)| (g == self.generation).then_some(f));
        let events = self
            .events
            .try_iter()
            .filter_map(|(g, e)| (g == self.generation).then_some(e))
            .collect();
        (frame, events)
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Control::Quit);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn send_bounded<T>(tx: &SyncSender<T>, mut value: T, c: &AtomicBool) -> bool {
    loop {
        if c.load(Ordering::Relaxed) {
            return false;
        }
        match tx.try_send(value) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(v)) => value = v,
        }
        thread::sleep(Duration::from_millis(3));
    }
}
struct AudioSource {
    rx: Receiver<Vec<f32>>,
    samples: VecDeque<f32>,
    played: Arc<AtomicU64>,
}
impl Iterator for AudioSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.samples.is_empty() {
            if let Ok(chunk) = self.rx.try_recv() {
                self.samples.extend(chunk);
            }
        }
        self.played.fetch_add(1, Ordering::Relaxed);
        Some(self.samples.pop_front().unwrap_or(0.))
    }
}
impl Source for AudioSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        CHANNELS
    }
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
struct Session {
    generation: u64,
    cancel: Arc<AtomicBool>,
    processes: Vec<Process>,
    frames: Receiver<image::RgbaImage>,
    pending: Option<image::RgbaImage>,
    index: u64,
    start: f64,
    duration: f64,
    paused: bool,
    ready: bool,
    clock: Instant,
    elapsed: f64,
    audio: Option<(OutputStream, Sink)>,
    audio_rx: Option<Receiver<Vec<f32>>>,
    played: Arc<AtomicU64>,
    created: Instant,
    notice: Option<String>,
    eof: bool,
}
impl Session {
    fn start(
        v: &Video,
        s: &Settings,
        start: f64,
        paused: bool,
        volume: f32,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self> {
        crate::check(&cancel)?;
        let start = start.clamp(0., (v.duration - 1. / FPS).max(0.));
        let scale = (1280. / v.width as f64).min(720. / v.height as f64).min(1.);
        let width = (v.width as f64 * scale).round().max(1.) as u32;
        let height = (v.height as f64 * scale).round().max(1.) as u32;
        std::fs::create_dir_all(".cache")?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(".cache/playback.log")?;
        let mut child = command(&s.ffmpeg)
            .args([
                "-v",
                "error",
                "-noautorotate",
                "-ss",
                &start.to_string(),
                "-i",
            ])
            .arg(&v.path)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-vf",
                &format!("scale={width}:{height},fps=30:start_time=0"),
                "-t",
                &(v.duration - start).to_string(),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .context("Cannot start playback FFmpeg")?;
        let mut stdout = child.stdout.take().unwrap();
        let video = Process(child);
        let (tx, frames) = mpsc::sync_channel(3);
        let c = cancel.clone();
        thread::spawn(move || {
            let size = width as usize * height as usize * 4;
            loop {
                let mut bytes = vec![0; size];
                if stdout.read_exact(&mut bytes).is_err() {
                    break;
                }
                let img = image::RgbaImage::from_raw(width, height, bytes).unwrap();
                if !send_bounded(&tx, img, &c) {
                    break;
                }
            }
        });
        let mut result = Self {
            generation: 0,
            cancel,
            processes: vec![video],
            frames,
            pending: None,
            index: 0,
            start,
            duration: v.duration,
            paused,
            ready: false,
            clock: Instant::now(),
            elapsed: 0.,
            audio: None,
            audio_rx: None,
            played: Arc::new(AtomicU64::new(0)),
            created: Instant::now(),
            notice: None,
            eof: false,
        };
        let mut probe = command(&s.ffprobe);
        probe
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=index",
                "-of",
                "csv=p=0",
            ])
            .arg(&v.path);
        let has_audio = !capture(probe, &result.cancel, Duration::from_secs(30))?.is_empty();
        if has_audio {
            match result.start_audio(v, s, start, volume) {
                Ok(()) => {}
                Err(e) => {
                    result.notice = Some(format!("Audio unavailable; playing silently: {e:#}"))
                }
            }
        }
        Ok(result)
    }
    fn start_audio(&mut self, v: &Video, s: &Settings, start: f64, volume: f32) -> Result<()> {
        let (stream, handle) = OutputStream::try_default().context("No audio output device")?;
        let sink = Sink::try_new(&handle)?;
        sink.pause();
        sink.set_volume(volume);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(".cache/playback.log")?;
        let mut child = command(&s.ffmpeg)
            .args(["-v", "error", "-ss", &start.to_string(), "-i"])
            .arg(&v.path)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-af",
                "aresample=async=1:first_pts=0",
                "-ac",
                "2",
                "-ar",
                "48000",
                "-f",
                "f32le",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()?;
        let mut stdout = child.stdout.take().unwrap();
        self.processes.push(Process(child));
        let (tx, rx) = mpsc::sync_channel(8);
        let cancel = self.cancel.clone();
        thread::spawn(move || loop {
            let mut chunk = vec![0u8; 8192];
            let mut filled = 0;
            while filled < chunk.len() {
                match stdout.read(&mut chunk[filled..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => filled += n,
                }
            }
            if filled == 0 {
                break;
            }
            let samples = chunk[..filled - filled % 8]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|x| f32::from_le_bytes(*x))
                .collect();
            if !send_bounded(&tx, samples, &cancel) {
                break;
            }
            if filled < chunk.len() {
                break;
            }
        });
        self.audio_rx = Some(rx);
        self.audio = Some((stream, sink));
        Ok(())
    }
    fn pause(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        if paused {
            self.elapsed += self.clock.elapsed().as_secs_f64();
        } else {
            self.clock = Instant::now();
        }
        self.paused = paused;
        if let Some((_, sink)) = &self.audio {
            if paused {
                sink.pause();
            } else if self.ready {
                sink.play();
            }
        }
    }
    fn position(&self) -> f64 {
        if self.audio.is_some() {
            self.start
                + self.played.load(Ordering::Relaxed) as f64 / (RATE as f64 * CHANNELS as f64)
        } else {
            self.start
                + self.elapsed
                + if self.paused {
                    0.
                } else {
                    self.clock.elapsed().as_secs_f64()
                }
        }
    }
    fn tick(
        &mut self,
        slot: &FrameSlot,
        events: &mpsc::Sender<(u64, PlaybackEvent)>,
    ) -> Result<bool> {
        if self.cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if self.created.elapsed() > Duration::from_secs(30) && !self.ready {
            anyhow::bail!("Playback decoder timed out. See .cache/playback.log");
        }
        if self.pending.is_none() && !self.eof {
            match self.frames.try_recv() {
                Ok(f) => self.pending = Some(f),
                Err(TryRecvError::Disconnected) => self.eof = true,
                Err(TryRecvError::Empty) => {}
            }
        }
        if !self.ready && self.pending.is_some() {
            if let Some(rx) = self.audio_rx.take() {
                match rx.try_recv() {
                    Ok(chunk) => {
                        let source = AudioSource {
                            rx,
                            samples: chunk.into(),
                            played: self.played.clone(),
                        };
                        self.audio.as_ref().unwrap().1.append(source);
                    }
                    Err(TryRecvError::Empty) => {
                        self.audio_rx = Some(rx);
                        return Ok(true);
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.audio = None;
                        self.notice=Some("Audio decoder returned no samples; playing silently. See .cache/playback.log".into());
                    }
                }
            }
            self.ready = true;
            self.clock = Instant::now();
            self.elapsed = 0.;
            if !self.paused {
                if let Some((_, sink)) = &self.audio {
                    sink.play();
                }
            }
            let _ = events.send((
                self.generation,
                PlaybackEvent::Ready {
                    audio: self.audio.is_some(),
                },
            ));
            if let Some(notice) = self.notice.take() {
                let _ = events.send((self.generation, PlaybackEvent::Notice(notice)));
            }
        }
        if !self.ready {
            if self.eof {
                anyhow::bail!("Playback returned no frames. See .cache/playback.log");
            }
            return Ok(true);
        }
        if self.eof {
            if let Some(status) = self.processes[0].0.try_wait()? {
                if !status.success() {
                    anyhow::bail!("Playback decoder failed: {status}. See .cache/playback.log");
                }
            }
        }
        let target = self.position().min(self.duration);
        let mut latest = None;
        while self.start + self.index as f64 / FPS <= target + 0.002 {
            if let Some(image) = self.pending.take() {
                latest = Some(DisplayFrame {
                    time: (self.start + self.index as f64 / FPS).min(self.duration),
                    image,
                });
                self.index += 1;
            } else {
                break;
            }
            match self.frames.try_recv() {
                Ok(f) => self.pending = Some(f),
                Err(TryRecvError::Disconnected) => {
                    self.eof = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if let Some(frame) = latest {
            *slot.lock().unwrap() = Some((self.generation, frame));
        }
        if self.paused {
            return Ok(true);
        }
        Ok(!(target >= self.duration
            || self.eof
                && self.pending.is_none()
                && target >= self.start + self.index as f64 / FPS))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some((_, sink)) = &self.audio {
            sink.stop();
        }
    }
}
