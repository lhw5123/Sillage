//! Turns raw agent stdout into a document the pane can paint.
//!
//! Callers pass the whole transcript to [`render`]. Markdown (including GFM
//! tables) and A2UI 1.0 envelopes are recognized in the same stream; the
//! split, protocol, and widget mapping stay inside this module.

mod a2ui;
mod split;
mod view;

use gpui::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, div};

use crate::matching::contains_ignoring_case;
use crate::output::split::Segment;
use crate::output::view::{highlighted_body, markdown_view, surface_view};

/// Paint agent output: Markdown, GFM tables, and A2UI 1.0 surfaces.
///
/// A non-empty `query` marks search hits. Since the Markdown renderer takes no
/// highlight ranges, a segment holding a hit falls back to plain prose with the
/// hits marked, and returns to Markdown once the search closes.
pub fn render(source: &str, query: &str, cx: &App) -> AnyElement {
    if source.is_empty() {
        return div().child("No output yet.").into_any_element();
    }

    let mut children = Vec::new();
    for (index, segment) in split::segments(source).into_iter().enumerate() {
        match segment {
            Segment::Markdown(text) => {
                if text.trim().is_empty() {
                    continue;
                }
                if !query.is_empty() && contains_ignoring_case(&text, query) {
                    children.push(highlighted_body(&text, query, cx));
                    continue;
                }
                children.push(markdown_view(format!("md-{index}"), &text, cx));
            }
            Segment::A2ui(messages) => {
                let mut store = a2ui::Store::new();
                store.apply_all(&messages);
                for surface in store.surfaces() {
                    children.push(surface_view(index, surface, cx));
                }
            }
        }
    }

    if children.is_empty() {
        if !query.is_empty() && contains_ignoring_case(source, query) {
            return highlighted_body(source, query, cx);
        }
        return markdown_view("md-fallback", source, cx);
    }

    div()
        .flex()
        .flex_col()
        .gap_5()
        .w_full()
        .children(children)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::split::Segment;

    #[test]
    fn mixed_markdown_and_a2ui_split() {
        let source = r#"Hello **world**

```a2ui
{"version":"v1.0","createSurface":{"surfaceId":"card","components":[{"id":"root","component":"Text","text":"Hi"}],"dataModel":{}}}
```

Done.
"#;
        let parts = split::segments(source);
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0], Segment::Markdown(text) if text.contains("Hello")));
        assert!(matches!(&parts[1], Segment::A2ui(msgs) if msgs.len() == 1));
        assert!(matches!(&parts[2], Segment::Markdown(text) if text.contains("Done")));
    }
}
