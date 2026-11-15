// SPDX-License-Identifier: CC0-1.0

//! What a submitted `server.properties` body declares, and which of those
//! declarations an Amazon MSK configuration may carry.
//!
//! An MSK configuration IS a `server.properties` file: the body is read as a Java
//! properties document — ISO-8859-1 bytes, one declaration per logical line,
//! `name=value`, `name:value` or a name separated from its value by whitespace,
//! `#`/`!` comment lines, lines continued by a trailing backslash and the usual
//! escapes — which is what the broker itself reads it as. That grammar is read
//! through the [`java_properties`] crate rather than re-implemented here, so this
//! module is about the PROPERTIES a body declares, not about the file format.
//!
//! MSK configures a published set of broker properties ([`catalog`]) and manages
//! the rest itself, so a body declaring a property outside that set declares one
//! no broker of a cluster could be given. Such a body is refused on the request,
//! per declaration, carrying the name and the value the body gave it and the
//! versions the name was checked against ([`unsupported_properties_message`]) —
//! the one thing a caller can act on, since the refusal has to say WHICH line to
//! fix. A property's VALUE is not validated: the service stores it verbatim as
//! part of the revision, so only the name is gated here.
//!
//! One reading, shared by the two writers that store a body (the create's first
//! revision and the update's next one). Nothing here can panic on a submitted
//! body and nothing here is unbounded:
//!
//! * both writers apply the stored-body ceiling
//!   ([`crate::state::server_properties_too_large`], 256 KiB) BEFORE asking
//!   anything here and outside the registry lock, so the bytes walked are bounded
//!   by that ceiling rather than by the request;
//! * the grammar is read by a parser that RETURNS its failures, which
//!   [`declarations`] reports as "no document" — there is no `unwrap`, `expect`,
//!   index, slice or arithmetic on anything the request carries;
//! * the refusal's size is capped at BOTH its factors, so no request can amplify
//!   itself into a large response: at most [`MAX_REPORTED_PROPERTIES`] (10)
//!   declarations are reported, and the versions each names are this service's
//!   published catalogue ([`checked_versions`]) rather than anything the request
//!   carried.

/// The published set of broker properties, which is data rather than reading.
mod catalog;

/// The member the two writers carry the body in, named as the API documents it on
/// the wire (`CreateConfigurationRequest$ServerProperties` -> `jsonName:
/// serverProperties`) — what their refusals report as the parameter at fault.
pub const SERVER_PROPERTIES_MEMBER: &str = "serverProperties";

/// How many offending declarations one refusal reports.
///
/// CHOSEN as a process-safety backstop, not an AWS quota: a body inside the stored
/// ceiling can declare a great many properties, and the refusal says which line to
/// fix — the first few are what a caller acts on, and reporting every one of them
/// would let a small request produce an arbitrarily large response.
pub const MAX_REPORTED_PROPERTIES: usize = 10;

/// One property a submitted body declares: the name and the value as the
/// properties grammar reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// The property's name, which is what decides whether MSK configures it.
    pub name: String,
    /// The value the body gave it, carried back in a refusal so the caller can
    /// find the line. Never validated.
    pub value: String,
}

/// Every property `body` declares, in the order it declares them, or `None` when
/// the body is not a properties document at all — one whose escapes or encoding
/// make it unreadable, which declares nothing this service could store.
pub fn declarations(body: &[u8]) -> Option<Vec<Declaration>> {
    let mut declared = Vec::new();
    let mut properties = ::java_properties::PropertiesIter::new(body);
    properties
        .read_into(|name, value| declared.push(Declaration { name, value }))
        .ok()?;
    Some(declared)
}

/// The admission of `body`: `None` when it is no properties document at all, and
/// otherwise the declarations of it MSK does not configure — in the order the body
/// declares them and at most [`MAX_REPORTED_PROPERTIES`] of them, which is what a
/// refusal reports. An EMPTY list is a body whose every declaration is a property
/// MSK configures.
///
/// The ONE call each writer makes, so the body is walked once.
pub fn unconfigurable_declarations(body: &[u8]) -> Option<Vec<Declaration>> {
    Some(
        declarations(body)?
            .into_iter()
            .filter(|declared| !is_configurable_property(&declared.name))
            .take(MAX_REPORTED_PROPERTIES)
            .collect(),
    )
}

/// Whether `name` is a broker property an MSK configuration may set
/// ([`catalog::CONFIGURABLE_PROPERTIES`]).
pub fn is_configurable_property(name: &str) -> bool {
    catalog::CONFIGURABLE_PROPERTIES.contains(&name)
}

/// The Apache Kafka versions a declaration's name is checked against: the
/// versions this service publishes, in the order it publishes them
/// ([`crate::state::catalogued_kafka_versions`], which names each published
/// version once however many rows the catalogue publishes it under).
///
/// A property of the SERVICE, not of the request: the service knows a broker
/// property from the versions it publishes, so the versions a refusal names are
/// the same ones whatever a request's `KafkaVersions` carried — a caller pinning
/// one version is not told the name was checked against that version alone. Its
/// length is therefore bounded by the catalogue however long a list a request
/// carried, and the published list is borrowed rather than copied, so a refusal
/// allocates nothing for it.
fn checked_versions() -> &'static [&'static str] {
    crate::state::catalogued_kafka_versions()
}

/// The message Amazon MSK refuses a body declaring properties it does not
/// configure with: one entry per reported declaration, in the order the body
/// declared them, each carrying the name and value as submitted and the sentence
/// about the name, which names the versions it was checked against.
///
/// The envelope is a JSON array of per-property objects, LAID OUT the way the
/// service writes it: the array's brackets carry a space inside them and its
/// elements are separated by `, `, an object's members sit one per line indented
/// by [`ENVELOPE_INDENT`], each member's name is separated from its value by
/// ` : `, and a member whose value is an array keeps that array on the member's
/// own line with a space inside its brackets. The layout is part of the message —
/// a client that displays or splits it reads these bytes — so it is rendered
/// here rather than left to a serializer's own separators.
///
/// Bounded at both factors: at most [`MAX_REPORTED_PROPERTIES`] entries, each
/// naming exactly the catalogue's versions ([`checked_versions`]), so no request
/// can amplify itself into a large response.
pub fn unsupported_properties_message(rejected: &[Declaration]) -> String {
    let checked = checked_versions().join(", ");
    let entries: Vec<String> = rejected
        .iter()
        .take(MAX_REPORTED_PROPERTIES)
        .map(|declared| {
            format!(
                "{{\n{indent}\"name\" : {},\n{indent}\"value\" : {},\n{indent}\"errorMessages\" : [ {} ]\n}}",
                quoted(&declared.name),
                quoted(&declared.value),
                quoted(&unsupported_property_sentence(&declared.name, &checked)),
                indent = ENVELOPE_INDENT,
            )
        })
        .collect();
    format!("[ {} ]", entries.join(", "))
}

/// What one level of the refusal envelope's indentation is: the two spaces each
/// member of a per-property object is written behind.
const ENVELOPE_INDENT: &str = "  ";

/// The sentence one offending declaration carries: the property's name and the
/// versions it was checked against, in the words Amazon MSK states it in.
fn unsupported_property_sentence(name: &str, checked: &str) -> String {
    format!(
        "Key '{name}' is not supported by at least one Apache Kafka version. \
         Key checked against versions: [{checked}]"
    )
}

/// `text` as a JSON string, escaped as JSON escapes one — through the crate that
/// owns that encoding, so the envelope above is JSON whatever a name or a value
/// carries, while the ORDER of each entry's members stays the one Amazon MSK
/// writes them in.
fn quoted(text: &str) -> String {
    ::serde_json::Value::String(text.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the refusal is bounded at both its factors — at most
    /// [`MAX_REPORTED_PROPERTIES`] declarations, and the versions each is checked
    /// against are the service's own checked set, each once.
    #[test]
    fn the_refusal_is_bounded_at_both_factors() {
        let many: Vec<u8> = (0..MAX_REPORTED_PROPERTIES * 3)
            .map(|index| format!("not.a.property.{index}={index}\n"))
            .collect::<String>()
            .into_bytes();
        let reported =
            unconfigurable_declarations(&many).expect("the body above is a properties document");
        assert_eq!(
            reported.len(),
            MAX_REPORTED_PROPERTIES,
            "case \"many offenders\": expected at most {MAX_REPORTED_PROPERTIES}, got {reported:?}"
        );
        assert_eq!(
            reported.first().map(|first| first.name.as_str()),
            Some("not.a.property.0"),
            "case \"many offenders\": expected the body's order, got {reported:?}"
        );

        let checked = checked_versions();
        assert_eq!(
            checked,
            crate::state::catalogued_kafka_versions(),
            "case \"the checked versions\": expected every published version, each once"
        );
        let mut once: Vec<&str> = checked.to_vec();
        once.sort_unstable();
        once.dedup();
        assert_eq!(
            once.len(),
            checked.len(),
            "case \"the checked versions\": expected each version named once, got {checked:?}"
        );
    }

    /// RULE: the refusal names each reported declaration, its value and the
    /// versions its name was checked against, in Amazon MSK's own words and
    /// layout.
    #[test]
    fn the_refusal_reports_each_declaration_in_msks_words() {
        let rejected = vec![Declaration {
            name: "broker.id".to_string(),
            value: "7".to_string(),
        }];
        let message = unsupported_properties_message(&rejected);
        assert_eq!(
            message,
            format!(
                "[ {{\n  \"name\" : \"broker.id\",\n  \"value\" : \"7\",\n  \
                 \"errorMessages\" : [ \"Key 'broker.id' is not supported by at least one \
                 Apache Kafka version. Key checked against versions: [{}]\" ]\n}} ]",
                crate::state::catalogued_kafka_versions().join(", ")
            ),
            "case \"one offending declaration\": got {message}"
        );
    }
}
