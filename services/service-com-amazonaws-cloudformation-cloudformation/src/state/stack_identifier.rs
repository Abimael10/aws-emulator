// SPDX-License-Identifier: CC0-1.0

//! The constraints CloudFormation holds a stack IDENTIFIER to, applied before the store is
//! read.
//!
//! ## The rule
//!
//! A member that identifies a stack by NAME OR ID carries two constraints — at least one
//! character, and the name-or-ARN expression — and CloudFormation checks them on the INPUT,
//! ahead of resolving the stack: a request whose identifier no stack could ever carry is told
//! that its request is malformed rather than that the stack is missing, and EVERY constraint
//! the value breaks is reported in one envelope, the expression first.
//!
//! The expression is what CloudFormation STATES when it will not take a value, and it is also
//! what it TAKES: a stack NAME, or a stack ARN. It is applied the way a `@pattern` constraint
//! is applied — as a SEARCH for the expression ANYWHERE in the value rather than a match of
//! the whole of it, which is how the expression reaches a refusal: unanchored, as the message
//! quotes it. So a value that carries a name run (a letter followed by letters, digits and
//! hyphens) or an `arn:` prefix anywhere in it is admitted, and only a value carrying neither
//! — digits, hyphens, punctuation or spaces alone, and the empty value — is refused for its
//! own shape ahead of the lookup.
//!
//! An admitted value is RESOLVED, and one that names no stack is answered as the missing stack
//! it is ([`super::stack_does_not_exist`] and the forms beside it). The bare unique id a stack
//! id (`arn:…:stack/<name>/<unique-id>`) ENDS in addresses no stack, but it carries a name run
//! whatever hexadecimal digit it begins with, so it is admitted and answered as the stack it
//! does not name rather than for its shape.
//!
//! ## Why the check lives here and not at the boundary
//!
//! Most of this service's identifier members target a model shape that declares NEITHER
//! constraint (`StackName`: `smithy_query(query="shape", shape="StackName")` carries no
//! `@length` and no `@pattern`), so the framework's constraint walk has nothing to check for
//! them and the operation is where the check belongs. A member whose model shape DOES carry
//! them (`StackNameOrId`) is already refused at the boundary, in the framework's own
//! envelope, and is not re-checked here.
//!
//! The clause, the envelope and the requirement wording are the framework's
//! ([`::aws_smithy_schema_server::envelope`]), so a refusal stated here cannot drift from one
//! the boundary states; what is this service's own is the EXPRESSION its validator states for
//! an identifier and the member path each operation states it at.

use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};

/// The expression CloudFormation's own input validation states for a stack identifier: a
/// stack NAME ("A stack name can contain only alphanumeric characters (case sensitive) and
/// hyphens. It must start with an alphabetical character", which is
/// [`super::STACK_NAME_PATTERN`]) or a stack ARN.
///
/// Quoted as the SERVICE states it, which is not the anchored spelling the model writes for
/// the members that do carry a `@pattern`
/// (`smithy_query(query="shape", shape="StackNameOrId")`): a requirement reaches a message as
/// the layer that refuses states it. It is both the expression a refusal STATES and the set of
/// values taken as addressing a stack ([`is_an_addressable_form`]), and it is unanchored in
/// both — the one expression, read the one way.
pub const STACK_IDENTIFIER_PATTERN: &str = "[a-zA-Z][-a-zA-Z0-9]*|arn:[-a-zA-Z0-9:/._+]*";

/// The least a stack identifier may hold: `@length(min: 1)`, which the model states for the
/// identifier shape that carries its constraints
/// (`smithy_query(query="shape", shape="StackNameOrId")`) and which CloudFormation applies to
/// every member that takes one.
pub const STACK_IDENTIFIER_MIN_LENGTH: usize = 1;

/// The member path a refusal states a stack identifier at for an operation whose validator
/// names that member `StackName` — the lowerCamel spelling every violation path takes.
pub const STACK_NAME_PATH: &str = "stackName";

/// The member path a refusal states a stack identifier at for an operation whose validator
/// names that member `StackNameOrId` instead (`ListStackResources`).
///
/// CloudFormation does not name the member identically in every operation, so the path is
/// the operation's own: it is what tells a caller which parameter of the call it sent was
/// refused.
pub const STACK_NAME_OR_ID_PATH: &str = "stackNameOrId";

/// [`STACK_IDENTIFIER_PATTERN`] as the expression an identifier is searched for, compiled once
/// for the process.
///
/// Compiled UNANCHORED, which is what a `@pattern` constraint states: the expression has to be
/// found somewhere in the value rather than to describe the whole of it, and that is also how
/// a refusal quotes it.
///
/// `None` only if the expression itself could not be compiled, in which case nothing is
/// refused by it rather than everything.
fn stated_expression() -> Option<&'static ::regex::Regex> {
    static COMPILED: ::std::sync::OnceLock<Option<::regex::Regex>> =
        ::std::sync::OnceLock::new();
    COMPILED
        .get_or_init(|| ::regex::Regex::new(STACK_IDENTIFIER_PATTERN).ok())
        .as_ref()
}

/// Whether CloudFormation takes `identifier` as naming a stack at all: whether the expression
/// it states ([`STACK_IDENTIFIER_PATTERN`]) is FOUND in the value — a name run or an `arn:`
/// prefix anywhere in it. Such a value is RESOLVED, so one that names no stack is answered as
/// a missing stack; a value carrying neither is answered for its own shape.
fn is_an_addressable_form(identifier: &str) -> bool {
    stated_expression().is_none_or(|expression| expression.is_match(identifier))
}

/// The message CloudFormation answers for an identifier that breaks its constraints, stated
/// at `member_path`, or `None` for one it admits.
///
/// Both constraints are reported, the expression first — the expression is the shape of the
/// value and the length is a bound on it — so a caller sees everything it has to fix rather
/// than one finding per round trip. The value is quoted through the shared rendering
/// ([`::service_common::validation::quoted_echo`]), so a message is never sized by the
/// request, and the CHECK itself is made against the value as sent.
pub fn malformed_identifier(member_path: &str, identifier: &str) -> Option<String> {
    let clause = |kind: ViolationKind| Violation {
        path: member_path.to_string(),
        kind,
        value: Some(::service_common::validation::quoted_echo(identifier).into_owned()),
        sensitive: false,
    };
    let mut violations: Vec<Violation> = Vec::new();
    if !is_an_addressable_form(identifier) {
        violations.push(clause(ViolationKind::Pattern(
            STACK_IDENTIFIER_PATTERN.to_string(),
        )));
    }
    if identifier.chars().count() < STACK_IDENTIFIER_MIN_LENGTH {
        violations.push(clause(ViolationKind::MinLength(
            // The requirement is stated from the bound itself, so the sentence and the check
            // cannot name two different numbers.
            STACK_IDENTIFIER_MIN_LENGTH as i64,
        )));
    }
    ::aws_smithy_schema_server::envelope(&violations)
}

/// Refuse the call when the stack identifier a request carries breaks CloudFormation's
/// constraints for it, so a malformed identifier is never answered as a stack that is not
/// there.
///
/// Called at the TOP of a handler, before any lookup: that order is part of the contract —
/// an identifier no stack could carry is a malformed request, and a caller can only tell the
/// two apart if the constraint refusal comes first. `identifier` is `None` for an operation
/// whose member is conditional and the request did not carry it, which constrains nothing.
///
/// Generic over the operation's error enum, so every operation refuses it the same way: HTTP
/// 400 with the wire code `ValidationError`.
pub fn refuse_malformed_identifier<E>(
    member_path: &str,
    identifier: Option<&str>,
) -> Result<(), crate::scaffolded::Scaffolded<E>> {
    match identifier.and_then(|identifier| malformed_identifier(member_path, identifier)) {
        Some(message) => Err(crate::scaffolded::Scaffolded::validation_error(message)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the expression CloudFormation states for a stack identifier is SEARCHED for in
    /// the value, so a value carrying a name run or an `arn:` prefix anywhere in it is
    /// admitted and reaches the lookup — the bare unique id a stack id ends in included,
    /// whichever hexadecimal digit it begins with — and only a value carrying neither is
    /// refused for its own shape; a value breaking BOTH constraints is reported with both
    /// clauses, the expression first.
    #[test]
    fn an_identifier_is_admitted_by_the_services_own_expression() {
        for admitted in [
            "a",
            "a-stack",
            "Stack9",
            "arn:aws:cloudformation:us-east-1:123456789012:stack/a/0",
            "arn:aws-cn:cloudformation:cn-north-1:123456789012:stack/a/0",
            // Values the expression is FOUND in, though it does not describe the whole of
            // them: the unique id a stack id ends in, led by a digit and by a letter, in
            // either case of hexadecimal, and names carrying a character a stack name may
            // not. Each is resolved, so each is answered as the stack it does not name.
            "0e81d159-090a-7b61-9856-04d0999cb465",
            "0E81D159-090A-7B61-9856-04D0999CB465",
            "af2504e0-4f89-11d3-9a0c-0305e82c3301",
            "0e81d159090a7b61985604d0999cb465",
            "a_stack",
            "a.stack",
            "9stack",
            "-stack",
        ] {
            assert_eq!(
                malformed_identifier(STACK_NAME_PATH, admitted),
                None,
                "case {admitted:?}: expected the identifier to be admitted"
            );
        }
        for (case, refused) in [
            ("a value of digits alone", "1234567890"),
            ("a single digit", "9"),
            ("a value of hyphens alone", "----"),
            ("a value of digits and hyphens alone", "0123-4567"),
            ("a value of punctuation alone", "._+:/"),
            ("a value of one space", " "),
            ("digits either side of a space", "12 34"),
            ("a value of symbols and digits", "+1.0"),
        ] {
            let expected = Some(format!(
                "1 validation error detected: Value '{refused}' at 'stackName' failed to \
                 satisfy constraint: Member must satisfy regular expression pattern: \
                 {STACK_IDENTIFIER_PATTERN}"
            ));
            assert_eq!(
                malformed_identifier(STACK_NAME_PATH, refused),
                expected,
                "case {case:?}: expected the expression clause alone"
            );
        }
        let expected = Some(format!(
            "2 validation errors detected: Value '' at 'stackNameOrId' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: \
             {STACK_IDENTIFIER_PATTERN}; Value '' at 'stackNameOrId' failed to satisfy \
             constraint: Member must have length greater than or equal to \
             {STACK_IDENTIFIER_MIN_LENGTH}"
        ));
        assert_eq!(
            malformed_identifier(STACK_NAME_OR_ID_PATH, ""),
            expected,
            "case \"an empty identifier\": expected both clauses, the expression first"
        );
    }
}
