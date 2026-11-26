// SPDX-License-Identifier: CC0-1.0

//! Whether a reader using one `JSON` schema document can read data written with another —
//! the ONE comparison every `Compatibility` mode of a `JSON` schema set is built from
//! ([`super::schema_compatibility`]).
//!
//! A `JSON` `SchemaDefinition` is a JSON Schema document, and the question a mode asks of
//! two of them is CONTAINMENT: a reader R can read data written with a writer W exactly when
//! every instance W admits is one R admits. Everything below is that one question answered
//! over the part of the vocabulary a schema set's documents describe records with — the
//! instance `type`, the declared `properties`, which of them are `required`, and whether the
//! document admits properties it does not declare (`additionalProperties`, the CONTENT
//! MODEL).
//!
//! The content model is what makes one difference compatible in one document and not in
//! another, and it is the rule the registry's own compatibility documentation states:
//!
//! * a property the READER declares and the writer does not is admitted by a writer with an
//!   OPEN content model carrying ANY value, so the reader's constraint on it is one W's
//!   instances need not satisfy — adding a property to an open content model is incompatible;
//! * the same addition against a CLOSED writer is compatible, because an instance of W can
//!   never carry that property at all;
//! * a property the WRITER declares (or may add, being open) and a CLOSED reader does not
//!   declare is forbidden by the reader, so such a pair is incompatible;
//! * a property R `required` that W does not require may be absent from an instance of W, so
//!   the reader's requirement is one W's instances need not satisfy.
//!
//! A document that describes its instances through a vocabulary outside that part — a
//! reference, a combinator, a negation — is not DECIDED here: this comparison reports a
//! failure only for a difference it can state, so a definition this emulator cannot judge is
//! never reported as incompatible for a reason it has not shown.

use ::serde_json::Value;

/// How deep into two documents' nested property schemas the comparison descends.
///
/// The comparison walks a property's own schema with the same rule as the document around
/// it, so its depth is the depth of the submitted definitions — the modelled
/// `SchemaDefinitionString` admits 170000 characters, which is thousands of nested objects.
/// A Rust stack that runs out aborts the process rather than failing the one request, so the
/// walk stops descending here and reports the pair as undecided rather than recursing on.
/// The value is the nesting bound the registry applies to a `PROTOBUF` definition
/// ([`super::MAX_PROTOBUF_NESTING_DEPTH`]), for the same reason and in the same place: far
/// deeper than a schema document nests.
const MAX_JSON_SCHEMA_DEPTH: usize = 100;

/// The keyword a document declares the properties of its instances under.
const PROPERTIES: &str = "properties";

/// The keyword a document names the properties an instance must carry under.
const REQUIRED: &str = "required";

/// The keyword a document states its CONTENT MODEL with: `false` closes the document to the
/// properties it declares, and anything else (a schema, `true`, or the keyword left out)
/// leaves it open.
const ADDITIONAL_PROPERTIES: &str = "additionalProperties";

/// The keyword a document constrains the TYPE of its instances with.
const TYPE: &str = "type";

/// The keywords that describe an instance by composing or referring to OTHER schemas, which
/// this comparison does not decide: a document carrying one of them is undecided rather than
/// compared on its declared properties alone, because those keywords can admit or forbid
/// instances the declared properties say nothing about.
///
/// Source: the JSON Schema vocabulary for applying subschemas (`$ref`, `allOf`, `anyOf`,
/// `oneOf`, `not`) and for the conditional one (`if`).
const COMPOSING_KEYWORDS: [&str; 6] = ["$ref", "allOf", "anyOf", "oneOf", "not", "if"];

/// Why a reader using `reader` cannot read data written with `writer`, or `None` when every
/// instance the writer admits is one the reader admits — as far as this comparison decides
/// the pair.
#[must_use]
pub fn json_reader_cannot_read(writer: &Value, reader: &Value) -> Option<String> {
    cannot_read_at(writer, reader, "", MAX_JSON_SCHEMA_DEPTH)
}

/// [`json_reader_cannot_read`] for the schemas at `at` — the empty string for the documents
/// themselves, and the property path under it for a nested pair — with `depth` levels left to
/// descend.
fn cannot_read_at(writer: &Value, reader: &Value, at: &str, depth: usize) -> Option<String> {
    if depth == 0 || composed(writer) || composed(reader) {
        return None;
    }
    // A boolean schema admits everything (`true`) or nothing (`false`): a reader admitting
    // everything reads any writer, and a writer admitting nothing has no instance to read.
    if writer.as_bool() == Some(false) || reader.as_bool() == Some(true) {
        return None;
    }
    if let Some(difference) = type_difference(writer, reader, at) {
        return Some(difference);
    }
    let writer_properties = properties(writer);
    let reader_properties = properties(reader);
    let writer_open = is_open(writer);
    let reader_open = is_open(reader);

    // A property the reader requires that the writer does not: an instance of the writer
    // need not carry it.
    for name in required(reader) {
        if !required(writer).contains(&name) {
            return Some(format!(
                "the reader requires the property {} the writer does not require",
                named(at, &name)
            ));
        }
    }
    for (name, reader_property) in &reader_properties {
        match writer_properties.get(name) {
            // Declared by both: every instance of the writer's property has to be one the
            // reader's property admits, which is this same question one level down.
            Some(writer_property) => {
                if let Some(difference) = cannot_read_at(
                    writer_property,
                    reader_property,
                    &named(at, name),
                    depth - 1,
                ) {
                    return Some(difference);
                }
            }
            // Declared by the reader alone. An OPEN writer admits an instance carrying that
            // property with any value at all, so the reader's constraint on it is one such
            // an instance need not satisfy; a CLOSED writer admits no instance carrying it.
            None if writer_open => {
                return Some(format!(
                    "the property {} is added to a writer with an open content model",
                    named(at, name)
                ))
            }
            None => {}
        }
    }
    // A closed reader forbids every property it does not declare, so a writer that declares
    // one — or that may carry one, being open — writes instances the reader does not admit.
    if !reader_open {
        if writer_open {
            return Some(format!(
                "the reader{} closes a content model the writer leaves open",
                at_clause(at)
            ));
        }
        for name in writer_properties.keys() {
            if !reader_properties.contains_key(name) {
                return Some(format!(
                    "the reader does not declare the property {} a closed content model \
                     forbids",
                    named(at, name)
                ));
            }
        }
    }
    None
}

/// Why the TYPE the reader admits does not cover the type the writer admits, or `None` when
/// it does.
///
/// A document that constrains no type admits every instance type, so a writer that
/// constrains none and a reader that constrains some is a pair whose writer admits instances
/// the reader does not.
fn type_difference(writer: &Value, reader: &Value, at: &str) -> Option<String> {
    let admitted = declared_types(reader)?;
    match declared_types(writer) {
        Some(written) => {
            let outside: Vec<&String> = written
                .iter()
                .filter(|kind| !admitted.contains(kind))
                .collect();
            let first = outside.first()?;
            Some(format!(
                "the writer{} admits the instance type {first} the reader does not",
                at_clause(at)
            ))
        }
        None => Some(format!(
            "the writer{} constrains no instance type while the reader admits only {}",
            at_clause(at),
            admitted.join(", ")
        )),
    }
}

/// The instance types a document admits, or `None` when it constrains none — the keyword
/// absent, or carrying something that is neither a name nor a list of names.
fn declared_types(schema: &Value) -> Option<Vec<String>> {
    match schema.get(TYPE)? {
        Value::String(name) => Some(vec![name.clone()]),
        Value::Array(names) => {
            let declared: Vec<String> = names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect();
            (!declared.is_empty()).then_some(declared)
        }
        _ => None,
    }
}

/// The properties a document declares, keyed by name, and the empty map for a document that
/// declares none.
fn properties(schema: &Value) -> ::std::collections::BTreeMap<String, Value> {
    schema
        .get(PROPERTIES)
        .and_then(Value::as_object)
        .map(|declared| {
            declared
                .iter()
                .map(|(name, property)| (name.clone(), property.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The names a document says an instance must carry.
fn required(schema: &Value) -> Vec<String> {
    schema
        .get(REQUIRED)
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a document admits properties it does not declare: `additionalProperties: false`
/// closes it, and every other value — a schema, `true`, the keyword left out — leaves it
/// open, which is the vocabulary's own default.
fn is_open(schema: &Value) -> bool {
    schema.get(ADDITIONAL_PROPERTIES).and_then(Value::as_bool) != Some(false)
}

/// Whether a document describes its instances through a schema this comparison does not
/// decide ([`COMPOSING_KEYWORDS`]).
fn composed(schema: &Value) -> bool {
    COMPOSING_KEYWORDS
        .iter()
        .any(|keyword| schema.get(keyword).is_some())
}

/// The path of the property `name` under the schema at `at`, as a difference names it.
fn named(at: &str, name: &str) -> String {
    if at.is_empty() {
        name.to_string()
    } else {
        format!("{at}.{name}")
    }
}

/// How a difference names WHERE in the two documents it was found: nothing for the documents
/// themselves, and the property path for a nested pair.
fn at_clause(at: &str) -> String {
    if at.is_empty() {
        String::new()
    } else {
        format!("'s property {at}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One document from its parts, so a row states only what it varies.
    fn schema(open: bool, properties: &[(&str, &str)], required: &[&str]) -> Value {
        let declared: Vec<String> = properties
            .iter()
            .map(|(name, kind)| format!(r#""{name}":{{"type":"{kind}"}}"#))
            .collect();
        let names: Vec<String> = required.iter().map(|name| format!("\"{name}\"")).collect();
        let text = format!(
            r#"{{"type":"object","additionalProperties":{open},"properties":{{{}}},"required":[{}]}}"#,
            declared.join(","),
            names.join(",")
        );
        ::serde_json::from_str(&text).expect("the test's own document must be JSON")
    }

    /// RULE: a reader reads a writer's data exactly when every instance the writer admits is
    /// one the reader admits — so a property the reader adds is compatible against a CLOSED
    /// writer and incompatible against an OPEN one, a requirement the reader adds is never
    /// compatible, and a property the writer declares is incompatible with a reader that
    /// closes it out.
    ///
    /// The rows differ along what the rule generalises over: the content model of each side,
    /// which side declares the extra property, whether it is required, the instance type of a
    /// shared property, and the nesting level the difference sits at.
    #[test]
    fn a_reader_reads_a_writer_exactly_when_it_admits_every_instance() {
        let closed_one = schema(false, &[("id", "string")], &["id"]);
        let closed_two = schema(false, &[("id", "string"), ("email", "string")], &["id"]);
        let open_one = schema(true, &[("id", "string")], &["id"]);
        let open_two = schema(true, &[("id", "string"), ("email", "string")], &["id"]);
        let open_two_required = schema(
            true,
            &[("id", "string"), ("email", "string")],
            &["id", "email"],
        );
        let closed_two_required = schema(
            false,
            &[("id", "string"), ("email", "string")],
            &["id", "email"],
        );
        let retyped = schema(false, &[("id", "integer")], &["id"]);
        let nested_writer: Value = ::serde_json::from_str(
            r#"{"type":"object","additionalProperties":true,"properties":
               {"user":{"type":"object","additionalProperties":true,
                        "properties":{"id":{"type":"string"}}}}}"#,
        )
        .expect("the test's own document must be JSON");
        let nested_reader: Value = ::serde_json::from_str(
            r#"{"type":"object","additionalProperties":true,"properties":
               {"user":{"type":"object","additionalProperties":true,
                        "properties":{"id":{"type":"string"},"email":{"type":"string"}}}}}"#,
        )
        .expect("the test's own document must be JSON");
        let composed: Value = ::serde_json::from_str(r#"{"allOf":[{"type":"object"}]}"#)
            .expect("the test's own document must be JSON");

        // (case, the writer, the reader, whether the reader can read the writer's data)
        let rows: &[(&str, &Value, &Value, bool)] = &[
            ("the same closed document", &closed_one, &closed_one, true),
            ("the same open document", &open_one, &open_one, true),
            (
                "a property added to a closed content model",
                &closed_one,
                &closed_two,
                true,
            ),
            (
                "a property added to an open content model",
                &open_one,
                &open_two,
                false,
            ),
            (
                "a required property added to a closed content model",
                &closed_one,
                &closed_two_required,
                false,
            ),
            (
                "a required property added to an open content model",
                &open_one,
                &open_two_required,
                false,
            ),
            (
                "a property dropped by an open reader",
                &open_two,
                &open_one,
                true,
            ),
            (
                "a property dropped by a closed reader",
                &closed_two,
                &closed_one,
                false,
            ),
            (
                "an open writer read by a closed reader",
                &open_one,
                &closed_one,
                false,
            ),
            (
                "a closed writer read by an open reader",
                &closed_one,
                &open_one,
                true,
            ),
            ("a property's type changed", &closed_one, &retyped, false),
            (
                "a property added to a nested open content model",
                &nested_writer,
                &nested_reader,
                false,
            ),
            // A document described through a vocabulary this comparison does not decide is
            // never reported as incompatible.
            ("a composed reader", &closed_one, &composed, true),
            ("a composed writer", &composed, &closed_one, true),
        ];

        for (case, writer, reader, readable) in rows {
            let difference = json_reader_cannot_read(writer, reader);
            assert_eq!(
                difference.is_none(),
                *readable,
                "case {case:?}: expected readable={readable}, got {difference:?}"
            );
        }
    }

    /// RULE: the comparison descends only as far as [`MAX_JSON_SCHEMA_DEPTH`], so the depth of
    /// its own recursion is never the caller's to choose — a pair nested deeper than that is
    /// reported as undecided rather than recursed into.
    ///
    /// The documents are built rather than parsed: the JSON reader applies a nesting limit of
    /// its own to a SUBMITTED definition (`serde_json`'s recursion limit), so a definition deep
    /// enough to reach this bound is refused as unreadable long before it is compared. This
    /// bound is what holds whatever that reader admits.
    #[test]
    fn the_comparison_descends_only_as_deep_as_its_bound() {
        /// A document nesting `levels` objects under the property `p`, the innermost carrying
        /// `properties`.
        fn nested(levels: usize, properties: Value) -> Value {
            let mut built = ::serde_json::json!({
                "type": "object",
                "additionalProperties": true,
                "properties": properties,
            });
            for _ in 0..levels {
                built = ::serde_json::json!({
                    "type": "object",
                    "additionalProperties": true,
                    "properties": { "p": built },
                });
            }
            built
        }

        // (case, how deep the difference sits, whether it is reported)
        let rows: &[(&str, usize, bool)] = &[
            ("a difference at the top", 0, true),
            ("a difference within the bound", 5, true),
            (
                "a difference one level past the bound",
                MAX_JSON_SCHEMA_DEPTH + 1,
                false,
            ),
            ("a difference far past the bound", 300, false),
        ];
        for (case, levels, reported) in rows {
            let writer = nested(*levels, ::serde_json::json!({ "id": { "type": "string" } }));
            let reader = nested(
                *levels,
                ::serde_json::json!({ "id": { "type": "string" }, "email": { "type": "string" } }),
            );
            let difference = json_reader_cannot_read(&writer, &reader);
            assert_eq!(
                difference.is_some(),
                *reported,
                "case {case:?}: expected reported={reported}, got {difference:?}"
            );
        }
    }
}
