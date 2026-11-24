// SPDX-License-Identifier: CC0-1.0

//! The NAME a user pool's hosted domain may take: the two forms a `Domain` value has, which
//! of them each request is held to, and the one sentence a name of neither form is refused
//! with.
//!
//! A pool's domain is named by the KIND of domain the request is about: "for custom domains,
//! this is the fully-qualified domain name, such as auth.example.com. For prefix domains, this
//! is the prefix alone, such as myprefix" (`CreateUserPoolDomain.Domain` in this service's
//! model). The model's `DomainType` pattern states the PREFIX form alone
//! ([`DOMAIN_PREFIX_PATTERN`]), which admits no dot — so it is not the form a custom domain's
//! name takes, and the operations that address a domain by its name serve a value the pattern
//! refuses (`crate::service`'s `served_despite`) and judge it here instead, where the request
//! says which kind it is about.
//!
//! Every reading here is TOTAL over any text a request can carry: the name is walked as
//! `char`s and split on a `char`, so nothing indexes into it, takes a byte offset in it or
//! computes a length from it — a caller cannot reach a panic through a `Domain` value,
//! whatever bytes it holds (the test at the end feeds it the shapes that would).

/// The expression a PREFIX domain's name has to match: `DomainType`'s own pattern
/// (`smithy_query(query="shape",shape="DomainType")`), as the model writes it — anchors
/// included, because a whole name has to match it. It is also the expression a refusal quotes
/// as the format a name must follow ([`domain_name_form_message`]).
pub const DOMAIN_PREFIX_PATTERN: &str = r"^[a-z0-9](?:[a-z0-9\-]{0,61}[a-z0-9])?$";

/// Whether `label` is one label of a domain name: the characters [`DOMAIN_PREFIX_PATTERN`]
/// admits — lowercase letters, digits and the hyphen — beginning and ending with one of the
/// first two, and not empty.
///
/// Read over `char`s, so a value carrying any text at all is answered rather than indexed
/// into.
fn is_domain_label(label: &str) -> bool {
    let inside = |character: char| character.is_ascii_lowercase() || character.is_ascii_digit();
    !label.is_empty()
        && label.starts_with(inside)
        && label.ends_with(inside)
        && label
            .chars()
            .all(|character| inside(character) || character == '-')
}

/// The sentence Amazon Cognito answers a `Domain` that is not a name a user pool's hosted
/// domain may take: it names the characters a domain name may carry and the format a name has
/// to follow, and quotes `pattern` as that format.
///
/// The one place that wording lives, so the refusal a HANDLER answers for a name whose form it
/// judged ([`domain_name_form_defect`]) and the one `crate::service` renders in place of a
/// constraint report cannot drift apart. Nothing of the submitted name is quoted into it.
#[must_use]
pub fn domain_name_form_message(pattern: &str) -> String {
    format!(
        "The domain name contains an invalid character. Domain names can only contain \
         lower-case letters, numbers, and hyphens. Please enter a different name that \
         follows this format: {pattern}"
    )
}

/// The refusal `domain` earns as the name of a domain of this kind, or `None` when it is a
/// name that kind may take.
///
/// `custom` is whether the request is about a CUSTOM domain (it carries a
/// `CustomDomainConfig`). A PREFIX is held to the one-label form the model's pattern states —
/// a dotted value is no prefix — while a CUSTOM domain's name is a name in the caller's OWN
/// namespace and is held to that same form per LABEL, of which it may have any number: the
/// fully-qualified name the member documents, and the single label the model's pattern admits.
///
/// Both refusals carry [`domain_name_form_message`], which quotes the expression a label is
/// built from.
#[must_use]
pub fn domain_name_form_defect(domain: &str, custom: bool) -> Option<String> {
    let mut labels = domain.split('.');
    let admitted = if custom {
        labels.all(is_domain_label)
    } else {
        // The prefix form is ONE label, of at most the 63 characters the model's length
        // admits — a bound the framework's own length check has already held it to.
        !domain.contains('.') && is_domain_label(domain)
    };
    (!admitted).then(|| domain_name_form_message(DOMAIN_PREFIX_PATTERN))
}

/// The refusal `domain` earns as the name a request ADDRESSES an existing domain by, or `None`
/// when it is a name a domain of either kind may have.
///
/// A create or an update STATES which kind of domain it means (it carries a
/// `CustomDomainConfig` or it does not), so [`domain_name_form_defect`] holds its name to that
/// kind's form. A delete names a domain the pool already has and says nothing about its kind,
/// so either form is a name it may address — and a value that is neither names no domain of
/// any pool, which is the form refusal rather than a lookup that found nothing.
#[must_use]
pub fn domain_name_addressed_defect(domain: &str) -> Option<String> {
    let admitted = domain_name_form_defect(domain, false).is_none()
        || domain_name_form_defect(domain, true).is_none();
    (!admitted).then(|| domain_name_form_message(DOMAIN_PREFIX_PATTERN))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row: the case, the name, whether the request is about a custom domain, whether
    /// that kind admits the name, and whether a request that merely ADDRESSES a domain by it
    /// does.
    type FormRow<'r> = (&'r str, &'r str, bool, bool, bool);

    /// Which names each form admits, and that EVERY name is answered rather than reaching a
    /// panic. The rows differ along the number of labels, the character that puts a label
    /// outside the form, the length of the value and the bytes it is built from — including
    /// the shapes a reading that indexed into the value would break on: multi-byte
    /// characters, a value of nothing but separators, and one far past every modelled bound.
    #[test]
    fn every_name_is_answered_and_each_form_admits_its_own() {
        let very_long = "a".repeat(10_000);
        let many_labels = "a.".repeat(5_000);
        let rows: [FormRow<'_>; 16] = [
            ("one label", "myprefix", false, true, true),
            ("one label as a custom domain", "myprefix", true, true, true),
            ("digits and hyphens", "a0-1-2b", false, true, true),
            ("one character", "a", false, true, true),
            (
                "a fully-qualified name",
                "auth.example.com",
                true,
                true,
                true,
            ),
            (
                "a fully-qualified name as a PREFIX",
                "auth.example.com",
                false,
                false,
                true,
            ),
            ("an upper-case letter", "Prefix", false, false, false),
            ("an underscore", "my_prefix", false, false, false),
            ("a leading hyphen", "-prefix", false, false, false),
            ("a trailing hyphen", "prefix-", false, false, false),
            ("nothing at all", "", false, false, false),
            ("a single separator", ".", true, false, false),
            ("nothing but separators", "....", true, false, false),
            (
                "a multi-byte character",
                "pr\u{e9}fix.example",
                true,
                false,
                false,
            ),
            ("a value far past every bound", very_long.as_str(), false, true, true),
            ("a value of many labels", many_labels.as_str(), true, false, false),
        ];
        for (case, domain, custom, admitted, addressable) in rows {
            let defect = domain_name_form_defect(domain, custom);
            assert_eq!(
                defect.is_none(),
                admitted,
                "case {case:?}: expected the form to admit the name: {admitted}, got {defect:?}"
            );
            if let Some(message) = defect {
                assert_eq!(
                    message,
                    domain_name_form_message(DOMAIN_PREFIX_PATTERN),
                    "case {case:?}: expected the one form sentence, got {message:?}"
                );
            }
            let addressed = domain_name_addressed_defect(domain);
            assert_eq!(
                addressed.is_none(),
                addressable,
                "case {case:?}: expected a request addressing a domain by it to be served: \
                 {addressable}, got {addressed:?}"
            );
        }
    }
}
