//! Case-insensitive matching shared by every search surface, plus the element
//! that paints a hit the way a find-in-page does.
//!
//! Callers need [`contains_ignoring_case`] to test a hit, [`find_ignoring_case`]
//! to slice an excerpt around one, and [`highlighted`] to render text with every
//! hit marked. Case folding stays inside this module so all three agree on what
//! counts as a match.

use std::ops::Range;

use gpui::{
    AnyElement, App, HighlightStyle, IntoElement as _, ParentElement as _, SharedString,
    StyledText, div,
};
use gpui_component::ActiveTheme as _;

/// Case-insensitive containment.
pub fn contains_ignoring_case(value: &str, query: &str) -> bool {
    find_ignoring_case(value, query).is_some()
}

/// Character offset of the first case-insensitive match of `query` in `value`.
///
/// The offset counts characters so callers can slice an excerpt around the hit.
pub fn find_ignoring_case(value: &str, query: &str) -> Option<usize> {
    let needle = fold(query);
    if needle.is_empty() {
        return Some(0);
    }
    let haystack = fold(value);
    haystack
        .windows(needle.len())
        .position(|window| window == needle.as_slice())
}

/// Byte ranges of every case-insensitive match of `query` in `value`, in the
/// form text highlight runs expect.
///
/// Matches never overlap: the scan resumes after each hit, so a query of `aa`
/// reports one match in `aaa`.
pub fn match_ranges(value: &str, query: &str) -> Vec<Range<usize>> {
    let needle = fold(query);
    if needle.is_empty() {
        return Vec::new();
    }

    let chars: Vec<(usize, char)> = value.char_indices().collect();
    let mut ranges = Vec::new();
    let mut at = 0;
    while at + needle.len() <= chars.len() {
        let window = &chars[at..at + needle.len()];
        if window
            .iter()
            .zip(&needle)
            .all(|((_, ch), folded)| fold_char(*ch) == *folded)
        {
            let (start, _) = window[0];
            let (last_at, last_char) = window[needle.len() - 1];
            ranges.push(start..last_at + last_char.len_utf8());
            at += needle.len();
        } else {
            at += 1;
        }
    }
    ranges
}

/// `text` with every match of `query` marked, or plain text when nothing
/// matches.
pub fn highlighted(text: impl Into<SharedString>, query: &str, cx: &App) -> AnyElement {
    let text: SharedString = text.into();
    let ranges = match_ranges(&text, query);
    if ranges.is_empty() {
        return div().child(text).into_any_element();
    }

    let style = HighlightStyle {
        background_color: Some(cx.theme().blue.opacity(0.45)),
        color: Some(cx.theme().foreground),
        ..Default::default()
    };
    StyledText::new(text)
        .with_highlights(ranges.into_iter().map(|range| (range, style)))
        .into_any_element()
}

/// Folding one character at a time keeps offsets aligned with the source, which
/// a whole-string `to_lowercase` would not for characters that grow when folded.
fn fold(value: &str) -> Vec<char> {
    value.chars().map(fold_char).collect()
}

fn fold_char(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_reports_the_character_offset() {
        assert_eq!(find_ignoring_case("héllo Needle", "needle"), Some(6));
        assert_eq!(find_ignoring_case("no match here", "needle"), None);
    }

    #[test]
    fn ranges_cover_every_match_case_insensitively() {
        let ranges = match_ranges("Needle in a needle stack", "NEEDLE");
        assert_eq!(ranges, vec![0..6, 12..18]);
    }

    #[test]
    fn ranges_are_byte_offsets_past_multibyte_text() {
        let text = "héllo needle";
        let ranges = match_ranges(text, "needle");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "needle");
    }

    #[test]
    fn overlapping_matches_are_reported_once() {
        assert_eq!(match_ranges("aaa", "aa"), vec![0..2]);
    }

    #[test]
    fn an_empty_query_matches_nothing_to_highlight() {
        assert!(match_ranges("anything", "").is_empty());
    }
}
