//! Built-in `CommonMark` conversion rules.

mod blockquote;
mod code_block;
mod definition_list;
mod emphasis;
mod heading;
mod horizontal_rule;
mod iframe;
mod image;
mod inline_code;
mod line_break;
mod link;
mod list;
mod paragraph;
mod strong;
mod table;

use crate::converter::{ConverterBuilder, Plugin};

/// Wraps each non-empty line with `delimiter` so that bold/italic spanning
/// multiple lines is rendered correctly.
///
/// Shared by [`strong::Strong`] and [`emphasis::Emphasis`].
pub(crate) fn wrap_delimiter_per_line(text: &str, delimiter: &str) -> String {
    if !text.contains('\n') {
        return format!("{delimiter}{text}{delimiter}");
    }

    let mut result = String::with_capacity(text.len() + delimiter.len() * 4);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            result.push('\n');
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        result.push_str(delimiter);
        result.push_str(trimmed);
        result.push_str(delimiter);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_single_line() {
        assert_eq!(wrap_delimiter_per_line("bold", "**"), "**bold**");
    }

    #[test]
    fn wrap_multiline() {
        assert_eq!(
            wrap_delimiter_per_line("line1\nline2", "**"),
            "**line1**\n**line2**"
        );
    }

    #[test]
    fn wrap_multiline_preserves_blank_line() {
        assert_eq!(
            wrap_delimiter_per_line("line1\n\nline2", "**"),
            "**line1**\n\n**line2**"
        );
    }

    #[test]
    fn wrap_single_char_delimiter() {
        assert_eq!(wrap_delimiter_per_line("text", "*"), "*text*");
    }
}

/// The `CommonMark` plugin — registers all built-in rules for standard HTML
/// tags.
///
/// This plugin handles headings, paragraphs, emphasis, strong, inline code,
/// code blocks, links, images, lists, blockquotes, horizontal rules, line
/// breaks, definition lists, and a plain-text table fallback.
#[derive(Debug, Clone, Copy)]
pub struct CommonMark;

impl Plugin for CommonMark {
    fn register(&self, builder: &mut ConverterBuilder) {
        builder.add_rule(paragraph::Paragraph);
        builder.add_rule(heading::Heading);
        builder.add_rule(code_block::CodeBlock);
        builder.add_rule(blockquote::Blockquote);
        builder.add_rule(list::List);
        builder.add_rule(list::ListItem);
        builder.add_rule(definition_list::DefinitionTerm);
        builder.add_rule(definition_list::DefinitionDescription);
        builder.add_rule(table::TableRow);
        builder.add_rule(table::TableCell);
        builder.add_rule(horizontal_rule::HorizontalRule);
        builder.add_rule(line_break::LineBreak);
        builder.add_rule(strong::Strong);
        builder.add_rule(emphasis::Emphasis);
        builder.add_rule(inline_code::InlineCode);
        builder.add_rule(link::Link);
        builder.add_rule(image::Image);
        builder.add_rule(iframe::Iframe);
    }
}
