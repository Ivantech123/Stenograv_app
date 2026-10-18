//! TXT (UTF-8 с BOM — Блокнот и Word открывают сразу), DOCX и SRT.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::settings::OnExists;
use crate::text::{self, clock, paragraphs, srt_clock, Segment};

pub struct Meta<'a> {
    pub source: &'a Path,
    pub duration: f64,
    pub engine: &'a str,
    pub language: &'a str,
    pub timestamps: bool,
    pub header: bool,
}

const PARAGRAPH_GAP: f64 = 1.4;

pub fn render_txt(segments: &[Segment], meta: &Meta) -> String {
    let mut lines: Vec<String> = Vec::new();
    if meta.header {
        lines.extend(header_lines(meta));
        lines.push(String::new());
    }
    let paras = paragraphs(segments, PARAGRAPH_GAP);
    if paras.is_empty() {
        lines.push("(Речь не обнаружена.)".into());
    }
    for p in &paras {
        lines.push(if meta.timestamps { format!("[{}] {}", clock(p.start), p.text) } else { p.text.clone() });
        lines.push(String::new());
    }
    let mut out = lines.join("\r\n").trim_end().to_string();
    out.push_str("\r\n");
    out
}

fn header_lines(meta: &Meta) -> Vec<String> {
    vec![
        format!("Источник: {}", meta.source.display()),
        format!("Длительность: {}", clock(meta.duration)),
        format!("Дата расшифровки: {}", chrono::Local::now().format("%d.%m.%Y %H:%M")),
        format!("Движок: {}", meta.engine),
        format!("Язык: {}", meta.language),
    ]
}

/// Субтитры: короткие строки, чтобы видео с записью можно было смотреть с текстом.
pub fn render_srt(segments: &[Segment]) -> String {
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{}\r\n{} --> {}\r\n{}\r\n\r\n",
            i + 1,
            srt_clock(seg.start),
            srt_clock(seg.end.max(seg.start + 0.3)),
            seg.text.trim()
        ));
    }
    out
}

/// Рабочий стандарт: Times New Roman 12, поля 25/20/10/20 мм, абзац 1,25 см.
pub fn render_docx(segments: &[Segment], meta: &Meta, path: &Path) -> Result<()> {
    use docx_rs::*;
    const MM: f32 = 56.692_9; // твипов в миллиметре
    let tw = |mm: f32| (mm * MM).round() as i32;

    let fonts = RunFonts::new().ascii("Times New Roman").hi_ansi("Times New Roman").cs("Times New Roman").east_asia("Times New Roman");
    let mut doc = Docx::new()
        .default_fonts(fonts)
        .default_size(24)
        .page_margin(PageMargin::new().top(tw(20.0)).bottom(tw(20.0)).left(tw(25.0)).right(tw(10.0)));

    let line = |text: &str| Paragraph::new().add_run(Run::new().add_text(text));
    if meta.header {
        let name = meta.source.file_stem().unwrap_or_default().to_string_lossy().to_string();
        doc = doc.add_paragraph(
            Paragraph::new().align(AlignmentType::Center).add_run(Run::new().add_text(format!("Расшифровка записи «{name}»")).bold()),
        );
        for text in header_lines(meta) {
            doc = doc.add_paragraph(line(&text).size(20).line_spacing(LineSpacing::new().after(0)));
        }
        doc = doc.add_paragraph(Paragraph::new());
    }
    let paras = paragraphs(segments, PARAGRAPH_GAP);
    if paras.is_empty() {
        doc = doc.add_paragraph(line("(Речь не обнаружена.)"));
    }
    for p in paras {
        let mut para = Paragraph::new()
            .align(AlignmentType::Both)
            .indent(None, Some(SpecialIndentType::FirstLine(tw(12.5))), None, None);
        if meta.timestamps {
            para = para.add_run(Run::new().add_text(format!("[{}] ", clock(p.start))).color("808080"));
        }
        doc = doc.add_paragraph(para.add_run(Run::new().add_text(&p.text)));
    }
    let file = std::fs::File::create(path)?;
    doc.build().pack(file).context("DOCX не записался")?;
    Ok(())
}

/// Куда класть результат и что делать, если файл уже есть.
/// `None` — пропустить (OnExists::Skip и файл есть).
pub fn target(source: &Path, out_dir: Option<&Path>, ext: &str, on_exists: OnExists) -> Option<PathBuf> {
    let dir = out_dir.map(Path::to_path_buf).unwrap_or_else(|| source.parent().map(Path::to_path_buf).unwrap_or_default());
    let stem = source.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return Some(first);
    }
    match on_exists {
        OnExists::Overwrite => Some(first),
        OnExists::Skip => None,
        OnExists::Rename => (2..1000).map(|n| dir.join(format!("{stem} ({n}).{ext}"))).find(|p| !p.exists()),
    }
}

/// Сначала во временный файл, потом переименование: сбой не оставит полфайла.
pub fn write_atomic(path: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("Нет доступа к папке {}", parent.display()))?;
    }
    let tmp = path.with_extension("stenograf.tmp");
    write(&tmp)?;
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("Файл занят другой программой: {}", path.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("Не удалось сохранить {}", path.display()))?;
    Ok(())
}

pub fn write_text(path: &Path, content: &str) -> Result<()> {
    write_atomic(path, |tmp| {
        let mut bytes = Vec::with_capacity(content.len() + 3);
        bytes.extend_from_slice(b"\xEF\xBB\xBF");
        bytes.extend_from_slice(content.as_bytes());
        std::fs::write(tmp, bytes)?;
        Ok(())
    })
}

pub fn plain_text(segments: &[Segment]) -> Vec<text::Paragraph> {
    paragraphs(segments, PARAGRAPH_GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_without_header() {
        let segs = vec![Segment::new(0.0, 2.0, "Первая фраза."), Segment::new(8.0, 9.0, "После паузы.")];
        let meta = Meta { source: Path::new("запись.wma"), duration: 10.0, engine: "test", language: "ru", timestamps: true, header: false };
        let txt = render_txt(&segs, &meta);
        assert_eq!(txt, "[00:00:00] Первая фраза.\r\n\r\n[00:00:08] После паузы.\r\n");
    }

    #[test]
    fn rename_when_exists() {
        let dir = std::env::temp_dir().join(format!("stenograf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("запись.wma");
        std::fs::write(dir.join("запись.txt"), "old").unwrap();
        let t = target(&src, None, "txt", OnExists::Rename).unwrap();
        assert_eq!(t.file_name().unwrap(), "запись (2).txt");
        assert!(target(&src, None, "txt", OnExists::Skip).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
