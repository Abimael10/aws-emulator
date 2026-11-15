// SPDX-License-Identifier: CC0-1.0

//! `TestRenderEmailTemplate` over the LIFECYCLE of the template it previews, end to end
//! over HTTP.
//!
//! The rule these cases pin: a preview is computed from the content the account holds
//! under the requested name AT THE MOMENT OF THE CALL, read afresh every time, and the
//! document it answers is an assembly of the parts that content carries. So a write the
//! service ACCEPTED (`UpdateEmailTemplate`) changes what a later preview renders, a write
//! it REFUSED (a second `CreateEmailTemplate` under a name the account already holds,
//! which "creates" nothing) changes nothing a later preview renders, and which media
//! types the document declares follows the parts the STORED content carries rather than
//! anything the render's own request says. The model is what makes the preview a function
//! of the stored content: `TestRenderEmailTemplate` takes only the template's name and
//! one set of rendering data
//! (`smithy_query(query="operation",operation="TestRenderEmailTemplate")`), and
//! `CreateEmailTemplate` models `AlreadyExistsException` for a name the account already
//! holds while `UpdateEmailTemplate` is the call that replaces that name's content.
//!
//! The non-ASCII cases pin the other half of the assembly: a header field is one line of
//! US-ASCII text (RFC 5322 sections 2.1.1 and 2.2), so a rendered subject line carrying
//! anything else travels as RFC 2047 encoded words and decodes back to exactly what was
//! rendered, while a BODY part states the transfer encoding its text is written in, so
//! that text decodes back to exactly what was rendered too (RFC 2045 section 6).
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — through the harness the sibling suite for this operation owns
//! (`crate::email_template_render`), so routing, deserialization, the handler and
//! serialization are all exercised, and every document is read back with `mailparse`, the
//! crate this project reads an RFC 5322 / MIME message with, so the assertions are about
//! the MESSAGE rather than about one spelling of it.

use mailparse::MailHeaderMap;
use serde_json::{json, Value};

// The harness, the template name and the part readers this operation's own suite already
// owns, rather than a second copy of either.
use crate::email_template_render::{part_of, part_with, Reply, Ses, TEMPLATE};

/// The rendering data the lifecycle cases render against, where the data is not what is
/// under test.
const DATA: &str = r#"{"name":"Ada"}"#;

/// The document's own header block: everything before the empty line that ends it.
fn header_block(document: &str) -> &str {
    document
        .split_once("\r\n\r\n")
        .map_or(document, |(headers, _)| headers)
}

/// What one render has to answer: the media type of the document itself, the subject line
/// it carries and the body parts it carries.
struct Rendered {
    top_level: &'static str,
    subject: &'static str,
    text: Option<&'static str>,
    html: Option<&'static str>,
}

/// Assert that `reply` is the document `expected` describes, and hand back the document it
/// carried so a case can compare two of them.
fn assert_rendered(case: &str, when: &str, reply: &Reply, expected: &Rendered) -> String {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (200, ""),
        "case {case:?} ({when}): expected the render to be answered with 200, got {} {:?}",
        reply.status,
        reply.body
    );
    let document = reply.rendered();
    let parsed = mailparse::parse_mail(document.as_bytes()).unwrap_or_else(|error| {
        panic!("case {case:?} ({when}): expected a MIME document, got {error} for {document:?}")
    });
    assert_eq!(
        parsed.ctype.mimetype, expected.top_level,
        "case {case:?} ({when}): expected the document's own media type to follow the stored \
         parts, got {:?} in {document:?}",
        parsed.ctype.mimetype
    );
    assert_eq!(
        parsed.headers.get_first_value("Subject").as_deref(),
        Some(expected.subject),
        "case {case:?} ({when}): expected the rendered subject line, got {:?} in {document:?}",
        parsed.headers.get_first_value("Subject")
    );
    assert_eq!(
        part_of(&parsed, "text/plain").as_deref(),
        expected.text,
        "case {case:?} ({when}): expected the rendered text part, got {:?} in {document:?}",
        part_of(&parsed, "text/plain")
    );
    assert_eq!(
        part_of(&parsed, "text/html").as_deref(),
        expected.html,
        "case {case:?} ({when}): expected the rendered HTML part, got {:?} in {document:?}",
        part_of(&parsed, "text/html")
    );
    document
}

/// One row of the update table: the content first saved, the content the update replaces
/// it with, and the document each of the two renders.
struct UpdateCase {
    case: &'static str,
    before: fn() -> Value,
    after: fn() -> Value,
    rendered_before: Rendered,
    rendered_after: Rendered,
}

/// The rows span what an update can change about the stored content, since that is what
/// the preview is a function of: the subject line alone, the language a part is written
/// in, which attribute a part names, which part carries the body, a part added and a part
/// removed.
const UPDATE_CASES: &[UpdateCase] = &[
    UpdateCase {
        case: "the subject line alone",
        before: || json!({"Subject": "Receipt for {{name}}", "Text": "Thanks {{name}}."}),
        after: || json!({"Subject": "Your receipt, {{name}}", "Text": "Thanks {{name}}."}),
        rendered_before: Rendered {
            top_level: "text/plain",
            subject: "Receipt for Ada",
            text: Some("Thanks Ada."),
            html: None,
        },
        rendered_after: Rendered {
            top_level: "text/plain",
            subject: "Your receipt, Ada",
            text: Some("Thanks Ada."),
            html: None,
        },
    },
    UpdateCase {
        case: "the language a part is written in",
        before: || json!({"Subject": "s", "Text": "Hi {{name}}"}),
        after: || json!({"Subject": "s", "Text": "{{#if name}}Hi {{name}}{{else}}Hi{{/if}}!"}),
        rendered_before: Rendered {
            top_level: "text/plain",
            subject: "s",
            text: Some("Hi Ada"),
            html: None,
        },
        rendered_after: Rendered {
            top_level: "text/plain",
            subject: "s",
            text: Some("Hi Ada!"),
            html: None,
        },
    },
    UpdateCase {
        case: "how often a part names the same attribute",
        before: || json!({"Subject": "s", "Text": "Hi {{name}}"}),
        after: || json!({"Subject": "s", "Text": "Hi {{name}} and {{name}}"}),
        rendered_before: Rendered {
            top_level: "text/plain",
            subject: "s",
            text: Some("Hi Ada"),
            html: None,
        },
        rendered_after: Rendered {
            top_level: "text/plain",
            subject: "s",
            text: Some("Hi Ada and Ada"),
            html: None,
        },
    },
    UpdateCase {
        case: "the text part replaced by an HTML one, so the media type follows it",
        before: || json!({"Subject": "s", "Text": "Hi {{name}}"}),
        after: || json!({"Subject": "s", "Html": "<p>Hi {{name}}</p>"}),
        rendered_before: Rendered {
            top_level: "text/plain",
            subject: "s",
            text: Some("Hi Ada"),
            html: None,
        },
        rendered_after: Rendered {
            top_level: "text/html",
            subject: "s",
            text: None,
            html: Some("<p>Hi Ada</p>"),
        },
    },
    UpdateCase {
        case: "a second part added, so the document becomes a multipart one",
        before: || json!({"Subject": "s", "Text": "Hi {{name}}"}),
        after: || json!({"Subject": "s", "Text": "Hi {{name}}", "Html": "<p>Hi {{name}}</p>"}),
        rendered_before: Rendered {
            top_level: "text/plain",
            subject: "s",
            text: Some("Hi Ada"),
            html: None,
        },
        rendered_after: Rendered {
            top_level: "multipart/alternative",
            subject: "s",
            text: Some("Hi Ada"),
            html: Some("<p>Hi Ada</p>"),
        },
    },
    UpdateCase {
        case: "a part removed, so the document carries only what is left",
        before: || json!({"Subject": "s", "Text": "Hi {{name}}", "Html": "<p>Hi {{name}}</p>"}),
        after: || json!({"Subject": "s", "Html": "<p>Hi {{name}}</p>"}),
        rendered_before: Rendered {
            top_level: "multipart/alternative",
            subject: "s",
            text: Some("Hi Ada"),
            html: Some("<p>Hi Ada</p>"),
        },
        rendered_after: Rendered {
            top_level: "text/html",
            subject: "s",
            text: None,
            html: Some("<p>Hi Ada</p>"),
        },
    },
];

#[tokio::test]
async fn an_accepted_update_changes_what_a_later_preview_renders() {
    for row in UPDATE_CASES {
        let case = row.case;
        let ses = Ses::new();
        let created = ses.create_template(TEMPLATE, (row.before)()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the template to be saved, got {} {:?}",
            created.status, created.body
        );

        let before = ses.render(TEMPLATE, DATA).await;
        let before = assert_rendered(case, "before the update", &before, &row.rendered_before);

        let updated = row.after;
        let replaced = ses.update_template(TEMPLATE, updated()).await;
        assert_eq!(
            replaced.status, 200,
            "case {case:?}: expected the update to be accepted, got {} {:?}",
            replaced.status, replaced.body
        );

        let after = ses.render(TEMPLATE, DATA).await;
        let after = assert_rendered(case, "after the update", &after, &row.rendered_after);
        assert_ne!(
            before, after,
            "case {case:?}: expected the update to change the document the preview renders"
        );

        // The round trip: the reader reports the content the preview rendered from.
        let read = ses.get_template(TEMPLATE).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the template to be readable, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body.get("TemplateContent"),
            Some(&updated()),
            "case {case:?}: expected the stored content to be the update's, got {:?}",
            read.body.get("TemplateContent")
        );
    }
}

/// One row of the refused-create table: how the second create under the taken name
/// differs from the first, and the tags it attaches.
struct DuplicateCase {
    case: &'static str,
    /// The content the second create carries, which the account never comes to hold.
    content: fn() -> Value,
    /// The tags the second create attaches, which selects which of the two sentences
    /// SES's naming conflict is worded with.
    tags: &'static [(&'static str, &'static str)],
}

/// The rows span how a refused create can differ from the template the account holds:
/// another subject line, other parts, the same content again, and a create that attaches
/// tags.
const DUPLICATE_CASES: &[DuplicateCase] = &[
    DuplicateCase {
        case: "another subject line",
        content: || json!({"Subject": "Replaced {{name}}", "Text": "Hi {{name}}"}),
        tags: &[],
    },
    DuplicateCase {
        case: "other parts, which would render another document shape",
        content: || json!({"Subject": "s", "Html": "<p>Replaced {{name}}</p>"}),
        tags: &[],
    },
    DuplicateCase {
        case: "the same content again",
        content: || json!({"Subject": "Receipt for {{name}}", "Text": "Thanks {{name}}."}),
        tags: &[],
    },
    DuplicateCase {
        case: "a create that attaches tags",
        content: || json!({"Subject": "Replaced", "Text": "Replaced"}),
        tags: &[("owner", "billing")],
    },
];

#[tokio::test]
async fn a_refused_duplicate_create_leaves_the_preview_rendering_the_original() {
    /// The content the account holds throughout, whatever the refused create carried.
    fn original() -> Value {
        json!({"Subject": "Receipt for {{name}}", "Text": "Thanks {{name}}."})
    }
    let expected = Rendered {
        top_level: "text/plain",
        subject: "Receipt for Ada",
        text: Some("Thanks Ada."),
        html: None,
    };

    for row in DUPLICATE_CASES {
        let case = row.case;
        let ses = Ses::new();
        let created = ses.create_template(TEMPLATE, original()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the template to be saved, got {} {:?}",
            created.status, created.body
        );

        let before = ses.render(TEMPLATE, DATA).await;
        let before = assert_rendered(case, "before the refused create", &before, &expected);

        let again = ses
            .create_tagged_template(TEMPLATE, (row.content)(), row.tags)
            .await;
        assert_eq!(
            (again.status, again.error_code().as_str()),
            (400, "AlreadyExistsException"),
            "case {case:?}: expected the second create to be refused, got {} {:?}",
            again.status,
            again.body
        );

        let after = ses.render(TEMPLATE, DATA).await;
        let after = assert_rendered(case, "after the refused create", &after, &expected);
        assert_eq!(
            before, after,
            "case {case:?}: expected the refused create to change nothing the preview renders"
        );

        let read = ses.get_template(TEMPLATE).await;
        assert_eq!(
            read.body.get("TemplateContent"),
            Some(&original()),
            "case {case:?}: expected the stored content to be the original, got {:?}",
            read.body.get("TemplateContent")
        );
    }
}

/// One row of the non-ASCII table: the content to save, the value the data states for
/// `name`, and the parts that value is rendered into.
struct NonAsciiCase {
    case: &'static str,
    content: fn() -> Value,
    /// The value the rendering data states, which is what the document has to carry.
    value: &'static str,
    subject: &'static str,
    text: Option<&'static str>,
    html: Option<&'static str>,
}

/// The rows span where a value lands (the subject line, one body part, every part) and
/// what the value is (US-ASCII, Latin letters outside it, a script outside the Latin one,
/// and a character outside the basic multilingual plane).
const NON_ASCII_CASES: &[NonAsciiCase] = &[
    NonAsciiCase {
        case: "a US-ASCII value in every part, which needs no encoding at all",
        content: || {
            json!({
                "Subject": "Receipt {{name}}",
                "Text": "Hi {{name}}",
                "Html": "<p>Hi {{name}}</p>",
            })
        },
        value: "Ada",
        subject: "Receipt Ada",
        text: Some("Hi Ada"),
        html: Some("<p>Hi Ada</p>"),
    },
    NonAsciiCase {
        case: "Latin letters outside US-ASCII in every part",
        content: || {
            json!({
                "Subject": "Receipt {{name}}",
                "Text": "Hi {{name}}",
                "Html": "<p>Hi {{name}}</p>",
            })
        },
        value: "Zoé Grüße",
        subject: "Receipt Zoé Grüße",
        text: Some("Hi Zoé Grüße"),
        html: Some("<p>Hi Zoé Grüße</p>"),
    },
    NonAsciiCase {
        case: "a script outside the Latin one, and an emoji outside the basic plane",
        content: || {
            json!({
                "Subject": "Receipt {{name}}",
                "Text": "Hi {{name}}",
                "Html": "<p>Hi {{name}}</p>",
            })
        },
        value: "領収書 😀",
        subject: "Receipt 領収書 😀",
        text: Some("Hi 領収書 😀"),
        html: Some("<p>Hi 領収書 😀</p>"),
    },
    NonAsciiCase {
        case: "the value in the subject line alone, so the text part stays US-ASCII",
        content: || json!({"Subject": "Receipt {{name}}", "Text": "Hi there"}),
        value: "Zoé",
        subject: "Receipt Zoé",
        text: Some("Hi there"),
        html: None,
    },
    NonAsciiCase {
        case: "the value in the text part alone, so the subject line stays US-ASCII",
        content: || json!({"Subject": "Receipt", "Text": "Hi {{name}}"}),
        value: "Zoé",
        subject: "Receipt",
        text: Some("Hi Zoé"),
        html: None,
    },
    NonAsciiCase {
        case: "the value in the HTML part alone",
        content: || json!({"Subject": "Receipt", "Html": "<p>Hi {{name}}</p>"}),
        value: "Zoé",
        subject: "Receipt",
        text: None,
        html: Some("<p>Hi Zoé</p>"),
    },
];

#[tokio::test]
async fn non_ascii_rendering_data_travels_mime_encoded_and_decodes_back() {
    for row in NON_ASCII_CASES {
        let case = row.case;
        let ses = Ses::new();
        let created = ses.create_template(TEMPLATE, (row.content)()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the template to be saved, got {} {:?}",
            created.status, created.body
        );

        let data = json!({"name": row.value}).to_string();
        let reply = ses.render(TEMPLATE, &data).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected the render to be answered with 200, got {} {:?}",
            reply.status, reply.body
        );
        let document = reply.rendered();
        let parsed = mailparse::parse_mail(document.as_bytes()).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a MIME document, got {error} for {document:?}")
        });

        // A header field is US-ASCII text, whatever the data substituted into it was, and
        // it decodes back to exactly what the template rendered.
        let block = header_block(&document);
        assert!(
            block.is_ascii(),
            "case {case:?}: expected the document's header block to be US-ASCII, got {block:?}"
        );
        assert_eq!(
            parsed.headers.get_first_value("Subject").as_deref(),
            Some(row.subject),
            "case {case:?}: expected the subject line to decode back, got {:?} in {document:?}",
            parsed.headers.get_first_value("Subject")
        );

        // Each body part states the encoding its text is written in, so that text
        // decodes back to exactly what was rendered. WHICH encoding it states is not
        // asserted: `7bit`, `8bit`, `base64` and quoted-printable all carry the same text
        // (RFC 2045 section 6), so a case that named one would pin a choice rather than
        // the rule, and the decode is what the rule is about.
        for (media_type, expected) in [("text/plain", row.text), ("text/html", row.html)] {
            assert_eq!(
                part_of(&parsed, media_type).as_deref(),
                expected,
                "case {case:?}: expected the {media_type} part to decode back, got {:?} in \
                 {document:?}",
                part_of(&parsed, media_type)
            );
            let Some(part) = part_with(&parsed, media_type) else {
                continue;
            };
            assert!(
                part.headers
                    .get_first_value("Content-Transfer-Encoding")
                    .is_some(),
                "case {case:?}: expected the {media_type} part to state its transfer \
                 encoding, got {document:?}"
            );
        }
    }
}
