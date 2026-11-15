// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `UpdateFirewallRuleGroupAssociation` does with an
//! association whose last change has not settled yet.
//!
//! The rule these span: a rule-group-to-VPC association is changed IN PLACE, so
//! the service serialises the callers that change it rather than refusing one.
//! A further update is SERVED while a change of the same association — the
//! caller's own or another writer's — is still settling; every member the later
//! request names is applied together, so no reader ever sees one writer's value
//! for one member beside another writer's value for the next; and the settle
//! deadline is restamped by the change that was just accepted. The invalid state
//! transition the operation's `ConflictException` describes is a step the
//! association cannot be changed in at all — it is on its way out — and not the
//! settling of a change the service already accepted.
//!
//! The refusals that are about the association rather than about its lifecycle
//! step are unaffected, which is why a row drives one of them inside the settle
//! window too: mutation protection still refuses the change, as a
//! `ValidationException` and not as a state-transition conflict.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! same router the emulator mounts, so what is asserted is the HTTP behaviour
//! and not a handler called directly. The transport helpers are the shared ones
//! in [`super::harness`].

use std::sync::Arc;

use serde_json::{json, Value};

use super::harness::{
    call, emulator, error_type, ok, settled_association, string_member, Emulator,
};

/// The `FirewallRuleGroupAssociationStatus` an association reports while a
/// change to it is still settling, from the model's enum
/// (`smithy_query(query="shape",shape="FirewallRuleGroupAssociationStatus")`).
const STATUS_UPDATING: &str = "UPDATING";

/// The status an association reports once no change to it is in flight, from the
/// same enum.
const STATUS_COMPLETE: &str = "COMPLETE";

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

/// Associate the rule group `group` with a VPC of the account at `priority` and
/// answer the association id the service minted, settled, so what a case drives
/// afterwards starts from an association with nothing in flight.
async fn settled_association_at(
    router: &Emulator,
    label: &str,
    group: &str,
    priority: i32,
    name: &str,
) -> String {
    let vpc = format!("vpc-{label}");
    router.hold_vpcs([&vpc]);
    let made = ok(
        router,
        "AssociateFirewallRuleGroup",
        json!({
            "CreatorRequestId": format!("assoc-{label}"),
            "FirewallRuleGroupId": group,
            "VpcId": vpc,
            "Priority": priority,
            "Name": name,
        }),
    )
    .await;
    let id = string_member(
        made.get("FirewallRuleGroupAssociation")
            .expect("the associate answers with the FirewallRuleGroupAssociation"),
        "Id",
    );
    settled_association(router, &id, label).await;
    id
}

/// The `FirewallRuleGroupAssociation` of a response body.
fn association_of(body: &Value) -> &Value {
    body.get("FirewallRuleGroupAssociation")
        .unwrap_or_else(|| panic!("the response carries an association, got {body}"))
}

/// The association's `Priority`, as a number so two of them compare numerically.
fn priority_of(case: &str, association: &Value) -> i64 {
    association
        .get("Priority")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: the association reports a Priority, got {association}"))
}

/// Read the association `id` back through its own reader.
async fn read_association(router: &Emulator, id: &str) -> Value {
    let read = ok(
        router,
        "GetFirewallRuleGroupAssociation",
        json!({ "FirewallRuleGroupAssociationId": id }),
    )
    .await;
    association_of(&read).clone()
}

/// One update of the association under test: the members the writer names.
#[derive(Debug, Clone, Copy)]
struct Write {
    priority: Option<i32>,
    name: Option<&'static str>,
}

/// Updates that follow one another with no wait in between are ALL served, and
/// each is applied to the association the one before it left: the member a
/// writer names takes its value, the member it leaves out keeps the value the
/// earlier write stored, and the association reports the settling status while
/// each change is in flight.
///
/// The table spans what the rule generalises over: how many writers follow one
/// another inside the settle window (one through five), and which members each
/// one names (both, the priority alone, the name alone, neither). The recorded
/// case — two writers, each naming a whole pair — is one row of it.
#[tokio::test]
async fn updates_inside_the_settle_window_are_all_served() {
    let rows: [(&str, &[Write]); 5] = [
        (
            "a single writer naming both members",
            &[Write {
                priority: Some(2100),
                name: Some("only-writer"),
            }],
        ),
        (
            "two writers naming both members",
            &[
                Write {
                    priority: Some(2200),
                    name: Some("first-pair"),
                },
                Write {
                    priority: Some(2300),
                    name: Some("second-pair"),
                },
            ],
        ),
        (
            "three writers naming one member each",
            &[
                Write {
                    priority: Some(3100),
                    name: None,
                },
                Write {
                    priority: None,
                    name: Some("named-alone"),
                },
                Write {
                    priority: Some(3300),
                    name: None,
                },
            ],
        ),
        (
            "five writers in a row",
            &[
                Write {
                    priority: Some(4100),
                    name: Some("a"),
                },
                Write {
                    priority: Some(4200),
                    name: Some("b"),
                },
                Write {
                    priority: Some(4300),
                    name: Some("c"),
                },
                Write {
                    priority: Some(4400),
                    name: Some("d"),
                },
                Write {
                    priority: Some(9900),
                    name: Some("e"),
                },
            ],
        ),
        (
            "a writer that names neither member, then one that names both",
            &[
                Write {
                    priority: None,
                    name: None,
                },
                Write {
                    priority: Some(5100),
                    name: Some("after-the-no-op"),
                },
            ],
        ),
    ];

    for (index, (case, writes)) in rows.iter().enumerate() {
        let router = emulator();
        let label = format!("serialised-{index}");
        let group = rule_group(&router, &label).await;
        // The association starts settled, so what the writers below meet is the
        // settle window of each OTHER and not the one the associate opened.
        let id = settled_association_at(&router, &label, &group, 1000, "before").await;

        let mut expected_priority = 1000_i64;
        let mut expected_name = "before".to_string();

        for (step, write) in writes.iter().enumerate() {
            let mut body = json!({ "FirewallRuleGroupAssociationId": id.clone() });
            if let Some(priority) = write.priority {
                body["Priority"] = json!(priority);
                expected_priority = i64::from(priority);
            }
            if let Some(name) = write.name {
                body["Name"] = json!(name);
                expected_name = name.to_string();
            }

            let (status, parsed) = call(&router, "UpdateFirewallRuleGroupAssociation", body).await;
            assert_eq!(
                status, 200,
                "case {case:?}/write {step}: an update of an association whose last change is \
                 still settling must be served, got {status}: {parsed}"
            );
            let answered = association_of(&parsed);
            assert_eq!(
                priority_of(case, answered),
                expected_priority,
                "case {case:?}/write {step}: expected Priority {expected_priority}, got {answered}"
            );
            assert_eq!(
                string_member(answered, "Name"),
                expected_name,
                "case {case:?}/write {step}: expected Name {expected_name:?}, got {answered}"
            );
            assert_eq!(
                string_member(answered, "Status"),
                STATUS_UPDATING,
                "case {case:?}/write {step}: an accepted change is in flight, so the association \
                 reports {STATUS_UPDATING}, got {answered}"
            );
        }

        // The change the last writer made is the one that settles, and it
        // settles whole: both members carry what the sequence above asked for.
        settled_association(&router, &id, case).await;
        let settled = read_association(&router, &id).await;
        assert_eq!(
            priority_of(case, &settled),
            expected_priority,
            "case {case:?}: a later read must report the last writer's Priority, got {settled}"
        );
        assert_eq!(
            string_member(&settled, "Name"),
            expected_name,
            "case {case:?}: a later read must report the last writer's Name, got {settled}"
        );
        assert_eq!(
            string_member(&settled, "Status"),
            STATUS_COMPLETE,
            "case {case:?}: the association must settle back to {STATUS_COMPLETE}, got {settled}"
        );
    }
}

/// Writers that change one association AT THE SAME TIME are each served, and the
/// association they leave behind holds ONE writer's pair whole: the service
/// serialises them, so the later change wins over the earlier one and no reader
/// sees a pair made of two writers' halves.
///
/// The rows differ along how many callers race on the one association.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_leave_one_whole_pair() {
    for racers in [2_usize, 3, 5] {
        let case = format!("{racers} writers on one association");
        let router = Arc::new(emulator());
        let label = format!("raced-{racers}");
        let group = rule_group(&router, &label).await;
        let id = settled_association_at(&router, &label, &group, 1000, "before").await;

        let mut tasks = Vec::new();
        for racer in 0..racers {
            let router = Arc::clone(&router);
            let id = id.clone();
            tasks.push(tokio::spawn(async move {
                let priority = 6000 + i32::try_from(racer).expect("a small racer count");
                let name = format!("writer-{racer}");
                let (status, parsed) = call(
                    &router,
                    "UpdateFirewallRuleGroupAssociation",
                    json!({
                        "FirewallRuleGroupAssociationId": id,
                        "Priority": priority,
                        "Name": name.clone(),
                    }),
                )
                .await;
                (i64::from(priority), name, status, parsed)
            }));
        }

        // Every writer is served, and each one's own answer reports ITS pair:
        // the change a call accepts is the change it answers with.
        let mut pairs = Vec::new();
        for task in tasks {
            let (priority, name, status, parsed) = task.await.expect("the racing task completed");
            assert_eq!(
                status, 200,
                "case {case:?}: a writer racing another on one association must be served, got \
                 {status}: {parsed}"
            );
            let answered = association_of(&parsed);
            assert_eq!(
                priority_of(&case, answered),
                priority,
                "case {case:?}: a writer's answer must report the Priority it asked for, got \
                 {answered}"
            );
            assert_eq!(
                string_member(answered, "Name"),
                name,
                "case {case:?}: a writer's answer must report the Name it asked for, got {answered}"
            );
            pairs.push((priority, name));
        }

        settled_association(&router, &id, &case).await;
        let settled = read_association(&router, &id).await;
        let landed = (
            priority_of(&case, &settled),
            string_member(&settled, "Name"),
        );
        assert!(
            pairs.contains(&landed),
            "case {case:?}: the settled association must hold one writer's pair whole, one of \
             {pairs:?}, got {landed:?} in {settled}"
        );
    }
}

/// The refusals that are about the association itself are untouched by the
/// window: a change asked for while mutation protection is in force is refused
/// as a `ValidationException` even when a change of the same association is
/// still settling, and the association keeps the values it held.
#[tokio::test]
async fn mutation_protection_still_refuses_a_change_inside_the_settle_window() {
    let router = emulator();
    let group = rule_group(&router, "protected-window").await;
    let id = settled_association_at(&router, "protected-window", &group, 1000, "before").await;

    // Switching protection on is itself a change, so the refusals below arrive
    // while that change is still settling.
    let protected = ok(
        &router,
        "UpdateFirewallRuleGroupAssociation",
        json!({
            "FirewallRuleGroupAssociationId": id.clone(),
            "MutationProtection": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        string_member(association_of(&protected), "MutationProtection"),
        "ENABLED",
        "switching mutation protection on must report it on, got {protected}"
    );

    for (case, body) in [
        (
            "a priority change under protection",
            json!({ "FirewallRuleGroupAssociationId": id.clone(), "Priority": 7100 }),
        ),
        (
            "a name change under protection",
            json!({ "FirewallRuleGroupAssociationId": id.clone(), "Name": "renamed" }),
        ),
        (
            "a removal under protection",
            json!({ "FirewallRuleGroupAssociationId": id.clone() }),
        ),
    ] {
        let operation = if case == "a removal under protection" {
            "DisassociateFirewallRuleGroup"
        } else {
            "UpdateFirewallRuleGroupAssociation"
        };
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must answer HTTP 400, got {status}: {parsed}"
        );
        assert!(
            error_type(&parsed).contains("ValidationException"),
            "case {case:?}: a protected association refuses the change for its protection, got \
             {parsed}"
        );
        assert!(
            !error_type(&parsed).contains("ConflictException"),
            "case {case:?}: the settling of an accepted change is not an invalid state \
             transition, got {parsed}"
        );
    }

    settled_association(&router, &id, "protected-window").await;
    let settled = read_association(&router, &id).await;
    assert_eq!(
        priority_of("protected-window", &settled),
        1000,
        "the refused changes must leave the stored Priority alone, got {settled}"
    );
    assert_eq!(
        string_member(&settled, "Name"),
        "before",
        "the refused changes must leave the stored Name alone, got {settled}"
    );
}

/// An association that is no longer there is not refused as an invalid state
/// transition either: the update answers the `ResourceNotFoundException` it
/// models, for an id a disassociate removed as well as for one the account never
/// held.
#[tokio::test]
async fn an_update_of_an_association_that_is_gone_is_a_not_found() {
    let router = emulator();
    let group = rule_group(&router, "removed").await;
    let id = settled_association_at(&router, "removed", &group, 1000, "before").await;
    ok(
        &router,
        "DisassociateFirewallRuleGroup",
        json!({ "FirewallRuleGroupAssociationId": id.clone() }),
    )
    .await;

    for (case, association_id) in [
        ("an association a disassociate removed", id.as_str()),
        (
            "an id the account never held",
            "rslvr-frgassoc-0000000000000000",
        ),
        ("an id of no such form at all", "not-an-association-id"),
    ] {
        let (status, parsed) = call(
            &router,
            "UpdateFirewallRuleGroupAssociation",
            json!({ "FirewallRuleGroupAssociationId": association_id, "Priority": 8100 }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the update must answer HTTP 400, got {status}: {parsed}"
        );
        assert!(
            error_type(&parsed).contains("ResourceNotFoundException"),
            "case {case:?}: an update with nothing to apply it to is a not-found, got {parsed}"
        );
    }
}
