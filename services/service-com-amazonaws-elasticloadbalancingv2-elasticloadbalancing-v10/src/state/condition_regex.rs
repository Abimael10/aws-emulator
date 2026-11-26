// SPDX-License-Identifier: CC0-1.0

//! The REGULAR EXPRESSIONS a rule condition matches on, and how many WILDCARDS the values
//! beside them may hold.
//!
//! A condition carries two kinds of value, and the model gives each its own member.
//! `Values` is written in the wildcard language the member documents — "* (matches 0 or more
//! characters) and ? (matches exactly 1 character)" — and `RegexValues` is "the regular
//! expressions to compare against the request URL" (`PathPatternConditionConfig`), the host
//! header or the HTTP header. So a regular expression is a value the service COMPILES: one it
//! cannot compile matches no request at all, and Elastic Load Balancing refuses it rather than
//! storing a condition that can never be met — exactly as it refuses a `source-ip` value that
//! is no address block ([`super::source_ip_conditions`]).
//!
//! The expressions are compiled by the maintained linear-time engine this workspace already
//! matches caller-supplied patterns with (`regex`), under two explicit bounds, so what
//! compiling one costs is bounded rather than left to what the caller wrote: the published
//! LENGTH of a condition value ([`CONDITION_VALUE_CHARS`]) and a program-size ceiling
//! ([`REGEX_PROGRAM_BYTES`]). How MANY are compiled for one request is bounded where the
//! values of a rule are counted ([`super::CONDITION_VALUES_PER_RULE`], judged before any of
//! them is read) and, on the data plane, by the matching budget that walk charges
//! (`crate::service::data_plane`). The engine has no backtracking, so a match costs time
//! linear in the text it is run over.
//!
//! ONE reading serves both sides of such a value's life, so the two cannot drift: the
//! operations that SET a rule's conditions refuse an expression that does not compile
//! ([`check_condition_regex_values`], reached from [`super::check_rule_conditions`]), and the
//! data plane evaluates a stored condition through the same compilation
//! (`crate::service::data_plane`).
//!
//! The WILDCARDS are the other bound a rule's values carry: a rule holds at most
//! [`CONDITION_WILDCARDS_PER_RULE`] of them across every condition it matches with, which is
//! the number Elastic Load Balancing states in the refusal itself
//! ([`condition_wildcards_message`]) — the way the condition COUNT is published in its own
//! refusal ([`super::CONDITIONS_PER_RULE`]).

use ::regex::{Regex, RegexBuilder};

use crate::ops::types::RuleCondition;

/// The wildcard characters a condition value may hold, from `RuleCondition$Values`' own
/// documentation: "* (matches 0 or more characters)" and "? (matches exactly 1 character)".
/// They are what [`condition_wildcards`] counts, and the language
/// `crate::service::data_plane` matches such a value in.
const WILDCARD_CHARACTERS: [char; 2] = ['*', '?'];

/// The `RuleCondition$Field` values whose configuration shape carries values of either kind,
/// from that member's own documented list — the three the model gives a `RegexValues`
/// spelling ("[s]pecify only when `Field` is `http-header`, `host-header`, or
/// `path-pattern`") and the one beside them whose values are written as patterns.
const HOST_HEADER_FIELD: &str = "host-header";
const PATH_PATTERN_FIELD: &str = "path-pattern";
const HTTP_HEADER_FIELD: &str = "http-header";
const QUERY_STRING_FIELD: &str = "query-string";

/// How many wildcards one rule's condition values hold in total.
///
/// Elastic Load Balancing states this bound in the refusal itself ("A rule can only have '6'
/// condition wildcards"), which is where this number comes from, exactly as
/// [`super::CONDITIONS_PER_RULE`] comes from its own refusal. It is a bound on the RULE rather
/// than on one value: the matching engine behind a rule evaluates every condition of it, so
/// the wildcards of all of them count together.
///
/// It is deliberately NOT the figure `DescribeAccountLimits` reports under
/// `condition-wildcards-per-alb-rule`, which is the published quota table's own number: what a
/// request hits is the bound the refusal quotes, and the read answers the published default
/// (`crate::ops::describe_account_limits`), exactly as the service does.
pub const CONDITION_WILDCARDS_PER_RULE: usize = 6;

/// The refusal a rule whose conditions hold more wildcards than that earns, in Elastic Load
/// Balancing's own wording.
///
/// Fixed text quoting the limit and naming neither the count the request carried nor the
/// condition it sat in — the shape of every refusal this service words about a rule's values
/// as a whole ([`super::check_rule_conditions`]).
#[must_use]
pub fn condition_wildcards_message() -> String {
    format!("A rule can only have '{CONDITION_WILDCARDS_PER_RULE}' condition wildcards")
}

/// The refusal a condition carrying a regular expression this engine cannot compile earns, in
/// Elastic Load Balancing's own wording.
///
/// Fixed text: the offending expression is NOT interpolated, because the refusal is about the
/// set of expressions the rule carries rather than about one value of it.
pub const CONDITION_REGEX_MESSAGE: &str = "One or more provided condition regex values are not valid";

/// How large a compiled condition expression may grow, in bytes of the engine's own program.
///
/// The emulator's OWN bound, as the data plane's matching bounds are: an expression is text a
/// caller wrote, and a short one can ask for a very large program (a wide bounded repetition
/// of a wide class), so the ceiling is what keeps compiling one — on the create that stores it
/// and on every request matched against it — costing a bounded amount of memory. An expression
/// past it compiles nowhere, so it is refused where a rule is stored rather than accepted by
/// the control plane and skipped by the data plane.
const REGEX_PROGRAM_BYTES: usize = 64 * 1024;

/// How long one condition value may be, from every member that carries one: "The maximum
/// length of each string is 128 characters" (`PathPatternConditionConfig$RegexValues`,
/// `HostHeaderConditionConfig$RegexValues`, `HttpHeaderConditionConfig$RegexValues`, and the
/// wildcard members beside them).
///
/// It is what bounds the WORK of compiling one: an expression past it is one no request could
/// be matched against anyway — the data plane matches a value of that length against nothing
/// (`crate::service::data_plane`) — so compiling it is refused here, and the expressions a
/// rule holds are exactly the ones that plane can evaluate.
pub const CONDITION_VALUE_CHARS: usize = 128;

/// The compiled form of one condition expression, or `None` when this engine compiles no
/// program for it — a value that is no regular expression, one longer than a condition value
/// may be ([`CONDITION_VALUE_CHARS`]), and one whose program is larger than
/// [`REGEX_PROGRAM_BYTES`].
///
/// `case_sensitive` is the comparison the FIELD documents, so one expression is read the same
/// way the wildcard values beside it are: a host name "comparison is case insensitive", a path
/// pattern's "is case sensitive".
#[must_use]
pub fn condition_regex(pattern: &str, case_sensitive: bool) -> Option<Regex> {
    // The length is judged through a BOUNDED probe and before anything is compiled, so the
    // work of one call is bounded by the published maximum rather than by the length of the
    // value a caller sent.
    if pattern.chars().take(CONDITION_VALUE_CHARS + 1).count() > CONDITION_VALUE_CHARS {
        return None;
    }
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .size_limit(REGEX_PROGRAM_BYTES)
        .build()
        .ok()
}

/// Whether `pattern` is a regular expression this service can match a request against at all.
#[must_use]
pub fn names_a_condition_regex(pattern: &str) -> bool {
    // The case flag changes no expression's SYNTAX, so whether one compiles is asked once.
    condition_regex(pattern, true).is_some()
}

/// Every regular expression one condition matches on, read through the ONE reading of a
/// condition's two spellings ([`super::reported_condition`]), so a value given directly in
/// `RegexValues` and the same value given inside the field's own configuration shape are read
/// once rather than twice.
///
/// A field the model gives no `RegexValues` spelling matches on no expression, so it reads as
/// none — which is the same answer the operations that SET a condition refuse such a member on
/// ([`super::check_rule_conditions`]).
#[must_use]
pub fn condition_regex_values(condition: &RuleCondition) -> Vec<String> {
    let reported = super::reported_condition(condition);
    let field = reported.field.clone().unwrap_or_default();
    let configured = match field.as_str() {
        HOST_HEADER_FIELD => reported
            .host_header_config
            .and_then(|config| config.regex_values),
        PATH_PATTERN_FIELD => reported
            .path_pattern_config
            .and_then(|config| config.regex_values),
        HTTP_HEADER_FIELD => reported
            .http_header_config
            .and_then(|config| config.regex_values),
        _ => None,
    };
    configured.unwrap_or_default()
}

/// Every value one condition matches on in the WILDCARD language, read through the same one
/// reading of its two spellings, so a value given either way is counted once.
///
/// A `query-string` condition's pairs are two such values each — the key and the value are
/// matched separately, so a wildcard in either is one the rule's matching engine evaluates.
/// `http-request-method` and `source-ip` carry none: a method is "the comparison is case
/// sensitive" with no wildcard documented, and a source address block documents "[w]ildcards
/// are not supported".
#[must_use]
fn condition_wildcard_values(condition: &RuleCondition) -> Vec<String> {
    let reported = super::reported_condition(condition);
    let field = reported.field.clone().unwrap_or_default();
    match field.as_str() {
        HOST_HEADER_FIELD => reported
            .host_header_config
            .and_then(|config| config.values)
            .unwrap_or_default(),
        PATH_PATTERN_FIELD => reported
            .path_pattern_config
            .and_then(|config| config.values)
            .unwrap_or_default(),
        HTTP_HEADER_FIELD => reported
            .http_header_config
            .and_then(|config| config.values)
            .unwrap_or_default(),
        QUERY_STRING_FIELD => {
            let mut values: Vec<String> = Vec::new();
            for pair in reported
                .query_string_config
                .and_then(|config| config.values)
                .unwrap_or_default()
            {
                values.extend(pair.key);
                values.extend(pair.value);
            }
            values
        }
        _ => Vec::new(),
    }
}

/// How many wildcards the conditions of one rule hold in total.
#[must_use]
pub fn condition_wildcards(conditions: &[RuleCondition]) -> usize {
    conditions
        .iter()
        .flat_map(condition_wildcard_values)
        .map(|value| {
            value
                .chars()
                .filter(|character| WILDCARD_CHARACTERS.contains(character))
                .count()
        })
        .sum()
}

/// Refuse a rule whose conditions hold more wildcards than one rule may carry.
pub fn check_condition_wildcards(conditions: &[RuleCondition]) -> Result<(), String> {
    if condition_wildcards(conditions) > CONDITION_WILDCARDS_PER_RULE {
        return Err(condition_wildcards_message());
    }
    Ok(())
}

/// Refuse a rule carrying a condition regular expression this service cannot compile.
///
/// Every expression of every condition is judged: the expressions of one condition are
/// alternatives, so one that can match nothing is a value the caller meant something by.
pub fn check_condition_regex_values(conditions: &[RuleCondition]) -> Result<(), String> {
    for condition in conditions {
        for value in condition_regex_values(condition) {
            if !names_a_condition_regex(&value) {
                return Err(CONDITION_REGEX_MESSAGE.to_string());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The reading of ONE value. What a rule's conditions are refused for, and what a stored
    //! condition then matches, are pinned end to end in `tests/all/alb_data_plane.rs` and
    //! `tests/all/rule_condition_regex_and_wildcards.rs`.

    use super::*;
    use crate::ops::types::{
        HttpHeaderConditionConfig, PathPatternConditionConfig, QueryStringConditionConfig,
        QueryStringKeyValuePair,
    };

    /// One `path-pattern` condition carrying `values` and `regex_values`.
    fn path_pattern(values: &[&str], regex_values: &[&str]) -> RuleCondition {
        RuleCondition {
            field: Some("path-pattern".to_string()),
            path_pattern_config: Some(PathPatternConditionConfig {
                values: (!values.is_empty())
                    .then(|| values.iter().map(|value| (*value).to_string()).collect()),
                regex_values: (!regex_values.is_empty()).then(|| {
                    regex_values
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect()
                }),
            }),
            ..RuleCondition::default()
        }
    }

    /// RULE: a value is a condition regular expression exactly when this engine compiles a
    /// program for it.
    #[test]
    fn a_value_is_a_condition_regex_exactly_when_it_compiles() {
        // The rows span what the rule generalises over: the forms a valid expression takes,
        // and each way one can fail to compile — an unclosed group, an unclosed class, a
        // repetition with no term, a backreference the engine has no program for, and a
        // repetition whose program no ceiling admits.
        let rows: &[(&str, &str, bool)] = &[
            ("a literal path", "^/api/items$", true),
            ("a class and a repetition", "^/api/v[0-9]+/items$", true),
            ("an alternation", "^/(red|green|blue)$", true),
            ("an unanchored expression", "items", true),
            ("a unicode class", r"^/\w+$", true),
            ("an unclosed group", "^/bad/(unclosed", false),
            ("an unclosed character class", "^/bad/[unclosed", false),
            ("a repetition with nothing to repeat", "*/bad", false),
            ("a backreference", r"^(a)\1$", false),
            ("a look-ahead", "^(?=/api)/api/items$", false),
            ("a program larger than the ceiling", r"^(?:\w{1000}){1000}$", false),
        ];
        for (case, pattern, expected) in rows {
            let read = names_a_condition_regex(pattern);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected it to read as an expression={expected}, got {read}"
            );
        }

        // And the published LENGTH of a condition value is a bound of its own: an expression
        // at it compiles, one past it is a value no request is matched against.
        let longest: String = "a".repeat(CONDITION_VALUE_CHARS);
        let past_it: String = "a".repeat(CONDITION_VALUE_CHARS + 1);
        for (case, pattern, expected) in [
            ("an expression at the published length", &longest, true),
            ("one character past it", &past_it, false),
        ] {
            let read = names_a_condition_regex(pattern);
            assert_eq!(
                read, expected,
                "case {case:?}: expected it to read as an expression={expected}, got {read}"
            );
        }
    }

    /// RULE: the refusal reaches a rule's expressions whichever of the two spellings carries
    /// them, and names no value.
    #[test]
    fn a_rule_is_refused_for_an_expression_under_either_spelling() {
        let direct = RuleCondition {
            field: Some("path-pattern".to_string()),
            regex_values: Some(vec!["^/bad/(unclosed".to_string()]),
            ..RuleCondition::default()
        };
        let configured = path_pattern(&[], &["^/bad/(unclosed"]);
        let header = RuleCondition {
            field: Some("http-header".to_string()),
            http_header_config: Some(HttpHeaderConditionConfig {
                http_header_name: Some("x-version".to_string()),
                values: None,
                regex_values: Some(vec!["^v[0-9]+".to_string(), "(".to_string()]),
            }),
            ..RuleCondition::default()
        };
        let rows: &[(&str, Vec<RuleCondition>, bool)] = &[
            ("the top-level spelling", vec![direct], false),
            ("the configuration spelling", vec![configured], false),
            ("one of several expressions", vec![header], false),
            (
                "a rule whose expressions all compile",
                vec![path_pattern(&["/img/*"], &["^/api/v[0-9]+$"])],
                true,
            ),
            (
                "a rule carrying no expression at all",
                vec![path_pattern(&["/img/*"], &[])],
                true,
            ),
        ];
        for (case, conditions, accepted) in rows {
            let answer = check_condition_regex_values(conditions);
            assert_eq!(
                answer.is_ok(),
                *accepted,
                "case {case:?}: expected accepted={accepted}, got {answer:?}"
            );
            if !*accepted {
                assert_eq!(
                    answer.as_ref().err().map(String::as_str),
                    Some(CONDITION_REGEX_MESSAGE),
                    "case {case:?}: expected the refusal an invalid expression earns, got \
                     {answer:?}"
                );
            }
        }
    }

    /// RULE: the wildcards of every condition of one rule count together, up to the published
    /// limit.
    #[test]
    fn the_wildcards_of_a_rule_are_counted_across_its_conditions() {
        let query = RuleCondition {
            field: Some("query-string".to_string()),
            query_string_config: Some(QueryStringConditionConfig {
                values: Some(vec![QueryStringKeyValuePair {
                    key: Some("ver*".to_string()),
                    value: Some("2*".to_string()),
                }]),
            }),
            ..RuleCondition::default()
        };
        // Each row: the case, the rule's conditions, and how many wildcards they hold. The
        // rows span what the rule generalises over — which spelling carries the value, both
        // wildcard characters, several values in one condition, several conditions, and the
        // limit with one past it.
        let rows: Vec<(&str, Vec<RuleCondition>, usize)> = vec![
            ("no wildcard at all", vec![path_pattern(&["/api"], &[])], 0),
            ("one of each character", vec![path_pattern(&["/a*/b?"], &[])], 2),
            (
                "the top-level spelling",
                vec![RuleCondition {
                    field: Some("path-pattern".to_string()),
                    values: Some(vec!["/a*/b*".to_string()]),
                    ..RuleCondition::default()
                }],
                2,
            ),
            (
                "two values of one condition",
                vec![path_pattern(&["/a*", "/b*"], &[])],
                2,
            ),
            (
                "a query string's key and value",
                vec![query.clone()],
                2,
            ),
            (
                "two conditions together",
                vec![path_pattern(&["/a*/b*"], &[]), query],
                4,
            ),
            (
                "an expression, whose characters are no wildcards",
                vec![path_pattern(&[], &["^/a.*$"])],
                0,
            ),
            (
                "the limit",
                vec![path_pattern(&["/w0*w1*w2*w3*w4*w5*w6"], &[])],
                6,
            ),
            (
                "one past the limit",
                vec![path_pattern(&["/w0*w1*w2*w3*w4*w5*w6*w7"], &[])],
                7,
            ),
        ];
        for (case, conditions, expected) in rows {
            let counted = condition_wildcards(&conditions);
            assert_eq!(
                counted, expected,
                "case {case:?}: expected {expected} wildcards, got {counted}"
            );
            let answer = check_condition_wildcards(&conditions);
            assert_eq!(
                answer.is_ok(),
                expected <= CONDITION_WILDCARDS_PER_RULE,
                "case {case:?}: expected the rule to be accepted only up to the limit, got \
                 {answer:?}"
            );
            if expected > CONDITION_WILDCARDS_PER_RULE {
                assert_eq!(
                    answer.as_ref().err(),
                    Some(&condition_wildcards_message()),
                    "case {case:?}: expected the refusal the limit earns, got {answer:?}"
                );
            }
        }
    }
}
