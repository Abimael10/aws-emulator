// SPDX-License-Identifier: CC0-1.0

//! The STOMP frame format: what is on the wire in both directions, and nothing about what
//! a frame MEANS (that is [`super::stomp_session`]'s).
//!
//! A frame is a command line, a header line per header, an empty line, the body, and a NUL
//! byte — every line ended by a line feed, which a client may precede with a carriage
//! return. Between frames a client may send a bare end of line as a heart-beat, which is
//! not a frame at all ([`skip_interframe`]).
//!
//! From STOMP 1.1 on, a header name and value carry the protocol's own escapes
//! (`\n`, `\c`, `\r`, `\\`) so that a value may hold a line feed or a colon; the CONNECT
//! and CONNECTED frames are exempt, and STOMP 1.0 has no escapes at all — which is why
//! every read and write of a frame says which [`Escaping`] is in force rather than
//! assuming one.
//!
//! A frame is written out by hand here rather than by a crate: STOMP is ActiveMQ's own
//! line-oriented text protocol, and no maintained crate on the registry serves the SERVER
//! half of it — the one that does parse the format (`stomp-parser`) keeps every
//! constructor of a server frame and of a header value private, so a server cannot build
//! an answer with it.
//!
//! # Bounds
//!
//! * a frame's command and headers take at most [`MAX_HEADER_SECTION`] bytes, and a frame
//!   carries at most [`MAX_HEADERS`] headers and a command of at most
//!   [`MAX_COMMAND_BYTES`];
//! * a body is at most [`MAX_BODY_BYTES`], which is the body size the broker behind the
//!   session accepts;
//! * anything past one of those bounds is [`Parsed::Malformed`], which a server answers
//!   with an ERROR frame and closes the connection on, so nothing unbounded is buffered.

/// The longest command this parser accepts. The longest command STOMP defines is
/// `UNSUBSCRIBE`, so a line past this is not a command at all.
pub const MAX_COMMAND_BYTES: usize = 32;

/// How many bytes a frame's command and header lines may take together.
pub const MAX_HEADER_SECTION: usize = 8 * 1024;

/// How many headers one frame may carry.
pub const MAX_HEADERS: usize = 64;

/// The largest body this parser accepts: the largest message the broker behind a session
/// holds ([`crate::state::message_broker::MAX_MESSAGE_BYTES`]), so a frame that could not
/// be stored is refused before it is buffered whole.
pub const MAX_BODY_BYTES: usize = crate::state::message_broker::MAX_MESSAGE_BYTES;

/// The header whose value is the exact byte length of the frame's body.
pub const HEADER_CONTENT_LENGTH: &str = "content-length";

/// Whether the protocol's header escapes are in force for a frame.
///
/// STOMP 1.0 defines no escapes, and STOMP 1.1 and 1.2 exempt the CONNECT and CONNECTED
/// frames from them, so a header's bytes are taken literally in those cases and decoded in
/// every other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escaping {
    /// Header bytes are taken literally.
    Literal,
    /// `\n`, `\r`, `\c` and `\\` stand for a line feed, a carriage return, a colon and a
    /// backslash; any other escape is a fatal protocol error.
    Escaped,
}

/// One STOMP frame: its command, its headers in the order they arrived, and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub command: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Frame {
    /// A frame carrying `command`, no header and no body.
    pub fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// The same frame with one header appended.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// The same frame carrying `body`.
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// The value of the header `name`, or `None` when the frame carries none.
    ///
    /// The FIRST entry wins: STOMP 1.1 and 1.2 keep a repeated header's later entries as
    /// the value's history, and say the first is the value in force.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }

    /// The frame's bytes, as the wire carries them.
    pub fn serialize(&self, escaping: Escaping) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.body.len() + 64);
        bytes.extend_from_slice(self.command.as_bytes());
        bytes.push(b'\n');
        for (name, value) in &self.headers {
            bytes.extend_from_slice(escape(name, escaping).as_bytes());
            bytes.push(b':');
            bytes.extend_from_slice(escape(value, escaping).as_bytes());
            bytes.push(b'\n');
        }
        bytes.push(b'\n');
        bytes.extend_from_slice(&self.body);
        bytes.push(0);
        bytes
    }
}

/// What one read of a buffer produced.
#[derive(Debug)]
pub enum Parsed {
    /// A whole frame, and how many bytes of the buffer it took.
    Frame(Box<Frame>, usize),
    /// What is buffered so far is the beginning of a frame.
    Incomplete,
    /// The bytes are not a frame. The text says why, which is what the ERROR frame a
    /// server answers with carries.
    Malformed(String),
}

/// How many bytes at the front of `buffered` are between frames rather than part of one: a
/// client's heart-beats, which are bare ends of line.
pub fn skip_interframe(buffered: &[u8]) -> usize {
    let mut at = 0;
    while at < buffered.len() {
        match buffered[at] {
            b'\n' => at += 1,
            b'\r' if buffered.get(at + 1) == Some(&b'\n') => at += 2,
            _ => break,
        }
    }
    at
}

/// The next frame at the front of `buffered`, which must start at a command line (see
/// [`skip_interframe`]).
pub fn parse(buffered: &[u8], escaping: Escaping) -> Parsed {
    if buffered.is_empty() {
        return Parsed::Incomplete;
    }
    let Some((section, body_at)) = header_section(buffered) else {
        return if buffered.len() > MAX_HEADER_SECTION {
            Parsed::Malformed(format!(
                "a frame's command and headers may take at most {MAX_HEADER_SECTION} bytes"
            ))
        } else {
            Parsed::Incomplete
        };
    };
    let Ok(text) = ::std::str::from_utf8(section) else {
        return Parsed::Malformed("a frame's command and headers must be UTF-8".to_string());
    };
    let mut lines = text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line));
    let Some(command) = lines.next().filter(|command| !command.is_empty()) else {
        return Parsed::Malformed("a frame must begin with a command".to_string());
    };
    if command.len() > MAX_COMMAND_BYTES {
        return Parsed::Malformed(format!(
            "a command may take at most {MAX_COMMAND_BYTES} bytes, got {}",
            command.len()
        ));
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    // Every line left is a header line: the section ends at the first empty line, which
    // `header_section` has already cut off.
    for line in lines {
        if headers.len() >= MAX_HEADERS {
            return Parsed::Malformed(format!(
                "a frame may carry at most {MAX_HEADERS} headers"
            ));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Parsed::Malformed(format!("the header line {line:?} carries no colon"));
        };
        let (Some(name), Some(value)) = (unescape(name, escaping), unescape(value, escaping)) else {
            return Parsed::Malformed(format!(
                "the header line {line:?} carries an undefined escape sequence"
            ));
        };
        headers.push((name, value));
    }
    let frame = Frame {
        command: command.to_string(),
        headers,
        body: Vec::new(),
    };
    match body_of(buffered, body_at, &frame) {
        Err(malformed) => malformed,
        Ok(None) => Parsed::Incomplete,
        Ok(Some((body, consumed))) => Parsed::Frame(Box::new(frame.with_body(body)), consumed),
    }
}

/// The command and header lines at the front of `buffered`, and where the body begins:
/// everything up to the empty line that ends the header section, or `None` when that line
/// has not arrived yet.
fn header_section(buffered: &[u8]) -> Option<(&[u8], usize)> {
    let limit = buffered.len().min(MAX_HEADER_SECTION + 2);
    for at in 0..limit {
        if buffered[at] != b'\n' {
            continue;
        }
        match buffered.get(at + 1) {
            Some(b'\n') => return Some((&buffered[..at], at + 2)),
            Some(b'\r') if buffered.get(at + 2) == Some(&b'\n') => {
                return Some((&buffered[..at], at + 3))
            }
            _ => {}
        }
    }
    None
}

/// The frame's body and the whole frame's length, `Ok(None)` when the body has not all
/// arrived, and `Err` when what follows the headers cannot be a body.
///
/// A frame carrying `content-length` has a body of exactly that many bytes, which may hold
/// NUL bytes of its own; a frame without one has a body that ends at the first NUL.
type Body = Result<Option<(Vec<u8>, usize)>, Parsed>;

fn body_of(buffered: &[u8], body_at: usize, frame: &Frame) -> Body {
    let declared = match frame.header(HEADER_CONTENT_LENGTH) {
        None => None,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(length) => Some(length),
            Err(_) => {
                return Err(Parsed::Malformed(format!(
                    "{HEADER_CONTENT_LENGTH} must be a byte count, got {raw:?}"
                )))
            }
        },
    };
    let rest = &buffered[body_at.min(buffered.len())..];
    let length = match declared {
        Some(length) => {
            if length > MAX_BODY_BYTES {
                return Err(Parsed::Malformed(format!(
                    "a body may take at most {MAX_BODY_BYTES} bytes, got {length}"
                )));
            }
            if rest.len() < length + 1 {
                return Ok(None);
            }
            length
        }
        None => match rest.iter().position(|byte| *byte == 0) {
            Some(end) => end,
            None => {
                return if rest.len() > MAX_BODY_BYTES {
                    Err(Parsed::Malformed(format!(
                        "a body may take at most {MAX_BODY_BYTES} bytes"
                    )))
                } else {
                    Ok(None)
                }
            }
        },
    };
    if rest.get(length) != Some(&0) {
        return Err(Parsed::Malformed(
            "a frame's body must be followed by a NUL byte".to_string(),
        ));
    }
    Ok(Some((rest[..length].to_vec(), body_at + length + 1)))
}

/// A header name or value as the wire carries it.
fn escape(text: &str, escaping: Escaping) -> String {
    if escaping == Escaping::Literal {
        return text.to_string();
    }
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            ':' => escaped.push_str("\\c"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// A header name or value as it was meant, or `None` for an undefined escape sequence —
/// which STOMP 1.1 and 1.2 both call a fatal protocol error rather than a literal
/// backslash.
fn unescape(text: &str, escaping: Escaping) -> Option<String> {
    if escaping == Escaping::Literal {
        return Some(text.to_string());
    }
    let mut decoded = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next()? {
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            'c' => decoded.push(':'),
            '\\' => decoded.push('\\'),
            _ => return None,
        }
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame bytes a case parses, written as text so the case reads like the wire.
    fn wire(text: &str) -> Vec<u8> {
        text.replace('@', "\0").into_bytes()
    }

    /// One frame a case parses: the bytes on the wire, the escaping in force, and what the
    /// parser must report — the command, how many headers, the values of the ones the case
    /// names, the body, and what is left of the buffer behind the frame.
    struct ParseCase {
        case: &'static str,
        text: &'static str,
        escaping: Escaping,
        command: &'static str,
        header_count: usize,
        headers: &'static [(&'static str, &'static str)],
        body: &'static str,
        rest: &'static str,
    }

    /// RULE: a frame is a command line, its header lines, an empty line, the body and a
    /// NUL — and what the parser reports is that frame plus exactly the bytes it took, so
    /// the next frame on the connection starts where this one ended.
    #[test]
    fn a_whole_frame_is_parsed_with_the_bytes_it_took() {
        // The rows span the forms a client's frame takes: the line endings STOMP allows,
        // a frame with no header, a frame with no body, a body given by content-length
        // (which may hold a NUL), a repeated header, and a second frame left in the
        // buffer.
        let rows: &[ParseCase] = &[
            ParseCase {
                case: "a send with a body",
                text: "SEND\ndestination:/queue/a\n\nhi@",
                escaping: Escaping::Escaped,
                command: "SEND",
                header_count: 1,
                headers: &[("destination", "/queue/a")],
                body: "hi",
                rest: "",
            },
            ParseCase {
                case: "carriage returns before every line feed",
                text: "SEND\r\ndestination:/queue/a\r\n\r\nhi@",
                escaping: Escaping::Escaped,
                command: "SEND",
                header_count: 1,
                headers: &[("destination", "/queue/a")],
                body: "hi",
                rest: "",
            },
            ParseCase {
                case: "no header and no body",
                text: "DISCONNECT\n\n@",
                escaping: Escaping::Escaped,
                command: "DISCONNECT",
                header_count: 0,
                headers: &[],
                body: "",
                rest: "",
            },
            ParseCase {
                case: "a content-length body holding a NUL",
                text: "SEND\ndestination:/queue/a\ncontent-length:3\n\na@b@",
                escaping: Escaping::Escaped,
                command: "SEND",
                header_count: 2,
                headers: &[("destination", "/queue/a")],
                body: "a\0b",
                rest: "",
            },
            ParseCase {
                case: "an escaped header value",
                text: "SEND\nx\\cy:a\\nb\n\n@",
                escaping: Escaping::Escaped,
                command: "SEND",
                header_count: 1,
                headers: &[("x:y", "a\nb")],
                body: "",
                rest: "",
            },
            ParseCase {
                case: "an unescaped header value on a CONNECT",
                text: "CONNECT\npasscode:a\\nb\n\n@",
                escaping: Escaping::Literal,
                command: "CONNECT",
                header_count: 1,
                headers: &[("passcode", "a\\nb")],
                body: "",
                rest: "",
            },
            ParseCase {
                case: "a repeated header, whose first entry is the value",
                text: "SEND\ndestination:/queue/a\ndestination:/queue/b\n\n@",
                escaping: Escaping::Escaped,
                command: "SEND",
                header_count: 2,
                headers: &[("destination", "/queue/a")],
                body: "",
                rest: "",
            },
            ParseCase {
                case: "a frame followed by the next one",
                text: "DISCONNECT\n\n@SEND\ndestination:/queue/a\n\nhi@",
                escaping: Escaping::Escaped,
                command: "DISCONNECT",
                header_count: 0,
                headers: &[],
                body: "",
                rest: "SEND\ndestination:/queue/a\n\nhi@",
            },
        ];
        for row in rows {
            let ParseCase {
                case,
                text,
                escaping,
                command,
                header_count,
                headers,
                body,
                rest,
            } = row;
            let bytes = wire(text);
            let parsed = parse(&bytes, *escaping);
            let Parsed::Frame(frame, consumed) = parsed else {
                panic!("case {case:?}: expected a whole frame, got {parsed:?}");
            };
            assert_eq!(
                &frame.command, command,
                "case {case:?}: expected the command {command:?}, got {:?}",
                frame.command
            );
            assert_eq!(
                frame.headers.len(),
                *header_count,
                "case {case:?}: expected {header_count} headers, got {:?}",
                frame.headers
            );
            for (name, value) in *headers {
                assert_eq!(
                    frame.header(name),
                    Some(*value),
                    "case {case:?}: expected the header {name:?} to be {value:?}, got {:?}",
                    frame.header(name)
                );
            }
            assert_eq!(
                frame.body,
                body.as_bytes(),
                "case {case:?}: expected the body {body:?}, got {:?}",
                String::from_utf8_lossy(&frame.body)
            );
            // The frame ends at its NUL, so what the frame took leaves exactly the rest
            // of the connection behind.
            let left = bytes.get(consumed..).unwrap_or_default();
            assert_eq!(
                left,
                wire(rest),
                "case {case:?}: expected {rest:?} left after the frame, got {:?}",
                String::from_utf8_lossy(left)
            );
        }
    }

    /// RULE: a frame that has not all arrived is reported as incomplete — never as a
    /// frame and never as an error — so a client that writes a frame in pieces is served.
    #[test]
    fn a_partial_frame_is_incomplete() {
        // The rows span where a frame can be cut: mid-command, mid-headers, before the
        // empty line, mid-body, and before the NUL of a content-length body.
        let rows: &[(&str, &str)] = &[
            ("nothing at all", ""),
            ("half a command", "SEN"),
            ("the command only", "SEND\n"),
            ("a header but no empty line", "SEND\ndestination:/queue/a\n"),
            ("a body with no NUL", "SEND\ndestination:/queue/a\n\nhi"),
            (
                "a content-length body whose NUL has not arrived",
                "SEND\ncontent-length:2\n\nhi",
            ),
        ];
        for (case, text) in rows {
            let bytes = wire(text);
            let parsed = parse(&bytes, Escaping::Escaped);
            assert!(
                matches!(parsed, Parsed::Incomplete),
                "case {case:?}: expected the frame to be incomplete, got {parsed:?}"
            );
        }
    }

    /// RULE: bytes that cannot be a frame — or a frame past one of this parser's bounds —
    /// are refused, which is what a server answers with an ERROR frame and closes on.
    #[test]
    fn bytes_that_are_not_a_frame_are_refused() {
        let long_header_section = format!("SEND\n{}\n\n\0", "x".repeat(MAX_HEADER_SECTION + 16));
        let too_many_headers = format!(
            "SEND\n{}\n\n\0",
            (0..=MAX_HEADERS)
                .map(|index| format!("h{index}:v"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let long_command = format!("{}\n\n\0", "C".repeat(MAX_COMMAND_BYTES + 1));
        // The rows span every way a frame can be refused: no command, a header line with
        // no colon, an undefined escape, a content-length that is not a count, one past
        // the body bound, a body not followed by a NUL, and the header-section bounds.
        let rows: &[(&str, &str)] = &[
            ("no command", "\n\n@"),
            ("a header line with no colon", "SEND\ndestination\n\n@"),
            ("an undefined escape", "SEND\nx:a\\qb\n\n@"),
            (
                "a content-length that is not a count",
                "SEND\ncontent-length:lots\n\n@",
            ),
            (
                "a content-length past the body bound",
                "SEND\ncontent-length:1048577\n\n@",
            ),
            (
                "a content-length body not followed by a NUL",
                "SEND\ncontent-length:2\n\nhix",
            ),
            ("a command past its bound", long_command.as_str()),
            ("a header section past its bound", long_header_section.as_str()),
            ("more headers than the bound", too_many_headers.as_str()),
        ];
        for (case, text) in rows {
            let bytes = wire(text);
            let parsed = parse(&bytes, Escaping::Escaped);
            assert!(
                matches!(parsed, Parsed::Malformed(_)),
                "case {case:?}: expected the bytes to be refused, got {parsed:?}"
            );
        }
    }

    /// RULE: what a server writes parses back to what it meant, escapes and all, so a
    /// header value carrying a colon or a line feed survives the wire.
    #[test]
    fn a_written_frame_parses_back_to_itself() {
        // The rows span the values a header can carry: plain text, a colon, a line feed, a
        // backslash, and a body with no header at all.
        let rows: &[(&str, &str, &str, &str)] = &[
            ("plain text", "destination", "/queue/a", "hi"),
            ("a colon in the value", "message-id", "ID:b-1-7", ""),
            ("a line feed in the value", "note", "one\ntwo", "body"),
            ("a backslash in the value", "note", "a\\b", ""),
            ("a colon in the name", "x:y", "v", ""),
        ];
        for (case, name, value, body) in rows {
            let frame = Frame::new("MESSAGE")
                .with_header(name, value)
                .with_body(body.as_bytes().to_vec());
            let bytes = frame.serialize(Escaping::Escaped);
            let parsed = parse(&bytes, Escaping::Escaped);
            let Parsed::Frame(read, consumed) = parsed else {
                panic!("case {case:?}: expected the written frame to parse, got {parsed:?}");
            };
            assert_eq!(
                consumed,
                bytes.len(),
                "case {case:?}: expected the frame to take every byte written"
            );
            assert_eq!(
                *read, frame,
                "case {case:?}: expected the frame to survive the wire, got {read:?}"
            );
        }
    }

    /// RULE: a bare end of line between frames is a heart-beat, not a frame, and is
    /// skipped — so a client beating while it is idle is not read as a protocol error.
    #[test]
    fn heart_beats_between_frames_are_not_frames() {
        // The rows span what a client can send between frames: nothing, one line feed,
        // several, a carriage-return pair, and a beat followed by a frame.
        let rows: &[(&str, &str, usize)] = &[
            ("nothing", "SEND\n\n@", 0),
            ("one line feed", "\nSEND\n\n@", 1),
            ("three line feeds", "\n\n\nSEND\n\n@", 3),
            ("a carriage-return pair", "\r\nSEND\n\n@", 2),
            ("a beat and nothing else", "\n", 1),
        ];
        for (case, text, expected) in rows {
            let bytes = wire(text);
            let skipped = skip_interframe(&bytes);
            assert_eq!(
                skipped, *expected,
                "case {case:?}: expected {expected} bytes of heart-beat, got {skipped}"
            );
        }
    }
}
