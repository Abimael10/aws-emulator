// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateQueue`, driven over the awsJson1_0 wire through the same
//! router the emulator serves from, on the harness [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: an update applies every configuration member the request carries to the
//! stored queue of the cluster it names, leaves every member the request omits at its stored
//! value, reports the queue `UPDATING` and owes it `ACTIVE` again — and refuses, without
//! storing anything, a request whose cluster or queue does not resolve, whose queue is not
//! settled, whose association names a compute node group the cluster does not hold, or whose
//! lists would size the stored record past the ceiling this emulator holds one to.
//!
//! Nothing here waits on the wall clock: a transition this service owes is materialised
//! before the next request is answered.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::{
    service_state, MAX_STORED_LIST_ENTRIES, STATUS_ACTIVE, STATUS_CREATING, STATUS_DELETING,
    STATUS_UPDATING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{
    call, create_cluster, create_compute_node_group, create_queue, emulator, emulator_with_engine,
};

/// The member a queue's associations travel in, in both request shapes that place them
/// (`smithy_query(query="shape", shape="CreateQueueRequest,UpdateQueueRequest")`).
const ASSOCIATIONS_MEMBER: &str = "computeNodeGroupConfigurations";

/// A cluster holding two compute node groups and one queue, as
/// `(cluster id, queue id, [(group name, group id)])`. Every resource is settled by the
/// request that follows its own create, so there is nothing to wait for.
async fn cluster_with_a_queue(
    router: &MultiServiceRouter,
    cluster_name: &str,
    queue_name: &str,
) -> (String, String, Vec<(String, String)>) {
    let (status, created) = call(
        router,
        "CreateCluster",
        &create_cluster(cluster_name, "SMALL"),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateCluster got {status} with {created}");
    let cluster_id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let mut groups = Vec::new();
    for name in ["nodes-a", "nodes-b"] {
        let (status, group) = call(
            router,
            "CreateComputeNodeGroup",
            &create_compute_node_group(&cluster_id, name),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateComputeNodeGroup {name} got {status} with {group}"
        );
        groups.push((
            name.to_string(),
            group["computeNodeGroup"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
    }

    let (status, created) = call(
        router,
        "CreateQueue",
        &create_queue(&cluster_id, queue_name, &[]),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateQueue got {status} with {created}");
    let queue_id = created["queue"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (cluster_id, queue_id, groups)
}

/// The stored queue, as `GetQueue` reports it.
async fn stored_queue(router: &MultiServiceRouter, cluster: &str, queue: &str) -> Value {
    let (status, read) = call(
        router,
        "GetQueue",
        &json!({"clusterIdentifier": cluster, "queueIdentifier": queue}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case read back {queue}: got {status} with {read}"
    );
    read["queue"].clone()
}

/// `patch` as a whole `UpdateQueue` request about `queue` in `cluster`.
fn update_request(cluster: &str, queue: &str, patch: &Value) -> Value {
    let mut request = json!({"clusterIdentifier": cluster, "queueIdentifier": queue});
    if let (Some(target), Some(members)) = (request.as_object_mut(), patch.as_object()) {
        for (member, value) in members {
            target.insert(member.clone(), value.clone());
        }
    }
    request
}

/// `resource` without `members`, so what one call answered can be compared WHOLE against
/// what the next read answers, minus only the members the call was expected to move.
fn without_members(resource: &Value, members: &[&str]) -> Value {
    let mut stripped = resource.clone();
    if let Some(object) = stripped.as_object_mut() {
        for member in members {
            object.remove(*member);
        }
    }
    stripped
}

/// A Slurm custom setting of the form `SlurmCustomSetting` declares — both members
/// `@required` (`smithy_query(query="shape", shape="SlurmCustomSetting")`).
fn slurm_setting(name: &str, value: &str) -> Value {
    json!({"parameterName": name, "parameterValue": value})
}

/// Every row of the member table: a case name, the members one request carries, and what the
/// stored queue must report for each of those members afterwards.
type MemberRow = (String, Value, Value);

/// One row per member (and per value class within a member) an update can carry: the
/// request's patch, and the members the stored queue must report afterwards. The expected
/// value differs from the patch only where the service MERGES rather than replaces — the
/// Slurm configuration, where a member the request omits keeps the value an earlier call
/// stored.
fn updatable_members(groups: &[(String, String)]) -> Vec<MemberRow> {
    let (first_name, first_id) = groups[0].clone();
    let (second_name, second_id) = groups[1].clone();

    let mut rows: Vec<MemberRow> = vec![
        // The two forms a configuration can name a group in — the generated id and the
        // caller-chosen name — then how many it names, their ORDER, an entry naming no group
        // at all (the shape's one member is optional), and the empty list that associates
        // nothing.
        (
            "associations-one-by-id".to_string(),
            json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &first_id}]}),
            json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &first_id}]}),
        ),
        (
            "associations-one-by-name".to_string(),
            json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &second_name}]}),
            json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &second_name}]}),
        ),
        (
            "associations-two".to_string(),
            json!({ASSOCIATIONS_MEMBER: [
                {"computeNodeGroupId": &first_id},
                {"computeNodeGroupId": &second_id},
            ]}),
            json!({ASSOCIATIONS_MEMBER: [
                {"computeNodeGroupId": &first_id},
                {"computeNodeGroupId": &second_id},
            ]}),
        ),
        (
            "associations-two-reordered".to_string(),
            json!({ASSOCIATIONS_MEMBER: [
                {"computeNodeGroupId": &second_id},
                {"computeNodeGroupId": &first_name},
            ]}),
            json!({ASSOCIATIONS_MEMBER: [
                {"computeNodeGroupId": &second_id},
                {"computeNodeGroupId": &first_name},
            ]}),
        ),
        (
            "associations-with-an-entry-naming-none".to_string(),
            json!({ASSOCIATIONS_MEMBER: [{}, {"computeNodeGroupId": &first_id}]}),
            json!({ASSOCIATIONS_MEMBER: [{}, {"computeNodeGroupId": &first_id}]}),
        ),
        (
            "associations-none".to_string(),
            json!({ASSOCIATIONS_MEMBER: []}),
            json!({ASSOCIATIONS_MEMBER: []}),
        ),
    ];

    // The Slurm configuration is MERGED, so each row's expectation carries what the rows
    // before it stored — which for this request shape's single member means the latest list.
    let mut slurm = json!({});
    let settings_rows: [(&str, Value); 3] = [
        (
            "slurmConfiguration-one-setting",
            json!([slurm_setting("Weight", "10")]),
        ),
        (
            "slurmConfiguration-two-settings",
            json!([
                slurm_setting("Weight", "20"),
                slurm_setting("MaxTime", "1-00:00:00"),
            ]),
        ),
        ("slurmConfiguration-no-setting", json!([])),
    ];
    for (case, settings) in settings_rows {
        slurm["slurmCustomSettings"] = settings.clone();
        rows.push((
            case.to_string(),
            json!({"slurmConfiguration": {"slurmCustomSettings": settings}}),
            json!({"slurmConfiguration": slurm.clone()}),
        ));
    }

    // Both members at once: an update carries as many of them as the caller wrote.
    slurm["slurmCustomSettings"] = json!([slurm_setting("CpuBind", "cores")]);
    rows.push((
        "both-members-at-once".to_string(),
        json!({
            ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &second_id}],
            "slurmConfiguration": {"slurmCustomSettings": [slurm_setting("CpuBind", "cores")]},
        }),
        json!({
            ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &second_id}],
            "slurmConfiguration": slurm.clone(),
        }),
    ));
    rows
}

/// RULE: every member an update carries is applied to the stored queue and every member it
/// omits keeps its stored value; the update itself reports the transient status and the queue
/// is `ACTIVE` again on the next request.
#[tokio::test]
async fn every_member_an_update_carries_is_applied_and_the_rest_are_left_alone() {
    let router = emulator();
    let (cluster_id, queue_id, groups) =
        cluster_with_a_queue(&router, "update-queue-cluster", "work").await;
    let mut settled = stored_queue(&router, &cluster_id, &queue_id).await;

    for (case, patch, expected) in updatable_members(&groups) {
        let (status, answered) = call(
            &router,
            "UpdateQueue",
            &update_request(&cluster_id, &queue_id, &patch),
        )
        .await;
        assert_eq!(status, 200, "case {case}: got {status} with {answered}");
        let updated = answered["queue"].clone();
        assert_eq!(
            updated["status"],
            json!(STATUS_UPDATING),
            "case {case}: expected the update to report the transient status, got {updated}"
        );

        let members = expected
            .as_object()
            .map(|members| members.keys().map(String::as_str).collect::<Vec<&str>>())
            .unwrap_or_default();
        for member in &members {
            assert_eq!(
                updated[*member], expected[*member],
                "case {case}: expected {member} to be {}, got {updated}",
                expected[*member]
            );
        }

        // Nothing else moved: the whole resource, minus the members this row changed and the
        // two a transition moves, is what the previous read answered.
        let untouched: Vec<&str> = members
            .iter()
            .copied()
            .chain(["status", "modifiedAt"])
            .collect();
        assert_eq!(
            without_members(&updated, &untouched),
            without_members(&settled, &untouched),
            "case {case}: expected every member the request omitted to keep its stored \
             value, got {updated} against {settled}"
        );

        // The change persisted, and the queue settled back on this very request.
        let read = stored_queue(&router, &cluster_id, &queue_id).await;
        assert_eq!(
            read["status"],
            json!(STATUS_ACTIVE),
            "case {case}: expected the queue ACTIVE again on the request that follows the \
             update, got {read}"
        );
        assert_eq!(
            without_members(&read, &["status", "modifiedAt"]),
            without_members(&updated, &["status", "modifiedAt"]),
            "case {case}: expected the read to answer what the update stored, got {read} \
             against {updated}"
        );
        settled = read;
    }
}

/// RULE: an update is addressed by either form `QueueIdentifier` and `ClusterIdentifier`
/// allow — the generated id or the caller-chosen name — and both reach one record.
#[tokio::test]
async fn an_update_resolves_the_queue_by_id_or_by_name() {
    for by_name in [false, true] {
        let case = if by_name { "by name" } else { "by id" };
        let router = emulator();
        let (cluster_id, queue_id, groups) =
            cluster_with_a_queue(&router, "ref-queue-cluster", "ref-queue").await;
        let (_group_name, group_id) = groups[0].clone();
        let queue_ref = if by_name {
            "ref-queue".to_string()
        } else {
            queue_id.clone()
        };
        let cluster_ref = if by_name {
            "ref-queue-cluster".to_string()
        } else {
            cluster_id.clone()
        };

        let (status, answered) = call(
            &router,
            "UpdateQueue",
            &update_request(
                &cluster_ref,
                &queue_ref,
                &json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &group_id}]}),
            ),
        )
        .await;
        assert_eq!(status, 200, "case {case}: got {status} with {answered}");
        assert_eq!(
            answered["queue"]["id"],
            json!(queue_id),
            "case {case}: expected the update to answer the one stored queue, got {answered}"
        );

        let read = stored_queue(&router, &cluster_id, &queue_id).await;
        assert_eq!(
            read[ASSOCIATIONS_MEMBER],
            json!([{"computeNodeGroupId": &group_id}]),
            "case {case}: expected the update to persist, got {read}"
        );
    }
}

/// RULE: an update that carries no configuration member asks for no change, so the queue
/// keeps its status and every stored value.
#[tokio::test]
async fn an_update_that_carries_no_member_changes_nothing() {
    let router = emulator();
    let (cluster_id, queue_id, _groups) =
        cluster_with_a_queue(&router, "noop-queue-cluster", "noop-queue").await;
    let settled = stored_queue(&router, &cluster_id, &queue_id).await;

    let (status, answered) = call(
        &router,
        "UpdateQueue",
        &update_request(&cluster_id, &queue_id, &json!({})),
    )
    .await;
    assert_eq!(status, 200, "case empty update: got {status} with {answered}");
    assert_eq!(
        answered["queue"], settled,
        "case empty update: expected the stored queue answered unchanged, got {answered}"
    );

    let read = stored_queue(&router, &cluster_id, &queue_id).await;
    assert_eq!(
        read, settled,
        "case empty update: expected the stored queue left alone, got {read}"
    );
}

/// RULE: an update of a queue that does not resolve — or inside a cluster that does not — is
/// refused, and stores nothing. WHICH refusal an unresolvable `clusterIdentifier` earns is
/// decided by its form: a generated id names a cluster that is not there, a cluster NAME is a
/// permissions failure.
#[tokio::test]
async fn an_update_is_refused_when_its_cluster_or_queue_does_not_resolve() {
    let router = emulator();
    let (cluster_id, queue_id, _groups) =
        cluster_with_a_queue(&router, "present-queue-cluster", "present-queue").await;
    let settled = stored_queue(&router, &cluster_id, &queue_id).await;
    let patch = json!({ASSOCIATIONS_MEMBER: []});

    // (case, clusterIdentifier, queueIdentifier, the error the refusal names)
    let rows: [(&str, String, String, &str); 5] = [
        (
            "a cluster id nothing holds",
            "pcs_0123456789abcdef".to_string(),
            queue_id.clone(),
            "ResourceNotFoundException",
        ),
        (
            "a cluster name nothing holds",
            "no-such-cluster".to_string(),
            queue_id.clone(),
            "AccessDeniedException",
        ),
        (
            "a queue name nothing holds",
            cluster_id.clone(),
            "no-such-queue".to_string(),
            "ResourceNotFoundException",
        ),
        (
            "a queue id nothing holds",
            cluster_id.clone(),
            "pcs_fedcba9876543210".to_string(),
            "ResourceNotFoundException",
        ),
        (
            "a compute node group's name in the queue's place",
            cluster_id.clone(),
            "nodes-a".to_string(),
            "ResourceNotFoundException",
        ),
    ];

    for (case, cluster_ref, queue_ref, expected) in rows {
        let (status, refused) = call(
            &router,
            "UpdateQueue",
            &update_request(&cluster_ref, &queue_ref, &patch),
        )
        .await;
        assert_eq!(
            status, CLIENT_ERROR_STATUS,
            "case {case}: expected the client-error status, got {status} with {refused}"
        );
        assert_eq!(
            refused["__type"],
            json!(expected),
            "case {case}: expected {expected}, got {refused}"
        );
        if expected == "ResourceNotFoundException" {
            let (kind, named) = if cluster_ref == cluster_id {
                ("queue", &queue_ref)
            } else {
                ("cluster", &cluster_ref)
            };
            assert_eq!(
                refused["message"],
                json!(format!("AWS PCS couldn't find the {kind} you specified.")),
                "case {case}: expected the service's own sentence, got {refused}"
            );
            assert_eq!(
                refused["resourceId"],
                json!(named),
                "case {case}: expected the identifier as supplied in resourceId, got {refused}"
            );
        }

        let read = stored_queue(&router, &cluster_id, &queue_id).await;
        assert_eq!(
            read, settled,
            "case {case}: expected the refusal to store nothing, got {read}"
        );
    }
}

/// RULE, on both paths that WRITE a queue's associations: a queue assigns jobs to compute
/// node groups of its OWN cluster, so a configuration naming one the cluster does not hold is
/// refused rather than stored as a reference nothing resolves.
#[tokio::test]
async fn neither_write_path_stores_an_association_the_cluster_cannot_resolve() {
    let router = emulator();
    let (cluster_id, queue_id, _groups) =
        cluster_with_a_queue(&router, "dangling-cluster", "dangling-queue").await;
    let settled = stored_queue(&router, &cluster_id, &queue_id).await;

    // Rows: the forms a named group can fail to resolve in — a name nothing holds, a
    // generated id nothing holds, and an absent group BESIDE one the cluster does hold.
    let absent_rows: [(&str, Value); 3] = [
        (
            "a group name nothing holds",
            json!([{"computeNodeGroupId": "no-such-group"}]),
        ),
        (
            "a group id nothing holds",
            json!([{"computeNodeGroupId": "pcs_fedcba9876543210"}]),
        ),
        (
            "an absent group beside a present one",
            json!([{"computeNodeGroupId": "nodes-a"}, {"computeNodeGroupId": "nodes-z"}]),
        ),
    ];

    for (case, configurations) in absent_rows {
        // The UPDATE path.
        let (status, refused) = call(
            &router,
            "UpdateQueue",
            &update_request(
                &cluster_id,
                &queue_id,
                &json!({ASSOCIATIONS_MEMBER: configurations.clone()}),
            ),
        )
        .await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ResourceNotFoundException")),
            "case {case} on UpdateQueue: expected the group not-found, got {status} with {refused}"
        );
        assert_eq!(
            refused["message"],
            json!("AWS PCS couldn't find the compute node group you specified."),
            "case {case} on UpdateQueue: expected the service's own sentence, got {refused}"
        );

        // The CREATE path, which holds a stored reference to the same rule.
        let mut creating = create_queue(&cluster_id, "another-queue", &[]);
        creating[ASSOCIATIONS_MEMBER] = configurations;
        let (status, refused) = call(&router, "CreateQueue", &creating).await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ResourceNotFoundException")),
            "case {case} on CreateQueue: expected the group not-found, got {status} with {refused}"
        );

        let read = stored_queue(&router, &cluster_id, &queue_id).await;
        assert_eq!(
            read, settled,
            "case {case}: expected the refusal to store nothing, got {read}"
        );
    }
}

/// Hold the queue `name` of the cluster `cluster` in `status` with nothing owed — a queue the
/// real service has not finished provisioning, is updating, or is taking out. No sequence of
/// requests leaves one there, so this is written through the service's own store.
fn hold_queue(engine: &engine::InMemoryEngine, cluster: &str, name: &str, status: &str) {
    let store = service_state(engine).expect("this service's own store");
    let mut state = store.lock();
    let held = state
        .cluster_mut(cluster)
        .expect("the cluster the create persisted")
        .queues
        .values_mut()
        .find(|queue| queue.name == name)
        .expect("the queue the create persisted");
    held.status = status.to_string();
    held.pending = None;
}

/// RULE: only a settled queue takes an update — one still provisioning, already applying an
/// update, or on its way out is refused with the modelled `ConflictException` naming the
/// queue.
#[tokio::test]
async fn an_update_is_refused_while_the_queue_is_not_settled() {
    // Rows: the unsettled `QueueStatus` members a queue can be held in
    // (`smithy_query(query="shape",shape="QueueStatus")`).
    for held in [STATUS_CREATING, STATUS_UPDATING, STATUS_DELETING] {
        let (router, engine) = emulator_with_engine();
        let (cluster_id, queue_id, groups) =
            cluster_with_a_queue(&router, "gated-queue-cluster", "gated-queue").await;
        let (_group_name, group_id) = groups[0].clone();
        hold_queue(&engine, &cluster_id, "gated-queue", held);

        let (status, refused) = call(
            &router,
            "UpdateQueue",
            &update_request(
                &cluster_id,
                &queue_id,
                &json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &group_id}]}),
            ),
        )
        .await;
        assert_eq!(
            status, CLIENT_ERROR_STATUS,
            "case {held}: expected the client-error status, got {status} with {refused}"
        );
        assert_eq!(
            refused["__type"],
            json!("ConflictException"),
            "case {held}: expected a conflict, got {refused}"
        );
        assert_eq!(
            refused["message"],
            json!("A queue isn't in ACTIVE status."),
            "case {held}: expected the service's own sentence, got {refused}"
        );
        assert_eq!(
            (
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (Some(queue_id.as_str()), Some("queue")),
            "case {held}: expected the refusal to name the queue, got {refused}"
        );

        let read = stored_queue(&router, &cluster_id, &queue_id).await;
        assert_eq!(
            read[ASSOCIATIONS_MEMBER],
            json!([]),
            "case {held}: expected the refusal to store nothing, got {read}"
        );
    }
}

/// RULE: a caller-sized list an update would PERSIST is accepted at the ceiling this emulator
/// holds a stored list to and refused one entry past it, with the modelled
/// `ValidationException` naming the member — the bound the create path shares.
#[tokio::test]
async fn a_persisted_list_is_accepted_at_its_bound_and_refused_past_it() {
    let router = emulator();
    let (cluster_id, queue_id, _groups) =
        cluster_with_a_queue(&router, "bounded-queue-cluster", "bounded-queue").await;

    // Rows: each unbounded list the update persists, at the published bound and one past it.
    // Entries that name no group are what makes the list's LENGTH the only thing under test.
    for member in [ASSOCIATIONS_MEMBER, "slurmConfiguration.slurmCustomSettings"] {
        for (entries, expected) in [
            (MAX_STORED_LIST_ENTRIES, 200u16),
            (MAX_STORED_LIST_ENTRIES + 1, CLIENT_ERROR_STATUS),
        ] {
            let patch = if member == ASSOCIATIONS_MEMBER {
                let configurations: Vec<Value> = (0..entries).map(|_| json!({})).collect();
                json!({ASSOCIATIONS_MEMBER: configurations})
            } else {
                let settings: Vec<Value> = (0..entries)
                    .map(|index| slurm_setting(&format!("Setting{index}"), "1"))
                    .collect();
                json!({"slurmConfiguration": {"slurmCustomSettings": settings}})
            };

            let (status, body) = call(
                &router,
                "UpdateQueue",
                &update_request(&cluster_id, &queue_id, &patch),
            )
            .await;
            assert_eq!(
                status, expected,
                "case {member} with {entries} entries: expected {expected}, got {status} \
                 with {body}"
            );
            if expected != 200 {
                assert_eq!(
                    body["__type"],
                    json!("ValidationException"),
                    "case {member} with {entries} entries: expected a validation failure, \
                     got {body}"
                );
                assert_eq!(
                    body["fieldList"][0]["name"],
                    json!(member),
                    "case {member} with {entries} entries: expected the refusal to name \
                     {member}, got {body}"
                );
            }
        }
    }
}

/// RULE: an `@idempotencyToken` retry returns the first call's result and has no further
/// effect, while the same token presented for ANOTHER queue is a conflict rather than a
/// second update.
#[tokio::test]
async fn a_reused_client_token_replays_the_first_update() {
    let router = emulator();
    let (cluster_id, queue_id, groups) =
        cluster_with_a_queue(&router, "token-queue-cluster", "token-queue").await;
    let (_first_name, first_id) = groups[0].clone();
    let (_second_name, second_id) = groups[1].clone();
    let token = "update-queue-token-0001";

    let mut first_request = update_request(
        &cluster_id,
        &queue_id,
        &json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &first_id}]}),
    );
    first_request["clientToken"] = json!(token);
    let (status, first) = call(&router, "UpdateQueue", &first_request).await;
    assert_eq!(status, 200, "case first update: got {status} with {first}");

    // The retry carries the token AND another value: the first call's result is what it
    // answers, and the second value is not applied.
    let mut retry = update_request(
        &cluster_id,
        &queue_id,
        &json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &second_id}]}),
    );
    retry["clientToken"] = json!(token);
    let (status, replayed) = call(&router, "UpdateQueue", &retry).await;
    assert_eq!(status, 200, "case retry: got {status} with {replayed}");
    assert_eq!(
        replayed["queue"][ASSOCIATIONS_MEMBER],
        json!([{"computeNodeGroupId": &first_id}]),
        "case retry: expected the first call's associations, got {replayed}"
    );
    let read = stored_queue(&router, &cluster_id, &queue_id).await;
    assert_eq!(
        read[ASSOCIATIONS_MEMBER],
        json!([{"computeNodeGroupId": &first_id}]),
        "case retry: expected the retry to have no further effect, got {read}"
    );

    // The same token for ANOTHER queue is not that retry.
    let (status, created) = call(
        &router,
        "CreateQueue",
        &create_queue(&cluster_id, "other-queue", &[]),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateQueue got {status} with {created}");
    let other_id = created["queue"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut other_request = update_request(
        &cluster_id,
        &other_id,
        &json!({ASSOCIATIONS_MEMBER: [{"computeNodeGroupId": &first_id}]}),
    );
    other_request["clientToken"] = json!(token);
    let (status, refused) = call(&router, "UpdateQueue", &other_request).await;
    assert_eq!(
        (status, refused["__type"].as_str()),
        (CLIENT_ERROR_STATUS, Some("ConflictException")),
        "case another queue on a used token: expected a conflict, got {status} with {refused}"
    );
    assert_eq!(
        refused["resourceId"],
        json!(queue_id),
        "case another queue on a used token: expected the refusal to name the queue the \
         token was used for, got {refused}"
    );
    let read = stored_queue(&router, &cluster_id, &other_id).await;
    assert_eq!(
        read[ASSOCIATIONS_MEMBER],
        json!([]),
        "case another queue on a used token: expected the refusal to store nothing, got {read}"
    );
}
