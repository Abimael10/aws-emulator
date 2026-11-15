// SPDX-License-Identifier: CC0-1.0

//! How this service states a request BODY it refused for a modelled constraint.
//!
//! Two layers refuse a request to this service, and they do not word the refusal alike.
//!
//! The layer in FRONT of this service validates the request's JSON body against the
//! operation's modelled input as a SCHEMA, and reports what it found as that validator's
//! bracketed list of findings — one per constraint the body broke, each naming the rule
//! the value failed and the value that failed it, with no member path and no error count:
//! `[ECMA 262 regex "<the expression the model declares>" does not match input string
//! "<the submitted value>"]`, `[array is too short: must have at least <min> elements but
//! instance has <n> elements]`, `[instance value ("<submitted>") not found in enum
//! (possible values: ["<v1>","<v2>"])]`. Every sentence is the shared one
//! ([`::service_common::validation_message`]), so this service cannot word a finding
//! differently from another that states the same one, and every variable part comes from
//! the request and from the model.
//!
//! A value the request carries OUTSIDE its body — an `@httpLabel` path segment, an
//! `@httpQuery` parameter — is not a body finding: it is refused by the service's own
//! modelled validation error, in the framework's `N validation error(s) detected: Value
//! '<value>' at '<member path>' failed to satisfy constraint: …` envelope, which
//! `crate::service`'s refusal seam keeps (with the one rendering it states there).
//!
//! WHICH half a member travels in is a property of each operation's own input schema —
//! `GroupSecurityIdentifier` is a body member of `CreateTemplateGroupAccessControlEntry`
//! and a path segment of the operations that read, update and delete one entry — so the
//! split cannot be made from the refusal seam, which is handed the violations alone. It is
//! made by the shared front door ([`::service_common::front_door`]), which reads each
//! violated member's HTTP binding off the operation it refused; this module is the one
//! thing that layer leaves to the service: the words.
//!
//! ALL OR NOTHING. A refusal carrying one violation this layer states no finding for keeps
//! the framework's whole envelope rather than being reported half in one voice and half in
//! the other, which is why [`body_findings`] answers `None` for it.

use ::service_common::validation::SubmittedEcho;
use ::service_common::validation_message::{
    array_too_short, bracketed_findings, enum_value_not_found, list_item_count, map_entry_count,
    numeric_above_maximum, numeric_below_minimum, pattern_mismatch, string_too_long,
    string_too_short, too_many_properties,
};
use ::smithy_server_runtime::{Refused, Violation, ViolationKind};

/// The error code this service answers a refused request with, on the body half as on the
/// other: every operation of this model declares `ValidationException` (HTTP 400) for an
/// input the service will not accept (`smithy_query(query="digest")`), and it is the code
/// the framework's own refusal already names — so the chosen message travels with the code
/// a client was already branching on.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The findings this layer states for `refused`, the violations of one request's BODY, or
/// `None` when any one of them is a class it states no finding for.
///
/// The list is in the order the validating walk found them, which is the order the
/// operation's input declares its members in.
pub fn body_findings(refused: &[(Violation, SubmittedEcho)]) -> Option<Refused> {
    if refused.is_empty() {
        return None;
    }
    let findings: Vec<String> = refused
        .iter()
        .map(|(violation, echo)| finding(violation, echo))
        .collect::<Option<Vec<_>>>()?;
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message: bracketed_findings(&findings),
    })
}

/// The one finding this layer states for `violation`, or `None` when it states none.
///
/// A finding reports the rule the schema declares and the value the caller submitted, so a
/// violation whose value this walk did not render — a value with no text rendering, or one
/// the model marks `@sensitive`, which the refusal withholds — has no finding here and
/// leaves the whole refusal to the framework's envelope, where the member is named and the
/// value is not.
fn finding(violation: &Violation, echo: &SubmittedEcho) -> Option<String> {
    if violation.sensitive {
        return None;
    }
    match &violation.kind {
        // The expression as the MODEL declares it, anchors included: this is the schema's
        // own expression, not the per-member clause's rendering of it.
        ViolationKind::Pattern(pattern) => Some(pattern_mismatch(pattern, submitted(echo)?)),
        // The values the model declares for the enumerated type, in its declared order —
        // the order the validating walk carries them in — and the submitted value.
        ViolationKind::EnumValue(permitted) => {
            Some(enum_value_not_found(submitted(echo)?, permitted))
        }
        // `@length` bounds a string's characters, a list's elements and a map's entries,
        // and the validator states a different finding for each: a string quotes itself and
        // its length, an ARRAY states the two counts (it has no scalar rendering to quote),
        // and a MAP is the request's own JSON OBJECT, so its ceiling is a property count.
        ViolationKind::MinLength(min) => match collection_size(violation) {
            Some(Collection::List(found)) => Some(array_too_short(found, *min)),
            // No recorded finding for a map below a minimum: a request that broke one keeps
            // the framework's envelope rather than being given a sentence.
            Some(Collection::Map(_)) => None,
            None => {
                let value = submitted(echo)?;
                Some(string_too_short(value, value.chars().count(), *min))
            }
        },
        ViolationKind::MaxLength(max) => match collection_size(violation) {
            Some(Collection::Map(found)) => Some(too_many_properties(found, *max)),
            // No recorded finding for a list above a maximum, as above.
            Some(Collection::List(_)) => None,
            None => {
                let value = submitted(echo)?;
                Some(string_too_long(value, value.chars().count(), *max))
            }
        },
        // A number outside its `@range`: the bound as the model writes it and the value as
        // the request carried it, neither re-rendered here.
        ViolationKind::MinValue(min) => Some(numeric_below_minimum(submitted(echo)?, min)),
        ViolationKind::MaxValue(max) => Some(numeric_above_maximum(submitted(echo)?, max)),
        // Every other class (an absent required member, a repeated element) keeps the
        // framework's own wording: naming the member is what those refusals do here.
        _ => None,
    }
}

/// The value a finding quotes: the bytes the caller submitted, as the collecting walk
/// rendered them. `None` for a value that was absent or has no text rendering.
fn submitted(echo: &SubmittedEcho) -> Option<&str> {
    match echo {
        SubmittedEcho::Rendered(value) => Some(value.as_str()),
        SubmittedEcho::Absent | SubmittedEcho::Unrendered => None,
    }
}

/// The size of the COLLECTION a `@length` violation is about, and which kind of collection
/// it was.
enum Collection {
    /// A list, with the number of elements it carried.
    List(usize),
    /// A map, with the number of entries it carried.
    Map(usize),
}

/// Which collection `violation` is about and how large it was, or `None` when the violation
/// is about a string.
///
/// Read off the summary the validating walk stores for a collection it length-checked (a
/// list as `<n> items`, a map as `<n> entries`), through the shared readings of that
/// summary — the only signal available, since the violation carries the constraint that
/// was broken and not the kind of shape that broke it.
fn collection_size(violation: &Violation) -> Option<Collection> {
    let stored = violation.value.as_deref()?;
    if let Some(found) = list_item_count(stored) {
        return Some(Collection::List(found));
    }
    map_entry_count(stored).map(Collection::Map)
}
