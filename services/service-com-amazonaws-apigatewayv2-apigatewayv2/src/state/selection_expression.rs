// SPDX-License-Identifier: CC0-1.0

//! Selection expressions: the `$`-variable expressions this service's members carry, and
//! what each member's protocol lets one select.
//!
//! A module of its own under [`crate::state`], because this is one concern several
//! members share — `Api$RouteSelectionExpression`, `Api$ApiKeySelectionExpression`, a
//! route response's and an integration response's selection expressions — and every
//! writer of one has to hold it to the same rule, or two operations would store
//! expressions the same reader cannot evaluate alike. [`crate::state`] re-exports every
//! public item here, so a handler reaches them the way it reaches the rest of this
//! service's state helpers.

use super::WEBSOCKET_PROTOCOL_TYPE;

/// The variable namespaces a selection expression may read, lowercased for comparison.
///
/// A selection expression selects a stored key by evaluating request DATA — the
/// documented defaults name exactly these namespaces (`$request.header.x-api-key` for
/// `Api$ApiKeySelectionExpression`, `$request.method $request.path` for an HTTP api's
/// `RouteSelectionExpression`, `$context.*` for context variables) — so a `$`-token
/// naming anything else names no variable at all.
pub const SELECTION_EXPRESSION_VARIABLES: &[&str] = &["request", "context", "stagevariables"];

/// The sentence API Gateway refuses a `RouteSelectionExpression` that is no selector with:
/// the one a WEBSOCKET api's request left out, and one sent with nothing in it.
///
/// Fixed text naming the MEMBER, nothing from the request: it is the whole phrase the
/// service answers, with no requirement restated after it and no member of the model's own
/// casing — the member is named as the wire spells it
/// (`CreateApiRequest$RouteSelectionExpression` carries `jsonName`
/// `routeSelectionExpression`). An expression that IS a selector but names a variable the
/// service does not evaluate takes [`unexpected_selection_variable_message`] instead.
pub const INVALID_ROUTE_SELECTION_EXPRESSION: &str = "Invalid routeSelectionExpression";

/// The sentence this service refuses a selection expression that names a variable it
/// does not evaluate with: fixed text, then the variable reference exactly as the
/// request wrote it (`$` included).
///
/// One wording for every member that carries a selection expression, so the refusal a
/// caller reads does not depend on which member it set.
pub fn unexpected_selection_variable_message(reference: &str) -> String {
    format!("Unexpected variable in selection expression: {reference}")
}

/// Every VARIABLE REFERENCE `expression` makes, in the order it makes them: the text
/// after each unescaped `$`, with the braces of the `${…}` form removed.
///
/// In a selection expression an unescaped `$` begins a variable reference, and a
/// reference is a dotted path into one of the namespaces the service evaluates
/// (`$request.body.action`, `${request.method}`). A `\` escapes the next character, which
/// is how a selection KEY that itself begins with `$` is written (`\$default`), so an
/// escaped `$` begins no reference. The reference is returned AS WRITTEN — neither
/// lowercased nor trimmed — because it is what a refusal echoes back.
pub fn selection_expression_references(expression: &str) -> Vec<String> {
    pieces(expression)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Reference(reference) => Some(reference),
            Piece::Literal(_) => None,
        })
        .collect()
}

/// `expression` EVALUATED: its literal text, with every variable reference it makes replaced
/// by the value `value` gives that reference.
///
/// The same reading of the grammar as [`selection_expression_references`] — one scanner
/// ([`pieces`]) — so what a refusal echoes back and what the data plane evaluates cannot
/// disagree about where a reference begins and ends. An escaped character is a literal, which
/// is what makes `\$default` evaluate to the key `$default`.
///
/// `None` when a reference has no value for this call: the expression then selects nothing
/// rather than a key spelled with a hole in it. `None` as well for an expression making more
/// than [`MAX_EVALUATED_REFERENCES`] references, which is a bound on the work one call does
/// and not a rule about the grammar.
pub fn evaluated_selection_expression(
    expression: &str,
    value: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut evaluated = String::new();
    let mut resolved = 0_usize;
    for piece in pieces(expression) {
        match piece {
            Piece::Literal(text) => evaluated.push_str(&text),
            Piece::Reference(reference) => {
                resolved += 1;
                if resolved > MAX_EVALUATED_REFERENCES {
                    return None;
                }
                evaluated.push_str(&value(&reference)?);
            }
        }
    }
    Some(evaluated)
}

/// How many references one evaluation resolves.
///
/// A selection expression names a KEY — the documented ones name one part of the request
/// (`$request.body.action`) — while its own length is not bounded by the member's shape, so
/// the bound keeps the work ONE call does independent of how long an expression was stored:
/// an expression past it selects nothing instead of being evaluated.
const MAX_EVALUATED_REFERENCES: usize = 32;

/// One piece of a selection expression: literal text, or a variable reference.
enum Piece {
    Literal(String),
    Reference(String),
}

/// `expression` split into the pieces the grammar makes of it, in order; see
/// [`selection_expression_references`] for the grammar this reads.
///
/// The ONE scanner of a selection expression, so every reader of one — the refusal that
/// echoes a reference back, the route a message selects, the request template a WebSocket
/// call selects — reads the same text the same way.
fn pieces(expression: &str) -> Vec<Piece> {
    let characters: Vec<char> = expression.chars().collect();
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            // An escaped character is a literal: this is how a selection key beginning
            // with `$` is written.
            '\\' => {
                if let Some(escaped) = characters.get(index + 1) {
                    literal.push(*escaped);
                }
                index += 2;
            }
            '$' => {
                index += 1;
                let braced = characters.get(index) == Some(&'{');
                if braced {
                    index += 1;
                }
                let reference: String = characters[index..]
                    .iter()
                    .take_while(|c| {
                        if braced {
                            **c != '}'
                        } else {
                            c.is_ascii_alphanumeric() || **c == '_' || **c == '.' || **c == '-'
                        }
                    })
                    .collect();
                index += reference.chars().count() + usize::from(braced);
                if !literal.is_empty() {
                    pieces.push(Piece::Literal(std::mem::take(&mut literal)));
                }
                pieces.push(Piece::Reference(reference));
            }
            character => {
                literal.push(character);
                index += 1;
            }
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Literal(literal));
    }
    pieces
}

/// WHICH namespace a variable reference names, with the name it names inside it.
///
/// The one reading of the namespaces a selection expression may read
/// ([`SELECTION_EXPRESSION_VARIABLES`]): the namespace is compared case-insensitively, the
/// way every writer of a selection expression compares it, and the name is read exactly as
/// written, because a JSON member name, a context member and a stage variable are all
/// case-sensitive keys. `None` for a reference naming no namespace this service evaluates,
/// and for one naming a namespace with nothing inside it.
pub fn selection_reference(reference: &str) -> Option<(SelectionNamespace, &str)> {
    for (prefix, namespace) in [
        (
            WEBSOCKET_ROUTE_SELECTION_REFERENCE_PREFIX,
            SelectionNamespace::RequestBody,
        ),
        (CONTEXT_REFERENCE_PREFIX, SelectionNamespace::Context),
        (
            STAGE_VARIABLE_REFERENCE_PREFIX,
            SelectionNamespace::StageVariables,
        ),
    ] {
        // Read with `str::get` rather than a slice expression, and FALL THROUGH on a
        // reference shorter than this prefix (or one whose prefix length falls inside a
        // multi-byte character): such a reference may still name one of the prefixes below,
        // which is what `$context.id` does.
        let Some(head) = reference.get(..prefix.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(prefix) {
            continue;
        }
        return reference
            .get(prefix.len()..)
            .filter(|name| !name.is_empty())
            .map(|name| (namespace, name));
    }
    None
}

/// The part of a call a variable reference names ([`selection_reference`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionNamespace {
    /// A FIELD of the request body, which is what a WebSocket api's expressions read
    /// ([`WEBSOCKET_ROUTE_SELECTION_REFERENCE_PREFIX`]).
    RequestBody,
    /// A `$context` member the gateway reports about the call.
    Context,
    /// A variable of the serving stage.
    StageVariables,
}

/// The two namespaces beside the message body a selection expression may read, lowercased
/// for comparison — the `context` and `stagevariables` of
/// [`SELECTION_EXPRESSION_VARIABLES`], as a reference spells the start of a path into each.
const CONTEXT_REFERENCE_PREFIX: &str = "context.";
const STAGE_VARIABLE_REFERENCE_PREFIX: &str = "stagevariables.";

/// `Ok(())`, or why this selection expression cannot be stored — the message for the
/// operation's `BadRequestException`.
///
/// One rule: every variable reference the expression makes
/// ([`selection_expression_references`]) has to name one of the variable namespaces the
/// service evaluates ([`SELECTION_EXPRESSION_VARIABLES`]). A selection KEY that itself
/// begins with `$` — `$default` is the one the service mints — is a literal, and a
/// literal is written escaped (`\$default`); unescaped it reads as a variable the service
/// does not define. The refusal names the namespace that was not recognised, which is the
/// part of the reference that is wrong.
pub fn validate_selection_expression(expression: &str) -> Result<(), String> {
    for reference in selection_expression_references(expression) {
        let namespace = reference.split('.').next().unwrap_or_default();
        if !SELECTION_EXPRESSION_VARIABLES.contains(&namespace.to_ascii_lowercase().as_str()) {
            return Err(unexpected_selection_variable_message(&format!(
                "${namespace}"
            )));
        }
    }
    Ok(())
}

/// The variable reference a WEBSOCKET api's `RouteSelectionExpression` selects a route
/// key with, lowercased for comparison: a FIELD of the request body.
///
/// A WebSocket api matches each inbound message against its routes by evaluating this
/// expression over that message — `$request.body.action` is the expression AWS documents
/// for a message carrying an `action` property — so the reference has to name a field
/// INSIDE the body. The whole body (`$request.body`, no field selector) selects no key
/// a route could carry, and neither does a part of the request a frame does not have.
pub const WEBSOCKET_ROUTE_SELECTION_REFERENCE_PREFIX: &str = "request.body.";

/// `Ok(())`, or why this `RouteSelectionExpression` cannot be stored on an api of this
/// `ProtocolType` — the message for the operation's `BadRequestException`.
///
/// Two rules, in order: the expression names only namespaces the service evaluates
/// ([`validate_selection_expression`]), and — on a WEBSOCKET api, whose route keys are
/// matched against the message body — every reference it makes names a field of that body
/// ([`WEBSOCKET_ROUTE_SELECTION_REFERENCE_PREFIX`]). A reference that names anything
/// else, the whole body included, selects no route key, so the api it was aimed at would
/// stop routing messages altogether; it is refused and nothing is written. The refusal
/// echoes the reference as the request wrote it.
///
/// An api of the other protocol is held to the namespace rule alone: "For HTTP APIs, the
/// routeSelectionExpression must be ${request.method} ${request.path}. If not provided,
/// this will be the default for HTTP APIs" (`CreateApi$RouteSelectionExpression`), which
/// is the value this service gives such an api when the request names none
/// (`crate::state::HTTP_ROUTE_SELECTION_EXPRESSION`).
///
/// Shared by every writer of an api's route selection expression, so the create and the
/// patch cannot hold one to a different rule; a patch weighs the expression the api would
/// CARRY.
pub fn validate_route_selection_expression(
    protocol_type: &str,
    expression: &str,
) -> Result<(), String> {
    validate_selection_expression(expression)?;
    if protocol_type != WEBSOCKET_PROTOCOL_TYPE {
        return Ok(());
    }
    for reference in selection_expression_references(expression) {
        let selects_a_body_field = reference
            .to_ascii_lowercase()
            .strip_prefix(WEBSOCKET_ROUTE_SELECTION_REFERENCE_PREFIX)
            .is_some_and(|field| !field.is_empty());
        if !selects_a_body_field {
            return Err(unexpected_selection_variable_message(&format!(
                "${reference}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_names_the_namespace_it_opens_with_and_the_name_inside_it() {
        // Rows span the three namespaces this service evaluates, the casing a reference may
        // be written in, references SHORTER than the longest prefix (which must still be
        // read), a namespace with nothing inside it, and a reference naming no namespace.
        let cases: &[(&str, Option<(SelectionNamespace, &str)>)] = &[
            (
                "request.body.action",
                Some((SelectionNamespace::RequestBody, "action")),
            ),
            (
                "Request.Body.action",
                Some((SelectionNamespace::RequestBody, "action")),
            ),
            ("context.id", Some((SelectionNamespace::Context, "id"))),
            (
                "context.identity.sourceIp",
                Some((SelectionNamespace::Context, "identity.sourceIp")),
            ),
            (
                "stageVariables.x",
                Some((SelectionNamespace::StageVariables, "x")),
            ),
            // Nothing inside the namespace, and namespaces this service does not evaluate.
            ("request.body.", None),
            ("context.", None),
            ("request.header.token", None),
            ("nothing.at.all", None),
            ("", None),
        ];
        for (reference, expected) in cases {
            let read = selection_reference(reference);
            assert_eq!(
                read, *expected,
                "case {reference:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    #[test]
    fn an_expression_evaluates_its_literals_and_the_references_it_has_values_for() {
        let value = |reference: &str| match reference {
            "request.body.action" => Some("send".to_string()),
            "context.stage" => Some("prod".to_string()),
            _ => None,
        };
        // Rows span what the grammar makes of an expression: an escaped literal key, each
        // spelling of a reference, a reference beside literal text, several references, and a
        // reference this call has no value for.
        let cases: &[(&str, Option<&str>)] = &[
            (r"\$default", Some("$default")),
            ("$request.body.action", Some("send")),
            ("${request.body.action}", Some("send")),
            ("route-$context.stage", Some("route-prod")),
            ("$context.stage/$request.body.action", Some("prod/send")),
            ("$request.body.absent", None),
            ("plain", Some("plain")),
        ];
        for (expression, expected) in cases {
            let evaluated = evaluated_selection_expression(expression, &value);
            assert_eq!(
                evaluated.as_deref(),
                *expected,
                "case {expression:?}: expected {expected:?}, got {evaluated:?}"
            );
        }
        // And the bound on the work one evaluation does: an expression at it evaluates, one
        // past it selects nothing.
        let many = "$context.stage".repeat(MAX_EVALUATED_REFERENCES);
        let too_many = format!("{many}$context.stage");
        assert!(
            evaluated_selection_expression(&many, &value).is_some(),
            "case {:?}: expected an expression at the bound to evaluate",
            "as many references as one evaluation resolves"
        );
        assert_eq!(
            evaluated_selection_expression(&too_many, &value),
            None,
            "case {:?}: expected one past the bound to evaluate to nothing",
            "one reference past the bound"
        );
    }
}
