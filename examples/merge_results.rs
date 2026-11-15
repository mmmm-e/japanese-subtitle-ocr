use japanese_subtitle_ocr::{temporal::merge_repeated_segments, *};
use std::{path::PathBuf, sync::atomic::AtomicBool};
fn main() -> anyhow::Result<()> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("Pass saved Studio results JSON"),
    );
    let rows = load_results(&path)?;
    let report = merge_repeated_segments(&rows, &AtomicBool::new(false))?;
    let name = path.file_stem().unwrap().to_string_lossy();
    let out = path.with_file_name(format!("{name}-merged.json"));
    export(&out, &report.segments)?;
    export(&out.with_extension("srt"), &report.segments)?;
    println!(
        "{} -> {} segments; {} removed; {} variant groups; {} missing/unsuitable images",
        rows.len(),
        report.segments.len(),
        report.merged,
        report.uncertain,
        report.missing_images
    );
    for r in &report.segments {
        if r.variants.len() > 1 {
            println!(
                "{:.3}..{:.3} | {} observations | {} candidates",
                r.start,
                r.end,
                r.observations,
                r.variants.len()
            );
        }
    }
    println!("Saved {}", out.display());
    Ok(())
}
