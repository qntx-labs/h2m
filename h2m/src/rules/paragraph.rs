//! Paragraph and generic block container rules.

use scraper::ElementRef;

use crate::context::Context;
use crate::converter::{Action, Rule};

/// Handles `<p>`, `<div>`, `<section>`, `<article>`, `<main>`, `<header>`,
/// `<footer>`, `<nav>`, `<dl>`, and `<table>` elements.
#[derive(Debug, Clone, Copy)]
pub(super) struct Paragraph;

impl Rule for Paragraph {
    fn tags(&self) -> &'static [&'static str] {
        &[
            "p", "div", "section", "article", "main", "header", "footer", "nav", "dl", "table",
        ]
    }

    fn apply(&self, content: &str, _element: &ElementRef<'_>, _ctx: &mut Context<'_>) -> Action {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Action::Skip;
        }
        Action::Replace(format!("\n\n{trimmed}\n\n"))
    }
}
