//! Чистка типичного мусора Whisper на русской речи.
//!
//! Модель училась на субтитрах с YouTube и на тишине или шуме диктофона
//! «вспоминает» титры: «Субтитры сделал DimaTorzok», «Продолжение следует…».
//! Ещё она иногда зацикливается и повторяет одну фразу десятки раз.

use crate::text::{normalize, Segment};

/// Подписи, которых не бывает в живой диктовке. Сравнение без регистра и знаков.
const PHANTOMS: &[&str] = &[
    "субтитры сделал",
    "субтитры создавал",
    "субтитры подогнал",
    "субтитры делал",
    "субтитры подготовил",
    "редактор субтитров",
    "корректор а егорова",
    "dimatorzok",
    "продолжение следует",
    "спасибо за просмотр",
    "благодарю за просмотр",
    "подписывайтесь на канал",
    "подписывайтесь на наш канал",
    "ставьте лайки",
    "ставьте лайк",
    "до новых встреч",
    "смотрите в следующей серии",
    "music",
    "динамичная музыка",
    "спокойная музыка",
    "музыкальная заставка",
    "аплодисменты",
];

pub struct Report {
    pub segments: Vec<Segment>,
    pub phantoms_removed: usize,
    pub repeats_removed: usize,
}

pub fn clean(segments: Vec<Segment>) -> Report {
    let mut phantoms_removed = 0;
    let mut repeats_removed = 0;
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    let mut last_key = String::new();
    let mut same_run = 0;

    for mut seg in segments {
        let text = normalize(&seg.text);
        let text = strip_brackets(&text);
        let (text, collapsed) = collapse_loops(&text);
        repeats_removed += collapsed;
        if text.is_empty() {
            continue;
        }
        let key = fold(&text);
        if key.is_empty() {
            continue;
        }
        if is_phantom(&key) {
            phantoms_removed += 1;
            continue;
        }
        // Один и тот же сегмент подряд: первый оставляем, второй терпим
        // (бывает «да, да»), третий и дальше — петля модели.
        if key == last_key {
            same_run += 1;
            if same_run >= 2 {
                repeats_removed += 1;
                continue;
            }
        } else {
            same_run = 0;
            last_key = key;
        }
        seg.text = text;
        out.push(seg);
    }
    Report { segments: out, phantoms_removed, repeats_removed }
}

/// Буквы и цифры в нижнем регистре через пробел.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_phantom(key: &str) -> bool {
    // Длинная фраза, где мелькнуло «спасибо за просмотр», — скорее живая речь.
    let words = key.split(' ').count();
    words <= 10 && PHANTOMS.iter().any(|p| key.contains(p))
}

/// «[музыка]», «(смех)», «*кашель*» — пометки из субтитров, не речь.
fn strip_brackets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0i32;
    let mut star = false;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            '*' => star = !star,
            _ if depth == 0 && !star => out.push(c),
            _ => {}
        }
    }
    // Незакрытая скобка или звёздочка — не трогаем текст вовсе.
    if depth != 0 || star {
        return text.to_string();
    }
    normalize(&out)
}

/// Сворачивает фразу из 1–8 слов, повторённую подряд 4+ раз, до одного раза.
/// Возвращает текст и сколько повторов выкинуто.
pub fn collapse_loops(text: &str) -> (String, usize) {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = words.iter().map(|w| fold(w)).collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut removed = 0;
    let mut i = 0;
    'outer: while i < words.len() {
        for n in 1..=8usize {
            if i + n * 4 > words.len() {
                break;
            }
            let mut reps = 1;
            while i + (reps + 1) * n <= words.len() && keys[i..i + n] == keys[i + reps * n..i + (reps + 1) * n] {
                reps += 1;
            }
            if reps >= 4 {
                // Последний повтор несёт точку в конце фразы — оставляем его.
                out.extend_from_slice(&words[i + (reps - 1) * n..i + reps * n]);
                removed += reps - 1;
                i += reps * n;
                continue 'outer;
            }
        }
        out.push(words[i]);
        i += 1;
    }
    (out.join(" "), removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_subtitle_credits() {
        let segs = vec![
            Segment::new(0.0, 2.0, "Истец просит взыскать неустойку."),
            Segment::new(2.0, 4.0, "Субтитры сделал DimaTorzok"),
            Segment::new(4.0, 6.0, "Продолжение следует..."),
        ];
        let r = clean(segs);
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.phantoms_removed, 2);
    }

    #[test]
    fn collapses_loops() {
        let (text, removed) = collapse_loops("и так далее и так далее и так далее и так далее и так далее. Конец");
        assert_eq!(text, "и так далее. Конец");
        assert_eq!(removed, 4);
        let (same, none) = collapse_loops("да да, конечно");
        assert_eq!(same, "да да, конечно");
        assert_eq!(none, 0);
    }

    #[test]
    fn drops_repeated_segments() {
        let segs = (0..6).map(|i| Segment::new(i as f64, i as f64 + 1.0, "Спасибо.")).collect();
        let r = clean(segs);
        assert_eq!(r.segments.len(), 2);
    }

    #[test]
    fn strips_annotations() {
        let r = clean(vec![Segment::new(0.0, 1.0, "[музыка] Начинаем заседание (шум)")]);
        assert_eq!(r.segments[0].text, "Начинаем заседание");
    }
}
