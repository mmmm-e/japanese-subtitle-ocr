use japanese_subtitle_ocr::{playback::subtitle_text_lane, temporal::*, *};
use std::{path::Path, sync::atomic::AtomicBool, time::Duration};
fn row(lane: SubtitleLane, start: f64, end: f64, text: &str) -> Segment {
    Segment {
        lane,
        start,
        end,
        text: text.into(),
        model: "mock".into(),
        revision: "1".into(),
        review: true,
        frame: "frame.png".into(),
        crop: "crop.png".into(),
        elapsed_ms: 1.,
        confidence: None,
        observations: 1,
        variants: vec![],
        merge_kind: None,
        edited: false,
    }
}
#[test]
fn identical_left_right_subtitles_remain_independent() -> anyhow::Result<()> {
    let rows = vec![
        row(SubtitleLane::Left, 0., 1., "今日は"),
        row(SubtitleLane::Right, 0., 1., "今日は"),
        row(SubtitleLane::Left, 1., 2., "今日は"),
        row(SubtitleLane::Right, 1., 2., "また明日"),
    ];
    let report = merge_repeated_segments(&rows, &AtomicBool::new(false))?;
    assert_eq!(report.segments.len(), 3);
    assert_eq!(report.segments[0].lane, SubtitleLane::Left);
    assert_eq!(report.segments[0].end, 2.);
    assert_eq!(
        subtitle_text_lane(&report.segments, 1.5, SubtitleLane::Right),
        "また明日"
    );
    assert_eq!(
        subtitle_text_lane(&report.segments, 2., SubtitleLane::Left),
        ""
    );
    assert!(merge_selected_segments(&rows, &[0, 1]).is_err());
    let manual = merge_selected_segments(&rows, &[0, 2])?;
    assert_eq!(manual.len(), 3);
    assert_eq!(manual[0].end, 2.);
    let mut value = serde_json::to_value(&rows[0])?;
    value.as_object_mut().unwrap().remove("lane");
    assert_eq!(
        serde_json::from_value::<Segment>(value)?.lane,
        SubtitleLane::Single
    );
    Ok(())
}
#[test]
fn ass_preserves_overlap_centiseconds_and_plain_text() -> anyhow::Result<()> {
    let rows = vec![
        row(SubtitleLane::Right, 0.005, 1.001, "右字幕\n二行目"),
        row(SubtitleLane::Left, 0.005, 1.001, "左字幕{\\pos(0,0)}"),
        row(SubtitleLane::Single, 3599.9996, 3600.0001, "中央"),
    ];
    let ass = ass::render(&rows)?;
    assert!(ass.contains("PlayResX: 1920"));
    assert!(ass.contains("0:00:00.01,0:00:01.00,Left"));
    assert!(ass.contains("0:00:00.01,0:00:01.00,Right"));
    assert!(ass.contains("{\\an2\\pos(480,1000)}左字幕｛＼pos(0,0)｝"));
    assert!(ass.contains("{\\an2\\pos(1440,1000)}右字幕\\N二行目"));
    assert!(ass.contains("1:00:00.00,1:00:00.01,Single"));
    assert!(ass.find(",Left,,").unwrap() < ass.find(",Right,,").unwrap());
    assert_eq!(rows[1].text, "左字幕{\\pos(0,0)}");
    assert!(ass::render(&[row(SubtitleLane::Left, f64::NAN, 1., "bad")]).is_err());
    Ok(())
}
struct Mock {
    calls: usize,
}
impl OcrBackend for Mock {
    fn identity(&self) -> String {
        "dual-mock-v1".into()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            input: "cropped_text_image".into(),
            gpu: false,
            confidence: false,
            max_concurrency: 1,
            internal_grayscale: false,
            multiline_reliable: true,
        }
    }
    fn recognize(&mut self, path: &Path, cancel: &AtomicBool) -> anyhow::Result<Recognition> {
        check(cancel)?;
        self.calls += 1;
        let img = image::open(path)?.to_rgb8();
        assert_eq!(img.dimensions(), (160, 180));
        let pixel = img.get_pixel(50, 50);
        let text = if pixel[0] > 200 {
            "左の台詞"
        } else if pixel[2] > 200 {
            "右の台詞"
        } else {
            "右の次の台詞"
        };
        Ok(Recognition {
            text: text.into(),
            elapsed_ms: 1.,
            confidence: None,
            model: "mock".into(),
            revision: "1".into(),
            device: "cpu".into(),
        })
    }
}
#[test]
fn mock_dual_video_to_one_ass_and_cache() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let dir = temp.path().join("한글 空白");
    std::fs::create_dir(&dir)?;
    for i in 0..3 {
        let img = image::RgbImage::from_fn(320, 180, |x, _| {
            if x < 160 {
                image::Rgb([255, 0, 0])
            } else if i < 2 {
                image::Rgb([0, 0, 255])
            } else {
                image::Rgb([0, 255, 0])
            }
        });
        img.save(dir.join(format!("frame-{i}.png")))?;
    }
    let cancel = AtomicBool::new(false);
    let mut cmd = command("ffmpeg");
    cmd.current_dir(&dir).args([
        "-v",
        "error",
        "-framerate",
        "2",
        "-i",
        "frame-%d.png",
        "-c:v",
        "ffv1",
        "左右 video.mkv",
    ]);
    capture(cmd, &cancel, Duration::from_secs(30))?;
    let settings = Settings {
        fps: 2,
        ..Default::default()
    };
    let video = probe(&dir.join("左右 video.mkv"), &settings, &cancel)?;
    let frames = sample(&video, &settings, &cancel)?;
    let tracks = [
        RegionTrack {
            lane: SubtitleLane::Left,
            region: Region {
                x: 0.,
                y: 0.,
                w: 0.5,
                h: 1.,
            },
        },
        RegionTrack {
            lane: SubtitleLane::Right,
            region: Region {
                x: 0.5,
                y: 0.,
                w: 0.5,
                h: 1.,
            },
        },
    ];
    let mut mock = Mock { calls: 0 };
    let mut last_progress = 0.;
    let rows = pipeline_tracks(
        &video,
        &frames,
        &tracks,
        &settings,
        &mut mock,
        &cancel,
        |p, rows| {
            assert!(p >= last_progress);
            last_progress = p;
            assert!(rows.iter().all(|r| r.lane != SubtitleLane::Single));
        },
    )?;
    assert_eq!(last_progress, 1.);
    assert_eq!(mock.calls, 3);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        (rows[0].lane, rows[0].start, rows[0].end),
        (SubtitleLane::Left, 0., 1.5)
    );
    assert_eq!(
        (rows[1].lane, rows[1].start, rows[1].end),
        (SubtitleLane::Right, 0., 1.)
    );
    assert_eq!(
        (rows[2].lane, rows[2].start, rows[2].end),
        (SubtitleLane::Right, 1., 1.5)
    );
    export(&dir.join("字幕.ass"), &rows)?;
    export(&dir.join("字幕.json"), &rows)?;
    assert_eq!(
        load_results(&dir.join("字幕.json"))?[2].lane,
        SubtitleLane::Right
    );
    let ass = std::fs::read_to_string(dir.join("字幕.ass"))?;
    assert_eq!(ass.matches("Dialogue:").count(), 3);
    let mut cached = Mock { calls: 0 };
    pipeline_tracks(
        &video,
        &frames,
        &tracks,
        &settings,
        &mut cached,
        &cancel,
        |_, _| {},
    )?;
    assert_eq!(cached.calls, 0);
    assert!(pipeline_tracks(
        &video,
        &frames,
        &tracks,
        &settings,
        &mut cached,
        &AtomicBool::new(true),
        |_, _| {}
    )
    .is_err());
    assert!(pipeline_tracks(
        &video,
        &frames,
        &[tracks[0], tracks[0]],
        &settings,
        &mut cached,
        &cancel,
        |_, _| {}
    )
    .is_err());
    Ok(())
}
#[test]
fn ass_positions_render_with_libass() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    export(
        &temp.path().join("captions.ass"),
        &[
            row(SubtitleLane::Left, 0., 1., "左側字幕"),
            row(SubtitleLane::Right, 0., 1., "右側字幕"),
        ],
    )?;
    let mut cmd = command("ffmpeg");
    cmd.current_dir(temp.path()).args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=960x540:d=1",
        "-vf",
        "ass=captions.ass",
        "-frames:v",
        "1",
        "render.png",
    ]);
    capture(cmd, &AtomicBool::new(false), Duration::from_secs(30))?;
    let img = image::open(temp.path().join("render.png"))?.to_rgb8();
    for (left, right) in [(80, 400), (560, 880)] {
        let bright = (left..right)
            .flat_map(|x| (440..530).map(move |y| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y)[0] > 180)
            .count();
        assert!(
            bright > 30,
            "Missing subtitle at {left}..{right}: {bright} pixels"
        );
    }
    assert_eq!(
        (0..960).filter(|&x| img.get_pixel(x, 250)[0] > 180).count(),
        0
    );
    Ok(())
}
