// SPDX-License-Identifier: CC0-1.0

//! How this service words a request it refuses for a parameter it cannot use.
//!
//! Every operation in this model declares ONE client-input error —
//! `BadRequestException` (HTTP 400, doc "The input you provided is invalid.";
//! `smithy_query(query="shape",shape="BadRequestException",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`)
//! — so the status and the code are the same for every such refusal and only the sentence
//! differs. This module is the single home of the two sentences, so two operations cannot
//! word one class of refusal differently.
//!
//! The two classes are distinguished by WHERE the parameter failed:
//!
//! * [`missing_request_parameters`] — a `@required` parameter the request binds to the URI
//!   arrived with no value at all, so the request never named a target. The request-binding
//!   layer reports it, which names a parameter by its MODELLED member name.
//! * [`invalid_parameters`] — a parameter reached this service's own validation carrying a
//!   value it cannot use: absent where this service needs one, blank, or malformed. That
//!   layer names a parameter by the service's own spelling, which is the modelled member
//!   name with its first character lowercased (see [`parameter_name`]).
//!
//! [`unusable_required_labels`] is which of the two a request's URI-bound parameters get,
//! so the layer split is decided in one place rather than per handler. It is handed ALL of
//! a request's `@required` labels before it words anything, because a sentence names every
//! parameter the request failed on, not merely the first one looked at.
//!
//! Both sentences take the parameter name(s) FROM THE REQUEST: nothing here is keyed to one
//! operation or one recorded value.

/// The refusal for `members`: `@required` request parameters that arrived with no value.
///
/// Each name is the parameter's modelled member name, as this layer reports it, and they
/// are listed in the order given — the order the operation's input declares them, so a
/// request missing two parameters reads the same way every time.
pub fn missing_request_parameters(members: &[&str]) -> String {
    format!("Missing required request parameters: [{}]", members.join(", "))
}

/// The refusal for `members`: parameters this service's validation cannot use, whether
/// they arrived null, empty, or holding a value it cannot read.
///
/// `members` are MODELLED member names; each is rendered through [`parameter_name`] and
/// single-quoted, in the order given.
pub fn invalid_parameters(members: &[&str]) -> String {
    let named: Vec<String> = members
        .iter()
        .map(|member| format!("'{}'", parameter_name(member)))
        .collect();
    format!(
        "The following parameter is null, empty or invalid: [{}]",
        named.join(", ")
    )
}

/// The refusal a request gets for the `@required` URI-bound `labels` it arrived with —
/// `(modelled member name, value)`, in the order the operation's input binds them — or
/// `None` when every value is usable.
///
/// This is the LAYER SPLIT the two sentences above belong to, stated once:
///
/// * an EMPTY value bound nothing at all, so the request never named a target and the
///   request-binding layer reports it ([`missing_request_parameters`]);
/// * a value that is present but BLANK did bind, so it reached this service's own
///   validation, which cannot use it ([`invalid_parameters`]).
///
/// A request is refused ONCE, so the one sentence names EVERY label unusable in the layer
/// that refused it: the binding layer runs first, so if any label bound no value at all
/// that layer's sentence lists all of them and this service's validation is never reached;
/// otherwise the validation sentence lists every label that bound a blank value. Either way
/// the names are listed in the order given, which is the order the input binds them.
///
/// Only the layer differs — both are the modelled `BadRequestException` at HTTP 400, and
/// both are answered before any state is read, so neither can be mistaken for the 404 a
/// name that merely does not exist gets.
pub fn unusable_required_labels(labels: &[(&str, &str)]) -> Option<String> {
    let named = |unusable: fn(&str) -> bool| -> Vec<&str> {
        labels
            .iter()
            .filter(|(_, value)| unusable(value))
            .map(|(member, _)| *member)
            .collect()
    };

    let missing = named(str::is_empty);
    if !missing.is_empty() {
        return Some(missing_request_parameters(&missing));
    }
    let invalid = named(|value| value.trim().is_empty());
    if !invalid.is_empty() {
        return Some(invalid_parameters(&invalid));
    }
    None
}

/// A modelled member name as this service's own validation spells a parameter: the member
/// name with its FIRST character lowercased and the rest left exactly as modelled
/// (`NextToken` -> `nextToken`, `PageSize` -> `pageSize`).
///
/// The models in this namespace declare an upper-camel member name and bind it to the wire
/// under that same name, while the service's validation layer reports the lower-camel form
/// of it — so the spelling is derived from the member rather than written out per call
/// site.
pub fn parameter_name(member: &str) -> String {
    let mut characters = member.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: one refusal names every label unusable in the layer that refused it, in the
    /// order given, and the request-binding layer (an EMPTY value) is reached before this
    /// service's validation (a BLANK one).
    ///
    /// The rows differ in how many labels are unusable, which of them, and the form of each
    /// unusable value. The all-empty row is pinned here rather than on the wire: the only
    /// operations with two `@required` labels bind the second as the LAST URI segment, to
    /// which no request can bind an empty value, so this layer is where that combination is
    /// stated.
    #[test]
    fn one_sentence_names_every_label_the_refusing_layer_cannot_use() {
        for (case, labels, expected) in [
            (
                "every label usable",
                &[("First", "alpha"), ("Second", "beta")][..],
                None,
            ),
            (
                "one label empty",
                &[("First", ""), ("Second", "beta")][..],
                Some("Missing required request parameters: [First]"),
            ),
            (
                "both labels empty",
                &[("First", ""), ("Second", "")][..],
                Some("Missing required request parameters: [First, Second]"),
            ),
            (
                "one label blank",
                &[("First", "alpha"), ("Second", " ")][..],
                Some("The following parameter is null, empty or invalid: ['second']"),
            ),
            (
                "both labels blank",
                &[("First", "\t"), ("Second", "   ")][..],
                Some("The following parameter is null, empty or invalid: ['first', 'second']"),
            ),
            (
                "one empty and one blank: the earlier layer answers alone",
                &[("First", " "), ("Second", "")][..],
                Some("Missing required request parameters: [Second]"),
            ),
            ("a single usable label", &[("First", "alpha")][..], None),
        ] {
            let answer = unusable_required_labels(labels);
            assert_eq!(
                answer.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
    }
}
