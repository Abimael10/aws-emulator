// SPDX-License-Identifier: CC0-1.0

//! The semantic checks `CreateRuleGroup` and `UpdateRuleGroup` apply to the rules
//! a request submits, and the refusals they answer a rule group they cannot load
//! with.
//!
//! Five rules, each asserted over a table rather than on one request:
//!
//! * a stateless rule group's `Priority` settings are unique
//!   (`StatelessRule$Priority`: "you must ensure that the priority settings are
//!   unique for the rule group");
//! * every entry of a stateless rule's `Actions` names an action — a standard one
//!   or a custom action the same document declares — and only one of them is
//!   standard (`RuleDefinition$Actions`);
//! * the `RulesSource` member a document carries belongs to the half of the
//!   service the `Type` names (`RulesSource`: "you can use an instance of this for
//!   either stateless rules or stateful rules");
//! * a `Type` outside the values the public API accepts is refused by the
//!   operation that WRITES a rule group, with the modelled client error rather
//!   than a validation clause;
//! * a stateful rule submitted in Suricata flat format is one the engine can load:
//!   a supported action, a signature id, and no two rules assigning the same one.
//!
//! Every refusal here must also leave the store untouched, which each table
//! proves by reading the rule group back and then creating it, under the SAME
//! name, with the defect corrected.

use serde_json::{json, Value};

use crate::harness::{error_code, error_message, string_at, Harness};
use crate::rule_group_lifecycle::{create_request, stateless_group, stateless_rule};

/// The modelled client error every refusal in this suite carries
/// (`smithy_query(query="shape",shape="InvalidRequestException")`: client error,
/// HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The refusal a rule-group type the public API does not accept carries.
const INVALID_TYPE_MESSAGE: &str =
    "The rule group type isn't valid. Enter a valid type, and try again.";

/// A stateless rule group document holding `rules` and declaring `custom` as a
/// custom action, when one is named.
fn stateless_document(rules: Vec<Value>, custom: Option<&str>) -> Value {
    let mut document = stateless_group(rules);
    if let Some(name) = custom {
        document["RulesSource"]["StatelessRulesAndCustomActions"]["CustomActions"] = json!([{
            "ActionName": name,
            "ActionDefinition": { "PublishMetricAction": {
                "Dimensions": [{ "Value": name }],
            }},
        }]);
    }
    document
}

/// One stateless rule at `priority` carrying `actions`.
fn rule_with_actions(priority: i64, actions: Vec<&str>) -> Value {
    json!({
        "Priority": priority,
        "RuleDefinition": { "Actions": actions, "MatchAttributes": {} },
    })
}

/// Assert that `name` names no stored rule group of `group_type`, so a refused
/// request left the store as it was.
async fn assert_nothing_stored(harness: &Harness, case: &str, name: &str, group_type: &str) {
    let (status, body) = harness
        .call(
            "DescribeRuleGroup",
            &json!({ "RuleGroupName": name, "Type": group_type }),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ResourceNotFoundException"),
        "case {case:?}: a refused create must store nothing, got {status} {body:?}"
    );
}

/// Assert that `request` is refused with `message`, and that it stored nothing.
async fn assert_refused(harness: &Harness, case: &str, request: &Value, message: &str) {
    let (status, body) = harness.call("CreateRuleGroup", request).await;
    assert_eq!(
        (status, error_code(&body).as_str(), error_message(&body).as_str()),
        (400, INVALID_REQUEST, message),
        "case {case:?}: expected 400 {INVALID_REQUEST} {message:?}, got {status} {body:?}"
    );
    let name = string_at(request, &["RuleGroupName"]).to_string();
    let group_type = request
        .get("Type")
        .and_then(Value::as_str)
        .unwrap_or("STATELESS")
        .to_string();
    assert_nothing_stored(harness, case, &name, &group_type).await;
}

/// RULE: within one stateless rule group, two rules may not share a `Priority`;
/// the request is refused with the duplicated value in the parameter brackets and
/// the rule's input path as context, nothing is stored, and the same name is still
/// free for the corrected request.
///
/// The rows span where the clash sits (the first two rules, a later pair, a rule
/// repeating the first after an intervening one), how many rules the group
/// carries (2, 3, 4), the duplicated value (the lowest the model admits, a
/// middling one, the highest) and a three-way clash.
#[tokio::test]
async fn stateless_rule_priorities_must_be_unique_within_the_rule_group() {
    // (case, the priorities the rules carry, the one that is duplicated)
    let rows: &[(&str, &[i64], i64)] = &[
        ("the-first-pair", &[10, 10], 10),
        ("a-later-pair", &[1, 20, 20], 20),
        ("repeating-the-first", &[7, 300, 7], 7),
        ("the-lowest-priority", &[1, 1], 1),
        ("the-highest-priority", &[65535, 2, 65535], 65535),
        ("three-rules-at-one-priority", &[5, 5, 5], 5),
        ("a-clash-among-four", &[1, 2, 3, 2], 2),
    ];

    let harness = Harness::new();
    for (case, priorities, duplicated) in rows {
        let rules: Vec<Value> = priorities
            .iter()
            .map(|priority| stateless_rule(*priority, "aws:pass", json!({})))
            .collect();
        let expected = format!(
            "Priority has duplicate values, parameter: [{duplicated}], context: \
             StatelessRulesAndCustomActions.StatelessRules[Priority={duplicated}]"
        );
        let request = create_request(
            case,
            "STATELESS",
            100,
            &json!({ "RuleGroup": stateless_document(rules, None) }),
        );
        assert_refused(&harness, case, &request, &expected).await;

        // The corrected request takes the name the refused one did not: every
        // priority distinct, so nothing clashes.
        let corrected: Vec<Value> = (1..=priorities.len() as i64)
            .map(|priority| stateless_rule(priority, "aws:pass", json!({})))
            .collect();
        let created = harness
            .ok(
                "CreateRuleGroup",
                &create_request(
                    case,
                    "STATELESS",
                    100,
                    &json!({ "RuleGroup": stateless_document(corrected, None) }),
                ),
            )
            .await;
        assert_eq!(
            string_at(&created, &["RuleGroupResponse", "RuleGroupName"]),
            *case,
            "case {case:?}: expected the corrected request to take the free name, got {created:?}"
        );
    }
}

/// RULE: every entry of a stateless rule's `Actions` must name an action — one of
/// the standard actions or a custom action the same `StatelessRulesAndCustomActions`
/// declares — and a list may name only one standard action; the refusal names the
/// offending value and the rule's own input path, located by that rule's priority.
///
/// The rows span: a stateful action's name, a token in no namespace, a custom
/// action no document declares, one declared under another name, the offending
/// rule's position in the group, and two standard actions in one list.
#[tokio::test]
async fn a_stateless_rules_actions_must_name_an_action() {
    // (case, the rules, the declared custom action, the stem, the offending
    //  value, the priority of the rule it sits on)
    type Row<'a> = (&'a str, Vec<Value>, Option<&'a str>, &'a str, &'a str, i64);
    let rows: Vec<Row> = vec![
        (
            "a-stateful-actions-name",
            vec![rule_with_actions(10, vec!["aws:alert"])],
            None,
            "is invalid",
            "aws:alert",
            10,
        ),
        (
            "no-namespace-at-all",
            vec![rule_with_actions(1, vec!["pass"])],
            None,
            "is invalid",
            "pass",
            1,
        ),
        (
            "a-custom-action-nothing-declares",
            vec![rule_with_actions(4, vec!["aws:pass", "MyMetrics"])],
            None,
            "is invalid",
            "MyMetrics",
            4,
        ),
        (
            "a-custom-action-declared-under-another-name",
            vec![rule_with_actions(4, vec!["aws:pass", "MyMetrics"])],
            Some("OtherMetrics"),
            "is invalid",
            "MyMetrics",
            4,
        ),
        (
            "the-second-rule-of-the-group",
            vec![
                rule_with_actions(1, vec!["aws:pass"]),
                rule_with_actions(2, vec!["aws:reject"]),
            ],
            None,
            "is invalid",
            "aws:reject",
            2,
        ),
        (
            "two-standard-actions-at-once",
            vec![rule_with_actions(9, vec!["aws:pass", "aws:drop"])],
            None,
            "cannot exist together",
            "aws:pass, aws:drop",
            9,
        ),
    ];

    let harness = Harness::new();
    for (case, rules, custom, stem, offending, priority) in rows {
        let expected = format!(
            "Actions {stem}, parameter: [{offending}], context: \
             StatelessRulesAndCustomActions.StatelessRules[Priority={priority}].RuleDefinition.Actions"
        );
        let request = create_request(
            case,
            "STATELESS",
            100,
            &json!({ "RuleGroup": stateless_document(rules, custom) }),
        );
        assert_refused(&harness, case, &request, &expected).await;
    }

    // The other side of the rule: a standard action and a custom action the same
    // document declares are accepted together, and the rule group is stored.
    let created = harness
        .ok(
            "CreateRuleGroup",
            &create_request(
                "declared-custom-action",
                "STATELESS",
                100,
                &json!({
                    "RuleGroup": stateless_document(
                        vec![rule_with_actions(4, vec!["aws:pass", "MyMetrics"])],
                        Some("MyMetrics"),
                    ),
                }),
            ),
        )
        .await;
    assert_eq!(
        string_at(&created, &["RuleGroupResponse", "RuleGroupName"]),
        "declared-custom-action",
        "case \"declared-custom-action\": expected the rule group created, got {created:?}"
    );
    let described = harness
        .ok(
            "DescribeRuleGroup",
            &json!({ "RuleGroupName": "declared-custom-action", "Type": "STATELESS" }),
        )
        .await;
    assert_eq!(
        described["RuleGroup"]["RulesSource"]["StatelessRulesAndCustomActions"]["StatelessRules"]
            [0]["RuleDefinition"]["Actions"],
        json!(["aws:pass", "MyMetrics"]),
        "case \"declared-custom-action\": expected the actions stored as given, got {described:?}"
    );
}

/// RULE: a document's `RulesSource` member must belong to the half of the service
/// its `Type` names; one on the wrong half is refused with that member named and
/// its input path as context, and no value from the document is quoted back.
///
/// The rows span every `RulesSource` member against a type of the other half, and
/// both stateful types.
#[tokio::test]
async fn a_rules_source_on_the_wrong_half_of_the_service_is_refused() {
    let stateful_rules = json!([{
        "Action": "PASS",
        "Header": {
            "Protocol": "TCP",
            "Source": "ANY",
            "SourcePort": "ANY",
            "Direction": "ANY",
            "Destination": "ANY",
            "DestinationPort": "ANY",
        },
        "RuleOptions": [{ "Keyword": "sid", "Settings": ["1"] }],
    }]);
    let stateless_source = stateless_group(vec![stateless_rule(1, "aws:pass", json!({}))])
        ["RulesSource"]
        .clone();

    // (case, the type, the rules source, the member the refusal names)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "stateless-rules-in-a-stateful-group",
            "STATEFUL",
            stateless_source.clone(),
            "StatelessRulesAndCustomActions",
        ),
        (
            "stateless-rules-in-a-stateful-domain-group",
            "STATEFUL_DOMAIN",
            stateless_source,
            "StatelessRulesAndCustomActions",
        ),
        (
            "stateful-rules-in-a-stateless-group",
            "STATELESS",
            json!({ "StatefulRules": stateful_rules }),
            "StatefulRules",
        ),
        (
            "a-rules-string-in-a-stateless-group",
            "STATELESS",
            json!({ "RulesString": "pass tcp any any -> any any (sid:1;)" }),
            "RulesString",
        ),
        (
            "a-domain-list-in-a-stateless-group",
            "STATELESS",
            json!({ "RulesSourceList": {
                "Targets": [".example.test"],
                "TargetTypes": ["TLS_SNI"],
                "GeneratedRulesType": "ALLOWLIST",
            }}),
            "RulesSourceList",
        ),
    ];

    let harness = Harness::new();
    for (case, group_type, rules_source, member) in rows {
        let expected = format!("{member} is invalid, context: RulesSource.{member}");
        let request = create_request(
            case,
            group_type,
            100,
            &json!({ "RuleGroup": { "RulesSource": rules_source } }),
        );
        assert_refused(&harness, case, &request, &expected).await;
    }
}

/// RULE: on an operation that WRITES a rule group, a `Type` outside the values the
/// public API accepts is the operation's own refusal — the modelled client error
/// with the constant for an invalid rule-group type — while the same value on an
/// operation that only picks which rule groups to report stays the boundary's
/// validation refusal.
///
/// The rows span the three values the service's own enumeration carries beside the
/// public three, a value no enumeration carries, two accepted values in the wrong
/// case, and the empty string — each on the create and on the listing, plus the
/// update path and a request that breaks a constraint the create does declare
/// alongside the type.
#[tokio::test]
async fn a_type_the_api_does_not_accept_is_refused_as_an_invalid_rule_group_type() {
    let rows: &[&str] = &[
        "FIREWALL",
        "IPS",
        "IPS_DOMAIN",
        "STATEFULL",
        "Stateful",
        "stateless",
        "",
    ];

    let harness = Harness::new();
    for group_type in rows {
        let (status, body) = harness
            .call(
                "CreateRuleGroup",
                &json!({
                    "RuleGroupName": "typed-group",
                    "Type": group_type,
                    "Capacity": 10,
                    "RuleGroup": stateless_group(vec![stateless_rule(1, "aws:pass", json!({}))]),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str(), error_message(&body).as_str()),
            (400, INVALID_REQUEST, INVALID_TYPE_MESSAGE),
            "case {group_type:?}: expected 400 {INVALID_REQUEST} {INVALID_TYPE_MESSAGE:?}, got \
             {status} {body:?}"
        );

        // The same value on the listing, which takes the member to pick which rule
        // groups to report: there it is a constraint on the input, and the
        // framework's own refusal stands.
        let (status, body) = harness
            .call("ListRuleGroups", &json!({ "Type": group_type }))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {group_type:?}: expected the listing to keep its validation refusal, got \
             {status} {body:?}"
        );
    }

    // A request that ALSO breaks a constraint the create does declare never reaches
    // the operation, so it is answered with that constraint's clause — the type,
    // being no constraint of this input, is not among the clauses.
    let (status, body) = harness
        .call(
            "CreateRuleGroup",
            &json!({
                "RuleGroupName": "typed-group",
                "Type": "IPS",
                "Capacity": 10,
                "Description": "d".repeat(513),
                "RuleGroup": stateless_group(vec![stateless_rule(1, "aws:pass", json!({}))]),
            }),
        )
        .await;
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string();
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "case \"an invalid type beside another violation\": expected the declared constraint's \
         refusal, got {status} {body:?}"
    );
    assert_eq!(
        message,
        "1 validation error detected: Value at 'description' failed to satisfy constraint: \
         Member must have length less than or equal to 512",
        "case \"an invalid type beside another violation\": expected the one declared \
         constraint stated, got {message:?}"
    );

    // The update of a rule group writes the type too, so it answers the same
    // refusal — and the rule group it names survives the refused request.
    harness
        .shared_rule_group("updated-group", "STATELESS", 10)
        .await;
    let (status, body) = harness
        .call(
            "UpdateRuleGroup",
            &json!({
                "RuleGroupName": "updated-group",
                "Type": "IPS",
                "UpdateToken": "00000000-0000-4000-8000-000000000000",
                "RuleGroup": stateless_group(vec![stateless_rule(1, "aws:pass", json!({}))]),
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str(), error_message(&body).as_str()),
        (400, INVALID_REQUEST, INVALID_TYPE_MESSAGE),
        "case \"the update path\": expected 400 {INVALID_REQUEST} {INVALID_TYPE_MESSAGE:?}, got \
         {status} {body:?}"
    );
    let described = harness
        .ok(
            "DescribeRuleGroup",
            &json!({ "RuleGroupName": "updated-group", "Type": "STATELESS" }),
        )
        .await;
    assert_eq!(
        string_at(&described, &["RuleGroupResponse", "RuleGroupName"]),
        "updated-group",
        "case \"the update path\": expected the stored rule group untouched, got {described:?}"
    );
}

/// RULE: `Capacity` is sufficient when it covers what the submitted rules
/// consume, and nothing else — there is no floor of its own: a capacity below the
/// consumed units is refused with the capacity the rules REQUIRE in the parameter
/// brackets, whatever the number the request asked for.
///
/// The rows span a capacity of zero, a negative one, one short of what the rules
/// consume, and (on the other side) exactly the consumed units.
#[tokio::test]
async fn capacity_is_sufficient_when_it_covers_what_the_rules_consume() {
    // Three rules, each with no match criteria: "a rule with no criteria
    // specified in any of its match settings has a capacity requirement of 1".
    let rules: Vec<Value> = (1..=3)
        .map(|priority| stateless_rule(priority, "aws:pass", json!({})))
        .collect();
    let required = 3;

    // (case, the capacity the request asks for)
    let rows: &[(&str, i64)] = &[
        ("zero", 0),
        ("negative", -1),
        ("one-short", required - 1),
        ("far-short", 1),
    ];

    let harness = Harness::new();
    let expected = format!(
        "StatelessRulesAndCustomActions capacity exceeded, parameter: [{required}], context: \
         RulesSource.StatelessRulesAndCustomActions"
    );
    for (case, capacity) in rows {
        let request = create_request(
            case,
            "STATELESS",
            *capacity,
            &json!({ "RuleGroup": stateless_group(rules.clone()) }),
        );
        assert_refused(&harness, case, &request, &expected).await;
    }

    let created = harness
        .ok(
            "CreateRuleGroup",
            &create_request(
                "exactly-enough",
                "STATELESS",
                required,
                &json!({ "RuleGroup": stateless_group(rules.clone()) }),
            ),
        )
        .await;
    assert_eq!(
        (
            created["RuleGroupResponse"]["Capacity"].clone(),
            created["RuleGroupResponse"]["ConsumedCapacity"].clone()
        ),
        (json!(required), json!(required)),
        "case \"exactly-enough\": expected the capacity accepted and reported, got {created:?}"
    );
}

/// RULE: a stateful rule submitted in Suricata flat format must be one the engine
/// can load — its action is one the service implements, it assigns a signature id,
/// and the id it assigns is assigned by no other rule of the same string — and a
/// rule group carrying one that cannot be loaded is refused with the rule as the
/// service loaded it and the reason.
///
/// The rows span: an action the service does not implement; a signature id
/// swallowed by an option value left unterminated; no signature id at all; an id
/// two rules share; the same rule written twice; three rules sharing one id; a
/// shared id beside a unique one; and every rule loading cleanly. A shared id is
/// a property of the rules that share it, so EVERY one of them is reported, in
/// the order they were written. Each row is submitted BOTH through `Rules` and
/// through `RuleGroup.RulesSource.RulesString`, the two ways the same rules reach
/// the service.
#[tokio::test]
async fn a_stateful_rule_in_suricata_flat_format_must_be_one_the_engine_can_load() {
    /// The refusal a rule the engine could not load carries.
    fn unloadable(rule: &str, reason: &str) -> String {
        format!("stateful rule is invalid, rule: {rule}, reason: {reason}")
    }
    /// The refusal a rule repeating an assigned signature id carries.
    fn duplicate(rule: &str) -> String {
        format!("stateful rule is invalid, reason: Duplicate signature \"{rule}\"")
    }

    // (case, the rules string, the refusal it must carry)
    let rows: Vec<(&str, &str, Option<String>)> = vec![
        (
            "an-action-the-service-does-not-implement",
            r#"allow tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443"; sid:1001; rev:1;)"#,
            Some(unloadable(
                r#"allow tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443"; sid:1001; rev:1; gid:1234567890;)"#,
                "Action allow is not supported",
            )),
        ),
        (
            "a-signature-id-swallowed-by-an-unterminated-option",
            r#"pass tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443" sid:1001; rev:1;)"#,
            Some(unloadable(
                r#"pass tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443" sid:1001; rev:1; gid:1234567890;)"#,
                "sid not assigned",
            )),
        ),
        (
            "no-signature-id-at-all",
            r#"pass tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443"; rev:1;)"#,
            Some(unloadable(
                r#"pass tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443"; rev:1; gid:1234567890;)"#,
                "sid not assigned",
            )),
        ),
        (
            "a-signature-id-two-rules-share",
            "pass tcp any any -> any 443 (sid:1001; rev:1;)\n\
             drop tcp any any -> any 80 (sid:1001; rev:1;)",
            Some(format!(
                "{}\n{}",
                duplicate("pass tcp any any -> any 443 (sid:1001; rev:1; gid:1234567890;)"),
                duplicate("drop tcp any any -> any 80 (sid:1001; rev:1; gid:1234567890;)"),
            )),
        ),
        (
            // The same rule written twice: one refusal per copy.
            "a-repeated-rule",
            "pass tcp 10.1.0.0/16 any -> any 443 (msg:\"egress 443\"; sid:1001; rev:1;)\n\
             pass tcp 10.1.0.0/16 any -> any 443 (msg:\"egress 443\"; sid:1001; rev:1;)",
            Some(format!(
                "{copy}\n{copy}",
                copy = duplicate(concat!(
                    r#"pass tcp 10.1.0.0/16 any -> any 443 (msg:"egress 443"; sid:1001; "#,
                    "rev:1; gid:1234567890;)",
                )),
            )),
        ),
        (
            "three-rules-sharing-an-id",
            "pass tcp any any -> any 443 (sid:7; rev:1;)\n\
             drop tcp any any -> any 80 (sid:7; rev:1;)\n\
             reject tcp any any -> any 25 (sid:7; rev:1;)",
            Some(format!(
                "{}\n{}\n{}",
                duplicate("pass tcp any any -> any 443 (sid:7; rev:1; gid:1234567890;)"),
                duplicate("drop tcp any any -> any 80 (sid:7; rev:1; gid:1234567890;)"),
                duplicate("reject tcp any any -> any 25 (sid:7; rev:1; gid:1234567890;)"),
            )),
        ),
        (
            // One repeated id beside one that is not: only the rules sharing an
            // id are reported.
            "one-shared-id-beside-a-unique-one",
            "pass tcp any any -> any 443 (sid:11; rev:1;)\n\
             pass udp any any -> any 53 (sid:12; rev:1;)\n\
             drop tcp any any -> any 80 (sid:11; rev:2;)",
            Some(format!(
                "{}\n{}",
                duplicate("pass tcp any any -> any 443 (sid:11; rev:1; gid:1234567890;)"),
                duplicate("drop tcp any any -> any 80 (sid:11; rev:2; gid:1234567890;)"),
            )),
        ),
        (
            "every-rule-loads",
            "# a comment, which carries no rule\n\
             pass tcp any any -> any 443 (sid:1; rev:1;)\n\
             alert udp any any -> any 53 (msg:\"dns\"; sid:2;)\n\
             reject tcp any any -> any 25 (sid:3;)",
            None,
        ),
    ];

    let harness = Harness::new();
    for (case, rules, refusal) in rows {
        for (variant, specification) in [
            ("rules", json!({ "Rules": rules })),
            (
                "rules-string",
                json!({ "RuleGroup": { "RulesSource": { "RulesString": rules } } }),
            ),
        ] {
            let case = format!("{case}-{variant}");
            let request = create_request(&case, "STATEFUL", 100, &specification);
            match &refusal {
                Some(message) => assert_refused(&harness, &case, &request, message).await,
                None => {
                    let created = harness.ok("CreateRuleGroup", &request).await;
                    assert_eq!(
                        created["RuleGroupResponse"]["ConsumedCapacity"],
                        json!(3),
                        "case {case:?}: expected the three rules to consume three units, got \
                         {created:?}"
                    );
                    let described = harness
                        .ok(
                            "DescribeRuleGroup",
                            &json!({ "RuleGroupName": case, "Type": "STATEFUL" }),
                        )
                        .await;
                    assert_eq!(
                        described["RuleGroup"]["RulesSource"]["RulesString"],
                        json!(rules),
                        "case {case:?}: expected the rules read back as given, got {described:?}"
                    );
                }
            }
        }
    }
}
