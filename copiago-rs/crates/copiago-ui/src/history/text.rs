use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime};
use copiago_core::i18n::{Language, tr};
use windows::core::Result;

const CLOSING: &str = "，。、；：！？）」』”’…》";
const ELLIPSIS: char = '…';

pub(super) fn short_time(created: &str, today: NaiveDate, language: Language) -> String {
    let parsed = ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S%.f"]
        .into_iter()
        .find_map(|format| NaiveDateTime::parse_from_str(created, format).ok())
        .or_else(|| {
            DateTime::parse_from_rfc3339(created)
                .ok()
                .map(|stamp| stamp.naive_local())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(created, "%Y-%m-%d")
                .ok()
                .map(|date| date.and_time(NaiveTime::MIN))
        });
    let Some(moment) = parsed else {
        return String::new();
    };
    let date = moment.date();
    if date == today {
        return moment.format("%H:%M").to_string();
    }
    if today.checked_sub_days(Days::new(1)) == Some(date) {
        return tr!(language, "昨天 {time}", time = moment.format("%H:%M")).into_owned();
    }
    date.format(if date.year() == today.year() {
        "%m-%d"
    } else {
        "%Y-%m-%d"
    })
    .to_string()
}

pub(super) fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    match bytes {
        GIB.. => format!("{:.1} GB", bytes as f64 / GIB as f64),
        MIB.. => format!("{:.1} MB", bytes as f64 / MIB as f64),
        KIB.. => format!("{:.0} KB", bytes as f64 / KIB as f64),
        _ => format!("{bytes} B"),
    }
}

/// Truncate on a Unicode character boundary and add the same ellipsis as Python.
pub(super) fn fit(text: &str, width: f32, measure: impl Fn(&str) -> Result<f32>) -> Result<String> {
    if measure(text)? <= width {
        return Ok(text.to_owned());
    }
    let boundaries: Vec<_> = text
        .char_indices()
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect();
    let (mut low, mut high) = (0, boundaries.len() - 1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if measure(&format!("{}{ELLIPSIS}", &text[..boundaries[middle]]))? <= width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Ok(format!("{}{ELLIPSIS}", text[..boundaries[low]].trim_end()))
}

/// Wrap CJK character by character, keep ASCII words together when possible,
/// and keep closing punctuation with its preceding text.
pub(super) fn wrap(
    text: &str,
    width: f32,
    max_lines: usize,
    measure: impl Fn(&str) -> Result<f32>,
) -> Result<Vec<String>> {
    debug_assert!(max_lines > 0);
    let mut lines = Vec::with_capacity(max_lines);
    let mut current = String::new();
    for character in text.chars() {
        let end = current.len();
        current.push(character);
        if end == 0 || CLOSING.contains(character) || measure(&current)? <= width {
            continue;
        }
        let prior = &current[..end];
        let word_start = prior.rfind(' ').filter(|&split| {
            split > 0
                && character.is_ascii()
                && !character.is_whitespace()
                && prior[split + 1..].is_ascii()
        });
        let (line, overflow) = match word_start {
            Some(split) => (&current[..split], &current[split + 1..]),
            None => (prior, current[end..].trim_start()),
        };
        let line = line.trim_end();
        lines.push(line.to_owned());
        current = overflow.to_owned();
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current.trim_end().to_owned());
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.last_mut().expect("wrap requires at least one line");
        *last = fit(&format!("{last}{ELLIPSIS}"), width, measure)?;
    }
    Ok(lines)
}
