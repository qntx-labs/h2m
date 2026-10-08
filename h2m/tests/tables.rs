#![cfg(test)]
//! `CommonMark` table fallback conversion tests.

use ego_tree as _;
#[cfg(feature = "scrape")]
use reqwest as _;
use scraper as _;
#[cfg(feature = "scrape")]
use serde as _;
use thiserror as _;
#[cfg(feature = "scrape")]
use tokio as _;
use url as _;

use h2m::convert;
use pretty_assertions::assert_eq;

#[test]
fn table_fallback_rows_on_separate_lines() {
    assert_eq!(
        convert("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"),
        "a | b\nc | d"
    );
}

#[test]
fn table_fallback_with_head_and_body() {
    assert_eq!(
        convert(
            "<table><thead><tr><th>Name</th><th>Age</th></tr></thead>\
             <tbody><tr><td>Alice</td><td>30</td></tr></tbody></table>"
        ),
        "Name | Age\nAlice | 30"
    );
}

#[test]
fn table_fallback_whitespace_between_tags() {
    assert_eq!(
        convert("<table>\n  <tr>\n    <td>a</td>\n    <td>b</td>\n  </tr>\n</table>"),
        "a | b"
    );
}

#[test]
fn table_fallback_inline_markup_in_cells() {
    assert_eq!(
        convert(
            r#"<table><tr><td><a href="https://example.com">link</a></td><td><b>bold</b></td></tr></table>"#
        ),
        "[link](https://example.com) | **bold**"
    );
}

#[test]
fn table_fallback_empty_table_skipped() {
    assert_eq!(convert("<table></table>"), "");
}

#[test]
fn table_fallback_empty_row_skipped() {
    assert_eq!(convert("<table><tr><td>a</td></tr><tr></tr></table>"), "a");
}

#[test]
fn table_fallback_leading_empty_cell_drops_pipe() {
    assert_eq!(
        convert("<table><tr><th></th><th><h3>Posts in 2026</h3></th></tr></table>"),
        "### Posts in 2026"
    );
}

#[test]
fn table_fallback_layout_blog_like() {
    let html = r#"<table>
        <tr><td>Sept. 3</td><td><a href="/2026/09/03/Rust-1.98.1/">Announcing Rust 1.98.1</a></td></tr>
        <tr><td>Sept. 1</td><td><a href="/2026/09/01/Rustup-1.29.1/">Announcing rustup 1.29.1</a></td></tr>
    </table>"#;
    let md = convert(html);
    let lines: Vec<&str> = md.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines.first().is_some_and(|l| l.contains("Sept. 3")));
    assert!(
        lines
            .first()
            .is_some_and(|l| l.contains("[Announcing Rust 1.98.1](/2026/09/03/Rust-1.98.1/)"))
    );
    assert!(lines.get(1).is_some_and(|l| l.contains("Sept. 1")));
    assert!(
        lines
            .get(1)
            .is_some_and(|l| l.contains("[Announcing rustup 1.29.1]"))
    );
}

#[test]
fn table_fallback_block_children_collapsed_to_single_line() {
    assert_eq!(
        convert("<table><tr><td><p>line1</p><p>line2</p></td><td>x</td></tr></table>"),
        "line1 line2 | x"
    );
}

#[test]
fn table_fallback_gfm_plugin_overrides_with_pipe_table() {
    use h2m::convert_gfm;
    let md = convert_gfm(
        "<table><tr><th>Name</th><th>Age</th></tr><tr><td>Alice</td><td>30</td></tr></table>",
    );
    let lines: Vec<&str> = md.lines().collect();
    assert!(lines.get(1).is_some_and(|l| l.contains("---")));
    assert!(md.contains("Alice"));
}

#[test]
fn table_fallback_intermediate_empty_cell_preserved() {
    assert_eq!(
        convert("<table><tr><td>a</td><td></td><td>b</td></tr></table>"),
        "a |  | b"
    );
}
