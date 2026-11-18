// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals a DNS Firewall rule write gets when it
//! leaves out a setting the rule it describes requires.
//!
//! Two requirements, each reported as its own fault:
//!
//! * a rule that blocks needs the option that says HOW it blocks —
//!   `BlockResponse$doc`: "this setting is required if the rule action setting is
//!   BLOCK" — and a write that leaves it out is refused `ValidationException`
//!   with ONE service-coded sentence naming that OPTION in the service's own
//!   spelling (its words, upper case, separated by spaces) and nothing else from
//!   the request;
//! * a rule whose block response is `OVERRIDE` needs all three settings the
//!   override consists of — `BlockOverrideDomain`, `BlockOverrideDnsType` and
//!   `BlockOverrideTtl` are each documented "This setting is required if the
//!   BlockResponse setting is OVERRIDE" — and a write that leaves ANY of them out
//!   is refused `ValidationException` with the service's own coded sentence for
//!   that one requirement, which names all three settings and the `BlockResponse`
//!   value that requires them and is otherwise entirely fixed text. It is a
//!   DIFFERENT code from the block-option sentence above, which is what lets a
//!   caller tell "you left the override settings out" from "the block option you
//!   gave is not one the service admits".
//!
//! Both conditional requirements are recorded behavior: `BlockResponse` and the
//! three `BlockOverride*` members are plain optional members of
//! `CreateFirewallRuleRequest`
//! (`smithy_query(query="operation",operation="CreateFirewallRule")`), so
//! nothing in the model expresses "required when".
//!
//! The tables span what the rules generalise over: which setting the write
//! leaves out — for the override, EVERY non-empty subset of the three, since the
//! requirement binds them together — and the operation that writes the rule: the
//! single create, one entry of a batch create (where the same code and sentence
//! are reported in the response body), and the update that moves a rule onto
//! `BLOCK` or onto `OVERRIDE`. Each table carries the control the requirement is
//! about: the write that states everything is served, and the refused writes
//! store nothing.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! same router the emulator mounts, so what is asserted is the HTTP behaviour
//! and not a handler called directly. The transport helpers are the shared ones
//! in [`super::harness`].

use serde_json::{json, Value};

use super::harness::{call, emulator, error_type, ok, string_member, Emulator};

/// One row: the block options the write states, the option it therefore leaves
/// out, and the sentence the service refuses it with.
struct Row {
    case: &'static str,
    /// The block options the request states beside `Action: BLOCK`.
    stated: fn() -> Value,
    sentence: &'static str,
}

/// The sentence a write that leaves out the block response is refused with,
/// written out here rather than built from the emulator's own constants so the
/// wording itself is what this suite pins.
const NO_BLOCK_RESPONSE: &str = "[RSLVR-02016] Your block option BLOCK RESPONSE is not valid.";

/// The sentence a write that asks for an override response without all three of
/// its settings is refused with — one sentence for the whole requirement,
/// naming the three settings and the `BlockResponse` value that requires them,
/// under the service's own code for that fault. Written out here, like
/// [`NO_BLOCK_RESPONSE`], so the wording is what this suite pins.
const OVERRIDE_INCOMPLETE: &str = "[RSLVR-02013] You must specify an override DNS type, an \
                                   override domain, and an override Ttl when you are using \
                                   OVERRIDE.";

/// Every block option a blocking rule may leave out, each with the sentence the
/// service states for it.
const ROWS: &[Row] = &[
    Row {
        case: "BLOCK with no block response at all",
        stated: || json!({}),
        sentence: NO_BLOCK_RESPONSE,
    },
    Row {
        case: "OVERRIDE with no override target",
        stated: || {
            json!({
                "BlockResponse": "OVERRIDE",
                "BlockOverrideDnsType": "CNAME",
                "BlockOverrideTtl": 60,
            })
        },
        sentence: OVERRIDE_INCOMPLETE,
    },
    Row {
        case: "OVERRIDE with no record type",
        stated: || {
            json!({
                "BlockResponse": "OVERRIDE",
                "BlockOverrideDomain": "override.example.com",
                "BlockOverrideTtl": 60,
            })
        },
        sentence: OVERRIDE_INCOMPLETE,
    },
    Row {
        case: "OVERRIDE with no cache lifetime",
        stated: || {
            json!({
                "BlockResponse": "OVERRIDE",
                "BlockOverrideDomain": "override.example.com",
                "BlockOverrideDnsType": "CNAME",
            })
        },
        sentence: OVERRIDE_INCOMPLETE,
    },
];

/// One of the three settings an override response consists of: the request
/// member that states it, and a value the service admits for it.
struct OverrideSetting {
    member: &'static str,
    value: fn() -> Value,
}

/// The three settings an override response consists of.
///
/// The members and their values are the model's
/// (`smithy_query(query="operation",operation="CreateFirewallRule")`:
/// `BlockOverrideDomain` len 1..255, `BlockOverrideDnsType` the enum whose one
/// value is `CNAME`, `BlockOverrideTtl` range 0..604800).
const OVERRIDE_SETTINGS: &[OverrideSetting] = &[
    OverrideSetting {
        member: "BlockOverrideDomain",
        value: || json!("override.example.com"),
    },
    OverrideSetting {
        member: "BlockOverrideDnsType",
        value: || json!("CNAME"),
    },
    OverrideSetting {
        member: "BlockOverrideTtl",
        value: || json!(60),
    },
];

/// Every subset of [`OVERRIDE_SETTINGS`] a write may leave out, as a bit per
/// setting: `0` states all three (the control the requirement is about) and
/// `1..=7` leave out every non-empty combination of them, one of which is all
/// three at once.
const OMITTED_SUBSETS: std::ops::RangeInclusive<u8> = 0..=7;

/// The block options a write states when it leaves out the settings `omitted`
/// names: `BlockResponse: OVERRIDE`, and each setting whose bit is clear.
fn override_options(omitted: u8) -> Value {
    let mut body = json!({ "BlockResponse": "OVERRIDE" });
    let object = body.as_object_mut().expect("the options are an object");
    for (index, setting) in OVERRIDE_SETTINGS.iter().enumerate() {
        if omitted & (1u8 << index) == 0 {
            object.insert(setting.member.to_string(), (setting.value)());
        }
    }
    body
}

/// How a case names the settings `omitted` leaves out.
fn omitted_case(omitted: u8) -> String {
    if omitted == 0 {
        return "OVERRIDE stating all three settings".to_string();
    }
    let left_out: Vec<&str> = OVERRIDE_SETTINGS
        .iter()
        .enumerate()
        .filter(|(index, _)| omitted & (1u8 << index) != 0)
        .map(|(_, setting)| setting.member)
        .collect();
    format!("OVERRIDE without {}", left_out.join(" and "))
}

/// Create one rule group and answer the id Route 53 Resolver minted for it.
async fn rule_group(router: &Emulator, name: &str) -> String {
    let created = ok(
        router,
        "CreateFirewallRuleGroup",
        json!({ "CreatorRequestId": format!("group-{name}"), "Name": name }),
    )
    .await;
    string_member(
        created
            .get("FirewallRuleGroup")
            .expect("the create answers with the FirewallRuleGroup"),
        "Id",
    )
}

/// Create one domain list and answer the id Route 53 Resolver minted for it.
async fn domain_list(router: &Emulator, name: &str) -> String {
    let created = ok(
        router,
        "CreateFirewallDomainList",
        json!({ "CreatorRequestId": format!("list-{name}"), "Name": name }),
    )
    .await;
    string_member(
        created
            .get("FirewallDomainList")
            .expect("the create answers with the FirewallDomainList"),
        "Id",
    )
}

/// The rule entry a row states: the group, the match source, the name, the
/// priority, `Action: BLOCK`, and the block options the row carries.
fn entry(row: &Row, group: &str, list: &str, request_id: &str, priority: i64) -> Value {
    rule_entry(&(row.stated)(), group, list, request_id, priority)
}

/// The same entry for block options stated directly.
fn rule_entry(
    stated: &Value,
    group: &str,
    list: &str,
    request_id: &str,
    priority: i64,
) -> Value {
    let mut body = json!({
        "CreatorRequestId": request_id,
        "FirewallRuleGroupId": group,
        "FirewallDomainListId": list,
        "Name": request_id,
        "Priority": priority,
        "Action": "BLOCK",
    });
    let object = body.as_object_mut().expect("the entry is an object");
    for (member, value) in stated
        .as_object()
        .expect("the stated options are an object")
        .clone()
    {
        object.insert(member, value);
    }
    body
}

/// Every rule the group holds.
async fn listed_rules(router: &Emulator, group: &str) -> Vec<Value> {
    ok(
        router,
        "ListFirewallRules",
        json!({ "FirewallRuleGroupId": group }),
    )
    .await
    .get("FirewallRules")
    .and_then(Value::as_array)
    .cloned()
    .unwrap_or_default()
}

/// `CreateFirewallRule` refuses a rule that leaves out a block option it
/// requires with the coded sentence naming that option, and writes nothing.
#[tokio::test]
async fn a_create_that_leaves_out_a_required_block_option_is_refused_with_its_coded_sentence() {
    let router = emulator();
    let group = rule_group(&router, "block-option").await;
    let list = domain_list(&router, "block-option").await;

    for (index, row) in ROWS.iter().enumerate()
    {
        let priority = 100 + i64::try_from(index).expect("a small index") * 10;
        let (status, parsed) = call(
            &router,
            "CreateFirewallRule",
            entry(
                row,
                &group,
                &list,
                &format!("block-option-{index}"),
                priority,
            ),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: the create must be refused with HTTP 400, got {status}: {parsed}",
            row.case
        );
        assert!(
            error_type(&parsed).contains("ValidationException"),
            "case {:?}: the refusal must name ValidationException, got {parsed}",
            row.case
        );
        let message = string_member(&parsed, "Message");
        assert_eq!(
            message, row.sentence,
            "case {:?}: expected Message {:?}, got {message:?}",
            row.case, row.sentence
        );
    }

    assert!(
        listed_rules(&router, &group).await.is_empty(),
        "a refused create must add no rule to the group"
    );
}

/// `CreateFirewallRule` refuses an override response that is missing ANY of the
/// three settings it consists of with the one coded sentence for that
/// requirement, serves the one that states all three, and stores nothing for the
/// refused ones.
#[tokio::test]
async fn a_create_asking_for_an_override_without_all_three_settings_is_refused_with_one_sentence() {
    let router = emulator();
    let group = rule_group(&router, "override-completeness").await;
    let list = domain_list(&router, "override-completeness").await;

    for omitted in OMITTED_SUBSETS {
        let case = omitted_case(omitted);
        let request_id = format!("override-completeness-{omitted}");
        let priority = 100 + i64::from(omitted) * 10;
        let (status, parsed) = call(
            &router,
            "CreateFirewallRule",
            rule_entry(
                &override_options(omitted),
                &group,
                &list,
                &request_id,
                priority,
            ),
        )
        .await;
        if omitted == 0 {
            assert_eq!(
                status, 200,
                "case {case:?}: the create stating all three settings must be served, \
                 got {status}: {parsed}"
            );
            let rule = parsed
                .get("FirewallRule")
                .unwrap_or_else(|| panic!("case {case:?}: the create answers the rule, {parsed}"));
            assert_eq!(
                string_member(rule, "BlockResponse"),
                "OVERRIDE",
                "case {case:?}: the served rule must carry the block response it asked for, \
                 got {rule}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: the create must be refused with HTTP 400, got {status}: {parsed}"
        );
        assert!(
            error_type(&parsed).contains("ValidationException"),
            "case {case:?}: the refusal must name ValidationException, got {parsed}"
        );
        let message = string_member(&parsed, "Message");
        assert_eq!(
            message, OVERRIDE_INCOMPLETE,
            "case {case:?}: expected Message {OVERRIDE_INCOMPLETE:?}, got {message:?}"
        );
    }

    let rules = listed_rules(&router, &group).await;
    assert_eq!(
        rules.len(),
        1,
        "only the create stating all three settings may be stored, got {rules:?}"
    );
    let rule = &rules[0];
    assert_eq!(
        string_member(rule, "BlockOverrideDnsType"),
        "CNAME",
        "the stored rule must keep the record type it was created with, got {rule}"
    );
    assert_eq!(
        rule.get("BlockOverrideTtl").and_then(Value::as_i64),
        Some(60),
        "the stored rule must keep the cache lifetime it was created with, got {rule}"
    );
}

/// One entry of a batch create is decided on the same grounds and reports the
/// same code and sentence in the response body, while the entries beside it are
/// created.
#[tokio::test]
async fn a_batch_entry_that_leaves_out_a_required_block_option_reports_the_same_sentence() {
    let router = emulator();
    let group = rule_group(&router, "batch-block-option").await;
    let good_list = domain_list(&router, "batch-block-option-good").await;

    let mut entries: Vec<Value> = Vec::new();
    let mut expected: Vec<&str> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let list = domain_list(&router, &format!("batch-block-option-{index}")).await;
        entries.push(entry(
            row,
            &group,
            &list,
            &format!("batch-block-option-{index}"),
            100 + i64::try_from(index).expect("a small index") * 10,
        ));
        expected.push(row.sentence);
    }
    // One entry the service admits, so the refusals are about the entries that
    // left an option out and not about the batch.
    entries.push(json!({
        "CreatorRequestId": "batch-block-option-good",
        "FirewallRuleGroupId": group,
        "FirewallDomainListId": good_list,
        "Name": "batch-block-option-good",
        "Priority": 900,
        "Action": "BLOCK",
        "BlockResponse": "NXDOMAIN",
    }));

    let body = ok(
        &router,
        "BatchCreateFirewallRule",
        json!({ "CreateFirewallRuleEntries": entries }),
    )
    .await;
    let errors = body
        .get("CreateErrors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        errors.len(),
        expected.len(),
        "the batch must report one error per entry that left an option out, got {body}"
    );
    for (sentence, error) in expected.iter().zip(errors.iter()) {
        assert_eq!(
            string_member(error, "Code"),
            "ValidationException",
            "the error must carry the code the single create answers with, got {error}"
        );
        assert_eq!(
            &string_member(error, "Message"),
            sentence,
            "the error must state the sentence the single create answers with, got {error}"
        );
    }
    let created = listed_rules(&router, &group).await;
    assert_eq!(
        created.len(),
        1,
        "only the entry the service admits may be created, got {created:?}"
    );
}

/// A batch entry asking for an override response without all three of its
/// settings reports the same code and sentence in the response body, for every
/// combination it leaves out, while the entry that states all three is created.
#[tokio::test]
async fn a_batch_entry_with_an_incomplete_override_reports_the_same_one_sentence() {
    let router = emulator();
    let group = rule_group(&router, "batch-override").await;

    let mut entries: Vec<Value> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for omitted in OMITTED_SUBSETS {
        let list = domain_list(&router, &format!("batch-override-{omitted}")).await;
        entries.push(rule_entry(
            &override_options(omitted),
            &group,
            &list,
            &format!("batch-override-{omitted}"),
            100 + i64::from(omitted) * 10,
        ));
        if omitted != 0 {
            refused.push(omitted_case(omitted));
        }
    }

    let body = ok(
        &router,
        "BatchCreateFirewallRule",
        json!({ "CreateFirewallRuleEntries": entries }),
    )
    .await;
    let errors = body
        .get("CreateErrors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        errors.len(),
        refused.len(),
        "the batch must report one error per entry with an incomplete override, got {body}"
    );
    for (case, error) in refused.iter().zip(errors.iter()) {
        assert_eq!(
            string_member(error, "Code"),
            "ValidationException",
            "case {case:?}: the error must carry the code the single create answers with, \
             got {error}"
        );
        assert_eq!(
            string_member(error, "Message"),
            OVERRIDE_INCOMPLETE,
            "case {case:?}: the error must state the sentence the single create answers with, \
             got {error}"
        );
    }
    let created = listed_rules(&router, &group).await;
    assert_eq!(
        created.len(),
        1,
        "only the entry stating all three settings may be created, got {created:?}"
    );
}

/// The update path answers the same sentence: a rule moved onto `BLOCK` without
/// the block response it then requires is refused, and the rule keeps the action
/// it had.
#[tokio::test]
async fn an_update_onto_block_without_the_block_response_is_refused_with_the_same_sentence() {
    let router = emulator();
    let group = rule_group(&router, "update-block-option").await;
    let list = domain_list(&router, "update-block-option").await;

    ok(
        &router,
        "CreateFirewallRule",
        json!({
            "CreatorRequestId": "update-block-option",
            "FirewallRuleGroupId": group,
            "FirewallDomainListId": list,
            "Name": "update-block-option",
            "Priority": 100,
            "Action": "ALERT",
        }),
    )
    .await;

    let (status, parsed) = call(
        &router,
        "UpdateFirewallRule",
        json!({
            "FirewallRuleGroupId": group,
            "FirewallDomainListId": list,
            "Action": "BLOCK",
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the update must be refused with HTTP 400, got {status}: {parsed}"
    );
    assert!(
        error_type(&parsed).contains("ValidationException"),
        "the refusal must name ValidationException, got {parsed}"
    );
    assert_eq!(
        string_member(&parsed, "Message"),
        NO_BLOCK_RESPONSE,
        "the update must state the sentence the create answers with, got {parsed}"
    );

    let rules = listed_rules(&router, &group).await;
    let rule = rules
        .first()
        .unwrap_or_else(|| panic!("the group must still hold its one rule, got {rules:?}"));
    assert_eq!(
        string_member(rule, "Action"),
        "ALERT",
        "the refused update must leave the rule's Action alone, got {rule}"
    );

    // The same update WITH the response it requires is served, so what the
    // refusal was about is the missing option.
    let updated = ok(
        &router,
        "UpdateFirewallRule",
        json!({
            "FirewallRuleGroupId": group,
            "FirewallDomainListId": list,
            "Action": "BLOCK",
            "BlockResponse": "NODATA",
        }),
    )
    .await;
    let rule = updated
        .get("FirewallRule")
        .unwrap_or_else(|| panic!("the update answers the rule, got {updated}"));
    assert_eq!(
        string_member(rule, "Action"),
        "BLOCK",
        "the update that states the option must be served, got {updated}"
    );
}

/// The update path answers the override requirement the same way: moving a
/// stored rule onto `OVERRIDE` without all three settings is refused with the
/// one coded sentence, whichever combination is left out, the rule keeps the
/// block response it had, and the update that states all three is served.
#[tokio::test]
async fn an_update_onto_an_incomplete_override_is_refused_with_the_same_one_sentence() {
    let router = emulator();
    let group = rule_group(&router, "update-override").await;
    let list = domain_list(&router, "update-override").await;

    ok(
        &router,
        "CreateFirewallRule",
        json!({
            "CreatorRequestId": "update-override",
            "FirewallRuleGroupId": group,
            "FirewallDomainListId": list,
            "Name": "update-override",
            "Priority": 100,
            "Action": "BLOCK",
            "BlockResponse": "NXDOMAIN",
        }),
    )
    .await;

    for omitted in OMITTED_SUBSETS.filter(|omitted| *omitted != 0) {
        let case = omitted_case(omitted);
        let mut body = json!({
            "FirewallRuleGroupId": group,
            "FirewallDomainListId": list,
        });
        let object = body.as_object_mut().expect("the update is an object");
        for (member, value) in override_options(omitted)
            .as_object()
            .expect("the options are an object")
            .clone()
        {
            object.insert(member, value);
        }
        let (status, parsed) = call(&router, "UpdateFirewallRule", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the update must be refused with HTTP 400, got {status}: {parsed}"
        );
        assert!(
            error_type(&parsed).contains("ValidationException"),
            "case {case:?}: the refusal must name ValidationException, got {parsed}"
        );
        let message = string_member(&parsed, "Message");
        assert_eq!(
            message, OVERRIDE_INCOMPLETE,
            "case {case:?}: expected Message {OVERRIDE_INCOMPLETE:?}, got {message:?}"
        );
        let rules = listed_rules(&router, &group).await;
        let rule = rules
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the group keeps its rule, got {rules:?}"));
        assert_eq!(
            string_member(rule, "BlockResponse"),
            "NXDOMAIN",
            "case {case:?}: the refused update must leave the block response alone, got {rule}"
        );
    }

    // The same move WITH all three settings is served, so what the refusals were
    // about is the settings the request left out.
    let mut body = json!({
        "FirewallRuleGroupId": group,
        "FirewallDomainListId": list,
    });
    let object = body.as_object_mut().expect("the update is an object");
    for (member, value) in override_options(0)
        .as_object()
        .expect("the options are an object")
        .clone()
    {
        object.insert(member, value);
    }
    let updated = ok(&router, "UpdateFirewallRule", body).await;
    let rule = updated
        .get("FirewallRule")
        .unwrap_or_else(|| panic!("the update answers the rule, got {updated}"));
    assert_eq!(
        string_member(rule, "BlockResponse"),
        "OVERRIDE",
        "the update stating all three settings must be served, got {updated}"
    );
}
