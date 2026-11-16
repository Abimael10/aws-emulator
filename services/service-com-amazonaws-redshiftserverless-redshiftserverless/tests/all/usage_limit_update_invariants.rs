// SPDX-License-Identifier: CC0-1.0

//! `UpdateUsageLimit` pinned as an invariant over SEQUENCES of updates.
//!
//! Every case drives the real request path — `MultiServiceRouter::dispatch` with
//! `POST /`, `X-Amz-Target: RedshiftServerless.<Operation>` and an awsJson1_1 body — so
//! the boundary validator, the protocol codec and the handler all take part, exactly as
//! they do for an SDK client.
//!
//! The rule under test: an update applies the members the request names — and only those
//! — to the stored record addressed by its id and answers the record as stored, so over
//! ANY sequence of updates the members the operation cannot change stay as the create
//! left them, re-applying a request changes nothing, the last write is what every later
//! reader reports, and no other record of the same resource moves.
//!
//! What one update does to one record, and the refusals an update owes, are
//! `tests/all/usage_limit_payload_and_name_refusals.rs`; a limit outliving its
//! workgroup's status transition is `tests/all/usage_limit_lifecycle.rs`. The harness
//! (emulator, signed call, error-code reader, create helpers) is shared with
//! `namespace_workgroup_lifecycle`, which owns it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::namespace_workgroup_lifecycle::{call, create_namespace, emulator};

/// The values `UsageLimitUsageType` accepts, from
/// `smithy_query(query="shape", shape="UsageLimitUsageType")`.
const USAGE_TYPES: [&str; 2] = ["serverless-compute", "cross-region-datasharing"];

/// The values `UsageLimitPeriod` accepts, from
/// `smithy_query(query="shape", shape="UsageLimitPeriod")`.
const PERIODS: [&str; 3] = ["daily", "weekly", "monthly"];

/// The values `UsageLimitBreachAction` accepts, from
/// `smithy_query(query="shape", shape="UsageLimitBreachAction")`.
const BREACH_ACTIONS: [&str; 3] = ["log", "emit-metric", "deactivate"];

/// The `UsageLimit` members `UpdateUsageLimit` cannot change: its request carries only
/// `amount` and `breachAction` ("You can't update the usage type or period of a usage
/// limit"), so every other member of the shape must read back as the create stored it.
const MEMBERS_AN_UPDATE_CANNOT_CHANGE: [&str; 5] = [
    "usageLimitId",
    "usageLimitArn",
    "resourceArn",
    "usageType",
    "period",
];

/// Create a namespace named `name` and answer its ARN, as a usage limit names it.
async fn namespace_arn(router: &MultiServiceRouter, name: &str) -> String {
    create_namespace(router, name).await["namespaceArn"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Create a usage limit over `resource_arn` and answer the record the create stored.
async fn create_limit(
    router: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
    usage_type: &str,
    period: &str,
    amount: i64,
) -> Value {
    let (status, body) = call(
        router,
        "CreateUsageLimit",
        json!({
            "resourceArn": resource_arn,
            "usageType": usage_type,
            "period": period,
            "amount": amount,
            "breachAction": "log",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateUsageLimit expected 200, got {status} with {body}"
    );
    body["usageLimit"].clone()
}

/// The id of a stored record.
fn id_of(limit: &Value) -> String {
    limit["usageLimitId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Read `id` back and answer the stored record, asserting the read succeeded.
async fn read_back(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(router, "GetUsageLimit", json!({ "usageLimitId": id })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetUsageLimit {id} expected 200, got {status} with {body}"
    );
    body["usageLimit"].clone()
}

/// Apply `update` to `id` and answer the record the update reported.
async fn apply_update(router: &MultiServiceRouter, case: &str, id: &str, update: &Value) -> Value {
    let mut request = json!({ "usageLimitId": id });
    let object = request.as_object_mut().expect("an object request");
    for (key, value) in update.as_object().into_iter().flatten() {
        object.insert(key.clone(), value.clone());
    }
    let (status, body) = call(router, "UpdateUsageLimit", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateUsageLimit expected 200, got {status} with {body}"
    );
    body["usageLimit"].clone()
}

/// The record `base` becomes once the members `update` names are applied to it.
fn expected_after(base: &Value, update: &Value) -> Value {
    let mut expected = base.clone();
    for member in ["amount", "breachAction"] {
        if let Some(value) = update.get(member) {
            expected[member] = value.clone();
        }
    }
    expected
}

/// The update sequences the invariants below are driven over. Each row differs along what
/// the rule generalises over: how many updates follow one another, which updatable
/// members each step names, whether a later step re-states or replaces an earlier one,
/// and the magnitude of the amount (both `Long` extremes included).
fn update_sequences() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        ("no update at all", vec![]),
        (
            "the same update twice",
            vec![
                json!({ "amount": 77, "breachAction": "emit-metric" }),
                json!({ "amount": 77, "breachAction": "emit-metric" }),
            ],
        ),
        (
            "one member then the other",
            vec![
                json!({ "amount": 12 }),
                json!({ "breachAction": "deactivate" }),
            ],
        ),
        (
            "every breach action in turn",
            vec![
                json!({ "breachAction": BREACH_ACTIONS[0] }),
                json!({ "breachAction": BREACH_ACTIONS[1] }),
                json!({ "breachAction": BREACH_ACTIONS[2] }),
            ],
        ),
        (
            "an update naming neither member between two that write",
            vec![
                json!({ "amount": 9 }),
                json!({}),
                json!({ "amount": 10, "breachAction": "log" }),
            ],
        ),
        (
            "the smallest and then the largest amount a Long carries",
            vec![json!({ "amount": 1 }), json!({ "amount": i64::MAX })],
        ),
        (
            "both members replaced by both members",
            vec![
                json!({ "amount": 2, "breachAction": "deactivate" }),
                json!({ "amount": 3_000_000_000_i64, "breachAction": "log" }),
            ],
        ),
    ]
}

#[tokio::test]
async fn an_update_sequence_leaves_only_the_members_it_names_changed() {
    let router = emulator();

    for (index, (case, sequence)) in update_sequences().into_iter().enumerate() {
        // A fresh resource per row, so each row owns its own limit and no row's
        // (resourceArn, usageType, period) triple collides with another's.
        let resource = namespace_arn(&router, &format!("update-invariants-ns-{index}")).await;
        let usage_type = USAGE_TYPES[index % USAGE_TYPES.len()];
        let period = PERIODS[index % PERIODS.len()];
        let created = create_limit(&router, case, &resource, usage_type, period, 60).await;
        let id = id_of(&created);

        // The floor every row is measured from, the zero-update row included: before any
        // update lands, the record the reader reports is the record the create stored.
        let before = read_back(&router, case, &id).await;
        assert_eq!(
            before, created,
            "case {case:?}: before any update GetUsageLimit must report the created record \
             {created}, got {before}"
        );

        let mut expected = created.clone();
        for (step, update) in sequence.iter().enumerate() {
            expected = expected_after(&expected, update);
            let answered = apply_update(&router, case, &id, update).await;
            assert_eq!(
                answered, expected,
                "case {case:?} step {step}: UpdateUsageLimit must answer the stored record \
                 {expected}, got {answered}"
            );

            // The members the operation cannot change read back as the create stored them,
            // however many updates have landed.
            for member in MEMBERS_AN_UPDATE_CANNOT_CHANGE {
                assert_eq!(
                    answered[member], created[member],
                    "case {case:?} step {step}: {member} must stay {:?}, got {:?}",
                    created[member], answered[member]
                );
            }

            // Reading twice reports the same record both times, and the record the update
            // answered.
            for attempt in 1..=2 {
                let read = read_back(&router, case, &id).await;
                assert_eq!(
                    read, expected,
                    "case {case:?} step {step}: GetUsageLimit #{attempt} must report \
                     {expected}, got {read}"
                );
            }
        }
    }
}

#[tokio::test]
async fn applying_the_same_update_again_changes_nothing() {
    let router = emulator();

    for (index, (case, sequence)) in update_sequences().into_iter().enumerate() {
        let Some(last) = sequence.last().cloned() else {
            continue;
        };
        // A fresh resource per row, for the same reason as above.
        let resource = namespace_arn(&router, &format!("idempotent-ns-{index}")).await;
        let usage_type = USAGE_TYPES[index % USAGE_TYPES.len()];
        let period = PERIODS[index % PERIODS.len()];
        let created = create_limit(&router, case, &resource, usage_type, period, 31).await;
        let id = id_of(&created);

        let settled = apply_update(&router, case, &id, &last).await;
        // Idempotence: the same request applied again answers and stores the same record.
        for repeat in 1..=3 {
            let again = apply_update(&router, case, &id, &last).await;
            assert_eq!(
                again, settled,
                "case {case:?} repeat {repeat}: re-applying {last} must answer {settled}, \
                 got {again}"
            );
            let read = read_back(&router, case, &id).await;
            assert_eq!(
                read, settled,
                "case {case:?} repeat {repeat}: GetUsageLimit must report {settled}, got {read}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_moves_no_other_limit_of_the_same_resource() {
    let router = emulator();
    let resource = namespace_arn(&router, "siblings-ns").await;

    // One limit per (usageType, period) pair over the one resource: the whole family, so
    // every sibling of the record under update is watched.
    let mut family: Vec<(String, Value)> = Vec::new();
    for usage_type in USAGE_TYPES {
        for period in PERIODS {
            let case = format!("{usage_type}/{period}");
            let created = create_limit(&router, &case, &resource, usage_type, period, 50).await;
            family.push((case, created));
        }
    }

    for position in 0..family.len() {
        let case = family[position].0.clone();
        let id = id_of(&family[position].1);
        let update = json!({
            "amount": 1_000 + position as i64,
            "breachAction": BREACH_ACTIONS[position % BREACH_ACTIONS.len()],
        });
        let expected = expected_after(&family[position].1, &update);
        let answered = apply_update(&router, &case, &id, &update).await;
        assert_eq!(
            answered, expected,
            "case {case:?}: UpdateUsageLimit must answer {expected}, got {answered}"
        );
        family[position].1 = expected;

        // Every member of the family, the updated one included, reads back as the writes
        // so far left it.
        for (sibling, record) in &family {
            let read = read_back(&router, sibling, &id_of(record)).await;
            assert_eq!(
                &read, record,
                "case {case:?}: the {sibling} limit must still read {record}, got {read}"
            );
        }

        // The listing reports the same records the reads do.
        let (status, body) = call(
            &router,
            "ListUsageLimits",
            json!({ "resourceArn": resource }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListUsageLimits expected 200, got {status} with {body}"
        );
        let listed = body["usageLimits"].as_array().cloned().unwrap_or_default();
        for (sibling, record) in &family {
            assert!(
                listed.contains(record),
                "case {case:?}: ListUsageLimits must report the {sibling} limit {record}, \
                 got {listed:?}"
            );
        }
    }
}
