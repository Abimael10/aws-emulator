// SPDX-License-Identifier: CC0-1.0

//! The `State` view an enumerated IAM Identity Center instance carries when SEVERAL clients
//! page one account's listing at the same time, over the `awsJson1_1` wire.
//!
//! THE RULE: the instance projection this service serves is computed, per REQUEST, from the
//! stored instance record alone, so every entry an enumeration serves carries a `State`
//! structure whose `Name` is that entry's own `Status` at the instant that request was
//! served — never a view cached when an enumeration began, and never a view that depends on
//! which other enumerations are in flight beside it.
//!
//! The sibling suites each drive ONE client: `instance_state_view.rs` pins that the member is
//! served and mirrors the status through the lifecycle,
//! `listed_instance_state_source.rs` the record it is read from,
//! `listed_instance_state_every_entry.rs` the cardinality of the listing and the position of
//! an entry in it, `listed_instance_state_at_the_list_bound.rs` the listing's published
//! bound, `listed_instance_state_after_instance_writes.rs` the write paths and
//! `listed_instance_state_across_read_edge_cases.rs` the read paths. This suite spans what
//! none of them drives: SEVERAL continuations open over one store at once —
//!
//! * interleaved enumerations, one request per client per round, so each client's pages are
//!   cut between another client's requests: every client has to assemble the same projection
//!   of every held instance, each entry carrying its own lifecycle value, and the records have
//!   to be exactly as they were once every enumeration has finished (an enumeration is a
//!   read);
//! * a continuation taken ACROSS a write another client made in between: the page served
//!   after it reports the lifecycle the record holds then, in `Status` and in `State.Name`
//!   alike, so neither view can be a value frozen when the enumeration started.
//!
//! The rows' spans are the model's own: the account populations run up to the published
//! maximum of the listing's shape
//! (`smithy_query(query="shape",shape="com.amazonaws.ssoadmin#InstanceList")`: `list of
//! InstanceMetadata`, `length min 0 max 10`), the page sizes are inside the published range of
//! the operation's `MaxResults`
//! (`smithy_query(query="operation",operation="ListInstances")`: `range 1..100`) plus the
//! request that names none, and the lifecycle values the rows seed are the model-generated
//! `ops::types::INSTANCE_STATUS_VALUES` rather than a hand-written catalog.
//!
//! An account holds one instance and these rows need many, so the records are seeded through
//! the SAME `state::Instance` constructor the handlers use; everything then asserted goes over
//! `MultiServiceRouter::dispatch`. The harness is the one `ssoadmin_permission_sets.rs` owns
//! and the rule assertion the one `instance_state_view.rs` owns. Where a phase lives on the
//! far side of a due instant the test moves the STORED instant rather than sleeping — the
//! transition is still applied by the service's own `materialize_due`, exactly as in
//! production.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_ssoadmin_swbexternalservice::ops::types::INSTANCE_STATUS_VALUES;
use service_com_amazonaws_ssoadmin_swbexternalservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::instance_state_view::assert_state_mirrors_status;
use crate::ssoadmin_permission_sets::{call_in_region, hold_deletion, router_over, with_store};

/// The Region every instance below is enabled in, and the Region its calls are signed for:
/// a `RegionName` (`^([a-z]+-){2,3}\d$`) none of the sibling suites uses.
const REGION: &str = "ap-southeast-4";

/// The published maximum of the output list (`InstanceList`, `length min 0 max 10`): the
/// largest listing one account can be enumerated for.
const MAX_LISTED_INSTANCES: usize = 10;

/// How many clients page the listing beside each other.
const CLIENTS: usize = 3;

/// One stored instance of this account, in `status`, with nothing owed on it: the status
/// seeded here IS the record's status, so no transition the clock owes can move it out from
/// under the interleaved reads.
fn instance_in(index: usize, status: &str) -> state::Instance {
    let mut instance = state::Instance::active(
        // `ssoins-` + 16 of `[a-zA-Z0-9-.]`, as the model's `InstanceArn` pattern requires.
        &format!("ssoins-{:016x}", 0x7c3d_0000_u64 + index as u64),
        // `d-` + 10 hex digits, per `InstanceIdentityStoreArn`.
        &format!("d-{:010x}", 0x00b4_0000_u64 + index as u64),
        if index.is_multiple_of(2) {
            state::InstanceType::Account
        } else {
            state::InstanceType::Organization
        },
        // The member is optional and no part of an instance's identity, so some rows carry
        // no name at all and the rest ad-hoc spellings the service did not choose.
        if index.is_multiple_of(3) {
            None
        } else {
            Some(format!("gen-idc-{:08x}", 0x4b70_0000_u32 + index as u32))
        },
        DateTime::from(SystemTime::now()),
        REGION,
    );
    instance.status = status.to_string();
    instance.active_due = None;
    instance.deleted_due = None;
    instance
}

/// A router over an account holding `count` instances, the engine behind it, and the status
/// each instance was seeded in keyed by `InstanceArn` — the map every client's enumeration is
/// checked against.
///
/// The statuses cycle through every value the model's `InstanceStatus` enum declares, so
/// neighbouring entries differ and no page can be right by reporting one constant.
fn seeded_account(
    count: usize,
) -> (
    MultiServiceRouter,
    Arc<engine::InMemoryEngine>,
    BTreeMap<String, String>,
) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let expected = with_store(&engine, |store| {
        store.instances.clear();
        // The account's ambient instance has been enabled once and is gone, so no later
        // request enables one beside the records seeded here.
        store.ambient_instance_enabled = true;
        let mut expected = BTreeMap::new();
        for index in 0..count {
            let status = INSTANCE_STATUS_VALUES[index % INSTANCE_STATUS_VALUES.len()];
            let instance = instance_in(index, status);
            expected.insert(instance.instance_arn.clone(), status.to_string());
            store
                .instances
                .insert(instance.instance_arn.clone(), instance);
        }
        expected
    });
    (router_over(Arc::clone(&engine)), engine, expected)
}

/// The statuses the store holds right now, keyed by `InstanceArn`: what an enumeration must
/// leave untouched.
fn stored_statuses(engine: &Arc<engine::InMemoryEngine>) -> BTreeMap<String, String> {
    with_store(engine, |store| {
        store
            .instances
            .values()
            .map(|instance| (instance.instance_arn.clone(), instance.status.clone()))
            .collect()
    })
}

/// One `ListInstances` request, as `(entries, next continuation token)`.
///
/// A page that returned nothing ends an enumeration, so its token is dropped rather than
/// followed — the same reading of the enumeration's end the sibling suites make.
async fn page(
    case: &str,
    router: &MultiServiceRouter,
    max_results: Option<i64>,
    next_token: Option<&str>,
) -> (Vec<Value>, Option<String>) {
    let mut request = json!({});
    if let Some(max) = max_results {
        request["MaxResults"] = json!(max);
    }
    if let Some(token) = next_token {
        request["NextToken"] = json!(token);
    }
    let (status, body) = call_in_region(router, REGION, "ListInstances", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListInstances expected 200, got {status} {body}"
    );
    let entries = body["Instances"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no Instances in {body}"))
        .clone();
    if let Some(max) = max_results {
        assert!(
            entries.len() as i64 <= max,
            "case {case:?}: a page serves at most the MaxResults asked for, got {} entries",
            entries.len()
        );
    }
    let token = match body["NextToken"].as_str() {
        Some(token) if !entries.is_empty() => Some(token.to_string()),
        _ => None,
    };
    (entries, token)
}

/// `clients` enumerations of the same listing, driven ROUND-ROBIN: one request each per
/// round, so every client's page is cut between the other clients' requests. Returns the
/// entries each client assembled, in the order that client was served them.
async fn interleaved(
    case: &str,
    router: &MultiServiceRouter,
    max_results: Option<i64>,
    clients: usize,
    held: usize,
) -> Vec<Vec<Value>> {
    let mut entries: Vec<Vec<Value>> = vec![Vec::new(); clients];
    let mut tokens: Vec<Option<String>> = vec![None; clients];
    let mut running: Vec<bool> = vec![true; clients];
    // The smallest page the model admits is one instance and an enumeration ends on the page
    // that returns nothing, so this many rounds is one more than any client can need.
    for _ in 0..held + 2 {
        for client in 0..clients {
            if !running[client] {
                continue;
            }
            let client_case = format!("{case} / client {client}");
            let (served, token) =
                page(&client_case, router, max_results, tokens[client].as_deref()).await;
            entries[client].extend(served);
            running[client] = token.is_some();
            tokens[client] = token;
        }
        if !running.iter().any(|open| *open) {
            break;
        }
    }
    assert!(
        !running.iter().any(|open| *open),
        "case {case:?}: an enumeration did not end within the pages this store can fill"
    );
    entries
}

/// THE RULE with several continuations open over one store at once: every client assembles
/// the same projection of every held instance, each entry carrying its own lifecycle value in
/// `State.Name`, and the records are unchanged once the reads are done.
#[tokio::test]
async fn interleaved_enumerations_each_serve_every_entry_with_its_own_state() {
    // Populations up to the published maximum of `InstanceList` (`length min 0 max 10`), and
    // page sizes inside the published range of `MaxResults` (`range 1..100`) — the smallest,
    // one between, the largest listing a page can hold, the ceiling — plus the request that
    // names no page size.
    for held in [1, 2, 5, MAX_LISTED_INSTANCES] {
        for max_results in [None, Some(1), Some(3), Some(MAX_LISTED_INSTANCES as i64), Some(100)] {
            let case = format!("{held} instance(s) / MaxResults {max_results:?} / {CLIENTS} clients");
            let (router, engine, expected) = seeded_account(held);
            let before = stored_statuses(&engine);
            let per_client = interleaved(&case, &router, max_results, CLIENTS, held).await;

            for (client, entries) in per_client.iter().enumerate() {
                let client_case = format!("{case} / client {client}");
                assert_eq!(
                    entries.len(),
                    held,
                    "case {client_case:?}: expected every held instance to be enumerated \
                     once, got {entries:?}"
                );
                let mut seen = BTreeMap::new();
                for (position, entry) in entries.iter().enumerate() {
                    let entry_case = format!("{client_case} / entry {position}");
                    assert_state_mirrors_status(&entry_case, entry);
                    let instance_arn = entry["InstanceArn"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {entry_case:?}: the entry reports no InstanceArn: {entry}")
                        })
                        .to_string();
                    let state_name = entry["State"]["Name"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {entry_case:?}: the entry reports no State.Name: {entry}")
                        })
                        .to_string();
                    seen.insert(instance_arn, state_name);
                }
                assert_eq!(
                    seen, expected,
                    "case {client_case:?}: expected each instance's State.Name to be its own \
                     stored status, got {seen:?}"
                );
                assert_eq!(
                    entries, &per_client[0],
                    "case {client_case:?}: expected every client to be served one projection \
                     of the listing, got {entries:?} vs {:?}",
                    per_client[0]
                );
            }

            let after = stored_statuses(&engine);
            assert_eq!(
                after, before,
                "case {case:?}: enumerating changed what a later caller sees, got {after:?} \
                 after {before:?}"
            );
        }
    }
}

/// THE RULE across a WRITE another client made while a continuation was open: the page served
/// after it reports the lifecycle the record holds at that request's instant, in `Status` and
/// in `State.Name` alike, so neither view is a value frozen when the enumeration began.
#[tokio::test]
async fn a_continued_enumeration_reports_the_lifecycle_of_the_record_as_it_is_then() {
    let (router, engine, expected) = seeded_account(2);
    // Both records start ACTIVE, so the lifecycle the continuation reports can only come from
    // the write made between the two pages.
    with_store(&engine, |store| {
        for instance in store.instances.values_mut() {
            instance.status = state::STATUS_ACTIVE.to_string();
            instance.active_due = None;
            instance.deleted_due = None;
        }
    });
    let case = "a continuation taken across another client's write";

    // PAGE 1 — one entry, with its own ACTIVE view, and a continuation left open.
    let (first, token) = page(case, &router, Some(1), None).await;
    assert_eq!(
        first.len(),
        1,
        "case {case:?}: expected a single-entry page, got {first:?}"
    );
    assert_state_mirrors_status(case, &first[0]);
    assert_eq!(
        first[0]["State"]["Name"].as_str(),
        Some(state::STATUS_ACTIVE),
        "case {case:?}: the record is active when the enumeration starts, got {}",
        first[0]
    );
    let served_arn = first[0]["InstanceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no InstanceArn in {}", first[0]))
        .to_string();
    let token = token.unwrap_or_else(|| {
        panic!("case {case:?}: the page that served an instance left no continuation open")
    });

    // THE WRITE another client makes in between: the instance this enumeration has NOT
    // reached yet enters its deletion, which is held open so the transient value is observed
    // for certain rather than raced.
    let pending_arn = expected
        .keys()
        .find(|instance_arn| **instance_arn != served_arn)
        .expect("the account holds a second instance")
        .clone();
    let (status, deleted) = call_in_region(
        &router,
        REGION,
        "DeleteInstance",
        &json!({"InstanceArn": pending_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteInstance expected 200, got {status} {deleted}"
    );
    hold_deletion(&engine, SystemTime::now() + Duration::from_secs(3_600));

    // PAGE 2 — the continuation taken across that write serves the record as it is NOW.
    let (second, _) = page(case, &router, Some(1), Some(&token)).await;
    assert_eq!(
        second.len(),
        1,
        "case {case:?}: the continuation serves the instance it resumes at, got {second:?}"
    );
    let entry = &second[0];
    assert_eq!(
        entry["InstanceArn"].as_str(),
        Some(pending_arn.as_str()),
        "case {case:?}: expected the continuation to resume at the second instance, got {entry}"
    );
    assert_state_mirrors_status(case, entry);
    assert_eq!(
        entry["State"]["Name"].as_str(),
        Some(state::STATUS_DELETE_IN_PROGRESS),
        "case {case:?}: expected the lifecycle the record holds after the write, got {entry}"
    );

    // And the instance this enumeration served BEFORE the write is still reported as it is:
    // a fresh page of the same listing reports the untouched record's own value.
    let (reread, _) = page(case, &router, Some(1), None).await;
    assert_eq!(
        reread.len(),
        1,
        "case {case:?}: expected a single-entry page, got {reread:?}"
    );
    assert_state_mirrors_status(case, &reread[0]);
    assert_eq!(
        reread[0]["InstanceArn"].as_str(),
        Some(served_arn.as_str()),
        "case {case:?}: expected the listing to start at the same instance, got {}",
        reread[0]
    );
    assert_eq!(
        reread[0]["State"]["Name"].as_str(),
        Some(state::STATUS_ACTIVE),
        "case {case:?}: the record the write did not touch is still active, got {}",
        reread[0]
    );
}
