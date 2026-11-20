// SPDX-License-Identifier: CC0-1.0

//! The policy document a resource-policy write carries: how it is read, how a value that is
//! no policy document is refused, and the form every read reports it in.
//!
//! A resource policy is submitted as TEXT (`PolicyInJson`, "the policy document to set, in
//! JSON format") and reported as text, and the text a read reports is not the text the write
//! sent: the service re-serializes the document it parsed, so a client that submits a compact
//! document reads back the service's own layout of it.
//!
//! Neither the LAYOUT nor the parse FAULT is this module's own, because neither is this
//! service's own question:
//!
//! * the layout is the canonical rendering every service that attaches a policy document
//!   reports one in ([`::service_common::iam_policy::canonical_document_keeping_value_lists`]),
//!   which copies each token of the submitted text out byte for byte and writes the canonical
//!   whitespace between them — so member order, a number's digits and a string's own escapes
//!   are the caller's, and only the whitespace is the service's. Glue's flavour of it keeps a
//!   one-value list's brackets;
//! * the fault a document that is not JSON stopped on is read by
//!   [`::service_common::json_parse_fault`] — the same questions about the same kind of
//!   caller-supplied document that this crate's schema-definition refusal asks
//!   ([`super::schema_definition_json`]), so two Glue surfaces handed one document cannot
//!   disagree about where it went wrong, and a fault class worded wrongly is corrected THERE.
//!
//! What this module adds is which values are policy documents at all, and this surface's own
//! framing of the fault.
//!
//! The rendering is done ONCE, by the write, and the rendered text is what the attachment
//! holds — so `GetResourcePolicy` and `GetResourcePolicies` report the same document for the
//! same attachment, and neither re-renders per call.

use ::service_common::json_parse_fault as fault;

use super::messages;

/// The fixed prefix of the refusal a document that is not JSON at all earns.
///
/// RECORDED: the service surfaces its JSON parser's own failure behind this prefix, so the
/// refusal states what could not be read and where, rather than naming the member.
const PARSE_FAILURE_PREFIX: &str = "Invalid input policy: Unexpected IOException: ";

/// The document `policy_in_json` carries, in the layout every read of the attachment reports
/// it in — or the SENTENCE the write is refused with when the value is no policy document.
///
/// `Err` covers both ways a value fails to be one: text that is not JSON at all, which is
/// refused with the parser's own failure and the position it stopped at
/// ([`PARSE_FAILURE_PREFIX`]), and JSON that is not an object, which is no policy whatever it
/// parses to ([`messages::POLICY_MUST_BE_JSON`]).
///
/// The ONE reading of the member, so the write that stores a document and the validation that
/// refuses one cannot disagree about which values are policies.
pub fn render_policy_document(policy_in_json: &str) -> Result<String, String> {
    match ::serde_json::from_str::<::serde_json::Value>(policy_in_json) {
        Ok(document) if document.is_object() => Ok(
            ::service_common::iam_policy::canonical_document_keeping_value_lists(policy_in_json),
        ),
        Ok(_) => Err(messages::POLICY_MUST_BE_JSON.to_string()),
        Err(error) => Err(parse_refusal(policy_in_json, &error)),
    }
}

/// The sentence a document that is not JSON at all is refused with: the parse's own fault,
/// then where the parse stopped — this surface's framing of the readings
/// [`::service_common::json_parse_fault`] takes off the parse that actually ran.
///
/// The fault class RECORDED for this surface is the one a bare word earns: the word itself,
/// quoted, and the classes a value may begin with, at the position the WORD starts. Every
/// other class the parse can stop on — a character that begins no value, an unquoted member
/// name, the end of the input inside a collection — is reported as that shared reading words
/// it, at the position the parse reports; what is pinned for those is the behaviour, 400 with
/// `InvalidInputException`.
fn parse_refusal(document: &str, error: &::serde_json::Error) -> String {
    let (reading, line, column) = match fault::unrecognized_token(document, error) {
        Some((word, line, column)) => (
            format!(
                "Unrecognized token '{word}': was expecting ({})",
                fault::VALUE_CLASSES
            ),
            line,
            column,
        ),
        None => {
            let (line, column) = fault::stopped_at(document, error);
            (fault::unexpected_input(document, error), line, column)
        }
    };
    // A document that ran out of input inside a collection reports where that collection
    // OPENED as well, which is what says which brace was left unmatched.
    let framed = match fault::unclosed_container_start(document, error) {
        Some((marker_line, marker_column)) => format!(
            "{reading} (start marker at {})",
            fault::location_clause(fault::REDACTED_SOURCE, marker_line, marker_column)
        ),
        None => reading,
    };
    format!(
        "{PARSE_FAILURE_PREFIX}{framed}{}",
        fault::source_location(line, column)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document is reported in the service's own layout: its members in the order the
    /// client sent them, one per line, two spaces per level, `" : "` between name and value,
    /// and a list on one line.
    #[test]
    fn a_document_is_rendered_in_the_services_own_layout() {
        // (case, the document as submitted, the layout it is reported in)
        let cases = [
            ("an empty object", "{}", "{ }"),
            (
                "one member",
                r#"{"Version":"2012-10-17"}"#,
                "{\n  \"Version\" : \"2012-10-17\"\n}",
            ),
            (
                "an empty list",
                r#"{"Statement":[]}"#,
                "{\n  \"Statement\" : [ ]\n}",
            ),
            (
                "a list of scalars",
                r#"{"Action":["glue:GetTable","glue:GetDatabase"]}"#,
                "{\n  \"Action\" : [ \"glue:GetTable\", \"glue:GetDatabase\" ]\n}",
            ),
            (
                "an object inside a list",
                r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"*"}}]}"#,
                concat!(
                    "{\n",
                    "  \"Statement\" : [ {\n",
                    "    \"Effect\" : \"Allow\",\n",
                    "    \"Principal\" : {\n",
                    "      \"AWS\" : \"*\"\n",
                    "    }\n",
                    "  } ]\n",
                    "}",
                ),
            ),
            (
                "values that are not strings",
                r#"{"n":1,"f":1.5,"t":true,"z":null}"#,
                "{\n  \"n\" : 1,\n  \"f\" : 1.5,\n  \"t\" : true,\n  \"z\" : null\n}",
            ),
            (
                // Every token is copied out of the submitted text, so a number keeps the
                // digits it was written with and a string keeps its own escapes.
                "a number's digits and a string's escapes, as written",
                r#"{"f":1.50,"s":"caf\u00e9"}"#,
                "{\n  \"f\" : 1.50,\n  \"s\" : \"caf\\u00e9\"\n}",
            ),
            (
                // This surface reports a list as the list it was written as.
                "a one-value list, which keeps its brackets",
                r#"{"Action":["glue:GetTable"]}"#,
                "{\n  \"Action\" : [ \"glue:GetTable\" ]\n}",
            ),
        ];

        for (case, submitted, expected) in cases {
            let rendered = render_policy_document(submitted);
            assert_eq!(
                rendered.as_deref(),
                Ok(expected),
                "case {case}: unexpected rendering of {submitted}"
            );
        }
    }

    /// The rendering is IDEMPOTENT: a document already in the service's layout renders to
    /// itself, so a read of what a write stored reports the same text.
    #[test]
    fn the_rendering_of_a_rendered_document_is_itself() {
        for submitted in [
            "{}",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow"}]}"#,
            "{\n  \"Statement\" : [ ]\n}",
        ] {
            let once = render_policy_document(submitted)
                .unwrap_or_else(|refusal| panic!("case {submitted}: expected a rendering, got {refusal}"));
            let twice = render_policy_document(&once).unwrap_or_else(|refusal| {
                panic!("case {submitted}: expected a rendering of {once}, got {refusal}")
            });
            assert_eq!(
                twice, once,
                "case {submitted}: rendering a rendered document must answer the same text"
            );
        }
    }

    /// A document that is not JSON at all is refused with the parse's own fault: for the
    /// recorded class, the word it stopped on and the position that word STARTS at; for the
    /// other classes, the shared reading's own wording at the position the parse reports.
    #[test]
    fn a_value_that_is_not_json_is_refused_with_the_parsers_complaint() {
        let redacted = fault::REDACTED_SOURCE;
        let classes = fault::VALUE_CLASSES;
        // (case, the value submitted, the fault reported)
        let cases: Vec<(&str, &str, String)> = vec![
            (
                "a bare word",
                "not a policy",
                format!(
                    "Unrecognized token 'not': was expecting ({classes})\n at [Source: \
                     {redacted}; line: 1, column: 1]"
                ),
            ),
            (
                "a word where a member's value belongs",
                "{\"Version\": nope}",
                format!(
                    "Unrecognized token 'nope': was expecting ({classes})\n at [Source: \
                     {redacted}; line: 1, column: 13]"
                ),
            ),
            (
                "a word on a later line",
                "{\n  \"Version\": oops\n}",
                format!(
                    "Unrecognized token 'oops': was expecting ({classes})\n at [Source: \
                     {redacted}; line: 2, column: 14]"
                ),
            ),
            (
                "a character no value begins",
                "*",
                format!(
                    "Unexpected character ('*' (code 42)): was expecting a valid value \
                     ({classes})\n at [Source: {redacted}; line: 1, column: 1]"
                ),
            ),
            (
                "an object left open",
                "{\"Version\":\"2012-10-17\"",
                format!(
                    "Unexpected end-of-input: expected close marker for Object (start marker \
                     at [Source: {redacted}; line: 1, column: 1])\n at [Source: {redacted}; \
                     line: 1, column: 24]"
                ),
            ),
        ];

        for (case, submitted, reported) in &cases {
            let refusal = render_policy_document(submitted)
                .err()
                .unwrap_or_else(|| panic!("case {case}: {submitted} is no policy document"));
            let expected = format!("{PARSE_FAILURE_PREFIX}{reported}");
            assert_eq!(
                refusal, expected,
                "case {case}: unexpected refusal for {submitted:?}"
            );
        }
    }
}
