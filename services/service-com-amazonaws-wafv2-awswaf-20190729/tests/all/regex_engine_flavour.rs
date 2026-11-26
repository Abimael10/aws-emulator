// SPDX-License-Identifier: CC0-1.0

//! WHICH regular expressions a WAFv2 save may carry, end to end over the wire.
//!
//! The rule asserted here: WAF inspects requests with a finite-automaton matcher, so
//! the expressions a save may store are the ones such a matcher can run. An
//! expression that asks for BACKTRACKING — a zero-width assertion of any of the
//! lookaround forms, a backreference, an atomic group or possessive quantifier, a
//! match-start reset, a recursion or subroutine call, a conditional, a control verb —
//! is refused with HTTP 400 `WAFInvalidParameterException` naming the one offending
//! expression; an expression outside that class is ACCEPTED and round-trips verbatim,
//! including constructs the matcher's own documentation lists as unsupported: a
//! capturing group and the single-byte directive are stored and read back as written.
//!
//! So the refusal is about what the matcher can run, not about which constructs are
//! documented or which ones some other engine happens to compile — and it is the same
//! judgement on every member that carries an expression:
//! `RegularExpressionList[].RegexString` on
//! `CreateRegexPatternSet`/`UpdateRegexPatternSet`, and
//! `RegexMatchStatement$RegexString` on the rule-carrying operations.
//! `crate::regex_syntax_validation` spans the text that is no expression at all, and
//! `crate::refusal_classes` the expression whose match can consume nothing.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness
//! in [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK
//! observes.

use serde_json::{json, Value};

use crate::web_acl_lifecycle::{rule, visibility, Waf};

/// The `ParameterExceptionField` value naming a regex pattern set
/// (`smithy_query(query="shape",shape="ParameterExceptionField")` lists
/// `REGEX_PATTERN_SET` among its values).
const FIELD_REGEX_PATTERN_SET: &str = "REGEX_PATTERN_SET";

/// The `ParameterExceptionField` value naming a rule statement. See
/// [`FIELD_REGEX_PATTERN_SET`].
const FIELD_STATEMENT: &str = "STATEMENT";

/// The `Reason` WAF carries for a parameter whose text it cannot read in the form
/// that parameter has to take, as the recorded refusals of that class word it.
const FORMAT_NOT_VALID: &str = "The parameter contains formatting that is not valid.";

/// The expressions a save must ACCEPT, each with what the row is about.
///
/// The rows span the constructs a finite-automaton matcher runs — a non-capturing
/// group, an anchored counted repetition, an inline flag, a POSIX class — and, the
/// half this is here for, the two constructs the matcher's documentation lists as
/// unsupported and WAF nevertheless stores: a CAPTURING group and the single-byte
/// directive `\C`. The last rows are ones no report named: the directive quantified,
/// beside a literal of the same letter (`\\C`, a backslash then a `C`), and two
/// capturing groups with a named one among them.
fn accepted() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a non-capturing group", "(?:admin|root)"),
        ("an anchored counted repetition", "^/api/v[0-9]{1,3}/"),
        ("an inline flag", r"(?i)wp-login\.php"),
        ("a POSIX character class", "[[:alpha:]]+"),
        ("a capturing group", "(abc)"),
        ("the single-byte directive", r"\C"),
        ("the single-byte directive, quantified", r"\C{2,4}"),
        ("an escaped backslash before the same letter", r"\\C"),
        ("two groups, one of them named", "(?P<verb>GET|POST)-(x)"),
    ]
}

/// The expressions a save must REFUSE: the backtracking class, one row per form.
fn refused() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a positive lookahead", "(?=.*password)"),
        ("a negative lookahead", "(?!x)admin"),
        ("a positive lookbehind", "(?<=/)admin"),
        ("a negative lookbehind", "(?<!/)admin"),
        ("a backreference", r"(a)\1"),
        ("a named backreference", r"(?P<a>x)\k<a>"),
        ("an atomic group", "(?>abc)"),
        ("a possessive quantifier", "a++"),
        ("a match-start reset", r"foo\Kbar"),
        ("a recursion", "(?R)x"),
        ("a subroutine call", "(?1)x"),
        ("the newline-matching directive", r"\R"),
        ("a conditional", "(?(1)a|b)"),
        ("a control verb", "(*FAIL)"),
    ]
}

/// `[{ "RegexString": … }, …]` — a `RegularExpressionList` over `patterns`.
fn regular_expression_list<S: AsRef<str>>(patterns: &[S]) -> Value {
    Value::Array(
        patterns
            .iter()
            .map(|pattern| json!({ "RegexString": pattern.as_ref() }))
            .collect(),
    )
}

/// A `RegexMatchStatement` over the URI path, carrying `pattern`.
fn regex_match_statement(pattern: &str) -> Value {
    json!({
        "RegexMatchStatement": {
            "RegexString": pattern,
            "FieldToMatch": { "UriPath": {} },
            "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
        }
    })
}

/// The refusal must carry the three members the model gives
/// `WAFInvalidParameterException`, and the message WAF composes from exactly those
/// three.
fn assert_regex_refusal(body: &Value, field: &str, pattern: &str, case: &str) {
    assert_eq!(
        (
            body["Field"].as_str(),
            body["Parameter"].as_str(),
            body["Reason"].as_str()
        ),
        (Some(field), Some(pattern), Some(FORMAT_NOT_VALID)),
        "case {case:?}: the refusal must name the settings, the offending expression \
         and the reason, got {body}"
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        format!("Error reason: {FORMAT_NOT_VALID}, field: {field}, parameter: {pattern}"),
        "case {case:?}: the refusal must read the way WAF words it, got {body}"
    );
}

/// Every accepted construct is stored by `CreateRegexPatternSet` and read back byte
/// for byte, alone and in a list with the others.
#[tokio::test]
async fn a_pattern_set_stores_every_construct_the_matcher_runs() {
    let waf = Waf::new();

    for (index, (case, pattern)) in accepted().iter().enumerate() {
        let name = format!("flavour-accepted-{index}");
        let submitted = [*pattern];
        let created = waf
            .ok(
                "CreateRegexPatternSet",
                json!({
                    "Name": &name,
                    "Scope": "REGIONAL",
                    "RegularExpressionList": regular_expression_list(&submitted),
                }),
            )
            .await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create answers Summary.Id"))
            .to_string();
        let got = waf
            .ok(
                "GetRegexPatternSet",
                json!({ "Name": &name, "Scope": "REGIONAL", "Id": &id }),
            )
            .await;
        assert_eq!(
            got["RegexPatternSet"]["RegularExpressionList"],
            regular_expression_list(&submitted),
            "case {case:?}: an accepted expression must read back byte for byte, got \
             {got}"
        );
    }

    // The same expressions as ONE list, which is how a rule set that needs them all
    // is written: the whole list is accepted and round-trips in the submitted order.
    let every: Vec<&str> = accepted().into_iter().map(|(_, pattern)| pattern).collect();
    let created = waf
        .ok(
            "CreateRegexPatternSet",
            json!({
                "Name": "flavour-accepted-all",
                "Scope": "REGIONAL",
                "RegularExpressionList": regular_expression_list(&every),
            }),
        )
        .await;
    let id = created["Summary"]["Id"].as_str().expect("Summary.Id").to_string();
    let got = waf
        .ok(
            "GetRegexPatternSet",
            json!({ "Name": "flavour-accepted-all", "Scope": "REGIONAL", "Id": &id }),
        )
        .await;
    assert_eq!(
        got["RegexPatternSet"]["RegularExpressionList"],
        regular_expression_list(&every),
        "the whole list must round-trip in the order it was submitted, got {got}"
    );
}

/// A rule's own expression is judged the same way: every accepted construct saves,
/// and the saved rule reads back verbatim.
#[tokio::test]
async fn a_rule_stores_every_construct_the_matcher_runs() {
    let waf = Waf::new();
    for (index, (case, pattern)) in accepted().iter().enumerate() {
        let name = format!("flavour-rule-{index}");
        let rules = json!([rule("inspect", regex_match_statement(pattern))]);
        let created = waf
            .ok(
                "CreateWebACL",
                json!({
                    "Name": &name,
                    "Scope": "REGIONAL",
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(&name),
                    "Rules": rules.clone(),
                }),
            )
            .await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create answers Summary.Id"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": "REGIONAL", "Id": &id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Rules"], rules,
            "case {case:?}: an accepted rule must read back verbatim, got {got}"
        );
        // And the same expression is priced rather than refused.
        let priced = waf
            .ok("CheckCapacity", json!({ "Scope": "REGIONAL", "Rules": rules }))
            .await;
        assert!(
            priced["Capacity"].as_i64().is_some_and(|capacity| capacity > 0),
            "case {case:?}: an accepted rule list must answer a capacity, got {priced}"
        );
    }
}

/// Every construct of the backtracking class is refused by a create and by an update,
/// naming only the offending expression — and the stored list and token stand.
#[tokio::test]
async fn a_pattern_set_refuses_every_construct_the_matcher_cannot_run() {
    let waf = Waf::new();
    let stored = ["^/admin", r"\.js$"];
    let created = waf
        .ok(
            "CreateRegexPatternSet",
            json!({
                "Name": "flavour-refused",
                "Scope": "REGIONAL",
                "RegularExpressionList": regular_expression_list(&stored),
            }),
        )
        .await;
    let id = created["Summary"]["Id"].as_str().expect("Summary.Id").to_string();
    let lock_token = created["Summary"]["LockToken"]
        .as_str()
        .expect("Summary.LockToken")
        .to_string();

    for (index, (case, pattern)) in refused().iter().enumerate() {
        // The offending expression is submitted after accepted neighbours, so the
        // refusal is shown to report the element at fault.
        let submitted = vec![stored[0], pattern];
        let create_name = format!("flavour-refused-{index}");
        let body = waf
            .refused(
                "CreateRegexPatternSet",
                json!({
                    "Name": &create_name,
                    "Scope": "REGIONAL",
                    "RegularExpressionList": regular_expression_list(&submitted),
                }),
                "WAFInvalidParameterException",
                case,
            )
            .await;
        assert_regex_refusal(&body, FIELD_REGEX_PATTERN_SET, pattern, case);

        let body = waf
            .refused(
                "UpdateRegexPatternSet",
                json!({
                    "Name": "flavour-refused",
                    "Scope": "REGIONAL",
                    "Id": &id,
                    "LockToken": &lock_token,
                    "RegularExpressionList": regular_expression_list(&submitted),
                }),
                "WAFInvalidParameterException",
                case,
            )
            .await;
        assert_regex_refusal(&body, FIELD_REGEX_PATTERN_SET, pattern, case);
        assert!(
            body["NextLockToken"].is_null(),
            "case {case:?}: a refusal must carry no NextLockToken, got {body}"
        );

        // And a rule carrying it is refused by the save path too, under the field the
        // statement is reported at.
        let body = waf
            .refused(
                "CreateWebACL",
                json!({
                    "Name": &create_name,
                    "Scope": "REGIONAL",
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(&create_name),
                    "Rules": [rule("inspect", regex_match_statement(pattern))],
                }),
                "WAFInvalidParameterException",
                case,
            )
            .await;
        assert_regex_refusal(&body, FIELD_STATEMENT, pattern, case);
    }

    // The refusals changed nothing: the stored list stands and the token the caller
    // holds still saves.
    let got = waf
        .ok(
            "GetRegexPatternSet",
            json!({ "Name": "flavour-refused", "Scope": "REGIONAL", "Id": &id }),
        )
        .await;
    assert_eq!(
        got["RegexPatternSet"]["RegularExpressionList"],
        regular_expression_list(&stored),
        "a refused update must leave the stored list as it was, got {got}"
    );
    assert_eq!(
        got["LockToken"].as_str(),
        Some(lock_token.as_str()),
        "a refused update must not rotate the token, got {got}"
    );
    let updated = waf
        .ok(
            "UpdateRegexPatternSet",
            json!({
                "Name": "flavour-refused",
                "Scope": "REGIONAL",
                "Id": &id,
                "LockToken": &lock_token,
                "RegularExpressionList": regular_expression_list(&[r"\C"]),
            }),
        )
        .await;
    assert!(
        updated["NextLockToken"]
            .as_str()
            .is_some_and(|next| next != lock_token),
        "the accepted update must rotate the token, got {updated}"
    );
}
