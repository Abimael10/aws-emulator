// SPDX-License-Identifier: CC0-1.0

//! How the calls that ask to delete ONE cluster resolve to ONE deletion, driven end
//! to end through the same `MultiServiceRouter` the production binary mounts.
//!
//! RULE: the calls that ask for the same resource's deletion are serialised on
//! STARTING it — the call that starts it is accepted with the resource in its
//! deleting status, a call that arrives while another call is starting that same
//! deletion is refused `ResourceInUseException` (409) because the resource it names
//! is in use by the transition the other call is making, and a call that arrives
//! once the deletion is RUNNING is answered with it (200, the deleting status, the
//! ARN the resource always had) rather than refused or told the resource is gone.
//!
//! Why it matters: a deleter that retries (CDK, Terraform, a controller) must be
//! able to re-send its own delete and be told the deletion it asked for is
//! happening, while a racer that lost a transition must be told to wait for the
//! in-flight one rather than that the resource never existed — the two answers are
//! in different retry classes.
//!
//! The rows differ along what the rule generalises over: the status the cluster was
//! in when the delete arrived (`ACTIVE`, still `CREATING`), the Region the request is
//! signed for, how many times the delete is re-sent, and how the call that does not
//! start the deletion ends (refused as a loser, answered as a re-send).
//!
//! The wire harness (`Answer`, `emulator_with_engine`, `call`, `encode_label`) is the
//! shared one in `tests/all/wire.rs` and the `CreateCluster` body is
//! `cluster_lifecycle`'s: no suite grows its own client.

use std::sync::Arc;

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    cluster_being_deleted, ComAmazonawsEksAwswesleyfrontendState, STATUS_DELETING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{minimal_request, REGION};
use crate::wire::{call, emulator_with_engine, encode_label, error_code, read_until, Answer};

/// Another Region a request may be signed for, so a row can show the rule holding
/// wherever the cluster lives.
const OTHER_REGION: &str = "eu-west-1";

async fn create_cluster(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "POST",
        "/clusters",
        &minimal_request(name).to_string(),
    )
    .await
}

async fn describe_cluster(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "GET",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

async fn delete_cluster(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "DELETE",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

/// The store behind the emulator under test, which is where a precondition only one
/// in-flight CALL could otherwise stand up is written.
fn store(
    engine: &Arc<engine::InMemoryEngine>,
) -> ::engine::ServiceState<ComAmazonawsEksAwswesleyfrontendState> {
    engine.state::<ComAmazonawsEksAwswesleyfrontendState>()
}

/// Whether a call is holding the claim on starting the deletion of one cluster.
fn claimed(engine: &Arc<engine::InMemoryEngine>, region: &str, name: &str) -> bool {
    store(engine)
        .lock()
        .cluster_delete_claimed(&(region.to_string(), name.to_string()))
}

/// One row: the cluster the delete arrives for.
struct Row {
    case: &'static str,
    region: &'static str,
    name: &'static str,
    /// Whether the clock is given its `CREATING` -> `ACTIVE` move before the delete.
    settle: bool,
    /// How many times the delete is sent again once the deletion is running.
    resends: usize,
}

// ---------------------------------------------------------------------------
// RULE: a call that arrives while another call is STARTING the deletion is the
// losing side and is refused as a resource in use; the deletion it lost is the one
// that runs.
// ---------------------------------------------------------------------------

/// The overlap is driven by STANDING THE WINNER UP: the claim a delete call holds
/// while it is making the transition is taken on the store, which is exactly the
/// state the losing side of two overlapping calls arrives in, and is then given
/// back the way the winning call gives it back.
#[tokio::test]
async fn a_delete_that_overlaps_the_call_starting_the_deletion_is_refused() {
    let rows = [
        Row {
            case: "a cluster that reached ACTIVE",
            region: REGION,
            name: "serialize-active",
            settle: true,
            resends: 1,
        },
        Row {
            case: "a cluster still CREATING",
            region: REGION,
            name: "serialize-creating",
            settle: false,
            resends: 1,
        },
        Row {
            case: "a cluster in another Region",
            region: OTHER_REGION,
            name: "serialize_other-2",
            settle: true,
            resends: 3,
        },
    ];

    for row in rows {
        let Row {
            case,
            region,
            name,
            settle,
            resends,
        } = row;
        // ONE emulator per row, so the store assertions name exactly this cluster.
        let (router, engine) = emulator_with_engine();
        let created = create_cluster(&router, region, name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: setup: the cluster must be created, got {created:?}"
        );
        if settle {
            read_until(
                case,
                "the cluster reported ACTIVE",
                || describe_cluster(&router, region, name),
                |read| read.cluster()["status"] == serde_json::json!("ACTIVE"),
            )
            .await;
        }

        // THE WINNER IS MAKING THE TRANSITION: it holds the claim the calls asking
        // for this deletion are serialised on.
        assert!(
            store(&engine)
                .lock()
                .claim_cluster_delete((region.to_string(), name.to_string())),
            "case {case:?}: setup: the claim must be free before any delete is sent"
        );

        // A call that arrives now is the losing side: the cluster it names is in use
        // by the transition the other call is making, not absent.
        let loser = delete_cluster(&router, region, name).await;
        assert_eq!(
            (loser.status, error_code(&loser).as_str()),
            (409, "ResourceInUseException"),
            "case {case:?}: a delete overlapping the call that starts the deletion is a \
             conflict, got {loser:?}"
        );
        assert_eq!(
            loser.message(),
            cluster_being_deleted(name),
            "case {case:?}: expected the in-flight-deletion sentence, got {:?}",
            loser.message()
        );
        // And it made no transition: the cluster is exactly as it was.
        let untouched = describe_cluster(&router, region, name).await;
        assert_eq!(
            untouched.status, 200,
            "case {case:?}: the refused delete must leave the cluster readable, got {untouched:?}"
        );
        assert_ne!(
            untouched.cluster()["status"],
            serde_json::json!(STATUS_DELETING),
            "case {case:?}: the refused delete must start no deletion, got {untouched:?}"
        );

        // THE WINNER FINISHES, giving the claim back the way the call that took it
        // does, and the deletion it started is the one that runs.
        store(&engine)
            .lock()
            .release_cluster_delete(&(region.to_string(), name.to_string()));
        let accepted = delete_cluster(&router, region, name).await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: the delete must be accepted once no call is starting one, got \
             {accepted:?}"
        );
        assert_eq!(
            accepted.cluster()["status"],
            serde_json::json!(STATUS_DELETING),
            "case {case:?}: the accepted delete reports {STATUS_DELETING}, got {accepted:?}"
        );

        // THE DELETE SENT AGAIN IS ANSWERED WITH THAT SAME DELETION, however many
        // times it is sent: the same ARN, the same status, nothing started twice.
        for resend in 1..=resends {
            let again = delete_cluster(&router, region, name).await;
            assert_eq!(
                again.status, 200,
                "case {case:?}: re-send {resend} must be answered with the running deletion, \
                 got {again:?}"
            );
            assert_eq!(
                (&again.cluster()["status"], &again.cluster()["arn"]),
                (
                    &accepted.cluster()["status"],
                    &accepted.cluster()["arn"]
                ),
                "case {case:?}: re-send {resend} must answer what the accepted delete did, got \
                 {again:?}"
            );
        }

        // No call is starting a deletion any more, so nothing holds the claim: the
        // call that took it gave it back when it ended.
        assert!(
            !claimed(&engine, region, name),
            "case {case:?}: no claim may be left behind once the delete has answered"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE, through the wire: however many calls ask for one cluster's deletion at
// once, ONE deletion runs.
// ---------------------------------------------------------------------------

/// Calls sent CONCURRENTLY, with nothing standing anything up: whatever order the
/// runtime serves them in, at least one is accepted, every accepted answer names
/// the one cluster in its deleting status, every refusal is the conflict a resource
/// in use earns, and the store holds exactly one deletion for that cluster.
///
/// The rows differ along how many calls race: two, as a retrying deleter sends, and
/// four, which is the same rule with more losers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_deletes_of_one_cluster_resolve_to_one_deletion() {
    for racers in [2_usize, 4] {
        let case = format!("{racers} concurrent deletes");
        let (router, engine) = emulator_with_engine();
        let router = Arc::new(router);
        let name = "concurrent-delete";
        let created = create_cluster(&router, REGION, name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: setup: the cluster must be created, got {created:?}"
        );
        read_until(
            &case,
            "the cluster reported ACTIVE",
            || describe_cluster(&router, REGION, name),
            |read| read.cluster()["status"] == serde_json::json!("ACTIVE"),
        )
        .await;

        // Each call is its own task, so they really do overlap rather than running
        // one after the other.
        let mut racing = Vec::new();
        for _ in 0..racers {
            let router = Arc::clone(&router);
            racing.push(tokio::spawn(async move {
                delete_cluster(&router, REGION, name).await
            }));
        }
        let mut answers = Vec::new();
        for racer in racing {
            answers.push(racer.await.expect("a racing delete must answer"));
        }

        let accepted: Vec<&Answer> = answers.iter().filter(|answer| answer.status == 200).collect();
        assert!(
            !accepted.is_empty(),
            "case {case:?}: at least one racer must be accepted, got {answers:?}"
        );
        let arn = accepted[0].cluster()["arn"].clone();
        for answer in &accepted {
            assert_eq!(
                answer.cluster()["status"],
                serde_json::json!(STATUS_DELETING),
                "case {case:?}: an accepted racer reports {STATUS_DELETING}, got {answer:?}"
            );
            assert_eq!(
                answer.cluster()["arn"],
                arn,
                "case {case:?}: every accepted racer names the one cluster, got {answer:?}"
            );
        }
        for answer in answers.iter().filter(|answer| answer.status != 200) {
            assert_eq!(
                (answer.status, error_code(answer).as_str()),
                (409, "ResourceInUseException"),
                "case {case:?}: a racer that lost the transition is a conflict, got {answer:?}"
            );
            assert_eq!(
                answer.message(),
                cluster_being_deleted(name),
                "case {case:?}: expected the in-flight-deletion sentence, got {:?}",
                answer.message()
            );
        }

        // ONE deletion runs, whatever the racers were told, and nothing holds the
        // claim once they have all answered.
        {
            let store = store(&engine);
            let guard = store.lock();
            let key = (REGION.to_string(), name.to_string());
            assert_eq!(
                guard.cluster_deletions.len(),
                1,
                "case {case:?}: exactly one deletion may be recorded, got {:?}",
                guard.cluster_deletions
            );
            assert!(
                guard.cluster_deletions.contains_key(&key),
                "case {case:?}: the recorded deletion must be this cluster's, got {:?}",
                guard.cluster_deletions
            );
            assert_eq!(
                guard.clusters.get(&key).map(|record| record.status.clone()),
                Some(STATUS_DELETING.to_string()),
                "case {case:?}: the cluster must be left deleting, got {:?}",
                guard.clusters.get(&key).map(|record| record.status.clone())
            );
        }
        assert!(
            !claimed(&engine, REGION, name),
            "case {case:?}: no claim may be left behind once every racer has answered"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: the claim is given back however the call that took it ends, so one call's
// outcome never blocks the next one.
// ---------------------------------------------------------------------------

/// Each row is a delete that ends differently — accepted, refused for a dependency
/// the cluster carries, refused for a cluster that is not there — and after every
/// one of them nothing holds the claim.
#[tokio::test]
async fn a_delete_leaves_no_claim_behind_however_it_ends() {
    let (router, engine) = emulator_with_engine();

    // A delete of a cluster this emulator does not hold.
    let absent = delete_cluster(&router, REGION, "claim-absent").await;
    assert_eq!(
        (absent.status, error_code(&absent).as_str()),
        (404, "ResourceNotFoundException"),
        "a delete of a cluster that is not there is a not-found, got {absent:?}"
    );
    assert!(
        !claimed(&engine, REGION, "claim-absent"),
        "a not-found delete may leave no claim behind"
    );

    // A delete a rule on the cluster refuses: deletion protection is on.
    let protected = "claim-protected";
    let mut body = minimal_request(protected);
    body["deletionProtection"] = serde_json::json!(true);
    let created = call(&router, REGION, "POST", "/clusters", &body.to_string()).await;
    assert_eq!(
        created.status, 200,
        "setup: the protected cluster must be created, got {created:?}"
    );
    let refused = delete_cluster(&router, REGION, protected).await;
    assert_eq!(
        refused.status, 400,
        "a delete of a protected cluster is refused, got {refused:?}"
    );
    assert!(
        !claimed(&engine, REGION, protected),
        "a refused delete may leave no claim behind"
    );

    // And a delete that is accepted: the claim is given back once the whole delete
    // has run, which is before the call answers.
    let deleted = "claim-deleted";
    let created = create_cluster(&router, REGION, deleted).await;
    assert_eq!(
        created.status, 200,
        "setup: the cluster must be created, got {created:?}"
    );
    let accepted = delete_cluster(&router, REGION, deleted).await;
    assert_eq!(
        accepted.status, 200,
        "the delete must be accepted, got {accepted:?}"
    );
    assert!(
        !claimed(&engine, REGION, deleted),
        "an accepted delete may leave no claim behind"
    );
}
