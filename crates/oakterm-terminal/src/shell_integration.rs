//! OSC 7 / OSC 133 interception.
//!
//! vte 0.15's `ansi::Handler` has no callbacks for OSC 133 (shell integration
//! semantic marks) or OSC 7 (working directory): its `osc_dispatch` routes both
//! to `unhandled()`. This scanner runs alongside the VT processor. It finds
//! complete OSC 7/133 sequences in the byte stream and splits each read at the
//! sequence terminator, so the caller can position the grid cursor — by feeding
//! all prior bytes to the processor — before attaching the mark to the row the
//! cursor lands on. Partial-sequence state persists across read chunks.
//!
//! Every byte is still fed to the VT processor unchanged; the OSC 7/133
//! payloads reach vte and are dropped as `unhandled`. The scanner only injects
//! mark events between the processor feeds — it never rewrites the stream.

/// OSC payload cap, matching the Spec-0002 OSC buffer limit. Payloads longer
/// than this are not decoded (the sequence still passes through to vte).
const MAX_OSC: usize = 4096;

/// A shell-integration event decoded from an OSC 7/133 sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellMark {
    /// OSC 133;A — prompt start.
    PromptStart,
    /// OSC 133;B — command input start.
    InputStart,
    /// OSC 133;C — command output start.
    OutputStart,
    /// OSC 133;D — command finished, with optional exit code.
    CommandFinished(Option<i32>),
    /// OSC 7 — current working directory (decoded filesystem path).
    WorkingDirectory(String),
}

/// One step in replaying a read chunk against the VT processor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanStep {
    /// Feed `chunk[start..end]` to the VT processor.
    Feed { start: usize, end: usize },
    /// Attach this mark to the row the cursor is on, then continue.
    Mark(ShellMark),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// Saw `ESC` in ground state.
    Esc,
    /// Inside an OSC payload.
    Osc,
    /// Saw `ESC` inside an OSC payload (potential ST terminator).
    OscEsc,
}

/// Stateful scanner for OSC 7/133 sequences. One per terminal parser, reused
/// across read chunks so sequences that span chunk boundaries are decoded.
#[derive(Debug, Default)]
pub struct ShellIntegrationScanner {
    state: State,
    /// Accumulated OSC payload, bounded by `MAX_OSC`.
    buf: Vec<u8>,
    /// The current payload exceeded `MAX_OSC`; stop decoding it.
    overflowed: bool,
}

impl ShellIntegrationScanner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all state, including any partial in-flight sequence. Called on RIS.
    pub fn reset(&mut self) {
        self.state = State::Ground;
        self.buf.clear();
        self.overflowed = false;
    }

    /// Scan a read chunk, returning the interleaved feed/mark steps to replay
    /// against the VT processor. `Feed` ranges cover the whole chunk in order
    /// with no gaps or overlaps; `Mark` steps are injected at OSC terminators.
    #[must_use]
    pub fn scan(&mut self, chunk: &[u8]) -> Vec<ScanStep> {
        let mut steps = Vec::new();
        let mut seg_start = 0usize;

        for (i, &b) in chunk.iter().enumerate() {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                    }
                }
                State::Esc => match b {
                    0x5d => {
                        self.state = State::Osc;
                        self.buf.clear();
                        self.overflowed = false;
                    }
                    0x1b => {}
                    _ => self.state = State::Ground,
                },
                State::Osc => match b {
                    0x07 => self.terminate(i, &mut seg_start, &mut steps),
                    // CAN/SUB cancel the sequence (vte aborts string controls on
                    // these); abort without decoding so no false mark is emitted.
                    0x18 | 0x1a => self.abort_osc(),
                    0x1b => self.state = State::OscEsc,
                    _ => self.push_byte(b),
                },
                State::OscEsc => match b {
                    0x5c => self.terminate(i, &mut seg_start, &mut steps),
                    0x1b => {
                        self.buf.clear();
                        self.overflowed = false;
                        self.state = State::Esc;
                    }
                    _ => self.abort_osc(),
                },
            }
        }

        if seg_start < chunk.len() {
            steps.push(ScanStep::Feed {
                start: seg_start,
                end: chunk.len(),
            });
        }
        steps
    }

    /// Handle an OSC terminator at index `end` (BEL) or the `\` of an ST.
    /// Emits a feed-through-terminator + mark pair only when the payload
    /// decodes to an intercepted mark; otherwise the bytes stay in the
    /// pending segment and pass through with the next feed.
    fn terminate(&mut self, end: usize, seg_start: &mut usize, steps: &mut Vec<ScanStep>) {
        if let Some(mark) = self.decode() {
            steps.push(ScanStep::Feed {
                start: *seg_start,
                end: end + 1,
            });
            steps.push(ScanStep::Mark(mark));
            *seg_start = end + 1;
        }
        self.buf.clear();
        self.overflowed = false;
        self.state = State::Ground;
    }

    /// Abandon the in-flight OSC payload without decoding it and return to
    /// ground. Used for CAN/SUB cancellation and malformed ESC sequences.
    fn abort_osc(&mut self) {
        self.buf.clear();
        self.overflowed = false;
        self.state = State::Ground;
    }

    fn push_byte(&mut self, b: u8) {
        if self.overflowed {
            return;
        }
        if self.buf.len() >= MAX_OSC {
            self.overflowed = true;
            tracing::debug!(cap = MAX_OSC, "OSC payload exceeded cap; not decoded");
            return;
        }
        self.buf.push(b);
    }

    /// Decode the accumulated payload into a mark, if it is an OSC 7 or 133
    /// sequence we intercept. Returns `None` for any other OSC code.
    fn decode(&self) -> Option<ShellMark> {
        if self.overflowed {
            return None;
        }
        let (code, rest) = split_once(&self.buf, b';');
        match code {
            b"133" => decode_133(rest),
            b"7" => decode_osc7(rest),
            _ => None,
        }
    }
}

/// OSC 133;`<letter>`[;`<params>`] → semantic mark.
fn decode_133(rest: &[u8]) -> Option<ShellMark> {
    let (letter, params) = split_once(rest, b';');
    match letter {
        b"A" => Some(ShellMark::PromptStart),
        b"B" => Some(ShellMark::InputStart),
        b"C" => Some(ShellMark::OutputStart),
        b"D" => Some(ShellMark::CommandFinished(parse_exit_code(params))),
        _ => None,
    }
}

/// The exit code in `OSC 133;D;<code>` is the first `;`-delimited field after
/// `D`. Absent or non-numeric fields yield `None` (command finished, status
/// unknown).
fn parse_exit_code(params: &[u8]) -> Option<i32> {
    let (field, _) = split_once(params, b';');
    if field.is_empty() {
        return None;
    }
    std::str::from_utf8(field).ok()?.trim().parse::<i32>().ok()
}

/// OSC 7;`file://<host>/<path>` → working directory. The path is the URI
/// component after the authority, percent-decoded. A non-`file://` scheme,
/// non-UTF-8 payload, or empty path yields `None` so live cwd keeps its last
/// known value rather than adopting a meaningless one.
fn decode_osc7(rest: &[u8]) -> Option<ShellMark> {
    let Ok(uri) = std::str::from_utf8(rest) else {
        tracing::debug!(len = rest.len(), "OSC 7 payload not UTF-8; cwd unchanged");
        return None;
    };
    let Some(after) = uri.strip_prefix("file://") else {
        tracing::debug!(uri, "OSC 7 payload missing file:// scheme; cwd unchanged");
        return None;
    };
    let path_start = after.find('/').unwrap_or(after.len());
    let path = percent_decode(&after[path_start..]);
    if path.is_empty() {
        tracing::debug!(uri, "OSC 7 has empty path; cwd unchanged");
        return None;
    }
    Some(ShellMark::WorkingDirectory(path))
}

/// When `sep` is absent, `after` is empty.
fn split_once(buf: &[u8], sep: u8) -> (&[u8], &[u8]) {
    match buf.iter().position(|&c| c == sep) {
        Some(p) => (&buf[..p], &buf[p + 1..]),
        None => (buf, &[]),
    }
}

/// Invalid escapes are left verbatim; the decoded bytes are interpreted as
/// UTF-8 (lossily, to tolerate exotic filesystems).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a full byte stream through the scanner, returning just the decoded
    /// marks (feed ranges are asserted separately where they matter).
    fn marks(chunks: &[&[u8]]) -> Vec<ShellMark> {
        let mut scanner = ShellIntegrationScanner::new();
        let mut out = Vec::new();
        for chunk in chunks {
            for step in scanner.scan(chunk) {
                if let ScanStep::Mark(m) = step {
                    out.push(m);
                }
            }
        }
        out
    }

    #[test]
    fn osc_133_a_bel_terminated() {
        assert_eq!(marks(&[b"\x1b]133;A\x07"]), vec![ShellMark::PromptStart]);
    }

    #[test]
    fn osc_133_a_st_terminated() {
        assert_eq!(marks(&[b"\x1b]133;A\x1b\\"]), vec![ShellMark::PromptStart]);
    }

    #[test]
    fn osc_133_all_variants() {
        assert_eq!(marks(&[b"\x1b]133;B\x07"]), vec![ShellMark::InputStart]);
        assert_eq!(marks(&[b"\x1b]133;C\x07"]), vec![ShellMark::OutputStart]);
        assert_eq!(
            marks(&[b"\x1b]133;D\x07"]),
            vec![ShellMark::CommandFinished(None)]
        );
    }

    #[test]
    fn osc_133_d_with_exit_code() {
        assert_eq!(
            marks(&[b"\x1b]133;D;0\x07"]),
            vec![ShellMark::CommandFinished(Some(0))]
        );
        assert_eq!(
            marks(&[b"\x1b]133;D;130\x07"]),
            vec![ShellMark::CommandFinished(Some(130))]
        );
    }

    #[test]
    fn osc_133_d_trailing_params_after_exit_code() {
        assert_eq!(
            marks(&[b"\x1b]133;D;1;aid=7\x07"]),
            vec![ShellMark::CommandFinished(Some(1))]
        );
    }

    #[test]
    fn osc_133_a_with_extra_params_ignored() {
        assert_eq!(
            marks(&[b"\x1b]133;A;aid=42\x07"]),
            vec![ShellMark::PromptStart]
        );
    }

    #[test]
    fn osc_7_absolute_path() {
        assert_eq!(
            marks(&[b"\x1b]7;file://host/home/jace\x07"]),
            vec![ShellMark::WorkingDirectory("/home/jace".into())]
        );
    }

    #[test]
    fn osc_7_empty_host() {
        assert_eq!(
            marks(&[b"\x1b]7;file:///var/log\x07"]),
            vec![ShellMark::WorkingDirectory("/var/log".into())]
        );
    }

    #[test]
    fn osc_7_percent_decoded() {
        assert_eq!(
            marks(&[b"\x1b]7;file://host/my%20dir/a%2Bb\x07"]),
            vec![ShellMark::WorkingDirectory("/my dir/a+b".into())]
        );
    }

    #[test]
    fn osc_1337_not_confused_with_133() {
        // iTerm2 proprietary OSC 1337 must not be decoded as OSC 133.
        assert_eq!(marks(&[b"\x1b]1337;SetMark\x07"]), vec![]);
    }

    #[test]
    fn osc_70_not_confused_with_7() {
        assert_eq!(marks(&[b"\x1b]70;something\x07"]), vec![]);
    }

    #[test]
    fn unrelated_osc_ignored() {
        // OSC 0 (title) and OSC 8 (hyperlink) produce no marks.
        assert_eq!(marks(&[b"\x1b]0;my title\x07"]), vec![]);
        assert_eq!(
            marks(&[b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07"]),
            vec![]
        );
    }

    #[test]
    fn split_across_chunks() {
        assert_eq!(
            marks(&[b"\x1b]133", b";A\x07"]),
            vec![ShellMark::PromptStart]
        );
        assert_eq!(
            marks(&[b"\x1b]7;file://h/a", b"/b\x07"]),
            vec![ShellMark::WorkingDirectory("/a/b".into())]
        );
    }

    #[test]
    fn st_terminator_split_across_chunks() {
        assert_eq!(
            marks(&[b"\x1b]133;A\x1b", b"\\"]),
            vec![ShellMark::PromptStart]
        );
    }

    #[test]
    fn multiple_marks_in_one_chunk() {
        assert_eq!(
            marks(&[b"\x1b]133;A\x07prompt\x1b]133;B\x07"]),
            vec![ShellMark::PromptStart, ShellMark::InputStart]
        );
    }

    #[test]
    fn feed_ranges_cover_chunk_without_gaps() {
        let mut scanner = ShellIntegrationScanner::new();
        let chunk = b"ab\x1b]133;A\x07cd";
        let steps = scanner.scan(chunk);
        // Reassemble the fed bytes; they must equal the whole chunk in order.
        let mut reassembled = Vec::new();
        for step in &steps {
            if let ScanStep::Feed { start, end } = step {
                reassembled.extend_from_slice(&chunk[*start..*end]);
            }
        }
        assert_eq!(reassembled, chunk);
        assert!(steps.iter().any(|s| matches!(s, ScanStep::Mark(_))));
    }

    #[test]
    fn mark_injected_after_terminator() {
        let mut scanner = ShellIntegrationScanner::new();
        let steps = scanner.scan(b"\x1b]133;A\x07x");
        // Order: feed through the BEL, then the mark, then feed the tail.
        assert_eq!(
            steps,
            vec![
                ScanStep::Feed { start: 0, end: 8 },
                ScanStep::Mark(ShellMark::PromptStart),
                ScanStep::Feed { start: 8, end: 9 },
            ]
        );
    }

    #[test]
    fn oversized_payload_not_decoded() {
        let mut seq = Vec::from(*b"\x1b]7;file://host/");
        seq.extend(std::iter::repeat_n(b'a', MAX_OSC + 10));
        seq.push(0x07);
        assert_eq!(marks(&[&seq]), vec![]);
    }

    #[test]
    fn reset_clears_partial_sequence() {
        let mut scanner = ShellIntegrationScanner::new();
        let _ = scanner.scan(b"\x1b]133;A"); // partial, no terminator
        scanner.reset();
        // The dangling terminator must not complete the abandoned sequence.
        let steps = scanner.scan(b"\x07");
        assert!(steps.iter().all(|s| !matches!(s, ScanStep::Mark(_))));
    }

    #[test]
    fn no_osc_is_single_feed() {
        let mut scanner = ShellIntegrationScanner::new();
        assert_eq!(
            scanner.scan(b"plain text"),
            vec![ScanStep::Feed { start: 0, end: 10 }]
        );
    }

    #[test]
    fn scanner_recovers_after_oversized_payload() {
        // An oversized OSC must not poison the scanner for subsequent marks.
        let mut scanner = ShellIntegrationScanner::new();
        let mut big = Vec::from(*b"\x1b]0;");
        big.extend(std::iter::repeat_n(b'x', MAX_OSC + 10));
        big.push(0x07);
        let _ = scanner.scan(&big);
        let mut out = Vec::new();
        for step in scanner.scan(b"\x1b]133;A\x07") {
            if let ScanStep::Mark(m) = step {
                out.push(m);
            }
        }
        assert_eq!(out, vec![ShellMark::PromptStart]);
    }

    #[test]
    fn can_cancels_osc_without_emitting_mark() {
        // CAN mid-payload aborts; the following real sequence still decodes.
        assert_eq!(
            marks(&[b"\x1b]133;A\x18\x1b]133;B\x07"]),
            vec![ShellMark::InputStart]
        );
    }

    #[test]
    fn sub_cancels_osc_without_emitting_mark() {
        assert_eq!(marks(&[b"\x1b]133;A\x1a\x07"]), vec![]);
    }

    #[test]
    fn esc_inside_osc_aborts_then_next_sequence_decodes() {
        // ESC followed by a non-ST byte aborts the current OSC; the subsequent
        // OSC 133;A must still be found.
        assert_eq!(
            marks(&[b"\x1b]0;title\x1bX\x1b]133;A\x07"]),
            vec![ShellMark::PromptStart]
        );
    }

    #[test]
    fn exit_code_non_numeric_is_none() {
        assert_eq!(
            marks(&[b"\x1b]133;D;abc\x07"]),
            vec![ShellMark::CommandFinished(None)]
        );
        assert_eq!(
            marks(&[b"\x1b]133;D;\x07"]),
            vec![ShellMark::CommandFinished(None)]
        );
    }

    #[test]
    fn osc_7_non_file_scheme_ignored() {
        assert_eq!(marks(&[b"\x1b]7;http://host/x\x07"]), vec![]);
    }

    #[test]
    fn osc_7_empty_path_ignored() {
        // `file://host` with no path component carries no usable cwd.
        assert_eq!(marks(&[b"\x1b]7;file://host\x07"]), vec![]);
    }

    #[test]
    fn percent_decode_invalid_escape_left_verbatim() {
        assert_eq!(
            marks(&[b"\x1b]7;file://h/a%zzb\x07"]),
            vec![ShellMark::WorkingDirectory("/a%zzb".into())]
        );
        // A truncated `%2` at the end is preserved rather than dropped.
        assert_eq!(
            marks(&[b"\x1b]7;file://h/dir%2\x07"]),
            vec![ShellMark::WorkingDirectory("/dir%2".into())]
        );
    }

    #[test]
    fn percent_decode_invalid_utf8_is_lossy() {
        // `%ff` decodes to a byte that is not valid UTF-8; it becomes U+FFFD
        // rather than dropping the mark.
        let marks = marks(&[b"\x1b]7;file://h/a%ffb\x07"]);
        assert_eq!(marks.len(), 1);
        let ShellMark::WorkingDirectory(path) = &marks[0] else {
            panic!("expected WorkingDirectory");
        };
        assert!(path.starts_with("/a") && path.ends_with('b') && path.contains('\u{fffd}'));
    }
}
