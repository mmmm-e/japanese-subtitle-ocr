use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};
pub const MODEL: &str = "muscgab/manga-ocr-base-CPT";
pub const PADDLE_MODEL: &str = "PaddlePaddle/PaddleOCR-VL-1.6";
pub fn model_label(id: &str) -> &str {
    match id {
        MODEL => "Manga OCR CPT v2",
        PADDLE_MODEL => "PaddleOCR-VL 1.6",
        other => other,
    }
}
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}
fn terminate_process(child: &mut Child) {
    // Windows venv python.exe may launch another interpreter. Kill the owned tree
    // so cancellation also releases the actual CUDA process, not only its launcher.
    if child.try_wait().ok().flatten().is_none() {
        #[cfg(windows)]
        {
            let _ = command("taskkill")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
    }
    let _ = child.wait();
}
pub fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled")
    }
    Ok(())
}
pub fn capture(mut cmd: Command, cancel: &AtomicBool, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Cannot start external tool; check executable path")?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let a = thread::spawn(move || {
        let mut b = Vec::new();
        BufReader::new(out).read_to_end(&mut b).map(|_| b)
    });
    let b = thread::spawn(move || {
        let mut b = Vec::new();
        BufReader::new(err).read_to_end(&mut b).map(|_| b)
    });
    let start = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() > timeout {
            terminate_process(&mut child);
            let _ = a.join();
            let _ = b.join();
            bail!("Cancelled or external tool timeout");
        }
        if let Some(s) = child.try_wait()? {
            break s;
        }
        thread::sleep(Duration::from_millis(30));
    };
    let out = a.join().map_err(|_| anyhow!("stdout reader failed"))??;
    let err = b.join().map_err(|_| anyhow!("stderr reader failed"))??;
    if !status.success() {
        bail!("External tool failed: {}", String::from_utf8_lossy(&err));
    }
    Ok(out)
}
fn is_true(value: &bool) -> bool {
    *value
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Settings {
    pub model: String,
    pub vl_python: String,
    pub vl_max_pixels: u32,
    pub vl_max_tokens: u32,
    pub python: String,
    pub worker: String,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub device: String,
    pub revision: String,
    pub fps: u32,
    pub upscale: bool,
    pub gray: bool,
    pub contrast: f32,
    pub sharpen: bool,
    pub fast: bool,
    #[serde(skip_serializing_if = "is_true")]
    pub merge_similar: bool,
}
impl Settings {
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(
            bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes),
        )?)
    }
    pub fn python_executable(&self) -> &str {
        if self.model == PADDLE_MODEL {
            &self.vl_python
        } else {
            &self.python
        }
    }
    pub fn ocr_cache_settings(&self) -> String {
        // Result grouping must not invalidate expensive, already cached OCR.
        let mut s = self.clone();
        s.merge_similar = true;
        serde_json::to_string(&s).expect("settings serialization")
    }
    pub fn worker_signature(&self) -> String {
        serde_json::json!({"model":self.model,"python":self.python_executable(),"worker":self.worker,"device":self.device,"revision":self.revision,"pixels":self.vl_max_pixels,"tokens":self.vl_max_tokens}).to_string()
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            model: MODEL.into(),
            vl_python: ".venv-vl/Scripts/python.exe".into(),
            vl_max_pixels: 1003520,
            vl_max_tokens: 512,
            python: if Path::new(".venv/Scripts/python.exe").exists() {
                ".venv/Scripts/python.exe".into()
            } else {
                "python".into()
            },
            worker: "worker/ocr_worker.py".into(),
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            device: "auto".into(),
            revision: "main".into(),
            fps: 3,
            upscale: false,
            gray: false,
            contrast: 0.,
            sharpen: false,
            fast: false,
            merge_similar: true,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Default for Region {
    fn default() -> Self {
        Self {
            x: 0.05,
            y: 0.72,
            w: 0.9,
            h: 0.24,
        }
    }
}
impl Region {
    pub fn crop(&self, img: &image::DynamicImage) -> Result<image::DynamicImage> {
        if ![self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
            || self.w <= 0.
            || self.h <= 0.
        {
            bail!("Invalid subtitle region");
        }
        let (w, h) = (img.width(), img.height());
        let x = (self.x.clamp(0., 0.999) * w as f32) as u32;
        let y = (self.y.clamp(0., 0.999) * h as f32) as u32;
        Ok(img.crop_imm(
            x,
            y,
            ((self.w * w as f32) as u32).clamp(1, w - x),
            ((self.h * h as f32) as u32).clamp(1, h - y),
        ))
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Video {
    pub path: PathBuf,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
}
pub fn probe(path: &Path, s: &Settings, c: &AtomicBool) -> Result<Video> {
    let mut cmd = command(&s.ffprobe);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,duration:format=duration",
        "-of",
        "json",
    ])
    .arg(path);
    let v: serde_json::Value = serde_json::from_slice(&capture(cmd, c, Duration::from_secs(30))?)?;
    let st = &v["streams"][0];
    let duration = v["format"]["duration"]
        .as_str()
        .or(st["duration"].as_str())
        .context("Video duration unavailable")?
        .parse::<f64>()?;
    if !duration.is_finite() || duration <= 0. {
        bail!("Invalid video duration");
    }
    Ok(Video {
        path: path.to_path_buf(),
        duration,
        width: st["width"].as_u64().context("No video width")? as u32,
        height: st["height"].as_u64().context("No video height")? as u32,
    })
}
pub fn preview(v: &Video, t: f64, s: &Settings, c: &AtomicBool) -> Result<image::DynamicImage> {
    let mut cmd = command(&s.ffmpeg);
    cmd.args(["-v", "error", "-noautorotate", "-ss", &t.to_string(), "-i"])
        .arg(&v.path)
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ]);
    Ok(image::load_from_memory(&capture(
        cmd,
        c,
        Duration::from_secs(60),
    )?)?)
}
pub fn key(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn video_cache(v: &Video) -> Result<PathBuf> {
    let meta = fs::metadata(&v.path)?;
    let id = key(format!(
        "{}:{}:{:?}",
        fs::canonicalize(&v.path)?.display(),
        meta.len(),
        meta.modified()?
    )
    .as_bytes());
    let p = PathBuf::from(".cache").join(id);
    fs::create_dir_all(&p)?;
    fs::write(p.join("metadata.json"), serde_json::to_vec_pretty(v)?)?;
    Ok(p)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Frame {
    pub time: f64,
    pub path: PathBuf,
}
pub fn sample(v: &Video, s: &Settings, c: &AtomicBool) -> Result<Vec<Frame>> {
    let dir = video_cache(v)?.join(format!("frames-{}-pts-v1", s.fps));
    fs::create_dir_all(&dir)?;
    let manifest = dir.join("manifest.json");
    if manifest.exists() {
        let f: Vec<Frame> = serde_json::from_slice(&fs::read(&manifest)?)?;
        if f.iter().all(|f| f.path.exists()) {
            return Ok(f);
        }
    }
    let mut cmd = command(&s.ffmpeg);
    cmd.args(["-hide_banner","-y","-i"]).arg(&v.path).args(["-map","0:v:0","-an","-vf",&format!("setpts=PTS-STARTPTS,select='isnan(prev_selected_t)+gte(t-prev_selected_t,{})',showinfo",1./s.fps as f64),"-fps_mode","vfr"]).arg(dir.join("%08d.png"));
    // showinfo stderr is captured separately to preserve source PTS rather than frame-index timing.
    let mut child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut times = Vec::new();
        let mut tail = String::new();
        for l in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if let Some(p) = l.split("pts_time:").nth(1) {
                if let Some(t) = p
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                {
                    times.push(t);
                }
            }
            tail = l;
        }
        let _ = tx.send((times, tail));
    });
    let start = Instant::now();
    let status = loop {
        if c.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(86400) {
            terminate_process(&mut child);
            let _ = reader.join();
            bail!("Cancelled or decode timeout");
        }
        if let Some(s) = child.try_wait()? {
            break s;
        }
        thread::sleep(Duration::from_millis(50));
    };
    let _ = reader.join();
    let (times, tail) = rx.recv()?;
    if !status.success() {
        bail!("FFmpeg: {tail}");
    }
    let frames: Vec<_> = times
        .into_iter()
        .enumerate()
        .map(|(i, time)| Frame {
            time,
            path: dir.join(format!("{:08}.png", i + 1)),
        })
        .collect();
    if frames.is_empty() || frames.iter().any(|f| !f.path.exists()) {
        bail!("Frame/PTS manifest mismatch");
    }
    fs::write(manifest, serde_json::to_vec(&frames)?)?;
    Ok(frames)
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Recognition {
    pub text: String,
    pub elapsed_ms: f64,
    pub confidence: Option<f32>,
    pub model: String,
    pub revision: String,
    pub device: String,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Capabilities {
    pub input: String,
    pub gpu: bool,
    pub confidence: bool,
    pub max_concurrency: u32,
    pub internal_grayscale: bool,
    pub multiline_reliable: bool,
}
pub trait OcrBackend {
    fn capabilities(&self) -> Capabilities;
    fn identity(&self) -> String;
    fn recognize(&mut self, image: &Path, c: &AtomicBool) -> Result<Recognition>;
}
pub struct Worker {
    child: Child,
    input: ChildStdin,
    rx: Receiver<String>,
    id: u64,
    pub info: serde_json::Value,
}
impl Worker {
    pub fn start(s: &Settings, c: &AtomicBool) -> Result<Self> {
        check(c)?;
        fs::create_dir_all(".cache")?;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(".cache/worker.log")?;
        let mut child = command(s.python_executable())
            .arg("-u")
            .arg(&s.worker)
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .context("Python worker launch failed")?;
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut w = Self {
            child,
            input,
            rx,
            id: 0,
            info: serde_json::Value::Null,
        };
        w.info = w.rpc(
            serde_json::json!({"op":"init","model":s.model,"revision":s.revision,"device":s.device,"max_pixels":s.vl_max_pixels,"max_new_tokens":s.vl_max_tokens}),
            c,
            1800,
        )?;
        let _: Capabilities = serde_json::from_value(w.info["capabilities"].clone())
            .context("Invalid OCR capabilities")?;
        Ok(w)
    }
    fn rpc(
        &mut self,
        mut req: serde_json::Value,
        c: &AtomicBool,
        timeout: u64,
    ) -> Result<serde_json::Value> {
        self.id += 1;
        req["id"] = self.id.into();
        writeln!(self.input, "{req}")?;
        self.input.flush()?;
        let start = Instant::now();
        loop {
            if c.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(timeout) {
                terminate_process(&mut self.child);
                bail!("OCR cancelled or timed out; reload worker");
            }
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => {
                    let v: serde_json::Value =
                        serde_json::from_str(&line).context("Invalid JSON from OCR worker")?;
                    if v["id"].as_u64() != Some(self.id) {
                        bail!("OCR response ID mismatch");
                    }
                    if v["ok"] != true {
                        bail!("OCR: {}", v["error"]);
                    }
                    return Ok(v["result"].clone());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("OCR worker exited unexpectedly")
                }
                Err(_) => {}
            }
        }
    }
}
impl OcrBackend for Worker {
    fn capabilities(&self) -> Capabilities {
        serde_json::from_value(self.info["capabilities"].clone())
            .expect("validated worker capabilities")
    }
    fn identity(&self) -> String {
        format!(
            "{}:{}:{}:worker-v2:{}",
            self.info["model"],
            self.info["revision"],
            self.info["device"],
            key(self.info["inference"].to_string().as_bytes())
        )
    }
    fn recognize(&mut self, image: &Path, c: &AtomicBool) -> Result<Recognition> {
        Ok(serde_json::from_value(self.rpc(
            serde_json::json!({"op":"ocr","image":fs::canonicalize(image)?}),
            c,
            120,
        )?)?)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "{{\"id\":0,\"op\":\"shutdown\"}}");
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(300) {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        terminate_process(&mut self.child);
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleLane {
    #[default]
    Single,
    Left,
    Right,
}
impl SubtitleLane {
    pub fn label(self) -> &'static str {
        match self {
            Self::Single => "전체",
            Self::Left => "왼쪽",
            Self::Right => "오른쪽",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct RegionTrack {
    pub lane: SubtitleLane,
    pub region: Region,
}
impl RegionTrack {
    pub fn split() -> [Self; 2] {
        [
            Self {
                lane: SubtitleLane::Left,
                region: Region {
                    x: 0.02,
                    y: 0.60,
                    w: 0.46,
                    h: 0.38,
                },
            },
            Self {
                lane: SubtitleLane::Right,
                region: Region {
                    x: 0.52,
                    y: 0.60,
                    w: 0.46,
                    h: 0.38,
                },
            },
        ]
    }
}
pub fn sort_segments(rows: &mut [Segment]) {
    rows.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.lane.cmp(&b.lane)));
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Segment {
    #[serde(default)]
    pub lane: SubtitleLane,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub model: String,
    pub revision: String,
    pub review: bool,
    pub frame: PathBuf,
    pub crop: PathBuf,
    pub elapsed_ms: f64,
    pub confidence: Option<f32>,
    pub observations: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<temporal::TextVariant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_kind: Option<String>,
    #[serde(default)]
    pub edited: bool,
}
pub fn preprocess(img: image::DynamicImage, s: &Settings) -> image::DynamicImage {
    let mut img = img;
    if s.upscale {
        img = img.resize_exact(
            img.width().saturating_mul(2).min(8192),
            img.height().saturating_mul(2).min(8192),
            image::imageops::FilterType::Lanczos3,
        );
    }
    if s.gray {
        img = img.grayscale();
    }
    if s.contrast != 0. {
        img = img.adjust_contrast(s.contrast);
    }
    if s.sharpen {
        img = img.unsharpen(1., 1);
    }
    img
}
pub fn aggregate(
    segments: &mut Vec<Segment>,
    r: Recognition,
    time: f64,
    end: f64,
    frame: PathBuf,
    crop: PathBuf,
) {
    if r.text.trim().is_empty() {
        return;
    }
    if let Some(last) = segments.last_mut() {
        if last.text == r.text
            && last.model == r.model
            && last.revision == r.revision
            && (last.end - time).abs() < 0.002
        {
            last.end = end;
            last.observations += 1;
            return;
        }
    }
    let review = r.confidence.is_none() || r.text.is_ascii();
    segments.push(Segment {
        lane: SubtitleLane::Single,
        start: time,
        end,
        text: r.text,
        model: r.model,
        revision: r.revision,
        review,
        frame,
        crop,
        elapsed_ms: r.elapsed_ms,
        confidence: r.confidence,
        observations: 1,
        variants: Vec::new(),
        merge_kind: None,
        edited: false,
    });
}
pub fn pipeline(
    v: &Video,
    frames: &[Frame],
    region: Region,
    s: &Settings,
    ocr: &mut dyn OcrBackend,
    c: &AtomicBool,
    mut progress: impl FnMut(f32, &[Segment]),
) -> Result<Vec<Segment>> {
    let root = video_cache(v)?;
    let cropdir = root.join("crops");
    let resultdir = root.join("ocr");
    fs::create_dir_all(&cropdir)?;
    fs::create_dir_all(&resultdir)?;
    let mut segments = Vec::new();
    let mut previous: Option<(String, Recognition)> = None;
    for (i, frame) in frames.iter().enumerate() {
        check(c)?;
        let img = preprocess(region.crop(&image::open(&frame.path)?)?, s);
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png)?;
        let hash = key(png.get_ref());
        let crop = cropdir.join(format!("{hash}.png"));
        if !crop.exists() {
            fs::write(&crop, png.into_inner())?;
        }
        let id = key(format!("{}:{hash}:{}", ocr.identity(), s.ocr_cache_settings()).as_bytes());
        let cache = resultdir.join(format!("{id}.json"));
        let r = if s.fast && previous.as_ref().is_some_and(|(h, _)| h == &hash) {
            previous.as_ref().unwrap().1.clone()
        } else if cache.exists() {
            serde_json::from_slice(&fs::read(&cache)?)?
        } else {
            let r = ocr.recognize(&crop, c)?;
            check(c)?;
            fs::write(cache, serde_json::to_vec(&r)?)?;
            r
        };
        previous = Some((hash, r.clone()));
        aggregate(
            &mut segments,
            r,
            frame.time,
            frames.get(i + 1).map_or(v.duration, |f| f.time),
            frame.path.clone(),
            crop,
        );
        if i + 1 < frames.len() {
            progress((i + 1) as f32 / frames.len() as f32, &segments);
        }
    }
    if s.merge_similar {
        segments = temporal::merge_repeated_segments(&segments, c)?.segments;
    }
    check(c)?;
    progress(1., &segments);
    let id = key(format!(
        "temporal-v1:{:?}:{}:{}",
        region,
        serde_json::to_string(s)?,
        ocr.identity()
    )
    .as_bytes());
    fs::create_dir_all(root.join("segments"))?;
    fs::write(
        root.join("segments").join(format!("{id}.json")),
        serde_json::to_vec_pretty(&segments)?,
    )?;
    Ok(segments)
}
/// Decode once; each ROI is recognized serially using the same resident model.
pub fn pipeline_tracks(
    v: &Video,
    frames: &[Frame],
    tracks: &[RegionTrack],
    s: &Settings,
    ocr: &mut dyn OcrBackend,
    c: &AtomicBool,
    mut progress: impl FnMut(f32, &[Segment]),
) -> Result<Vec<Segment>> {
    if tracks.is_empty()
        || tracks.len() > 2
        || (tracks.len() == 2
            && (tracks[0].lane == tracks[1].lane
                || tracks.iter().any(|t| t.lane == SubtitleLane::Single)))
    {
        bail!("영역은 전체 하나 또는 왼쪽·오른쪽 한 쌍이어야 합니다.");
    }
    let mut complete = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        check(c)?;
        let mut rows = pipeline(v, frames, track.region, s, ocr, c, |p, current| {
            let mut combined = complete.clone();
            combined.extend(current.iter().cloned().map(|mut r| {
                r.lane = track.lane;
                r
            }));
            sort_segments(&mut combined);
            progress((index as f32 + p) / tracks.len() as f32, &combined);
        })?;
        for row in &mut rows {
            row.lane = track.lane;
        }
        complete.extend(rows);
    }
    sort_segments(&mut complete);
    check(c)?;
    let id = key(format!(
        "tracks-v1:{}:{}:{}",
        serde_json::to_string(tracks)?,
        serde_json::to_string(s)?,
        ocr.identity()
    )
    .as_bytes());
    fs::write(
        video_cache(v)?.join("segments").join(format!("{id}.json")),
        serde_json::to_vec_pretty(&complete)?,
    )?;
    Ok(complete)
}
pub fn timestamp(t: f64) -> String {
    let m = (t.max(0.) * 1000.).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        m / 3600000,
        m / 60000 % 60,
        m / 1000 % 60,
        m % 1000
    )
}
pub fn validate_segments(segments: &[Segment]) -> Result<()> {
    for s in segments {
        if !s.start.is_finite() || !s.end.is_finite() || s.start < 0. || s.end <= s.start {
            bail!("Invalid subtitle time range");
        }
    }
    Ok(())
}
pub fn load_results(path: &Path) -> Result<Vec<Segment>> {
    if fs::metadata(path)?.len() > 32 * 1024 * 1024 {
        bail!("Results JSON exceeds 32 MiB");
    }
    let rows: Vec<Segment> =
        serde_json::from_slice(&fs::read(path)?).context("Invalid Studio results JSON")?;
    validate_segments(&rows)?;
    Ok(rows)
}
pub fn export(path: &Path, segments: &[Segment]) -> Result<()> {
    validate_segments(segments)?;
    let content = match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "ass" => ass::render(segments)?,
        "json" => serde_json::to_string_pretty(segments)?,
        "txt" => segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        "srt" => segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    "{}\n{} --> {}\n{}\n\n",
                    i + 1,
                    timestamp(s.start),
                    timestamp(s.end),
                    s.text.replace("\r\n", "\n").replace("\n\n", "\n")
                )
            })
            .collect(),
        _ => bail!("Choose .ass, .srt, .txt or .json"),
    };
    fs::write(path, content)?;
    Ok(())
}

pub mod playback;

pub mod guide;

pub mod temporal;

pub mod ass;
