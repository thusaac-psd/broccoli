#[derive(Debug)]
pub enum FilenameError {
    Empty,
    ContainsPathSeparator,
    PathTraversal,
    NullByte,
    Hidden,
    ControlCharacter,
    LeadingDash,
}

impl FilenameError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Empty => "Filename cannot be empty",
            Self::ContainsPathSeparator => "Invalid filename: path separators are not allowed",
            Self::PathTraversal => "Invalid filename: '..' is not allowed",
            Self::NullByte => "Invalid filename: null bytes are not allowed",
            Self::Hidden => "Invalid filename: hidden files (starting with '.') are not allowed",
            Self::ControlCharacter => "Invalid filename: control characters are not allowed",
            Self::LeadingDash => "Invalid filename: cannot start with '-'",
        }
    }
}

pub fn validate_flat_filename(filename: &str) -> Result<&str, FilenameError> {
    let trimmed = filename.trim();

    if trimmed.is_empty() {
        return Err(FilenameError::Empty);
    }

    if trimmed.contains('\0') {
        return Err(FilenameError::NullByte);
    }

    if trimmed.chars().any(|c| c.is_ascii_control()) {
        return Err(FilenameError::ControlCharacter);
    }

    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(FilenameError::ContainsPathSeparator);
    }

    if trimmed == ".." {
        return Err(FilenameError::PathTraversal);
    }

    if trimmed.starts_with('.') {
        return Err(FilenameError::Hidden);
    }

    // A leading '-' makes the filename look like a command-line OPTION. Source
    // filenames are passed as bare argv tokens to the compiler/interpreter (e.g.
    // `g++ ... solution.cpp <name> grader.cpp`), so a file named `-DNDEBUG` or
    // `-w` would be parsed as a compiler FLAG and applied while building a TRUSTED
    // co-compiled grader (stripping asserts / anti-cheat), not treated as a file.
    // Reject it here; the argv builders also prefix `./` as defense in depth.
    if trimmed.starts_with('-') {
        return Err(FilenameError::LeadingDash);
    }

    Ok(trimmed)
}

pub fn contains_path_traversal(path: &str) -> bool {
    path == ".."
        || path.starts_with("../")
        || path.contains("/../")
        || path.ends_with("/..")
        || path.starts_with("..\\")
        || path.contains("\\..\\")
        || path.ends_with("\\..")
}

pub fn split_dir_filename(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("", path),
    }
}

pub fn validate_virtual_path(path: &str) -> Result<String, &'static str> {
    let trimmed = path.trim();

    if trimmed.is_empty() {
        return Err("Path cannot be empty");
    }

    if trimmed.len() > 512 {
        return Err("Path exceeds maximum length of 512 characters");
    }

    if trimmed.contains('\0') {
        return Err("Path must not contain null bytes");
    }

    if trimmed.contains('\\') {
        return Err("Path must not contain backslashes");
    }

    if trimmed.starts_with('/') {
        return Err("Path must not start with '/'");
    }

    if trimmed.ends_with('/') {
        return Err("Path must not end with '/'");
    }

    if trimmed.contains("//") {
        return Err("Path must not contain consecutive slashes");
    }

    if contains_path_traversal(trimmed) {
        return Err("Path must not contain '..' traversal");
    }

    for segment in trimmed.split('/') {
        if segment.is_empty() {
            return Err("Path must not contain empty segments");
        }
        if segment.starts_with('.') {
            return Err("Path segments must not start with '.'");
        }
    }

    if trimmed.chars().any(|c| c.is_control()) {
        return Err("Path must not contain control characters");
    }

    Ok(trimmed.to_string())
}

pub fn is_sample_directory(dir: &str) -> bool {
    let lower = dir.to_lowercase();
    lower == "sample" || lower.ends_with("/sample")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_filename_accepts_typical_source_filenames() {
        assert!(validate_flat_filename("solution.cpp").is_ok());
        assert!(validate_flat_filename("Main.java").is_ok());
        assert!(validate_flat_filename("test_file.py").is_ok());
        assert!(validate_flat_filename("file-name.rs").is_ok()); // dash mid-name is fine
    }

    #[test]
    fn flat_filename_rejects_leading_dash_argument_injection() {
        // A leading '-' would be parsed as a compiler/interpreter FLAG when the
        // filename is passed as a bare argv token (e.g. injecting -DNDEBUG into a
        // trusted co-compiled grader). Must be rejected; a mid-name dash is fine.
        assert!(validate_flat_filename("-DNDEBUG").is_err());
        assert!(validate_flat_filename("-o").is_err());
        assert!(validate_flat_filename("--version").is_err());
        assert!(validate_flat_filename("-w.cpp").is_err());
    }

    #[test]
    fn flat_filename_returns_trimmed_value() {
        assert_eq!(
            validate_flat_filename("  padded.txt  ").unwrap(),
            "padded.txt"
        );
    }

    #[test]
    fn validate_flat_filename_rejects_empty() {
        assert!(matches!(
            validate_flat_filename(""),
            Err(FilenameError::Empty)
        ));
        assert!(matches!(
            validate_flat_filename("   "),
            Err(FilenameError::Empty)
        ));
    }

    #[test]
    fn validate_flat_filename_rejects_path_separators() {
        assert!(matches!(
            validate_flat_filename("src/main.rs"),
            Err(FilenameError::ContainsPathSeparator)
        ));
        assert!(matches!(
            validate_flat_filename("src\\main.rs"),
            Err(FilenameError::ContainsPathSeparator)
        ));
    }

    #[test]
    fn validate_flat_filename_rejects_path_traversal() {
        assert!(matches!(
            validate_flat_filename(".."),
            Err(FilenameError::PathTraversal)
        ));
    }

    #[test]
    fn validate_flat_filename_allows_double_dots_in_name() {
        assert!(validate_flat_filename("foo..bar").is_ok());
        assert!(validate_flat_filename("archive..tar.gz").is_ok());
    }

    #[test]
    fn validate_flat_filename_rejects_null_bytes() {
        assert!(matches!(
            validate_flat_filename("foo\0bar"),
            Err(FilenameError::NullByte)
        ));
    }

    #[test]
    fn validate_flat_filename_rejects_control_characters() {
        assert!(matches!(
            validate_flat_filename("file\r\nname.txt"),
            Err(FilenameError::ControlCharacter)
        ));
        assert!(matches!(
            validate_flat_filename("file\tname.txt"),
            Err(FilenameError::ControlCharacter)
        ));
    }

    #[test]
    fn validate_flat_filename_rejects_hidden_files() {
        assert!(matches!(
            validate_flat_filename(".hidden"),
            Err(FilenameError::Hidden)
        ));
        assert!(matches!(
            validate_flat_filename(".gitignore"),
            Err(FilenameError::Hidden)
        ));
    }

    #[test]
    fn contains_path_traversal_detects_patterns() {
        assert!(contains_path_traversal(".."));
        assert!(contains_path_traversal("../foo"));
        assert!(contains_path_traversal("foo/../bar"));
        assert!(contains_path_traversal("foo/.."));
        assert!(!contains_path_traversal("foo/bar"));
        assert!(!contains_path_traversal("foo..bar"));
    }

    #[test]
    fn is_sample_directory_matches_case_insensitively() {
        assert!(is_sample_directory("sample"));
        assert!(is_sample_directory("Sample"));
        assert!(is_sample_directory("SAMPLE"));
    }

    #[test]
    fn is_sample_directory_matches_trailing_sample_segment() {
        assert!(is_sample_directory("tests/sample"));
    }

    #[test]
    fn is_sample_directory_rejects_non_sample_dirs() {
        assert!(!is_sample_directory("samples"));
        assert!(!is_sample_directory("main"));
    }

    #[test]
    fn validate_virtual_path_accepts_valid_paths() {
        assert!(validate_virtual_path("images/figure1.png").is_ok());
        assert!(validate_virtual_path("statements/en.md").is_ok());
        assert!(validate_virtual_path("file.txt").is_ok());
        assert!(validate_virtual_path("a/b/c/d.txt").is_ok());
        assert!(validate_virtual_path("my-file_v2.tar.gz").is_ok());
        assert!(validate_virtual_path("  padded.txt  ").is_ok());
    }

    #[test]
    fn validate_virtual_path_rejects_empty() {
        assert!(validate_virtual_path("").is_err());
        assert!(validate_virtual_path("   ").is_err());
    }

    #[test]
    fn validate_virtual_path_rejects_traversal() {
        assert!(validate_virtual_path("..").is_err());
        assert!(validate_virtual_path("../etc/passwd").is_err());
        assert!(validate_virtual_path("foo/../bar").is_err());
        assert!(validate_virtual_path("foo/..").is_err());
    }

    #[test]
    fn validate_virtual_path_rejects_leading_trailing_slash() {
        assert!(validate_virtual_path("/absolute").is_err());
        assert!(validate_virtual_path("trailing/").is_err());
    }

    #[test]
    fn validate_virtual_path_rejects_consecutive_slashes() {
        assert!(validate_virtual_path("foo//bar").is_err());
    }

    #[test]
    fn validate_virtual_path_rejects_backslash() {
        assert!(validate_virtual_path("foo\\bar").is_err());
    }

    #[test]
    fn validate_virtual_path_rejects_hidden_segments() {
        assert!(validate_virtual_path(".hidden").is_err());
        assert!(validate_virtual_path("dir/.hidden").is_err());
        assert!(validate_virtual_path(".git/config").is_err());
    }

    #[test]
    fn validate_virtual_path_rejects_null_bytes() {
        assert!(validate_virtual_path("foo\0bar").is_err());
    }

    #[test]
    fn validate_virtual_path_rejects_control_characters() {
        assert!(validate_virtual_path("file\r\nname.txt").is_err());
        assert!(validate_virtual_path("file\tname.txt").is_err());
        assert!(validate_virtual_path("file\x7fname.txt").is_err());
    }

    #[test]
    fn validate_virtual_path_accepts_unicode_filenames() {
        assert!(validate_virtual_path("测试附件.png").is_ok());
        assert!(validate_virtual_path("attachments/テスト.pdf").is_ok());
        assert!(validate_virtual_path("문서/한글.txt").is_ok());
        assert!(validate_virtual_path("file name.txt").is_ok());
        assert!(validate_virtual_path("résumé.pdf").is_ok());
    }

    #[test]
    fn validate_virtual_path_rejects_too_long() {
        let long_path = "a".repeat(513);
        assert!(validate_virtual_path(&long_path).is_err());
    }
}
