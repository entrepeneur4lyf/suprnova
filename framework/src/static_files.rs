//! Framework-native static file fallback serving.
//!
//! [`StaticFiles`] is intended for `fallback!` registration: normal routes
//! win first, and safe `GET` / `HEAD` misses can be resolved from a configured
//! public directory without handing the request to application code.

use crate::app::paths::public_path;
use crate::http::{HttpResponse, Request, Response};
use bytes::Bytes;
use futures::Stream;
use hyper::Method;
use std::convert::Infallible;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

const FILE_CHUNK_SIZE: usize = 64 * 1024;
const SNIFF_PREFIX_BYTES: usize = 8 * 1024;

/// Static file fallback handler rooted at a configured directory.
#[derive(Clone, Debug)]
pub struct StaticFiles {
    root: PathBuf,
    cache_control: Option<String>,
}

impl StaticFiles {
    /// Serve files from Suprnova's configured public directory.
    pub fn public() -> Self {
        Self::from_dir(public_path(""))
    }

    /// Serve files from `root`.
    pub fn from_dir(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache_control: None,
        }
    }

    /// Attach a `Cache-Control` header to successful static file responses.
    pub fn cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = Some(value.into());
        self
    }

    /// Build a cloneable fallback handler suitable for `fallback!(...)`.
    pub fn handler(
        self,
    ) -> impl Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Clone + Send + Sync + 'static
    {
        let files = Arc::new(self);
        move |request| {
            let files = files.clone();
            Box::pin(async move { files.serve(request).await })
        }
    }

    async fn serve(self: Arc<Self>, request: Request) -> Response {
        let is_head = request.method() == Method::HEAD;
        if request.method() != Method::GET && !is_head {
            return not_found();
        }

        let Some(relative_path) = safe_relative_path(request.path()) else {
            return not_found();
        };

        let Ok(root) = tokio::fs::canonicalize(&self.root).await else {
            return not_found();
        };

        let candidate = root.join(relative_path);
        let Ok(file_path) = tokio::fs::canonicalize(candidate).await else {
            return not_found();
        };

        if !file_path.starts_with(&root) {
            return not_found();
        }

        let Ok(file) = tokio::fs::File::open(&file_path).await else {
            return not_found();
        };

        let Ok(metadata) = file.metadata().await else {
            return not_found();
        };
        if !metadata.is_file() {
            return not_found();
        }

        let content_type = content_type_for(&file_path).await;
        let content_length = metadata.len();
        let mut response = if is_head {
            drop(file);
            HttpResponse::bytes_body(Bytes::new(), content_type)
        } else {
            HttpResponse::stream_bytes(FileByteStream::new(file, content_length))
                .header("Content-Type", content_type)
        }
        .header("Content-Length", content_length.to_string());

        if let Some(value) = &self.cache_control {
            response = response.header("Cache-Control", value.clone());
        }

        Ok(response)
    }
}

fn not_found() -> Response {
    Ok(HttpResponse::text("404 Not Found").status(404))
}

fn safe_relative_path(path: &str) -> Option<PathBuf> {
    let decoded = decode_url_path(path)?;
    let relative = decoded.strip_prefix('/')?;
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.contains('\0')
        || relative.contains('\\')
        || has_windows_drive_prefix(relative)
        || has_dot_component(relative)
    {
        return None;
    }

    let path = Path::new(relative);
    if path.is_absolute() {
        return None;
    }

    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return None;
            }
        }
    }

    if safe.as_os_str().is_empty() {
        None
    } else {
        Some(safe)
    }
}

fn decode_url_path(path: &str) -> Option<String> {
    let encoded = path
        .replace('+', "%2B")
        .replace('&', "%26")
        .replace('=', "%3D");
    let query = format!("path={encoded}");
    url::form_urlencoded::parse(query.as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn has_dot_component(path: &str) -> bool {
    path.split('/')
        .any(|segment| segment == "." || segment == "..")
}

async fn content_type_for(path: &Path) -> String {
    let mime = match mime_from_extension(path) {
        Some(mime) => mime,
        None => {
            let prefix = sniff_prefix(path).await.unwrap_or_default();
            mime_from_content(&prefix).unwrap_or("application/octet-stream")
        }
    };

    if should_add_charset(mime) {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    }
}

fn mime_from_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "aac" => "audio/aac",
        "avif" => "image/avif",
        "bin" => "application/octet-stream",
        "bmp" => "image/bmp",
        "br" => "application/octet-stream",
        "css" => "text/css",
        "csv" => "text/csv",
        "eot" => "application/vnd.ms-fontobject",
        "gif" => "image/gif",
        "gz" => "application/gzip",
        "htm" | "html" => "text/html",
        "ico" => "image/x-icon",
        "jpeg" | "jpg" => "image/jpeg",
        "js" | "mjs" => "text/javascript",
        "json" | "map" => "application/json",
        "md" => "text/markdown",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "oga" => "audio/ogg",
        "ogg" => "audio/ogg",
        "ogv" => "video/ogg",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "tar" => "application/x-tar",
        "text" | "txt" => "text/plain",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "wav" => "audio/wav",
        "webm" => "video/webm",
        "webmanifest" => "application/manifest+json",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "xml" => "application/xml",
        "zip" => "application/zip",
        _ => return None,
    })
}

fn mime_from_content(bytes: &[u8]) -> Option<&'static str> {
    if let Some(kind) = infer::get(bytes) {
        return Some(kind.mime_type());
    }

    let text = std::str::from_utf8(bytes).ok()?;
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let prefix = trimmed
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();

    if prefix.starts_with("<svg") {
        return Some("image/svg+xml");
    }
    if prefix.starts_with("<?xml") {
        return Some("application/xml");
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return Some("application/json");
    }

    Some("text/plain")
}

fn should_add_charset(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/javascript"
                | "application/json"
                | "application/manifest+json"
                | "application/xml"
                | "image/svg+xml"
                | "text/javascript"
                | "text/xml"
        )
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
}

async fn sniff_prefix(path: &Path) -> Option<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut prefix = vec![0; SNIFF_PREFIX_BYTES];
    let read = file.read(&mut prefix).await.ok()?;
    prefix.truncate(read);
    Some(prefix)
}

struct FileByteStream {
    file: tokio::fs::File,
    remaining: u64,
    finished: bool,
}

impl FileByteStream {
    fn new(file: tokio::fs::File, content_length: u64) -> Self {
        Self {
            file,
            remaining: content_length,
            finished: false,
        }
    }
}

impl Stream for FileByteStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        if this.remaining == 0 {
            this.finished = true;
            return Poll::Ready(None);
        }

        let chunk_size = FILE_CHUNK_SIZE.min(this.remaining as usize);
        let mut buffer = vec![0; chunk_size];
        let mut read_buffer = ReadBuf::new(&mut buffer);
        match Pin::new(&mut this.file).poll_read(cx, &mut read_buffer) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => {
                let read = read_buffer.filled().len();
                if read == 0 {
                    this.finished = true;
                    Poll::Ready(None)
                } else {
                    this.remaining = this.remaining.saturating_sub(read as u64);
                    buffer.truncate(read);
                    Poll::Ready(Some(Ok(Bytes::from(buffer))))
                }
            }
            Poll::Ready(Err(error)) => {
                this.finished = true;
                tracing::warn!(
                    error = %error,
                    "static file stream read failed; ending response body"
                );
                Poll::Ready(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn file_byte_stream_never_exceeds_captured_length() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("asset.bin");
        tokio::fs::write(&path, b"abc").await.expect("write seed");

        let file = tokio::fs::File::open(&path)
            .await
            .expect("open stream file");
        let captured_len = 3;

        let mut append = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open append handle");
        append.write_all(b"def").await.expect("append growth");
        append.flush().await.expect("flush growth");
        drop(append);

        let mut stream = FileByteStream::new(file, captured_len);
        let mut emitted = Vec::new();
        while let Some(chunk) = stream.next().await {
            emitted.extend_from_slice(&chunk.expect("infallible stream chunk"));
        }

        assert_eq!(emitted, b"abc");
    }
}
