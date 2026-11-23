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
//! [`unusable_parameters`] is which of the two a request gets, so the layer split is decided
//! in one place rather than per handler. It is handed a finding ([`Unusable`]) for EVERY
//! parameter the request could not be served on before it words anything, because a sentence
//! names every parameter the request failed on, not merely the first one a handler looked at
//! — so a handler reads all of its own parameters first and words the refusal once.
//! [`unusable_required_labels`] is that same rule for a request whose only own-validation
//! parameters are its `@required` URI labels.
//!
//! WHICH values are unusable is the same question for a parameter this service merely has a
//! default for, so it is answered here too: [`optional_parameter`] tells a value that bound
//! NOTHING (so the default applies) from one that bound a BLANK value (so this service's
//! validation was reached and cannot use it). An optional parameter is not a reason to read
//! an unusable value as an absent one.
//!
//! A parameter this service refuses with NO sentence at all ([`Unusable::Unworded`])
//! contributes no name, so it can never silence a sentence another parameter of the same
//! request earned: the refusal carries none only when every unusable parameter is of that
//! class.
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

/// WHY one parameter of a request could not be served — the finding a handler records for
/// it, which is what decides the layer, and so the sentence, the whole request is refused
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// The parameter bound NO value at all, so the request never named a target: the
    /// request-binding layer reports it ([`missing_request_parameters`], by the modelled
    /// member name).
    Unbound,
    /// The parameter reached this service's own validation carrying a value it cannot use
    /// — absent where this service needs one, blank, or malformed — and that layer NAMES
    /// it ([`invalid_parameters`], by the service's own spelling).
    Unreadable,
    /// The parameter reached this service's own validation carrying a value it cannot use,
    /// and this service states NO sentence for that class of refusal: the code and the
    /// status are the whole answer. It therefore contributes no NAME to the one sentence,
    /// and never silences a sentence another parameter of the same request earned.
    Unworded,
}

/// The ONE refusal a request earns for the parameters its layers cannot use.
///
/// Separate from the `Option` that says whether the request is refused at all, because a
/// refusal that carries no sentence is not the same thing as no refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterRefusal {
    /// The detail text to answer with, or `None` where every unusable parameter belongs to
    /// the class this service states no sentence for ([`Unusable::Unworded`]).
    pub message: Option<String>,
}

/// The refusal for `findings` — `(modelled member name, why it cannot be served)` for every
/// parameter of one request a layer could not use, in the order the operation's input binds
/// them — or `None` when the request carried no unusable parameter at all.
///
/// This is the LAYER SPLIT the two sentences above belong to, stated once. A request is
/// refused ONCE, so the one sentence names EVERY parameter unusable in the layer that
/// refused it: the binding layer runs first, so if any parameter bound no value at all that
/// layer's sentence lists all of them and this service's validation is never reached;
/// otherwise the validation sentence lists every parameter it cannot read. Either way the
/// names are listed in the order given, which is the order the input binds them — not the
/// order a handler happened to look at them in, which is why a handler records a finding for
/// every one of its own parameters before calling this.
///
/// Only the layer differs — both are the modelled `BadRequestException` at HTTP 400, and
/// both are answered before any state is read, so neither can be mistaken for the 404 a
/// name that merely does not exist gets.
pub fn unusable_parameters(findings: &[(&str, Unusable)]) -> Option<ParameterRefusal> {
    if findings.is_empty() {
        return None;
    }

    let named = |layer: Unusable| -> Vec<&str> {
        findings
            .iter()
            .filter(|(_, why)| *why == layer)
            .map(|(member, _)| *member)
            .collect()
    };

    let unbound = named(Unusable::Unbound);
    if !unbound.is_empty() {
        return Some(ParameterRefusal {
            message: Some(missing_request_parameters(&unbound)),
        });
    }
    // Every remaining finding reached this service's own validation. Those it has a name
    // for are listed; one of the sentence-less class adds nothing, so a sentence is worded
    // whenever ANY of them is nameable, and withheld only when none is.
    let unreadable = named(Unusable::Unreadable);
    Some(ParameterRefusal {
        message: (!unreadable.is_empty()).then(|| invalid_parameters(&unreadable)),
    })
}

/// Why a `@required` URI-bound label carrying `value` cannot be served, or `None` when the
/// value is usable.
///
/// An EMPTY value bound nothing at all, so the request never named a target and the
/// request-binding layer reports it; a value that is present but BLANK did bind, so it
/// reached this service's own validation, which cannot use it. A label is never the
/// sentence-less class: this service names every label it refuses.
pub fn unusable_label(value: &str) -> Option<Unusable> {
    if value.is_empty() {
        Some(Unusable::Unbound)
    } else if value.trim().is_empty() {
        Some(Unusable::Unreadable)
    } else {
        None
    }
}

/// What a request bound to one OPTIONAL parameter, as the parameter's reader must read it.
///
/// The same EMPTY/BLANK split [`unusable_label`] states, for a parameter whose absence is
/// SERVED (by this service's own default) instead of refused: a value that bound nothing is
/// no choice at all, while a BLANK value did bind and so reached this service's validation,
/// which cannot use it. Folding the blank case onto the absent one would answer a request
/// that named an unusable value as though it had named none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalValue<'a> {
    /// Nothing bound at all — the parameter is absent, or present carrying an empty value
    /// — so the request named no choice and this service's own default applies.
    Nothing,
    /// A value bound, and it is BLANK: it reached this service's own validation, which
    /// cannot use it, so the request is refused for it like any other unusable value.
    Blank,
    /// A value bound that carries content, with any surrounding padding removed — what the
    /// parameter's own reader then reads.
    Present(&'a str),
}

/// Classify the `value` a request bound to one optional parameter, or `None` where the
/// parameter was absent.
///
/// This is [`unusable_label`]'s split stated for the optional parameters, so the line
/// between "named no choice" and "named a value this service cannot use" is drawn in ONE
/// place for every parameter of this service, whether it travels as a URI label, a query
/// parameter or a body member.
pub fn optional_parameter(value: Option<&str>) -> OptionalValue<'_> {
    match value {
        None => OptionalValue::Nothing,
        Some(raw) => match unusable_label(raw) {
            // Empty: nothing bound, exactly as an absent parameter.
            Some(Unusable::Unbound) => OptionalValue::Nothing,
            // Present but blank: bound, and unusable.
            Some(_) => OptionalValue::Blank,
            // Content: the padding around it is not part of the value.
            None => OptionalValue::Present(raw.trim()),
        },
    }
}

/// The refusal a request gets for the `@required` URI-bound `labels` it arrived with —
/// `(modelled member name, value)`, in the order the operation's input binds them — or
/// `None` when every value is usable.
///
/// [`unusable_parameters`] stated for the operations whose only own-validation parameters
/// are their labels: each value is classified by [`unusable_label`] and the findings are
/// worded as one refusal. The sentence is never absent here, because no label is of the
/// sentence-less class.
pub fn unusable_required_labels(labels: &[(&str, &str)]) -> Option<String> {
    let findings: Vec<(&str, Unusable)> = labels
        .iter()
        .filter_map(|(member, value)| unusable_label(value).map(|why| (*member, why)))
        .collect();
    unusable_parameters(&findings).and_then(|refusal| refusal.message)
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

    /// RULE: a request is refused ONCE, so the one refusal reports every parameter the
    /// refusing layer cannot use, in the order the input binds them — and a parameter of the
    /// class this service states no sentence for adds no name, so it cannot silence the
    /// sentence another parameter of the same request earned.
    ///
    /// The rows differ along what the rule generalises over: how many parameters are
    /// unusable, which of them, their order, and WHICH LAYER each failed in — including both
    /// pairings of the sentence-less class with a nameable one.
    #[test]
    fn one_refusal_reports_every_parameter_the_refusing_layer_cannot_use() {
        for (case, findings, expected) in [
            ("every parameter usable", &[][..], None),
            (
                "one parameter unreadable",
                &[("First", Unusable::Unreadable)][..],
                Some(Some(
                    "The following parameter is null, empty or invalid: ['first']".to_string(),
                )),
            ),
            (
                "two parameters unreadable, named in the order the input binds them",
                &[
                    ("First", Unusable::Unreadable),
                    ("Second", Unusable::Unreadable),
                ][..],
                Some(Some(
                    "The following parameter is null, empty or invalid: ['first', 'second']"
                        .to_string(),
                )),
            ),
            (
                "the sentence-less class alone: refused, with no sentence",
                &[("First", Unusable::Unworded)][..],
                Some(None),
            ),
            (
                "two of the sentence-less class: still no sentence",
                &[("First", Unusable::Unworded), ("Second", Unusable::Unworded)][..],
                Some(None),
            ),
            (
                "a nameable parameter before one of the sentence-less class",
                &[("First", Unusable::Unreadable), ("Second", Unusable::Unworded)][..],
                Some(Some(
                    "The following parameter is null, empty or invalid: ['first']".to_string(),
                )),
            ),
            (
                "a nameable parameter AFTER one of the sentence-less class",
                &[("First", Unusable::Unworded), ("Second", Unusable::Unreadable)][..],
                Some(Some(
                    "The following parameter is null, empty or invalid: ['second']".to_string(),
                )),
            ),
            (
                "an unbound parameter: the earlier layer answers alone",
                &[
                    ("First", Unusable::Unreadable),
                    ("Second", Unusable::Unbound),
                    ("Third", Unusable::Unworded),
                ][..],
                Some(Some(
                    "Missing required request parameters: [Second]".to_string(),
                )),
            ),
            (
                "two unbound parameters, in the order the input binds them",
                &[("First", Unusable::Unbound), ("Second", Unusable::Unbound)][..],
                Some(Some(
                    "Missing required request parameters: [First, Second]".to_string(),
                )),
            ),
        ] {
            let answer = unusable_parameters(findings).map(|refusal| refusal.message);
            assert_eq!(
                answer, expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
    }

    /// RULE: an optional parameter's value bound NOTHING only when it is absent or empty; a
    /// value that is present but blank DID bind and is unusable, and a value with content is
    /// read with its surrounding padding removed.
    ///
    /// The rows differ along what the rule generalises over: whether a value bound at all,
    /// the FORM of a blank value (space, tab, newline, several of them), and whether a value
    /// with content carries padding, on which side, and of which whitespace.
    #[test]
    fn an_optional_value_is_nothing_only_when_it_bound_nothing() {
        for (case, value, expected) in [
            ("absent", None, OptionalValue::Nothing),
            ("present but empty", Some(""), OptionalValue::Nothing),
            ("one space", Some(" "), OptionalValue::Blank),
            ("one tab", Some("\t"), OptionalValue::Blank),
            ("a newline", Some("\n"), OptionalValue::Blank),
            ("several kinds of whitespace", Some(" \t\r\n "), OptionalValue::Blank),
            ("content alone", Some("value"), OptionalValue::Present("value")),
            ("content padded on the left", Some("  value"), OptionalValue::Present("value")),
            ("content padded on the right", Some("value\t"), OptionalValue::Present("value")),
            ("content padded on both sides", Some(" value "), OptionalValue::Present("value")),
            // Whitespace INSIDE a value is part of it: only the padding is removed.
            ("content with whitespace inside it", Some(" a b "), OptionalValue::Present("a b")),
        ] {
            let answer = optional_parameter(value);
            assert_eq!(
                answer, expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
    }
}
