use suprnova::content::MarkdownRenderer;

#[test]
fn markdown_renderer_adds_anchors_highlights_and_strips_html() {
    let out = MarkdownRenderer::default()
        .render("# Install\n\n```rust\nfn main() {}\n```\n\n<script>alert(1)</script>")
        .unwrap();

    assert!(out.html.contains("id=\"install\""));
    assert!(out.html.contains("language-rust"));
    assert!(!out.html.contains("<script>"));
    assert_eq!(out.headings[0].title, "Install");
}

#[test]
fn markdown_renderer_suffixes_duplicate_headings_and_extracts_plain_text() {
    let out = MarkdownRenderer::default()
        .render("# Intro\n\nBody text.\n\n## Intro\n\nMore text.")
        .unwrap();

    assert_eq!(out.headings[0].id, "intro");
    assert_eq!(out.headings[1].id, "intro-2");
    assert!(out.plain_text.contains("Body text."));
    assert_eq!(out.excerpt, "Intro Body text. Intro More text.");
}
