//! HTML inspection utilities for extracting metadata from parsed documents.
//!
//! These functions operate on raw HTML strings and are always available
//! (no feature flags required).
//!
//! The module is organized into three sub-modules:
//!
//! - **extract** — metadata extraction (title, links, language, description)
//! - **select** — CSS selector application
//! - **readable** — smart content extraction and noise stripping

mod extract;
mod readable;
pub(crate) mod select;

pub(crate) use extract::extract_links_doc;
pub use extract::{PageMeta, extract_links, extract_links_with_base, extract_title};
pub(crate) use readable::readable_content_doc;
pub use readable::{detect_main_content, readable_content, strip_noise};
pub use select::select;
pub(crate) use select::select_doc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_basic() {
        let html = "<html><head><title>  Test Page  </title></head><body></body></html>";
        assert_eq!(extract_title(html).as_deref(), Some("Test Page"));
    }

    #[test]
    fn title_missing() {
        assert_eq!(extract_title("<html><body>no title</body></html>"), None);
    }

    #[test]
    fn title_empty() {
        assert_eq!(
            extract_title("<html><head><title>  </title></head></html>"),
            None
        );
    }

    #[test]
    fn links_basic() {
        let html = r##"<a href="/a">A</a><a href="https://b.com">B</a><a href="#c">C</a>"##;
        let links = extract_links(html);
        assert_eq!(links, vec!["/a", "https://b.com"]);
    }

    #[test]
    fn links_empty_href_excluded() {
        assert_eq!(
            extract_links(r#"<a href="">empty</a>"#),
            Vec::<String>::new()
        );
    }

    #[test]
    fn select_basic() {
        let html = r"<article>content</article><aside>nav</aside>";
        assert_eq!(select(html, "article"), "content");
    }

    #[test]
    fn select_no_match_returns_original() {
        let html = "<p>hello</p>";
        assert_eq!(select(html, ".missing"), html);
    }

    #[test]
    fn select_invalid_selector_returns_original() {
        let html = "<p>hello</p>";
        assert_eq!(select(html, ":::invalid"), html);
    }

    #[test]
    fn detect_main_content_article() {
        let html = "<nav>nav</nav><article><p>main</p></article>";
        assert_eq!(detect_main_content(html).as_deref(), Some("<p>main</p>"));
    }

    #[test]
    fn detect_main_content_none() {
        assert!(detect_main_content("<div>plain</div>").is_none());
    }
}
