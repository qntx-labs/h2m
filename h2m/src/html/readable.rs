//! Smart readable content extraction and noise stripping.

use std::collections::HashSet;
use std::fmt::Write;

use scraper::Html;

use super::select::collect_inner_html;

/// Attempts to detect and extract the main content area of an HTML document.
///
/// Tries common selectors (`article`, `main`, `[role="main"]`, etc.) in
/// priority order and returns the inner HTML of the first non-empty match.
/// Returns `None` if no main content area is detected.
///
/// # Examples
///
/// ```
/// let html = "<nav>menu</nav><article><p>Hello</p></article>";
/// assert_eq!(h2m::html::detect_main_content(html).as_deref(), Some("<p>Hello</p>"));
/// ```
#[must_use]
pub fn detect_main_content(html: &str) -> Option<String> {
    detect_main_content_doc(&Html::parse_document(html))
}

/// Extracts readable content from an HTML document.
///
/// Two-phase approach:
/// 1. Tries semantic selectors (`article`, `main`, `[role="main"]`, …).
/// 2. If none match, strips noise elements (`nav`, `footer`, `aside`, …)
///    and returns the cleaned HTML.
///
/// # Examples
///
/// ```
/// let html = "<nav>menu</nav><div><p>Hello world</p></div><footer>ft</footer>";
/// let result = h2m::html::readable_content(html);
/// assert!(!result.contains("menu"));
/// assert!(!result.contains("ft"));
/// assert!(result.contains("Hello world"));
/// ```
#[must_use]
pub fn readable_content(html: &str) -> String {
    let doc = Html::parse_document(html);
    readable_content_doc(&doc, html)
}

/// Strips noise elements (`nav`, `footer`, `aside`, …) from an HTML document.
///
/// Returns the cleaned HTML with navigational chrome removed.
///
/// # Examples
///
/// ```
/// let html = "<nav>menu</nav><p>content</p><footer>ft</footer>";
/// let result = h2m::html::strip_noise(html);
/// assert!(!result.contains("menu"));
/// assert!(!result.contains("ft"));
/// assert!(result.contains("content"));
/// ```
#[must_use]
pub fn strip_noise(html: &str) -> String {
    let doc = Html::parse_document(html);
    strip_noise_doc(&doc, html)
}

/// CSS selectors tried in priority order for automatic main-content detection.
const MAIN_CONTENT_SELECTORS: &[&str] = &[
    "article",
    "[role=\"main\"]",
    "main",
    "#main",
    ".post-content",
    ".entry-content",
    "#content",
    ".content",
];

/// Elements that are almost always navigational/chrome noise, not content.
const NOISE_SELECTORS: &[&str] = &[
    "nav",
    "footer",
    "aside",
    "body > header",
    "[role=\"navigation\"]",
    "[role=\"banner\"]",
    "[role=\"contentinfo\"]",
    "[role=\"complementary\"]",
    "[role=\"search\"]",
    "[aria-hidden=\"true\"]",
];

/// Auto-detects main content from a pre-parsed document.
fn detect_main_content_doc(doc: &Html) -> Option<String> {
    MAIN_CONTENT_SELECTORS
        .iter()
        .find_map(|sel| collect_inner_html(doc, sel))
}

/// Extracts readable content from a pre-parsed document.
///
/// Two-phase approach:
/// 1. Try semantic selectors (`article`, `main`, `[role="main"]`, …).
/// 2. If none match, strip noise elements (`nav`, `footer`, `aside`, …)
///    and return the cleaned body HTML.
pub(crate) fn readable_content_doc(doc: &Html, original: &str) -> String {
    if let Some(main) = detect_main_content_doc(doc) {
        return main;
    }
    strip_noise_doc(doc, original)
}

/// Strips noise elements from a pre-parsed document, returning cleaned HTML.
///
/// Walks the DOM tree and re-serializes it while skipping subtrees that match
/// [`NOISE_SELECTORS`]. Returns `original` unchanged if no noise is found.
fn strip_noise_doc(doc: &Html, original: &str) -> String {
    let noise_ids = collect_noise_ids(doc);
    if noise_ids.is_empty() {
        return original.to_owned();
    }
    let mut buf = String::with_capacity(original.len());
    render_children(doc.tree.root(), &noise_ids, &mut buf);
    buf
}

/// Collects `ego_tree::NodeId`s of all elements matching [`NOISE_SELECTORS`].
fn collect_noise_ids(doc: &Html) -> HashSet<ego_tree::NodeId> {
    let mut ids = HashSet::new();
    for &sel_str in NOISE_SELECTORS {
        if let Ok(sel) = scraper::Selector::parse(sel_str) {
            for el in doc.select(&sel) {
                ids.insert(el.id());
            }
        }
    }
    ids
}

/// Recursively serializes DOM children, skipping subtrees in `skip`.
fn render_children(
    node: ego_tree::NodeRef<'_, scraper::Node>,
    skip: &HashSet<ego_tree::NodeId>,
    buf: &mut String,
) {
    for child in node.children() {
        if skip.contains(&child.id()) {
            continue;
        }
        match child.value() {
            scraper::Node::Text(text) => buf.push_str(text),
            scraper::Node::Element(el) => {
                let tag = el.name();
                _ = write!(buf, "<{tag}");
                for (name, val) in el.attrs() {
                    _ = write!(buf, " {name}=\"");
                    escape_attr_value(val, buf);
                    buf.push('"');
                }
                buf.push('>');
                if !is_void_element(tag) {
                    render_children(child, skip, buf);
                    _ = write!(buf, "</{tag}>");
                }
            }
            scraper::Node::Document | scraper::Node::Fragment => {
                render_children(child, skip, buf);
            }
            _ => {}
        }
    }
}

/// Escapes HTML attribute value characters in a single pass, writing directly
/// into `buf` without intermediate allocations.
fn escape_attr_value(val: &str, buf: &mut String) {
    for c in val.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '"' => buf.push_str("&quot;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            _ => buf.push(c),
        }
    }
}

/// HTML void elements that have no closing tag.
fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}
