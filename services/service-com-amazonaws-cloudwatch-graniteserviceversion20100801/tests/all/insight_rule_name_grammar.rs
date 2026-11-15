// SPDX-License-Identifier: CC0-1.0

//! Cover for the GRAMMAR a Contributor Insights rule name is judged against, across every
//! operation of this service that judges one: `DeleteInsightRules`, `EnableInsightRules` and
//! `DisableInsightRules` per `RuleNames` entry, and `PutInsightRule` on the one name it writes.
//!
//! The rule under test: **a Contributor Insights rule name is 1..128 characters of printable
//! ASCII none of which is the space — narrower than the `@pattern ^[\x20-\x7E]+$` the model's
//! `InsightRuleName` declares (`smithy_query(query="shape",shape="InsightRuleName")`), whose
//! span opens on `\x20`, the space. A batch rule operation judges each entry on its own and
//! reports one outside that grammar as that entry's own `Failures` entry of the
//! invalid-parameter class on an HTTP 200 — never as a refusal of the request — while a legal
//! name is resolved: held, it is acted on, and unknown it follows the operation's own semantics
//! (nothing to report for a delete, the resource that is not there for a state move). The write
//! that creates a rule refuses a name outside the same grammar, so no rule is ever stored under
//! a name those operations would report back as unusable.**
//!
//! "If the operation could not process one of the rules, the following data is returned for
//! each of those rules." (`PartialFailure`) — the per-entry report IS the mechanism, and
//! `DeleteInsightRules` models no error for the shape of one entry
//! (`smithy_query(query="operation",operation="DeleteInsightRules")` declares only
//! `InvalidParameterValueException` and `MissingRequiredParameterException`, neither of which
//! AWS answers this request with).
//!
//! Rows differ along what the rule generalises over: which operation is called, WHERE the space
//! sits in the name (inside, leading, trailing, repeated, the whole name), which other way a
//! value misses the grammar (no characters, too many characters, a byte outside the printable
//! span), where in the batch the entry sits, and — on the other side of the same bound — the
//! shortest, the longest and the punctuation-carrying names that ARE legal, including both ends
//! of the admitted span.

use crate::harness::{build_router, call, error_code, error_message, ok};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Every operation that judges a `RuleNames` ENTRY, and the state it moves a held rule to (the
/// delete removes it instead, which is what the empty state stands for).
const BATCH_OPERATIONS: [(&str, &str); 3] = [
    ("DeleteInsightRules", ""),
    ("EnableInsightRules", "ENABLED"),
    ("DisableInsightRules", "DISABLED"),
];

/// A rule definition in the Contributor Insights rule syntax, as `PutInsightRule` takes it.
fn definition() -> String {
    json!({
        "Schema": {"Name": "CloudWatchLogRule", "Version": 1},
        "LogGroupNames": ["API-Gateway-Access-Logs"],
        "LogFormat": "JSON",
        "Contribution": {"Keys": ["$.ip"]},
    })
    .to_string()
}

/// Create the rule `name` in `rule_state`.
async fn given_rule(router: &MultiServiceRouter, name: &str, rule_state: &str) {
    ok(
        router,
        "PutInsightRule",
        &json!({
            "RuleName": name,
            "RuleDefinition": definition(),
            "RuleState": rule_state,
        }),
    )
    .await;
}

/// The rule `DescribeInsightRules` reports under `name`, if any.
async fn described_rule(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let body = ok(router, "DescribeInsightRules", &json!({})).await;
    body.get("InsightRules")?
        .as_array()?
        .iter()
        .find(|rule| rule.get("Name").and_then(Value::as_str) == Some(name))
        .cloned()
}

/// The `Failures` entries an answer carries, in the order it carried them.
fn failures(body: &Value) -> Vec<Value> {
    body.get("Failures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The entry every one of these operations reports for a value that is not a rule name at all:
/// the member VERBATIM, the invalid-parameter class, and the code of that class.
fn invalid_rule_name(rule_name: &str, fault: &str) -> Value {
    json!({
        "FailureResource": rule_name,
        "ExceptionType": "InvalidParameterException",
        "FailureCode": "INVALID_RULE_NAME",
        "FailureDescription": fault,
    })
}

/// The entry the state-moving operations report for a LEGAL name that addresses no rule.
fn rule_not_found(rule_name: &str) -> Value {
    json!({
        "FailureResource": rule_name,
        "ExceptionType": "ResourceNotFoundException",
        "FailureCode": "RULE_NAME",
        "FailureDescription": format!("Rule {rule_name} does not exist."),
    })
}

/// The grammar a refusal and a `FailureDescription` quote: the printable-ASCII span the model's
/// `@pattern` admits with the space, its first character, taken out.
const RULE_NAME_GRAMMAR: &str = r"[\x21-\x7E]+";

/// One value that is not a rule name: what the case is, the value, and how the fault is stated.
struct IllegalName {
    /// What this case is, named in every assertion message.
    case: &'static str,
    /// The value the request carries, verbatim.
    value: String,
    /// The `FailureDescription` the entry carries, and the sentence the write is refused with.
    fault: String,
}

/// A value outside the grammar because of its CHARACTERS, described by the expression a client
/// re-validates against.
fn outside_the_grammar(case: &'static str, value: &str) -> IllegalName {
    IllegalName {
        case,
        value: value.to_string(),
        fault: format!("RuleName \"{value}\" does not match the pattern: {RULE_NAME_GRAMMAR}"),
    }
}

/// Every placement of the SPACE the model's pattern admits and the service's grammar does not:
/// inside the name, at either end, repeated, and the whole name.
fn names_with_a_space() -> Vec<IllegalName> {
    vec![
        outside_the_grammar("spaces inside the name", "bad rule name"),
        outside_the_grammar("one space inside the name", "bad rule"),
        outside_the_grammar("a leading space", " bad-rule"),
        outside_the_grammar("a trailing space", "bad-rule "),
        outside_the_grammar("two spaces in a row", "bad  rule"),
        outside_the_grammar("nothing but a space", " "),
    ]
}

/// Every way one value misses the grammar: each placement of a space ([`names_with_a_space`]),
/// two bytes outside the printable-ASCII span, a value with no characters at all, and one
/// character more than the 128 the length allows.
fn illegal_names() -> Vec<IllegalName> {
    let mut names = names_with_a_space();
    names.extend([
        outside_the_grammar("a control byte below the span", "\u{1}bad-rule"),
        outside_the_grammar("a character above ASCII", "règle"),
        IllegalName {
            case: "no characters at all",
            value: String::new(),
            fault: "RuleName cannot be an empty string.".to_string(),
        },
        IllegalName {
            case: "one character more than the maximum",
            value: "n".repeat(129),
            fault: "RuleName cannot be longer than 128 characters.".to_string(),
        },
    ]);
    names
}

#[tokio::test]
async fn an_entry_outside_the_grammar_is_reported_per_entry_on_a_served_call() {
    // The batch each row submits: a rule this service holds, a legal name it holds no rule for,
    // and the row's unusable entry last. The delete reports only the unusable entry (a legal
    // name that names nothing is already deleted); the state moves report both, classified
    // independently. Either way the call is served and the held rule is acted on.
    for (operation, target_state) in BATCH_OPERATIONS {
        for illegal in illegal_names() {
            let case = format!("{operation} / {}", illegal.case);
            let router = build_router();
            let created_state = if target_state == "ENABLED" {
                "DISABLED"
            } else {
                "ENABLED"
            };
            given_rule(&router, "held", created_state).await;
            given_rule(&router, "kept", created_state).await;

            let (status, body) = call(
                &router,
                operation,
                &json!({"RuleNames": ["held", "no-such-rule", illegal.value]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: an unusable entry is reported, not refused, got {status} {body}"
            );

            let mut expected = Vec::new();
            if operation != "DeleteInsightRules" {
                expected.push(rule_not_found("no-such-rule"));
            }
            expected.push(invalid_rule_name(&illegal.value, &illegal.fault));
            assert_eq!(
                failures(&body),
                expected,
                "case {case:?}: expected each entry classified on its own, got {body}"
            );

            // The legal held name in the same batch was still acted on.
            let held = described_rule(&router, "held").await;
            if operation == "DeleteInsightRules" {
                assert_eq!(
                    held, None,
                    "case {case:?}: the held rule named by the batch must be gone, got {held:?}"
                );
            } else {
                let actual = held
                    .as_ref()
                    .and_then(|rule| rule.get("State"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert_eq!(
                    actual, target_state,
                    "case {case:?}: expected the held rule in {target_state:?}, got {held:?}"
                );
            }
            // The rule the batch did not name is untouched, and the unusable entry stored
            // nothing of its own.
            let kept = described_rule(&router, "kept").await;
            assert_eq!(
                kept.as_ref()
                    .and_then(|rule| rule.get("State"))
                    .and_then(Value::as_str),
                Some(created_state),
                "case {case:?}: a rule the batch did not name is left alone, got {kept:?}"
            );
            let stored = described_rule(&router, &illegal.value).await;
            assert_eq!(
                stored, None,
                "case {case:?}: an entry reported back stores no rule, got {stored:?}"
            );
        }
    }
}

#[tokio::test]
async fn where_the_unusable_entry_sits_does_not_change_what_is_reported() {
    // The same unusable value, moved through the batch, and a batch carrying two of them: each
    // entry is reported in REQUEST order, about itself, and every legal name is still resolved.
    let illegal = "bad rule name";
    let fault = format!("RuleName \"{illegal}\" does not match the pattern: {RULE_NAME_GRAMMAR}");
    let empty_fault = "RuleName cannot be an empty string.";
    let rows: Vec<(&str, Vec<&str>, Vec<Value>)> = vec![
        (
            "the unusable entry first",
            vec![illegal, "held"],
            vec![invalid_rule_name(illegal, &fault)],
        ),
        (
            "the unusable entry in the middle",
            vec!["held", illegal, "ghost"],
            vec![invalid_rule_name(illegal, &fault)],
        ),
        (
            "the unusable entry last",
            vec!["held", illegal],
            vec![invalid_rule_name(illegal, &fault)],
        ),
        (
            "the same unusable entry twice",
            vec![illegal, illegal],
            vec![
                invalid_rule_name(illegal, &fault),
                invalid_rule_name(illegal, &fault),
            ],
        ),
        (
            "two unusable entries of different classes",
            vec![illegal, "held", ""],
            vec![
                invalid_rule_name(illegal, &fault),
                invalid_rule_name("", empty_fault),
            ],
        ),
        ("every entry legal", vec!["held", "ghost"], Vec::new()),
    ];

    for (row, named, expected) in &rows {
        let case = format!("DeleteInsightRules / {row}");
        let router = build_router();
        given_rule(&router, "held", "ENABLED").await;

        let answered = ok(&router, "DeleteInsightRules", &json!({"RuleNames": named})).await;
        assert_eq!(
            &failures(&answered),
            expected,
            "case {case:?}: expected one entry per unusable name, in request order, got \
             {answered}"
        );
        let held = described_rule(&router, "held").await;
        let deleted = named.contains(&"held");
        assert_eq!(
            held.is_none(),
            deleted,
            "case {case:?}: the legal held name must be deleted when it is named, got {held:?}"
        );
    }
}

#[tokio::test]
async fn a_name_inside_the_grammar_is_resolved_rather_than_reported() {
    // The other side of the same bound: the shortest and the longest name the length admits,
    // both ends of the admitted span, and punctuation-only names — each one held, so a legal
    // name must be deleted rather than come back as an entry. A legal name that is NOT held is
    // reported by nothing either: it already names nothing to delete.
    let longest = "n".repeat(128);
    let ends_of_the_span = format!("{}{}", '\u{21}', '\u{7e}');
    let legal_names: Vec<(&str, &str)> = vec![
        ("one character", "r"),
        ("the longest name the length admits", longest.as_str()),
        ("both ends of the admitted span", ends_of_the_span.as_str()),
        ("punctuation and digits only", "99.9%_$#@!"),
        ("a name of several punctuated words", "My_rule(v2)/high-cardinality"),
    ];

    for (case, name) in &legal_names {
        let router = build_router();
        given_rule(&router, name, "ENABLED").await;

        let answered = ok(
            &router,
            "DeleteInsightRules",
            &json!({"RuleNames": [name, "never-created"]}),
        )
        .await;
        assert_eq!(
            failures(&answered),
            Vec::<Value>::new(),
            "case {case:?}: a legal name reports no failure, got {answered}"
        );
        let actual = described_rule(&router, name).await;
        assert_eq!(
            actual, None,
            "case {case:?}: the held legal name must be deleted, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn the_write_refuses_a_name_outside_the_grammar_and_stores_nothing() {
    // No rule is ever held under a name the batch operations would report back as unusable:
    // the create refuses the value as the bad parameter it is, under the modelled
    // `InvalidParameterValueException` (HTTP 400), and persists nothing. The rows are the
    // values the MODEL admits on this member and the service's grammar does not — the other
    // ways a value misses the grammar break `InsightRuleName`'s own length or pattern, which
    // this member (one name, not an entry of a batch) is refused for before any handler runs.
    for illegal in names_with_a_space() {
        let case = illegal.case;
        let router = build_router();

        let (status, body) = call(
            &router,
            "PutInsightRule",
            &json!({"RuleName": illegal.value, "RuleDefinition": definition()}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {case:?}: expected the write refused as a bad parameter value, got {status} \
             {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("INVALID_RULE_NAME: {}", illegal.fault),
            "case {case:?}: expected the coded refusal for an unusable name, got {body}"
        );
        let described = ok(&router, "DescribeInsightRules", &json!({})).await;
        assert_eq!(
            described["InsightRules"],
            json!([]),
            "case {case:?}: a refused write stores no rule, got {described}"
        );
    }
}
