// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW this service answers a request it refuses before any handler
//! runs: which error class the refusal names, which body member carries its text, and how
//! that text quotes the constraint.
//!
//! Five rules, each a property of the MEMBER or of the CONSTRAINT rather than of one request,
//! so every test below is a TABLE whose rows span the class its rule covers:
//!
//! * a value in a member an operation uses to IDENTIFY a resource — the optional name/ARN
//!   pair its handler resolves, and the name a create gives the resource it mints — that
//!   cannot be a resource ARN, or cannot be a resource name, is refused as the malformed
//!   IDENTIFIER it is: the modelled `InvalidRequestException` carrying one constant per half
//!   of the pair, which is what the resolution path answers the same defect with once a value
//!   reaches a handler. A member of a create that is NOT a resource name — the required ARN
//!   reference to another resource it takes beside it — keeps the generic constraint clause;
//! * the text of a body naming one of the error shapes this model declares travels in its
//!   `Message` member, whichever layer rendered the body, because that is the member every
//!   error shape of this model declares it in; the generic constraint refusal names
//!   `ValidationException`, which this model does not declare, and keeps the framework's
//!   lowercase `message`;
//! * a requirement quoting a `@pattern` quotes the expression AWS PUBLISHES for that
//!   member's shape: the model's expression WITHOUT the anchor that opens it for the
//!   pagination token, and the model's own text — anchors and all — for every other shape;
//! * a requirement enumerating an enumerated type's value set states the set in the order
//!   this service states it, which for the generated-rules type of a domain-list rule group
//!   is not the model's declaration order;
//! * a value outside a `@range` is stated as the range violation it is, and a member the
//!   request OMITTED is stated as the requirement the zero a numeric slot defaults to breaks —
//!   so an absent number whose range excludes zero reads as that range's bound, while a member
//!   whose absence breaks no declared bound keeps the absent-member requirement.
//!
//! Every test drives the service the way a client does, through the shared [`Harness`] in
//! `crate::harness`: an awsJson1_0 `POST /` with
//! `X-Amz-Target: NetworkFirewall_20201112.<OperationName>`, so what is asserted is the WIRE
//! behaviour and not a handler's return value.

use serde_json::{json, Value};

use crate::harness::{error_code, Harness, ACCOUNT, REGION};

/// The error class this service answers a malformed resource reference with — the client
/// error every operation that resolves one declares
/// (`smithy_query(query="shape",shape="InvalidRequestException")`: client error, HTTP 400,
/// one member `Message`).
const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The error class a request that broke a constraint the model declares is refused under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The text a reference whose ARN cannot be a resource ARN is refused with, as recorded off
/// the service.
const ARN_INVALID_FORMAT: &str = "Resource ARN has an invalid format.";

/// The text a reference whose name cannot be a resource name is refused with, as recorded off
/// the service — with no closing period, where the ARN half carries one.
const NAME_INVALID_FORMAT: &str = "Resource name has an invalid format";

/// A firewall-policy ARN in this emulator's form, so a request under test is refused for the
/// member it is about rather than for this one.
fn policy_arn(name: &str) -> String {
    format!("arn:aws:network-firewall:{REGION}:{ACCOUNT}:firewall-policy/{name}")
}

/// A stateless rule group ARN in this emulator's form, for a policy document's references.
fn stateless_rule_group_arn(name: &str) -> String {
    format!("arn:aws:network-firewall:{REGION}:{ACCOUNT}:stateless-rulegroup/{name}")
}

/// The `Message` member of an error body: where every error shape this model declares carries
/// its text.
fn modelled_message(body: &Value) -> Option<&str> {
    body.get("Message").and_then(Value::as_str)
}

/// The framework's own lowercase `message` member, which the generic constraint refusal keeps
/// because `ValidationException` is not a shape this model declares.
fn rendered_message(body: &Value) -> Option<&str> {
    body.get("message").and_then(Value::as_str)
}

/// The envelope over one clause.
fn one_error(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The clause stating `requirement` at `member`, with no value.
fn clause(member: &str, requirement: &str) -> String {
    format!("Value at '{member}' failed to satisfy constraint: {requirement}")
}

/// RULE: a value in a member an operation uses to identify a resource that ALREADY EXISTS,
/// which cannot be a resource ARN or cannot be a resource name, is refused with the modelled
/// `InvalidRequestException` whose `Message` carries the one constant for that half of the
/// pair.
///
/// The rows differ along what the rule generalises over: which half of the pair carried the
/// value (ARN or name), which resource kind the pair identifies, the operation (a read, a
/// delete, an update, an associate), and WHICH bound of the member's shape the value broke —
/// its `@pattern`, its `@length` minimum (the empty string) and its `@length` maximum.
#[tokio::test]
async fn a_malformed_identifier_is_refused_as_a_bad_reference() {
    let harness = Harness::new();
    let over_the_name_maximum = "a".repeat(129);
    let over_the_arn_maximum = format!("arn:aws:{}", "n".repeat(250));
    // (case, operation, request body, the text expected)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "a read by an ARN of no partition at all",
            "DescribeFirewall",
            json!({ "FirewallArn": "not-an-arn" }),
            ARN_INVALID_FORMAT,
        ),
        (
            "a read by an empty ARN, under the ARN minimum",
            "DescribeFirewall",
            json!({ "FirewallArn": "" }),
            ARN_INVALID_FORMAT,
        ),
        (
            "a read by an ARN past the ARN maximum",
            "DescribeRuleGroup",
            json!({ "RuleGroupArn": over_the_arn_maximum }),
            ARN_INVALID_FORMAT,
        ),
        (
            "a read by a name outside the name pattern",
            "DescribeFirewall",
            json!({ "FirewallName": "bad name!" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "a read by an empty name, under the name minimum",
            "DescribeFirewall",
            json!({ "FirewallName": "" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "a read by a name one character past the name maximum",
            "DescribeFirewall",
            json!({ "FirewallName": over_the_name_maximum }),
            NAME_INVALID_FORMAT,
        ),
        (
            "a policy read by a malformed name",
            "DescribeFirewallPolicy",
            json!({ "FirewallPolicyName": "policy/one" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "a rule group read by a malformed name",
            "DescribeRuleGroup",
            json!({ "RuleGroupName": "rule group", "Type": "STATEFUL" }),
            NAME_INVALID_FORMAT,
        ),
        (
            // The fourth resource kind, whose member name opens on an acronym: a violation
            // path lowers only the FIRST character of a member name, so the rule has to be
            // keyed to `tLSInspectionConfigurationName`.
            "a TLS inspection configuration read by a malformed name",
            "DescribeTLSInspectionConfiguration",
            json!({ "TLSInspectionConfigurationName": "tls config" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "a TLS inspection configuration deleted by a malformed name",
            "DeleteTLSInspectionConfiguration",
            json!({ "TLSInspectionConfigurationName": "tls/config" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "a delete by a malformed name",
            "DeleteFirewall",
            json!({ "FirewallName": "fw one" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "a policy delete by a malformed name",
            "DeleteFirewallPolicy",
            json!({ "FirewallPolicyName": "p_one" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "an update by a malformed name",
            "UpdateFirewallDescription",
            json!({ "FirewallName": "fw one", "Description": "x" }),
            NAME_INVALID_FORMAT,
        ),
        (
            "an associate by a malformed name",
            "AssociateFirewallPolicy",
            json!({ "FirewallName": "fw one", "FirewallPolicyArn": policy_arn("p-one") }),
            NAME_INVALID_FORMAT,
        ),
        (
            // Both halves broken: the resolution path reads the ARN first, so the ARN's
            // constant is the one stated.
            "a read whose name and ARN are both malformed",
            "DescribeFirewall",
            json!({ "FirewallName": "fw one", "FirewallArn": "not-an-arn" }),
            ARN_INVALID_FORMAT,
        ),
    ];

    for (case, target, body, expected) in rows {
        let (status, parsed) = harness.call(target, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, INVALID_REQUEST_EXCEPTION),
            "case {case}: expected 400 {INVALID_REQUEST_EXCEPTION}, got {status} with body \
             {parsed:?}"
        );
        let actual = modelled_message(&parsed);
        assert_eq!(
            actual,
            Some(expected),
            "case {case}: expected the Message {expected:?}, got {actual:?} in {parsed:?}"
        );
        assert_eq!(
            rendered_message(&parsed),
            None,
            "case {case}: the text must be carried under ONE member, got {parsed:?}"
        );
    }
}

/// RULE: on the CREATE of a resource, the member naming the resource being created is held to
/// the same form as the reference half of the pair — a value that cannot be a resource name is
/// refused with the modelled `InvalidRequestException` carrying the name constant — while a
/// member of the create that is NOT a resource name keeps the generic constraint clause.
///
/// The rows differ along the resource kind created, the bound the value broke (pattern, length
/// maximum, length minimum) and, for the clause half, the member kind: the required ARN
/// reference to another resource the create takes beside the name.
#[tokio::test]
async fn a_create_states_a_malformed_name_as_a_bad_reference() {
    let harness = Harness::new();
    let over_the_maximum = "a".repeat(129);
    let rule_group_document =
        json!({ "RulesSource": { "RulesString": "pass tcp any any -> any any (sid:1;)" } });
    // (case, operation, request body) — each refused as a malformed resource name
    let names: Vec<(&str, &str, Value)> = vec![
        (
            "a firewall name outside its pattern",
            "CreateFirewall",
            json!({ "FirewallName": "bad name!", "FirewallPolicyArn": policy_arn("p-one") }),
        ),
        (
            "a firewall name past its maximum",
            "CreateFirewall",
            json!({ "FirewallName": over_the_maximum, "FirewallPolicyArn": policy_arn("p-two") }),
        ),
        (
            "an empty firewall name, under its minimum",
            "CreateFirewall",
            json!({ "FirewallName": "", "FirewallPolicyArn": policy_arn("p-three") }),
        ),
        (
            "a policy name outside its pattern",
            "CreateFirewallPolicy",
            json!({
                "FirewallPolicyName": "p one",
                "FirewallPolicy": crate::harness::minimal_policy_document(),
            }),
        ),
        (
            "a policy name carrying a path separator",
            "CreateFirewallPolicy",
            json!({
                "FirewallPolicyName": "p/one",
                "FirewallPolicy": crate::harness::minimal_policy_document(),
            }),
        ),
        (
            "a rule group name outside its pattern",
            "CreateRuleGroup",
            json!({
                "RuleGroupName": "rg one",
                "Type": "STATEFUL",
                "Capacity": 10,
                "RuleGroup": rule_group_document,
            }),
        ),
        (
            "a TLS inspection configuration name outside its pattern",
            "CreateTLSInspectionConfiguration",
            json!({
                "TLSInspectionConfigurationName": "tls config",
                "TLSInspectionConfiguration": { "ServerCertificateConfigurations": [] },
            }),
        ),
    ];

    for (case, target, body) in names {
        let (status, parsed) = harness.call(target, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, INVALID_REQUEST_EXCEPTION),
            "case {case}: expected 400 {INVALID_REQUEST_EXCEPTION}, got {status} with body \
             {parsed:?}"
        );
        let actual = modelled_message(&parsed);
        assert_eq!(
            actual,
            Some(NAME_INVALID_FORMAT),
            "case {case}: expected the Message {NAME_INVALID_FORMAT:?}, got {actual:?} in \
             {parsed:?}"
        );
        assert_eq!(
            rendered_message(&parsed),
            None,
            "case {case}: the text must be carried under ONE member, got {parsed:?}"
        );
    }

    // (case, operation, request body, member path, the requirement expected)
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "a required reference the create takes beside the name",
            "CreateFirewall",
            json!({ "FirewallName": "fw-one", "FirewallPolicyArn": "not-an-arn" }),
            "firewallPolicyArn",
            "Member must satisfy regular expression pattern: ^arn:aws",
        ),
        (
            "a description past its maximum, beside a well-formed name",
            "CreateFirewall",
            json!({
                "FirewallName": "fw-two",
                "FirewallPolicyArn": policy_arn("p-four"),
                "Description": "d".repeat(513),
            }),
            "description",
            "Member must have length less than or equal to 512",
        ),
    ];

    for (case, target, body, member, requirement) in rows {
        let expected = one_error(&clause(member, requirement));
        let (status, parsed) = harness.call(target, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case}: expected 400 {VALIDATION_EXCEPTION}, got {status} with body {parsed:?}"
        );
        let actual = rendered_message(&parsed);
        assert_eq!(
            actual,
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {actual:?} in {parsed:?}"
        );
        assert_eq!(
            modelled_message(&parsed),
            None,
            "case {case}: a class this model does not declare keeps the framework's member, \
             got {parsed:?}"
        );
    }
}

/// RULE: a `@pattern` requirement quotes the expression AWS publishes for the member's shape:
/// the pagination token without the anchor that opens it, and every other shape's expression
/// as the model declares it.
///
/// The rows differ along which shape's expression is quoted and where the member sits: a
/// top-level scalar on two operations (the published form), a member nested beside a
/// reference, and a collection's elements (whose whole constraint set is stated at the
/// collection member). The expressions are the model's, read with
/// `smithy_query(query="shape",shape="PaginationToken,VpcId,UpdateToken")`.
#[tokio::test]
async fn a_pattern_requirement_quotes_the_published_expression() {
    let harness = Harness::new();
    // (case, operation, request body, the clause expected)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a pagination token outside its pattern",
            "ListFirewalls",
            json!({ "NextToken": "bad token!" }),
            clause(
                "nextToken",
                "Member must satisfy regular expression pattern: [0-9A-Za-z:\\/+=]+$",
            ),
        ),
        (
            "a pagination token outside its pattern on another listing",
            "ListRuleGroups",
            json!({ "NextToken": "nope nope" }),
            clause(
                "nextToken",
                "Member must satisfy regular expression pattern: [0-9A-Za-z:\\/+=]+$",
            ),
        ),
        (
            "a VPC id element outside its pattern, stated at the collection member with the \
             model's own expression",
            "ListFirewalls",
            json!({ "VpcIds": ["DMZVPC"] }),
            clause(
                "vpcIds",
                "Member must satisfy constraint: [Member must have length less than or equal to \
                 128, Member must have length greater than or equal to 1, Member must satisfy \
                 regular expression pattern: ^vpc-[0-9a-f]+$]",
            ),
        ),
        (
            "an update token outside its pattern, beside a well-formed reference",
            "UpdateFirewallDescription",
            json!({
                "FirewallName": "fw-one",
                "UpdateToken": "not-a-token",
                "Description": "x",
            }),
            clause(
                "updateToken",
                "Member must satisfy regular expression pattern: \
                 ^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$",
            ),
        ),
    ];

    for (case, target, body, expected_clause) in rows {
        let expected = one_error(&expected_clause);
        let (status, parsed) = harness.call(target, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case}: expected 400 {VALIDATION_EXCEPTION}, got {status} with body {parsed:?}"
        );
        let actual = rendered_message(&parsed);
        assert_eq!(
            actual,
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: an enumerated value set is stated in the order this service states it — for the
/// generated-rules type of a domain-list rule group the model's four values with the alert
/// behaviour before the reject one — and every other enumerated member of this service states
/// the values its model declares.
///
/// The rows differ along which enumerated type the refused member targets, where that member
/// sits (nested three deep inside a submitted document, nested two deep, top level) and the
/// operation. The value sets are the model's, read with
/// `smithy_query(query="shape",shape="GeneratedRulesType,RuleOrder,EnabledAnalysisType")`.
#[tokio::test]
async fn an_enumerated_value_set_is_stated_in_the_order_the_service_states_it() {
    let harness = Harness::new();
    let rules_source_list = |generated: &str| {
        json!({ "RulesSource": { "RulesSourceList": {
            "Targets": ["example.com"],
            "TargetTypes": ["TLS_SNI"],
            "GeneratedRulesType": generated,
        }}})
    };
    // (case, operation, request body, member path, the requirement expected)
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "an unknown generated rules type",
            "CreateRuleGroup",
            json!({
                "RuleGroupName": "rg-one",
                "Type": "STATEFUL",
                "Capacity": 10,
                "RuleGroup": rules_source_list("NOPELIST"),
            }),
            "ruleGroup.rulesSource.rulesSourceList.generatedRulesType",
            "Member must satisfy enum value set: [ALLOWLIST, DENYLIST, ALERTLIST, REJECTLIST]",
        ),
        (
            "a lower-cased generated rules type on the update path",
            "UpdateRuleGroup",
            json!({
                "RuleGroupArn": format!(
                    "arn:aws:network-firewall:{REGION}:{ACCOUNT}:stateful-rulegroup/rg-two"
                ),
                "UpdateToken": "00000000-0000-0000-0000-000000000000",
                "RuleGroup": rules_source_list("allowlist"),
            }),
            "ruleGroup.rulesSource.rulesSourceList.generatedRulesType",
            "Member must satisfy enum value set: [ALLOWLIST, DENYLIST, ALERTLIST, REJECTLIST]",
        ),
        (
            "a lower-cased rule order, an enumeration this service does not restate",
            "CreateFirewallPolicy",
            json!({
                "FirewallPolicyName": "p-one",
                "FirewallPolicy": {
                    "StatelessDefaultActions": ["aws:forward_to_sfe"],
                    "StatelessFragmentDefaultActions": ["aws:forward_to_sfe"],
                    "StatefulEngineOptions": { "RuleOrder": "strict_order" },
                },
            }),
            "firewallPolicy.statefulEngineOptions.ruleOrder",
            "Member must satisfy enum value set: [DEFAULT_ACTION_ORDER, STRICT_ORDER]",
        ),
    ];

    for (case, target, body, member, requirement) in rows {
        let expected = one_error(&clause(member, requirement));
        let (status, parsed) = harness.call(target, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case}: expected 400 {VALIDATION_EXCEPTION}, got {status} with body {parsed:?}"
        );
        let actual = rendered_message(&parsed);
        assert_eq!(
            actual,
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: a value a `@range` admits no more is stated as the RANGE violation it is — a
/// supplied integer below the minimum is not an absent member, however small it is.
///
/// The rows differ along where the integer sits (inside a structure element of a list, at the
/// top level), which end of the range it broke, and the operation. The bounds are the model's,
/// read with `smithy_query(query="shape",shape="Priority,PaginationMaxResults")`. What an
/// OMITTED number is stated as is the companion rule, pinned by
/// [`an_absent_number_is_stated_as_the_bound_its_zero_default_breaks`].
#[tokio::test]
async fn a_supplied_integer_below_the_minimum_is_a_range_violation() {
    let harness = Harness::new();
    let reference = |priority: Value| {
        json!({
            "StatelessDefaultActions": ["aws:forward_to_sfe"],
            "StatelessFragmentDefaultActions": ["aws:forward_to_sfe"],
            "StatelessRuleGroupReferences": [
                { "ResourceArn": stateless_rule_group_arn("rg-one"), "Priority": priority },
            ],
        })
    };
    // (case, operation, request body, member path, the requirement expected)
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "a zero priority inside a policy document",
            "CreateFirewallPolicy",
            json!({ "FirewallPolicyName": "p-one", "FirewallPolicy": reference(json!(0)) }),
            "firewallPolicy.statelessRuleGroupReferences.1.member.priority",
            "Member must have value greater than or equal to 1",
        ),
        (
            "a negative priority inside a policy document",
            "CreateFirewallPolicy",
            json!({ "FirewallPolicyName": "p-two", "FirewallPolicy": reference(json!(-1)) }),
            "firewallPolicy.statelessRuleGroupReferences.1.member.priority",
            "Member must have value greater than or equal to 1",
        ),
        (
            "a priority past the range maximum",
            "CreateFirewallPolicy",
            json!({ "FirewallPolicyName": "p-three", "FirewallPolicy": reference(json!(65_536)) }),
            "firewallPolicy.statelessRuleGroupReferences.1.member.priority",
            "Member must have value less than or equal to 65535",
        ),
        (
            "a zero page size at the top level of another operation",
            "ListFirewallPolicies",
            json!({ "MaxResults": 0 }),
            "maxResults",
            "Member must have value greater than or equal to 1",
        ),
    ];

    for (case, target, body, member, requirement) in rows {
        let expected = one_error(&clause(member, requirement));
        let (status, parsed) = harness.call(target, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case}: expected 400 {VALIDATION_EXCEPTION}, got {status} with body {parsed:?}"
        );
        let actual = rendered_message(&parsed);
        assert_eq!(
            actual,
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: a member the request OMITTED whose target is a NUMBER is stated as the requirement
/// the zero a numeric slot defaults to breaks — the `@range` bound the model declares on that
/// member — and the absent-member requirement is stated for a member whose absence breaks no
/// declared bound: a number whose own range admits zero, a number the model gives no range,
/// and a member that is no number at all.
///
/// The rows differ along what the rule generalises over: which structure the absent member
/// sits in and how deep (a reference inside a policy document, a rule inside a rule-group
/// document, a port range nested three structures below that), the ELEMENT position the
/// structure holds, the operation (two creates and an update), and, for the other side of the
/// rule, the target's own kind and bound. The bounds are the model's, read with
/// `smithy_query(query="closure",operation="CreateFirewallPolicy,CreateRuleGroup")`:
/// `Priority` is `@range` 1..65535, `PortRangeBound` is 0..65535 and `RuleCapacity` carries no
/// range at all.
#[tokio::test]
async fn an_absent_number_is_stated_as_the_bound_its_zero_default_breaks() {
    let harness = Harness::new();
    let reference = |priority: Option<i64>| match priority {
        Some(priority) => {
            json!({ "ResourceArn": stateless_rule_group_arn("rg-one"), "Priority": priority })
        }
        None => json!({ "ResourceArn": stateless_rule_group_arn("rg-one") }),
    };
    let policy = |references: Value| {
        json!({
            "StatelessDefaultActions": ["aws:forward_to_sfe"],
            "StatelessFragmentDefaultActions": ["aws:forward_to_sfe"],
            "StatelessRuleGroupReferences": references,
        })
    };
    // A stateless rule group document whose one rule carries `rule`, so a row can leave a
    // member off the rule itself or off a port range nested under it.
    let stateless_group = |rule: Value| {
        json!({
            "RuleGroupName": "rg-one",
            "Type": "STATELESS",
            "Capacity": 10,
            "RuleGroup": { "RulesSource": { "StatelessRulesAndCustomActions": {
                "StatelessRules": [rule],
            }}},
        })
    };
    let rule_definition = |ports: Value| {
        json!({ "Actions": ["aws:pass"], "MatchAttributes": { "SourcePorts": ports } })
    };
    // (case, operation, request body, member path, the requirement expected)
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "an absent priority on a reference inside a policy document",
            "CreateFirewallPolicy",
            json!({ "FirewallPolicyName": "p-one", "FirewallPolicy": policy(json!([reference(None)])) }),
            "firewallPolicy.statelessRuleGroupReferences.1.member.priority",
            "Member must have value greater than or equal to 1",
        ),
        (
            "an absent priority on the second reference, beside one that carries it",
            "CreateFirewallPolicy",
            json!({
                "FirewallPolicyName": "p-two",
                "FirewallPolicy": policy(json!([reference(Some(1)), reference(None)])),
            }),
            "firewallPolicy.statelessRuleGroupReferences.2.member.priority",
            "Member must have value greater than or equal to 1",
        ),
        (
            "an absent priority on the update of a policy",
            "UpdateFirewallPolicy",
            json!({
                "FirewallPolicyName": "p-three",
                "UpdateToken": "00000000-0000-0000-0000-000000000000",
                "FirewallPolicy": policy(json!([reference(None)])),
            }),
            "firewallPolicy.statelessRuleGroupReferences.1.member.priority",
            "Member must have value greater than or equal to 1",
        ),
        (
            "an absent priority on a rule inside a rule-group document",
            "CreateRuleGroup",
            stateless_group(json!({ "RuleDefinition": rule_definition(json!([])) })),
            "ruleGroup.rulesSource.statelessRulesAndCustomActions.statelessRules.1.member.priority",
            "Member must have value greater than or equal to 1",
        ),
        (
            // The other side of the rule: a required number whose own range ADMITS the zero,
            // so being absent breaks no bound and the absent-member requirement stands.
            "an absent port-range bound, whose range admits zero",
            "CreateRuleGroup",
            stateless_group(json!({
                "Priority": 1,
                "RuleDefinition": rule_definition(json!([{ "ToPort": 80 }])),
            })),
            "ruleGroup.rulesSource.statelessRulesAndCustomActions.statelessRules.1.member.\
             ruleDefinition.matchAttributes.sourcePorts.1.member.fromPort",
            "Member must not be null",
        ),
        (
            // A required number the model gives no range at all.
            "an absent rule-group capacity, a number with no declared range",
            "CreateRuleGroup",
            json!({
                "RuleGroupName": "rg-two",
                "Type": "STATEFUL",
                "RuleGroup": { "RulesSource": { "RulesString": "pass tcp any any -> any any (sid:1;)" } },
            }),
            "capacity",
            "Member must not be null",
        ),
        (
            // A required member that is no number: its slot holds nothing, not a zero.
            "an absent policy reference on the create of a firewall, a required string",
            "CreateFirewall",
            json!({ "FirewallName": "fw-one", "VpcId": "vpc-0123abcd" }),
            "firewallPolicyArn",
            "Member must not be null",
        ),
        (
            // One more non-number: a required list.
            "an absent default-action list inside a policy document",
            "CreateFirewallPolicy",
            json!({
                "FirewallPolicyName": "p-four",
                "FirewallPolicy": { "StatelessDefaultActions": ["aws:forward_to_sfe"] },
            }),
            "firewallPolicy.statelessFragmentDefaultActions",
            "Member must not be null",
        ),
    ];

    for (case, target, body, member, requirement) in rows {
        let expected = one_error(&clause(member, requirement));
        let (status, parsed) = harness.call(target, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case}: expected 400 {VALIDATION_EXCEPTION}, got {status} with body {parsed:?}"
        );
        let actual = rendered_message(&parsed);
        assert_eq!(
            actual,
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: these refusal rules replace no part of the request walk — a request inside every
/// declared bound is still served, and the modelled errors a HANDLER returns still carry
/// their own text in the same `Message` member.
#[tokio::test]
async fn a_well_formed_request_is_still_served_and_handler_errors_still_read() {
    let harness = Harness::new();
    let policy_arn = harness.shared_policy("served-policy").await;

    // A reference at each published bound of the name: one character, and 128.
    let created = harness
        .ok(
            "CreateFirewall",
            &json!({
                "FirewallName": "a",
                "FirewallPolicyArn": policy_arn,
                "VpcId": "vpc-0123abcd",
                "SubnetMappings": [{ "SubnetId": "subnet-0123abcd" }],
            }),
        )
        .await;
    let name = crate::harness::string_at(&created, &["Firewall", "FirewallName"]);
    assert_eq!(
        name, "a",
        "case a one-character name: expected it created, got {created:?}"
    );
    let read = harness
        .ok("DescribeFirewall", &json!({ "FirewallName": "a" }))
        .await;
    assert_eq!(
        crate::harness::string_at(&read, &["Firewall", "FirewallName"]),
        "a",
        "case a one-character name: expected the firewall read back, got {read:?}"
    );

    // A well-formed name nothing was stored under is the handler's own miss, whose text
    // travels in the member every error shape of this model declares.
    let (status, parsed) = harness
        .call("DescribeFirewall", &json!({ "FirewallName": "never-created" }))
        .await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ResourceNotFoundException"),
        "case a name nothing is stored under: expected 400 ResourceNotFoundException, got \
         {status} with body {parsed:?}"
    );
    assert_eq!(
        modelled_message(&parsed),
        Some(crate::harness::NOT_FOUND_MESSAGE),
        "case a name nothing is stored under: expected the recorded miss text, got {parsed:?}"
    );
}
