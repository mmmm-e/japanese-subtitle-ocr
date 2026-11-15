use japanese_subtitle_ocr::*;
use std::{path::PathBuf, sync::atomic::AtomicBool};
fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("cargo run --example gpu_smoke -- <video>");
    let c = AtomicBool::new(false);
    let s = Settings {
        model: std::env::args().nth(2).unwrap_or_else(|| MODEL.into()),
        device: "cuda".into(),
        fps: 1,
        ..Default::default()
    };
    let v = probe(&path, &s, &c)?;
    let mut worker = Worker::start(&s, &c)?;
    println!("{}", worker.info);
    anyhow::ensure!(
        worker.info["device"]
            .as_str()
            .unwrap_or("")
            .starts_with("cuda"),
        "Model is not on CUDA"
    );
    let frames = sample(&v, &s, &c)?;
    let rows = pipeline(
        &v,
        &frames,
        Region {
            x: 0.,
            y: 0.,
            w: 1.,
            h: 1.,
        },
        &s,
        &mut worker,
        &c,
        |p, _| println!("progress {p}"),
    )?;
    let out = path.with_extension("srt");
    export(&out, &rows)?;
    println!("Saved {} segments to {}", rows.len(), out.display());
    for row in rows {
        println!(
            "{} -> {}: {} ({:.0} ms)",
            row.start, row.end, row.text, row.elapsed_ms
        );
    }
    Ok(())
}
