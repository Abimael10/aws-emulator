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
//! what it TAKES: a stack NAME, or a stack ARN. A constraint expression is satisfied by the
//! WHOLE of the value — the requirement is that the value IS one of the forms the expression
//! describes, not that one of them can be found inside it — so a value only part of which is
//! a name run or an ARN is refused for its own shape ahead of the lookup, whatever else it
//! carries. That is why the value quoted in the refusal is the whole value as sent, while the
//! expression is quoted unanchored, as the service states it.
//!
//! An admitted value is RESOLVED, and one that names no stack is answered as the missing stack
//! it is ([`super::stack_does_not_exist`] and the forms beside it). The bare unique id a stack
//! id (`arn:…:stack/<name>/<unique-id>`) ENDS in addresses no stack, and it reaches the lookup
//! all the same: it is a value CloudFormation itself MINTED for the stack, so it is one of the
//! forms the service takes as addressing one ([`is_a_minted_unique_id`]). Which character a
//! hexadecimal id happens to begin with is no part of the decision — one id of a kind is every
//! id of that kind — so both a letter-led and a digit-led unique id are answered as the stack
//! they do not name.
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
/// values taken as addressing a stack ([`is_an_addressable_form`]) — the one expression, read
/// the one way: quoted unanchored, and satisfied by the whole of the value.
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

/// [`STACK_IDENTIFIER_PATTERN`] as the expression an identifier is held to, compiled once for
/// the process.
///
/// Compiled to describe the WHOLE value: a constraint expression is satisfied by the value
/// itself rather than by some run inside it, so the alternation is wrapped and anchored at
/// both ends here while the refusal keeps quoting the expression as the service states it.
///
/// `None` only if the expression itself could not be compiled, in which case nothing is
/// refused by it rather than everything.
fn stated_expression() -> Option<&'static ::regex::Regex> {
    static COMPILED: ::std::sync::OnceLock<Option<::regex::Regex>> =
        ::std::sync::OnceLock::new();
    COMPILED
        .get_or_init(|| {
            ::regex::Regex::new(&format!("^(?:{STACK_IDENTIFIER_PATTERN})$")).ok()
        })
        .as_ref()
}

/// Whether CloudFormation takes `identifier` as naming a stack at all: whether the WHOLE value
/// is one of the forms the expression it states ([`STACK_IDENTIFIER_PATTERN`]) describes — a
/// stack name (a letter followed by letters, digits and hyphens) or a stack ARN — or the bare
/// unique id CloudFormation mints for a stack ([`is_a_minted_unique_id`]). Such a value is
/// RESOLVED, so one that names no stack is answered as a missing stack; a value of any other
/// form is answered for its own shape.
fn is_an_addressable_form(identifier: &str) -> bool {
    stated_expression().is_none_or(|expression| expression.is_match(identifier))
        || is_a_minted_unique_id(identifier)
}

/// How many hexadecimal characters the unique id CloudFormation mints for a stack carries
/// ([`super::identifiers`] mints exactly these, grouped 8-4-4-4-12).
const UNIQUE_ID_HEX_DIGITS: usize = 32;

/// Where a hyphen falls in the grouped spelling of a minted unique id: after the 8th, 12th,
/// 16th and 20th hexadecimal character.
const UNIQUE_ID_GROUP_BOUNDARIES: [usize; 4] = [8, 12, 16, 20];

/// Whether `identifier` is the bare unique id CloudFormation mints for a stack: the 128 bits of
/// that id written as [`UNIQUE_ID_HEX_DIGITS`] hexadecimal characters, in either case, grouped
/// 8-4-4-4-12 by hyphens or ungrouped.
///
/// This is a FORM the service takes as addressing a stack beside the name and the ARN, because
/// it is a value the service itself mints and hands back inside a stack id. The hexadecimal
/// alphabet spans both digits and letters, so which of the two an id starts with says nothing
/// about the KIND of value it is: a decision that read the first character would admit some of
/// the ids this service mints and refuse the rest.
fn is_a_minted_unique_id(identifier: &str) -> bool {
    let grouped = UNIQUE_ID_GROUP_BOUNDARIES
        .iter()
        .enumerate()
        .map(|(seen, boundary)| boundary + seen)
        .collect::<Vec<usize>>();
    let mut digits = 0usize;
    for (position, character) in identifier.chars().enumerate() {
        if character == '-' {
            if !grouped.contains(&position) {
                return false;
            }
            continue;
        }
        if !character.is_ascii_hexdigit() {
            return false;
        }
        digits += 1;
    }
    let hyphens = identifier.chars().filter(|char| *char == '-').count();
    digits == UNIQUE_ID_HEX_DIGITS && (hyphens == 0 || hyphens == grouped.len())
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

    /// RULE: the expression CloudFormation states for a stack identifier describes the WHOLE
    /// value, so a value that IS a stack name or a stack ARN is admitted and reaches the
    /// lookup — as does the bare unique id this service mints for a stack, whichever
    /// hexadecimal character it begins with — and a value of any other form is refused for
    /// its own shape; a value breaking BOTH constraints is reported with both clauses, the
    /// expression first.
    #[test]
    fn an_identifier_is_admitted_by_the_services_own_expression() {
        for admitted in [
            "a",
            "a-stack",
            "Stack9",
            "arn:aws:cloudformation:us-east-1:123456789012:stack/a/0",
            "arn:aws-cn:cloudformation:cn-north-1:123456789012:stack/a/0",
            // The unique id a stack id ends in, though no stack carries them: a value this
            // service MINTS, grouped and ungrouped, in either case and whichever hexadecimal
            // character it opens with. Each is resolved, so each is answered as the stack it
            // does not name.
            "af2504e0-4f89-11d3-9a0c-0305e82c3301",
            "AF2504E0-4F89-11D3-9A0C-0305E82C3301",
            // A value the whole of which is a name run, though no stack carries it.
            "ae81d159090a7b61985604d0999cb465",
            "0e81d159-090a-7b61-9856-04d0999cb465",
            "0E81D159-090A-7B61-9856-04D0999CB465",
            "0e81d159090a7b61985604d0999cb465",
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
            // Values only PART of which is a name run or an ARN: a character a stack name
            // may not carry, or a leading one it may not begin with, is refused however much
            // of the rest would pass on its own.
            ("a name carrying an underscore", "a_stack"),
            ("a name carrying a dot", "a.stack"),
            ("a name led by a digit", "9stack"),
            ("a name led by a hyphen", "-stack"),
            (
                "a hexadecimal run narrower than a minted unique id",
                "0e81d159-090a-7b61-9856-04d0999cb46",
            ),
            (
                "a hexadecimal run wider than a minted unique id",
                "0e81d159-090a-7b61-9856-04d0999cb4650",
            ),
            (
                "a minted unique id's digits grouped somewhere else",
                "0e81d1-59090a-7b61-9856-04d0999cb465",
            ),
            ("an ARN of a partition no stack carries", "arn aws:x"),
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
