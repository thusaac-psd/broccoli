pub const MIB: usize = 1024 * 1024;
pub const GIB: usize = 1024 * MIB;

pub const LARGE_UPLOAD_LIMIT_BYTES: usize = GIB;
pub const LARGE_UPLOAD_LIMIT_MIB: usize = LARGE_UPLOAD_LIMIT_BYTES / MIB;

pub const BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_BYTES: u64 = 4 * GIB as u64;
pub const BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_MIB: u64 =
    BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_BYTES / MIB as u64;

/// Body limit for a JSON request carrying source files (submissions, code
/// runs): `submission.max_size` of file content plus room for the envelope.
pub const fn source_request_limit_bytes(submission_max_size: usize) -> usize {
    submission_max_size.saturating_add(4096)
}
