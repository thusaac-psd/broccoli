//! Comparison modes for `broccoli-compare`.
//!
//! Each mode reads a solution's output and an expected answer and returns a
//! [`Verdict`]. The verdict maps to the process exit code in `main`.
//!
//! The `output` side is borrowed as `&mut impl Read` (rather than taken by
//! value) so that `main` retains ownership of the underlying stdin reader and
//! can keep draining it to EOF after the verdict is decided, so a still-running
//! solution is never killed by SIGPIPE. The `answer` side is a file and
//! is taken by value; it does not need draining.

use std::io::Read;

/// The result of a comparison. The numeric values are the process exit codes
/// defined by the CLI contract: 0 = Accepted, 1 = WrongAnswer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    WrongAnswer,
}

impl Verdict {
    /// The process exit code for this verdict.
    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Accepted => 0,
            Verdict::WrongAnswer => 1,
        }
    }

    /// A short human-readable message for stderr.
    pub fn message(self) -> &'static str {
        match self {
            Verdict::Accepted => "Accepted",
            Verdict::WrongAnswer => "Wrong Answer",
        }
    }
}

/// Strict byte-for-byte comparison of `output` against `answer`.
///
/// There is **no** trailing-whitespace or trailing-newline normalization: the
/// two streams must contain the identical sequence of
/// bytes and end at the same position. A trailing newline (or any other byte)
/// present in one stream but not the other is a [`Verdict::WrongAnswer`].
///
/// Both readers are consumed in fixed-size chunks so ~10 MB inputs do not need
/// to be buffered in full. `output` is borrowed so the caller keeps the reader
/// (and can drain any unread bytes after this returns early).
pub fn compare_exact<O: Read, A: Read>(output: &mut O, mut answer: A) -> std::io::Result<Verdict> {
    const CHUNK: usize = 64 * 1024;
    let mut out_buf = vec![0u8; CHUNK];
    let mut ans_buf = vec![0u8; CHUNK];

    // Carry-over slices: leftover, still-unmatched bytes from a previous read
    // of each side (one side may yield a longer chunk than the other).
    let mut out_lo = 0usize;
    let mut out_hi = 0usize;
    let mut ans_lo = 0usize;
    let mut ans_hi = 0usize;

    let mut out_done = false;
    let mut ans_done = false;

    loop {
        if out_lo == out_hi && !out_done {
            out_lo = 0;
            out_hi = read_some(output, &mut out_buf)?;
            if out_hi == 0 {
                out_done = true;
            }
        }
        if ans_lo == ans_hi && !ans_done {
            ans_lo = 0;
            ans_hi = read_some(&mut answer, &mut ans_buf)?;
            if ans_hi == 0 {
                ans_done = true;
            }
        }

        let out_avail = out_hi - out_lo;
        let ans_avail = ans_hi - ans_lo;
        let n = out_avail.min(ans_avail);

        if out_buf[out_lo..out_lo + n] != ans_buf[ans_lo..ans_lo + n] {
            return Ok(Verdict::WrongAnswer);
        }
        out_lo += n;
        ans_lo += n;

        let out_empty = out_lo == out_hi && out_done;
        let ans_empty = ans_lo == ans_hi && ans_done;

        // Both streams exhausted simultaneously -> identical content -> AC.
        if out_empty && ans_empty {
            return Ok(Verdict::Accepted);
        }
        // One stream exhausted while the other still has unmatched bytes -> WA.
        if (out_empty && ans_lo < ans_hi) || (ans_empty && out_lo < out_hi) {
            return Ok(Verdict::WrongAnswer);
        }
    }
}

/// Read into `buf`, retrying on a zero-length non-EOF read so that a single
/// short read does not look like EOF. Returns the number of bytes read; 0 means
/// genuine EOF.
fn read_some<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match reader.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming character source
// ---------------------------------------------------------------------------

/// Pulls UTF-8 `char`s out of a byte `Read` one at a time, buffering only the
/// few bytes needed to complete a multi-byte code point. Invalid UTF-8 and
/// a stream ending mid-code-point are hard errors (surfaced as
/// [`std::io::ErrorKind::InvalidData`]).
struct CharStream<R: Read> {
    reader: R,
    /// Bytes read but not yet decoded into a full code point.
    pending: Vec<u8>,
    /// Decoded-but-not-yet-yielded characters.
    chars: std::collections::VecDeque<char>,
    eof: bool,
}

impl<R: Read> CharStream<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            chars: std::collections::VecDeque::new(),
            eof: false,
        }
    }

    fn next_char(&mut self) -> std::io::Result<Option<char>> {
        loop {
            if let Some(ch) = self.chars.pop_front() {
                return Ok(Some(ch));
            }
            if self.eof {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stream ended with incomplete UTF-8 sequence",
                ));
            }
            self.fill()?;
        }
    }

    fn fill(&mut self) -> std::io::Result<()> {
        const CHUNK: usize = 64 * 1024;
        let mut buf = [0u8; CHUNK];
        let n = read_some(&mut self.reader, &mut buf)?;
        if n == 0 {
            self.eof = true;
        } else {
            self.pending.extend_from_slice(&buf[..n]);
        }

        match std::str::from_utf8(&self.pending) {
            Ok(valid) => {
                self.chars.extend(valid.chars());
                self.pending.clear();
                Ok(())
            }
            Err(e) => {
                if e.error_len().is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("stream is not UTF-8: {e}"),
                    ));
                }
                // Trailing bytes are a valid *prefix* of a multi-byte code
                // point - keep them pending until more bytes arrive.
                let valid_up_to = e.valid_up_to();
                if valid_up_to == 0 {
                    return Ok(());
                }
                let valid = std::str::from_utf8(&self.pending[..valid_up_to]).map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("stream is not UTF-8: {err}"),
                    )
                })?;
                self.chars.extend(valid.chars());
                self.pending.drain(..valid_up_to);
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Token comparison (tokens / tokens-case-insensitive / tokens-float)
// ---------------------------------------------------------------------------

/// How two tokens at the same position are considered equal.
#[derive(Debug, Clone, Copy)]
pub enum TokenMode {
    /// Byte-for-byte (well, char-for-char) token equality.
    Exact,
    /// Unicode-case-folded equality (`str::to_lowercase`).
    CaseInsensitive,
    /// Numeric tokens compare within tolerance; non-numeric compare exactly.
    Float { abs_tol: f64, rel_tol: f64 },
}

/// Splits a [`CharStream`] into whitespace-delimited tokens. A "token" is a
/// maximal run of non-whitespace `char`s; whitespace is anything Rust's
/// `char::is_whitespace` (Unicode) reports - matching `str::split_whitespace`,
/// which is what the WASM comparer uses (`util::tokenize`). Leading, trailing,
/// and repeated whitespace produce no empty tokens.
struct TokenStream<R: Read> {
    chars: CharStream<R>,
    partial: String,
}

impl<R: Read> TokenStream<R> {
    fn new(reader: R) -> Self {
        Self {
            chars: CharStream::new(reader),
            partial: String::new(),
        }
    }

    fn next_token(&mut self) -> std::io::Result<Option<String>> {
        while let Some(ch) = self.chars.next_char()? {
            if ch.is_whitespace() {
                if !self.partial.is_empty() {
                    return Ok(Some(std::mem::take(&mut self.partial)));
                }
            } else {
                self.partial.push(ch);
            }
        }
        if self.partial.is_empty() {
            Ok(None)
        } else {
            Ok(Some(std::mem::take(&mut self.partial)))
        }
    }
}

fn tokens_match(expected: &str, actual: &str, mode: &TokenMode) -> bool {
    match mode {
        TokenMode::Exact => expected == actual,
        TokenMode::CaseInsensitive => expected.to_lowercase() == actual.to_lowercase(),
        TokenMode::Float { abs_tol, rel_tol } => {
            match (expected.parse::<f64>(), actual.parse::<f64>()) {
                (Ok(exp_f), Ok(act_f)) => {
                    if !exp_f.is_finite() || !act_f.is_finite() {
                        // Both must be the same non-finite value (NaN==NaN, or
                        // equal infinities).
                        (exp_f.is_nan() && act_f.is_nan()) || exp_f == act_f
                    } else {
                        let diff = (exp_f - act_f).abs();
                        let tolerance = abs_tol.max(rel_tol * exp_f.abs().max(act_f.abs()));
                        diff <= tolerance
                    }
                }
                // At least one token is not numeric - compare as strings.
                _ => expected == actual,
            }
        }
    }
}

/// Compare `output` and `answer` as whitespace-delimited token sequences using
/// `mode`. Returns [`Verdict::Accepted`] iff the two sequences have equal length
/// and every paired token matches; otherwise [`Verdict::WrongAnswer`].
///
/// Only AC/WA are ever returned. `output` is borrowed so the
/// caller keeps the reader (and can drain any unread bytes after an early WA).
pub fn compare_tokens<O: Read, A: Read>(
    output: &mut O,
    answer: A,
    mode: TokenMode,
) -> std::io::Result<Verdict> {
    let mut out = TokenStream::new(output);
    let mut ans = TokenStream::new(answer);

    loop {
        // Invalid UTF-8 in the SOLUTION output cannot match a (valid UTF-8)
        // answer, so it is a WrongAnswer - NOT a comparator IO error. A
        // comparator error exits 64, which the interpreter maps to a checker
        // SystemError and would wrongly reject a legal submission (invariant
        // violation). The ANSWER side keeps `?`: a non-UTF-8 jury answer is a
        // problem-setup bug and must stay a hard error (-> SystemError).
        let o = match out.next_token() {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(Verdict::WrongAnswer);
            }
            Err(e) => return Err(e),
        };
        let a = ans.next_token()?;
        match (o, a) {
            (Some(o), Some(a)) => {
                if !tokens_match(&a, &o, &mode) {
                    return Ok(Verdict::WrongAnswer);
                }
            }
            (None, None) => return Ok(Verdict::Accepted),
            // One side has more tokens than the other -> count mismatch -> WA.
            (Some(_), None) | (None, Some(_)) => return Ok(Verdict::WrongAnswer),
        }
    }
}

// ---------------------------------------------------------------------------
// Line comparison (lines)
// ---------------------------------------------------------------------------

/// Yields logical lines from a [`CharStream`] with the same normalization as
/// `util::split_lines_trimmed`:
///
/// - lines are split on `\n`; a `\r` immediately before the `\n` is dropped
///   (CRLF handling, matching `str::lines`),
/// - each line has trailing whitespace removed (`str::trim_end`, Unicode),
/// - empty lines are only emitted when a later non-empty line follows, which
///   has the effect of dropping all *trailing* empty lines while preserving
///   internal blank lines.
struct LineStream<R: Read> {
    chars: CharStream<R>,
    current: String,
    /// Count of trimmed-empty lines seen but not yet confirmed internal.
    pending_empty: usize,
    /// Count of confirmed-internal empty lines still to be emitted, one at a time.
    /// Held as a COUNTER (not materialized String entries) so a contestant cannot
    /// OOM the checker by emitting a huge run of blank lines before a non-empty
    /// one.
    flush_empty: usize,
    /// Lines ready to be handed out.
    queued: std::collections::VecDeque<String>,
}

impl<R: Read> LineStream<R> {
    fn new(reader: R) -> Self {
        Self {
            chars: CharStream::new(reader),
            current: String::new(),
            pending_empty: 0,
            flush_empty: 0,
            queued: std::collections::VecDeque::new(),
        }
    }

    fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            // Emit confirmed-internal blank lines one at a time from the counter,
            // BEFORE the buffered non-empty line, without ever materializing all
            // of them (a run of N blanks costs O(1) memory, not O(N) Strings).
            if self.flush_empty > 0 {
                self.flush_empty -= 1;
                return Ok(Some(String::new()));
            }
            if let Some(line) = self.queued.pop_front() {
                return Ok(Some(line));
            }
            match self.next_raw_line()? {
                Some(line) => {
                    let line = line.trim_end().to_string();
                    if line.is_empty() {
                        self.pending_empty += 1;
                        continue;
                    }
                    // A non-empty line confirms the buffered blanks were internal:
                    // schedule them as a COUNT to be emitted before this line.
                    self.flush_empty = self.pending_empty;
                    self.pending_empty = 0;
                    self.queued.push_back(line);
                }
                None => {
                    // Drop any trailing empty lines by discarding pending_empty.
                    self.pending_empty = 0;
                    return Ok(None);
                }
            }
        }
    }

    fn next_raw_line(&mut self) -> std::io::Result<Option<String>> {
        while let Some(ch) = self.chars.next_char()? {
            if ch == '\n' {
                if self.current.ends_with('\r') {
                    self.current.pop();
                }
                return Ok(Some(std::mem::take(&mut self.current)));
            }
            self.current.push(ch);
        }
        if self.current.is_empty() {
            Ok(None)
        } else {
            Ok(Some(std::mem::take(&mut self.current)))
        }
    }
}

/// Compare `output` and `answer` line by line. Trailing per-line whitespace is
/// ignored, CRLF is
/// normalized to LF, and trailing empty lines are dropped. Returns AC iff both
/// have the same number of normalized lines and every pair is byte-equal.
/// `output` is borrowed so the caller keeps the reader.
pub fn compare_lines<O: Read, A: Read>(output: &mut O, answer: A) -> std::io::Result<Verdict> {
    let mut out = LineStream::new(output);
    let mut ans = LineStream::new(answer);

    loop {
        // Output-side invalid UTF-8 -> WrongAnswer (see `compare_tokens`); the
        // answer side keeps `?` so a broken jury answer stays a hard error.
        let o = match out.next_line() {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(Verdict::WrongAnswer);
            }
            Err(e) => return Err(e),
        };
        let a = ans.next_line()?;
        match (o, a) {
            (Some(o), Some(a)) => {
                if o != a {
                    return Ok(Verdict::WrongAnswer);
                }
            }
            (None, None) => return Ok(Verdict::Accepted),
            (Some(_), None) | (None, Some(_)) => return Ok(Verdict::WrongAnswer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(output: &[u8], answer: &[u8]) -> Verdict {
        let mut output = output;
        compare_exact(&mut output, answer).unwrap()
    }

    #[test]
    fn identical_is_ac() {
        assert_eq!(exact(b"hello\n", b"hello\n"), Verdict::Accepted);
    }

    #[test]
    fn differing_is_wa() {
        assert_eq!(exact(b"hello\n", b"world\n"), Verdict::WrongAnswer);
    }

    #[test]
    fn trailing_newline_only_in_output_is_wa() {
        assert_eq!(exact(b"42\n", b"42"), Verdict::WrongAnswer);
    }

    #[test]
    fn trailing_newline_only_in_answer_is_wa() {
        assert_eq!(exact(b"42", b"42\n"), Verdict::WrongAnswer);
    }

    #[test]
    fn trailing_space_is_significant() {
        assert_eq!(exact(b"42 \n", b"42\n"), Verdict::WrongAnswer);
    }

    #[test]
    fn both_empty_is_ac() {
        assert_eq!(exact(b"", b""), Verdict::Accepted);
    }

    #[test]
    fn empty_vs_nonempty_is_wa() {
        assert_eq!(exact(b"", b"hello"), Verdict::WrongAnswer);
    }

    /// A `Read` that hands out at most one byte per call, to exercise the
    /// carry-over logic across asymmetric chunk boundaries.
    struct OneByteAtATime<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Read for OneByteAtATime<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.bytes.len() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.bytes[self.pos];
            self.pos += 1;
            Ok(1)
        }
    }

    #[test]
    fn detects_mismatch_across_chunk_boundaries() {
        let mut out = OneByteAtATime {
            bytes: b"42\n",
            pos: 0,
        };
        let ans = OneByteAtATime {
            bytes: b"42",
            pos: 0,
        };
        assert_eq!(compare_exact(&mut out, ans).unwrap(), Verdict::WrongAnswer);
    }

    // -----------------------------------------------------------------------
    // tokens
    // -----------------------------------------------------------------------

    fn tokens(output: &[u8], answer: &[u8]) -> Verdict {
        let mut output = output;
        compare_tokens(&mut output, answer, TokenMode::Exact).unwrap()
    }

    #[test]
    fn tokens_different_whitespace_same_tokens() {
        assert_eq!(tokens(b"  1   2  3  \n", b"1 2 3"), Verdict::Accepted);
    }

    #[test]
    fn tokens_on_different_lines() {
        assert_eq!(tokens(b"1\n2\n3\n", b"1 2 3"), Verdict::Accepted);
    }

    #[test]
    fn tokens_count_mismatch_is_wa() {
        // The output has fewer tokens.
        assert_eq!(tokens(b"1 2", b"1 2 3"), Verdict::WrongAnswer);
    }

    #[test]
    fn tokens_value_mismatch_is_wa() {
        assert_eq!(tokens(b"1 X 3", b"1 2 3"), Verdict::WrongAnswer);
    }

    #[test]
    fn tokens_empty_vs_nonempty_is_wa() {
        assert_eq!(tokens(b"", b"hello"), Verdict::WrongAnswer);
    }

    #[test]
    fn tokens_trailing_newline_tolerant() {
        // A trailing newline is just whitespace -> same token sequence -> AC.
        assert_eq!(tokens(b"42\n", b"42"), Verdict::Accepted);
    }

    #[test]
    fn tokens_both_empty_is_ac() {
        // No tokens on either side -> equal sequences -> AC.
        assert_eq!(tokens(b"", b""), Verdict::Accepted);
        assert_eq!(tokens(b"   \n\t ", b""), Verdict::Accepted);
    }

    #[test]
    fn tokens_accepts_large_chunked_output() {
        // Token sequences are equal regardless of which whitespace separates them.
        let expected = format!("{} tail", "123 ".repeat(100_000));
        let actual = format!("{} tail", "123\n".repeat(100_000));
        let mut actual_bytes = actual.as_bytes();
        assert_eq!(
            compare_tokens(&mut actual_bytes, expected.as_bytes(), TokenMode::Exact).unwrap(),
            Verdict::Accepted
        );
    }

    // -----------------------------------------------------------------------
    // tokens-case-insensitive
    // -----------------------------------------------------------------------

    fn tokens_ci(output: &[u8], answer: &[u8]) -> Verdict {
        let mut output = output;
        compare_tokens(&mut output, answer, TokenMode::CaseInsensitive).unwrap()
    }

    #[test]
    fn tokens_ci_yes_vs_yes() {
        assert_eq!(tokens_ci(b"YES\n", b"yes"), Verdict::Accepted);
    }

    #[test]
    fn tokens_ci_hello_world() {
        assert_eq!(
            tokens_ci(b"Hello World\n", b"hello world"),
            Verdict::Accepted
        );
    }

    #[test]
    fn tokens_ci_different_tokens_regardless_of_case() {
        assert_eq!(tokens_ci(b"abc\n", b"xyz"), Verdict::WrongAnswer);
    }

    #[test]
    fn tokens_ci_unicode() {
        assert_eq!(
            tokens_ci("Ü Ö Ä\n".as_bytes(), "ü ö ä".as_bytes()),
            Verdict::Accepted
        );
    }

    // -----------------------------------------------------------------------
    // lines
    // -----------------------------------------------------------------------

    fn lines(output: &[u8], answer: &[u8]) -> Verdict {
        let mut output = output;
        compare_lines(&mut output, answer).unwrap()
    }

    #[test]
    fn lines_trailing_whitespace_per_line_ignored() {
        assert_eq!(
            lines(b"hello  \nworld  \n", b"hello\nworld\n"),
            Verdict::Accepted
        );
    }

    #[test]
    fn lines_trailing_empty_lines_ignored() {
        assert_eq!(lines(b"hello\n\n\n", b"hello\n"), Verdict::Accepted);
    }

    #[test]
    fn lines_internal_whitespace_preserved() {
        assert_eq!(
            lines(b"hello  world\n", b"hello world\n"),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn lines_different_line_count_is_wa() {
        assert_eq!(lines(b"a\nb\n", b"a\nb\nc\n"), Verdict::WrongAnswer);
    }

    #[test]
    fn lines_first_diff_line_is_wa() {
        assert_eq!(lines(b"a\nX\nc\n", b"a\nb\nc\n"), Verdict::WrongAnswer);
    }

    #[test]
    fn lines_preserves_internal_empty_lines_but_drops_trailing() {
        assert_eq!(lines(b"a\n\nb\n\n", b"a\n\nb\n"), Verdict::Accepted);
    }

    #[test]
    fn lines_crlf_normalized() {
        // str::lines drops the \r before \n; CRLF output matches LF answer.
        assert_eq!(lines(b"a\r\nb\r\n", b"a\nb\n"), Verdict::Accepted);
    }

    #[test]
    fn lines_both_empty_is_ac() {
        assert_eq!(lines(b"", b""), Verdict::Accepted);
        assert_eq!(lines(b"\n\n", b""), Verdict::Accepted);
    }

    // -----------------------------------------------------------------------
    // tokens-float
    // -----------------------------------------------------------------------

    const ABS: f64 = 1e-9;
    const REL: f64 = 1e-6;

    fn float_default(output: &[u8], answer: &[u8]) -> Verdict {
        let mut output = output;
        compare_tokens(
            &mut output,
            answer,
            TokenMode::Float {
                abs_tol: ABS,
                rel_tol: REL,
            },
        )
        .unwrap()
    }

    #[test]
    fn float_exact_match() {
        assert_eq!(float_default(b"3.14159", b"3.14159"), Verdict::Accepted);
    }

    #[test]
    fn float_within_abs_tolerance() {
        // Default abs_tol = 1e-9.
        assert_eq!(
            float_default(b"1.0000000001", b"1.0000000000"),
            Verdict::Accepted
        );
    }

    #[test]
    fn float_within_rel_tolerance() {
        assert_eq!(float_default(b"1000.0005", b"1000.0000"), Verdict::Accepted);
    }

    #[test]
    fn float_outside_both_tolerances() {
        assert_eq!(float_default(b"1.0", b"2.0"), Verdict::WrongAnswer);
    }

    #[test]
    fn float_nan_vs_nan() {
        assert_eq!(float_default(b"NaN", b"NaN"), Verdict::Accepted);
    }

    #[test]
    fn float_inf_vs_inf() {
        assert_eq!(float_default(b"inf", b"inf"), Verdict::Accepted);
    }

    #[test]
    fn float_mixed_float_string_tokens() {
        assert_eq!(float_default(b"YES 3.14", b"YES 3.14"), Verdict::Accepted);
    }

    #[test]
    fn float_non_numeric_mismatch_is_wa() {
        // Non-numeric tokens fall through to string compare.
        assert_eq!(float_default(b"YES", b"NO"), Verdict::WrongAnswer);
    }

    #[test]
    fn float_custom_abs_tol_overrides() {
        // With abs_tol = 1.0 a diff of 0.5 is accepted. (The CLI's --epsilon maps to
        // abs_tol; rel_tol stays at its default.)
        let mut output = b"1.5".as_slice();
        let v = compare_tokens(
            &mut output,
            b"1.0".as_slice(),
            TokenMode::Float {
                abs_tol: 1.0,
                rel_tol: REL,
            },
        )
        .unwrap();
        assert_eq!(v, Verdict::Accepted);
    }

    #[test]
    fn float_count_mismatch_is_wa() {
        assert_eq!(float_default(b"1.0 2.0", b"1.0"), Verdict::WrongAnswer);
    }

    // -----------------------------------------------------------------------
    // exact - asymmetric chunk carry-over. compare_exact's whole reason for the
    // out_lo/out_hi vs ans_lo/ans_hi index bookkeeping is to compare two readers
    // that yield DIFFERENT amounts per read; feed it exactly that.
    // -----------------------------------------------------------------------

    /// A `Read` that hands out at most `chunk` bytes per call.
    struct ChunkedReader<'a> {
        bytes: &'a [u8],
        pos: usize,
        chunk: usize,
    }

    impl Read for ChunkedReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let remaining = &self.bytes[self.pos..];
            let n = remaining.len().min(self.chunk).min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn exact_asymmetric_chunks_identical_is_ac() {
        // output 3 bytes/read, answer 2 bytes/read; identical content -> AC.
        let mut out = ChunkedReader {
            bytes: b"abcdefg",
            pos: 0,
            chunk: 3,
        };
        let ans = ChunkedReader {
            bytes: b"abcdefg",
            pos: 0,
            chunk: 2,
        };
        assert_eq!(compare_exact(&mut out, ans).unwrap(), Verdict::Accepted);
    }

    #[test]
    fn exact_asymmetric_chunks_interior_diff_is_wa() {
        // Differ at an interior byte that straddles the asymmetric boundaries.
        let mut out = ChunkedReader {
            bytes: b"abcXefg",
            pos: 0,
            chunk: 3,
        };
        let ans = ChunkedReader {
            bytes: b"abcdefg",
            pos: 0,
            chunk: 2,
        };
        assert_eq!(compare_exact(&mut out, ans).unwrap(), Verdict::WrongAnswer);
    }

    #[test]
    fn exact_asymmetric_chunks_length_diff_is_wa() {
        // Identical prefix, output longer -> exercises the one-side-empty path.
        let mut out = ChunkedReader {
            bytes: b"abcdefg",
            pos: 0,
            chunk: 3,
        };
        let ans = ChunkedReader {
            bytes: b"abcde",
            pos: 0,
            chunk: 2,
        };
        assert_eq!(compare_exact(&mut out, ans).unwrap(), Verdict::WrongAnswer);
    }

    // -----------------------------------------------------------------------
    // Multi-byte UTF-8 split across a read boundary. OneByteAtATime forces
    // CharStream to assemble each multi-byte code point from its partial-prefix
    // branch (fill() Err/valid_up_to path) - the path real piped stdin hits for
    // any non-ASCII output.
    // -----------------------------------------------------------------------

    #[test]
    fn tokens_multibyte_utf8_split_across_reads_is_ac() {
        let mut out = OneByteAtATime {
            bytes: "café α β\n".as_bytes(),
            pos: 0,
        };
        assert_eq!(
            compare_tokens(&mut out, "café α β".as_bytes(), TokenMode::Exact).unwrap(),
            Verdict::Accepted
        );
    }

    #[test]
    fn tokens_multibyte_utf8_split_mismatch_is_wa() {
        let mut out = OneByteAtATime {
            bytes: "café α β\n".as_bytes(),
            pos: 0,
        };
        assert_eq!(
            compare_tokens(&mut out, "café α γ".as_bytes(), TokenMode::Exact).unwrap(),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn tokens_ci_multibyte_utf8_split_is_ac() {
        // Case-fold across split reads: "CAFÉ Ω" lowercases to "café ω".
        let mut out = OneByteAtATime {
            bytes: "CAFÉ Ω\n".as_bytes(),
            pos: 0,
        };
        assert_eq!(
            compare_tokens(&mut out, "café ω".as_bytes(), TokenMode::CaseInsensitive).unwrap(),
            Verdict::Accepted
        );
    }

    #[test]
    fn lines_multibyte_utf8_split_across_reads_is_ac() {
        let mut out = OneByteAtATime {
            bytes: "café\nα β\n".as_bytes(),
            pos: 0,
        };
        assert_eq!(
            compare_lines(&mut out, "café\nα β\n".as_bytes()).unwrap(),
            Verdict::Accepted
        );
    }

    // -----------------------------------------------------------------------
    // float accept/reject boundary - pins the `<=` at the tolerance edge and the
    // max(abs_tol, rel_tol*max(|a|,|b|)) formula in both the abs- and rel-driven
    // regimes (all values exactly representable in f64).
    // -----------------------------------------------------------------------

    fn float_tol(output: &[u8], answer: &[u8], abs_tol: f64, rel_tol: f64) -> Verdict {
        let mut output = output;
        compare_tokens(&mut output, answer, TokenMode::Float { abs_tol, rel_tol }).unwrap()
    }

    #[test]
    fn float_diff_exactly_abs_tol_is_ac() {
        // diff |1.5-1.0| = 0.5 == abs_tol 0.5 -> AC (the `<=` boundary).
        assert_eq!(float_tol(b"1.5", b"1.0", 0.5, 0.0), Verdict::Accepted);
    }

    #[test]
    fn float_diff_just_over_abs_tol_is_wa() {
        // diff 0.5 > abs_tol 0.25 -> WA.
        assert_eq!(float_tol(b"1.5", b"1.0", 0.25, 0.0), Verdict::WrongAnswer);
    }

    #[test]
    fn float_rel_tol_regime_is_ac() {
        // abs_tol 0; tol = rel_tol*max(|a|,|b|) = 1e-3*1001 = 1.001; diff 1.0 -> AC.
        assert_eq!(
            float_tol(b"1001.0", b"1000.0", 0.0, 1e-3),
            Verdict::Accepted
        );
    }

    #[test]
    fn float_rel_tol_just_over_is_wa() {
        // tol = 1e-4*1000.5 = 0.10005; diff 0.5 -> WA.
        assert_eq!(
            float_tol(b"1000.5", b"1000.0", 0.0, 1e-4),
            Verdict::WrongAnswer
        );
    }

    // -----------------------------------------------------------------------
    // float - signed and overflow-to-infinity tokens (sign/.abs() handling and
    // the non-finite branch for parse-overflow inputs).
    // -----------------------------------------------------------------------

    #[test]
    fn float_negative_tokens() {
        assert_eq!(float_default(b"-3.5", b"-3.5"), Verdict::Accepted);
        assert_eq!(float_default(b"-3.5", b"3.5"), Verdict::WrongAnswer);
    }

    #[test]
    fn float_signed_zero_is_ac() {
        // -0.0 == 0.0 in f64 -> diff 0 -> AC.
        assert_eq!(float_default(b"-0.0", b"0.0"), Verdict::Accepted);
    }

    #[test]
    fn float_overflow_parses_to_infinity() {
        // "1e400" overflows f64 to +inf in Rust's FromStr (Ok, not Err).
        assert_eq!(float_default(b"1e400", b"1e400"), Verdict::Accepted);
        assert_eq!(float_default(b"1e400", b"1.0"), Verdict::WrongAnswer);
    }

    // -----------------------------------------------------------------------
    // lines - lone CR (not a separator, matching str::lines) and trailing
    // Unicode whitespace trimming.
    // -----------------------------------------------------------------------

    #[test]
    fn lines_lone_cr_is_interior_not_a_separator() {
        // Only '\n' splits; a bare '\r' stays inside the line. A regression
        // treating '\r' as a separator would change the line count and flip this.
        assert_eq!(lines(b"a\rb\n", b"a\rb\n"), Verdict::Accepted);
        assert_eq!(lines(b"a\rb\n", b"a\nb\n"), Verdict::WrongAnswer);
    }

    #[test]
    fn lines_trailing_unicode_whitespace_trimmed() {
        // trim_end strips Unicode White_Space: a trailing ideographic space
        // (U+3000) is removed, matching the answer without it.
        assert_eq!(
            lines("hello\u{3000}\n".as_bytes(), b"hello\n"),
            Verdict::Accepted
        );
    }

    // -----------------------------------------------------------------------
    // exact - multi-byte content (byte-level identity over non-ASCII).
    // -----------------------------------------------------------------------

    #[test]
    fn exact_multibyte_unicode_ac_and_wa() {
        assert_eq!(
            exact("café α\n".as_bytes(), "café α\n".as_bytes()),
            Verdict::Accepted
        );
        assert_eq!(
            exact("café α\n".as_bytes(), "café β\n".as_bytes()),
            Verdict::WrongAnswer
        );
    }

    // -----------------------------------------------------------------------
    // Invalid UTF-8 on the OUTPUT side is the contestant's fault: their output
    // cannot match a (valid UTF-8) answer, so it is a WrongAnswer - NOT a
    // comparator IO error. The interpreter maps a comparator IO error (exit 64)
    // to a checker SystemError, which would wrongly reject a legal submission
    // (invariant violation). The ANSWER side staying a hard error preserves
    // setup-bug detection (a non-UTF-8 jury answer -> SystemError, author's bug).
    // exact mode is byte-level and already tolerates arbitrary bytes.
    // -----------------------------------------------------------------------

    #[test]
    fn tokens_invalid_utf8_output_is_wa() {
        let mut out = b"\xff\xfe\xfd".as_slice();
        assert_eq!(
            compare_tokens(&mut out, b"5".as_slice(), TokenMode::Exact).unwrap(),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn tokens_ci_invalid_utf8_output_is_wa() {
        let mut out = b"\xff\xfe".as_slice();
        assert_eq!(
            compare_tokens(&mut out, b"yes".as_slice(), TokenMode::CaseInsensitive).unwrap(),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn tokens_float_invalid_utf8_output_is_wa() {
        let mut out = b"\xff\xfe".as_slice();
        assert_eq!(
            compare_tokens(
                &mut out,
                b"1.0".as_slice(),
                TokenMode::Float {
                    abs_tol: ABS,
                    rel_tol: REL
                }
            )
            .unwrap(),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn lines_invalid_utf8_output_is_wa() {
        let mut out = b"\xff\xfe".as_slice();
        assert_eq!(
            compare_lines(&mut out, b"hello".as_slice()).unwrap(),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn tokens_partial_utf8_at_eof_output_is_wa() {
        // A truncated multi-byte sequence at EOF (lead byte, no continuation) is
        // incomplete -> not valid text -> WrongAnswer, not a comparator error.
        let mut out = b"caf\xc3".as_slice();
        assert_eq!(
            compare_tokens(&mut out, b"cafe".as_slice(), TokenMode::Exact).unwrap(),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn lines_invalid_utf8_output_is_wa_mid_stream() {
        // Invalid byte after a valid prefix is still WrongAnswer for the line mode.
        let mut out = b"hello\n\xff".as_slice();
        assert_eq!(
            compare_lines(&mut out, b"hello\n".as_slice()).unwrap(),
            Verdict::WrongAnswer
        );
    }

    #[test]
    fn tokens_invalid_utf8_answer_is_error() {
        // Author-side broken answer stays a hard error (interpreter -> SystemError).
        let mut out = b"5".as_slice();
        let r = compare_tokens(&mut out, b"\xff\xfe".as_slice(), TokenMode::Exact);
        assert!(
            r.is_err(),
            "answer-side invalid UTF-8 must remain an error, got {r:?}"
        );
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn lines_invalid_utf8_answer_is_error() {
        let mut out = b"hello\n".as_slice();
        let r = compare_lines(&mut out, b"\xff\xfe".as_slice());
        assert!(
            r.is_err(),
            "answer-side invalid UTF-8 must remain an error, got {r:?}"
        );
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }
}
