// SPDX-License-Identifier: CC0-1.0

//! The `name` a resource of a GraphQL API carries: the form `ResourceName` declares for
//! it, and the refusal a value outside that form takes.
//!
//! `ResourceName` is a string `@length` 1..65536 with `@pattern
//! "^[_A-Za-z][_0-9A-Za-z]*$"` (`smithy_query(query="shape", shape="ResourceName")`) — a
//! GraphQL identifier, which is why no name may start with a digit or hold a hyphen.
//!
//! THE REFUSAL IS THE SERVICE'S, not the framework's. A value outside that form is
//! answered with the operation's own modelled `BadRequestException` (HTTP 400) carrying
//! ONE sentence that names the resource whose name was refused, where the framework's
//! generic report would name `ValidationException` — a shape no operation of this service
//! models, so a client's typed-error dispatch would miss it entirely — and recite the
//! member's expression.
//!
//! That is why the check runs HERE rather than at the request boundary (`skip_validation`
//! on `name` in `src/ops/types/`, the same split [`crate::state::api_name_refusal`] is
//! there for): the seam that words a boundary refusal
//! (`smithy_server_runtime::Service::override_refusal`) is handed the broken clauses
//! alone, with no operation to key on, and the sentence names the RESOURCE the operation
//! creates — so the operation's own handler is the one place it can be chosen.

/// `ResourceName`'s modelled length bounds.
pub const RESOURCE_NAME_MIN_LEN: usize = 1;
pub const RESOURCE_NAME_MAX_LEN: usize = 65_536;

/// `ResourceName`'s modelled `@pattern`, as the model writes it — anchored at both ends,
/// so the expression has to admit the WHOLE name.
pub const RESOURCE_NAME_PATTERN: &str = r"^[_A-Za-z][_0-9A-Za-z]*$";

/// The resource a function's name belongs to, as the service names it in a refusal of
/// that name.
pub const FUNCTION_RESOURCE: &str = "Function";

/// [`RESOURCE_NAME_PATTERN`] compiled, which is how it is EVALUATED: see
/// [`super::declared_pattern`] for the one reading of a `@pattern` a handler checks
/// itself.
static RESOURCE_NAME_REGEX: super::declared_pattern::CompiledPattern =
    ::std::sync::LazyLock::new(|| ::regex::Regex::new(RESOURCE_NAME_PATTERN).ok());

/// Whether `name` satisfies [`RESOURCE_NAME_PATTERN`].
fn matches_resource_name_pattern(name: &str) -> bool {
    super::declared_pattern::matches_declared_pattern(&RESOURCE_NAME_REGEX, name)
}

/// The text `BadRequestException` (HTTP 400) carries for a `resource`'s name that is
/// outside the form `ResourceName` declares.
///
/// The sentence states WHICH resource's name was refused and sends the caller to the
/// documentation for the form: it recites neither the expression nor the submitted value,
/// which is how the service words a refusal of this member.
pub fn invalid_resource_name_message(resource: &str) -> String {
    format!(
        "The {resource} name is invalid. Please refer to the documentation for more details."
    )
}

/// The refusal text for a `resource`'s `name` outside the form `ResourceName` declares —
/// its length bounds or its expression — or `None` for one inside it.
///
/// The length is counted in CHARACTERS, which is what Smithy's `@length` bounds on a
/// string, so a multi-byte name is measured the way the model measures it. Both
/// constraints take the SAME sentence: the service states the member as invalid rather
/// than stating which clause it broke.
pub fn resource_name_refusal(resource: &str, name: &str) -> Option<String> {
    let length = name.chars().count();
    let within_bounds = (RESOURCE_NAME_MIN_LEN..=RESOURCE_NAME_MAX_LEN).contains(&length);
    (!within_bounds || !matches_resource_name_pattern(name))
        .then(|| invalid_resource_name_message(resource))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a name outside the declared form takes the service's one sentence, naming the
    /// resource and nothing of the request.
    #[test]
    fn a_name_outside_the_declared_form_is_refused_by_naming_the_resource() {
        for (case, name) in [
            ("a hyphen, which the expression does not admit", "bad-name"),
            ("a leading digit", "1function"),
            ("a dot", "my.function"),
            ("a space", "my function"),
            ("an empty name, short of the minimum length", ""),
            ("a non-ASCII letter", "fonctionné"),
        ] {
            let refusal = resource_name_refusal(FUNCTION_RESOURCE, name);
            assert_eq!(
                refusal.as_deref(),
                Some(
                    "The Function name is invalid. Please refer to the documentation for more \
                     details."
                ),
                "case {case:?}: expected the service's sentence for {name:?}, got {refusal:?}"
            );
            let stated = refusal.unwrap_or_default();
            assert!(
                !stated.contains(name) || name.is_empty(),
                "case {case:?}: expected nothing of the submitted name quoted, got {stated:?}"
            );
        }
    }

    /// RULE: a name INSIDE the declared form is not refused — at each published bound of
    /// the length and over every character class the expression admits.
    #[test]
    fn a_name_inside_the_declared_form_is_not_refused() {
        for (case, name) in [
            ("one character, the shortest the bound admits", "f".to_string()),
            ("an underscore start", "_private".to_string()),
            ("digits after the first character", "f1_2".to_string()),
            ("every class the expression admits", "Fn_0az".to_string()),
            (
                "the longest name the bound admits",
                "f".repeat(RESOURCE_NAME_MAX_LEN),
            ),
        ] {
            let refusal = resource_name_refusal(FUNCTION_RESOURCE, &name);
            assert!(
                refusal.is_none(),
                "case {case:?}: expected no refusal, got {refusal:?}"
            );
        }

        // One past the published maximum is refused, so the bound above is the bound.
        let over_long = "f".repeat(RESOURCE_NAME_MAX_LEN + 1);
        assert!(
            resource_name_refusal(FUNCTION_RESOURCE, &over_long).is_some(),
            "case \"one character past the maximum length\": expected a refusal"
        );
    }

    /// RULE: the sentence names whichever resource it was asked about.
    #[test]
    fn the_sentence_names_the_resource_it_was_asked_about() {
        for resource in [FUNCTION_RESOURCE, "DataSource"] {
            let stated = invalid_resource_name_message(resource);
            assert_eq!(
                stated,
                format!(
                    "The {resource} name is invalid. Please refer to the documentation for \
                     more details."
                ),
                "case {resource:?}: got {stated:?}"
            );
        }
    }
}
