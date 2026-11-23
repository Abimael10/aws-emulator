// SPDX-License-Identifier: CC0-1.0

//! The expression a `@pattern` violation of this service's input quotes: the one this
//! service PUBLISHES for the member's shape.
//!
//! RULE: a constraint clause quotes the expression the service advertises for the shape the
//! member targets — for most of this model's shapes that is the modelled expression without
//! the whole-value `^`/`$` pair the model writes around it, and for the shapes listed here
//! it is the model's own text, that pair included. Which of the two a shape is published
//! with cannot be read off the text, because both are spelled `^…$` in the model, so it is
//! reference data about the SHAPE, keyed by the expression — what is published belongs to
//! the shape, so it holds for every member targeting it, in any operation and at any depth.
//!
//! Nothing about either rendering lives here: the rule and both forms are the shared home's
//! ([`::service_common::validation::unanchored_unless_published_as_declared`]), and this
//! module supplies only which of this model's expressions are published as declared. The
//! CHECK is unchanged either way — a value is always matched against the model's pattern as
//! written; only the quoting is decided here.

/// `KmsKeyIdentifier`'s own expression, as the model declares it
/// (`smithy_query(query="shape", shape="KmsKeyIdentifier")`): the shape the
/// `KmsKeyIdentifier` member of a connection, an event bus and an archive all target.
/// Named here because this service publishes it exactly as declared
/// ([`PUBLISHED_AS_DECLARED`]).
const KMS_KEY_IDENTIFIER_PATTERN: &str = r"^[a-zA-Z0-9_\-/:]*$";

/// The expressions this service publishes as the model writes them, the whole-value
/// anchoring pair included, keyed by the EXPRESSION.
///
/// Reference data about those shapes rather than something readable off their text: this
/// API's refusals are recorded quoting some of its expressions as declared and the rest
/// without that pair (an event source's name, say, is quoted without it).
const PUBLISHED_AS_DECLARED: [&str; 1] = [KMS_KEY_IDENTIFIER_PATTERN];

/// The requirement clause for a `@pattern` violation: the expression the value had to
/// satisfy, as this service publishes it for the member's shape.
pub fn pattern_requirement(pattern: &str) -> String {
    format!(
        "Member must satisfy regular expression pattern: {}",
        ::service_common::validation::unanchored_unless_published_as_declared(
            pattern,
            &PUBLISHED_AS_DECLARED,
        )
    )
}
