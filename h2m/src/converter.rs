//! Core converter: traits, builder, frozen converter, and traversal pipeline.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ego_tree::NodeId;
use scraper::node::Node;
use scraper::{ElementRef, Html};

use crate::context::Context;
use crate::dom;
use crate::escape;
use crate::options::Options;
use crate::whitespace;

/// The action a rule returns to control how an element is converted.
#[derive(Debug)]
#[non_exhaustive]
pub enum Action {
    /// Replace the element with the given markdown string.
    Replace(String),
    /// Skip this rule and try the next registered rule for this tag.
    Skip,
    /// Remove the element and all its content from the output.
    Remove,
}

/// A conversion rule that handles one or more HTML tag types.
///
/// Rules are registered with the converter and dispatched by tag name.
/// Multiple rules can be registered for the same tag; they are tried in
/// reverse registration order (last-registered first). The first rule that
/// returns [`Action::Replace`] wins.
pub trait Rule: Send + Sync {
    /// Returns the HTML tag names this rule handles.
    fn tags(&self) -> &'static [&'static str];

    /// Applies this rule to an element.
    ///
    /// # Arguments
    ///
    /// * `content` - The already-converted markdown content of the element's
    ///   children.
    /// * `element` - The HTML element being converted.
    /// * `ctx` - The current conversion context with options and mutable state
    ///   (e.g. for accumulating reference-style link definitions).
    fn apply(&self, content: &str, element: &ElementRef<'_>, ctx: &mut Context<'_>) -> Action;
}

/// A plugin that registers rules and hooks with a converter.
///
/// Plugins provide a composable way to extend the converter with additional
/// tag handlers. For example, the GFM plugin bundles table, strikethrough,
/// and task list support.
///
/// # Examples
///
/// ```
/// use h2m::Converter;
/// use h2m::plugins::Gfm;
/// use h2m::rules::CommonMark;
///
/// let converter = Converter::builder()
///     .use_plugin(&CommonMark)
///     .use_plugin(&Gfm)
///     .build();
///
/// let md = converter.convert("<table><tr><th>A</th></tr></table>");
/// assert!(md.contains("| A"));
/// ```
pub trait Plugin {
    /// Registers this plugin's rules and hooks with the given builder.
    fn register(&self, builder: &mut ConverterBuilder);
}

/// Builder for constructing a [`Converter`] with custom rules and options.
#[derive(Default)]
pub struct ConverterBuilder {
    /// Conversion options.
    options: Options,
    /// Registered rules, keyed by tag name.
    rules: HashMap<&'static str, Vec<Arc<dyn Rule>>>,
    /// Tags whose raw HTML should be preserved in the output.
    keep_tags: HashSet<String>,
    /// Tags (and their content) to remove entirely from the output.
    remove_tags: HashSet<String>,
    /// Base domain for resolving relative URLs.
    domain: Option<String>,
}

impl std::fmt::Debug for ConverterBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConverterBuilder")
            .field("options", &self.options)
            .field(
                "rule_count",
                &self.rules.values().map(Vec::len).sum::<usize>(),
            )
            .field("keep_tags", &self.keep_tags)
            .field("remove_tags", &self.remove_tags)
            .field("domain", &self.domain)
            .finish()
    }
}

impl ConverterBuilder {
    /// Creates a new builder with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the conversion options.
    #[must_use]
    pub const fn options(mut self, opts: Options) -> Self {
        self.options = opts;
        self
    }

    /// Registers a conversion rule.
    ///
    /// Rules registered later take priority over earlier rules for the same
    /// tag.
    pub fn add_rule(&mut self, rule: impl Rule + 'static) {
        let arc: Arc<dyn Rule> = Arc::new(rule);
        for &tag in arc.tags() {
            self.rules.entry(tag).or_default().push(Arc::clone(&arc));
        }
    }

    /// Applies a plugin, which may register rules and hooks.
    #[must_use]
    pub fn use_plugin(mut self, plugin: &impl Plugin) -> Self {
        plugin.register(&mut self);
        self
    }

    /// Adds tags whose raw HTML should be kept in the output.
    #[must_use]
    pub fn keep(mut self, tags: &[&str]) -> Self {
        self.keep_tags.extend(tags.iter().map(|&s| s.to_owned()));
        self
    }

    /// Adds tags (and their content) to remove from the output.
    #[must_use]
    pub fn remove(mut self, tags: &[&str]) -> Self {
        self.remove_tags.extend(tags.iter().map(|&s| s.to_owned()));
        self
    }

    /// Sets the base domain for resolving relative URLs to absolute.
    ///
    /// For example, setting `"example.com"` will turn `/page.html` into
    /// `https://example.com/page.html`.
    #[must_use]
    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Builds the frozen [`Converter`].
    #[must_use]
    pub fn build(self) -> Converter {
        Converter {
            options: self.options,
            rules: self.rules,
            keep_tags: self.keep_tags,
            remove_tags: self.remove_tags,
            domain: self.domain,
        }
    }
}

/// A frozen, reusable HTML-to-Markdown converter.
///
/// Construct via [`Converter::builder()`] or the convenience
/// [`crate::convert()`] function.
///
/// # Examples
///
/// ```
/// use h2m::{Converter, Options};
/// use h2m::rules::CommonMark;
/// use h2m::plugins::Gfm;
///
/// let converter = Converter::builder()
///     .options(Options::default())
///     .use_plugin(&CommonMark)
///     .use_plugin(&Gfm)
///     .domain("example.com")
///     .build();
///
/// let md = converter.convert("<p><a href=\"/about\">About</a></p>");
/// assert_eq!(md, "[About](https://example.com/about)");
/// ```
#[derive(Clone)]
pub struct Converter {
    /// Conversion options.
    options: Options,
    /// Rules keyed by tag name, tried in reverse order.
    rules: HashMap<&'static str, Vec<Arc<dyn Rule>>>,
    /// Tags to preserve as raw HTML.
    keep_tags: HashSet<String>,
    /// Tags to remove entirely.
    remove_tags: HashSet<String>,
    /// Base domain for resolving relative URLs.
    domain: Option<String>,
}

impl std::fmt::Debug for Converter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Converter")
            .field("options", &self.options)
            .field(
                "rule_count",
                &self.rules.values().map(Vec::len).sum::<usize>(),
            )
            .field("keep_tags", &self.keep_tags)
            .field("remove_tags", &self.remove_tags)
            .field("domain", &self.domain)
            .finish()
    }
}

// Compile-time assertion that `Converter` is `Send + Sync`.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Converter>();
};

impl Converter {
    /// Returns a new [`ConverterBuilder`].
    #[must_use]
    pub fn builder() -> ConverterBuilder {
        ConverterBuilder::new()
    }

    /// Converts an HTML string to Markdown.
    ///
    /// This operation is infallible: html5ever recovers from any malformed
    /// input, so parsing never fails.
    #[must_use]
    pub fn convert(&self, html: &str) -> String {
        let document = Html::parse_document(html);
        let mut ctx = Context::new(self.options, self.domain.as_deref());

        // Pre-pass: compute list metadata.
        ctx.annotate_lists(document.root_element().id(), &document);

        // Traverse from the root element (<html>).
        let root_id = document.root_element().id();
        let mut output = String::with_capacity(html.len());
        self.write_node(root_id, &document, &mut ctx, &mut output);

        // Append reference-style link definitions if any.
        if ctx.has_references() {
            output.push_str("\n\n");
            output.push_str(&ctx.take_references());
        }

        whitespace::clean_output(&output)
    }

    /// Converts HTML read from a reader to Markdown.
    ///
    /// # Errors
    ///
    /// Returns an error if reading from the reader fails.
    pub fn convert_reader(&self, mut reader: impl std::io::Read) -> crate::Result<String> {
        let mut html = String::new();
        reader.read_to_string(&mut html)?;
        Ok(self.convert(&html))
    }

    /// Writes a DOM node's markdown representation into `buf`.
    fn write_node(
        &self,
        node_id: NodeId,
        document: &Html,
        ctx: &mut Context<'_>,
        buf: &mut String,
    ) {
        let Some(node_ref) = document.tree.get(node_id) else {
            return;
        };

        match node_ref.value() {
            Node::Text(text) => Self::write_text(text, ctx, buf),
            Node::Element(_) => {
                if let Some(element_ref) = ElementRef::wrap(node_ref) {
                    self.write_element(&element_ref, document, ctx, buf);
                }
            }
            Node::Document => {
                for child in node_ref.children() {
                    self.write_node(child.id(), document, ctx, buf);
                }
            }
            _ => {}
        }
    }

    /// Writes a text node's content into `buf`.
    fn write_text(text: &scraper::node::Text, ctx: &Context<'_>, buf: &mut String) {
        let raw: &str = text;

        if ctx.in_pre() {
            buf.push_str(raw);
            return;
        }

        let collapsed = whitespace::collapse_whitespace(raw);
        let escaped = escape::escape_markdown(&collapsed, ctx.options().escape_mode());
        buf.push_str(&escaped);
    }

    /// Writes an element node by converting children first, then applying
    /// rules.
    fn write_element(
        &self,
        element: &ElementRef<'_>,
        document: &Html,
        ctx: &mut Context<'_>,
        buf: &mut String,
    ) {
        let tag = element.value().name();

        // Check if this tag should be removed entirely.
        if self.remove_tags.contains(tag) || matches!(tag, "script" | "style" | "noscript" | "head")
        {
            return;
        }

        // Track preformatted context — suppress whitespace collapse and
        // escaping inside `<pre>` and inline `<code>`/`<kbd>`/`<samp>`/`<tt>`.
        let was_in_pre = ctx.in_pre();
        if matches!(tag, "pre" | "code" | "kbd" | "samp" | "tt") {
            ctx.set_in_pre(true);
        }

        // Recursively convert children into a temporary buffer, since rules
        // need the complete child content to decide their output.
        let child_start = buf.len();
        let Some(node_ref) = document.tree.get(element.id()) else {
            return;
        };
        for child in node_ref.children() {
            // Drop whitespace-only text adjacent to block-level elements —
            // it carries no rendered content (mirrors browser rendering).
            if !ctx.in_pre() && Self::is_inter_block_whitespace(&child, tag) {
                continue;
            }
            self.write_node(child.id(), document, ctx, buf);
        }

        // Restore `<pre>` context.
        ctx.set_in_pre(was_in_pre);

        // Check if raw HTML should be kept.
        if self.keep_tags.contains(tag) {
            let kept = element.html();
            buf.truncate(child_start);
            buf.push_str(&kept);
            return;
        }

        // Dispatch to rules (LIFO — last registered wins).
        if let Some(rules) = self.rules.get(tag) {
            let content = buf[child_start..].to_owned();
            let action =
                rules
                    .iter()
                    .rev()
                    .find_map(|rule| match rule.apply(&content, element, ctx) {
                        Action::Skip => None,
                        other => Some(other),
                    });
            match action {
                Some(Action::Replace(md)) => {
                    buf.truncate(child_start);
                    buf.push_str(&md);
                }
                Some(Action::Remove) => {
                    buf.truncate(child_start);
                }
                _ => {}
            }
        }

        // No rule matched — children already written as transparent passthrough.
    }

    /// Returns `true` for whitespace-only text nodes adjacent to a
    /// block-level sibling, or padding at the start/end of a block parent.
    fn is_inter_block_whitespace(child: &ego_tree::NodeRef<'_, Node>, parent_tag: &str) -> bool {
        let Node::Text(text) = child.value() else {
            return false;
        };
        if !text.trim().is_empty() {
            return false;
        }

        let sibling_is_block = |sib: Option<ego_tree::NodeRef<'_, Node>>| {
            sib.is_some_and(|s| {
                s.value()
                    .as_element()
                    .is_some_and(|el| dom::is_block_tag(el.name()))
            })
        };

        if dom::is_block_tag(parent_tag)
            && (child.prev_sibling().is_none() || child.next_sibling().is_none())
        {
            return true;
        }

        sibling_is_block(child.prev_sibling()) || sibling_is_block(child.next_sibling())
    }
}
