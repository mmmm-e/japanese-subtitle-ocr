fn main() -> anyhow::Result<()> {
    std::fs::create_dir_all("docs")?;
    std::fs::write(
        "docs/사용설명서.md",
        japanese_subtitle_ocr::guide::manual_markdown(),
    )?;
    println!("docs/사용설명서.md 생성 완료");
    Ok(())
}
