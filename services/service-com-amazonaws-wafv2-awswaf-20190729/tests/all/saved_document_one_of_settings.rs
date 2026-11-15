// SPDX-License-Identifier: CC0-1.0

//! The ONE-OF settings of a saved WAFv2 document, end to end over the wire.
//!
//! The rule asserted here: a setting the model documents as a one-of — a structure whose
//! members are alternatives, or two members of which exactly one may be set — has to name
//! exactly ONE of its alternatives, and a save naming none or several is refused with the
//! modelled `WAFInvalidParameterException` (HTTP 400) whose `Field` names the setting at
//! fault and whose `Reason` is WAF's exactly-one sentence. Nothing is written: a refused
//! create leaves no document behind, and a refused update leaves the stored document and
//! its `LockToken` exactly as they were.
//!
//! The settings this spans, each with the `ParameterExceptionField` value WAF names it by
//! (`smithy_query(query="shape",shape="ParameterExceptionField")` lists DEFAULT_ACTION,
//! RULE, RULE_ACTION and OVERRIDE_ACTION):
//!
//! * a web ACL's `DefaultAction`, "the action that you want WAF to perform when a web
//!   request doesn't match any of the rules", which "must be a terminating action" — the
//!   two alternatives `Block` and `Allow`
//!   (`smithy_query(query="shape",shape="DefaultAction")`);
//! * a rule's `Action`/`OverrideAction` pair — "you must specify either this `Action`
//!   setting or the rule `OverrideAction` setting, but not both" (`Rule$Action`);
//! * the `RuleAction` a rule does set — one of the six alternatives
//!   `smithy_query(query="shape",shape="RuleAction")` declares;
//! * the `OverrideAction` a rule does set — `Count` or `None`.
//!
//! The rows differ along what the rule generalises over: WHICH setting is at fault, the
//! shape of the fault (none of the alternatives, and two of them), WHICH alternative is
//! named by the accepted control that differs from the refused row in that one thing
//! alone, and the operation the document is saved by — `CreateWebACL`, `UpdateWebACL`
//! and, for a rule's own action settings, `CreateRuleGroup`, because a rule is a rule
//! wherever it is saved.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness in
//! [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK observes.

use serde_json::{json, Value};

use crate::rule_group_lifecycle::create_rule_group;
use crate::web_acl_lifecycle::{create_web_acl, sqli_statement, visibility, Waf};

/// The scope the rows are saved in. A scope is only the namespace a save lands in and
/// these preconditions are about the document itself, so one namespace carries the table.
const SCOPE: &str = "REGIONAL";

/// The `ParameterExceptionField` values WAF names these settings by. Written out here
/// rather than imported, so the table does not restate the code under test.
const FIELD_DEFAULT_ACTION: &str = "DEFAULT_ACTION";
const FIELD_RULE: &str = "RULE";
const FIELD_RULE_ACTION: &str = "RULE_ACTION";
const FIELD_OVERRIDE_ACTION: &str = "OVERRIDE_ACTION";

/// The `ErrorReason` WAF carries for a one-of that names none of its alternatives or
/// several, as the recorded refusals of that class word it.
const EXACTLY_ONE_REASON: &str =
    "You have used none or multiple values for a field that requires exactly one value.";

/// The error code every row is refused with.
const INVALID_PARAMETER: &str = "WAFInvalidParameterException";

/// The error body's message member, whichever casing the shape models.
fn error_message(body: &Value) -> &str {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The three members the model gives `WAFInvalidParameterException`, and the message WAF
/// composes from exactly those three.
fn assert_invalid_parameter(body: &Value, field: &str, parameter: &str, case: &str) {
    assert_eq!(
        (
            body["Field"].as_str(),
            body["Parameter"].as_str(),
            body["Reason"].as_str()
        ),
        (Some(field), Some(parameter), Some(EXACTLY_ONE_REASON)),
        "case {case:?}: expected Field {field:?}, Parameter {parameter:?} and the \
         exactly-one reason, got {body}"
    );
    assert_eq!(
        error_message(body),
        format!("Error reason: {EXACTLY_ONE_REASON}, field: {field}, parameter: {parameter}"),
        "case {case:?}: the message must read the way WAF composes it, got {body}"
    );
}

/// A rule carrying exactly the action settings `settings` names, over a statement that
/// references no rule group.
fn rule_with(name: &str, priority: i64, settings: &[(&str, Value)]) -> Value {
    let mut rule = json!({
        "Name": name,
        "Priority": priority,
        "Statement": sqli_statement(),
        "VisibilityConfig": visibility(name),
    });
    for (member, value) in settings {
        rule[*member] = value.clone();
    }
    rule
}

/// A rule whose top-level statement RUNS the rule group `arn`, carrying exactly the action
/// settings `settings` names. Such a rule is the one that legitimately carries an
/// `OverrideAction` ("you can only use this for rule statements that reference a rule
/// group", `Rule$OverrideAction`).
fn group_running_rule_with(name: &str, arn: &str, settings: &[(&str, Value)]) -> Value {
    let mut rule = json!({
        "Name": name,
        "Priority": 0,
        "Statement": { "RuleGroupReferenceStatement": { "ARN": arn } },
        "VisibilityConfig": visibility(name),
    });
    for (member, value) in settings {
        rule[*member] = value.clone();
    }
    rule
}

/// The names a listing of `operation`'s resource kind reports.
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

/// One row: the document a save carries, and the setting WAF reports it at fault in.
struct Row {
    case: &'static str,
    /// `DefaultAction`, as this row writes it.
    default_action: Value,
    /// `Rules`, as this row writes them.
    rules: Value,
    /// The `Field` and `Parameter` the refusal names, or `None` for an accepted row.
    refused_as: Option<(&'static str, &'static str)>,
}

/// Every row of the table, over the rule group `group_arn` for the rows that run one.
fn rows(group_arn: &str) -> Vec<Row> {
    let block = || json!({ "Block": {} });
    let allow = || json!({ "Allow": {} });
    vec![
        // The web ACL's own default action.
        Row {
            case: "a DefaultAction naming none of its alternatives",
            default_action: json!({}),
            rules: json!([]),
            refused_as: Some((FIELD_DEFAULT_ACTION, "DefaultAction")),
        },
        Row {
            case: "a DefaultAction naming both of its alternatives",
            default_action: json!({ "Allow": {}, "Block": {} }),
            rules: json!([]),
            refused_as: Some((FIELD_DEFAULT_ACTION, "DefaultAction")),
        },
        Row {
            case: "a DefaultAction naming Allow alone",
            default_action: allow(),
            rules: json!([]),
            refused_as: None,
        },
        Row {
            case: "a DefaultAction naming Block alone",
            default_action: block(),
            rules: json!([]),
            refused_as: None,
        },
        // A rule's Action/OverrideAction pair.
        Row {
            case: "a rule setting neither Action nor OverrideAction",
            default_action: allow(),
            rules: json!([rule_with("a", 0, &[])]),
            refused_as: Some((FIELD_RULE, "Rule")),
        },
        Row {
            case: "a rule setting both Action and OverrideAction",
            default_action: allow(),
            rules: json!([group_running_rule_with(
                "a",
                group_arn,
                &[("Action", block()), ("OverrideAction", json!({ "None": {} }))]
            )]),
            refused_as: Some((FIELD_RULE, "Rule")),
        },
        Row {
            case: "the SECOND rule of the list setting neither",
            default_action: allow(),
            rules: json!([
                rule_with("a", 0, &[("Action", block())]),
                rule_with("b", 1, &[]),
            ]),
            refused_as: Some((FIELD_RULE, "Rule")),
        },
        Row {
            case: "a rule setting Action alone",
            default_action: allow(),
            rules: json!([rule_with("a", 0, &[("Action", block())])]),
            refused_as: None,
        },
        Row {
            case: "a group-running rule setting OverrideAction alone",
            default_action: allow(),
            rules: json!([group_running_rule_with(
                "a",
                group_arn,
                &[("OverrideAction", json!({ "None": {} }))]
            )]),
            refused_as: None,
        },
        // The RuleAction a rule does set.
        Row {
            case: "a rule Action naming none of its alternatives",
            default_action: allow(),
            rules: json!([rule_with("a", 0, &[("Action", json!({}))])]),
            refused_as: Some((FIELD_RULE_ACTION, "RuleAction")),
        },
        Row {
            case: "a rule Action naming two of its alternatives",
            default_action: allow(),
            rules: json!([rule_with(
                "a",
                0,
                &[("Action", json!({ "Block": {}, "Count": {} }))]
            )]),
            refused_as: Some((FIELD_RULE_ACTION, "RuleAction")),
        },
        Row {
            case: "a rule Action naming Count alone",
            default_action: allow(),
            rules: json!([rule_with("a", 0, &[("Action", json!({ "Count": {} }))])]),
            refused_as: None,
        },
        Row {
            case: "a rule Action naming Captcha alone",
            default_action: allow(),
            rules: json!([rule_with("a", 0, &[("Action", json!({ "Captcha": {} }))])]),
            refused_as: None,
        },
        Row {
            case: "a rule Action naming Challenge alone",
            default_action: allow(),
            rules: json!([rule_with("a", 0, &[("Action", json!({ "Challenge": {} }))])]),
            refused_as: None,
        },
        // The OverrideAction a rule does set.
        Row {
            case: "a rule OverrideAction naming none of its alternatives",
            default_action: allow(),
            rules: json!([group_running_rule_with(
                "a",
                group_arn,
                &[("OverrideAction", json!({}))]
            )]),
            refused_as: Some((FIELD_OVERRIDE_ACTION, "OverrideAction")),
        },
        Row {
            case: "a rule OverrideAction naming both of its alternatives",
            default_action: allow(),
            rules: json!([group_running_rule_with(
                "a",
                group_arn,
                &[("OverrideAction", json!({ "None": {}, "Count": {} }))]
            )]),
            refused_as: Some((FIELD_OVERRIDE_ACTION, "OverrideAction")),
        },
        Row {
            case: "a group-running rule OverrideAction naming Count alone",
            default_action: allow(),
            rules: json!([group_running_rule_with(
                "a",
                group_arn,
                &[("OverrideAction", json!({ "Count": {} }))]
            )]),
            refused_as: None,
        },
    ]
}

/// `CreateWebACL` judges every one-of of the document it is asked to store, and a refused
/// create leaves no web ACL behind.
#[tokio::test]
async fn a_create_refuses_a_one_of_that_names_none_or_several_and_stores_nothing() {
    let waf = Waf::new();
    let (_group_id, group_arn, _group_lock) =
        create_rule_group(&waf, SCOPE, "one-of-group", 100, json!([])).await;

    for (position, row) in rows(&group_arn).into_iter().enumerate() {
        let name = format!("one-of-create-{position}");
        let request = json!({
            "Name": &name,
            "Scope": SCOPE,
            "DefaultAction": row.default_action,
            "VisibilityConfig": visibility(&name),
            "Rules": row.rules,
        });
        let (status, body) = waf.call("CreateWebACL", request).await;
        match row.refused_as {
            Some((field, parameter)) => {
                assert_eq!(
                    status, 400,
                    "case {:?}: expected the one-of refusal, got status {status} and body \
                     {body}",
                    row.case
                );
                assert!(
                    body["__type"]
                        .as_str()
                        .unwrap_or_default()
                        .ends_with(INVALID_PARAMETER),
                    "case {:?}: expected {INVALID_PARAMETER}, got {body}",
                    row.case
                );
                assert_invalid_parameter(&body, field, parameter, row.case);
            }
            None => assert_eq!(
                status, 200,
                "case {:?}: the document names exactly one of every one-of, so it is \
                 stored; got status {status} and body {body}",
                row.case
            ),
        }

        // The round trip: an accepted row reads back, a refused one left nothing.
        let listed = waf.ok("ListWebACLs", json!({ "Scope": SCOPE })).await;
        let present = names_listed(&listed, "WebACLs").contains(&name);
        assert_eq!(
            present,
            row.refused_as.is_none(),
            "case {:?}: expected the name to be stored: {}, got {listed}",
            row.case,
            row.refused_as.is_none()
        );
    }
}

/// `UpdateWebACL` makes the same judgement before it writes: a refused update leaves the
/// stored document and its `LockToken` exactly as they were, and an accepted one replaces
/// the document and rotates the token.
#[tokio::test]
async fn an_update_refuses_a_one_of_that_names_none_or_several_and_writes_nothing() {
    let waf = Waf::new();
    let (_group_id, group_arn, _group_lock) =
        create_rule_group(&waf, SCOPE, "one-of-update-group", 100, json!([])).await;
    let name = "one-of-update-acl";
    let (id, _arn, created_lock) = create_web_acl(
        &waf,
        SCOPE,
        name,
        json!([rule_with("kept", 7, &[("Action", json!({ "Block": {} }))])]),
    )
    .await;

    let mut lock = created_lock;
    for row in rows(&group_arn) {
        let request = json!({
            "Name": name,
            "Scope": SCOPE,
            "Id": &id,
            "LockToken": &lock,
            "DefaultAction": row.default_action,
            "VisibilityConfig": visibility(name),
            "Rules": row.rules,
        });
        let (status, body) = waf.call("UpdateWebACL", request).await;
        let held = waf
            .ok(
                "GetWebACL",
                json!({ "Name": name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        match row.refused_as {
            Some((field, parameter)) => {
                assert_eq!(
                    status, 400,
                    "case {:?}: expected the one-of refusal, got status {status} and body \
                     {body}",
                    row.case
                );
                assert_invalid_parameter(&body, field, parameter, row.case);
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
                    "case {:?}: the document names exactly one of every one-of, so the \
                     update lands; got status {status} and body {body}",
                    row.case
                );
                let rotated = body["NextLockToken"]
                    .as_str()
                    .expect("an accepted update answers a NextLockToken")
                    .to_string();
                assert_ne!(
                    rotated, lock,
                    "case {:?}: an accepted update must rotate the LockToken, got {body}",
                    row.case
                );
                assert_eq!(
                    held["LockToken"].as_str(),
                    Some(rotated.as_str()),
                    "case {:?}: the stored token must be the one the update answered, got \
                     {held}",
                    row.case
                );
                lock = rotated;
                // The accepted document is what a later refused row is checked against,
                // so put the kept rule back before the next row runs.
                let restore = waf
                    .ok(
                        "UpdateWebACL",
                        json!({
                            "Name": name,
                            "Scope": SCOPE,
                            "Id": &id,
                            "LockToken": &lock,
                            "DefaultAction": { "Allow": {} },
                            "VisibilityConfig": visibility(name),
                            "Rules": [rule_with("kept", 7, &[("Action", json!({ "Block": {} }))])],
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

/// One row of the rule-group table: the rule to save, and the setting WAF reports it at
/// fault in.
struct GroupRow {
    case: &'static str,
    rule: Value,
    refused_as: Option<(&'static str, &'static str)>,
}

/// A rule is a rule wherever it is saved: `CreateRuleGroup` makes the same judgement about
/// the action settings of each of its own rules, and a refused create leaves no rule group
/// behind.
#[tokio::test]
async fn a_rule_group_save_refuses_the_same_rule_action_one_ofs() {
    let waf = Waf::new();

    let rows = vec![
        GroupRow {
            case: "a rule setting neither Action nor OverrideAction",
            rule: rule_with("a", 0, &[]),
            refused_as: Some((FIELD_RULE, "Rule")),
        },
        GroupRow {
            case: "a rule setting both Action and OverrideAction",
            rule: rule_with(
                "a",
                0,
                &[
                    ("Action", json!({ "Block": {} })),
                    ("OverrideAction", json!({ "None": {} })),
                ],
            ),
            refused_as: Some((FIELD_RULE, "Rule")),
        },
        GroupRow {
            case: "a rule Action naming none of its alternatives",
            rule: rule_with("a", 0, &[("Action", json!({}))]),
            refused_as: Some((FIELD_RULE_ACTION, "RuleAction")),
        },
        GroupRow {
            case: "a rule Action naming Block alone",
            rule: rule_with("a", 0, &[("Action", json!({ "Block": {} }))]),
            refused_as: None,
        },
    ];

    for (position, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let name = format!("one-of-group-{position}");
        let request = json!({
            "Name": &name,
            "Scope": SCOPE,
            "Capacity": 100,
            "VisibilityConfig": visibility(&name),
            "Rules": [row.rule],
        });
        let (status, body) = waf.call("CreateRuleGroup", request).await;
        match row.refused_as {
            Some((field, parameter)) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the one-of refusal, got status {status} and \
                     body {body}"
                );
                assert_invalid_parameter(&body, field, parameter, case);
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: the rule names exactly one action, so it is stored; got \
                 status {status} and body {body}"
            ),
        }
        let listed = waf.ok("ListRuleGroups", json!({ "Scope": SCOPE })).await;
        assert_eq!(
            names_listed(&listed, "RuleGroups").contains(&name),
            row.refused_as.is_none(),
            "case {case:?}: expected the name to be stored: {}, got {listed}",
            row.refused_as.is_none()
        );
    }
}
