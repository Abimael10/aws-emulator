// SPDX-License-Identifier: CC0-1.0

//! Reading a declared property document as the TYPES the owning API models its members as.
//!
//! ## The rule this module implements
//!
//! "You can write CloudFormation templates in JSON or YAML", and neither spelling makes a
//! quoted number a number: a template's scalar is TEXT wherever the author quoted it or fed it
//! through a `Ref` to a `String` parameter, while the member of the create it reaches is
//! modelled as a number or a boolean. So a provisioning arm reads each member the owning
//! model types as one of those from the text the declaration carries, at every depth the
//! document nests it, before the request is sent.
//!
//! Which members those are is PER TYPE — it comes from the owning model's own member list —
//! and is passed in; the WALK is the same for every type, so it lives here rather than once per
//! provisioning module ([`super::service_discovery`] and [`super::application_auto_scaling`]
//! both reach it). A value that is already of the modelled type travels as written, and one
//! that is NO such value is left exactly as the template wrote it, because the owning service's
//! own validation is what judges it. A member whose keys are the REQUESTER's rather than the
//! model's — a function's environment variables, a tag set, a weight per version — is classified
//! as the whole subtree it is, because no name inside it says anything about a modelled type.
//!
//! The reading goes BOTH ways, because the mismatch does: a member the model types as a string
//! takes the TEXT a number or a boolean literal spells, since a template may state one there and
//! "all property values in a template are strings".

use ::serde_json::Value;

/// What the owning API models one member as, where that is not TEXT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum ModelledScalar {
    /// Text, or anything else this reading does not change: the value travels as the
    /// declaration wrote it.
    AsWritten,
    /// A number — read as the WHOLE number the text names where it names one (a capacity is an
    /// integer member, and `1` is not the same JSON value as `1.0`), and as a fractional one
    /// otherwise.
    Number,
    /// A boolean, written `true` or `false` in either case.
    Boolean,
    /// A member the owning model types as a collection of TEXT whose keys are the
    /// requester's own (a function's environment variables, a tag set): every scalar under
    /// the member is the TEXT it spells, so text travels as the declaration wrote it — a
    /// value that merely LOOKS like a number is still text there — and a number or a boolean
    /// literal travels as the text that literal spells, which is the value such a member
    /// takes. The names inside it are the requester's, so none of them says anything about a
    /// modelled type.
    TextThroughout,
    /// A member the owning model types as a collection of NUMBERS whose keys are the
    /// requester's own (a weight per function version): every scalar under it is read as a
    /// number, for the same reason — the keys are the requester's, so there is no member name
    /// the reading could be told about.
    NumberThroughout,
}

/// How deep [`modelled_declaration`] walks a declared document.
///
/// The document's nesting is the REQUESTER's, and a recursive walk bounded only by its input
/// exhausts the native stack — which aborts the process rather than failing one stack. Past
/// this depth the values travel exactly as the template wrote them, and the owning service's
/// own validation is what judges them: the structures these types declare nest a handful of
/// levels at most.
const MAX_MODELLED_NESTING: usize = 32;

/// `value`, with every member `modelled_as` types as a number or a boolean read as that type.
///
/// `member` is the name `value` stands under — the list's own member name for each entry of a
/// list, which is what makes a list entry's members reachable.
pub(in crate::state) fn modelled_declaration(
    member: &str,
    value: &Value,
    modelled_as: &impl Fn(&str) -> ModelledScalar,
) -> Value {
    modelled_at(member, value, 0, modelled_as)
}

/// [`modelled_declaration`] over one value, `depth` levels into the document.
fn modelled_at(
    member: &str,
    value: &Value,
    depth: usize,
    modelled_as: &impl Fn(&str) -> ModelledScalar,
) -> Value {
    if depth >= MAX_MODELLED_NESTING {
        return value.clone();
    }
    match (modelled_as(member), value) {
        // The two classifications that are about the whole SUBTREE under a member rather than
        // about one scalar: a collection the model types as text, and one it types as numbers,
        // both keyed by names of the requester's own.
        (ModelledScalar::TextThroughout, _) => text_at(value, depth),
        (ModelledScalar::NumberThroughout, _) => numbers_throughout(value, depth),
        (ModelledScalar::Number, Value::String(text)) => {
            as_number(text).unwrap_or_else(|| value.clone())
        }
        (ModelledScalar::Boolean, Value::String(text)) => {
            as_boolean(text).unwrap_or_else(|| value.clone())
        }
        (_, Value::Object(members)) => Value::Object(
            members
                .iter()
                .map(|(name, held)| (name.clone(), modelled_at(name, held, depth + 1, modelled_as)))
                .collect(),
        ),
        // A list's entries are values of the member the list belongs to, so each is read under
        // that same member name.
        (_, Value::Array(entries)) => Value::Array(
            entries
                .iter()
                .map(|entry| modelled_at(member, entry, depth + 1, modelled_as))
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// [`ModelledScalar::TextThroughout`] over one value, for a caller that knows the member it
/// holds takes a collection of TEXT without classifying a member name for it: the caller-keyed
/// maps an API Gateway call takes as maps of string to string are read this way.
pub(in crate::state) fn text_throughout(value: &Value) -> Value {
    text_at(value, 0)
}

/// [`ModelledScalar::TextThroughout`] over one value: every scalar under it as the TEXT it
/// spells, at the same bound the rest of the walk keeps.
///
/// The rendering is the crate's one reading of a declared value as the text a member takes
/// ([`crate::state::provisioner::attribute_text`]), so a number, a boolean and a string spell
/// here exactly what they spell for every other member whose value travels as text. A `null`
/// states nothing and is left as it stands, which is what every caller of this layer reads it
/// as.
fn text_at(value: &Value, depth: usize) -> Value {
    if depth >= MAX_MODELLED_NESTING {
        return value.clone();
    }
    match value {
        Value::Number(_) | Value::Bool(_) => {
            Value::String(crate::state::provisioner::attribute_text(value))
        }
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, held)| (name.clone(), text_at(held, depth + 1)))
                .collect(),
        ),
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| text_at(entry, depth + 1))
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// [`ModelledScalar::NumberThroughout`] over one value: every scalar under it read as the
/// number its text names, at the same bound the rest of the walk keeps.
fn numbers_throughout(value: &Value, depth: usize) -> Value {
    if depth >= MAX_MODELLED_NESTING {
        return value.clone();
    }
    match value {
        Value::String(text) => as_number(text).unwrap_or_else(|| value.clone()),
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, held)| (name.clone(), numbers_throughout(held, depth + 1)))
                .collect(),
        ),
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| numbers_throughout(entry, depth + 1))
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// The number `text` names, or `None` for text that names no number at all.
fn as_number(text: &str) -> Option<Value> {
    let text = text.trim();
    if let Ok(whole) = text.parse::<i64>() {
        return Some(Value::Number(whole.into()));
    }
    ::serde_json::Number::from_f64(text.parse::<f64>().ok()?).map(Value::Number)
}

/// The boolean `text` names, or `None` for text that names neither.
fn as_boolean(text: &str) -> Option<Value> {
    match text.trim() {
        flag if flag.eq_ignore_ascii_case("true") => Some(Value::Bool(true)),
        flag if flag.eq_ignore_ascii_case("false") => Some(Value::Bool(false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classifier one test row reads a member with: the names it types as numbers, the
    /// names it types as booleans, and text for everything else.
    fn classified(member: &str) -> ModelledScalar {
        match member {
            "TTL" | "FailureThreshold" | "MaxCapacity" => ModelledScalar::Number,
            "DisableScaleIn" => ModelledScalar::Boolean,
            "Variables" => ModelledScalar::TextThroughout,
            "AdditionalVersionWeights" => ModelledScalar::NumberThroughout,
            _ => ModelledScalar::AsWritten,
        }
    }

    /// RULE: a member the owning model types as a number or a boolean is read as that type from
    /// the text a template may have quoted it as, at every depth the document nests it; every
    /// other member, and a value that names no such scalar, travels as written.
    ///
    /// The rows differ along what the rule generalises over: the scalar's modelled type, the
    /// depth it sits at, whether it stands inside a list, the spelling of a boolean, whether
    /// the number is whole or fractional, whether the text names such a value at all, and
    /// whether the member is a collection keyed by names of the requester's own.
    #[test]
    fn a_quoted_scalar_is_read_as_the_type_the_model_declares() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "a quoted integer three levels down",
                serde_json::json!({"DnsProperties": {"SOA": {"TTL": "60"}}}),
                serde_json::json!({"DnsProperties": {"SOA": {"TTL": 60}}}),
            ),
            (
                "quoted and unquoted entries of one list",
                serde_json::json!({"DnsRecords": [{"Type": "A", "TTL": "15"}, {"TTL": 30}]}),
                serde_json::json!({"DnsRecords": [{"Type": "A", "TTL": 15}, {"TTL": 30}]}),
            ),
            (
                "a quoted boolean in either case",
                serde_json::json!({"DisableScaleIn": "TRUE", "Other": "true"}),
                serde_json::json!({"DisableScaleIn": true, "Other": "true"}),
            ),
            (
                "a fractional number",
                serde_json::json!({"MaxCapacity": "1.5"}),
                serde_json::json!({"MaxCapacity": 1.5}),
            ),
            (
                "text that names no number",
                serde_json::json!({"FailureThreshold": "many"}),
                serde_json::json!({"FailureThreshold": "many"}),
            ),
            (
                "a collection of text, whose own keys name modelled members",
                serde_json::json!({"Variables": {"TTL": "60", "DisableScaleIn": "true"}}),
                serde_json::json!({"Variables": {"TTL": "60", "DisableScaleIn": "true"}}),
            ),
            (
                "number and boolean literals inside a collection of text",
                serde_json::json!({"Variables": {
                    "RETRIES": 3,
                    "RATIO": 1.5,
                    "DEBUG": true,
                    "QUIET": false,
                    "NAME": "reader"
                }}),
                serde_json::json!({"Variables": {
                    "RETRIES": "3",
                    "RATIO": "1.5",
                    "DEBUG": "true",
                    "QUIET": "false",
                    "NAME": "reader"
                }}),
            ),
            (
                "a collection of numbers, keyed by names of the requester's own",
                serde_json::json!({"AdditionalVersionWeights": {"2": "0.1", "3": 0.2}}),
                serde_json::json!({"AdditionalVersionWeights": {"2": 0.1, "3": 0.2}}),
            ),
        ];
        for (case, declared, expected) in cases {
            let actual = modelled_declaration("", declared, &classified);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// A document nested past the bound is left exactly as it stands rather than walked: the
    /// nesting is the requester's, and a walk bounded only by its input exhausts the stack.
    #[test]
    fn a_document_nested_past_the_bound_travels_as_written() {
        let mut declared = serde_json::json!({"TTL": "60"});
        for _ in 0..MAX_MODELLED_NESTING {
            declared = serde_json::json!({"Wrapper": declared});
        }
        let actual = modelled_declaration("", &declared, &classified);
        assert_eq!(
            actual, declared,
            "case \"a document nested past the bound\": expected it to travel as written, got \
             {actual}"
        );
    }
}
