// SPDX-License-Identifier: CC0-1.0

//! Whether a configuration document PARSES for the engine whose configuration it is
//! sent as.
//!
//! RULE: Amazon MQ parses the document a write carries against the engine the
//! configuration runs — `Data` is "the base64-encoded XML configuration" for ActiveMQ
//! and "the base64-encoded Cuttlefish configuration" for RabbitMQ
//! (`smithy_query(query="doc", shape="UpdateConfigurationRequest", member="Data")`) —
//! and refuses a document that does not parse in that format, so no revision is ever
//! made from a document the engine could not read. `UpdateConfiguration`'s `Warnings`
//! member, "the first 20 warnings about the configuration elements or attributes that
//! were sanitized", is the model's own statement that the service reads the document
//! rather than storing it opaquely.
//!
//! Only the SYNTAX of the document is judged here: whether it is a well-formed XML
//! document, or a Cuttlefish configuration whose statements are assignments. What the
//! elements and the settings mean is the engine's, and a document whose syntax this
//! module accepts is stored in the form [`super::stored_configuration_data`] decides.
//!
//! The XML half is the ONE walk `configuration_document` makes over the document's
//! events with the `quick-xml` crate, the walk that also decides the form the document
//! is stored in. The Cuttlefish half is spelled out here: Cuttlefish is an
//! Erlang-ecosystem configuration format (the one RabbitMQ's `rabbitmq.conf` uses) that
//! crates.io carries no parser for, and only its statement form — a `#` comment, a
//! blank line, or a dotted key assigned a value — is needed to tell a document the
//! engine can read from one it cannot.
//!
//! Both halves cost one pass over the document the request already carried, with no
//! recursion on its nesting and no allocation beyond the answer. Neither adds a size bound
//! of its own: the document is the base64 `Data` of a request the framework has already
//! deserialized, so what either half scans is bounded by that one request's body and grows
//! with nothing else.

/// The reason a parse reports for the line it stopped on, which is the one part of the
/// refusal's sentence that varies ([`configuration_data_unparsable_message`]).
///
/// A recorded Amazon MQ answer covers the reason for a RabbitMQ document only, so the XML
/// half of this module reports the line in the same words rather than inventing a second
/// vocabulary for a sentence no recording shows.
fn invalid_token_at(line: usize) -> String {
    format!("invalid token at line {line}")
}

/// The 1-based line an offset into `document` falls on.
///
/// The line is COUNTED over the document's bytes rather than taken from a slice of it:
/// an offset a parser reports is not guaranteed to fall on a character boundary, and
/// slicing a document a caller sent at one that does not would panic.
fn line_at(document: &str, offset: usize) -> usize {
    document
        .bytes()
        .take(offset)
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

/// Why `document` is not a configuration document of the engine `engine_type` names, or
/// `None` when it parses.
///
/// The reason is the one part of the refusal's sentence that varies
/// ([`configuration_data_unparsable_message`]), so it is computed from the document the
/// request sent and names the line the parse stopped at.
pub fn configuration_document_syntax_error(engine_type: &str, document: &str) -> Option<String> {
    if engine_type.eq_ignore_ascii_case(super::ENGINE_TYPE_RABBITMQ) {
        cuttlefish_syntax_error(document)
    } else {
        xml_syntax_error(document)
    }
}

/// The sentence Amazon MQ refuses a `Data` it could not parse with: fixed text whose
/// only variable part is the parser's reason ([`invalid_token_at`]).
pub fn configuration_data_unparsable_message(reason: &str) -> String {
    format!(
        "The specified configuration data could not be parsed for reason [{reason}]. \
         Check your syntax."
    )
}

/// Why `document` is not a well-formed XML configuration document — the form an
/// ActiveMQ `broker.xml` takes — or `None` when it is.
///
/// The judgement is the ONE walk [`super::canonical_xml_document`] makes for the form
/// the document is stored in: a document is one when its events parse, it has exactly
/// one root element, every element closes, and nothing but whitespace stands outside the
/// root — so a Cuttlefish configuration, which is all text and no element, is not one.
/// Sharing the walk is what keeps the document this service ACCEPTS and the document it
/// stores the same document.
fn xml_syntax_error(document: &str) -> Option<String> {
    super::canonical_xml_document(document)
        .err()
        .map(|offset| invalid_token_at(line_at(document, offset)))
}

/// Why `document` is not a Cuttlefish configuration — the form a RabbitMQ
/// `rabbitmq.conf` takes — or `None` when it is.
///
/// A Cuttlefish configuration is a sequence of statements, one per line: a blank line,
/// a `#` comment, or a dotted key assigned a value (`consumer_timeout = 1800000`).
/// Anything else — a line carrying no assignment, a key outside the characters a
/// Cuttlefish key is spelled with, or an assignment with no value — is the token the
/// parse stops on.
fn cuttlefish_syntax_error(document: &str) -> Option<String> {
    for (index, line) in document.lines().enumerate() {
        let statement = line.trim();
        if statement.is_empty() || statement.starts_with('#') {
            continue;
        }
        let reason = || Some(invalid_token_at(index + 1));
        let Some((key, value)) = statement.split_once('=') else {
            return reason();
        };
        let key = key.trim();
        if key.is_empty()
            || value.trim().is_empty()
            || !key.chars().all(is_cuttlefish_key_character)
        {
            return reason();
        }
    }
    None
}

/// The characters a Cuttlefish key is spelled with: a dotted path of alphanumerics,
/// underscores and dashes (`secure.management.http.headers.enabled`).
fn is_cuttlefish_key_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || character == '.'
        || character == '_'
        || character == '-'
}
