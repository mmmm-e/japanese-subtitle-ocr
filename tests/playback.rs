use japanese_subtitle_ocr::{
    playback::{subtitle_text, PlaybackEvent, Player},
    *,
};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn wait_frame(player: &Player, min: f64) -> f64 {
    let limit = Instant::now();
    loop {
        let (frame, events) = player.poll();
        for e in events {
            if let PlaybackEvent::Error(s) = e {
                panic!("{s}");
            }
        }
        if let Some(f) = frame {
            if f.time >= min {
                return f.time;
            }
        }
        assert!(
            limit.elapsed() < Duration::from_secs(15),
            "Playback frame timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn playback_pause_resume_seek_and_end() -> anyhow::Result<()> {
    let settings = Settings::default();
    let video = probe(
        std::path::Path::new("fixtures/일본어 test.mp4"),
        &settings,
        &AtomicBool::new(false),
    )?;
    let mut player = Player::new();
    player.load(video.clone(), settings.clone(), 0., true, 0.);
    assert_eq!(wait_frame(&player, 0.), 0.);
    std::thread::sleep(Duration::from_millis(120));
    assert!(player.poll().0.is_none(), "Paused player advanced");
    player.pause(false);
    assert!(wait_frame(&player, 0.15) >= 0.15);
    player.pause(true);
    std::thread::sleep(Duration::from_millis(50));
    let _ = player.poll();
    std::thread::sleep(Duration::from_millis(100));
    assert!(player.poll().0.is_none());
    player.load(video.clone(), settings.clone(), 1.2, true, 0.);
    let t = wait_frame(&player, 1.2);
    assert!((t - 1.2).abs() < 0.04);
    player.load(video.clone(), settings.clone(), 0.4, true, 0.);
    let t = wait_frame(&player, 0.4);
    assert!((t - 0.4).abs() < 0.04, "Stale seek frame displayed");
    player.load(video, settings, 2.8, false, 0.);
    let start = Instant::now();
    loop {
        let (_, events) = player.poll();
        if events.iter().any(|e| matches!(e, PlaybackEvent::Ended)) {
            break;
        }
        assert!(!events.iter().any(|e| matches!(e, PlaybackEvent::Error(_))));
        assert!(start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(10));
    }
    player.stop();
    Ok(())
}
#[test]
fn broken_decoder_reports_error() -> anyhow::Result<()> {
    let mut player = Player::new();
    let s = Settings {
        ffmpeg: "no-such-ffmpeg-executable".into(),
        ..Default::default()
    };
    let v = Video {
        path: "missing.mp4".into(),
        duration: 1.,
        width: 320,
        height: 180,
    };
    player.load(v, s, 0., false, 0.);
    let start = Instant::now();
    loop {
        if player
            .poll()
            .1
            .iter()
            .any(|e| matches!(e, PlaybackEvent::Error(_)))
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn row(start: f64, end: f64, text: &str) -> Segment {
    Segment {
        lane: SubtitleLane::Single,
        start,
        end,
        text: text.into(),
        model: MODEL.into(),
        revision: "test".into(),
        review: true,
        frame: "frame.png".into(),
        crop: "crop.png".into(),
        elapsed_ms: 0.,
        confidence: None,
        observations: 1,
        variants: Vec::new(),
        merge_kind: None,
        edited: false,
    }
}
#[test]
fn subtitles_follow_boundaries_gaps_overlaps_and_edits() {
    let mut rows = vec![
        row(0., 1., "今日は"),
        row(1., 2., "また明日"),
        row(1.5, 2., "二行目"),
    ];
    assert_eq!(subtitle_text(&rows, -0.1), "");
    assert_eq!(subtitle_text(&rows, 0.999), "今日は");
    assert_eq!(subtitle_text(&rows, 1.), "また明日");
    assert_eq!(subtitle_text(&rows, 1.5), "また明日\n二行目");
    assert_eq!(subtitle_text(&rows, 2.), "");
    rows[1].text = "修正済み".into();
    assert_eq!(subtitle_text(&rows, 1.), "修正済み");
    rows[1].start = 1.2;
    assert_eq!(subtitle_text(&rows, 1.), "");
}

#[test]
fn saved_results_can_be_reopened_and_invalid_ranges_rejected() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("字幕.json");
    let rows = vec![row(0., 1., "保存済み")];
    export(&path, &rows)?;
    let loaded = load_results(&path)?;
    assert_eq!(subtitle_text(&loaded, 0.5), "保存済み");
    let mut invalid = loaded;
    invalid[0].end = -1.;
    std::fs::write(&path, serde_json::to_vec(&invalid)?)?;
    assert!(load_results(&path).is_err());
    Ok(())
}
