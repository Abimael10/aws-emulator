// SPDX-License-Identifier: CC0-1.0

//! The preconditions a SAVED rule set has to satisfy, end to end over the wire.
//!
//! The rule asserted here: WAF resolves a web ACL's rule set as a WHOLE before it
//! writes it, and refuses the save — with the modelled `WAFInvalidParameterException`
//! (HTTP 400) naming the settings at fault, the value the request sent and WAF's
//! reason for it — when
//!
//! * two rules of one list claim the same `Name`, which is the identity every
//!   override, metric and sample resolves a rule by;
//! * two rules claim the same `Priority`, which IS the evaluation order ("the
//!   priorities don't need to be consecutive, but they must all be different",
//!   `Rule$Priority`);
//! * a `CustomResponse` references a body key the document's own
//!   `CustomResponseBodies` map does not define ("you first define the response body
//!   key and value in the `CustomResponseBodies` setting … then … you reference the
//!   response body using this key", `CustomResponse$CustomResponseBodyKey`);
//! * a `RuleActionOverride` on a CUSTOMER-OWNED rule-group reference names a rule
//!   that group does not declare — while the same unknown name on a MANAGED group's
//!   reference is accepted, the asymmetry `RuleActionOverride`'s own doc states;
//! * the rules price above the per-web-ACL web ACL capacity unit ceiling.
//!
//! And the preconditions ONE statement of that list has to satisfy on its own — which
//! every operation that takes a rule list judges, the four that SAVE one and
//! `CheckCapacity`, which only prices one:
//!
//! * a `RateBasedStatement` is accepted only as a rule's own top-level `Statement`,
//!   never nested ("you can use any statement that can be nested",
//!   `NotStatement$Statement`, which a rate-based statement is not);
//! * the MONETIZATION settings of the document, which `MonetizeAction` states in one
//!   place: the action is "available only for web ACLs associated with Amazon
//!   CloudFront distributions", "you must configure a `MonetizationConfig` on the web
//!   ACL or rule group before adding rules that use this action", and "you cannot use
//!   the Monetize action for rate-based rules";
//! * a `RateBasedStatement$EvaluationWindowSec` names one of the four settings the
//!   member enumerates;
//! * `PreParseTextTransformations` are applied only to a `FieldToMatch` they are
//!   defined for, and what they cost is charged to the rule set's capacity.
//!
//! A refused save writes nothing: no web ACL appears in `ListWebACLs` after a refused
//! create, and after a refused update the stored rules, bodies and `LockToken` are
//! exactly what they were.
//!
//! The rows span the class rather than one recorded request: for each precondition,
//! several shapes of violation (a repeated name at two positions, a repeated priority
//! at zero and above it, a reference from a rule action and from the web ACL's
//! `DefaultAction`, a name differing only in case) and an ACCEPTED control that
//! differs from the refused row only in the one thing the rule is about. The capacity
//! rows are driven at the published ceiling, one WCU past it and far past it, with the
//! price never written out: it is read from `CheckCapacity`, the report
//! `WebACL$Capacity` is derived with.
//!
//! Every call goes through the router's front door, the harness in
//! [`crate::web_acl_lifecycle`], so what these tests observe is what an SDK observes.

use serde_json::{json, Value};
use service_com_amazonaws_wafv2_awswaf_20190729::state::MAX_WEB_ACL_CAPACITY;

use crate::managed_rule_groups::managed_statement;
use crate::regex_pattern_set_lifecycle::create_regex_pattern_set;
use crate::rule_group_lifecycle::{byte_match_statement, create_rule_group};
use crate::web_acl_lifecycle::{base64_of, sqli_statement, visibility, xss_statement, Waf};

/// The scope the rows are saved in. A scope is only the namespace a save lands in,
/// and the preconditions are about the rule set itself, so one namespace carries the
/// table; `crate::web_acl_lifecycle` covers the scopes.
const SCOPE: &str = "REGIONAL";

/// The `ParameterExceptionField` values WAF names for the settings these refusals
/// are found in (`smithy_query(query="shape",shape="ParameterExceptionField")` lists
/// `RULE`, `CUSTOM_RESPONSE`, `RULE_GROUP_REFERENCE_STATEMENT` and `WEB_ACL`).
/// Written out here rather than imported, so the table does not restate the code
/// under test.
const FIELD_RULE: &str = "RULE";
const FIELD_CUSTOM_RESPONSE: &str = "CUSTOM_RESPONSE";
const FIELD_RULE_GROUP_REFERENCE_STATEMENT: &str = "RULE_GROUP_REFERENCE_STATEMENT";
const FIELD_WEB_ACL: &str = "WEB_ACL";

/// The `ErrorReason` sentences WAF carries for these refusals. See [`FIELD_RULE`].
const DUPLICATE_REASON: &str = "You have duplicated some of the information in the parameter.";
const PRIORITY_REASON: &str = "You have a duplicate priority. Priorities must be unique.";
const REFERENCE_REASON: &str = "AWS WAF couldn't find a resource that a parameter references.";
const CAPACITY_REASON: &str = "You exceeded the capacity limit for a rule group or web ACL.";

/// The rule the customer-owned rule group of these rows declares, and a name it does
/// not declare.
const DECLARED_RULE: &str = "deny-upload";

/// The error code of a refusal, as its `__type` carries it.
fn error_code(body: &Value) -> &str {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// A rule at `priority` whose statement is `statement` and whose action is `action`.
fn acting_rule(name: &str, priority: i64, statement: Value, action: Value) -> Value {
    json!({
        "Name": name,
        "Priority": priority,
        "Statement": statement,
        "Action": action,
        "VisibilityConfig": visibility(name),
    })
}

/// A blocking rule at `priority`.
fn blocking_rule(name: &str, priority: i64, statement: Value) -> Value {
    acting_rule(name, priority, statement, json!({ "Block": {} }))
}

/// A rule whose top-level statement RUNS a rule group, which carries an
/// `OverrideAction` rather than an `Action` ("you can only use this for rule
/// statements that reference a rule group", `Rule$OverrideAction`).
fn group_running_rule(name: &str, priority: i64, statement: Value) -> Value {
    json!({
        "Name": name,
        "Priority": priority,
        "Statement": statement,
        "OverrideAction": { "None": {} },
        "VisibilityConfig": visibility(name),
    })
}

/// A `Block` action serving the custom response body `key`.
fn block_serving(key: &str) -> Value {
    json!({
        "Block": {
            "CustomResponse": { "ResponseCode": 403, "CustomResponseBodyKey": key }
        }
    })
}

/// A `CustomResponseBodies` map defining exactly `keys`.
fn bodies_defining(keys: &[&str]) -> Value {
    let mut map = serde_json::Map::new();
    for key in keys {
        map.insert(
            (*key).to_string(),
            json!({ "ContentType": "TEXT_HTML", "Content": "<html>blocked</html>" }),
        );
    }
    Value::Object(map)
}

/// A `RuleGroupReferenceStatement` on `arn` overriding the action of each of
/// `overridden`.
fn reference_overriding(arn: &str, overridden: &[&str]) -> Value {
    json!({
        "RuleGroupReferenceStatement": {
            "ARN": arn,
            "RuleActionOverrides": overridden
                .iter()
                .map(|name| json!({ "Name": name, "ActionToUse": { "Count": {} } }))
                .collect::<Vec<Value>>(),
        }
    })
}

/// A `ManagedRuleGroupStatement` overriding the action of each of `overridden`.
fn managed_overriding(group: &str, overridden: &[&str]) -> Value {
    let mut statement = managed_statement("AWS", group);
    statement["ManagedRuleGroupStatement"]["RuleActionOverrides"] = overridden
        .iter()
        .map(|name| json!({ "Name": name, "ActionToUse": { "Count": {} } }))
        .collect();
    statement
}

/// One rule set, with the document's other members, as a save carries it.
#[derive(Clone)]
struct RuleSet {
    case: String,
    default_action: Value,
    rules: Value,
    bodies: Option<Value>,
}

impl RuleSet {
    /// A rule set with the plain allowing default action and no custom response
    /// bodies.
    fn of(case: impl Into<String>, rules: Value) -> Self {
        Self {
            case: case.into(),
            default_action: json!({ "Allow": {} }),
            rules,
            bodies: None,
        }
    }

    fn with_bodies(mut self, bodies: Value) -> Self {
        self.bodies = Some(bodies);
        self
    }

    fn with_default_action(mut self, default_action: Value) -> Self {
        self.default_action = default_action;
        self
    }

    /// A `CreateWebACL` body for this rule set under `name`.
    fn create_body(&self, name: &str) -> Value {
        let mut body = json!({
            "Name": name,
            "Scope": SCOPE,
            "DefaultAction": self.default_action,
            "VisibilityConfig": visibility(name),
            "Rules": self.rules,
        });
        if let Some(bodies) = &self.bodies {
            body["CustomResponseBodies"] = bodies.clone();
        }
        body
    }

    /// An `UpdateWebACL` body for this rule set on `(name, id, lock_token)`.
    fn update_body(&self, name: &str, id: &str, lock_token: &str) -> Value {
        let mut body = self.create_body(name);
        body["Id"] = json!(id);
        body["LockToken"] = json!(lock_token);
        body
    }
}

/// One rule set a save must refuse, and the refusal it must answer.
struct Refusal {
    set: RuleSet,
    /// `WAFInvalidParameterException$Field`.
    field: &'static str,
    /// `WAFInvalidParameterException$Parameter` — the value the request sent.
    parameter: String,
    /// `WAFInvalidParameterException$Reason`.
    reason: &'static str,
}

/// Assert that `body` is the invalid-parameter refusal `expected` describes.
fn assert_refusal(body: &Value, expected: &Refusal) {
    let case = &expected.set.case;
    assert_eq!(
        error_code(body),
        "WAFInvalidParameterException",
        "case {case:?}: a rule set WAF will not resolve is an invalid parameter, got {body}"
    );
    assert_eq!(
        (
            body["Field"].as_str(),
            body["Parameter"].as_str(),
            body["Reason"].as_str()
        ),
        (
            Some(expected.field),
            Some(expected.parameter.as_str()),
            Some(expected.reason)
        ),
        "case {case:?}: expected the refusal to name field {:?}, parameter {:?} and \
         reason {:?}, got {body}",
        expected.field,
        expected.parameter,
        expected.reason
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        format!(
            "Error reason: {}, field: {}, parameter: {}",
            expected.reason, expected.field, expected.parameter
        ),
        "case {case:?}: the message must be composed from the three members, got {body}"
    );
}

/// Every rule set the save path must refuse, with the refusal each earns.
///
/// `group_arn` is a customer-owned rule group declaring exactly [`DECLARED_RULE`].
fn refusals(group_arn: &str) -> Vec<Refusal> {
    let dangling = "undefined-body-key";
    let defined = "blocked-html";
    vec![
        // Two rules claiming one name, at the two positions a repeat can sit in.
        Refusal {
            set: RuleSet::of(
                "two rules sharing a name, at distinct priorities",
                json!([
                    blocking_rule("dup-name", 0, sqli_statement()),
                    blocking_rule("dup-name", 1, xss_statement()),
                ]),
            ),
            field: FIELD_RULE,
            parameter: "dup-name".to_string(),
            reason: DUPLICATE_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "the first and last of three rules share a name",
                json!([
                    blocking_rule("repeated", 0, sqli_statement()),
                    blocking_rule("between", 1, xss_statement()),
                    blocking_rule("repeated", 2, byte_match_statement()),
                ]),
            ),
            field: FIELD_RULE,
            parameter: "repeated".to_string(),
            reason: DUPLICATE_REASON,
        },
        // Two rules claiming one priority: at zero, above zero, and among three.
        Refusal {
            set: RuleSet::of(
                "two rules at priority 0",
                json!([
                    blocking_rule("first", 0, sqli_statement()),
                    blocking_rule("second", 0, xss_statement()),
                ]),
            ),
            field: FIELD_RULE,
            parameter: "0".to_string(),
            reason: PRIORITY_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "two rules at priority 7",
                json!([
                    blocking_rule("first", 7, sqli_statement()),
                    blocking_rule("second", 7, xss_statement()),
                ]),
            ),
            field: FIELD_RULE,
            parameter: "7".to_string(),
            reason: PRIORITY_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "the last two of three rules share priority 5",
                json!([
                    blocking_rule("first", 0, sqli_statement()),
                    blocking_rule("second", 5, xss_statement()),
                    blocking_rule("third", 5, byte_match_statement()),
                ]),
            ),
            field: FIELD_RULE,
            parameter: "5".to_string(),
            reason: PRIORITY_REASON,
        },
        // A custom-response reference that resolves to nothing, from each position
        // one can be written in and in each way it can fail to resolve.
        Refusal {
            set: RuleSet::of(
                "a rule action serving a body key the map does not define",
                json!([acting_rule(
                    "blocks",
                    0,
                    sqli_statement(),
                    block_serving(dangling)
                )]),
            )
            .with_bodies(bodies_defining(&[defined])),
            field: FIELD_CUSTOM_RESPONSE,
            parameter: dangling.to_string(),
            reason: REFERENCE_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "the bodies map is left out from under a live reference",
                json!([acting_rule(
                    "blocks",
                    0,
                    sqli_statement(),
                    block_serving(defined)
                )]),
            ),
            field: FIELD_CUSTOM_RESPONSE,
            parameter: defined.to_string(),
            reason: REFERENCE_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "the web ACL's own DefaultAction serves an undefined key",
                json!([blocking_rule("inspects", 0, sqli_statement())]),
            )
            .with_bodies(bodies_defining(&[defined]))
            .with_default_action(block_serving(dangling)),
            field: FIELD_CUSTOM_RESPONSE,
            parameter: dangling.to_string(),
            reason: REFERENCE_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "a body key differing from the defined one only in case",
                json!([acting_rule(
                    "blocks",
                    0,
                    sqli_statement(),
                    block_serving("Blocked-HTML")
                )]),
            )
            .with_bodies(bodies_defining(&[defined])),
            field: FIELD_CUSTOM_RESPONSE,
            parameter: "Blocked-HTML".to_string(),
            reason: REFERENCE_REASON,
        },
        // An override naming no rule of the customer-owned group it overrides.
        Refusal {
            set: RuleSet::of(
                "an override naming a rule the referenced group does not declare",
                json!([group_running_rule(
                    "runs-group",
                    0,
                    reference_overriding(group_arn, &["no-such-rule"])
                )]),
            ),
            field: FIELD_RULE_GROUP_REFERENCE_STATEMENT,
            parameter: "no-such-rule".to_string(),
            reason: REFERENCE_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "an override differing from a declared rule only in case",
                json!([group_running_rule(
                    "runs-group",
                    0,
                    reference_overriding(group_arn, &["Deny-Upload"])
                )]),
            ),
            field: FIELD_RULE_GROUP_REFERENCE_STATEMENT,
            parameter: "Deny-Upload".to_string(),
            reason: REFERENCE_REASON,
        },
        Refusal {
            set: RuleSet::of(
                "the second of two overrides is unknown",
                json!([group_running_rule(
                    "runs-group",
                    0,
                    reference_overriding(group_arn, &[DECLARED_RULE, "also-missing"])
                )]),
            ),
            field: FIELD_RULE_GROUP_REFERENCE_STATEMENT,
            parameter: "also-missing".to_string(),
            reason: REFERENCE_REASON,
        },
    ]
}

/// Every rule set the save path must ACCEPT: for each refused row above, one that
/// differs from it only in the thing that rule is about.
fn accepted(group_arn: &str) -> Vec<RuleSet> {
    let defined = "blocked-html";
    vec![
        RuleSet::of(
            "distinct names at distinct priorities",
            json!([
                blocking_rule("first", 0, sqli_statement()),
                blocking_rule("second", 1, xss_statement()),
            ]),
        ),
        RuleSet::of(
            "names differing only in case are different names",
            json!([
                blocking_rule("Dup-Name", 0, sqli_statement()),
                blocking_rule("dup-name", 1, xss_statement()),
            ]),
        ),
        RuleSet::of(
            "distinct, non-consecutive priorities",
            json!([
                blocking_rule("first", 7, sqli_statement()),
                blocking_rule("second", 8, xss_statement()),
            ]),
        ),
        RuleSet::of(
            "a rule action and the default action both serving a defined key",
            json!([acting_rule(
                "blocks",
                0,
                sqli_statement(),
                block_serving(defined)
            )]),
        )
        .with_bodies(bodies_defining(&[defined, "too-many"]))
        .with_default_action(block_serving(defined)),
        RuleSet::of(
            "an override naming the rule the referenced group declares",
            json!([group_running_rule(
                "runs-group",
                0,
                reference_overriding(group_arn, &[DECLARED_RULE])
            )]),
        ),
        RuleSet::of(
            "an unknown override name on a MANAGED group, which WAF ignores",
            json!([group_running_rule(
                "runs-managed",
                0,
                managed_overriding("AWSManagedRulesAdminProtectionRuleSet", &["no-such-rule"])
            )]),
        ),
    ]
}

/// A customer-owned rule group declaring exactly [`DECLARED_RULE`].
async fn declaring_group(waf: &Waf, name: &str) -> String {
    let (_id, arn, _lock) = create_rule_group(
        waf,
        SCOPE,
        name,
        100,
        json!([blocking_rule(DECLARED_RULE, 0, byte_match_statement())]),
    )
    .await;
    arn
}

/// The names `ListWebACLs` reports for the scope.
async fn listed_names(waf: &Waf) -> Vec<String> {
    let listed = waf.ok("ListWebACLs", json!({ "Scope": SCOPE })).await;
    listed["WebACLs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| summary["Name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// `CreateWebACL` refuses every rule set the preconditions reject, names the value
/// at fault, and leaves no web ACL behind — while the controls beside them are saved
/// and read back verbatim.
#[tokio::test]
async fn create_web_acl_refuses_an_unresolvable_rule_set_and_stores_nothing() {
    let waf = Waf::new();
    let group_arn = declaring_group(&waf, "precondition-group").await;

    for (index, expected) in refusals(&group_arn).iter().enumerate() {
        let case = &expected.set.case;
        let name = format!("refused-acl-{index}");
        let (status, body) = waf
            .call("CreateWebACL", expected.set.create_body(&name))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the save must be refused, got status {status} and body {body}"
        );
        assert_refusal(&body, expected);
        assert!(
            body["Summary"].is_null(),
            "case {case:?}: a refused create must answer no summary, got {body}"
        );
        let listed = listed_names(&waf).await;
        assert!(
            !listed.contains(&name),
            "case {case:?}: a refused create must leave no web ACL behind, got {listed:?}"
        );
    }

    for (index, set) in accepted(&group_arn).iter().enumerate() {
        let case = &set.case;
        let name = format!("accepted-acl-{index}");
        let created = waf.ok("CreateWebACL", set.create_body(&name)).await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Rules"], set.rules,
            "case {case:?}: an accepted rule set must read back verbatim, got {got}"
        );
        if let Some(bodies) = &set.bodies {
            assert_eq!(
                got["WebACL"]["CustomResponseBodies"], *bodies,
                "case {case:?}: the bodies the reference resolves against must read \
                 back verbatim, got {got}"
            );
        }
    }
}

/// `UpdateWebACL` refuses the same rule sets, and a refused update changes nothing
/// at all: the stored rules, the stored bodies and the `LockToken` are what they
/// were, and the token the caller holds still works.
#[tokio::test]
async fn update_web_acl_refuses_the_same_rule_sets_and_changes_nothing() {
    let waf = Waf::new();
    let group_arn = declaring_group(&waf, "precondition-group").await;
    let name = "updated-acl";
    let defined = "blocked-html";
    // The stored starting point: a rule set that satisfies every precondition, with
    // a live custom-response reference and a resolvable override.
    let before = RuleSet::of(
        "the stored rule set",
        json!([
            acting_rule("blocks", 0, sqli_statement(), block_serving(defined)),
            group_running_rule(
                "runs-group",
                1,
                reference_overriding(&group_arn, &[DECLARED_RULE])
            ),
        ]),
    )
    .with_bodies(bodies_defining(&[defined]));
    let created = waf.ok("CreateWebACL", before.create_body(name)).await;
    let id = created["Summary"]["Id"]
        .as_str()
        .expect("Summary.Id")
        .to_string();
    let lock_token = created["Summary"]["LockToken"]
        .as_str()
        .expect("Summary.LockToken")
        .to_string();

    for expected in refusals(&group_arn).iter() {
        let case = &expected.set.case;
        let (status, body) = waf
            .call(
                "UpdateWebACL",
                expected.set.update_body(name, &id, &lock_token),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the update must be refused, got status {status} and body {body}"
        );
        assert_refusal(&body, expected);
        assert!(
            body["NextLockToken"].is_null(),
            "case {case:?}: a refused update must answer no lock token, got {body}"
        );

        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        assert_eq!(
            (
                got["WebACL"]["Rules"].clone(),
                got["WebACL"]["CustomResponseBodies"].clone(),
                got["LockToken"].as_str()
            ),
            (
                before.rules.clone(),
                before.bodies.clone().unwrap_or(Value::Null),
                Some(lock_token.as_str())
            ),
            "case {case:?}: a refused update must change nothing at all, got {got}"
        );
    }

    // The token the caller still holds works, so nothing was consumed either.
    let after = RuleSet::of(
        "a resolvable replacement",
        json!([blocking_rule("replacement", 0, xss_statement())]),
    );
    let updated = waf
        .ok("UpdateWebACL", after.update_body(name, &id, &lock_token))
        .await;
    assert!(
        updated["NextLockToken"].as_str().is_some_and(|next| next != lock_token),
        "case \"a resolvable replacement\": an accepted update must rotate the lock \
         token, got {updated}"
    );
}

/// An override naming no rule of a CUSTOMER-OWNED group is refused, while the same
/// name on a MANAGED group's reference is accepted and ignored: "with managed rule
/// groups, WAF silently ignores any override that uses an invalid rule name. With
/// customer-owned rule groups, invalid rule names in your overrides will cause web
/// ACL updates to fail" (`RuleActionOverride`'s own doc).
///
/// The accepted managed row is read back, so the override WAF ignores is still
/// STORED rather than dropped; the refused customer-owned row names the override.
#[tokio::test]
async fn an_unknown_override_name_is_refused_for_an_owned_group_and_ignored_for_a_managed_one() {
    let waf = Waf::new();
    let group_arn = declaring_group(&waf, "asymmetry-group").await;
    let unknown = "no-such-rule";

    // Two managed groups, so the acceptance is not a property of one of them.
    for (index, group) in [
        "AWSManagedRulesAdminProtectionRuleSet",
        "AWSManagedRulesAmazonIpReputationList",
    ]
    .iter()
    .enumerate()
    {
        let case = format!("an unknown override on the managed group {group:?}");
        let name = format!("managed-override-{index}");
        let statement = managed_overriding(group, &[unknown]);
        let rules = json!([group_running_rule("runs-managed", 0, statement)]);
        let created = waf
            .ok(
                "CreateWebACL",
                json!({
                    "Name": &name,
                    "Scope": SCOPE,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(&name),
                    "Rules": &rules,
                }),
            )
            .await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Rules"], rules,
            "case {case:?}: an override WAF ignores must still be stored as sent, got {got}"
        );
    }

    // The same unknown name against the customer-owned group is refused, whichever
    // save path writes it.
    let set = RuleSet::of(
        "an unknown override on a customer-owned group",
        json!([group_running_rule(
            "runs-group",
            0,
            reference_overriding(&group_arn, &[unknown])
        )]),
    );
    let expected = Refusal {
        set: set.clone(),
        field: FIELD_RULE_GROUP_REFERENCE_STATEMENT,
        parameter: unknown.to_string(),
        reason: REFERENCE_REASON,
    };
    let (status, body) = waf
        .call("CreateWebACL", set.create_body("owned-override"))
        .await;
    assert_eq!(
        status, 400,
        "case {:?}: CreateWebACL must refuse it, got status {status} and body {body}",
        set.case
    );
    assert_refusal(&body, &expected);
}

/// The rules of a web ACL may price up to the published web ACL capacity unit
/// ceiling and no further: a save whose rules cost more is refused as the invalid
/// parameter, naming the price those rules cost — the figure `CheckCapacity`
/// publishes for them — and nothing is stored.
///
/// One row at the ceiling, one WCU past it, and far past it, each reached through the
/// capacity a referenced rule group declares (`RuleGroup$Capacity` is stored as
/// declared), plus a row that reaches the same total through two references and one
/// that adds a statement of its own on top of a reference at the ceiling.
#[tokio::test]
async fn the_rules_of_a_web_acl_may_price_up_to_the_published_ceiling_and_no_further() {
    let waf = Waf::new();
    let reference = |arn: &str| json!({ "RuleGroupReferenceStatement": { "ARN": arn } });
    let mut declaring: Vec<String> = Vec::new();
    for capacity in [
        MAX_WEB_ACL_CAPACITY,
        MAX_WEB_ACL_CAPACITY + 1,
        MAX_WEB_ACL_CAPACITY / 2,
        MAX_WEB_ACL_CAPACITY * 3,
    ] {
        let (_id, arn, _lock) = create_rule_group(
            &waf,
            SCOPE,
            &format!("ceiling-group-{capacity}"),
            capacity,
            json!([]),
        )
        .await;
        declaring.push(arn);
    }
    let (at_ceiling, one_past, half, far_past) = (
        declaring[0].clone(),
        declaring[1].clone(),
        declaring[2].clone(),
        declaring[3].clone(),
    );

    // `(case, rules, saved)`.
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "one reference priced exactly at the ceiling",
            json!([group_running_rule("rg", 0, reference(&at_ceiling))]),
            true,
        ),
        (
            "two references summing to exactly the ceiling",
            json!([
                group_running_rule("rg-a", 0, reference(&half)),
                group_running_rule("rg-b", 1, reference(&half)),
            ]),
            true,
        ),
        (
            "one reference priced one WCU past the ceiling",
            json!([group_running_rule("rg", 0, reference(&one_past))]),
            false,
        ),
        (
            "a reference at the ceiling plus a statement of its own",
            json!([
                group_running_rule("rg", 0, reference(&at_ceiling)),
                blocking_rule("sqli", 1, sqli_statement()),
            ]),
            false,
        ),
        (
            "one reference priced far past the ceiling",
            json!([group_running_rule("rg", 0, reference(&far_past))]),
            false,
        ),
    ];

    for (index, (case, rules, saved)) in rows.iter().enumerate() {
        // The price is the one the service PUBLISHES for these rules, never a
        // figure written out here.
        let priced = waf
            .ok(
                "CheckCapacity",
                json!({ "Scope": SCOPE, "Rules": rules.clone() }),
            )
            .await;
        let price = priced["Capacity"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: CheckCapacity must price it, got {priced}"));
        assert_eq!(
            price > MAX_WEB_ACL_CAPACITY,
            !*saved,
            "case {case:?}: the row must sit on the side of the ceiling it claims, \
             priced {price} against {MAX_WEB_ACL_CAPACITY}"
        );

        let name = format!("ceiling-acl-{index}");
        let set = RuleSet::of(*case, rules.clone());
        if *saved {
            let created = waf.ok("CreateWebACL", set.create_body(&name)).await;
            let id = created["Summary"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
                .to_string();
            let got = waf
                .ok(
                    "GetWebACL",
                    json!({ "Name": &name, "Scope": SCOPE, "Id": &id }),
                )
                .await;
            assert_eq!(
                got["WebACL"]["Capacity"].as_i64(),
                Some(price),
                "case {case:?}: a web ACL at the ceiling must store the price it was \
                 checked at, got {got}"
            );
            continue;
        }

        let (status, body) = waf.call("CreateWebACL", set.create_body(&name)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: rules above the ceiling must be refused, got status \
             {status} and body {body}"
        );
        assert_refusal(
            &body,
            &Refusal {
                set,
                field: FIELD_WEB_ACL,
                parameter: price.to_string(),
                reason: CAPACITY_REASON,
            },
        );
        let listed = listed_names(&waf).await;
        assert!(
            !listed.contains(&name),
            "case {case:?}: a refused create must leave no web ACL behind, got {listed:?}"
        );
    }

    // And an UPDATE is judged the same way, leaving the stored web ACL untouched.
    let name = "ceiling-updated-acl";
    let before = RuleSet::of(
        "a web ACL below the ceiling",
        json!([blocking_rule("sqli", 0, sqli_statement())]),
    );
    let created = waf.ok("CreateWebACL", before.create_body(name)).await;
    let id = created["Summary"]["Id"]
        .as_str()
        .expect("Summary.Id")
        .to_string();
    let lock_token = created["Summary"]["LockToken"]
        .as_str()
        .expect("Summary.LockToken")
        .to_string();
    let over = RuleSet::of(
        "an update past the ceiling",
        json!([group_running_rule("rg", 0, reference(&one_past))]),
    );
    let (status, body) = waf
        .call("UpdateWebACL", over.update_body(name, &id, &lock_token))
        .await;
    assert_eq!(
        status, 400,
        "case {:?}: an update past the ceiling must be refused, got status {status} \
         and body {body}",
        over.case
    );
    assert_refusal(
        &body,
        &Refusal {
            set: over,
            field: FIELD_WEB_ACL,
            parameter: (MAX_WEB_ACL_CAPACITY + 1).to_string(),
            reason: CAPACITY_REASON,
        },
    );
    let got = waf
        .ok(
            "GetWebACL",
            json!({ "Name": name, "Scope": SCOPE, "Id": &id }),
        )
        .await;
    assert_eq!(
        (got["WebACL"]["Rules"].clone(), got["LockToken"].as_str()),
        (before.rules.clone(), Some(lock_token.as_str())),
        "case \"an update past the ceiling\": the stored web ACL must be untouched, got {got}"
    );
}

// ---------------------------------------------------------------------------
// The settings of ONE statement
//
// Beside the judgements above, which are about the rule list as a whole, every
// statement of that list is judged on its own settings: where a statement of its
// kind may be written, which values its enumerated settings accept, and which
// request components a setting of it is defined for. These are properties of the
// statement rather than of the document, so they are judged by every operation that
// takes a rule list — the four that SAVE one and `CheckCapacity`, which prices one.
// ---------------------------------------------------------------------------

/// The `ParameterExceptionField` values WAF names for the settings of one statement
/// (`smithy_query(query="shape",shape="ParameterExceptionField")` lists
/// `RATE_BASED_STATEMENT` and `PRE_PARSE_TEXT_TRANSFORMATION`). See [`FIELD_RULE`].
const FIELD_RATE_BASED_STATEMENT: &str = "RATE_BASED_STATEMENT";
const FIELD_PRE_PARSE_TEXT_TRANSFORMATION: &str = "PRE_PARSE_TEXT_TRANSFORMATION";

/// The `ErrorReason` sentences WAF carries for these refusals. See [`FIELD_RULE`].
const NESTED_STATEMENT_REASON: &str = "A reference in your rule statement is not valid.";
const NOT_VALID_REASON: &str = "Your request in not valid.";
const PRE_PARSE_REASON: &str = "PreParseTextTransformations is only supported for \
     SingleQueryArgument and AllQueryArguments field to match types.";

/// The member name a refusal about a rate-based statement, or about a pre-parse text
/// transformation, carries in `Parameter`.
const PARAMETER_RATE_BASED_STATEMENT: &str = "RateBasedStatement";
const PARAMETER_PRE_PARSE_TEXT_TRANSFORMATION: &str = "PreParseTextTransformation";

/// Every evaluation window a rate-based statement may look back over: "Valid settings
/// are 60, 120, 300, and 600" (`RateBasedStatement$EvaluationWindowSec`), its lowest
/// and its highest included. Written out here rather than imported, so the table pins
/// the set and not merely its reuse.
const PUBLISHED_WINDOWS_SEC: [i64; 4] = [60, 120, 300, 600];

/// The window a statement that names none looks back over: "Default: `300`
/// (5 minutes)" (the same member's doc).
const DEFAULT_WINDOW_SEC: i64 = 300;

/// `RateBasedStatement$Limit` is `@range(min 10)`, so this is a limit every row can
/// carry.
const RATE_LIMIT: i64 = 100;

/// A `RateBasedStatement` aggregating on the request origin's address, carrying
/// `window` as its `EvaluationWindowSec` only when `window` is `Some` — so an absent
/// member is really absent on the wire.
fn rate_based_statement(window: Option<i64>) -> Value {
    let mut statement = json!({ "Limit": RATE_LIMIT, "AggregateKeyType": "IP" });
    if let Some(window) = window {
        statement["EvaluationWindowSec"] = json!(window);
    }
    json!({ "RateBasedStatement": statement })
}

/// Assert that `operation` refuses `request` with the refusal `expected` describes.
async fn assert_operation_refuses(
    waf: &Waf,
    operation: &str,
    request: Value,
    expected: &Refusal,
) {
    let case = &expected.set.case;
    let (status, body) = waf.call(operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {operation} must refuse it, got status {status} and body {body}"
    );
    assert_refusal(&body, expected);
}

/// A `RateBasedStatement` counts the requests of a whole rule over time, so it is not
/// one of the statements that "can be nested" (`NotStatement$Statement` and both
/// logical lists are documented "you can use any statement that can be nested"): WAF
/// accepts one only as a rule's OWN top-level `Statement`, and refuses a rule list
/// that nests one with the modelled `WAFInvalidParameterException` naming the
/// statement kind — whichever operation the list was handed to, and without storing
/// anything.
///
/// One row per nesting the model has (each logical operator, both operand positions,
/// two levels down, and each statement that carries a nested statement of its own),
/// driven through every operation that takes a rule list. The controls beside them
/// differ in the one thing the rule is about: the same statement as the rule's own
/// statement is saved, read back and priced, and an ordinary statement nested in the
/// same places is accepted — so what is refused is the KIND of statement, not the
/// nesting.
#[tokio::test]
async fn a_rate_based_statement_may_only_be_a_rule_s_own_top_level_statement() {
    let waf = Waf::new();
    let nested = rate_based_statement(None);

    // `(case, rules, judged by a rule group save)`. A rule group save judges a row
    // whose statements it could hold at all: a managed rule group statement may not
    // appear in a rule group's rules in the first place, so that row is a web ACL's.
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "a rate-based statement inside a NotStatement",
            json!([blocking_rule(
                "nests",
                0,
                json!({ "NotStatement": { "Statement": nested.clone() } })
            )]),
            true,
        ),
        (
            "a rate-based statement as the first operand of an AndStatement",
            json!([blocking_rule(
                "nests",
                0,
                json!({ "AndStatement": { "Statements": [nested.clone(), sqli_statement()] } })
            )]),
            true,
        ),
        (
            "a rate-based statement as the second operand of an OrStatement",
            json!([blocking_rule(
                "nests",
                0,
                json!({ "OrStatement": { "Statements": [sqli_statement(), nested.clone()] } })
            )]),
            true,
        ),
        (
            "a rate-based statement two levels down, in an AndStatement in a NotStatement",
            json!([blocking_rule(
                "nests",
                0,
                json!({
                    "NotStatement": {
                        "Statement": {
                            "AndStatement": { "Statements": [nested.clone(), xss_statement()] }
                        }
                    }
                })
            )]),
            true,
        ),
        (
            "a rate-based statement as another rate-based statement's scope-down",
            json!([blocking_rule(
                "nests",
                0,
                json!({
                    "RateBasedStatement": {
                        "Limit": RATE_LIMIT,
                        "AggregateKeyType": "IP",
                        "ScopeDownStatement": nested.clone(),
                    }
                })
            )]),
            true,
        ),
        (
            "a rate-based statement as a managed rule group statement's scope-down",
            json!([group_running_rule("nests", 0, {
                let mut statement =
                    managed_statement("AWS", "AWSManagedRulesAmazonIpReputationList");
                statement["ManagedRuleGroupStatement"]["ScopeDownStatement"] = nested.clone();
                statement
            })]),
            false,
        ),
        (
            "a rate-based statement beside an ordinary rule, nested in the second rule",
            json!([
                blocking_rule("plain", 0, sqli_statement()),
                blocking_rule(
                    "nests",
                    1,
                    json!({ "NotStatement": { "Statement": nested.clone() } })
                ),
            ]),
            true,
        ),
    ];

    // One stored web ACL every refused update is measured against.
    let stored = RuleSet::of(
        "the stored rule set",
        json!([blocking_rule("kept", 0, sqli_statement())]),
    );
    let created = waf.ok("CreateWebACL", stored.create_body("nesting-acl")).await;
    let acl_id = created["Summary"]["Id"]
        .as_str()
        .expect("Summary.Id")
        .to_string();
    let acl_lock = created["Summary"]["LockToken"]
        .as_str()
        .expect("Summary.LockToken")
        .to_string();

    for (index, (case, rules, in_rule_group)) in rows.iter().enumerate() {
        let set = RuleSet::of(*case, rules.clone());
        let expected = Refusal {
            set: set.clone(),
            field: FIELD_RATE_BASED_STATEMENT,
            parameter: PARAMETER_RATE_BASED_STATEMENT.to_string(),
            reason: NESTED_STATEMENT_REASON,
        };

        let name = format!("nested-rate-acl-{index}");
        assert_operation_refuses(&waf, "CreateWebACL", set.create_body(&name), &expected).await;
        let listed = listed_names(&waf).await;
        assert!(
            !listed.contains(&name),
            "case {case:?}: a refused create must leave no web ACL behind, got {listed:?}"
        );
        assert_operation_refuses(
            &waf,
            "UpdateWebACL",
            set.update_body("nesting-acl", &acl_id, &acl_lock),
            &expected,
        )
        .await;
        assert_operation_refuses(
            &waf,
            "CheckCapacity",
            json!({ "Scope": SCOPE, "Rules": rules.clone() }),
            &expected,
        )
        .await;
        if *in_rule_group {
            assert_operation_refuses(
                &waf,
                "CreateRuleGroup",
                json!({
                    "Name": format!("nested-rate-group-{index}"),
                    "Scope": SCOPE,
                    "Capacity": 500,
                    "VisibilityConfig": visibility("nested-rate-group"),
                    "Rules": rules.clone(),
                }),
                &expected,
            )
            .await;
        }
    }

    // The refused update changed nothing, and the token the caller holds still works.
    let held = waf
        .ok(
            "GetWebACL",
            json!({ "Name": "nesting-acl", "Scope": SCOPE, "Id": &acl_id }),
        )
        .await;
    assert_eq!(
        (held["WebACL"]["Rules"].clone(), held["LockToken"].as_str()),
        (stored.rules.clone(), Some(acl_lock.as_str())),
        "case \"the stored rule set\": a refused update must change nothing, got {held}"
    );

    // The controls: the same statement where it belongs, and an ordinary statement in
    // each of the places the rate-based one was refused in.
    let controls: Vec<(&str, Value)> = vec![
        (
            "the rate-based statement as the rule's own statement",
            json!([blocking_rule("rate", 0, rate_based_statement(None))]),
        ),
        (
            "the rate-based statement with an ordinary scope-down of its own",
            json!([blocking_rule(
                "rate",
                0,
                json!({
                    "RateBasedStatement": {
                        "Limit": RATE_LIMIT,
                        "AggregateKeyType": "IP",
                        "ScopeDownStatement": sqli_statement(),
                    }
                })
            )]),
        ),
        (
            "an ordinary statement inside a NotStatement",
            json!([blocking_rule(
                "nests",
                0,
                json!({ "NotStatement": { "Statement": sqli_statement() } })
            )]),
        ),
        (
            "an ordinary statement in an AndStatement beside a rate-based rule",
            json!([
                blocking_rule("rate", 0, rate_based_statement(Some(60))),
                blocking_rule(
                    "nests",
                    1,
                    json!({ "AndStatement": { "Statements": [sqli_statement(), xss_statement()] } })
                ),
            ]),
        ),
    ];
    for (index, (case, rules)) in controls.iter().enumerate() {
        let name = format!("accepted-rate-acl-{index}");
        let set = RuleSet::of(*case, rules.clone());
        let created = waf.ok("CreateWebACL", set.create_body(&name)).await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        // The stored rules are the submitted ones with the documented evaluation
        // window filled in wherever a rate-based statement named none.
        let mut expected_rules = rules.clone();
        for rule in expected_rules.as_array_mut().expect("a list of rules") {
            if let Some(statement) = rule["Statement"].get_mut("RateBasedStatement") {
                if statement.get("EvaluationWindowSec").is_none() {
                    statement["EvaluationWindowSec"] = json!(DEFAULT_WINDOW_SEC);
                }
            }
        }
        assert_eq!(
            got["WebACL"]["Rules"], expected_rules,
            "case {case:?}: an accepted rule set must read back as stored, got {got}"
        );
        let priced = waf
            .ok(
                "CheckCapacity",
                json!({ "Scope": SCOPE, "Rules": rules.clone() }),
            )
            .await;
        assert!(
            priced["Capacity"].as_i64().is_some_and(|price| price > 0),
            "case {case:?}: an accepted rule set must be priced, got {priced}"
        );
    }

    // And a rule group holds the same rate-based rule.
    let (_group_id, _group_arn, _group_lock) = create_rule_group(
        &waf,
        SCOPE,
        "accepted-rate-group",
        500,
        json!([blocking_rule("rate", 0, rate_based_statement(None))]),
    )
    .await;
}

/// `RateBasedStatement$EvaluationWindowSec` is "the amount of time, in seconds, that
/// WAF should include in its request counts" and enumerates what it accepts ("valid
/// settings are 60, 120, 300, and 600"), with no modelled range narrowing the numeric
/// domain — so a value inside that domain but outside the set is refused with the
/// modelled `WAFInvalidParameterException` naming the rejected value, nothing is
/// stored, and each of the four settings is saved and read back unchanged.
///
/// The rows span the published set and the values around it: every accepted setting,
/// the absent member (which reads back as the documented default), each published
/// bound and one second outside it, values far outside in both directions, and a
/// negative one — judged on the save paths and on the capacity report alike.
#[tokio::test]
async fn a_rate_based_statement_s_evaluation_window_must_be_one_of_the_published_settings() {
    let waf = Waf::new();

    // Every accepted window, plus the absent member: `(case, requested, stored)`.
    let accepted: Vec<(String, Option<i64>, i64)> = ::std::iter::once((
        "an absent EvaluationWindowSec".to_string(),
        None,
        DEFAULT_WINDOW_SEC,
    ))
    .chain(PUBLISHED_WINDOWS_SEC.iter().map(|window| {
        (
            format!("EvaluationWindowSec {window}"),
            Some(*window),
            *window,
        )
    }))
    .collect();

    for (index, (case, requested, stored)) in accepted.iter().enumerate() {
        let name = format!("window-acl-{index}");
        let rules = json!([blocking_rule("rate", 0, rate_based_statement(*requested))]);
        let set = RuleSet::of(case.clone(), rules.clone());
        let created = waf.ok("CreateWebACL", set.create_body(&name)).await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Rules"][0]["Statement"]["RateBasedStatement"]["EvaluationWindowSec"]
                .as_i64(),
            Some(*stored),
            "case {case:?}: the stored window must be {stored}, got {got}"
        );
        // A rule group holds the same rule, so the acceptance is not the web ACL
        // path's alone.
        let (_id, _arn, _lock) = create_rule_group(
            &waf,
            SCOPE,
            &format!("window-group-{index}"),
            500,
            rules.clone(),
        )
        .await;
    }

    // Every window outside the published set: each bound's neighbour, the values the
    // domain holds between the settings, far outside, and below zero.
    let refused: [i64; 10] = [-600, -1, 1, 30, 59, 61, 90, 299, 601, 86_400];
    for (index, window) in refused.iter().enumerate() {
        let case = format!("EvaluationWindowSec {window}");
        let rules = json!([blocking_rule("rate", 0, rate_based_statement(Some(*window)))]);
        let set = RuleSet::of(case.clone(), rules.clone());
        let expected = Refusal {
            set: set.clone(),
            field: FIELD_RATE_BASED_STATEMENT,
            parameter: window.to_string(),
            reason: NOT_VALID_REASON,
        };

        let name = format!("bad-window-acl-{index}");
        assert_operation_refuses(&waf, "CreateWebACL", set.create_body(&name), &expected).await;
        let listed = listed_names(&waf).await;
        assert!(
            !listed.contains(&name),
            "case {case:?}: a refused create must leave no web ACL behind, got {listed:?}"
        );
        assert_operation_refuses(
            &waf,
            "CheckCapacity",
            json!({ "Scope": SCOPE, "Rules": rules.clone() }),
            &expected,
        )
        .await;
        assert_operation_refuses(
            &waf,
            "CreateRuleGroup",
            json!({
                "Name": format!("bad-window-group-{index}"),
                "Scope": SCOPE,
                "Capacity": 500,
                "VisibilityConfig": visibility("bad-window-group"),
                "Rules": rules.clone(),
            }),
            &expected,
        )
        .await;
    }
}

/// The base WCUs a `CONTAINS` byte match costs: the price this service publishes for
/// a statement that has to scan the component it inspects. Written out here rather
/// than imported, so the table pins the arithmetic and not merely its reuse.
const BYTE_MATCH_SCANNING_WCU: i64 = 10;

/// The WCUs inspecting EVERY value of a multi-valued component adds.
const MULTI_VALUED_COMPONENT_WCU: i64 = 10;

/// The WCUs one transformation of a request component costs, and what applying
/// pre-parse transformations at all costs on top: a statement that normalizes the raw
/// query string makes WAF parse that component out of text it did not arrive as.
const TRANSFORMATION_WCU: i64 = 10;
const PRE_PARSE_PARSE_WCU: i64 = 10;

/// Every `FieldToMatch` these rows inspect. The two pre-parse text transformations are
/// defined for — "only supported when `FieldToMatch` is `SingleQueryArgument` or
/// `AllQueryArguments`" (`smithy_query(query="shape",shape="PreParseTextTransformation")`)
/// — and a spread of the others the shape declares
/// (`smithy_query(query="shape",shape="FieldToMatch")`).
fn supported_fields() -> Vec<(&'static str, Value)> {
    vec![
        (
            "SingleQueryArgument",
            json!({ "SingleQueryArgument": { "Name": "q" } }),
        ),
        ("AllQueryArguments", json!({ "AllQueryArguments": {} })),
    ]
}

fn unsupported_fields() -> Vec<(&'static str, Value)> {
    vec![
        ("UriPath", json!({ "UriPath": {} })),
        ("QueryString", json!({ "QueryString": {} })),
        ("Body", json!({ "Body": {} })),
        ("Method", json!({ "Method": {} })),
        (
            "SingleHeader",
            json!({ "SingleHeader": { "Name": "x-probe" } }),
        ),
        ("UriFragment", json!({ "UriFragment": {} })),
    ]
}

/// A `PreParseTextTransformations` list of `types`, at ascending priorities
/// (`PreParseTextTransformation$Priority` "sets the relative processing order").
fn pre_parse(types: &[&str]) -> Value {
    Value::Array(
        types
            .iter()
            .enumerate()
            .map(|(position, kind)| json!({ "Priority": position, "Type": kind }))
            .collect(),
    )
}

/// One statement of `kind` inspecting `field_to_match`, carrying `pre_parse_list` as
/// its `PreParseTextTransformations` when that is `Some`.
///
/// Every statement kind that carries the member
/// (`smithy_query(query="shape",shape="Statement")` → the six statements with a
/// `FieldToMatch`), so the rule is asserted on all of them rather than on the one
/// kind an example happened to use. `regex_set_arn` is a stored regex pattern set, for
/// the kind that references one.
fn inspecting_statement(
    kind: &str,
    field_to_match: &Value,
    pre_parse_list: Option<&Value>,
    regex_set_arn: &str,
) -> Value {
    let transformations = json!([{ "Priority": 0, "Type": "NONE" }]);
    let mut inner = match kind {
        "ByteMatchStatement" => json!({
            "SearchString": base64_of("probe"),
            "FieldToMatch": field_to_match,
            "TextTransformations": transformations,
            "PositionalConstraint": "CONTAINS",
        }),
        "SizeConstraintStatement" => json!({
            "FieldToMatch": field_to_match,
            "ComparisonOperator": "GT",
            "Size": 1024,
            "TextTransformations": transformations,
        }),
        "RegexMatchStatement" => json!({
            "RegexString": "^/admin",
            "FieldToMatch": field_to_match,
            "TextTransformations": transformations,
        }),
        "RegexPatternSetReferenceStatement" => json!({
            "ARN": regex_set_arn,
            "FieldToMatch": field_to_match,
            "TextTransformations": transformations,
        }),
        // `SqliMatchStatement` and `XssMatchStatement` carry the component and its
        // transformations and nothing else required.
        _ => json!({
            "FieldToMatch": field_to_match,
            "TextTransformations": transformations,
        }),
    };
    if let Some(list) = pre_parse_list {
        inner["PreParseTextTransformations"] = list.clone();
    }
    json!({ kind: inner })
}

/// Pre-parse text transformations "normalize the raw query string before WAF parses
/// it into individual query arguments", so they are defined only for the components
/// parsed out of that query string: a statement that applies one to any other
/// `FieldToMatch` is refused with the modelled `WAFInvalidParameterException` naming
/// the setting, by every operation that takes a rule list, and nothing is stored.
///
/// One refused row per (statement kind that carries the member × component it is not
/// defined for), one accepted row per (statement kind × component it IS defined for)
/// — read back to show the setting is stored as sent — and an empty list, which
/// applies no transformation to anything and so is accepted wherever it is written.
#[tokio::test]
async fn pre_parse_text_transformations_are_only_accepted_on_a_query_argument_field() {
    let waf = Waf::new();
    let (_set_id, regex_set_arn, _set_lock) =
        create_regex_pattern_set(&waf, SCOPE, "pre-parse-patterns", &["^/admin"], None).await;
    let kinds = [
        "ByteMatchStatement",
        "SqliMatchStatement",
        "XssMatchStatement",
        "SizeConstraintStatement",
        "RegexMatchStatement",
        "RegexPatternSetReferenceStatement",
    ];
    let applied = pre_parse(&["URL_DECODE"]);

    let mut index = 0usize;
    for kind in kinds {
        for (component, field_to_match) in unsupported_fields() {
            let case = format!("{kind} applying a pre-parse transformation to {component}");
            let statement =
                inspecting_statement(kind, &field_to_match, Some(&applied), &regex_set_arn);
            let rules = json!([blocking_rule("inspects", 0, statement)]);
            let set = RuleSet::of(case.clone(), rules.clone());
            let expected = Refusal {
                set: set.clone(),
                field: FIELD_PRE_PARSE_TEXT_TRANSFORMATION,
                parameter: PARAMETER_PRE_PARSE_TEXT_TRANSFORMATION.to_string(),
                reason: PRE_PARSE_REASON,
            };

            let name = format!("pre-parse-acl-{index}");
            assert_operation_refuses(&waf, "CreateWebACL", set.create_body(&name), &expected)
                .await;
            let listed = listed_names(&waf).await;
            assert!(
                !listed.contains(&name),
                "case {case:?}: a refused create must leave no web ACL behind, got {listed:?}"
            );
            assert_operation_refuses(
                &waf,
                "CheckCapacity",
                json!({ "Scope": SCOPE, "Rules": rules.clone() }),
                &expected,
            )
            .await;
            assert_operation_refuses(
                &waf,
                "CreateRuleGroup",
                json!({
                    "Name": format!("pre-parse-group-{index}"),
                    "Scope": SCOPE,
                    "Capacity": 500,
                    "VisibilityConfig": visibility("pre-parse-group"),
                    "Rules": rules.clone(),
                }),
                &expected,
            )
            .await;
            index += 1;
        }
    }

    // The accepted rows: the supported components, and an EMPTY list on a component
    // the transformations are not defined for — it applies none of them.
    let mut accepted: Vec<(String, Value)> = Vec::new();
    for kind in kinds {
        for (component, field_to_match) in supported_fields() {
            accepted.push((
                format!("{kind} applying a pre-parse transformation to {component}"),
                inspecting_statement(kind, &field_to_match, Some(&applied), &regex_set_arn),
            ));
        }
        accepted.push((
            format!("{kind} carrying an empty PreParseTextTransformations on UriPath"),
            inspecting_statement(
                kind,
                &json!({ "UriPath": {} }),
                Some(&json!([])),
                &regex_set_arn,
            ),
        ));
    }
    for (index, (case, statement)) in accepted.iter().enumerate() {
        let name = format!("pre-parse-accepted-acl-{index}");
        let rules = json!([blocking_rule("inspects", 0, statement.clone())]);
        let set = RuleSet::of(case.clone(), rules.clone());
        let created = waf.ok("CreateWebACL", set.create_body(&name)).await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Rules"], rules,
            "case {case:?}: an accepted statement must read back as sent, got {got}"
        );
    }
}

/// A pre-parse text transformation is work WAF does on a request component, so it is
/// charged like every other transformation — once per (component, chain) across the
/// rule list — and the statement that applies one also pays for parsing the component
/// out of the normalized text. So a rule set carrying them prices strictly above the
/// same rule set without them, and `CheckCapacity` publishes that price.
///
/// The rows differ along what the arithmetic generalises over: no transformation, one,
/// two, a chain of nothing but the no-op, the component inspected, and two statements
/// sharing one chain. The first two are the figures the recorded corpus pins: the
/// statement alone prices 10 and the same statement applying one pre-parse
/// transformation prices 30.
#[tokio::test]
async fn pre_parse_text_transformations_are_charged_web_acl_capacity_units() {
    let waf = Waf::new();
    let (_set_id, regex_set_arn, _set_lock) =
        create_regex_pattern_set(&waf, SCOPE, "pre-parse-cost-patterns", &["^/admin"], None).await;
    let single = json!({ "SingleQueryArgument": { "Name": "q" } });
    let byte_match = |field_to_match: &Value, pre_parse_list: Option<&Value>| {
        inspecting_statement(
            "ByteMatchStatement",
            field_to_match,
            pre_parse_list,
            &regex_set_arn,
        )
    };

    let one = pre_parse(&["URL_DECODE"]);
    let two = pre_parse(&["URL_DECODE", "REPLACE_SEMICOLONS_WITH_AMPERSANDS"]);
    let none_only = pre_parse(&["NONE"]);
    let rows: Vec<(&str, Value, i64)> = vec![
        (
            "a scanning byte match over one query argument, no pre-parse transformations",
            json!([blocking_rule("a", 0, byte_match(&single, None))]),
            BYTE_MATCH_SCANNING_WCU,
        ),
        (
            "the same statement applying one pre-parse transformation",
            json!([blocking_rule("a", 0, byte_match(&single, Some(&one)))]),
            BYTE_MATCH_SCANNING_WCU + PRE_PARSE_PARSE_WCU + TRANSFORMATION_WCU,
        ),
        (
            "the same statement applying two pre-parse transformations",
            json!([blocking_rule("a", 0, byte_match(&single, Some(&two)))]),
            BYTE_MATCH_SCANNING_WCU + PRE_PARSE_PARSE_WCU + 2 * TRANSFORMATION_WCU,
        ),
        (
            "a pre-parse chain of nothing but the no-op normalizes nothing",
            json!([blocking_rule("a", 0, byte_match(&single, Some(&none_only)))]),
            BYTE_MATCH_SCANNING_WCU,
        ),
        (
            "one pre-parse transformation over every query argument",
            json!([blocking_rule(
                "a",
                0,
                byte_match(&json!({ "AllQueryArguments": {} }), Some(&one))
            )]),
            BYTE_MATCH_SCANNING_WCU
                + MULTI_VALUED_COMPONENT_WCU
                + PRE_PARSE_PARSE_WCU
                + TRANSFORMATION_WCU,
        ),
        (
            "two statements over different arguments sharing one pre-parse chain",
            json!([
                blocking_rule("a", 0, byte_match(&single, Some(&one))),
                blocking_rule(
                    "b",
                    1,
                    byte_match(
                        &json!({ "SingleQueryArgument": { "Name": "r" } }),
                        Some(&one)
                    )
                ),
            ]),
            // Each statement is matched and its component parsed, and the chain they
            // both normalize the query string with is charged once.
            2 * (BYTE_MATCH_SCANNING_WCU + PRE_PARSE_PARSE_WCU) + TRANSFORMATION_WCU,
        ),
    ];

    for (index, (case, rules, expected)) in rows.iter().enumerate() {
        let priced = waf
            .ok(
                "CheckCapacity",
                json!({ "Scope": SCOPE, "Rules": rules.clone() }),
            )
            .await;
        assert_eq!(
            priced["Capacity"].as_i64(),
            Some(*expected),
            "case {case:?}: CheckCapacity must publish {expected} WCU, got {priced}"
        );

        // The web ACL that stores those rules reports the price it was quoted.
        let name = format!("pre-parse-cost-acl-{index}");
        let set = RuleSet::of(*case, rules.clone());
        let created = waf.ok("CreateWebACL", set.create_body(&name)).await;
        let id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": SCOPE, "Id": &id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Capacity"].as_i64(),
            Some(*expected),
            "case {case:?}: the stored web ACL must report {expected} WCU, got {got}"
        );
    }
}

// ---------------------------------------------------------------------------
// The MONETIZATION settings of a saved document
// ---------------------------------------------------------------------------

/// The one namespace monetization is served in: the `Monetize` action "is available
/// only for web ACLs associated with Amazon CloudFront distributions"
/// (`smithy_query(query="shape",shape="MonetizeAction")`), which is the CLOUDFRONT
/// scope. Written out here rather than imported, so the table pins the namespace and
/// not merely its reuse; [`SCOPE`] is the regional namespace the rows that are about
/// the scope use.
const MONETIZED_SCOPE: &str = "CLOUDFRONT";

/// The `ErrorReason` WAF carries for a value the setting it was written in does not
/// support. See [`FIELD_RULE`].
const VALUE_NOT_SUPPORTED_REASON: &str = "The parameter value isn't supported.";

/// The `ParameterExceptionField` value naming a rule's `Action` (`RULE_ACTION`, as the
/// enum spells it). See [`FIELD_RULE`].
const FIELD_RULE_ACTION: &str = "RULE_ACTION";

/// The sentence WAF answers a monetization setting asked for in the regional
/// namespace with.
const MONETIZATION_NOT_AVAILABLE: &str = "Monetization is not available for regional \
     resources. Use CloudFront scope to configure monetization.";

/// How the configuration-required refusal names the two kinds of resource the setting
/// lives on, as the model spells each shape.
const RESOURCE_WEB_ACL: &str = "WebACL";
const RESOURCE_RULE_GROUP: &str = "RuleGroup";

/// The sentence WAF answers a document whose rules take the `Monetize` action and that
/// carries no `MonetizationConfig` with, for the kind of resource being saved.
fn config_required_message(resource_kind: &str) -> String {
    format!(
        "MonetizeAction requires MonetizationConfig to be configured on the \
         {resource_kind}. Provide a MonetizationConfig with payment network details."
    )
}

/// A well-formed `MonetizationConfig`: one production payment network with a wallet
/// address and a price per request, settled in the one cryptocurrency the model
/// declares (`smithy_query(query="shape",shape="Currency")` → `USDC`), in the matching
/// production currency mode.
fn monetization_config() -> Value {
    json!({
        "CryptoConfig": {
            "PaymentNetworks": [{
                "Chain": "BASE",
                "WalletAddress": "0x00000000000000000000000000000000000000a1",
                "Prices": [{ "Amount": "0.01", "Currency": "USDC" }],
            }]
        },
        "CurrencyMode": "REAL",
    })
}

/// A `Monetize` rule action, carrying `PriceMultiplier` only when `multiplier` is
/// `Some` — so an absent member is really absent on the wire.
fn monetize_action(multiplier: Option<&str>) -> Value {
    match multiplier {
        Some(multiplier) => json!({ "Monetize": { "PriceMultiplier": multiplier } }),
        None => json!({ "Monetize": {} }),
    }
}

/// The `Parameter` a refusal about that action carries: the action as the request wrote
/// it, with the literal `null` where it named no multiplier.
fn monetize_parameter(multiplier: Option<&str>) -> String {
    format!(
        "MonetizeAction(priceMultiplier={})",
        multiplier.unwrap_or("null")
    )
}

/// A rule taking the `Monetize` action over a statement that is not rate-based.
fn monetizing_rule(name: &str, priority: i64, multiplier: Option<&str>) -> Value {
    acting_rule(name, priority, sqli_statement(), monetize_action(multiplier))
}

/// A rate-based rule taking `action`.
fn rate_based_rule(name: &str, action: Value) -> Value {
    acting_rule(name, 0, rate_based_statement(None), action)
}

/// What a save must answer one row of the monetization table.
enum Monetization {
    /// Stored: the document satisfies every monetization precondition.
    Accepted,
    /// The document's rules take the action and it carries no configuration: the
    /// invalid-parameter refusal naming the kind of resource the setting belongs on.
    ConfigRequired,
    /// A rate-based rule takes the action: the invalid-parameter refusal naming the
    /// rule's action setting, with the action rendered as the `Parameter`.
    RateBasedAction(String),
    /// Monetization was asked for in a namespace that does not serve it: the
    /// invalid-OPERATION refusal.
    NotAvailableInScope,
}

/// One row: the document a save carries, the namespace it is saved in, and what WAF
/// answers it.
struct MonetizationRow {
    case: &'static str,
    scope: &'static str,
    rules: Value,
    /// `MonetizationConfig`, present only where the row supplies one.
    config: Option<Value>,
    outcome: Monetization,
    /// `true` for a row only a web ACL can carry — a rule running another rule group,
    /// which a rule group's own rules may not do.
    web_acl_only: bool,
}

impl MonetizationRow {
    /// A row in the monetized namespace carrying no configuration.
    fn of(case: &'static str, rules: Value, outcome: Monetization) -> Self {
        Self {
            case,
            scope: MONETIZED_SCOPE,
            rules,
            config: None,
            outcome,
            web_acl_only: false,
        }
    }

    fn configured(mut self) -> Self {
        self.config = Some(monetization_config());
        self
    }

    fn in_scope(mut self, scope: &'static str) -> Self {
        self.scope = scope;
        self
    }

    fn web_acl_only(mut self) -> Self {
        self.web_acl_only = true;
        self
    }
}

/// Every row of the monetization table, over the CLOUDFRONT-scoped rule group
/// `group_arn` for the row that overrides one of its rules.
///
/// The rows differ along what the rule generalises over: WHICH precondition is at
/// fault, where the `Monetize` action is written (a rule's own action, a later rule's,
/// and the action an override puts in a referenced group's place), whether it carries a
/// `PriceMultiplier` and at which end of the published 1..100 range, the namespace the
/// document is saved in, and which of the two settings asked for monetization. Beside
/// each refused row sits an accepted one differing from it in the one thing that rule
/// is about.
fn monetization_rows(group_arn: &str) -> Vec<MonetizationRow> {
    vec![
        // The configuration a document whose rules take the action must carry.
        MonetizationRow::of(
            "a Monetize rule and no MonetizationConfig",
            json!([monetizing_rule("monetizes", 0, None)]),
            Monetization::ConfigRequired,
        ),
        MonetizationRow::of(
            "a Monetize rule carrying a PriceMultiplier, and no MonetizationConfig",
            json!([monetizing_rule("monetizes", 0, Some("50"))]),
            Monetization::ConfigRequired,
        ),
        MonetizationRow::of(
            "the SECOND rule of the list takes the Monetize action, and no \
             MonetizationConfig",
            json!([
                blocking_rule("blocks", 0, xss_statement()),
                monetizing_rule("monetizes", 1, None),
            ]),
            Monetization::ConfigRequired,
        ),
        MonetizationRow::of(
            "an override puts the Monetize action in a referenced group's place, and no \
             MonetizationConfig",
            json!([group_running_rule(
                "runs-group",
                0,
                json!({
                    "RuleGroupReferenceStatement": {
                        "ARN": group_arn,
                        "RuleActionOverrides": [{
                            "Name": DECLARED_RULE,
                            "ActionToUse": monetize_action(None),
                        }],
                    }
                })
            )]),
            Monetization::ConfigRequired,
        )
        .web_acl_only(),
        // The action a rate-based rule may not take, with the configuration present so
        // the row differs from the accepted ones only in the statement under the
        // action.
        MonetizationRow::of(
            "a rate-based rule taking the Monetize action",
            json!([rate_based_rule("rate-monetizes", monetize_action(None))]),
            Monetization::RateBasedAction(monetize_parameter(None)),
        )
        .configured(),
        MonetizationRow::of(
            "a rate-based rule taking the Monetize action at the lowest multiplier",
            json!([rate_based_rule("rate-monetizes", monetize_action(Some("1")))]),
            Monetization::RateBasedAction(monetize_parameter(Some("1"))),
        )
        .configured(),
        MonetizationRow::of(
            "a rate-based rule taking the Monetize action at the highest multiplier",
            json!([rate_based_rule("rate-monetizes", monetize_action(Some("100")))]),
            Monetization::RateBasedAction(monetize_parameter(Some("100"))),
        )
        .configured(),
        MonetizationRow::of(
            "a rate-based rule taking the Monetize action, among rules that may take it",
            json!([
                monetizing_rule("monetizes", 1, Some("25")),
                rate_based_rule("rate-monetizes", monetize_action(Some("25"))),
            ]),
            Monetization::RateBasedAction(monetize_parameter(Some("25"))),
        )
        .configured(),
        // The namespace monetization is served in, asked for by each setting.
        MonetizationRow::of(
            "a Monetize rule and a MonetizationConfig in the regional namespace",
            json!([monetizing_rule("monetizes", 0, None)]),
            Monetization::NotAvailableInScope,
        )
        .configured()
        .in_scope(SCOPE),
        MonetizationRow::of(
            "a MonetizationConfig alone in the regional namespace",
            json!([blocking_rule("blocks", 0, sqli_statement())]),
            Monetization::NotAvailableInScope,
        )
        .configured()
        .in_scope(SCOPE),
        MonetizationRow::of(
            "a Monetize rule alone in the regional namespace",
            json!([monetizing_rule("monetizes", 0, None)]),
            Monetization::NotAvailableInScope,
        )
        .in_scope(SCOPE),
        // And the documents WAF stores: each differs from a refused row above in the
        // one thing that rule is about.
        MonetizationRow::of(
            "a Monetize rule with a MonetizationConfig",
            json!([monetizing_rule("monetizes", 0, None)]),
            Monetization::Accepted,
        )
        .configured(),
        MonetizationRow::of(
            "a Monetize rule carrying a PriceMultiplier, with a MonetizationConfig",
            json!([monetizing_rule("monetizes", 0, Some("100"))]),
            Monetization::Accepted,
        )
        .configured(),
        MonetizationRow::of(
            "a rate-based rule taking a terminating action other than Monetize",
            // The window is written out, because a statement that names none is stored
            // with the one the member documents and would not read back verbatim.
            json!([acting_rule(
                "rate-blocks",
                0,
                rate_based_statement(Some(DEFAULT_WINDOW_SEC)),
                json!({ "Block": {} })
            )]),
            Monetization::Accepted,
        )
        .configured(),
        MonetizationRow::of(
            "a MonetizationConfig on a document no rule of which monetizes",
            json!([blocking_rule("blocks", 0, sqli_statement())]),
            Monetization::Accepted,
        )
        .configured(),
        MonetizationRow::of(
            "neither setting, in the regional namespace",
            json!([blocking_rule("blocks", 0, sqli_statement())]),
            Monetization::Accepted,
        )
        .in_scope(SCOPE),
        MonetizationRow::of(
            "an override putting the Monetize action in a referenced group's place, \
             with a MonetizationConfig",
            json!([group_running_rule(
                "runs-group",
                0,
                json!({
                    "RuleGroupReferenceStatement": {
                        "ARN": group_arn,
                        "RuleActionOverrides": [{
                            "Name": DECLARED_RULE,
                            "ActionToUse": monetize_action(Some("2")),
                        }],
                    }
                })
            )]),
            Monetization::Accepted,
        )
        .configured()
        .web_acl_only(),
    ]
}

/// Assert that a save of `row` answering `(status, body)` answered what the row
/// expects, for a resource of kind `resource_kind`.
fn assert_monetization(status: u16, body: &Value, row: &MonetizationRow, resource_kind: &str) {
    let case = row.case;
    match &row.outcome {
        Monetization::Accepted => {
            assert_eq!(
                status, 200,
                "case {case:?}: the document satisfies every monetization precondition, \
                 so it is stored; got status {status} and body {body}"
            );
            assert!(
                !body["Summary"].is_null(),
                "case {case:?}: an accepted save answers a populated Summary, got {body}"
            );
        }
        Monetization::ConfigRequired => {
            assert_eq!(
                status, 400,
                "case {case:?}: the save must be refused, got status {status} and body \
                 {body}"
            );
            assert_eq!(
                error_code(body),
                "WAFInvalidParameterException",
                "case {case:?}: a missing MonetizationConfig is an invalid parameter, \
                 got {body}"
            );
            assert_eq!(
                body["message"].as_str().unwrap_or_default(),
                config_required_message(resource_kind),
                "case {case:?}: expected the configuration-required sentence for a \
                 {resource_kind}, got {body}"
            );
            assert_eq!(
                (
                    body["Field"].as_str(),
                    body["Parameter"].as_str(),
                    body["Reason"].as_str()
                ),
                (None, None, None),
                "case {case:?}: the setting the request left out has no value to \
                 report, so the refusal names no field, parameter or reason; got {body}"
            );
        }
        Monetization::RateBasedAction(parameter) => {
            assert_eq!(
                status, 400,
                "case {case:?}: the save must be refused, got status {status} and body \
                 {body}"
            );
            assert_eq!(
                error_code(body),
                "WAFInvalidParameterException",
                "case {case:?}: an action a rate-based rule may not take is an invalid \
                 parameter, got {body}"
            );
            assert_eq!(
                (
                    body["Field"].as_str(),
                    body["Parameter"].as_str(),
                    body["Reason"].as_str()
                ),
                (
                    Some(FIELD_RULE_ACTION),
                    Some(parameter.as_str()),
                    Some(VALUE_NOT_SUPPORTED_REASON)
                ),
                "case {case:?}: expected the refusal to name field {FIELD_RULE_ACTION:?} \
                 and parameter {parameter:?}, got {body}"
            );
            assert_eq!(
                body["message"].as_str().unwrap_or_default(),
                format!(
                    "Error reason: {VALUE_NOT_SUPPORTED_REASON}, field: \
                     {FIELD_RULE_ACTION}, parameter: {parameter}"
                ),
                "case {case:?}: the message must be composed from the three members, \
                 got {body}"
            );
        }
        Monetization::NotAvailableInScope => {
            assert_eq!(
                status, 400,
                "case {case:?}: the save must be refused, got status {status} and body \
                 {body}"
            );
            assert_eq!(
                error_code(body),
                "WAFInvalidOperationException",
                "case {case:?}: monetization outside the namespace that serves it is an \
                 invalid operation, got {body}"
            );
            assert_eq!(
                body["Message"].as_str().unwrap_or_default(),
                MONETIZATION_NOT_AVAILABLE,
                "case {case:?}: expected the unavailable-in-this-scope sentence, got \
                 {body}"
            );
        }
    }
}

/// A rule group of `scope` declaring exactly [`DECLARED_RULE`], for the rows that
/// override one of its rules.
async fn declaring_group_in(waf: &Waf, scope: &str, name: &str) -> String {
    let (_id, arn, _lock) = create_rule_group(
        waf,
        scope,
        name,
        100,
        json!([blocking_rule(DECLARED_RULE, 0, byte_match_statement())]),
    )
    .await;
    arn
}

/// The names a listing of `operation`'s resource kind reports for `scope`.
async fn names_in_scope(waf: &Waf, operation: &str, member: &str, scope: &str) -> Vec<String> {
    let listed = waf.ok(operation, json!({ "Scope": scope })).await;
    listed[member]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| summary["Name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// `CreateWebACL` judges every monetization precondition of the document it is asked
/// to store: it refuses a `Monetize` rule with no `MonetizationConfig`, a rate-based
/// rule taking that action, and either setting under the namespace that does not serve
/// monetization — each with the modelled exception and sentence that refusal carries —
/// and it STORES the configurations WAF accepts, which read back verbatim. A refused
/// create leaves no web ACL behind.
#[tokio::test]
async fn create_web_acl_judges_the_monetization_settings_of_the_document() {
    let waf = Waf::new();
    let group_arn = declaring_group_in(&waf, MONETIZED_SCOPE, "monetize-group").await;

    for (index, row) in monetization_rows(&group_arn).iter().enumerate() {
        let case = row.case;
        let name = format!("monetize-acl-{index}");
        let mut request = json!({
            "Name": &name,
            "Scope": row.scope,
            "DefaultAction": { "Allow": {} },
            "VisibilityConfig": visibility(&name),
            "Rules": row.rules,
        });
        if let Some(config) = &row.config {
            request["MonetizationConfig"] = config.clone();
        }
        let (status, body) = waf.call("CreateWebACL", request).await;
        assert_monetization(status, &body, row, RESOURCE_WEB_ACL);

        let stored = names_in_scope(&waf, "ListWebACLs", "WebACLs", row.scope)
            .await
            .contains(&name);
        let accepted = matches!(row.outcome, Monetization::Accepted);
        assert_eq!(
            stored, accepted,
            "case {case:?}: expected the name to be stored: {accepted}, got stored: \
             {stored}"
        );
        if !accepted {
            continue;
        }
        let id = body["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {body}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &name, "Scope": row.scope, "Id": &id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Rules"], row.rules,
            "case {case:?}: an accepted rule set must read back verbatim, got {got}"
        );
        assert_eq!(
            got["WebACL"].get("MonetizationConfig").cloned(),
            row.config.clone(),
            "case {case:?}: the stored MonetizationConfig must be the one the save \
             carried, got {got}"
        );
    }
}

/// `CreateRuleGroup` makes the same judgements about the same document: the setting
/// lives on both kinds of resource ("you must configure a `MonetizationConfig` on the
/// web ACL or rule group", `MonetizeAction`), and the configuration-required sentence
/// names the kind being saved.
#[tokio::test]
async fn create_rule_group_judges_the_same_monetization_settings() {
    let waf = Waf::new();
    let group_arn = declaring_group_in(&waf, MONETIZED_SCOPE, "monetize-ref-group").await;

    for (index, row) in monetization_rows(&group_arn)
        .iter()
        .filter(|row| !row.web_acl_only)
        .enumerate()
    {
        let case = row.case;
        let name = format!("monetize-group-{index}");
        let mut request = json!({
            "Name": &name,
            "Scope": row.scope,
            "Capacity": 1_000,
            "VisibilityConfig": visibility(&name),
            "Rules": row.rules,
        });
        if let Some(config) = &row.config {
            request["MonetizationConfig"] = config.clone();
        }
        let (status, body) = waf.call("CreateRuleGroup", request).await;
        assert_monetization(status, &body, row, RESOURCE_RULE_GROUP);

        let stored = names_in_scope(&waf, "ListRuleGroups", "RuleGroups", row.scope)
            .await
            .contains(&name);
        let accepted = matches!(row.outcome, Monetization::Accepted);
        assert_eq!(
            stored, accepted,
            "case {case:?}: expected the name to be stored: {accepted}, got stored: \
             {stored}"
        );
        if !accepted {
            continue;
        }
        let id = body["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {body}"))
            .to_string();
        let got = waf
            .ok(
                "GetRuleGroup",
                json!({ "Name": &name, "Scope": row.scope, "Id": &id }),
            )
            .await;
        assert_eq!(
            got["RuleGroup"].get("MonetizationConfig").cloned(),
            row.config.clone(),
            "case {case:?}: the stored MonetizationConfig must be the one the save \
             carried, got {got}"
        );
    }
}

/// An UPDATE is a save too, so it makes the same judgements before it writes: a
/// refused update leaves the stored document and its `LockToken` exactly as they were,
/// for a web ACL and for a rule group alike.
#[tokio::test]
async fn an_update_makes_the_same_monetization_judgements_and_writes_nothing() {
    let waf = Waf::new();
    let group_arn = declaring_group_in(&waf, MONETIZED_SCOPE, "monetize-update-group").await;
    let kept = blocking_rule("kept", 7, sqli_statement());

    // One stored document of each kind, in each namespace the rows save into, none of
    // them monetizing: what a refused update must leave untouched.
    let mut held: Vec<(&str, &str, String, String, String)> = Vec::new();
    for scope in [MONETIZED_SCOPE, SCOPE] {
        let acl_name = format!("monetize-update-acl-{}", scope.to_lowercase());
        let created = waf
            .ok(
                "CreateWebACL",
                json!({
                    "Name": &acl_name,
                    "Scope": scope,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(&acl_name),
                    "Rules": [kept.clone()],
                }),
            )
            .await;
        held.push((
            "WebACL",
            scope,
            acl_name,
            created["Summary"]["Id"]
                .as_str()
                .expect("Summary.Id")
                .to_string(),
            created["Summary"]["LockToken"]
                .as_str()
                .expect("Summary.LockToken")
                .to_string(),
        ));
        let group_name = format!("monetize-update-rg-{}", scope.to_lowercase());
        let (id, _arn, lock) =
            create_rule_group(&waf, scope, &group_name, 1_000, json!([kept.clone()])).await;
        held.push(("RuleGroup", scope, group_name, id, lock));
    }

    for row in monetization_rows(&group_arn)
        .iter()
        .filter(|row| !matches!(row.outcome, Monetization::Accepted))
    {
        let case = row.case;
        for (kind, scope, name, id, lock) in &held {
            if *scope != row.scope || (row.web_acl_only && *kind != RESOURCE_WEB_ACL) {
                continue;
            }
            let (operation, reader, member) = if *kind == RESOURCE_WEB_ACL {
                ("UpdateWebACL", "GetWebACL", "WebACL")
            } else {
                ("UpdateRuleGroup", "GetRuleGroup", "RuleGroup")
            };
            let mut request = json!({
                "Name": name,
                "Scope": scope,
                "Id": id,
                "LockToken": lock,
                "VisibilityConfig": visibility(name),
                "Rules": row.rules,
            });
            if *kind == RESOURCE_WEB_ACL {
                request["DefaultAction"] = json!({ "Allow": {} });
            } else {
                request["Capacity"] = json!(1_000);
            }
            if let Some(config) = &row.config {
                request["MonetizationConfig"] = config.clone();
            }
            let (status, body) = waf.call(operation, request).await;
            assert_monetization(status, &body, row, kind);

            let got = waf
                .ok(
                    reader,
                    json!({ "Name": name, "Scope": scope, "Id": id }),
                )
                .await;
            assert_eq!(
                got["LockToken"].as_str(),
                Some(lock.as_str()),
                "case {case:?}: a refused {operation} must not rotate the LockToken, \
                 got {got}"
            );
            assert_eq!(
                got[member]["Rules"][0]["Name"].as_str(),
                Some("kept"),
                "case {case:?}: a refused {operation} must leave the stored rules, got \
                 {got}"
            );
            assert!(
                got[member].get("MonetizationConfig").is_none(),
                "case {case:?}: a refused {operation} must store no MonetizationConfig, \
                 got {got}"
            );
        }
    }
}
