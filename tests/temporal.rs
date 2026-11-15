use japanese_subtitle_ocr::{temporal::*, *};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
fn image(path: &Path, background: u8, changed: bool) -> anyhow::Result<()> {
    let mut image = image::RgbImage::from_pixel(480, 100, image::Rgb([background; 3]));
    for index in 0..24 {
        let x = 20 + index * 18;
        for y in 35..60 {
            image.put_pixel(x, y, image::Rgb([255; 3]));
            if !(changed && index == 6) {
                image.put_pixel(x + 8, y, image::Rgb([255; 3]));
            }
        }
        for dx in 0..9 {
            image.put_pixel(x + dx, 46, image::Rgb([255; 3]));
            if changed && index == 6 {
                image.put_pixel(x + dx, 35, image::Rgb([255; 3]));
            }
        }
    }
    image.save(path)?;
    Ok(())
}
fn row(start: f64, observations: u32, text: &str, crop: &Path) -> Segment {
    Segment {
        lane: SubtitleLane::Single,
        start,
        end: start + observations as f64 * 0.333,
        text: text.into(),
        model: MODEL.into(),
        revision: "v1".into(),
        review: true,
        frame: PathBuf::from("frame.png"),
        crop: crop.into(),
        elapsed_ms: 1.,
        confidence: None,
        observations,
        variants: vec![],
        merge_kind: None,
        edited: false,
    }
}
fn consecutive(mut rows: Vec<Segment>) -> Vec<Segment> {
    let mut next = 0.;
    for r in &mut rows {
        let duration = r.end - r.start;
        r.start = next;
        r.end = next + duration;
        next = r.end;
    }
    rows
}
const TEXT: &str = "今日は天気が良いので一緒に散歩に出かけましょう";
#[test]
fn one_frame_wobble_uses_observed_majority_and_keeps_candidates() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let a = temp.path().join("a.png");
    let b = temp.path().join("b.png");
    image(&a, 20, false)?;
    image(&b, 80, false)?;
    let bad = TEXT.replace('天', "夭");
    let rows = consecutive(vec![
        row(0., 1, TEXT, &a),
        row(0., 1, &bad, &b),
        row(0., 1, TEXT, &a),
    ]);
    let report = merge_repeated_segments(&rows, &AtomicBool::new(false))?;
    assert_eq!(report.segments.len(), 1);
    let r = &report.segments[0];
    assert_eq!(r.text, TEXT);
    assert_eq!(r.observations, 3);
    assert_eq!(r.start, 0.);
    assert_eq!(r.end, rows[2].end);
    assert_eq!(r.variants.len(), 2);
    assert!(r.review);
    assert_eq!(r.variants.iter().map(|v| v.observations).sum::<u32>(), 3);
    assert_eq!(
        merge_repeated_segments(&report.segments, &AtomicBool::new(false))?
            .segments
            .len(),
        1
    );
    Ok(())
}
#[test]
fn two_readings_with_a_box_choose_the_unbroken_observed_text() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let p = temp.path().join("text.png");
    image(&p, 30, false)?;
    let bad = TEXT.replace('天', "ㅁ");
    let rows = consecutive(vec![row(0., 1, &bad, &p), row(0., 1, TEXT, &p)]);
    let report = merge_repeated_segments(&rows, &AtomicBool::new(false))?;
    assert_eq!(report.segments.len(), 1);
    assert_eq!(report.segments[0].text, TEXT);
    assert_eq!(report.segments[0].variants.len(), 2);
    Ok(())
}
#[test]
fn local_glyph_change_is_not_hidden_by_matching_background() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let a = temp.path().join("a.png");
    let b = temp.path().join("b.png");
    image(&a, 20, false)?;
    image(&b, 20, true)?;
    let different = TEXT.replace('天', "夭");
    let rows = consecutive(vec![row(0., 3, TEXT, &a), row(0., 1, &different, &b)]);
    assert_eq!(
        merge_repeated_segments(&rows, &AtomicBool::new(false))?
            .segments
            .len(),
        2
    );
    Ok(())
}
#[test]
fn stable_changed_caption_short_caption_gap_and_missing_evidence_stay_separate(
) -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let p = temp.path().join("text.png");
    image(&p, 20, false)?;
    let different = TEXT.replace('天', "夭");
    let c = AtomicBool::new(false);
    let stable = consecutive(vec![row(0., 3, TEXT, &p), row(0., 3, &different, &p)]);
    assert_eq!(merge_repeated_segments(&stable, &c)?.segments.len(), 2);
    let short = consecutive(vec![row(0., 3, "今日は", &p), row(0., 1, "今曰は", &p)]);
    assert_eq!(merge_repeated_segments(&short, &c)?.segments.len(), 2);
    let mut gap = consecutive(vec![row(0., 1, TEXT, &p), row(0., 1, TEXT, &p)]);
    gap[1].start += 0.1;
    gap[1].end += 0.1;
    assert_eq!(merge_repeated_segments(&gap, &c)?.segments.len(), 2);
    let missing = consecutive(vec![
        row(0., 3, TEXT, Path::new("missing.png")),
        row(0., 1, &different, Path::new("missing-too.png")),
    ]);
    assert_eq!(merge_repeated_segments(&missing, &c)?.segments.len(), 2);
    Ok(())
}
#[test]
fn exact_duplicates_join_without_images_but_not_across_models() -> anyhow::Result<()> {
    let p = Path::new("missing.png");
    let rows = consecutive(vec![row(0., 1, TEXT, p), row(0., 1, TEXT, p)]);
    assert_eq!(
        merge_repeated_segments(&rows, &AtomicBool::new(false))?
            .segments
            .len(),
        1
    );
    let mut mixed = rows;
    mixed[1].model = PADDLE_MODEL.into();
    assert_eq!(
        merge_repeated_segments(&mixed, &AtomicBool::new(false))?
            .segments
            .len(),
        2
    );
    Ok(())
}
#[test]
fn manual_merge_is_reversible_and_preserves_user_edits() -> anyhow::Result<()> {
    let p = Path::new("missing.png");
    let mut input = consecutive(vec![
        row(0., 3, TEXT, p),
        row(0., 1, "別の読み取り", p),
        row(0., 1, "次の字幕", p),
    ]);
    let original = input[0].text.clone();
    input[0].text = "ユーザー修正".into();
    preserve_edit(&mut input[0], &original);
    let snapshot = input.clone();
    let merged = merge_selected_segments(&input, &[0, 1])?;
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].text, "ユーザー修正");
    assert!(merged[0].edited);
    assert!(merged[0].variants.iter().any(|v| v.text == TEXT));
    assert_eq!(input[0].text, snapshot[0].text);
    assert_eq!(input.len(), 3);
    assert!(merge_selected_segments(&input, &[0, 2]).is_err());
    assert!(merge_selected_segments(&input, &[0, 9]).is_err());
    let tmp = tempfile::tempdir()?;
    let file = tmp.path().join("merged.json");
    export(&file, &merged)?;
    let loaded = load_results(&file)?;
    assert_eq!(loaded[0].variants.len(), merged[0].variants.len());
    Ok(())
}
#[test]
fn auto_grouping_never_changes_user_edited_text_and_honors_cancel() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let p = temp.path().join("text.png");
    image(&p, 20, false)?;
    let mut rows = consecutive(vec![
        row(0., 3, TEXT, &p),
        row(0., 1, &TEXT.replace('天', "夭"), &p),
    ]);
    rows[1].edited = true;
    assert_eq!(
        merge_repeated_segments(&rows, &AtomicBool::new(false))?
            .segments
            .len(),
        2
    );
    assert!(merge_repeated_segments(&rows, &AtomicBool::new(true)).is_err());
    Ok(())
}
#[test]
fn grouping_setting_does_not_invalidate_raw_ocr_cache() -> anyhow::Result<()> {
    let old = Settings::default();
    let off = Settings {
        merge_similar: false,
        ..old.clone()
    };
    assert_eq!(old.ocr_cache_settings(), off.ocr_cache_settings());
    assert_eq!(old.worker_signature(), off.worker_signature());
    assert!(!serde_json::to_string(&old)?.contains("merge_similar"));
    assert!(serde_json::to_string(&off)?.contains("\"merge_similar\":false"));
    assert!(!Settings::from_json_bytes(&serde_json::to_vec(&off)?)?.merge_similar);
    assert!(Settings::from_json_bytes(b"{}")?.merge_similar);
    Ok(())
}

#[test]
fn canonical_glyph_variants_join_without_rewriting_raw_candidates() -> anyhow::Result<()> {
    let p = Path::new("missing.png");
    let input = consecutive(vec![row(0., 1, "慰み者", p), row(0., 1, "慰み者", p)]);
    let result = merge_repeated_segments(&input, &AtomicBool::new(false))?;
    assert_eq!(result.segments.len(), 1);
    assert_eq!(result.segments[0].variants.len(), 2);
    assert!(result.segments[0]
        .variants
        .iter()
        .any(|v| v.text == "慰み者"));
    let distinct = consecutive(vec![row(0., 1, "①", p), row(0., 1, "1", p)]);
    assert_eq!(
        merge_repeated_segments(&distinct, &AtomicBool::new(false))?
            .segments
            .len(),
        2
    );
    Ok(())
}
