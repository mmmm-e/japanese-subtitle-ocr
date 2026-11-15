use japanese_subtitle_ocr::*;
use std::{path::PathBuf, sync::atomic::AtomicBool};
fn main() -> anyhow::Result<()> {
    let source = PathBuf::from(std::env::args_os().nth(1).expect("Pass cached crop PNG"));
    let img = image::open(&source)?;
    println!("Input {}x{}", img.width(), img.height());
    let cancel = AtomicBool::new(false);
    let mut worker = Worker::start(
        &Settings {
            model: std::env::args().nth(2).unwrap_or_else(|| MODEL.into()),
            device: "cuda".into(),
            ..Default::default()
        },
        &cancel,
    )?;
    println!("device {}", worker.info["device"]);
    for (name, region) in [
        (
            "whole",
            Region {
                x: 0.,
                y: 0.,
                w: 1.,
                h: 1.,
            },
        ),
        (
            "line1",
            Region {
                x: 0.315,
                y: 0.49,
                w: 0.37,
                h: 0.20,
            },
        ),
        (
            "line2",
            Region {
                x: 0.195,
                y: 0.71,
                w: 0.61,
                h: 0.21,
            },
        ),
    ] {
        let path = PathBuf::from(format!(".cache/diagnostic-{name}.png"));
        region.crop(&img)?.save(&path)?;
        let r = worker.recognize(&path, &cancel)?;
        println!("{name}: {} ({:.0}ms)", r.text, r.elapsed_ms);
    }
    Ok(())
}
