// SPDX-License-Identifier: CC0-1.0

//! The ONE scanner over a text that refers to values BY NAME.
//!
//! Two different texts of this service carry such references, with two different
//! reference forms:
//!
//! * an action's `configuration` value refers to a variable of its execution as
//!   `#{<namespace>.<name>}` ([`VARIABLE_REFERENCE_PREFIX`]) — what the engine resolves
//!   before it runs the action and what `ActionExecutionInput$resolvedConfiguration`
//!   reports;
//! * an action type's URL template refers to a property of the action's configuration as
//!   `{Config:<property>}` (`CreateCustomActionTypeInput$configurationProperties`: "you
//!   can refer to a name in the configuration properties of the custom action within the
//!   URL templates by following the format of {Config:name}").
//!
//! The WALK is the same either way, and it is the walk that carries the properties that
//! matter: ONE pass over the text, so a replacement's own text is never re-scanned and no
//! chain of values can amplify one request; and a byte budget checked as the text is
//! built, so the allocation a request can drive is bounded by a stated figure however many
//! references the text carries. Written once here, so the two cannot drift apart on
//! either.
//!
//! What differs between them is only what an UNRESOLVABLE reference means, which each
//! caller states as an [`Unresolved`] policy.

use std::collections::BTreeMap;

/// The reference form an action's configuration refers to one of its execution's
/// variables by.
pub const VARIABLE_REFERENCE_PREFIX: &str = "#{";

/// The character a reference of either form closes with.
const REFERENCE_SUFFIX: char = '}';

/// What a scan does with a reference it cannot resolve — one naming a value the caller
/// has none for, or one the text never closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolved {
    /// Keep the reference's own text, exactly as written, and carry on.
    ///
    /// The right answer where the resolved text is still usable without it: an action's
    /// configuration keeps an unresolved `#{...}` so the action can see — and fail on —
    /// the reference itself rather than an empty string.
    Literal,
    /// The text has no resolved form at all, so the scan answers `None`.
    ///
    /// The right answer where a half-resolved text would be WRONG rather than
    /// unresolved: a reported `resolvedConfiguration` (which is absent instead), or a URL
    /// that would otherwise carry template text where a resource name belongs.
    NoResolution,
}

/// `text` with every `<prefix><name>}` reference replaced by `value_of(name)`.
///
/// `None` when the text has no resolved form: a reference `unresolved` says there is none
/// for, or a result that would pass `budget` bytes — the length the member carrying this
/// text is modelled for, so a text past it is answered unresolved rather than allocated.
/// A text with no reference in it resolves to itself.
///
/// The scan is a SINGLE pass: what a replacement puts into the result is never read as a
/// reference again, so a value that itself looks like one is substituted literally.
pub fn substitute_references(
    text: &str,
    prefix: &str,
    budget: usize,
    unresolved: Unresolved,
    value_of: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut resolved = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(prefix) {
        let (before, reference) = (&rest[..start], &rest[start + prefix.len()..]);
        let Some(end) = reference.find(REFERENCE_SUFFIX) else {
            // A reference the text never closes: nothing names a value, so the rest of
            // the text is literal — or there is no resolved form at all.
            match unresolved {
                Unresolved::Literal => break,
                Unresolved::NoResolution => return None,
            }
        };
        let name = &reference[..end];
        let substitution = match (value_of(name), unresolved) {
            (Some(value), _) => value,
            (None, Unresolved::NoResolution) => return None,
            // The reference's own text, which is what the budget then accounts for.
            (None, Unresolved::Literal) => format!("{prefix}{name}{REFERENCE_SUFFIX}"),
        };
        if resolved.len() + before.len() + substitution.len() > budget {
            return None;
        }
        resolved.push_str(before);
        resolved.push_str(&substitution);
        rest = &reference[end + REFERENCE_SUFFIX.len_utf8()..];
    }
    (resolved.len() + rest.len() <= budget).then(|| {
        resolved.push_str(rest);
        resolved
    })
}

/// The lookup [`substitute_references`] takes over a map keyed by the reference's full
/// name, so a caller holding one states the lookup once rather than closing over it by
/// hand.
pub fn value_in(values: &BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| values.get(name).cloned()
}
