//! Markdown to sanitized HTML rendering.

use std::fmt;
use std::sync::Mutex;

use ammonia::Builder as SanitizerBuilder;
use comrak::adapters::{HeadingAdapter, HeadingMeta};
use comrak::nodes::{AstNode, NodeValue};
use comrak::options::Plugins;
use comrak::plugins::syntect::SyntectAdapter;
use comrak::{Arena, Options, markdown_to_html_with_plugins, parse_document};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::headings::{Heading, HeadingIdGenerator, prefixed_heading_id};

/// Result type returned by content rendering and docs build helpers.
pub type ContentResult<T> = Result<T, ContentError>;

/// Errors produced while rendering content or writing documentation artifacts.
#[derive(Debug, Error)]
pub enum ContentError {
    /// Filesystem read or write failed.
    #[error("content filesystem error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization failed.
    #[error("content serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Options controlling Markdown rendering.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MarkdownOptions {
    /// Render raw HTML without sanitizing it.
    pub unsafe_html: bool,
    /// Prefix applied to generated heading anchor IDs.
    pub heading_anchor_prefix: String,
    /// Enable Comrak's math extensions and stable `language-math` code markers.
    pub render_math: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            unsafe_html: false,
            heading_anchor_prefix: String::new(),
            render_math: true,
        }
    }
}

/// Rendered Markdown plus metadata extracted from the parsed document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RenderedMarkdown {
    /// Sanitized HTML ready to send to the browser.
    pub html: String,
    /// Plain text extracted from the Markdown AST.
    pub plain_text: String,
    /// Short preview text derived from `plain_text`.
    pub excerpt: String,
    /// Ordered heading metadata for table-of-contents UIs.
    pub headings: Vec<Heading>,
}

/// Shared Markdown renderer for application pages, docs, and articles.
#[derive(Clone, Debug)]
pub struct MarkdownRenderer {
    options: MarkdownOptions,
}

impl MarkdownRenderer {
    /// Create a renderer from explicit options.
    pub fn new(options: MarkdownOptions) -> Self {
        Self { options }
    }

    /// Return the options used by this renderer.
    pub fn options(&self) -> &MarkdownOptions {
        &self.options
    }

    /// Render a Markdown document to HTML and extracted metadata.
    pub fn render(&self, markdown: &str) -> ContentResult<RenderedMarkdown> {
        let options = comrak_options(&self.options);
        let arena = Arena::new();
        let root = parse_document(&arena, markdown, &options);
        let (headings, plain_text) = extract_metadata(root, &self.options.heading_anchor_prefix);
        let excerpt = excerpt_from_plain_text(&plain_text);

        let syntax_highlighter = SyntectAdapter::new(None);
        let heading_adapter = AnchorHeadingAdapter::new(self.options.heading_anchor_prefix.clone());
        let mut plugins = Plugins::default();
        plugins.render.codefence_syntax_highlighter = Some(&syntax_highlighter);
        plugins.render.heading_adapter = Some(&heading_adapter);

        let rendered = markdown_to_html_with_plugins(markdown, &options, &plugins);
        let html = if self.options.unsafe_html {
            rendered
        } else {
            sanitize_html(&rendered)
        };

        Ok(RenderedMarkdown {
            html,
            plain_text,
            excerpt,
            headings,
        })
    }
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new(MarkdownOptions::default())
    }
}

fn comrak_options(render_options: &MarkdownOptions) -> Options<'static> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.front_matter_delimiter = Some("---".to_owned());
    options.extension.tagfilter = !render_options.unsafe_html;
    options.extension.math_code = render_options.render_math;
    options.extension.math_dollars = render_options.render_math;
    options.render.r#unsafe = render_options.unsafe_html;
    options
}

fn sanitize_html(html: &str) -> String {
    let mut sanitizer = SanitizerBuilder::default();
    sanitizer.add_generic_attributes(&[
        "aria-hidden",
        "aria-label",
        "checked",
        "class",
        "data-footnote-backref",
        "data-footnote-backref-idx",
        "data-footnote-ref",
        "data-footnotes",
        "data-math-style",
        "disabled",
        "id",
        "type",
    ]);
    sanitizer.clean(html).to_string()
}

fn extract_metadata<'a>(root: &'a AstNode<'a>, prefix: &str) -> (Vec<Heading>, String) {
    let mut headings = Vec::new();
    let mut heading_ids = HeadingIdGenerator::new();
    let mut plain_text = String::new();
    walk_metadata(
        root,
        prefix,
        &mut heading_ids,
        &mut headings,
        &mut plain_text,
    );
    (headings, normalize_whitespace(&plain_text))
}

fn walk_metadata<'a>(
    node: &'a AstNode<'a>,
    prefix: &str,
    heading_ids: &mut HeadingIdGenerator,
    headings: &mut Vec<Heading>,
    plain_text: &mut String,
) {
    {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Heading(heading) => {
                let title = normalize_whitespace(&collect_visible_text(node));
                let id = prefixed_heading_id(prefix, heading_ids, &title);
                headings.push(Heading {
                    level: heading.level,
                    id,
                    title,
                });
            }
            NodeValue::Text(text) => append_plain_text(plain_text, text),
            NodeValue::Code(code) => append_plain_text(plain_text, &code.literal),
            NodeValue::CodeBlock(code) => append_plain_text(plain_text, &code.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => append_space(plain_text),
            _ => {}
        }
    }

    for child in node.children() {
        walk_metadata(child, prefix, heading_ids, headings, plain_text);
    }
}

fn collect_visible_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    collect_visible_text_into(node, &mut out);
    out
}

fn collect_visible_text_into<'a>(node: &'a AstNode<'a>, out: &mut String) {
    {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Text(text) => append_plain_text(out, text),
            NodeValue::Code(code) => append_plain_text(out, &code.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => append_space(out),
            _ => {}
        }
    }

    for child in node.children() {
        collect_visible_text_into(child, out);
    }
}

fn append_plain_text(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }

    // A SoftBreak/LineBreak before a node whose text opens with closing
    // punctuation (`.`, `,`, etc.) leaves a single break-introduced space
    // sitting in front of that punctuation — e.g. `Hello\n. World` walks to
    // `"Hello . World"`. Drop only that spurious break space; intentional
    // spaced punctuation inside a single Text run (French `Bonjour : ...`)
    // is left untouched because it never crosses a break boundary.
    if starts_with_closing_punctuation(text) && out.ends_with(' ') {
        out.pop();
    }

    if should_insert_space(out, text) {
        out.push(' ');
    }
    out.push_str(text);
}

/// Closing punctuation that should hug the preceding word rather than be
/// pushed onto a new line by a Markdown SoftBreak/LineBreak.
fn starts_with_closing_punctuation(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|ch| matches!(ch, '.' | ',' | ':' | ';' | '!' | '?' | ')' | ']'))
}

fn should_insert_space(out: &str, text: &str) -> bool {
    if out.is_empty() || out.ends_with(char::is_whitespace) {
        return false;
    }

    !starts_with_closing_punctuation(text)
}

fn append_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn excerpt_from_plain_text(plain_text: &str) -> String {
    const MAX_EXCERPT_CHARS: usize = 240;

    if plain_text.chars().count() <= MAX_EXCERPT_CHARS {
        return plain_text.to_owned();
    }

    let mut excerpt = plain_text
        .chars()
        .take(MAX_EXCERPT_CHARS)
        .collect::<String>()
        .trim_end()
        .to_owned();
    excerpt.push_str("...");
    excerpt
}

struct AnchorHeadingAdapter {
    prefix: String,
    heading_ids: Mutex<HeadingIdGenerator>,
}

impl AnchorHeadingAdapter {
    fn new(prefix: String) -> Self {
        Self {
            prefix,
            heading_ids: Mutex::new(HeadingIdGenerator::new()),
        }
    }
}

impl HeadingAdapter for AnchorHeadingAdapter {
    fn enter(
        &self,
        output: &mut dyn fmt::Write,
        heading: &HeadingMeta,
        _sourcepos: Option<comrak::nodes::Sourcepos>,
    ) -> fmt::Result {
        let mut heading_ids = self.heading_ids.lock().map_err(|_| fmt::Error)?;
        let id = prefixed_heading_id(&self.prefix, &mut heading_ids, &heading.content);
        write!(
            output,
            "<h{} id=\"{}\">",
            heading.level,
            escape_attribute(&id)
        )
    }

    fn exit(&self, output: &mut dyn fmt::Write, heading: &HeadingMeta) -> fmt::Result {
        write!(output, "</h{}>", heading.level)
    }
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn break_before_punctuation_drops_only_the_spurious_space() {
        // Simulate the AST walk for `Hello\n. World`: Text, SoftBreak, Text.
        // The break introduces a space that would otherwise sit in front of
        // the leading `.`.
        let mut out = String::new();
        append_plain_text(&mut out, "Hello");
        append_space(&mut out); // SoftBreak / LineBreak
        append_plain_text(&mut out, ". World");
        assert_eq!(out, "Hello. World");
    }

    #[test]
    fn intentional_spaced_punctuation_in_a_single_run_is_preserved() {
        // French typography keeps a space before `:` and `?`. When the text
        // arrives as a single run (no break boundary), the spacing must
        // survive — the old global ` :` -> `:` replace corrupted it.
        let mut out = String::new();
        append_plain_text(&mut out, "Bonjour : comment ça va ?");
        assert_eq!(out, "Bonjour : comment ça va ?");
        // normalize_whitespace only collapses runs; it must not touch the
        // intentional single spaces around the punctuation.
        assert_eq!(
            normalize_whitespace(&out),
            "Bonjour : comment ça va ?",
            "spaced punctuation in body text must not be glued to its word"
        );
    }

    #[test]
    fn spaced_punctuation_survives_full_render_pipeline() {
        // End-to-end through the renderer: intentional spaced punctuation in
        // the body must reach plain_text and excerpt untouched.
        let out = MarkdownRenderer::default()
            .render("Bonjour : comment ça va ?")
            .expect("render");
        assert_eq!(out.plain_text, "Bonjour : comment ça va ?");
        assert_eq!(out.excerpt, "Bonjour : comment ça va ?");
    }

    #[test]
    fn closing_brackets_after_break_also_hug_the_word() {
        // `)` and `]` are in the closing set too, so a break before them is
        // collapsed the same way.
        let mut out = String::new();
        append_plain_text(&mut out, "see footnote");
        append_space(&mut out);
        append_plain_text(&mut out, ") done");
        assert_eq!(out, "see footnote) done");
    }

    #[test]
    fn normalize_whitespace_only_collapses_runs() {
        assert_eq!(
            normalize_whitespace("  a \t b\n c  "),
            "a b c",
            "whitespace runs collapse to single spaces; nothing else changes"
        );
    }
}
