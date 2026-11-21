// SPDX-License-Identifier: CC0-1.0

//! What an expression's OPERANDS may be: the closed domains the functions and the
//! grammar hold a literal operand to.
//!
//! These are facts of the TEXT one request carries, so they are settled where the
//! expression is read rather than where an item is compared: an operand outside its
//! domain is the caller's mistake whatever the addressed table holds, and a request
//! carrying one is refused before anything is read. [`super`] re-exports them, so every
//! caller still names them `crate::state::…`.

use super::*;

/// Whether a bare word begins with a digit, which is what tells a numeric literal from
/// an attribute name written out in an expression ([`ExprParser::path_name`]).
pub(super) fn starts_with_digit(word: &str) -> bool {
    word.starts_with(|character: char| character.is_ascii_digit())
}

/// What ONE operand position of a function takes.
///
/// Every function of the expression languages declares each of its operands as one or the
/// other, and the declaration is read off the documented signature rather than guessed
/// from the operand: `attribute_exists (path)`, `begins_with (path, substr)`,
/// `contains (path, operand)`, `attribute_type (path, type)`, `size (path)`,
/// `if_not_exists (path, operand)` and `list_append (operand, operand)` (the
/// `ConditionExpression` and `UpdateExpression` function references).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperandKind {
    /// A document path: the location in the item the function reads.
    Path,
    /// A value the request carried: a `:placeholder` standing for one.
    Value,
    /// Either — a value, or a location the value is read from.
    Either,
}

/// The documented signature of one function the expression languages define, or `None`
/// for an identifier that names no function of theirs.
///
/// The ONE place the arity and the operand kinds of these functions are stated, so the
/// refusals a malformed call earns — the wrong NUMBER of operands
/// ([`incorrect_operand_count`]), a value where a path belongs
/// ([`requires_document_path`]) and a path where a value belongs
/// ([`incorrect_operand_type`]) — are decided by the signature and not by where a
/// recursive-descent parser happened to stop.
///
/// A position the references call an OPERAND takes either: a value the request carried, or
/// a path the value is read from. "contains (path, operand) ... The path and the operand
/// must be distinct; for example, contains (a, a) returns an error" and
/// "begins_with (path, substr)" read the second operand off the item exactly as the first
/// is read, so what it holds is known only once an item is read. The one position that
/// needs a literal is `attribute_type (path, type)`'s type code, which names a member of a
/// closed set ([`check_attribute_type_name`]).
///
/// `contains` is the function whose SEARCHED operand takes either as well: the collection it
/// searches may be one the REQUEST carried rather than one the item holds, which is how a
/// membership test over a list of expression-attribute values is written
/// (`contains(:candidates, #attribute)`), and AWS is observed to evaluate that call per item
/// instead of refusing it. Both positions are then read the same way, and what each holds is
/// known only once an item is read.
pub(super) fn function_signature(name: &str) -> Option<&'static [OperandKind]> {
    const PATH: &[OperandKind] = &[OperandKind::Path];
    const PATH_VALUE: &[OperandKind] = &[OperandKind::Path, OperandKind::Value];
    const PATH_EITHER: &[OperandKind] = &[OperandKind::Path, OperandKind::Either];
    const EITHER_EITHER: &[OperandKind] = &[OperandKind::Either, OperandKind::Either];
    match name {
        "attribute_exists" | "attribute_not_exists" | "size" => Some(PATH),
        "attribute_type" => Some(PATH_VALUE),
        "begins_with" | "if_not_exists" => Some(PATH_EITHER),
        "contains" | "list_append" => Some(EITHER_EITHER),
        _ => None,
    }
}

/// The functions a CONDITION may call, with their signatures: a closed set — "These
/// function names are case-sensitive" and the `ConditionExpression` doc lists exactly
/// these — so an identifier called like a function that is not one of them names no
/// function of this grammar ([`unknown_function`]).
pub(super) fn condition_function_signature(name: &str) -> Option<&'static [OperandKind]> {
    match name {
        "attribute_exists" | "attribute_not_exists" | "attribute_type" | "begins_with"
        | "contains" => function_signature(name),
        _ => None,
    }
}

/// The function that reports the LENGTH of what a path addresses, which is an operand of
/// a comparison rather than a condition of its own.
pub(super) const SIZE_FUNCTION: &str = "size";

/// The range comparator as an expression writes it, which is what its refusals name.
pub(super) const BETWEEN_OPERATOR: &str = "BETWEEN";

/// How a refusal names an operand that reads a LOCATION where the signature declares a
/// value: the kind of operand it is, which is all the request states about it — what the
/// location holds is known only once an item is read.
const PATH_OPERAND_TYPE: &str = "PATH";

/// Hold one operand to the kind its position declares, where the REQUEST settles it.
///
/// A `:placeholder` written where a path belongs names no location at all, and a path
/// written where a value belongs carries none: both are facts of the expression's text, so
/// both are refused as the expression is read rather than evaluated against an item.
pub(super) fn check_operand_kind(
    member: &str,
    function: &str,
    kind: OperandKind,
    operand: &Operand,
) -> Result<(), RequestError> {
    match (kind, operand) {
        (OperandKind::Value, Operand::Path(_) | Operand::Size(_)) => Err(incorrect_operand_type(
            member,
            function,
            PATH_OPERAND_TYPE,
        )),
        (OperandKind::Path, Operand::Value(_)) => Err(requires_document_path(member, function)),
        _ => Ok(()),
    }
}

/// Hold `contains` to the one rule the reference states about its operands beyond the kind
/// each position takes: "contains (path, operand) ... The path and the operand must be
/// distinct; for example, contains (a, a) returns an error" (the `ConditionExpression`
/// function reference).
///
/// A call whose operand reads the very location the first operand searches asks whether a
/// value holds itself, which selects every item carrying the attribute whatever it holds,
/// so the doc makes it the caller's mistake rather than a condition. AWS states the rule
/// over the OPERATOR and the first operand, rendered as the token list its refusals print a
/// document path as ([`DocumentPath::render_tokens`]). Two DIFFERENT locations are compared,
/// so that call is read against the item like any other.
///
/// The rule is about two operands naming ONE LOCATION, so it is asked only where both of
/// them name one: a call searching a collection the request carried has no path to be
/// distinct from, and the values it compares are the request's own rather than the same
/// attribute read twice.
pub(super) fn check_contains_operands(
    member: &str,
    searched: &Operand,
    operand: &Operand,
) -> Result<(), RequestError> {
    let Operand::Path(path) = searched else {
        return Ok(());
    };
    if matches!(operand, Operand::Path(other) if other == path) {
        return Err(invalid_expression_member(
            member,
            &format!(
                "The first operand must be distinct from the remaining operands for this \
                 operator or function; operator: contains, first operand: {}",
                path.render_tokens()
            ),
        ));
    }
    Ok(())
}

/// Hold one operand of an ORDERING comparison to the types DynamoDB orders, where the
/// request stated that operand as a literal.
///
/// "For type Number, value comparisons are numeric. String value comparisons ... are
/// based on ASCII character code values. For Binary, DynamoDB treats each byte ... as
/// unsigned when it compares binary values" (the `AttributeValueList` doc) — and nothing
/// for the other types, so a value of any of them cannot satisfy `<`, `<=`, `>`, `>=` or
/// `BETWEEN` whatever the addressed item holds. That makes it the caller's mistake and not
/// an unmet condition, refused when the expression is read; an operand that READS an
/// attribute is left to the evaluation, which is the only place its type is known.
pub(super) fn check_orderable_operand(
    member: &str,
    operator: &str,
    operand: &Operand,
) -> Result<(), RequestError> {
    let Operand::Value(value) = operand else {
        return Ok(());
    };
    if matches!(
        value,
        AttributeValue::S(_) | AttributeValue::N(_) | AttributeValue::B(_)
    ) {
        return Ok(());
    }
    Err(incorrect_operand_type(
        member,
        operator,
        attribute_value_type(value),
    ))
}

/// The two bounds of a `BETWEEN` the request stated as LITERALS, held to the order the
/// comparator requires ([`check_between_bounds`]).
pub(super) fn check_between_operands(
    member: &str,
    low: &Operand,
    high: &Operand,
) -> Result<(), RequestError> {
    let (Operand::Value(low), Operand::Value(high)) = (low, high) else {
        return Ok(());
    };
    check_between_bounds(member, low, high)
}

/// `begins_with`'s prefix operand is a PREFIX, so it is a string or binary value, and a
/// literal of any other type is refused when the expression is read.
///
/// "begins_with (path, substr) — true if the attribute specified by path begins with a
/// particular substring" (the `ConditionExpression` doc): only `S` and `B` have prefixes
/// to compare, so a number or a collection cannot be one, and the operand is a literal
/// the request already carried — the expression is refused whether or not the addressed
/// table holds an item to evaluate it against. An operand that reads an attribute is
/// left to the evaluation, which is the only place its type is known.
pub(super) fn check_begins_with_prefix(
    member: &str,
    operand: &Operand,
) -> Result<(), RequestError> {
    let Operand::Value(value) = operand else {
        return Ok(());
    };
    if matches!(value, AttributeValue::S(_) | AttributeValue::B(_)) {
        return Ok(());
    }
    Err(incorrect_operand_type(
        member,
        "begins_with",
        attribute_value_type(value),
    ))
}

/// The type codes a type test may name, in the order AWS's own refusal lists them.
///
/// Source: this service's Smithy model, the members of the `AttributeValue` union
/// (`smithy_query(query="shape",shape="AttributeValue")`) — the codes a stored value can
/// carry are exactly the codes a test of its type can name.
pub const ATTRIBUTE_TYPE_NAMES: [&str; 10] =
    ["S", "SS", "N", "NS", "B", "BS", "BOOL", "NULL", "L", "M"];

/// `attribute_type`'s second operand names a TYPE, so it is held to that closed set as
/// the expression is READ rather than as an item is compared.
///
/// The operand is a literal the request already carried, and a type test naming no type
/// can select nothing, so the request is refused whether or not the addressed table
/// holds an item to evaluate it against — a token that is no type code is the caller's
/// mistake, not an empty result. A code the set does not hold is named back as the
/// request spelled it, beside the whole set; a literal that is not a string at all is
/// the operand-type refusal, since the function takes a type NAME. An operand that
/// reads an attribute instead of naming a literal is left to the evaluation, which is
/// the only place its type is known.
pub(super) fn check_attribute_type_name(
    member: &str,
    operand: &Operand,
) -> Result<(), RequestError> {
    let Operand::Value(value) = operand else {
        return Ok(());
    };
    let AttributeValue::S(name) = value else {
        return Err(incorrect_operand_type(
            member,
            "attribute_type",
            attribute_value_type(value),
        ));
    };
    if ATTRIBUTE_TYPE_NAMES.contains(&name.as_str()) {
        return Ok(());
    }
    Err(invalid_expression_member(
        member,
        &format!(
            "Invalid attribute type name found; type: {name}, valid types: {{{}}}",
            ATTRIBUTE_TYPE_NAMES.join(",")
        ),
    ))
}
