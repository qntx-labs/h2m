//! Definition list (`<dt>`, `<dd>`) conversion rules.
//!
//! Emits the PHP Markdown Extra / Pandoc `term` + `:   definition` syntax;
//! `<dl>` itself is handled as a generic block container by
//! [`Paragraph`](super::paragraph::Paragraph).

use scraper::ElementRef;

use crate::context::Context;
use crate::converter::{Action, Rule};

/// Handles `<dt>` elements — the term being defined.
#[derive(Debug, Clone, Copy)]
pub(super) struct DefinitionTerm;

impl Rule for DefinitionTerm {
    fn tags(&self) -> &'static [&'static str] {
        &["dt"]
    }

    fn apply(&self, content: &str, _element: &ElementRef<'_>, _ctx: &mut Context<'_>) -> Action {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Action::Skip;
        }
        Action::Replace(format!("\n\n{trimmed}\n"))
    }
}

/// Handles `<dd>` elements — the description of the preceding `<dt>`.
#[derive(Debug, Clone, Copy)]
pub(super) struct DefinitionDescription;

impl Rule for DefinitionDescription {
    fn tags(&self) -> &'static [&'static str] {
        &["dd"]
    }

    fn apply(&self, content: &str, _element: &ElementRef<'_>, _ctx: &mut Context<'_>) -> Action {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Action::Skip;
        }

        // First line uses `:   ` (marker + 3 spaces); continuation lines
        // are indented by 4 spaces to align under the definition text.
        let mut result = String::with_capacity(trimmed.len() + trimmed.lines().count() * 4 + 2);
        for (i, line) in trimmed.lines().enumerate() {
            if i > 0 {
                result.push('\n');
            }
            if line.is_empty() {
                continue;
            }
            if i == 0 {
                result.push_str(":   ");
            } else {
                result.push_str("    ");
            }
            result.push_str(line);
        }
        result.push('\n');

        Action::Replace(result)
    }
}
