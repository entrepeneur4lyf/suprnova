//! Markdown rendering and documentation build helpers.

mod docs;
mod headings;
mod markdown;

pub use docs::{
    DocsBuildConfig, DocsCatalog, DocsCatalogEntry, DocsChapter, DocsSearchEntry, build_docs,
};
pub use headings::{Heading, slugify_heading};
pub use markdown::{
    ContentError, ContentResult, MarkdownOptions, MarkdownRenderer, RenderedMarkdown,
};
