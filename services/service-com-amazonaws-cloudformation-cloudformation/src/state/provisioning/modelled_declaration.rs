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
//! own validation is what judges it.

use ::serde_json::Value;

/// What the owning API models one member as, where that is not TEXT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModelledScalar {
    /// Text, or anything else this reading does not change: the value travels as the
    /// declaration wrote it.
    AsWritten,
    /// A number — read as the WHOLE number the text names where it names one (a capacity is an
    /// integer member, and `1` is not the same JSON value as `1.0`), and as a fractional one
    /// otherwise.
    Number,
    /// A boolean, written `true` or `false` in either case.
    Boolean,
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
pub(super) fn modelled_declaration(
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
    match value {
        Value::String(text) => match modelled_as(member) {
            ModelledScalar::AsWritten => value.clone(),
            ModelledScalar::Number => as_number(text).unwrap_or_else(|| value.clone()),
            ModelledScalar::Boolean => as_boolean(text).unwrap_or_else(|| value.clone()),
        },
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, held)| (name.clone(), modelled_at(name, held, depth + 1, modelled_as)))
                .collect(),
        ),
        // A list's entries are values of the member the list belongs to, so each is read under
        // that same member name.
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| modelled_at(member, entry, depth + 1, modelled_as))
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
            _ => ModelledScalar::AsWritten,
        }
    }

    /// RULE: a member the owning model types as a number or a boolean is read as that type from
    /// the text a template may have quoted it as, at every depth the document nests it; every
    /// other member, and a value that names no such scalar, travels as written.
    ///
    /// The rows differ along what the rule generalises over: the scalar's modelled type, the
    /// depth it sits at, whether it stands inside a list, the spelling of a boolean, whether
    /// the number is whole or fractional, and whether the text names such a value at all.
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
