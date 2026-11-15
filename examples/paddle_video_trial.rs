use japanese_subtitle_ocr::*;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("Pass an existing video's cache directory"),
    );
    let v: Video = serde_json::from_slice(&std::fs::read(root.join("metadata.json"))?)?;
    let frames: Vec<Frame> =
        serde_json::from_slice(&std::fs::read(root.join("frames-3-pts-v1/manifest.json"))?)?;
    let mut previous = f64::NEG_INFINITY;
    let frames: Vec<Frame> = frames
        .into_iter()
        .filter(|frame| {
            if frame.time - previous >= 1. {
                previous = frame.time;
                true
            } else {
                false
            }
        })
        .collect();
    let old: Vec<Segment> = serde_json::from_slice(&std::fs::read(
        std::fs::read_dir(root.join("segments"))?
            .next()
            .unwrap()?
            .path(),
    )?)?;
    let reference = &old[0];
    let full = image::open(&reference.frame)?;
    let crop = image::open(&reference.crop)?;
    // Recover the exact original ROI by matching pixels, not by guessing GUI coordinates.
    anyhow::ensure!(
        full.width() == crop.width(),
        "Trial expects the existing full-width ROI"
    );
    let full = full.to_rgba8();
    let crop = crop.to_rgba8();
    let y = (0..=full.height() - crop.height())
        .find(|y| {
            (0..crop.height()).step_by(31).all(|dy| {
                (0..crop.width())
                    .step_by(127)
                    .all(|x| full.get_pixel(x, y + dy) == crop.get_pixel(x, dy))
            })
        })
        .ok_or_else(|| anyhow::anyhow!("Original crop not located"))?;
    let region = Region {
        x: 0.,
        y: y as f32 / full.height() as f32,
        w: 1.,
        h: crop.height() as f32 / full.height() as f32,
    };
    let s = Settings {
        model: PADDLE_MODEL.into(),
        device: "cuda".into(),
        fps: 1,
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let mut worker = Worker::start(&s, &cancel)?;
    println!("{}", worker.info);
    println!("ROI y={y}, h={}, samples={}", crop.height(), frames.len());
    let mut mark = 0;
    let rows = pipeline(&v, &frames, region, &s, &mut worker, &cancel, |p, rows| {
        let percent = (p * 100.) as u32;
        if percent >= mark {
            println!(
                "{percent}% | {} segments | {}",
                rows.len(),
                rows.last()
                    .map(|r| r.text.replace('\n', " / "))
                    .unwrap_or_default()
            );
            mark = percent + 5;
        }
    })?;
    let out = Path::new(".cache/paddle-video-trial");
    export(&out.with_extension("srt"), &rows)?;
    export(&out.with_extension("json"), &rows)?;
    println!(
        "DONE: {} segments; .cache/paddle-video-trial.srt / .json",
        rows.len()
    );
    Ok(())
}
