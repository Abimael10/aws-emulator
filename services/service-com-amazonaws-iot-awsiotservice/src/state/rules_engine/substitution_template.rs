// SPDX-License-Identifier: CC0-1.0

//! The SUBSTITUTION TEMPLATES a rule action's parameters carry: `${<sql-expression>}`, and what
//! one is worth for the message that is being acted on.
//!
//! An action parameter is not always a literal. AWS IoT documents the form on the action member
//! that is most explicit about it — "The `hashKeyValue` and `rangeKeyvalue` fields use a
//! substitution template syntax. These templates provide data at runtime. The syntax is as
//! follows: ${sql-expression}. You can specify any valid expression in a WHERE or SELECT clause,
//! including JSON properties, comparisons, calculations, and functions"
//! (`DynamoDBAction`) — so a parameter's value is the text around its templates with each
//! template replaced by what its expression is worth.
//!
//! The expression is read by the same SQL parser a rule's statement is read by
//! ([`::sqlparser`]) and evaluated by the same bounded walk of the published message
//! ([`super::super::rule_expression`]), so a template computes exactly what the statement's own
//! clauses compute and nothing else: no host primitive is reachable from one.
//!
//! What a template is evaluated OVER is the message the broker took and the topic it was
//! published to — NOT the document the statement selected: a template's expression is read on
//! its own, so a name the `SELECT` clause introduced (an `AS` alias) is no part of it.
//!
//! Fails CLOSED, the reading the whole engine takes: a parameter whose template this emulator
//! cannot compute — an expression the parser does not read, one the walk does not evaluate, one
//! the message has no value for, or a template the text never closes — has NO value here, so
//! the action carrying it is left unperformed rather than performed on a guessed address. It is
//! bounded too: at most [`MAX_TEMPLATES`] templates of at most [`MAX_EXPRESSION_CHARACTERS`]
//! each are read out of one parameter, and nothing here panics on a caller's text.

use ::serde_json::Value;
use ::sqlparser::dialect::GenericDialect;
use ::sqlparser::parser::Parser;
use ::sqlparser::tokenizer::Token;

use super::super::rule_expression::{evaluate, Evaluated, Message};

/// The opening of a substitution template, which is what makes a parameter a template rather
/// than a literal.
pub const TEMPLATE_OPENING: &str = "${";

/// The character that closes one.
const TEMPLATE_CLOSING: char = '}';

/// How many templates ONE parameter is read for: a parameter is an address, a key or a name, so
/// far above any a client under test writes, and past it the parameter has no value here.
const MAX_TEMPLATES: usize = 16;

/// How long ONE template's expression may be. The expression is a caller's text parsed by the
/// SQL parser, so the bound is what keeps the reading of one parameter finite.
const MAX_EXPRESSION_CHARACTERS: usize = 1024;

/// How long the RENDERED parameter may be. A template's value comes out of the message, so what
/// one parameter renders to is driven by a published payload; far above any address, key or name
/// an action parameter carries (an object key is at most 1024 characters), and past it the
/// parameter has no value here rather than one that grew with the message.
const MAX_RENDERED_CHARACTERS: usize = 64 * 1024;

/// The ONE message every parameter of one dispatch is rendered against: the topic it was
/// published to and the payload as the document it is.
///
/// Built once per published message and handed to each action, so two parameters of one rule
/// cannot be rendered against two different readings of the same message.
#[derive(Debug, Clone)]
pub struct Parameters {
    /// The topic the message was published to, which the expression's `topic()` reports.
    topic: String,
    /// The payload as the document it is, or `Value::Null` for a payload that is no document —
    /// which carries no attribute, so a template naming one has no value for it.
    document: Value,
}

impl Parameters {
    /// The rendering context of the message `payload` published to `topic`.
    #[must_use]
    pub fn of(topic: &str, payload: &[u8]) -> Self {
        Self {
            topic: topic.to_string(),
            document: ::serde_json::from_slice::<Value>(payload).unwrap_or(Value::Null),
        }
    }

    /// What the action parameter `parameter` is worth for this message: its own text with every
    /// substitution template replaced by the value of the expression inside it.
    ///
    /// `None` is a parameter this engine does not render, which is an action it does not perform
    /// ([the module documentation](self) states that reading). A parameter carrying no template
    /// at all is itself, whatever it holds.
    #[must_use]
    pub fn rendered(&self, parameter: &str) -> Option<String> {
        if !parameter.contains(TEMPLATE_OPENING) {
            return Some(parameter.to_string());
        }
        let message = Message {
            topic: &self.topic,
            document: &self.document,
        };
        let mut rendered = String::with_capacity(parameter.len());
        let mut rest = parameter;
        let mut templates = 0_usize;
        while let Some(opening) = rest.find(TEMPLATE_OPENING) {
            templates += 1;
            if templates > MAX_TEMPLATES {
                return None;
            }
            rendered.push_str(rest.get(..opening)?);
            let inner = rest.get(opening + TEMPLATE_OPENING.len()..)?;
            let closing = closing_offset(inner)?;
            let expression = inner.get(..closing)?;
            if expression.chars().count() > MAX_EXPRESSION_CHARACTERS {
                return None;
            }
            rendered.push_str(&value_of(expression, message)?);
            if rendered.chars().count() > MAX_RENDERED_CHARACTERS {
                return None;
            }
            rest = inner.get(closing + TEMPLATE_CLOSING.len_utf8()..)?;
        }
        rendered.push_str(rest);
        Some(rendered)
    }
}

/// The byte offset of the `}` that closes the template whose expression `inner` opens, or `None`
/// for text that never closes it.
///
/// A brace INSIDE the expression is paired, so an expression that itself holds braces does not
/// close the template early, and a brace inside a single-quoted SQL string is text rather than a
/// brace — the quoting the AWS IoT SQL reference writes a string literal with.
fn closing_offset(inner: &str) -> Option<usize> {
    let mut depth = 0_usize;
    let mut quoted = false;
    for (offset, character) in inner.char_indices() {
        if character == '\'' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match character {
            '{' => depth += 1,
            TEMPLATE_CLOSING if depth == 0 => return Some(offset),
            TEMPLATE_CLOSING => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The text ONE template's expression is worth for `message`.
///
/// The expression is parsed as one SQL expression and nothing more — text the parser leaves over
/// is not an expression this engine reads — and then evaluated by the walk every clause of a
/// statement is evaluated by. A STRING is worth its own characters, because the template puts a
/// value INTO text rather than into a document, and every other value is worth the text the
/// document form writes it as, so a number is its digits.
///
/// `None` where the walk decides nothing ([`Evaluated::Undecided`]) and where the message has no
/// value at all ([`Evaluated::Absent`], and a `NULL`): a parameter cannot be rendered from a
/// value that is not there.
fn value_of(expression: &str, message: Message<'_>) -> Option<String> {
    let mut parser = Parser::new(&GenericDialect {})
        .try_with_sql(expression)
        .ok()?;
    let parsed = parser.parse_expr().ok()?;
    if parser.peek_token().token != Token::EOF {
        return None;
    }
    match evaluate(&parsed, message, 0) {
        Evaluated::Known(Value::String(text)) => Some(text),
        Evaluated::Known(Value::Null) | Evaluated::Absent | Evaluated::Undecided => None,
        Evaluated::Known(value) => Some(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The topic every row below publishes to.
    const TOPIC: &str = "sensors/room-1/temp";

    /// The message every row below publishes.
    const PAYLOAD: &str = r#"{"id":"m-1","temp":42,"ratio":0.5,"ok":true,
        "state":{"reported":{"room":"r1"}},"empty":null}"#;

    #[test]
    fn a_parameter_is_its_text_with_each_template_replaced_by_what_it_is_worth() {
        // The rows differ along what the rule generalises over: how many templates the
        // parameter carries and where in the text they sit, the KIND of expression inside one
        // (an attribute, a nested path, the `topic()` builtin whole and by segment, a literal,
        // arithmetic, a quoted string holding a brace), the type of the value it is worth
        // (string, integer, fraction, boolean), and the parameters this engine renders
        // NOTHING for (an attribute the message does not carry, a NULL one, a function of the
        // AWS IoT SQL library this engine does not compute, text that never closes the
        // template, and an expression the parser does not read).
        //
        // `timestamp()` and `newuuid()` are computed too, but each call has its own value, so
        // what they render is asserted on its FORM below rather than against a literal here.
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("no template at all", "telemetry/raw", Some("telemetry/raw")),
            ("one template, whole", "${id}", Some("m-1")),
            (
                "a template between literal text",
                "readings/${id}.json",
                Some("readings/m-1.json"),
            ),
            (
                "two templates in one parameter",
                "${topic(2)}/${id}",
                Some("room-1/m-1"),
            ),
            (
                "the whole topic",
                "archive/${topic()}",
                Some("archive/sensors/room-1/temp"),
            ),
            ("an integer attribute", "${temp}", Some("42")),
            ("a fractional attribute", "${ratio}", Some("0.5")),
            ("a boolean attribute", "${ok}", Some("true")),
            (
                "a nested attribute path",
                "${state.reported.room}",
                Some("r1"),
            ),
            ("arithmetic over the message", "${temp * 2}", Some("84")),
            ("a literal expression", "${'fixed'}", Some("fixed")),
            (
                "a quoted string holding a brace",
                "${'a}b'}",
                Some("a}b"),
            ),
            ("an attribute the message does not carry", "${absent}", None),
            ("an attribute whose value is null", "${empty}", None),
            (
                "a function of the library this engine does not compute",
                "${clientid()}",
                None,
            ),
            ("a template the text never closes", "${id", None),
            ("an expression the parser does not read", "${)}", None),
            (
                "text the parser leaves over",
                "${id, temp}",
                None,
            ),
        ];
        let parameters = Parameters::of(TOPIC, PAYLOAD.as_bytes());
        for (case, parameter, expected) in rows {
            let actual = parameters.rendered(parameter);
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?} for {parameter:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn the_computed_functions_render_the_form_each_one_documents() {
        let parameters = Parameters::of(TOPIC, PAYLOAD.as_bytes());
        // `timestamp()` is "the current timestamp in milliseconds", so what it renders is
        // digits that read back as a number past the start of this millennium — the clock's
        // own value, not one asserted literally.
        let rendered = parameters
            .rendered("readings/${timestamp()}.json")
            .unwrap_or_default();
        let stamp = rendered
            .strip_prefix("readings/")
            .and_then(|rest| rest.strip_suffix(".json"))
            .unwrap_or_default()
            .parse::<i64>()
            .unwrap_or_default();
        assert!(
            stamp > 946_684_800_000,
            "case \"timestamp(), in milliseconds\": expected a millisecond clock reading in \
             {rendered:?}, read {stamp}"
        );
        // `newuuid()` is "a random 16-byte UUID", so each call renders its own value and both
        // are of that form.
        let first = parameters.rendered("${newuuid()}").unwrap_or_default();
        let second = parameters.rendered("${newuuid()}").unwrap_or_default();
        for (case, rendered) in [("the first", &first), ("the second", &second)] {
            assert_eq!(
                rendered.len(),
                36,
                "case {case:?} newuuid(): expected the 36-character form, got {rendered:?}"
            );
        }
        assert_ne!(
            first, second,
            "case \"newuuid() twice\": each call has its own value, got {first:?} twice"
        );
    }

    #[test]
    fn a_payload_that_is_no_document_renders_only_what_the_topic_answers() {
        let parameters = Parameters::of(TOPIC, b"not a document");
        for (case, parameter, expected) in [
            ("the topic, which is no part of the payload", "${topic(1)}", Some("sensors")),
            ("an attribute, which a non-document carries none of", "${id}", None),
        ] {
            let actual = parameters.rendered(parameter);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected {expected:?} for {parameter:?}, got {actual:?}"
            );
        }
    }
}
