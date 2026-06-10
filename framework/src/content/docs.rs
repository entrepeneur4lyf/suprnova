//! Documentation catalog builder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::headings::{Heading, slugify_heading};
use super::markdown::{ContentResult, MarkdownRenderer};

/// Input and output paths for a documentation build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocsBuildConfig {
    /// Directory containing Markdown chapter files.
    pub source_dir: PathBuf,
    /// Directory where JSON artifacts are written.
    pub output_dir: PathBuf,
    /// Markdown table-of-contents file, usually `documentation.md`.
    pub toc_file: PathBuf,
}

/// A rendered documentation chapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocsChapter {
    /// URL slug and JSON filename stem for this chapter.
    pub slug: String,
    /// Display title for this chapter.
    pub title: String,
    /// Rendered chapter HTML.
    pub html: String,
    /// Short chapter preview.
    pub excerpt: String,
    /// Ordered headings extracted from the chapter.
    pub headings: Vec<Heading>,
    /// Previous chapter slug, if one exists.
    pub previous: Option<String>,
    /// Next chapter slug, if one exists.
    pub next: Option<String>,
}

/// Catalog artifact used by docs index and search pages.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocsCatalog {
    /// Ordered chapter entries.
    pub chapters: Vec<DocsCatalogEntry>,
    /// Search payload for client-side filtering.
    pub search: Vec<DocsSearchEntry>,
}

/// Lightweight catalog entry for one chapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocsCatalogEntry {
    /// Chapter slug.
    pub slug: String,
    /// Chapter display title.
    pub title: String,
    /// Chapter preview.
    pub excerpt: String,
    /// Ordered headings extracted from the chapter.
    pub headings: Vec<Heading>,
    /// Previous chapter slug, if one exists.
    pub previous: Option<String>,
    /// Next chapter slug, if one exists.
    pub next: Option<String>,
}

/// Search index entry for one chapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocsSearchEntry {
    /// Chapter slug.
    pub slug: String,
    /// Chapter display title.
    pub title: String,
    /// Chapter preview.
    pub excerpt: String,
    /// Ordered headings extracted from the chapter.
    pub headings: Vec<Heading>,
    /// Plain text extracted from the rendered Markdown.
    pub plain_text: String,
}

/// Build JSON documentation artifacts from a Markdown table of contents.
pub async fn build_docs(config: DocsBuildConfig) -> ContentResult<DocsCatalog> {
    let toc = tokio::fs::read_to_string(&config.toc_file).await?;
    let entries = parse_toc_entries(&toc);
    let renderer = MarkdownRenderer::default();

    tokio::fs::create_dir_all(&config.output_dir).await?;

    let mut chapters = Vec::with_capacity(entries.len());
    let mut catalog_entries = Vec::with_capacity(entries.len());
    let mut search_entries = Vec::with_capacity(entries.len());

    for (index, entry) in entries.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .and_then(|previous| entries.get(previous))
            .map(|entry| entry.slug.clone());
        let next = entries.get(index + 1).map(|entry| entry.slug.clone());
        let markdown_path = config.source_dir.join(&entry.path);
        let markdown = tokio::fs::read_to_string(markdown_path).await?;
        let rewritten = rewrite_markdown_links(&markdown);
        let rendered = renderer.render(&rewritten)?;
        let title = rendered
            .headings
            .first()
            .map(|heading| heading.title.clone())
            .unwrap_or_else(|| entry.title.clone());

        let chapter = DocsChapter {
            slug: entry.slug.clone(),
            title: title.clone(),
            html: rendered.html,
            excerpt: rendered.excerpt.clone(),
            headings: rendered.headings.clone(),
            previous: previous.clone(),
            next: next.clone(),
        };

        tokio::fs::write(
            config.output_dir.join(format!("{}.json", entry.slug)),
            serde_json::to_string(&chapter)?,
        )
        .await?;

        catalog_entries.push(DocsCatalogEntry {
            slug: entry.slug.clone(),
            title: title.clone(),
            excerpt: chapter.excerpt.clone(),
            headings: chapter.headings.clone(),
            previous,
            next,
        });
        search_entries.push(DocsSearchEntry {
            slug: entry.slug.clone(),
            title,
            excerpt: chapter.excerpt.clone(),
            headings: chapter.headings.clone(),
            plain_text: rendered.plain_text,
        });
        chapters.push(chapter);
    }

    let catalog = DocsCatalog {
        chapters: catalog_entries,
        search: search_entries,
    };
    tokio::fs::write(
        config.output_dir.join("catalog.json"),
        serde_json::to_string(&catalog)?,
    )
    .await?;

    Ok(catalog)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TocEntry {
    title: String,
    path: PathBuf,
    slug: String,
}

fn parse_toc_entries(markdown: &str) -> Vec<TocEntry> {
    markdown
        .lines()
        .filter_map(parse_toc_line)
        .collect::<Vec<_>>()
}

fn parse_toc_line(line: &str) -> Option<TocEntry> {
    let title_start = line.find('[')? + 1;
    let title_end = line[title_start..].find(']')? + title_start;
    let path_start = line[title_end..].find('(')? + title_end + 1;
    let path_end = line[path_start..].find(')')? + path_start;

    let title = line[title_start..title_end].trim();
    let path = line[path_start..path_end].trim();
    if title.is_empty() || path.is_empty() {
        return None;
    }

    Some(TocEntry {
        title: title.to_owned(),
        path: PathBuf::from(path),
        slug: slug_from_markdown_path(path),
    })
}

fn rewrite_markdown_links(markdown: &str) -> String {
    let mut output = String::with_capacity(markdown.len());
    let mut rest = markdown;

    while let Some(start) = rest.find("](") {
        output.push_str(&rest[..start + 2]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find(')') else {
            output.push_str(after_open);
            return output;
        };

        output.push_str(&rewrite_link_target(&after_open[..end]));
        rest = &after_open[end..];
    }

    output.push_str(rest);
    output
}

fn rewrite_link_target(target: &str) -> String {
    let split_at = target
        .char_indices()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
        .unwrap_or(target.len());
    let (url, suffix) = target.split_at(split_at);

    if should_rewrite_markdown_url(url) {
        let (path, fragment) = url.split_once('#').unwrap_or((url, ""));
        let fragment = if fragment.is_empty() {
            String::new()
        } else {
            format!("#{fragment}")
        };
        format!(
            "/docs/{}{}{}",
            slug_from_markdown_path(path),
            fragment,
            suffix
        )
    } else {
        target.to_owned()
    }
}

fn should_rewrite_markdown_url(url: &str) -> bool {
    let (path, _) = url.split_once('#').unwrap_or((url, ""));
    path.ends_with(".md")
        && !path.starts_with('/')
        && !path.starts_with('#')
        && !path.contains("://")
}

fn slug_from_markdown_path(path: impl AsRef<str>) -> String {
    let path = Path::new(path.as_ref());
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(slugify_heading)
        .unwrap_or_else(|| "chapter".to_owned())
}
