//! Table fallback (`<tr>`, `<td>`, `<th>`) conversion rules.
//!
//! `CommonMark` has no table syntax, so each `<tr>` degrades to one line of
//! plain text with cells joined by ` | `. The GFM plugin's pipe-table rules
//! take precedence when it is registered.

use scraper::ElementRef;

use crate::context::Context;
use crate::converter::{Action, Rule};

/// Handles `<tr>` elements — one output line per row.
#[derive(Debug, Clone, Copy)]
pub(super) struct TableRow;

impl Rule for TableRow {
    fn tags(&self) -> &'static [&'static str] {
        &["tr"]
    }

    fn apply(&self, content: &str, _element: &ElementRef<'_>, _ctx: &mut Context<'_>) -> Action {
        // Cells append a trailing ` | `; strip the surplus separators (and
        // any pipe from an empty first cell) so rows read as `a | b`.
        let trimmed = content
            .trim()
            .trim_end_matches('|')
            .trim_end()
            .trim_start_matches('|')
            .trim_start();
        if trimmed.is_empty() {
            return Action::Skip;
        }
        Action::Replace(format!("{trimmed}\n"))
    }
}

/// Handles `<td>` and `<th>` elements — cell text joined with ` | `.
#[derive(Debug, Clone, Copy)]
pub(super) struct TableCell;

impl Rule for TableCell {
    fn tags(&self) -> &'static [&'static str] {
        &["td", "th"]
    }

    fn apply(&self, content: &str, _element: &ElementRef<'_>, _ctx: &mut Context<'_>) -> Action {
        // Force single-line cell text: block children (e.g. paragraphs)
        // would otherwise break the one-line-per-row layout.
        let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
        Action::Replace(format!("{collapsed} | "))
    }
}
