// SPDX-License-Identifier: CC0-1.0

//! What ONE rule statement has to satisfy on its own, wherever in a rule list it
//! sits: where a statement kind may be written, which values an enumerated setting
//! may take, and which request components a setting of a statement is defined for.
//!
//! Each of these is a judgement about a single statement rather than about the list
//! that holds it ([`rule_set`](super::rule_set) holds the whole-list ones), and none
//! of them reads the store — so one walk serves every operation that takes a rule
//! list, whether it SAVES the list or only PRICES it: a rule list WAF would refuse to
//! store is not a rule list it will quote a capacity for either.
//!
//! They are all refusals of the modelled `WAFInvalidParameterException` (client,
//! HTTP 400), whose `Field` names the settings at fault, `Parameter` the member or the
//! value the request wrote and `Reason` what WAF makes of it
//! ([`invalid_parameter_exception`]).

use super::*;
use crate::ops::types::PreParseTextTransformation;

/// The `ParameterExceptionField` value naming a `RateBasedStatement`
/// (`RATE_BASED_STATEMENT`, as
/// `smithy_query(query="shape",shape="ParameterExceptionField")` spells it). See
/// [`FIELD_RESOURCE_ARN`].
pub const FIELD_RATE_BASED_STATEMENT: &str = "RATE_BASED_STATEMENT";

/// The `ParameterExceptionField` value naming a `PreParseTextTransformation`
/// (`PRE_PARSE_TEXT_TRANSFORMATION`). See [`FIELD_RESOURCE_ARN`].
pub const FIELD_PRE_PARSE_TEXT_TRANSFORMATION: &str = "PRE_PARSE_TEXT_TRANSFORMATION";

/// The shape name a refusal about a pre-parse text transformation carries in
/// `Parameter`: the setting at fault, as the model names it.
pub const PARAMETER_PRE_PARSE_TEXT_TRANSFORMATION: &str = "PreParseTextTransformation";

/// The `ErrorReason` WAF carries when a statement is written somewhere the kind of
/// statement it is may not appear, as the recorded WAF responses word it.
///
/// One sentence for the whole class — the `Field` member names the kind of statement
/// that was misplaced, so the reason does not have to. It is the reason
/// `WAFInvalidParameterException`'s own documentation describes this case with
/// ("Your nested statement isn't valid. You might have tried to nest a statement
/// that can't be nested").
pub const STATEMENT_REFERENCE_NOT_VALID_REASON: &str =
    "A reference in your rule statement is not valid.";

/// The `ErrorReason` WAF carries when a setting's value is outside the set of values
/// that setting accepts, as the recorded WAF responses word it — WAF's own wording,
/// grammatical slip included.
///
/// One sentence for the whole class: the `Field` member names the settings the
/// rejected value was written in and `Parameter` carries the value itself, so the
/// reason does not have to name either.
pub const PARAMETER_NOT_VALID_REASON: &str = "Your request in not valid.";

/// The `ErrorReason` WAF carries when a statement applies pre-parse text
/// transformations to a request component they are not defined for: the rule the
/// request broke, in the words WAF states it in.
pub const PRE_PARSE_UNSUPPORTED_FIELD_REASON: &str =
    "PreParseTextTransformations is only supported for SingleQueryArgument and \
     AllQueryArguments field to match types.";

/// Every evaluation window a rate-based statement may look back over, in seconds.
///
/// `RateBasedStatement$EvaluationWindowSec` is "the amount of time, in seconds, that
/// WAF should include in its request counts, looking back from the current time",
/// and the member's own doc enumerates what it accepts: "Valid settings are 60, 120,
/// 300, and 600." The member carries no `@range` narrowing the numeric domain, so
/// this is an enumerated service rule rather than a bound: a value inside the domain
/// but outside this set is refused ([`statement_settings_refusal`]).
///
/// The member's modelled zero-value default is `0`, which is not one of these, so a
/// request that omits it is not judged against them and gets the window its doc
/// publishes instead ([`DEFAULT_EVALUATION_WINDOW_SEC`]).
pub const VALID_EVALUATION_WINDOWS_SEC: &[i64] = &[60, 120, 300, 600];

/// The statements of this model that carry `PreParseTextTransformations`, as the
/// `FieldToMatch` each one inspects paired with the transformations it applies to the
/// raw query string before that component is parsed.
///
/// Every statement that inspects a request component carries the member, and the
/// model states the SAME restriction on all of them (the one
/// `PreParseTextTransformation` documents), so one accessor covers every statement
/// kind the judgement below has to reach.
pub fn pre_parse_inspections(
    statement: &Statement,
) -> Vec<(&FieldToMatch, &[PreParseTextTransformation])> {
    // One arm per statement kind that carries the member: the component it inspects
    // and the pre-parse transformations it applies to the raw query string first. A
    // statement kind the model grows the member on belongs here, so the judgement and
    // the charge both reach it.
    [
        statement.byte_match_statement.as_ref().map(|statement| {
            (
                &statement.field_to_match,
                statement.pre_parse_text_transformations.as_ref(),
            )
        }),
        statement.sqli_match_statement.as_ref().map(|statement| {
            (
                &statement.field_to_match,
                statement.pre_parse_text_transformations.as_ref(),
            )
        }),
        statement.xss_match_statement.as_ref().map(|statement| {
            (
                &statement.field_to_match,
                statement.pre_parse_text_transformations.as_ref(),
            )
        }),
        statement.size_constraint_statement.as_ref().map(|statement| {
            (
                &statement.field_to_match,
                statement.pre_parse_text_transformations.as_ref(),
            )
        }),
        statement.regex_match_statement.as_ref().map(|statement| {
            (
                &statement.field_to_match,
                statement.pre_parse_text_transformations.as_ref(),
            )
        }),
        statement
            .regex_pattern_set_reference_statement
            .as_ref()
            .map(|statement| {
                (
                    &statement.field_to_match,
                    statement.pre_parse_text_transformations.as_ref(),
                )
            }),
    ]
    .into_iter()
    .flatten()
    .filter_map(|(field_to_match, pre_parse)| {
        pre_parse.map(|transformations| (field_to_match, transformations.as_slice()))
    })
    .collect()
}

/// Whether `field_to_match` is a request component pre-parse text transformations are
/// defined for.
///
/// A pre-parse text transformation normalizes "the raw query string before WAF parses
/// it into individual query arguments", so it is defined only for the components that
/// ARE parsed out of that query string: "pre-parse text transformations are only
/// supported when `FieldToMatch` is `SingleQueryArgument` or `AllQueryArguments`"
/// (`smithy_query(query="shape",shape="PreParseTextTransformation")`). Any other
/// component is reached without parsing the query string, so there is nothing for the
/// transformation to normalize.
pub fn supports_pre_parse_text_transformations(field_to_match: &FieldToMatch) -> bool {
    field_to_match.single_query_argument.is_some() || field_to_match.all_query_arguments.is_some()
}

/// The refusal one STATEMENT of `rules` earns from its own settings, or `None` when
/// every statement in the list is one WAF accepts.
///
/// Run by every operation that takes a rule list — the four that save one and the one
/// that prices one — before anything is written or quoted, so a refused call stores
/// nothing and quotes nothing:
///
/// * a `RateBasedStatement` written anywhere other than as a rule's own top-level
///   `Statement`. The nestable statements are the ones the model says can be nested
///   ("you can use any statement that can be nested", `NotStatement$Statement` and
///   the two logical lists), and a rate-based statement is not one of them: it counts
///   the requests of a whole rule over time, which has no meaning as one operand of a
///   match. The `Field` names the statement kind and the `Parameter` the member it
///   was written as.
/// * a `RateBasedStatement$EvaluationWindowSec` outside the four windows the member
///   enumerates ([`VALID_EVALUATION_WINDOWS_SEC`]), with the rejected value itself as
///   the `Parameter`. The member's modelled zero-value default stands for an absent
///   member, which asks for the window the member documents instead
///   ([`DEFAULT_EVALUATION_WINDOW_SEC`]) and so is not judged against the set.
/// * a statement applying `PreParseTextTransformations` to a `FieldToMatch` they are
///   not defined for ([`supports_pre_parse_text_transformations`]). The member carries
///   no `@length(min 1)`, so a list that is present and EMPTY is well formed and
///   applies no transformation to anything, which is accepted.
///
/// Walked with an explicit worklist, like every other walk over a caller-supplied
/// rule tree ([`validate_statement_nesting`]), and one rule at a time, so the refusal
/// is the first offending rule's in the order the request wrote them.
pub fn statement_settings_refusal(rules: &[Rule]) -> Option<WAFInvalidParameterException> {
    for rule in rules {
        let mut pending: Vec<(&Statement, bool)> = vec![(&rule.statement, true)];
        while let Some((statement, top_level)) = pending.pop() {
            if let Some(rate_based) = statement.rate_based_statement.as_deref() {
                if !top_level {
                    return Some(invalid_parameter_exception(
                        FIELD_RATE_BASED_STATEMENT,
                        PARAMETER_RATE_BASED_STATEMENT,
                        STATEMENT_REFERENCE_NOT_VALID_REASON,
                    ));
                }
                let window = rate_based.evaluation_window_sec;
                if window != 0 && !VALID_EVALUATION_WINDOWS_SEC.contains(&window) {
                    return Some(invalid_parameter_exception(
                        FIELD_RATE_BASED_STATEMENT,
                        &window.to_string(),
                        PARAMETER_NOT_VALID_REASON,
                    ));
                }
            }
            if pre_parse_inspections(statement)
                .into_iter()
                .any(|(field_to_match, pre_parse)| {
                    !pre_parse.is_empty() && !supports_pre_parse_text_transformations(field_to_match)
                })
            {
                return Some(invalid_parameter_exception(
                    FIELD_PRE_PARSE_TEXT_TRANSFORMATION,
                    PARAMETER_PRE_PARSE_TEXT_TRANSFORMATION,
                    PRE_PARSE_UNSUPPORTED_FIELD_REASON,
                ));
            }
            pending.extend(
                nested_statements(statement)
                    .into_iter()
                    .map(|nested| (nested, false)),
            );
        }
    }
    None
}
