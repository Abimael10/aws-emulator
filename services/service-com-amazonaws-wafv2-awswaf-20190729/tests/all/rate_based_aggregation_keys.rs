// SPDX-License-Identifier: CC0-1.0

//! What a rate-based statement's AGGREGATION needs before it can be saved, end to end
//! over the wire.
//!
//! The rule asserted here: `RateBasedStatement$AggregateKeyType` is `@required` and IS the
//! aggregation — what the counts are kept per — and three of the four settings it declares
//! are configured by another member of the SAME statement, so a save whose statement names
//! a setting without the member that setting requires is refused with the modelled
//! `WAFInvalidParameterException` (HTTP 400) naming the absent member, and nothing is
//! stored.
//!
//! The settings and the member each one requires, from
//! `smithy_query(query="shape",shape="RateBasedStatement")` (member `AggregateKeyType`):
//!
//! * `FORWARDED_IP` — "with this option, you must specify the header to use in the
//!   `ForwardedIPConfig` property"; the refusal's `Field` is FORWARDED_IP_CONFIG;
//! * `CUSTOM_KEYS` — "with this option, you must specify the aggregate keys in the
//!   `CustomKeys` property"; the refusal's `Field` is CUSTOM_KEYS;
//! * `CONSTANT` — "with this option, you must configure the `ScopeDownStatement`
//!   property"; the refusal's `Field` is SCOPE_DOWN;
//! * `IP` — aggregates on the request origin's own address and requires nothing.
//!
//! The rows differ along what the rule generalises over: every value of the enum
//! (`smithy_query(query="shape",shape="RateBasedStatementAggregateKeyType")` → `IP`,
//! `FORWARDED_IP`, `CUSTOM_KEYS`, `CONSTANT`), whether the member that setting requires is
//! absent, present or — for the list-valued one, whose `@length` floor is 0 — present and
//! empty, the position of the statement in the rule list, and the operation the statement
//! is saved by (`CreateWebACL`, `UpdateWebACL` and `CreateRuleGroup`, since a rate-based
//! rule is the same rule wherever it is saved).
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness in
//! [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK observes.

use serde_json::{json, Value};

use crate::web_acl_lifecycle::{create_web_acl, sqli_statement, visibility, Waf};

/// The scope the rows are saved in: the aggregation is a property of the statement, so one
/// namespace carries the table.
const SCOPE: &str = "REGIONAL";

/// The `ParameterExceptionField` values WAF names the absent members by
/// (`smithy_query(query="shape",shape="ParameterExceptionField")` lists
/// FORWARDED_IP_CONFIG, CUSTOM_KEYS and SCOPE_DOWN). Written out here rather than
/// imported, so the table does not restate the code under test.
const FIELD_FORWARDED_IP_CONFIG: &str = "FORWARDED_IP_CONFIG";
const FIELD_CUSTOM_KEYS: &str = "CUSTOM_KEYS";
const FIELD_SCOPE_DOWN: &str = "SCOPE_DOWN";

/// The `ErrorReason` WAF carries when a parameter leaves out a member its own settings
/// require, as the recorded refusals of that class word it.
const REQUIRED_FIELD_MISSING: &str = "A required field is missing from the parameter.";

/// The `ErrorReason` WAF carries when a parameter carries none of a set of members it
/// needs at least one of, as the recorded refusals of that class word it.
const MISSING_CONDITION: &str = "You are missing at-least one of condition between some \
                                 parameters or all parameters of object.";

/// The `Parameter` every refusal of this class names: the statement whose aggregation
/// asked for the absent member.
const PARAMETER: &str = "RateBasedStatement";

/// The error body's message member, whichever casing the shape models.
fn error_message(body: &Value) -> &str {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The three members the model gives `WAFInvalidParameterException`, and the message WAF
/// composes from exactly those three.
fn assert_invalid_parameter(body: &Value, field: &str, reason: &str, case: &str) {
    assert_eq!(
        (
            body["__type"].as_str().unwrap_or_default().rsplit('#').next(),
            body["Field"].as_str(),
            body["Parameter"].as_str(),
            body["Reason"].as_str()
        ),
        (
            Some("WAFInvalidParameterException"),
            Some(field),
            Some(PARAMETER),
            Some(reason)
        ),
        "case {case:?}: expected the invalid-parameter refusal naming {field:?}, got {body}"
    );
    assert_eq!(
        error_message(body),
        format!("Error reason: {reason}, field: {field}, parameter: {PARAMETER}"),
        "case {case:?}: the message must read the way WAF composes it, got {body}"
    );
}

/// Whether the member a setting requires is absent, supplied, or supplied EMPTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Companion {
    Absent,
    Supplied,
    /// A list-valued member written as the empty list, which `@length(min 0)` admits on
    /// the wire and which specifies no aggregate key either.
    Empty,
}

/// One row: an `AggregateKeyType`, how its companion member is written, and the `Field`
/// and `Reason` of the refusal that earns — or `None` when the statement is complete.
struct Row {
    case: &'static str,
    aggregate_key_type: &'static str,
    companion: Companion,
    refused_as: Option<(&'static str, &'static str)>,
}

/// Every row of the table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "IP, which requires no companion member",
            aggregate_key_type: "IP",
            companion: Companion::Absent,
            refused_as: None,
        },
        Row {
            case: "FORWARDED_IP without its ForwardedIPConfig",
            aggregate_key_type: "FORWARDED_IP",
            companion: Companion::Absent,
            refused_as: Some((FIELD_FORWARDED_IP_CONFIG, REQUIRED_FIELD_MISSING)),
        },
        Row {
            case: "FORWARDED_IP with its ForwardedIPConfig",
            aggregate_key_type: "FORWARDED_IP",
            companion: Companion::Supplied,
            refused_as: None,
        },
        Row {
            case: "CUSTOM_KEYS without its CustomKeys",
            aggregate_key_type: "CUSTOM_KEYS",
            companion: Companion::Absent,
            refused_as: Some((FIELD_CUSTOM_KEYS, MISSING_CONDITION)),
        },
        Row {
            case: "CUSTOM_KEYS with an empty CustomKeys list",
            aggregate_key_type: "CUSTOM_KEYS",
            companion: Companion::Empty,
            refused_as: Some((FIELD_CUSTOM_KEYS, MISSING_CONDITION)),
        },
        Row {
            case: "CUSTOM_KEYS with one aggregate key",
            aggregate_key_type: "CUSTOM_KEYS",
            companion: Companion::Supplied,
            refused_as: None,
        },
        Row {
            case: "CONSTANT without its ScopeDownStatement",
            aggregate_key_type: "CONSTANT",
            companion: Companion::Absent,
            refused_as: Some((FIELD_SCOPE_DOWN, REQUIRED_FIELD_MISSING)),
        },
        Row {
            case: "CONSTANT with its ScopeDownStatement",
            aggregate_key_type: "CONSTANT",
            companion: Companion::Supplied,
            refused_as: None,
        },
    ]
}

/// The rate-based statement one row writes: `Limit` is `@range(min 10, max 2000000000)`,
/// so every row names one inside it.
fn rate_based_statement(row: &Row) -> Value {
    let mut statement = json!({
        "Limit": 2000,
        "AggregateKeyType": row.aggregate_key_type,
    });
    match (row.aggregate_key_type, row.companion) {
        (_, Companion::Absent) => {}
        ("FORWARDED_IP", _) => {
            statement["ForwardedIPConfig"] = json!({
                "HeaderName": "X-Forwarded-For",
                "FallbackBehavior": "MATCH",
            });
        }
        ("CUSTOM_KEYS", Companion::Empty) => statement["CustomKeys"] = json!([]),
        ("CUSTOM_KEYS", Companion::Supplied) => {
            statement["CustomKeys"] = json!([{ "HTTPMethod": {} }]);
        }
        ("CONSTANT", _) => statement["ScopeDownStatement"] = sqli_statement(),
        _ => {}
    }
    json!({ "RateBasedStatement": statement })
}

/// The rules one row saves: the rate-based rule at `position` in a list that also carries
/// an ordinary blocking rule, so the statement is judged wherever it sits in the list.
fn rules(row: &Row, position: usize) -> Value {
    let rate_based = json!({
        "Name": "rate",
        "Priority": 0,
        "Statement": rate_based_statement(row),
        "Action": { "Block": {} },
        "VisibilityConfig": visibility("rate"),
    });
    let plain = json!({
        "Name": "plain",
        "Priority": 1,
        "Statement": sqli_statement(),
        "Action": { "Count": {} },
        "VisibilityConfig": visibility("plain"),
    });
    if position == 0 {
        json!([rate_based, plain])
    } else {
        json!([
            { "Name": "plain", "Priority": 0, "Statement": sqli_statement(),
              "Action": { "Count": {} }, "VisibilityConfig": visibility("plain") },
            { "Name": "rate", "Priority": 1, "Statement": rate_based_statement(row),
              "Action": { "Block": {} }, "VisibilityConfig": visibility("rate") }
        ])
    }
}

/// The names a listing reports.
fn names_listed(listed: &Value, member: &str) -> Vec<String> {
    listed[member]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `CreateWebACL` judges the aggregation of every rate-based statement it is asked to
/// store, wherever the statement sits in the rule list, and a refused create leaves no web
/// ACL behind.
#[tokio::test]
async fn a_create_refuses_an_aggregation_missing_the_member_it_requires() {
    let waf = Waf::new();

    for (index, row) in rows().into_iter().enumerate() {
        for position in [0usize, 1usize] {
            let case = format!("{} at position {position}", row.case);
            let name = format!("rate-create-{index}-{position}");
            let (status, body) = waf
                .call(
                    "CreateWebACL",
                    json!({
                        "Name": &name,
                        "Scope": SCOPE,
                        "DefaultAction": { "Allow": {} },
                        "VisibilityConfig": visibility(&name),
                        "Rules": rules(&row, position),
                    }),
                )
                .await;
            match row.refused_as {
                Some((field, reason)) => {
                    assert_eq!(
                        status, 400,
                        "case {case:?}: expected the aggregation refusal, got status \
                         {status} and body {body}"
                    );
                    assert_invalid_parameter(&body, field, reason, &case);
                }
                None => assert_eq!(
                    status, 200,
                    "case {case:?}: the aggregation is configured, so the web ACL is \
                     stored; got status {status} and body {body}"
                ),
            }

            let listed = waf.ok("ListWebACLs", json!({ "Scope": SCOPE })).await;
            assert_eq!(
                names_listed(&listed, "WebACLs").contains(&name),
                row.refused_as.is_none(),
                "case {case:?}: expected the name to be stored: {}, got {listed}",
                row.refused_as.is_none()
            );
        }
    }
}

/// `UpdateWebACL` makes the same judgement before it writes: a refused update leaves the
/// stored rules and the `LockToken` exactly as they were.
#[tokio::test]
async fn an_update_refuses_an_aggregation_missing_the_member_it_requires() {
    let waf = Waf::new();
    let name = "rate-update-acl";
    let kept = json!([{
        "Name": "kept",
        "Priority": 3,
        "Statement": sqli_statement(),
        "Action": { "Block": {} },
        "VisibilityConfig": visibility("kept"),
    }]);
    let (id, _arn, created_lock) = create_web_acl(&waf, SCOPE, name, kept.clone()).await;
    let mut lock = created_lock;

    for row in rows() {
        let (status, body) = waf
            .call(
                "UpdateWebACL",
                json!({
                    "Name": name,
                    "Scope": SCOPE,
                    "Id": &id,
                    "LockToken": &lock,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(name),
                    "Rules": rules(&row, 0),
                }),
            )
            .await;
        let held = waf
            .ok(
                "GetWebACL",
                json!({ "Name": name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        match row.refused_as {
            Some((field, reason)) => {
                assert_eq!(
                    status, 400,
                    "case {:?}: expected the aggregation refusal, got status {status} and \
                     body {body}",
                    row.case
                );
                assert_invalid_parameter(&body, field, reason, row.case);
                assert_eq!(
                    held["LockToken"].as_str(),
                    Some(lock.as_str()),
                    "case {:?}: a refused update must not rotate the LockToken, got {held}",
                    row.case
                );
                assert_eq!(
                    held["WebACL"]["Rules"][0]["Name"].as_str(),
                    Some("kept"),
                    "case {:?}: a refused update must leave the stored rules, got {held}",
                    row.case
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: the aggregation is configured, so the update lands; got \
                     status {status} and body {body}",
                    row.case
                );
                assert_eq!(
                    held["WebACL"]["Rules"][0]["Statement"]["RateBasedStatement"]
                        ["AggregateKeyType"]
                        .as_str(),
                    Some(row.aggregate_key_type),
                    "case {:?}: the stored statement must be the one the update carried, \
                     got {held}",
                    row.case
                );
                // Put the original rule list back, so each row is judged against the same
                // stored document.
                let restore = waf
                    .ok(
                        "UpdateWebACL",
                        json!({
                            "Name": name,
                            "Scope": SCOPE,
                            "Id": &id,
                            "LockToken": body["NextLockToken"].as_str().unwrap_or_default(),
                            "DefaultAction": { "Allow": {} },
                            "VisibilityConfig": visibility(name),
                            "Rules": kept.clone(),
                        }),
                    )
                    .await;
                lock = restore["NextLockToken"]
                    .as_str()
                    .expect("the restoring update answers a NextLockToken")
                    .to_string();
            }
        }
    }
}

/// A rate-based rule is the same rule inside a rule group: `CreateRuleGroup` judges its
/// aggregation too, and a refused create leaves no rule group behind.
#[tokio::test]
async fn a_rule_group_save_refuses_an_aggregation_missing_the_member_it_requires() {
    let waf = Waf::new();

    for (index, row) in rows().into_iter().enumerate() {
        let name = format!("rate-group-{index}");
        let (status, body) = waf
            .call(
                "CreateRuleGroup",
                json!({
                    "Name": &name,
                    "Scope": SCOPE,
                    // Above the price of any row's rules, so capacity never decides a row.
                    "Capacity": 1000,
                    "VisibilityConfig": visibility(&name),
                    "Rules": rules(&row, 0),
                }),
            )
            .await;
        match row.refused_as {
            Some((field, reason)) => {
                assert_eq!(
                    status, 400,
                    "case {:?}: expected the aggregation refusal, got status {status} and \
                     body {body}",
                    row.case
                );
                assert_invalid_parameter(&body, field, reason, row.case);
            }
            None => assert_eq!(
                status, 200,
                "case {:?}: the aggregation is configured, so the rule group is stored; \
                 got status {status} and body {body}",
                row.case
            ),
        }
        let listed = waf.ok("ListRuleGroups", json!({ "Scope": SCOPE })).await;
        assert_eq!(
            names_listed(&listed, "RuleGroups").contains(&name),
            row.refused_as.is_none(),
            "case {:?}: expected the name to be stored: {}, got {listed}",
            row.case,
            row.refused_as.is_none()
        );
    }
}
