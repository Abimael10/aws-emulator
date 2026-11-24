// SPDX-License-Identifier: CC0-1.0

//! How `TestRenderEmailTemplate` WRITES the parts it rendered into one MIME document: the
//! transfer encoding each part carries, and the delimiter that separates two of them.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service
//! registered — and reads the answered document back with `mailparse`, the crate this
//! project reads an RFC 5322 / MIME message with, so the assertions are about the MESSAGE
//! rather than about one spelling of it. What a template RENDERS TO is the sibling suite
//! `email_template_render`; this one is about the document those parts travel in.
//!
//! The rules these cases pin:
//!
//! * a part is written unencoded and declared `7bit` only where its rendered text is
//!   already what RFC 2045 section 6.2 admits as that — US-ASCII, in lines a message may
//!   carry (RFC 5322 section 2.1.1 bounds a line at 998 octets); any other text is
//!   quoted-printable encoded and declared `quoted-printable`, so the part is transportable
//!   over a 7-bit path, its lines stay within the 76 octets RFC 2045 section 6.7 wraps at,
//!   and it decodes back to exactly the text that was rendered. The decision is per PART,
//!   so one alternative may be encoded while the other is not;
//! * a rendered subject line that is not printable US-ASCII goes in as RFC 2047 encoded
//!   words, which decode back to what was rendered;
//! * each render ASSEMBLES a new message, so the boundary that delimits its parts is minted
//!   per response: two renders of one template and one set of rendering data carry the same
//!   parts behind two different delimiters, and no part carries the delimiter of its own
//!   document.
//!
//! The tables span what those rules generalise over: the form of the rendered text (short
//! US-ASCII, US-ASCII carrying the characters the encoding escapes, US-ASCII with a line
//! break, non-ASCII, and long text at the published line bound and one octet past it) and,
//! for the delimiter, the parts the renders carry and whether their rendering data is the
//! same.

use mailparse::{MailHeaderMap, ParsedMail};
use serde_json::json;

use crate::email_template_render::{part_of, part_with, Ses, TEMPLATE};

/// `Content-Transfer-Encoding: 7bit` — the label a part written as it was rendered carries.
const SEVEN_BIT: &str = "7bit";

/// `Content-Transfer-Encoding: quoted-printable` — the label an encoded part carries.
const QUOTED_PRINTABLE: &str = "quoted-printable";

/// The longest line a message may carry, excluding its terminator: RFC 5322 section 2.1.1.
const MAX_LINE_OCTETS: usize = 998;

/// The longest line quoted-printable writes, excluding its terminator: RFC 2045 section
/// 6.7 bounds an encoded line at 76 octets, the soft `=` break included.
const MAX_ENCODED_LINE_OCTETS: usize = 76;

/// One row of the encoding table: the value the render substitutes, and the encoding each
/// alternative then has to declare.
struct EncodingCase {
    case: &'static str,
    /// The value the rendering data states for the one attribute both parts name.
    value: fn() -> String,
    /// What the text alternative — the value alone — has to declare.
    text_encoding: &'static str,
    /// What the HTML alternative — the value inside one element — has to declare.
    html_encoding: &'static str,
}

/// The rows span the form of the rendered text, including the published line bound itself
/// and one octet past it.
///
/// The HTML alternative wraps the same value in `<p>…</p>`, so it is seven octets longer
/// than the text one: at the bound, that is a text part a message may carry a line of and
/// an HTML part it may not, which is what makes the decision a PART's rather than a
/// message's.
fn encoding_cases() -> Vec<EncodingCase> {
    vec![
        EncodingCase {
            case: "short US-ASCII, which a part carries as it was rendered",
            value: || "Ada".to_string(),
            text_encoding: SEVEN_BIT,
            html_encoding: SEVEN_BIT,
        },
        EncodingCase {
            case: "US-ASCII carrying the characters the encoding escapes",
            value: || "a=b c\td~e".to_string(),
            text_encoding: SEVEN_BIT,
            html_encoding: SEVEN_BIT,
        },
        EncodingCase {
            case: "US-ASCII over two short lines",
            value: || "first\nsecond".to_string(),
            text_encoding: SEVEN_BIT,
            html_encoding: SEVEN_BIT,
        },
        EncodingCase {
            case: "a non-ASCII value, which no 7-bit path carries",
            value: || "café-日本 😀".to_string(),
            text_encoding: QUOTED_PRINTABLE,
            html_encoding: QUOTED_PRINTABLE,
        },
        EncodingCase {
            case: "a line of exactly the longest a message may carry",
            value: || "x".repeat(MAX_LINE_OCTETS),
            text_encoding: SEVEN_BIT,
            html_encoding: QUOTED_PRINTABLE,
        },
        EncodingCase {
            case: "a line one octet past that bound",
            value: || "x".repeat(MAX_LINE_OCTETS + 1),
            text_encoding: QUOTED_PRINTABLE,
            html_encoding: QUOTED_PRINTABLE,
        },
        EncodingCase {
            case: "a long non-ASCII value",
            value: || "Grüße ".repeat(400),
            text_encoding: QUOTED_PRINTABLE,
            html_encoding: QUOTED_PRINTABLE,
        },
    ]
}

/// The template both tables render: the one attribute in the subject line and in both body
/// parts.
fn substituting_content() -> serde_json::Value {
    json!({
        "Subject": "S {{v}}",
        "Text": "{{v}}",
        "Html": "<p>{{v}}</p>",
    })
}

/// The transfer encoding `part` declares, or an empty string when it declares none.
fn declared_encoding(part: &ParsedMail) -> String {
    part.headers
        .get_first_value("Content-Transfer-Encoding")
        .unwrap_or_default()
}

#[tokio::test]
async fn a_part_is_encoded_exactly_where_its_rendered_text_is_not_transportable_as_it_is() {
    for row in encoding_cases() {
        let case = row.case;
        let value = (row.value)();
        let ses = Ses::new();
        let created = ses.create_template(TEMPLATE, substituting_content()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the template to be saved, got {} {:?}",
            created.status, created.body
        );

        let data = json!({"v": value}).to_string();
        let reply = ses.render(TEMPLATE, &data).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected the render to be answered with 200, got {} {:?}",
            reply.status, reply.body
        );
        let document = reply.rendered();
        let parsed = mailparse::parse_mail(document.as_bytes()).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a MIME document, got {error}")
        });

        // The subject line decodes back to what was rendered, whatever form it went in as,
        // and a subject that is not printable US-ASCII goes in as RFC 2047 encoded words.
        let subject = parsed.headers.get_first_value("Subject").unwrap_or_default();
        assert_eq!(
            subject,
            format!("S {value}"),
            "case {case:?}: expected the rendered subject line to decode back, got \
             {subject:?}"
        );
        let printable_ascii = subject
            .chars()
            .all(|character| character.is_ascii() && !character.is_ascii_control());
        if !printable_ascii || subject.len() > MAX_LINE_OCTETS {
            assert!(
                document.contains("=?UTF-8?"),
                "case {case:?}: expected the subject as RFC 2047 encoded words, got \
                 {:?}",
                document.lines().next().unwrap_or_default()
            );
        }

        // Each alternative declares the encoding its own text needs, and decodes back to
        // exactly the text the template rendered.
        for (media_type, expected, rendered) in [
            ("text/plain", row.text_encoding, value.clone()),
            ("text/html", row.html_encoding, format!("<p>{value}</p>")),
        ] {
            let part = part_with(&parsed, media_type).unwrap_or_else(|| {
                panic!("case {case:?}: expected a {media_type} alternative in the document")
            });
            let declared = declared_encoding(part);
            assert_eq!(
                declared, expected,
                "case {case:?}: expected the {media_type} part to declare {expected:?}, got \
                 {declared:?}"
            );
            let decoded = part_of(&parsed, media_type).unwrap_or_default();
            assert_eq!(
                decoded, rendered,
                "case {case:?}: expected the {media_type} part to decode back to the text the \
                 template rendered ({} octets), got {} octets",
                rendered.len(),
                decoded.len()
            );
            if declared == SEVEN_BIT {
                // Written as it was rendered, so the document carries that text itself.
                assert!(
                    document.contains(&rendered),
                    "case {case:?}: expected the unencoded {media_type} part to be written as it \
                     was rendered, got {} octets of document",
                    document.len()
                );
            }
        }

        // The whole document is a message a 7-bit path carries: every octet of it is
        // US-ASCII, which is what the encoding above is for, and every line is one a
        // message may carry.
        assert!(
            document.is_ascii(),
            "case {case:?}: expected the whole document to be US-ASCII, got {} octets that are \
             not",
            document
                .chars()
                .filter(|character| !character.is_ascii())
                .count()
        );
        let longest = document
            .split("\r\n")
            .map(|line| line.trim_end_matches('\n').len())
            .max()
            .unwrap_or_default();
        assert!(
            longest <= MAX_LINE_OCTETS,
            "case {case:?}: expected every line to be at most {MAX_LINE_OCTETS} octets, got one \
             of {longest} in {} octets of document",
            document.len()
        );
        // A part longer than the codec's line bound is broken with its soft line breaks
        // rather than written as one long line.
        let encoded = row.text_encoding == QUOTED_PRINTABLE
            || row.html_encoding == QUOTED_PRINTABLE;
        if encoded && value.len() > MAX_ENCODED_LINE_OCTETS {
            assert!(
                document.contains("=\r\n"),
                "case {case:?}: expected a soft line break in a part of {} octets, got none in \
                 {} octets of document",
                value.len(),
                document.len()
            );
        }
    }
}

/// One row of the delimiter table: the template the renders share and the two sets of
/// rendering data they are driven with.
struct DelimiterCase {
    case: &'static str,
    content: fn() -> serde_json::Value,
    /// The data the first render carries.
    first: &'static str,
    /// The data the second render carries — the same data, or other data.
    second: &'static str,
    /// Whether both renders produce the same parts, so the delimiter is all that may
    /// differ.
    same_parts: bool,
}

/// The rows span the parts the renders carry, whether their data is the same, and whether
/// the parts are short or long.
const DELIMITER_CASES: &[DelimiterCase] = &[
    DelimiterCase {
        case: "two renders of one template with the identical rendering data",
        content: || {
            json!({
                "Subject": "Hi {{name}}",
                "Text": "plain {{name}}",
                "Html": "<p>html {{name}}</p>",
            })
        },
        first: r#"{"name":"Ada"}"#,
        second: r#"{"name":"Ada"}"#,
        same_parts: true,
    },
    DelimiterCase {
        case: "two renders with other rendering data",
        content: || {
            json!({
                "Subject": "Hi {{name}}",
                "Text": "plain {{name}}",
                "Html": "<p>html {{name}}</p>",
            })
        },
        first: r#"{"name":"Ada"}"#,
        second: r#"{"name":"Grace"}"#,
        same_parts: false,
    },
    DelimiterCase {
        case: "two renders whose parts carry no reference at all",
        content: || json!({"Subject": "s", "Text": "plain", "Html": "<p>html</p>"}),
        first: "{}",
        second: "",
        same_parts: true,
    },
    DelimiterCase {
        case: "two renders whose parts are encoded",
        content: || json!({"Subject": "s", "Text": "{{v}}", "Html": "<p>{{v}}</p>"}),
        first: r#"{"v":"Grüße 😀"}"#,
        second: r#"{"v":"Grüße 😀"}"#,
        same_parts: true,
    },
];

#[tokio::test]
async fn each_render_delimits_its_parts_with_a_boundary_of_its_own() {
    let mut minted: Vec<String> = Vec::new();
    for row in DELIMITER_CASES {
        let case = row.case;
        let ses = Ses::new();
        let created = ses.create_template(TEMPLATE, (row.content)()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the template to be saved, got {} {:?}",
            created.status, created.body
        );

        let mut documents: Vec<String> = Vec::new();
        let mut boundaries: Vec<String> = Vec::new();
        let mut parts: Vec<Vec<String>> = Vec::new();
        for (index, data) in [row.first, row.second].into_iter().enumerate() {
            let reply = ses.render(TEMPLATE, data).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?} render {index}: expected 200, got {} {:?}",
                reply.status, reply.body
            );
            let document = reply.rendered();
            let parsed = mailparse::parse_mail(document.as_bytes()).unwrap_or_else(|error| {
                panic!("case {case:?} render {index}: expected a MIME document, got {error}")
            });
            assert_eq!(
                parsed.ctype.mimetype, "multipart/alternative",
                "case {case:?} render {index}: expected both alternatives in one document, got \
                 {:?}",
                parsed.ctype.mimetype
            );
            let boundary = parsed
                .ctype
                .params
                .get("boundary")
                .cloned()
                .unwrap_or_default();
            assert!(
                !boundary.is_empty(),
                "case {case:?} render {index}: expected the document to name its boundary, got \
                 {document:?}"
            );
            // The delimiter appears in no part, since a part carrying it would end early.
            let bodies: Vec<String> = ["text/plain", "text/html"]
                .into_iter()
                .map(|media_type| part_of(&parsed, media_type).unwrap_or_default())
                .collect();
            for (media_type, body) in ["text/plain", "text/html"].into_iter().zip(bodies.iter()) {
                assert!(
                    !body.contains(&boundary),
                    "case {case:?} render {index}: expected the {media_type} part to carry no \
                     delimiter of its own document, got {body:?}"
                );
            }
            documents.push(document);
            boundaries.push(boundary);
            parts.push(bodies);
        }

        assert_ne!(
            boundaries[0], boundaries[1],
            "case {case:?}: expected each render to mint a boundary of its own, got {:?} twice",
            boundaries[0]
        );
        if row.same_parts {
            assert_eq!(
                parts[0], parts[1],
                "case {case:?}: expected both renders to carry the same parts, got {:?} and {:?}",
                parts[0], parts[1]
            );
            assert_ne!(
                documents[0], documents[1],
                "case {case:?}: expected the two documents to differ in their delimiter, got one \
                 document twice"
            );
        }
        minted.extend(boundaries);
    }

    // Across every render above: no two documents were delimited by one token.
    let mut distinct = minted.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected every one of the {} boundaries to be distinct, got {} distinct ones",
        minted.len(),
        distinct.len()
    );
}
