//! DOM traversal and inspection utilities.

use ego_tree::NodeRef;
use scraper::ElementRef;
use scraper::node::Node;

/// HTML tags treated as block-level for whitespace handling.
const BLOCK_TAGS: &[&str] = &[
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "p",
    "div",
    "section",
    "article",
    "main",
    "aside",
    "header",
    "footer",
    "nav",
    "address",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "blockquote",
    "pre",
    "hr",
    "figure",
    "figcaption",
    "details",
    "summary",
    "table",
    "caption",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
];

/// Returns `true` if the tag is a block-level element.
#[must_use]
pub(crate) fn is_block_tag(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

/// Returns the length of the longest consecutive run of `needle` in `text`.
#[inline]
pub(crate) fn max_consecutive_char(text: &str, needle: char) -> usize {
    let mut max = 0usize;
    let mut current = 0usize;
    for c in text.chars() {
        if c == needle {
            current += 1;
            if current > max {
                max = current;
            }
        } else {
            current = 0;
        }
    }
    max
}

/// Returns the value of an attribute on an element.
#[inline]
#[must_use]
pub(crate) fn attr<'a>(element: &'a ElementRef<'_>, name: &str) -> Option<&'a str> {
    element.value().attr(name)
}

/// Returns `true` if the given element has an ancestor with the specified tag
/// name.
#[must_use]
pub(crate) fn has_ancestor(element: &ElementRef<'_>, target_tag: &str) -> bool {
    let mut current = element.parent();
    while let Some(parent) = current {
        if let Some(el) = parent.value().as_element()
            && el.name() == target_tag
        {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// Returns `true` if the given element's immediate parent has the specified tag
/// name.
#[must_use]
pub(crate) fn parent_tag_is(element: &ElementRef<'_>, target_tag: &str) -> bool {
    element
        .parent()
        .and_then(|p| p.value().as_element())
        .is_some_and(|el| el.name() == target_tag)
}

/// Collects all text content from a DOM subtree recursively.
#[must_use]
pub(crate) fn collect_text(node: &NodeRef<'_, Node>) -> String {
    let mut buf = String::new();
    collect_text_inner(node, &mut buf);
    buf
}

/// Inner recursive text collector.
fn collect_text_inner(node: &NodeRef<'_, Node>, buf: &mut String) {
    match node.value() {
        Node::Text(t) => buf.push_str(t),
        _ => {
            for child in node.children() {
                collect_text_inner(&child, buf);
            }
        }
    }
}

/// Returns `true` if the given element has an ancestor matching any of the
/// specified tag names.
#[must_use]
pub(crate) fn has_ancestor_any(element: &ElementRef<'_>, tags: &[&str]) -> bool {
    let mut current = element.parent();
    while let Some(parent) = current {
        if let Some(el) = parent.value().as_element()
            && tags.contains(&el.name())
        {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// Adds a leading/trailing space around `markdown` if the neighbouring DOM
/// text would otherwise run into the delimiter without whitespace.
///
/// This mirrors the Go `AddSpaceIfNecessary` function.
#[must_use]
pub(crate) fn add_space_if_necessary(element: &ElementRef<'_>, markdown: String) -> String {
    let node = element.id();
    let tree = element.tree();
    let Some(node_ref) = tree.get(node) else {
        return markdown;
    };

    let mut result = markdown;

    // Check previous sibling text.
    if let Some(prev) = node_ref.prev_sibling() {
        let text = collect_text(&prev);
        if let Some(last_char) = text.chars().next_back()
            && !last_char.is_whitespace()
        {
            result.insert(0, ' ');
        }
    }

    // Check next sibling text.
    if let Some(next) = node_ref.next_sibling() {
        let text = collect_text(&next);
        if let Some(first_char) = text.chars().next()
            && !first_char.is_whitespace()
            && !first_char.is_ascii_punctuation()
        {
            result.push(' ');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_consecutive_empty() {
        assert_eq!(max_consecutive_char("", '`'), 0);
    }

    #[test]
    fn max_consecutive_no_match() {
        assert_eq!(max_consecutive_char("hello world", '`'), 0);
    }

    #[test]
    fn max_consecutive_single() {
        assert_eq!(max_consecutive_char("a`b", '`'), 1);
    }

    #[test]
    fn max_consecutive_multiple_runs_picks_longest() {
        assert_eq!(max_consecutive_char("``a```b`", '`'), 3);
    }

    #[test]
    fn max_consecutive_entire_string() {
        assert_eq!(max_consecutive_char("~~~~", '~'), 4);
    }

    #[test]
    fn max_consecutive_at_boundaries() {
        assert_eq!(max_consecutive_char("``hello``", '`'), 2);
    }

    #[test]
    fn block_tag_matches_known_tags() {
        assert!(is_block_tag("p"));
        assert!(is_block_tag("tr"));
        assert!(is_block_tag("h3"));
    }

    #[test]
    fn block_tag_rejects_inline_tags() {
        assert!(!is_block_tag("a"));
        assert!(!is_block_tag("span"));
        assert!(!is_block_tag("em"));
    }
}
