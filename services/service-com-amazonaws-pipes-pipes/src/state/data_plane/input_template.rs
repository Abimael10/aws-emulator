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
//! A path that selects no one value of the event stands for the EMPTY STRING: the render
//! carries that placeholder as nothing at all and the record is still delivered, rather
//! than the event being failed over a member it happens not to carry — and never a payload
//! carrying the path itself.
//!
//! A record's PAYLOAD member (an Amazon SQS body, a Kinesis record's data) is the one value
//! the record carries as TEXT the producer wrote, so the two readings of a record meet
//! there: a path DESCENDING into it names a value of the document that text spells, while a
//! placeholder selecting the member AS A WHOLE stands for the payload itself. Where the
//! place that placeholder stands in takes the producer's own bytes, they are what the render
//! carries, byte for byte ([`payload_as_sent`]) — a pipe whose template is that one
//! placeholder hands the stage the payload unchanged rather than a serializer's rendering of
//! the document it spells. Those places are the ones the form above already names: a
//! template that is not JSON text, whose placeholders carry their value's own characters,
//! and a placeholder standing where a JSON VALUE stands whose payload spells a document,
//! which is the one form whose own text IS the JSON of that value. Everywhere else the
//! payload is written like any other value of its type — inside a string literal the
//! template's quotes do the quoting, so a payload spelling no document contributes its
//! characters escaped for that literal — and a payload spelling no document standing as a
//! JSON value is the string it is, quoted by the render.
//!
//! The template's own JSON, on the other hand, is the AUTHOR's to get right, and it is read
//! at authoring time rather than at delivery time: a template that is JSON text but spells
//! no JSON document even once each of its placeholders stands for a value is one no write
//! stores ([`spells_malformed_json`]).
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

/// The characters a template whose own JSON is READ opens with: the two that open a JSON
/// document, and the quote that opens a JSON string — every form of the "valid JSON text"
/// an `InputTemplate` is documented to be ([`spells_malformed_json`]). A template opening
/// with anything else spells no JSON at all and carries none to read.
const JSON_TEXT_OPENERS: [char; 3] = ['{', '[', JSON_STRING_QUOTE_CHARACTER];

/// What stands where a placeholder standing as a JSON VALUE stands when the template's own
/// JSON is read with no event to fill it: a JSON literal, so what is read is the
/// template's STRUCTURE and not the type of the value a placeholder will carry.
const JSON_VALUE_STAND_IN: &str = "null";

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
///
/// `event` is the record envelope as the stage is handed it, PAYLOAD MEMBER AS SENT, because
/// the render reads the record both ways and only this one carries the producer's own bytes:
/// a path DESCENDING into the payload is resolved against the document the payload spells
/// ([`path_view`]), while a placeholder selecting that payload AS A WHOLE stands for the
/// payload itself ([`payload_as_sent`]).
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
    // The record read BOTH ways, each reading taken ONCE for the whole render however many
    // placeholders the template carries. The payload AS THE PRODUCER SENT IT is read off the
    // envelope itself, beside whether that text spells a document — the one question the
    // place rule below asks of it, which depends on the payload alone and not on where a
    // placeholder stands. The record as a PATH reads it — that member replaced by the
    // document the payload spells — is built on first use, because a template whose
    // placeholders all select the payload as a whole needs it not at all.
    let as_sent = payload_as_read(event);
    let payload_spells_a_document = as_sent
        .as_ref()
        .is_some_and(|(_, text)| spells_a_document(text));
    let mut read: Option<Cow<'_, Value>> = None;
    let mut rendered = String::with_capacity(template.len());
    let mut taken = 0usize;
    for (span, placeholder) in &placeholders {
        // WHERE this placeholder stands, which decides how whatever it stands for is
        // written: inside one of the template's own string literals, or where a JSON value
        // stands.
        let in_string = structured && inside_a_json_string(template, span.start);
        let as_sent_here = payload_as_sent(
            placeholder,
            as_sent.as_ref(),
            PayloadPlace {
                spells_a_document: payload_spells_a_document,
                structured,
                in_string,
            },
        );
        let written = match as_sent_here {
            Some(payload) => payload.to_string(),
            None => {
                let read = read.get_or_insert_with(|| path_view(event));
                let stands_for = filled(placeholder, read.as_ref(), pipe, received_at)?;
                written(stands_for.as_ref(), structured, in_string)
            }
        };
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

/// Where one placeholder stands, as far as the payload is concerned: whether the payload
/// spells a JSON document, and the two facts about the PLACE the form rules turn on.
///
/// The document question is the payload's alone, so it is answered once per render and
/// carried here rather than asked per placeholder: a template carries up to a thousand
/// placeholders ([`MAX_TEMPLATE_BYTES`]) and the payload is as large as the record its source
/// admits, so asking it per placeholder would multiply one parse by the other.
#[derive(Debug, Clone, Copy)]
struct PayloadPlace {
    /// Whether the payload's own text spells a JSON document ([`spells_a_document`]).
    spells_a_document: bool,
    /// Whether the template's own JSON is kept.
    structured: bool,
    /// Whether this placeholder stands inside one of that JSON's string literals.
    in_string: bool,
}

/// The record's payload AS THE PRODUCER SENT IT when `placeholder` selects the payload
/// member as a WHOLE and the place it stands in takes the producer's own bytes, and `None`
/// for every other placeholder and every other place.
///
/// A record's payload travels as text — an Amazon SQS body as the text its producer sent, a
/// Kinesis record's data as the base64 of the bytes it put — and that text is what the
/// record carries: a placeholder naming the member itself therefore contributes those bytes,
/// not a serializer's rendering of the document they spell, so a pipe whose template is that
/// one placeholder hands the stage the payload unchanged. A placeholder that descends INTO
/// the payload (`$.body.id`) names a value of the document the text spells instead, and is
/// read from [`path_view`] like any other path.
///
/// Which paths select the member itself is decided by the path's own SEGMENTS
/// (`service_common::json_reference_path`), so every spelling of that one segment (`$.body`,
/// `$['body']`) is the same selection, and nothing longer is. It is decided FIRST, so a
/// placeholder naming anything else costs this nothing but its own path's segments.
///
/// WHERE such a placeholder stands decides whether the producer's bytes are what stands
/// there, under the form rules of this module's docs: in a template that is not JSON text
/// every placeholder carries its value's own characters, so they are; standing where a JSON
/// VALUE stands they are only when the payload spells a document, whose own text IS the JSON
/// of that value; and inside one of the template's string literals they never are, because
/// the template's quotes are what quote the value and a string's characters have to be
/// escaped for the literal they land in. Every place this answers `None` for falls through to
/// [`written`], which writes the payload as the value its type makes it.
fn payload_as_sent<'a>(
    placeholder: &Placeholder,
    as_sent: Option<&'a (&'static str, Cow<'_, str>)>,
    place: PayloadPlace,
) -> Option<&'a str> {
    let Placeholder::Path(path) = placeholder else {
        return None;
    };
    let (member, text) = as_sent?;
    let segments = ::service_common::json_reference_path::segments(path)?;
    if segments != [::service_common::json_reference_path::Segment::Key((*member).to_string())] {
        return None;
    }
    let place_takes_the_bytes = match (place.structured, place.in_string) {
        (false, _) => true,
        (true, true) => false,
        (true, false) => place.spells_a_document,
    };
    place_takes_the_bytes.then_some(text.as_ref())
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

/// Whether the JSON an `InputTemplate` spells is BROKEN: the template is JSON text
/// ([`JSON_TEXT_OPENERS`]) and does not parse as a JSON document even once each of its
/// placeholders stands for the value it names.
///
/// This is the template's own text read as the "valid JSON text" the member is documented to
/// be, which is a property of the TEMPLATE and of nothing else — no event, no pipe, no
/// clock — so it is answered for a write that is about to store one, before any record has
/// been polled. A template that opens no JSON at all is text the author wrote, carries no
/// JSON to read, and is left alone here.
///
/// The parse is `serde_json`'s, over text the member's own `@length` already bounds, and it
/// answers with an error rather than failing: a template no parser can read is one this
/// answers `true` for.
///
/// `pub(crate)` because the writes that configure a pipe ask it ([`super::super`]'s
/// configuration rules): the template language lives in this module, so whether a template
/// spells its own JSON is answered here rather than restated at each call site.
pub(crate) fn spells_malformed_json(template: &str) -> bool {
    if !template.trim_start().starts_with(JSON_TEXT_OPENERS) {
        return false;
    }
    ::serde_json::from_str::<Value>(&placeholders_stood_in(template)).is_err()
}

/// `template` with every placeholder replaced by what STANDS where it stands when the
/// template's own JSON is read: a JSON literal where a JSON value stands
/// ([`JSON_VALUE_STAND_IN`]), and nothing at all inside a string literal, whose quotes are
/// the template's own. What is left is the template's JSON with no placeholder in it.
fn placeholders_stood_in(template: &str) -> String {
    let mut text = String::with_capacity(template.len());
    let mut taken = 0usize;
    for (span, _) in placeholders_of(template) {
        text.push_str(&template[taken..span.start]);
        if !inside_a_json_string(template, span.start) {
            text.push_str(JSON_VALUE_STAND_IN);
        }
        taken = span.end;
    }
    text.push_str(&template[taken..]);
    text
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
/// event; a path that selects no one value of the event stands for the EMPTY STRING, so a
/// member the event happens not to carry is rendered as nothing rather than failing the
/// record. A reserved variable is read from the pipe's own configuration, the clock, or the
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
            return Ok(::service_common::json_reference_path::select(path, event)
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(Value::String(String::new()))))
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
    use super::{render_template, spells_malformed_json, PipeVariables, RESERVED_VARIABLES};
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

    /// RULE: a path that selects no one value of the event stands for the EMPTY STRING, so
    /// the render succeeds and the rest of the template is delivered as written.
    ///
    /// The rows differ along where the unresolved placeholder stands (as a JSON value,
    /// inside a string literal, in a template that is not JSON text), how deep the path
    /// reaches past what the event carries, and what resolves beside it.
    #[test]
    fn a_path_that_selects_nothing_stands_for_the_empty_string() {
        let event = event();
        // (case, the template, the bytes the stage is handed)
        let rows: Vec<(&str, &str, &str)> = vec![
            (
                "absent-member-inside-a-string-literal",
                r#"{"v": "<$.body.missing>"}"#,
                r#"{"v": ""}"#,
            ),
            (
                "absent-member-as-a-json-value",
                r#"{"v": <$.body.missing>}"#,
                r#"{"v": ""}"#,
            ),
            (
                "absent-beside-a-path-that-resolves",
                r#"{"v": "<$.body.missing>", "t": "<$.body.text>"}"#,
                r#"{"v": "", "t": "hello"}"#,
            ),
            (
                "a-path-under-a-value-that-is-no-document",
                r#"{"v": "<$.body.text.deeper>"}"#,
                r#"{"v": ""}"#,
            ),
            (
                "absent-in-a-text-template",
                "missing=<$.body.missing>;",
                "missing=;",
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

    /// RULE: a template that is JSON TEXT spells a JSON document once each of its
    /// placeholders stands for a value, and one that does not spells broken JSON; a
    /// template that opens no JSON at all carries none to read.
    ///
    /// The rows differ along the form of the template (object, array, quoted string, bare
    /// text), where its placeholders stand, and HOW the JSON is broken (an unterminated
    /// placeholder, an unclosed object or array, a missing value, a trailing comma, an
    /// unquoted key).
    #[test]
    fn a_json_text_template_that_spells_no_json_is_malformed() {
        // (case, the template, whether its own JSON is broken)
        let rows: Vec<(&str, &str, bool)> = vec![
            ("an-object-with-a-quoted-path", r#"{"ok": "<$.body.text>"}"#, false),
            ("an-object-with-an-unquoted-path", r#"{"ok": <$.body.text>}"#, false),
            (
                "an-object-with-a-reserved-variable",
                r#"{"event": <aws.pipes.event.json>}"#,
                false,
            ),
            ("a-json-array", r#"[<$.body.count>, "<$.body.text>"]"#, false),
            ("a-constant-object", r#"{"fixed": true}"#, false),
            ("a-quoted-text-template", "\"order <$.body.text>\"", false),
            ("a-bare-text-template", "order <$.body.text> received", false),
            ("a-bare-path", "<$.body>", false),
            ("a-bare-path-with-no-brackets", "$.body", false),
            // The template whose placeholder never closes: no placeholder stands in it at
            // all, and the object it opened is never closed either.
            ("an-unterminated-placeholder", r#"{"broken": <$.body.marker"#, true),
            ("an-unclosed-object", r#"{"ok": "<$.body.text>""#, true),
            ("an-unclosed-array", r#"[<$.body.count>"#, true),
            ("a-member-with-no-value", r#"{"ok": }"#, true),
            ("a-trailing-comma", r#"{"ok": "<$.body.text>",}"#, true),
            ("an-unquoted-key", r#"{ok: "<$.body.text>"}"#, true),
            ("an-unterminated-string-template", "\"order <$.body.text>", true),
        ];
        for (case, template, broken) in rows {
            assert_eq!(
                spells_malformed_json(template),
                broken,
                "case {case:?}: expected malformed={broken} for {template:?}, got {}",
                spells_malformed_json(template)
            );
        }
    }
}
