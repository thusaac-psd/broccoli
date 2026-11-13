//! Output-capture helpers shared by the sandbox backends.
//!
//! Both the isolate and mock backends report a capped text preview of a
//! step's stdout/stderr, built here so the two cannot drift. The preview keeps
//! every byte's text as captured, NUL included; the database handle replaces
//! NUL when the text is stored (`common::db`).

use std::path::Path;
use tokio::io::AsyncReadExt;

pub(super) const INLINE_OUTPUT_PREVIEW_BYTES: usize = 64 * 1024;

pub(super) fn text_preview_from_bytes(mut bytes: Vec<u8>, truncated: bool) -> String {
    let was_truncated = truncated || bytes.len() > INLINE_OUTPUT_PREVIEW_BYTES;
    if bytes.len() > INLINE_OUTPUT_PREVIEW_BYTES {
        bytes.truncate(INLINE_OUTPUT_PREVIEW_BYTES);
    }

    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if was_truncated {
        text.push_str("\n... (truncated)");
    }
    text
}

pub(super) async fn read_text_preview(path: &Path) -> Result<String, std::io::Error> {
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::with_capacity(INLINE_OUTPUT_PREVIEW_BYTES + 1);
    let mut limited = file.take((INLINE_OUTPUT_PREVIEW_BYTES + 1) as u64);
    limited.read_to_end(&mut bytes).await?;
    let truncated = bytes.len() > INLINE_OUTPUT_PREVIEW_BYTES;
    Ok(text_preview_from_bytes(bytes, truncated))
}

#[cfg(test)]
mod text_preview_tests {
    use super::text_preview_from_bytes;

    #[test]
    fn clean_text_passes_through_unchanged() {
        let out = text_preview_from_bytes(b"999 998 997\n".to_vec(), false);
        assert_eq!(out, "999 998 997\n");
    }

    #[test]
    fn truncated_text_ends_with_the_marker() {
        let out = text_preview_from_bytes(b"a\0b".to_vec(), true);
        assert_eq!(out, "a\0b\n... (truncated)");
    }
}
