//! Сегменты, абзацы, метки времени.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Вероятность, что на отрезке нет речи (оценка whisper).
    #[serde(skip)]
    pub no_speech: f32,
}

impl Segment {
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Self { start, end, text: text.into(), no_speech: 0.0 }
    }
}

pub fn normalize(text: &str) -> String {
    text.replace('\u{a0}', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 00:01:05
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
}

/// 00:01:05,400 — формат SRT.
pub fn srt_clock(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// «1 ч 05 мин», «4 мин 12 с», «38 с».
pub fn human_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h} ч {m:02} мин")
    } else if m > 0 {
        format!("{m} мин {s:02} с")
    } else {
        format!("{s} с")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Paragraph {
    pub start: f64,
    pub text: String,
}

/// Абзац рвётся на паузе дольше `gap` секунд или когда он разросся,
/// а фраза закончилась: стена текста на полстраницы не читается.
pub fn paragraphs(segments: &[Segment], gap: f64) -> Vec<Paragraph> {
    const SOFT_LIMIT: usize = 700;
    let mut out: Vec<Paragraph> = Vec::new();
    let mut prev_end = f64::NEG_INFINITY;
    for seg in segments {
        let text = normalize(&seg.text);
        if text.is_empty() {
            continue;
        }
        let split = match out.last() {
            None => true,
            Some(p) => {
                seg.start - prev_end > gap
                    || (p.text.chars().count() > SOFT_LIMIT && ends_sentence(&p.text))
            }
        };
        if split {
            out.push(Paragraph { start: seg.start, text });
        } else if let Some(p) = out.last_mut() {
            p.text.push(' ');
            p.text.push_str(&text);
        }
        prev_end = seg.end;
    }
    out
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['.', '!', '?', '…'])
}

pub fn word_count(segments: &[Segment]) -> usize {
    segments.iter().map(|s| s.text.split_whitespace().count()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        assert_eq!(clock(0.0), "00:00:00");
        assert_eq!(clock(65.4), "00:01:05");
        assert_eq!(clock(3661.0), "01:01:01");
        assert_eq!(srt_clock(65.4), "00:01:05,400");
        assert_eq!(human_duration(3900.0), "1 ч 05 мин");
        assert_eq!(human_duration(252.0), "4 мин 12 с");
    }

    #[test]
    fn groups_on_pause() {
        let segs = vec![
            Segment::new(0.2, 2.0, "Первая фраза."),
            Segment::new(2.4, 4.0, " Вторая  фраза."),
            Segment::new(8.0, 9.5, "После паузы."),
        ];
        let p = paragraphs(&segs, 1.4);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].text, "Первая фраза. Вторая фраза.");
        assert_eq!(p[1].start, 8.0);
    }
}
