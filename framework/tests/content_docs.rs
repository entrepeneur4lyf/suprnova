use suprnova::content::{DocsBuildConfig, build_docs};

#[tokio::test]
async fn docs_builder_emits_catalog_and_rewrites_markdown_links() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let out = tmp.path().join("out");

    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(
        src.join("documentation.md"),
        "- [Getting Started](getting-started.md)\n- [Configuration](configuration.md)\n",
    )
    .await
    .unwrap();
    tokio::fs::write(
        src.join("getting-started.md"),
        "# Getting Started\n\nRead [configuration](configuration.md).\n",
    )
    .await
    .unwrap();
    tokio::fs::write(src.join("configuration.md"), "# Configuration\n")
        .await
        .unwrap();

    build_docs(DocsBuildConfig {
        source_dir: src.clone(),
        output_dir: out.clone(),
        toc_file: src.join("documentation.md"),
    })
    .await
    .unwrap();

    let chapter = tokio::fs::read_to_string(out.join("getting-started.json"))
        .await
        .unwrap();
    assert!(chapter.contains("/docs/configuration"));

    let catalog = tokio::fs::read_to_string(out.join("catalog.json"))
        .await
        .unwrap();
    assert!(catalog.contains("Getting Started"));
    assert!(catalog.contains("\"previous\":null"));
    assert!(catalog.contains("\"next\":\"configuration\""));
}
