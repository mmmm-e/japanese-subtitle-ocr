use crate::{validate_segments, Segment, SubtitleLane};
use anyhow::Result;
use std::fmt::Write;

fn timestamp(cs: u64) -> String {
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360000,
        cs / 6000 % 60,
        cs / 100 % 60,
        cs % 100
    )
}
/// ASS treats braces and backslash escapes as formatting. Render literal OCR
/// punctuation with fullwidth equivalents; the original is preserved in JSON.
fn text(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter_map(|c| match c {
            '\n' => Some("\\N".into()),
            '\\' => Some("＼".into()),
            '{' => Some("｛".into()),
            '}' => Some("｝".into()),
            '\t' => Some(" ".into()),
            c if c.is_control() => None,
            c => Some(c.to_string()),
        })
        .collect()
}
pub fn render(rows: &[Segment]) -> Result<String> {
    validate_segments(rows)?;
    let mut result=String::from("[Script Info]\nTitle: Japanese Video Subtitle OCR Studio\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    for (name, left, right) in [("Single", 60, 60), ("Left", 60, 1020), ("Right", 1020, 60)] {
        writeln!(result,"Style: {name},Yu Gothic,42,&H00FFFFFF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,1,2,{left},{right},80,1")?;
    }
    result.push_str("\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    let mut sorted: Vec<_> = rows.iter().collect();
    sorted.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.lane.cmp(&b.lane)));
    for row in sorted {
        let start = (row.start * 100.).round() as u64;
        let end = ((row.end * 100.).round() as u64).max(start.saturating_add(1));
        let (style, x) = match row.lane {
            SubtitleLane::Single => ("Single", 960),
            SubtitleLane::Left => ("Left", 480),
            SubtitleLane::Right => ("Right", 1440),
        };
        writeln!(
            result,
            "Dialogue: 0,{},{},{style},,0,0,0,,{{\\an2\\pos({x},1000)}}{}",
            timestamp(start),
            timestamp(end),
            text(&row.text)
        )?;
    }
    Ok(result)
}
