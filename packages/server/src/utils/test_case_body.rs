use std::sync::Arc;

use common::storage::{BlobStore, ContentHash};

use crate::error::AppError;
use crate::utils::text::sanitize_db_text;

pub const INLINE_TEST_CASE_BODY_THRESHOLD_BYTES: usize = 1_048_576;
const PREVIEW_CHARS: usize = 100;
const STORED_PREVIEW_CHARS: usize = PREVIEW_CHARS + 1;

/// Upper bound, in bytes, on the TOTAL inline test-case body bytes (input and
/// expected_output combined, across every test case) assembled into a single
/// submission's plugin dispatch payload.
///
/// [`INLINE_TEST_CASE_BODY_THRESHOLD_BYTES`] only bounds ONE body at a time.
/// It does nothing to bound the sum across a whole test-case set, and that
/// sum is what the judging plugin's WASM guest actually has to hold. This was
/// measured end-to-end (real judging, not simulated) against a 50-test-case
/// problem with a trivial `cat`-style solution:
///
/// | per-case size | aggregate inline | result                              |
/// |---------------|-------------------|--------------------------------------|
/// | 100,000 B     | 4.8 MiB           | judged, 3.3s                         |
/// | 1,000,000 B   | 47.7 MiB          | guest OOM (`oom` trap)                |
/// | 1,100,000 B   | 52.5 MiB (blob)   | judged, 2.0s -- faster, and BIGGER    |
/// | 1,000,000 B x 25 | 25 MiB         | judged, 10.6s                        |
///
/// The failure tracks aggregate inline bytes, not case count or total
/// problem size: 50 cases at 100 KB is fine, 50 at 1 MB is not, and the
/// all-blob 52.5 MiB run is both correct and 4x faster than the 4.8 MiB
/// inline run. The measured safe/unsafe cliff sits between 25 MiB and
/// 48 MiB; this budget is set well below that margin. Note that the guest's
/// own configured cap is 1.5 GiB (`max_instance_memory_pages` in
/// `plugin-core`), so ~48 MiB tripping it implies roughly 30x amplification
/// somewhere that this budget does NOT explain or fix -- it only removes the
/// trigger.
pub const AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Decide, in a fixed and deterministic order, which candidate inline bodies
/// (given as `sizes`, in that order) fit within `budget` cumulative bytes.
///
/// Greedy left-to-right: a candidate is kept inline as long as adding its
/// size to the running total of already-kept candidates does not exceed
/// `budget`; otherwise it is marked to spill (and does not contribute to the
/// running total, so a later, smaller candidate can still fit). The result
/// is `true`/`false` per input position, same length and order as `sizes`.
///
/// This is a pure function of `(sizes, order, budget)`: the same input
/// always produces the same split. That determinism is what makes the
/// dispatch-time re-derivation of the inline/blob split for a given
/// submission's test cases stable across rejudges, and what lets it repair
/// pre-existing over-budget problems without a data migration -- every
/// dispatch just recomputes the split from current storage state.
pub fn select_inline_within_budget(sizes: &[usize], budget: usize) -> Vec<bool> {
    let mut running: usize = 0;
    sizes
        .iter()
        .map(|&size| {
            let next = running.saturating_add(size);
            if next <= budget {
                running = next;
                true
            } else {
                false
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct PreparedTestCaseBody {
    pub inline_text: String,
    pub blob_hash: Option<String>,
    pub size: i64,
    pub preview: String,
}

pub async fn prepare_test_case_body(
    body: String,
    blob_store: Arc<dyn BlobStore>,
) -> Result<PreparedTestCaseBody, AppError> {
    // Sanitize ONCE up front so the inline column, the blob, the reported size,
    // and the preview all describe the SAME bytes. The inline column is Postgres
    // TEXT, which rejects NUL, so the inline path must sanitize; sanitizing the
    // blob path identically means a test case containing a NUL is judged against
    // the same bytes whether it is small (inline) or large (blob), instead of
    // diverging at the 1 MiB boundary. Deriving `size` from the sanitized form
    // also keeps `input_size` consistent with what is actually stored (each NUL
    // expands 1 -> 3 bytes as U+FFFD).
    let body = sanitize_db_text(body);
    let size = i64::try_from(body.len())
        .map_err(|_| AppError::Validation("Test case body is too large".into()))?;
    let preview = body.chars().take(STORED_PREVIEW_CHARS).collect::<String>();

    if body.len() < INLINE_TEST_CASE_BODY_THRESHOLD_BYTES {
        return Ok(PreparedTestCaseBody {
            inline_text: body,
            blob_hash: None,
            size,
            preview,
        });
    }

    let hash = blob_store
        .put(body.as_bytes())
        .await
        .map_err(|e| AppError::Internal(format!("Failed to store test case body blob: {e}")))?;

    Ok(PreparedTestCaseBody {
        inline_text: String::new(),
        blob_hash: Some(hash.to_hex()),
        size,
        preview,
    })
}

pub async fn read_test_case_body(
    inline_text: &str,
    blob_hash: Option<&str>,
    blob_store: &dyn BlobStore,
) -> Result<String, AppError> {
    let Some(hash) = blob_hash else {
        return Ok(inline_text.to_string());
    };

    let hash = ContentHash::from_hex(hash)
        .map_err(|e| AppError::Internal(format!("Invalid test case body blob hash: {e}")))?;
    let bytes = blob_store
        .get(&hash)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read test case body blob: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| AppError::Internal(format!("Test case body blob is not UTF-8: {e}")))
}

/// Maximum bytes of a test-case `input`/`expected_output` body to inline into an
/// API *response*. Test data can be tens of megabytes; inlining it whole turns a
/// submission-status poll into a multi-gigabyte JSON serialization. A
/// 30 MB-per-testcase problem with ~30 test cases, polled by N clients while
/// judging, would otherwise serialize ~N x 1.8 GB and OOM-kill the server (the
/// `serde_json` output buffer grows by doubling toward 2 GB per response). The
/// response shows a bounded preview instead; the full body remains available via
/// the dedicated blob/attachment download endpoints, which stream.
pub const RESPONSE_BODY_PREVIEW_BYTES: usize = 64 * 1024;

/// Read a bounded preview of a test-case body for inclusion in a response.
///
/// Unlike [`read_test_case_body`], this **never reads the whole blob**: for
/// blob-backed bodies it issues a single bounded range read of at most
/// `RESPONSE_BODY_PREVIEW_BYTES` (+1 byte to detect truncation), so peak memory
/// is bounded regardless of test-case size or request concurrency. A
/// `"\n... (truncated)"` marker is appended when the body exceeds the cap. UTF-8
/// boundaries are handled via lossy decoding, so a multi-byte character split at
/// the cap never produces an error.
pub async fn read_test_case_body_preview(
    inline_text: &str,
    blob_hash: Option<&str>,
    blob_store: &dyn BlobStore,
) -> Result<String, AppError> {
    let cap = RESPONSE_BODY_PREVIEW_BYTES;

    let Some(hash) = blob_hash else {
        return Ok(preview_from_bytes(inline_text.as_bytes(), cap));
    };

    let hash = ContentHash::from_hex(hash)
        .map_err(|e| AppError::Internal(format!("Invalid test case body blob hash: {e}")))?;
    // Read one extra byte so we can tell whether the body was longer than the
    // cap without ever pulling the whole (potentially 30 MB) blob into memory.
    let (bytes, _eof) = blob_store
        .get_range(&hash, 0, cap + 1)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read test case body blob: {e}")))?;
    Ok(preview_from_bytes(&bytes, cap))
}

fn preview_from_bytes(bytes: &[u8], cap: usize) -> String {
    if bytes.len() <= cap {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut text = String::from_utf8_lossy(&bytes[..cap]).into_owned();
    text.push_str("\n… (truncated)");
    text
}

pub fn test_case_body_size(inline_text: &str, stored_size: Option<i64>) -> usize {
    stored_size
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(inline_text.len())
}

pub fn test_case_body_preview(inline_text: &str, stored_preview: Option<&str>) -> String {
    stored_preview.map(ToString::to_string).unwrap_or_else(|| {
        sanitize_db_text(
            inline_text
                .chars()
                .take(STORED_PREVIEW_CHARS)
                .collect::<String>(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::storage::filesystem::FilesystemBlobStore;

    async fn blob_store() -> Arc<dyn BlobStore> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("blobs");
        Arc::new(
            FilesystemBlobStore::new(path, 16 * 1024 * 1024)
                .await
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn small_body_stays_inline() {
        let prepared = prepare_test_case_body("hello".to_string(), blob_store().await)
            .await
            .unwrap();

        assert_eq!(prepared.inline_text, "hello");
        assert_eq!(prepared.blob_hash, None);
        assert_eq!(prepared.size, 5);
        assert_eq!(prepared.preview, "hello");
    }

    #[tokio::test]
    async fn large_body_moves_to_blob() {
        let store = blob_store().await;
        let body = "x".repeat(INLINE_TEST_CASE_BODY_THRESHOLD_BYTES);

        let prepared = prepare_test_case_body(body.clone(), store.clone())
            .await
            .unwrap();

        assert!(prepared.inline_text.is_empty());
        let hash = prepared.blob_hash.expect("blob hash");
        assert_eq!(
            read_test_case_body("", Some(&hash), &*store).await.unwrap(),
            body
        );
    }

    #[tokio::test]
    async fn inline_nul_is_sanitized_and_size_matches_stored_bytes() {
        let prepared = prepare_test_case_body("a\0b".to_string(), blob_store().await)
            .await
            .unwrap();

        assert_eq!(prepared.inline_text, "a\u{FFFD}b");
        assert!(!prepared.inline_text.contains('\0'));
        // U+FFFD is 3 bytes, so "a\0b" (3 bytes raw) stores as 5 bytes; the
        // reported size must match the stored (sanitized) form, not the raw len.
        assert_eq!(prepared.size, "a\u{FFFD}b".len() as i64);
    }

    #[tokio::test]
    async fn blob_path_sanitizes_nul_identically_to_inline() {
        let store = blob_store().await;
        // Large enough to take the blob path, and containing a NUL: the blob must
        // be sanitized just like the inline path so the same content is judged
        // identically on either side of the 1 MiB boundary.
        let body = format!("{}\0", "x".repeat(INLINE_TEST_CASE_BODY_THRESHOLD_BYTES));

        let prepared = prepare_test_case_body(body, store.clone()).await.unwrap();

        assert!(prepared.inline_text.is_empty());
        let hash = prepared.blob_hash.expect("blob hash");
        let read_back = read_test_case_body("", Some(&hash), &*store).await.unwrap();
        assert!(!read_back.contains('\0'), "blob body must not retain NUL");
        assert!(read_back.ends_with('\u{FFFD}'), "NUL sanitized to U+FFFD");
    }

    // -- aggregate inline budget (`select_inline_within_budget`) --
    //
    // These are pure, non-async tests of the selection function itself. The
    // async, blob-store-touching wiring lives in
    // `services::submission_dispatch`, which is why the "which body spills"
    // decision is tested here as a pure function while the "how a spill
    // actually happens" is tested there.

    #[test]
    fn aggregate_budget_triggers_spill_even_though_every_case_is_under_the_per_case_threshold() {
        // 50 bodies at 1,000,000 B each (< INLINE_TEST_CASE_BODY_THRESHOLD_BYTES,
        // so every single one would pass the per-case check) sum to ~47.7 MiB,
        // which is the exact measured OOM shape. None of them individually
        // trips the per-case threshold, so only an aggregate check can catch
        // this.
        let sizes = vec![1_000_000usize; 50];
        assert!(
            sizes
                .iter()
                .all(|&s| s < INLINE_TEST_CASE_BODY_THRESHOLD_BYTES)
        );

        let keep = select_inline_within_budget(&sizes, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES);

        assert!(
            keep.iter().any(|&k| !k),
            "50 x 1,000,000 B must not all stay inline under the aggregate budget"
        );
        let kept_bytes: usize = sizes
            .iter()
            .zip(&keep)
            .filter(|&(_, &k)| k)
            .map(|(&s, _)| s)
            .sum();
        assert!(kept_bytes <= AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES);
    }

    #[test]
    fn boundary_exactly_at_budget_stays_inline_one_byte_over_spills() {
        let at_budget = vec![AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES];
        assert_eq!(
            select_inline_within_budget(&at_budget, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES),
            vec![true]
        );

        let one_over = vec![AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES + 1];
        assert_eq!(
            select_inline_within_budget(&one_over, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES),
            vec![false]
        );
    }

    #[test]
    fn small_problem_is_entirely_unaffected() {
        // Negative control: a small problem (well under budget) must have
        // every body stay inline -- no spilling.
        let sizes = vec![100_000usize; 50]; // 4.8 MiB aggregate, the measured-fine shape.
        let keep = select_inline_within_budget(&sizes, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES);
        assert!(
            keep.iter().all(|&k| k),
            "small problem must not spill anything"
        );
    }

    #[test]
    fn selection_is_deterministic_across_repeated_calls() {
        let sizes = vec![
            2 * 1024 * 1024,
            3 * 1024 * 1024,
            1024,
            5 * 1024 * 1024,
            4 * 1024 * 1024,
        ];
        let first = select_inline_within_budget(&sizes, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES);
        let second = select_inline_within_budget(&sizes, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES);
        let third = select_inline_within_budget(&sizes, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES);
        assert_eq!(
            first, second,
            "same input (same order) must produce the same split"
        );
        assert_eq!(
            second, third,
            "same input (same order) must produce the same split"
        );
    }
}
