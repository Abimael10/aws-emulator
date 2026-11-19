// SPDX-License-Identifier: CC0-1.0

//! The `InputTemplate` RENDERER: what a pipe's enrichment or its target is handed INSTEAD
//! of the event.
//!
//! "Valid JSON text passed to the target. In this case, nothing from the event itself is
//! passed to the target" (`PipeTargetParameters$InputTemplate`, and the same sentence for
//! `PipeEnrichmentParameters$InputTemplate`) — so the template, not the record, is the
//! payload, and the only parts of the event that reach the stage are the ones the template
//! names.
//!
//! A render is a SUBSTITUTION into the template's own text: what the stage is handed is the
//! template byte for byte, with each placeholder replaced by what it stands for, so the
//! template's own punctuation and spacing reach the stage exactly as its author wrote them
//! ([`Rendered`]). Two kinds of placeholder stand in a template:
//!
//! * a JSON PATH between [`PLACEHOLDER_OPEN`] and [`PLACEHOLDER_CLOSE`] (`<$.body.id>`,
//!   `<$>` for the whole event), standing for the one value that path selects in the event;
//! * a RESERVED VARIABLE of the `aws.pipes.` namespace ([`RESERVED_VARIABLES`]), standing
//!   for the pipe's own identity, the ARNs it is configured with, the instant this stage
//!   received the event, or the event itself — the values the service fills rather than the
//!   caller.
//!
//! HOW a placeholder is written into the text follows the FORM of the template, decided
//! before any value is substituted:
//!
//! * a template that opens a JSON object or array carries its placeholders "in quotes or
//!   out", so one standing as a JSON value contributes the JSON of the value it stands for
//!   — a string is therefore QUOTED by the render itself — and one standing inside a
//!   string literal of the template contributes that string's own characters, escaped for
//!   the string it lands in, the template's quotes doing the quoting. A value that is no
//!   string (an object, an array, a number) has no characters but its JSON, so inside a
//!   string literal it is written as that JSON, quotes and all — which breaks the literal
//!   the template opened rather than escaping what the author inlined;
//! * any other template is TEXT (a bare placeholder, a quoted one, a sentence): every
//!   placeholder contributes its value's own characters, nothing escapes them, and every
//!   character the template wrote is delivered as written.
//!
//! A path that selects no one value of the event is a FAILED render — the record stays on
//! the source — rather than a payload carrying the path itself.
//!
//! The text is walked ONCE, left to right, its length is bounded ([`MAX_TEMPLATE_BYTES`])
//! and what the placeholders ADD is bounded by [`MAX_RENDERED_PAYLOAD`], because a template
//! may name the same value many times. Each path is read by
//! `service_common::json_reference_path` and the JSON the render spells by `serde_json`, not
//! by anything spelled out here.

use super::*;

/// The namespace EventBridge Pipes reserves for the variables it fills itself, so a
/// `<…>` span opening with it is never a caller's path.
const RESERVED_VARIABLE_PREFIX: &str = "aws.pipes.";

/// The pipe's own ARN and name, as the reserved variables of that namespace name them.
const PIPE_ARN_VARIABLE: &str = "aws.pipes.pipe-arn";
const PIPE_NAME_VARIABLE: &str = "aws.pipes.pipe-name";

/// The ARNs of the resources the pipe is configured with: its source, its enrichment and
/// its target.
const SOURCE_ARN_VARIABLE: &str = "aws.pipes.source-arn";
const ENRICHMENT_ARN_VARIABLE: &str = "aws.pipes.enrichment-arn";
const TARGET_ARN_VARIABLE: &str = "aws.pipes.target-arn";

/// The instant the stage being rendered for received the event.
const INGESTION_TIME_VARIABLE: &str = "aws.pipes.event.ingestion-time";

/// The event itself: as the TEXT of its document, which a template may put anywhere, and
/// as the DOCUMENT that text spells, which stands where a JSON value stands.
const EVENT_VARIABLE: &str = "aws.pipes.event";
const EVENT_JSON_VARIABLE: &str = "aws.pipes.event.json";

/// Every variable of the reserved namespace this renderer fills, named once: a `<…>` span
/// naming one of these is a placeholder and one naming anything else in the namespace is
/// text the template wrote.
///
/// Source: the reserved-variable table of the EventBridge Pipes input-transformation
/// documentation, which names these eight and no others.
const RESERVED_VARIABLES: [&str; 8] = [
    PIPE_ARN_VARIABLE,
    PIPE_NAME_VARIABLE,
    SOURCE_ARN_VARIABLE,
    ENRICHMENT_ARN_VARIABLE,
    TARGET_ARN_VARIABLE,
    INGESTION_TIME_VARIABLE,
    EVENT_VARIABLE,
    EVENT_JSON_VARIABLE,
];

/// The characters a template that is JSON TEXT opens with: the ones that open a JSON
/// object and a JSON array, which are the templates whose own JSON a render keeps.
const JSON_DOCUMENT_OPENERS: [char; 2] = ['{', '['];

/// What the reserved variables of an `InputTemplate` render to: the identity of the pipe
/// whose stage is being handed the payload, and the resources it is configured with.
///
/// Read from the pipe's own record with the rest of the configuration
/// ([`RunningPipe::template_variables`]), so a template renders what the record says NOW.
#[derive(Debug, Clone, Copy)]
pub(super) struct PipeVariables<'a> {
    /// The pipe's own ARN, and the name that ARN ends in.
    pub(super) pipe_arn: &'a str,
    pub(super) pipe_name: &'a str,
    /// The `Source` the records are polled from.
    pub(super) source_arn: &'a str,
    /// The `Enrichment` the batch is handed to, or the empty text for a pipe that
    /// configures none — the variable then stands for no resource at all.
    pub(super) enrichment_arn: &'a str,
    /// The `Target` the delivery is made to.
    pub(super) target_arn: &'a str,
}

/// One event rendered through an `InputTemplate`: what the stage the template belongs to
/// is handed INSTEAD of the event.
///
/// It carries BOTH readings of one render, because a target takes one or the other and
/// neither is the other's serialization: the BYTES the render produced
/// ([`Rendered::body`]), which is what a target carrying one payload of text delivers —
/// an Amazon SQS message body, an event bus entry's `Detail` — and the JSON VALUE those
/// bytes spell ([`Rendered::value`]), which is what a target whose call carries a document
/// delivers, as one element of the array that document is. A render whose bytes spell no
/// JSON document is the text they are, so a target handed a value is handed that text.
#[derive(Debug, Clone)]
pub(super) struct Rendered {
    /// The template's own text with every placeholder filled, byte for byte.
    text: String,
    /// The JSON those bytes spell, or the text itself when they spell none.
    value: Value,
}

impl Rendered {
    /// One render, read both ways.
    fn of(text: String) -> Self {
        let value = ::serde_json::from_str::<Value>(&text)
            .unwrap_or_else(|_| Value::String(text.clone()));
        Self { text, value }
    }

    /// The bytes the render produced.
    pub(super) fn body(&self) -> &str {
        &self.text
    }

    /// The JSON value those bytes spell.
    pub(super) fn value(&self) -> &Value {
        &self.value
    }
}

/// `event` rendered through `template` for the pipe `pipe` describes — the substitution
/// this module's docs state, or why the render failed.
pub(super) fn render_template(
    template: &str,
    event: &Value,
    pipe: &PipeVariables<'_>,
) -> Result<Rendered, String> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(format!(
            "the input template is {} bytes, past the {MAX_TEMPLATE_BYTES} a template carries",
            template.len()
        ));
    }
    let placeholders = placeholders_of(template);
    if placeholders.is_empty() {
        // A constant template: the text it is, which is also the payload it is.
        return Ok(Rendered::of(template.to_string()));
    }
    // The instant this stage received the event, read ONCE for the whole render, so a
    // template naming the ingestion time twice does not render two instants.
    let received_at = SystemTime::now();
    // Whether the template's own JSON is kept, decided from the TEMPLATE and not from what
    // the substitution happens to produce: a render that breaks the template's JSON is
    // still the bytes the template's author asked for.
    let structured = opens_a_json_document(template);
    let mut rendered = String::with_capacity(template.len());
    let mut taken = 0usize;
    for (span, placeholder) in &placeholders {
        let stands_for = filled(placeholder, event, pipe, received_at)?;
        let written = written(
            stands_for.as_ref(),
            structured,
            structured && inside_a_json_string(template, span.start),
        );
        let before = &template[taken..span.start];
        // Checked BEFORE the text grows, so what the render holds never passes the bound
        // rather than passing it and being refused afterwards.
        within_the_payload_bound(rendered.len() + before.len() + written.len())?;
        rendered.push_str(before);
        rendered.push_str(&written);
        taken = span.end;
    }
    let rest = &template[taken..];
    within_the_payload_bound(rendered.len() + rest.len())?;
    rendered.push_str(rest);
    Ok(Rendered::of(rendered))
}

/// Whether a render of `bytes` is one this emulator delivers, or why it is not.
fn within_the_payload_bound(bytes: usize) -> Result<(), String> {
    if bytes > MAX_RENDERED_PAYLOAD {
        return Err(format!(
            "the input template renders more than {MAX_RENDERED_PAYLOAD} bytes for one event, \
             which is more than a payload this emulator delivers"
        ));
    }
    Ok(())
}

/// Whether `template` is JSON TEXT — it opens a JSON object or array — so the render keeps
/// its own JSON and writes each placeholder as that JSON takes it.
fn opens_a_json_document(template: &str) -> bool {
    template.trim_start().starts_with(JSON_DOCUMENT_OPENERS)
}

/// What one placeholder of a template STANDS FOR.
#[derive(Debug, Clone)]
enum Placeholder {
    /// A JSON path into the event: the one value it selects there.
    Path(String),
    /// One of the reserved variables the service fills itself
    /// ([`RESERVED_VARIABLES`]), by its name.
    Reserved(&'static str),
}

/// The placeholders of `template`, in the order they appear: where each one sits and what
/// it stands for.
///
/// Only a `<…>` span whose content is a JSON path ([`DYNAMIC_PATH_PREFIX`], or the event
/// itself) or a reserved variable of the `aws.pipes.` namespace is a placeholder, so a
/// template carrying `<` for any other reason is left alone — and the scan then resumes
/// just PAST that `<`, so a stray one cannot swallow the placeholder behind it.
fn placeholders_of(template: &str) -> Vec<(std::ops::Range<usize>, Placeholder)> {
    let mut found: Vec<(std::ops::Range<usize>, Placeholder)> = Vec::new();
    let mut rest = template;
    let mut base = 0usize;
    while let Some(open) = rest.find(PLACEHOLDER_OPEN) {
        let after_open = open + PLACEHOLDER_OPEN.len_utf8();
        let Some(close) = rest[after_open..].find(PLACEHOLDER_CLOSE) else {
            break;
        };
        let end = after_open + close;
        let content = rest[after_open..end].trim();
        let consumed = match stands_for(content) {
            Some(placeholder) => {
                found.push((
                    base + open..base + end + PLACEHOLDER_CLOSE.len_utf8(),
                    placeholder,
                ));
                end + PLACEHOLDER_CLOSE.len_utf8()
            }
            // Not a placeholder: only this `<` is consumed, because the span it opened
            // may hold the real one.
            None => after_open,
        };
        base += consumed;
        rest = &rest[consumed..];
    }
    found
}

/// What the content of a `<…>` span stands for, or `None` when it stands for nothing and
/// is therefore text the template wrote.
fn stands_for(content: &str) -> Option<Placeholder> {
    if content == WHOLE_EVENT_PATH || content.starts_with(DYNAMIC_PATH_PREFIX) {
        return Some(Placeholder::Path(content.to_string()));
    }
    if !content.starts_with(RESERVED_VARIABLE_PREFIX) {
        return None;
    }
    RESERVED_VARIABLES
        .iter()
        .find(|variable| **variable == content)
        .map(|variable| Placeholder::Reserved(variable))
}

/// The value one placeholder stands for, or why the render fails.
///
/// A path is read through `service_common::json_reference_path`, the one bounded evaluation
/// of a caller-supplied path in this tree, so a template cannot ask for a walk of the whole
/// event. A reserved variable is read from the pipe's own configuration, the clock, or the
/// event as the stage's readers read it:
///
/// * the pipe's ARN and name, and the ARNs of its source, enrichment and target, as the
///   text they are;
/// * the ingestion time, as the instant this stage received the event;
/// * the event as the TEXT of its document, which carries anywhere a text value does, and
///   the event as the DOCUMENT itself, which stands where a JSON value stands.
fn filled<'a>(
    placeholder: &Placeholder,
    event: &'a Value,
    pipe: &PipeVariables<'_>,
    received_at: SystemTime,
) -> Result<Cow<'a, Value>, String> {
    let variable = match placeholder {
        Placeholder::Path(path) => {
            return ::service_common::json_reference_path::select(path, event)
                .map(Cow::Borrowed)
                .ok_or_else(|| {
                    format!("the input template's {path} names no one value of the event")
                })
        }
        Placeholder::Reserved(variable) => *variable,
    };
    let text = match variable {
        PIPE_ARN_VARIABLE => pipe.pipe_arn.to_string(),
        PIPE_NAME_VARIABLE => pipe.pipe_name.to_string(),
        SOURCE_ARN_VARIABLE => pipe.source_arn.to_string(),
        ENRICHMENT_ARN_VARIABLE => pipe.enrichment_arn.to_string(),
        TARGET_ARN_VARIABLE => pipe.target_arn.to_string(),
        INGESTION_TIME_VARIABLE => ingestion_time(received_at),
        EVENT_VARIABLE => event.to_string(),
        // The event as the document it is, which is what a template naming it writes
        // where a JSON value stands.
        _ => return Ok(Cow::Borrowed(event)),
    };
    Ok(Cow::Owned(Value::String(text)))
}

/// The instant `at` as the ingestion time a reserved variable carries: the RFC 3339 /
/// ISO 8601 UTC form, truncated to the second, so a rendered time never names an instant
/// later than the stage that rendered it.
fn ingestion_time(at: SystemTime) -> String {
    let whole_seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| SystemTime::UNIX_EPOCH + Duration::from_secs(since.as_secs()))
        .unwrap_or(at);
    ::aws_smithy_types::DateTime::from(whole_seconds)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// How the value a placeholder stands for is WRITTEN into the render: the form this
/// module's docs state, by where the placeholder stands.
///
/// `structured` is whether the template's own JSON is kept, and `in_string` whether the
/// placeholder stands inside one of that JSON's string literals.
fn written(value: &Value, structured: bool, in_string: bool) -> String {
    match value {
        // A template that is not JSON text escapes nothing: every placeholder carries its
        // value's own characters.
        _ if !structured => value_text(value),
        // Inside a string literal the template supplies the quotes, so a STRING carries
        // its characters escaped for the literal it lands in.
        Value::String(text) if in_string => json_string_body(text),
        // Everywhere else — standing as a JSON value, and standing inside a literal with
        // no characters of its own to carry — the value carries its own JSON.
        other => other.to_string(),
    }
}

/// Whether the placeholder that starts at `upto` sits INSIDE a quoted string of the
/// template, so the template's own quotes are what quote the value.
///
/// Decided by walking the text before it and counting the quotes that open and close a
/// JSON string (a backslash escapes the next character inside one), so a placeholder with
/// text around it inside the same string is read as quoted too.
fn inside_a_json_string(template: &str, upto: usize) -> bool {
    let mut quoted = false;
    let mut escaped = false;
    for character in template[..upto].chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            JSON_STRING_ESCAPE if quoted => escaped = true,
            JSON_STRING_QUOTE_CHARACTER => quoted = !quoted,
            _ => {}
        }
    }
    quoted
}

/// A value as its own TEXT: the contents of a string, the JSON of anything else (a number,
/// a boolean, an object or an array has no text but its JSON).
fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `text` as the BODY of a JSON string — its JSON encoding without the surrounding
/// quotes — so a template's own quotes quote it, whatever characters it carries.
fn json_string_body(text: &str) -> String {
    let encoded = Value::String(text.to_string()).to_string();
    encoded
        .strip_prefix(JSON_STRING_QUOTE)
        .and_then(|rest| rest.strip_suffix(JSON_STRING_QUOTE))
        .unwrap_or(&encoded)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{render_template, PipeVariables, RESERVED_VARIABLES};
    use ::serde_json::{json, Value};

    /// The pipe every render below belongs to: the identity and the ARNs its reserved
    /// variables render.
    const PIPE_ARN: &str = "arn:aws:pipes:us-west-2:000000000000:pipe/orders-to-results";
    const PIPE_NAME: &str = "orders-to-results";
    const SOURCE_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:orders";
    const ENRICHMENT_ARN: &str = "arn:aws:lambda:us-west-2:000000000000:function:enrich";
    const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:results";

    fn pipe() -> PipeVariables<'static> {
        PipeVariables {
            pipe_arn: PIPE_ARN,
            pipe_name: PIPE_NAME,
            source_arn: SOURCE_ARN,
            enrichment_arn: ENRICHMENT_ARN,
            target_arn: TARGET_ARN,
        }
    }

    /// The event the rows below render from, as a reader of a record reads one: an Amazon
    /// SQS envelope whose body spells a JSON document.
    fn event() -> Value {
        json!({
            "messageId": "4fd2-1",
            "body": {
                "text": "hello",
                "object": { "key": "value" },
                "quoted": "she said \"hi\" today",
                "count": 3,
                "flag": true,
                "list": [1, 2],
            },
        })
    }

    /// RULE: a render is a SUBSTITUTION into the template's own text — what the stage is
    /// handed is the template byte for byte with each placeholder filled — and HOW a value
    /// is written follows the FORM of the template: in JSON text a placeholder standing as
    /// a value carries the value's JSON (so a string is quoted by the render), one standing
    /// inside a string literal carries a string's characters escaped for that literal and
    /// any other value's own JSON, and in a template that is not JSON text every
    /// placeholder carries its value's characters unescaped.
    ///
    /// The rows differ along what the rule generalises over: the form of the template (JSON
    /// object, JSON array, quoted text, bare text), where the placeholder stands (as a
    /// value, inside a literal, with text around it), and the type of the value it carries
    /// (string, string holding a quote, object, array, number, boolean).
    #[test]
    fn a_render_is_the_templates_own_text_with_every_placeholder_filled() {
        let event = event();
        // (case, the template, the bytes the stage is handed)
        let rows: Vec<(&str, &str, &str)> = vec![
            (
                // An unquoted path whose value is a string is quoted BY THE RENDER, and the
                // spacing the template's author wrote is delivered as written.
                "an-unquoted-string-is-auto-quoted",
                r#"{"v": <$.body.text>}"#,
                r#"{"v": "hello"}"#,
            ),
            (
                // An object inlined INSIDE a string literal keeps its own quotes, so the
                // literal the template opened is broken rather than the object escaped.
                "an-object-inlined-in-a-string-literal",
                r#"{"v": "x <$.body.object>"}"#,
                r#"{"v": "x {"key":"value"}"}"#,
            ),
            (
                "an-array-inlined-in-a-string-literal",
                r#"{"v": "x <$.body.list>"}"#,
                r#"{"v": "x [1,2]"}"#,
            ),
            (
                // A quoted path's value is escaped like any JSON string, quote and all.
                "a-quoted-string-is-escaped",
                r#"{"v": "<$.body.quoted>"}"#,
                r#"{"v": "she said \"hi\" today"}"#,
            ),
            (
                "an-unquoted-number-and-boolean",
                r#"{"n": <$.body.count>, "f": <$.body.flag>}"#,
                r#"{"n": 3, "f": true}"#,
            ),
            (
                "an-unquoted-object-as-a-value",
                r#"{"v": <$.body.object>}"#,
                r#"{"v": {"key":"value"}}"#,
            ),
            (
                "a-json-array-template",
                r#"[<$.body.count>, "<$.body.text>"]"#,
                r#"[3, "hello"]"#,
            ),
            (
                // Not JSON text: the characters, with the template's own quotes kept.
                "a-quoted-text-template",
                "\"order <$.body.text>\"",
                "\"order hello\"",
            ),
            (
                "a-bare-text-template",
                "order <$.body.text> received",
                "order hello received",
            ),
            (
                // A text template escapes nothing at all, not even a value holding a quote.
                "a-bare-text-template-with-a-quote-in-the-value",
                "said <$.body.quoted>",
                "said she said \"hi\" today",
            ),
            (
                "a-template-with-no-placeholder",
                r#"{"fixed": true}"#,
                r#"{"fixed": true}"#,
            ),
        ];
        for (case, template, expected) in rows {
            let rendered = render_template(template, &event, &pipe())
                .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
            assert_eq!(
                rendered.body(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                rendered.body()
            );
        }
    }

    /// RULE: the reserved `aws.pipes.*` variables of an `InputTemplate` render the values
    /// the service fills — the pipe's own ARN and name, the ARNs of its source, enrichment
    /// and target, the instant the stage received the event, and the event itself — and a
    /// `<…>` span naming no variable of that namespace is text the template wrote.
    ///
    /// The rows cover every variable of the catalog ([`RESERVED_VARIABLES`]), including the
    /// ones the issue did not cite, and a name the namespace does not hold.
    #[test]
    fn a_reserved_variable_renders_the_value_the_service_fills() {
        let event = event();
        // (case, the template, the rendered bytes)
        let rows: Vec<(&str, &str, String)> = vec![
            (
                "the-pipe-arn",
                "{\"v\": \"<aws.pipes.pipe-arn>\"}",
                format!("{{\"v\": \"{PIPE_ARN}\"}}"),
            ),
            (
                "the-pipe-name",
                "{\"v\": \"<aws.pipes.pipe-name>\"}",
                format!("{{\"v\": \"{PIPE_NAME}\"}}"),
            ),
            (
                "the-source-arn",
                "{\"v\": \"<aws.pipes.source-arn>\"}",
                format!("{{\"v\": \"{SOURCE_ARN}\"}}"),
            ),
            (
                "the-enrichment-arn",
                "{\"v\": \"<aws.pipes.enrichment-arn>\"}",
                format!("{{\"v\": \"{ENRICHMENT_ARN}\"}}"),
            ),
            (
                "the-target-arn",
                "{\"v\": \"<aws.pipes.target-arn>\"}",
                format!("{{\"v\": \"{TARGET_ARN}\"}}"),
            ),
            (
                // Unquoted, the ARN is a string and so is quoted by the render itself.
                "an-unquoted-arn-is-auto-quoted",
                "{\"v\": <aws.pipes.target-arn>}",
                format!("{{\"v\": \"{TARGET_ARN}\"}}"),
            ),
            (
                // The event as TEXT: a value a template may put inside a string literal,
                // escaped for it.
                "the-event-as-text",
                "{\"v\": \"<aws.pipes.event>\"}",
                format!(
                    "{{\"v\": \"{}\"}}",
                    super::json_string_body(&event.to_string())
                ),
            ),
            (
                // The event as the DOCUMENT it is, standing where a JSON value stands.
                "the-event-as-json",
                "{\"v\": <aws.pipes.event.json>}",
                format!("{{\"v\": {}}}", event),
            ),
            (
                // Not a variable of the namespace: the span is text the template wrote.
                "a-name-the-namespace-does-not-hold",
                "{\"v\": \"<aws.pipes.not-a-variable>\"}",
                "{\"v\": \"<aws.pipes.not-a-variable>\"}".to_string(),
            ),
        ];
        for (case, template, expected) in rows {
            let rendered = render_template(template, &event, &pipe())
                .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
            assert_eq!(
                rendered.body(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                rendered.body()
            );
        }

        // The ingestion time is the one reserved value the CLOCK fills, so it is asserted
        // on its FORM: the instant the stage received the event, as a timestamp a consumer
        // can read back.
        let rendered = render_template(
            "{\"at\": \"<aws.pipes.event.ingestion-time>\"}",
            &event,
            &pipe(),
        )
        .unwrap_or_else(|error| panic!("case the-ingestion-time: {error}"));
        let carried = rendered.value()["at"].as_str().unwrap_or_default().to_string();
        assert!(
            ::aws_smithy_types::DateTime::from_str(
                &carried,
                ::aws_smithy_types::date_time::Format::DateTime
            )
            .is_ok(),
            "case the-ingestion-time: expected an instant a consumer can read, got {carried:?}"
        );

        // Every variable of the catalog is filled: none of them reaches a payload as the
        // placeholder text itself.
        for variable in RESERVED_VARIABLES {
            let template = format!("{{\"v\": \"<{variable}>\"}}");
            let rendered = render_template(&template, &event, &pipe())
                .unwrap_or_else(|error| panic!("case {variable:?}: {error}"));
            assert!(
                !rendered.body().contains(&format!("<{variable}>")),
                "case {variable:?}: expected the variable filled, got {:?}",
                rendered.body()
            );
        }
    }
}
