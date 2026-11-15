// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of an HTTP `Range` request header, for every service that serves part of
//! a representation it holds.
//!
//! A caller asks for part of an object with `Range: bytes=<first>-<last>`, and HTTP (RFC 9110,
//! "Range", "Content-Range" and "Range Requests") states what the three outcomes are. They are
//! read HERE, once, so two services handed the same header over a representation of the same
//! size answer the same span, the same refusal and the same ignored header:
//!
//! * the header SELECTS bytes of the representation — a [`ByteSpan`], which an answer carries
//!   under `206 Partial Content` with [`ByteSpan::content_range`] and a `Content-Length` of
//!   [`ByteSpan::len`];
//! * the header is well formed and selects NOTHING of it: its first byte position is at or past
//!   the length, or it asks for the last zero bytes. "A byte-range-set is satisfiable if it
//!   includes at least one byte-range-spec with a first-byte-pos that is less than the current
//!   length of the representation, or at least one suffix-byte-range-spec with a non-zero
//!   suffix-length", so this is the one range condition answered with a REFUSAL, carrying
//!   [`unsatisfied_content_range`] and none of the representation's bytes;
//! * the header is one this reading does not act on — a unit other than `bytes`, a specifier
//!   that does not parse, a spec whose last byte position is below its first ("a
//!   byte-range-spec is invalid if the last-byte-pos value is present and less than the
//!   first-byte-pos"), or several ranges at once, for which no `multipart/byteranges` answer is
//!   produced. "A server MAY ignore the Range header field", so the WHOLE representation is
//!   served.
//!
//! The three forms of the `bytes` unit are the ones RFC 9110's grammar defines:
//! `bytes=<first>-<last>`, `bytes=<first>-` (to the end) and `bytes=-<suffix>` (the last
//! `<suffix>` bytes). A `<last>` past the end is clamped to the final byte — "if the
//! last-byte-pos value is absent, or greater than or equal to the current length of the
//! representation data, the byte range is interpreted as the remaining bytes of the
//! representation" — and a suffix longer than the representation selects the whole of it.
//!
//! Two readers in this tree are deliberately NOT this rule, because neither reads an HTTP
//! `Range`: `service-com-amazonaws-mediastoredata-mediastoreobject-20170901` and
//! `service-com-amazonaws-glacier-glacier` each read a MODELLED member of their own API, whose
//! form their own model or documentation fixes — a `@pattern`ed `Range` string answered with
//! one modelled error, and a `RetrievalByteRange` of the form `StartByteValue-EndByteValue`
//! with no unit and no suffix form. Where such a service does serve an HTTP `Range` over bytes
//! it holds, it reads it here: MediaStore Data's object read does exactly that and renders only
//! its own modelled `Content-Range` form itself.
//!
//! It computes over two values and holds nothing: no clock, no store, no network.

/// One span of a representation's bytes: the first and last byte INCLUSIVE, the way HTTP's
/// `Range` and `Content-Range` both state a span.
///
/// Non-empty by construction ([`ByteSpan::new`]), because an empty span is not a span HTTP can
/// state: `Content-Range` names a first and a last byte, so a response can only report one that
/// holds at least one byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteSpan {
    /// The first byte of the span, counted from zero.
    first: usize,
    /// The LAST byte of the span, inclusive.
    last: usize,
}

impl ByteSpan {
    /// The span from `first` to `last` inclusive, or `None` when that is no span at all
    /// (`last` before `first`).
    pub fn new(first: usize, last: usize) -> Option<Self> {
        (first <= last).then_some(Self { first, last })
    }

    /// The first byte of the span.
    pub fn first(self) -> usize {
        self.first
    }

    /// The last byte of the span, inclusive.
    pub fn last(self) -> usize {
        self.last
    }

    /// How many bytes the span holds — what `Content-Length` reports for a response carrying
    /// it, since "Content-Length is the length of the body actually being returned".
    pub fn len(self) -> usize {
        self.last - self.first + 1
    }

    /// Never empty: see [`ByteSpan`].
    pub fn is_empty(self) -> bool {
        false
    }

    /// Those bytes of `body`, clamped to what it holds so a span can never read past it.
    pub fn slice(self, body: &[u8]) -> Vec<u8> {
        let first = self.first.min(body.len());
        let end = self.last.saturating_add(1).min(body.len());
        body.get(first..end).map(<[u8]>::to_vec).unwrap_or_default()
    }

    /// The `Content-Range` header value for this span of a representation of `total` bytes:
    /// `bytes <first>-<last>/<total>`, the one form RFC 9110 defines for a `bytes` range.
    pub fn content_range(self, total: usize) -> String {
        format!("bytes {}-{}/{}", self.first, self.last, total)
    }
}

/// The `Content-Range` header value of a REFUSED range over a representation of `total` bytes:
/// `bytes */<total>`, the "unsatisfied-range" form, which states the length and names no
/// window — "a 416 response ... SHOULD generate a Content-Range header field specifying the
/// current length of the selected representation".
pub fn unsatisfied_content_range(total: usize) -> String {
    format!("bytes */{total}")
}

/// What a `Range` header asks of a representation of a given size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRequest {
    /// The header asks for exactly this span.
    Span(ByteSpan),
    /// The header is well formed but asks for bytes the representation does not have, which
    /// is the one range condition HTTP answers with a refusal.
    Unsatisfiable,
    /// The header asks for nothing this reading can answer as a single range — a form the
    /// `bytes` unit does not define, several ranges at once, or an invalid spec. RFC 9110 lets
    /// a server "ignore a Range header field", so the whole representation is served.
    Ignored,
}

/// One byte POSITION a `Range` header states, as a decimal number of bytes.
///
/// A position is 1*DIGIT in RFC 9110's grammar, with no stated ceiling, so one a caller writes
/// with more digits than this machine counts bytes in is still a well-formed position — it just
/// names a byte no representation can hold. Telling that from a position that is not a number
/// at all is what keeps the outcome from turning on the DIGIT COUNT: a first byte written that
/// way is past the end of every representation, which is a refusal, while a specifier that is
/// no number is a header to ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// A position this machine can index: that many bytes from the start.
    At(usize),
    /// A position past the end of any representation this machine can hold.
    PastEverything,
}

/// The byte position `text` states, or `None` when it states none (it is empty, or it is not a
/// run of decimal digits).
fn position(text: &str) -> Option<Position> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(match text.parse::<usize>() {
        Ok(position) => Position::At(position),
        Err(_) => Position::PastEverything,
    })
}

/// What the `Range` header `header` asks of a representation of `total` bytes.
///
/// The module docs carry the rule; this is the whole of it, so every service that serves a
/// ranged read answers the same span, the same refusal and the same ignored header.
pub fn requested_range(header: &str, total: usize) -> RangeRequest {
    let Some(set) = header.trim().strip_prefix("bytes=") else {
        // Another unit entirely: "An origin server MUST ignore a Range header field that
        // contains a range unit it does not understand".
        return RangeRequest::Ignored;
    };
    let mut spans = set.split(',');
    let (Some(single), None) = (spans.next(), spans.next()) else {
        return RangeRequest::Ignored;
    };
    let Some((first, last)) = single.trim().split_once('-') else {
        return RangeRequest::Ignored;
    };
    let (first, last) = (first.trim(), last.trim());
    match (first.is_empty(), last.is_empty()) {
        // `bytes=-<suffix>`: the last `suffix` bytes. A zero-length suffix selects nothing,
        // which is unsatisfiable, and so is any suffix of an empty representation; a suffix
        // longer than the representation — this machine's ceiling included — selects the whole
        // of it.
        (true, false) => {
            let Some(suffix) = position(last) else {
                return RangeRequest::Ignored;
            };
            if total == 0 || suffix == Position::At(0) {
                return RangeRequest::Unsatisfiable;
            }
            let first = match suffix {
                Position::At(suffix) => total.saturating_sub(suffix),
                Position::PastEverything => 0,
            };
            ByteSpan::new(first, total - 1).map_or(RangeRequest::Unsatisfiable, RangeRequest::Span)
        }
        // `bytes=<first>-` : from `first` to the end of the representation.
        (false, true) => match position(first) {
            None => RangeRequest::Ignored,
            // Past the end of any representation, so past the end of this one.
            Some(Position::PastEverything) => RangeRequest::Unsatisfiable,
            Some(Position::At(first)) => {
                if first >= total {
                    return RangeRequest::Unsatisfiable;
                }
                ByteSpan::new(first, total - 1)
                    .map_or(RangeRequest::Unsatisfiable, RangeRequest::Span)
            }
        },
        // `bytes=<first>-<last>` : the span, with `last` clamped to the final byte. A
        // `<last>` BEFORE `<first>` makes the spec syntactically invalid, which is a header to
        // ignore rather than a range to refuse: the refusal is for a valid range the
        // representation cannot satisfy.
        (false, false) => {
            let (Some(first), Some(last)) = (position(first), position(last)) else {
                return RangeRequest::Ignored;
            };
            match (first, last) {
                // A last byte below the first: no range at all.
                (Position::PastEverything, Position::At(_)) => RangeRequest::Ignored,
                (Position::At(first), Position::At(last)) if last < first => {
                    RangeRequest::Ignored
                }
                // A first byte past the end of every representation, so past this one's.
                (Position::PastEverything, Position::PastEverything) => {
                    RangeRequest::Unsatisfiable
                }
                (Position::At(first), last) => {
                    if first >= total {
                        return RangeRequest::Unsatisfiable;
                    }
                    // A last byte past the end is "the remaining bytes of the representation",
                    // this machine's ceiling included.
                    let last = match last {
                        Position::At(last) => last.min(total - 1),
                        Position::PastEverything => total - 1,
                    };
                    ByteSpan::new(first, last)
                        .map_or(RangeRequest::Unsatisfiable, RangeRequest::Span)
                }
            }
        }
        // `bytes=-` names no bytes at all.
        (true, true) => RangeRequest::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What each shape of `Range` asks of a representation, over the three outcomes HTTP
    /// defines. The rows differ in where the window sits, how it is spelled (both ends, an open
    /// end, a suffix), how far past the end it reaches, the size of the representation, and
    /// whether the header is one the `bytes` unit defines at all.
    #[test]
    fn a_range_header_selects_a_window_a_refusal_or_nothing_at_all() {
        // (case, header, representation length, what it asks for)
        let cases: [(&str, &str, usize, RangeRequest); 22] = [
            (
                "a window inside the representation",
                "bytes=10-19",
                2048,
                RangeRequest::Span(ByteSpan { first: 10, last: 19 }),
            ),
            (
                "the first byte alone",
                "bytes=0-0",
                2048,
                RangeRequest::Span(ByteSpan { first: 0, last: 0 }),
            ),
            (
                "the last byte alone",
                "bytes=2047-2047",
                2048,
                RangeRequest::Span(ByteSpan {
                    first: 2047,
                    last: 2047,
                }),
            ),
            (
                "the whole representation spelled as a range",
                "bytes=0-2047",
                2048,
                RangeRequest::Span(ByteSpan { first: 0, last: 2047 }),
            ),
            (
                "an open end runs to the last byte",
                "bytes=2040-",
                2048,
                RangeRequest::Span(ByteSpan {
                    first: 2040,
                    last: 2047,
                }),
            ),
            (
                "a last byte past the end is clamped to it",
                "bytes=2040-9999",
                2048,
                RangeRequest::Span(ByteSpan {
                    first: 2040,
                    last: 2047,
                }),
            ),
            (
                "a suffix of the representation",
                "bytes=-10",
                2048,
                RangeRequest::Span(ByteSpan {
                    first: 2038,
                    last: 2047,
                }),
            ),
            (
                "a suffix longer than the representation is the whole of it",
                "bytes=-4096",
                2048,
                RangeRequest::Span(ByteSpan { first: 0, last: 2047 }),
            ),
            (
                "the only byte of a one-byte representation",
                "bytes=0-",
                1,
                RangeRequest::Span(ByteSpan { first: 0, last: 0 }),
            ),
            (
                "the first byte one past the end",
                "bytes=2048-2057",
                2048,
                RangeRequest::Unsatisfiable,
            ),
            (
                "a window far past the end",
                "bytes=4096-8191",
                2048,
                RangeRequest::Unsatisfiable,
            ),
            ("the last zero bytes", "bytes=-0", 2048, RangeRequest::Unsatisfiable),
            (
                "any byte of an empty representation",
                "bytes=0-0",
                0,
                RangeRequest::Unsatisfiable,
            ),
            (
                "a suffix of an empty representation",
                "bytes=-10",
                0,
                RangeRequest::Unsatisfiable,
            ),
            (
                "a last byte below the first",
                "bytes=19-10",
                2048,
                RangeRequest::Ignored,
            ),
            ("a unit that is not bytes", "items=0-9", 2048, RangeRequest::Ignored),
            ("two ranges at once", "bytes=0-9,20-29", 2048, RangeRequest::Ignored),
            (
                // A position with more digits than this machine counts bytes in is still a
                // position: it names a byte no representation holds, so it is refused rather
                // than served whole.
                "a first byte past every representation",
                "bytes=99999999999999999999-",
                2048,
                RangeRequest::Unsatisfiable,
            ),
            (
                "a window past every representation",
                "bytes=99999999999999999999-99999999999999999999",
                2048,
                RangeRequest::Unsatisfiable,
            ),
            (
                "a last byte past every representation is the end of this one",
                "bytes=2040-99999999999999999999",
                2048,
                RangeRequest::Span(ByteSpan {
                    first: 2040,
                    last: 2047,
                }),
            ),
            (
                "a suffix longer than every representation is the whole of this one",
                "bytes=-99999999999999999999",
                2048,
                RangeRequest::Span(ByteSpan { first: 0, last: 2047 }),
            ),
            (
                "a last byte below a first byte past every representation",
                "bytes=99999999999999999999-10",
                2048,
                RangeRequest::Ignored,
            ),
        ];

        for (case, header, total, expected) in cases {
            let actual = requested_range(header, total);
            assert_eq!(
                actual, expected,
                "case {case:?} ({header:?} over {total} bytes): expected {expected:?}, got \
                 {actual:?}"
            );
        }
    }

    /// A specifier that is no range at all is ignored rather than read as one.
    #[test]
    fn a_specifier_that_is_not_a_range_is_ignored() {
        for case in ["bytes=abc", "bytes=", "bytes=-", "bytes=1-x", "bytes=+1-2", "0-9", ""] {
            let actual = requested_range(case, 2048);
            assert_eq!(
                actual,
                RangeRequest::Ignored,
                "case {case:?}: expected the header to be ignored, got {actual:?}"
            );
        }
    }

    /// What a span states about itself: its length, its bytes, and the two `Content-Range`
    /// forms — the one a served window carries and the one a refusal carries.
    #[test]
    fn a_span_states_its_length_its_bytes_and_its_content_range() {
        let body: Vec<u8> = (0..=255u8).collect();
        // (case, first, last, expected length, expected Content-Range)
        let cases: [(&str, usize, usize, usize, &str); 4] = [
            ("one byte", 0, 0, 1, "bytes 0-0/256"),
            ("a window inside the body", 10, 19, 10, "bytes 10-19/256"),
            ("the tail of the body", 248, 255, 8, "bytes 248-255/256"),
            ("the whole body", 0, 255, 256, "bytes 0-255/256"),
        ];
        for (case, first, last, length, content_range) in cases {
            let span = ByteSpan::new(first, last).expect("the case states a non-empty span");
            assert_eq!(
                span.len(),
                length,
                "case {case:?}: expected {length} bytes, got {}",
                span.len()
            );
            assert_eq!(
                span.slice(&body),
                body[first..=last].to_vec(),
                "case {case:?}: expected exactly the bytes {first}..={last}, got {} of them",
                span.slice(&body).len()
            );
            assert_eq!(
                span.content_range(body.len()),
                content_range,
                "case {case:?}: expected {content_range:?}, got {:?}",
                span.content_range(body.len())
            );
            assert!(!span.is_empty(), "case {case:?}: a span holds at least one byte");
        }
        assert_eq!(
            ByteSpan::new(19, 10),
            None,
            "a last byte before the first is no span at all"
        );
        assert_eq!(
            unsatisfied_content_range(2048),
            "bytes */2048",
            "a refusal states the representation's length and names no window"
        );
    }

    /// A span is clamped to the body it reads, so it can never read past it.
    #[test]
    fn a_span_reads_no_further_than_the_body_holds() {
        let body: Vec<u8> = (0..16u8).collect();
        // (case, first, last, expected bytes)
        let cases: [(&str, usize, usize, &[u8]); 3] = [
            ("a span inside the body", 2, 5, &[2, 3, 4, 5]),
            ("a span whose end is past the body", 14, 99, &[14, 15]),
            ("a span wholly past the body", 32, 64, &[]),
        ];
        for (case, first, last, expected) in cases {
            let span = ByteSpan::new(first, last).expect("the case states a non-empty span");
            let actual = span.slice(&body);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
