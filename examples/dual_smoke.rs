use japanese_subtitle_ocr::*;
use std::{path::Path, sync::atomic::AtomicBool, time::Duration};
fn main() -> anyhow::Result<()> {
    std::fs::create_dir_all("outputs")?;
    let cancel = AtomicBool::new(false);
    let mut cmd = command("ffmpeg");
    cmd.args([
        "-v",
        "error",
        "-y",
        "-loop",
        "1",
        "-i",
        "fixtures/japanese.png",
        "-filter_complex",
        "[0:v]scale=900:144,split[a][b];[a][b]hstack,pad=1920:1080:60:860:white",
        "-t",
        "2",
        "-r",
        "2",
        "-pix_fmt",
        "yuv420p",
        "outputs/dual-smoke.mp4",
    ]);
    capture(cmd, &cancel, Duration::from_secs(30))?;
    let settings = Settings {
        model: PADDLE_MODEL.into(),
        device: "cuda".into(),
        fps: 1,
        ..Default::default()
    };
    let video = probe(Path::new("outputs/dual-smoke.mp4"), &settings, &cancel)?;
    let frames = sample(&video, &settings, &cancel)?;
    let tracks = [
        RegionTrack {
            lane: SubtitleLane::Left,
            region: Region {
                x: 0.02,
                y: 0.78,
                w: 0.46,
                h: 0.16,
            },
        },
        RegionTrack {
            lane: SubtitleLane::Right,
            region: Region {
                x: 0.52,
                y: 0.78,
                w: 0.46,
                h: 0.16,
            },
        },
    ];
    let mut worker = Worker::start(&settings, &cancel)?;
    anyhow::ensure!(
        worker.info["device"]
            .as_str()
            .unwrap_or("")
            .starts_with("cuda"),
        "Model is not on CUDA"
    );
    println!(
        "actual device={} GPU={}",
        worker.info["device"], worker.info["gpu"]
    );
    let rows = pipeline_tracks(
        &video,
        &frames,
        &tracks,
        &settings,
        &mut worker,
        &cancel,
        |p, _| println!("progress {p:.2}"),
    )?;
    anyhow::ensure!(
        rows.len() == 2,
        "Expected two independent subtitle lanes: {}",
        rows.len()
    );
    for (row, lane) in rows.iter().zip([SubtitleLane::Left, SubtitleLane::Right]) {
        anyhow::ensure!(
            row.lane == lane
                && row.text == "今日は楽しかった！"
                && row.start == 0.
                && row.end == 2.,
            "Unexpected subtitle {row:?}"
        );
        println!(
            "{} {:.2}..{:.2} {}",
            row.lane.label(),
            row.start,
            row.end,
            row.text
        );
    }
    export(Path::new("outputs/dual-smoke.ass"), &rows)?;
    export(Path::new("outputs/dual-smoke.json"), &rows)?;
    Ok(())
}
