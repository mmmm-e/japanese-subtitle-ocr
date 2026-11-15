use japanese_subtitle_ocr::*;
use std::{path::Path, sync::atomic::AtomicBool};
struct Mock {
    calls: usize,
}
impl OcrBackend for Mock {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            input: "cropped_text_image".into(),
            gpu: false,
            confidence: false,
            max_concurrency: 1,
            internal_grayscale: false,
            multiline_reliable: false,
        }
    }
    fn identity(&self) -> String {
        "mock-v1".into()
    }
    fn recognize(&mut self, _: &Path, _: &AtomicBool) -> anyhow::Result<Recognition> {
        self.calls += 1;
        Ok(Recognition {
            text: if self.calls <= 2 {
                "今日は楽しかった！"
            } else {
                "また明日！"
            }
            .into(),
            elapsed_ms: 1.,
            confidence: None,
            model: "mock".into(),
            revision: "v1".into(),
            device: "cpu".into(),
        })
    }
}
#[test]
fn mock_video_to_srt_and_cache() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let dir = temp.path().join("한글 경로 空白");
    std::fs::create_dir(&dir)?;
    let path = dir.join("字幕 video.mkv");
    let cancel = AtomicBool::new(false);
    let mut cmd = command("ffmpeg");
    cmd.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=320x180:rate=12:duration=1.5",
        "-c:v",
        "ffv1",
    ])
    .arg(&path);
    capture(cmd, &cancel, std::time::Duration::from_secs(30))?;
    let s = Settings {
        fps: 2,
        ..Default::default()
    };
    let v = probe(&path, &s, &cancel)?;
    let frames = sample(&v, &s, &cancel)?;
    assert_eq!(frames.len(), 3);
    assert!((frames[1].time - 0.5).abs() < 0.01);
    assert_eq!(sample(&v, &s, &cancel)?.len(), 3);
    let mut mock = Mock { calls: 0 };
    let rows = pipeline(
        &v,
        &frames,
        Region::default(),
        &s,
        &mut mock,
        &cancel,
        |_, _| {},
    )?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].observations, 2);
    assert_eq!(rows[0].end, rows[1].start);
    let out = dir.join("日本語.srt");
    export(&out, &rows)?;
    let text = std::fs::read_to_string(out)?;
    assert!(text.contains("00:00:00,000 --> 00:00:01,000"));
    assert!(text.contains("今日は楽しかった！"));
    let mut again = Mock { calls: 0 };
    let cached = pipeline(
        &v,
        &frames,
        Region::default(),
        &s,
        &mut again,
        &cancel,
        |_, _| {},
    )?;
    assert_eq!(again.calls, 0);
    assert_eq!(cached.len(), rows.len());
    export(&dir.join("out.json"), &rows)?;
    export(&dir.join("out.txt"), &rows)?;
    Ok(())
}
#[test]
fn roi_clamps_and_rejects_nan() {
    let img = image::DynamicImage::new_rgb8(100, 50);
    let r = Region {
        x: 0.9,
        y: 0.9,
        w: 0.5,
        h: 0.5,
    };
    let c = r.crop(&img).unwrap();
    assert_eq!((c.width(), c.height()), (10, 5));
    assert!(Region { x: f32::NAN, ..r }.crop(&img).is_err());
}
#[test]
fn timestamp_rounding() {
    assert_eq!(timestamp(3599.9996), "01:00:00,000");
    assert_eq!(timestamp(-1.), "00:00:00,000");
}
#[test]
fn cancellation() {
    assert!(check(&AtomicBool::new(true)).is_err());
}
#[test]
fn different_text_never_fuzzy_merges() {
    let mut rows = Vec::new();
    let r = Recognition {
        text: "今日は".into(),
        elapsed_ms: 1.,
        confidence: None,
        model: "mock".into(),
        revision: "v1".into(),
        device: "cpu".into(),
    };
    aggregate(&mut rows, r.clone(), 0., 1., "a".into(), "b".into());
    aggregate(
        &mut rows,
        Recognition {
            text: "今曰は".into(),
            ..r
        },
        1.,
        2.,
        "a".into(),
        "b".into(),
    );
    assert_eq!(rows.len(), 2);
}
