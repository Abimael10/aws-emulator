// SPDX-License-Identifier: CC0-1.0

//! The refusal each operation ADDRESSED TO ONE UPDATE BY ITS ID words for an id that
//! resolves to no update of the resource the request names, driven end to end over the
//! same `MultiServiceRouter` the production binary mounts.
//!
//! The RULE: every operation that addresses one update by its `updateId` refuses an id
//! it cannot resolve with 404 `ResourceNotFoundException` carrying THAT OPERATION's own
//! fixed sentence — the read (`DescribeUpdate`) and the cancellation (`CancelUpdate`)
//! each have one, so two operations asked for the SAME id on the SAME cluster answer two
//! DIFFERENT sentences, and the only variable part of either is the id the request
//! carried. Neither refusal changes what a later caller sees: the cluster's updates are
//! exactly the ones that were minted.
//!
//! `tests/all/refusal_contract.rs` already pins each sentence's own text over a table of
//! ids and scopes; what this suite adds is the part a single shared helper satisfies and
//! the rule does not — that the two operations' sentences are DISTINCT for one id on one
//! cluster, under every member `DescribeUpdate` scopes its lookup by — and that a refused
//! lookup writes nothing. The sentences are written out here as AWS words them and then
//! checked against the helpers the handlers compose them with, so neither a handler nor a
//! helper can drift from this text while the suite is green.
//!
//! The wire plumbing is the crate's one copy (`tests/all/wire.rs`) and the cluster and
//! update calls are the ones the suites owning those slices already export
//! (`tests/all/certificate_authority.rs`); only the two paths no suite exports —
//! `ListUpdates` and `CancelUpdate` — are below. Nothing here waits out a span: the
//! clock-owed cluster move is waited for by looking again.

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    cancel_update_not_found_message, describe_update_not_found_message,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::{
    append_authority, create_cluster, describe_cluster, describe_update, REGION,
};
use crate::wire::{call, emulator, encode_label, error_code, read_until, Answer};

/// The modelled class of both refusals: "the specified resource could not be found".
const NOT_FOUND: &str = "ResourceNotFoundException";

/// `ListUpdates`: `GET /clusters/{name}/updates` — the listing that says which ids the
/// cluster actually holds, so a refusal can be shown to have added none.
async fn list_updates(router: &MultiServiceRouter, cluster: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("/clusters/{}/updates", encode_label(cluster)),
        "",
    )
    .await
}

/// `CancelUpdate`: `POST /clusters/{name}/updates/{updateId}/cancel-update`.
async fn cancel_update(router: &MultiServiceRouter, cluster: &str, update_id: &str) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!(
            "/clusters/{}/updates/{}/cancel-update",
            encode_label(cluster),
            encode_label(update_id)
        ),
        "{}",
    )
    .await
}

/// Which operation asks for the update. `DescribeUpdate` carries the query string the
/// row narrows its lookup with — a resource inside the cluster whose updates are then
/// the only ones that may answer (empty for the cluster's own control plane).
#[derive(Debug, Clone, Copy)]
enum Asked {
    Describe(&'static str),
    Cancel,
}

/// Ask for `update_id` on `cluster` the way `asked` names.
async fn ask(
    router: &MultiServiceRouter,
    asked: Asked,
    cluster: &str,
    update_id: &str,
) -> Answer {
    match asked {
        Asked::Cancel => cancel_update(router, cluster, update_id).await,
        Asked::Describe(scope) => {
            if scope.is_empty() {
                describe_update(router, REGION, cluster, update_id).await
            } else {
                call(
                    router,
                    REGION,
                    "GET",
                    &format!(
                        "/clusters/{}/updates/{}?{scope}",
                        encode_label(cluster),
                        encode_label(update_id)
                    ),
                    "",
                )
                .await
            }
        }
    }
}

/// AWS's sentence for the operation that asked, and the helper this crate composes it
/// with — written out here so the helper is checked against the text rather than the
/// text read off the helper.
fn expected_sentence(asked: Asked, update_id: &str) -> (String, String) {
    match asked {
        Asked::Describe(_) => (
            format!("No update found for ID: {update_id}"),
            describe_update_not_found_message(update_id),
        ),
        Asked::Cancel => (
            format!("Update {update_id} not found"),
            cancel_update_not_found_message(update_id),
        ),
    }
}

/// What the rule requires of ONE refusal: the 404 class, the modelled code, and the
/// asking operation's own sentence with the requested id as its only variable part.
fn assert_refused_as_missing_update(case: &str, asked: Asked, answer: &Answer, update_id: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected HTTP 404, got {answer:?}"
    );
    assert_eq!(
        error_code(answer),
        NOT_FOUND,
        "case {case:?}: expected {NOT_FOUND}, got {answer:?}"
    );
    let (expected, from_helper) = expected_sentence(asked, update_id);
    let actual = answer.message();
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {expected:?}, got {actual:?}"
    );
    assert_eq!(
        from_helper, expected,
        "case {case:?}: the helper this operation words the refusal with must be AWS's \
         sentence, got {from_helper:?}"
    );
}

/// An `ACTIVE` cluster with ONE real update of its own, and that update's id: the
/// precondition every row needs, plus the CONTROL that the read serves what it holds.
async fn cluster_with_one_update(router: &MultiServiceRouter, case: &str, cluster: &str) -> String {
    let created = create_cluster(router, REGION, cluster).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: setup CreateCluster expected 200, got {created:?}"
    );
    read_until(
        case,
        "the new cluster reached ACTIVE",
        || describe_cluster(router, REGION, cluster),
        |answer| answer.status == 200 && answer.cluster()["status"] == serde_json::json!("ACTIVE"),
    )
    .await;

    // A REAL producer of a cluster-plane update, so the id below is one the service
    // minted rather than one a test wrote into the store.
    let minted = append_authority(router, REGION, cluster, None).await;
    assert_eq!(
        minted.status, 200,
        "case {case:?}: setup CreateCertificateAuthority expected 200, got {minted:?}"
    );
    let update_id = minted.body["update"]["id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the minted update must report its id, got {minted:?}")
        })
        .to_owned();

    // CONTROL: that id IS served, so the refusals below are about the ids they name and
    // not about an operation that refuses everything.
    let served = describe_update(router, REGION, cluster, &update_id).await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: the minted update id is readable, got {served:?}"
    );
    update_id
}

/// The ids a row asks for that resolve to no update of the cluster, differing along the
/// SHAPE of the value: a well-formed UUID nothing minted, the minted id in the wrong
/// case (ids are case-sensitive), a value that is no UUID at all, one carrying a
/// character path encoding must carry through, a single character, and a long one.
fn unresolvable_ids(minted: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "a well-formed UUID nothing minted",
            "11111111-2222-3333-4444-555555555555".to_string(),
        ),
        ("the minted id in upper case", minted.to_uppercase()),
        ("a value that is not a UUID", "not-an-update-id".to_string()),
        (
            "a value needing path encoding",
            "update id/with slash".to_string(),
        ),
        ("a single character", "z".to_string()),
        ("a 300-character value", "7".repeat(300)),
    ]
}

/// The members `DescribeUpdate` may scope its lookup by
/// (`smithy_query query="operation" operation="DescribeUpdate"`: `nodegroupName`,
/// `addonName`, `capabilityName`), plus the unscoped read of the cluster's own plane.
const DESCRIBE_SCOPES: [(&str, &str); 4] = [
    ("DescribeUpdate", ""),
    (
        "DescribeUpdate scoped by nodegroupName",
        "nodegroupName=workers",
    ),
    ("DescribeUpdate scoped by addonName", "addonName=vpc-cni"),
    (
        "DescribeUpdate scoped by capabilityName",
        "capabilityName=ACK",
    ),
];

#[tokio::test]
async fn the_read_and_the_cancellation_refuse_one_missing_update_in_different_sentences() {
    const CLUSTER: &str = "per-operation-update-refusal";
    let router = emulator();
    let minted = cluster_with_one_update(&router, "setup", CLUSTER).await;

    for (row, update_id) in unresolvable_ids(&minted) {
        // ONE id, ONE cluster, asked for both ways.
        let cancelled = ask(&router, Asked::Cancel, CLUSTER, &update_id).await;
        assert_refused_as_missing_update(
            &format!("{row} / CancelUpdate"),
            Asked::Cancel,
            &cancelled,
            &update_id,
        );
        let cancel_sentence = cancelled.message();

        for (operation, scope) in DESCRIBE_SCOPES {
            let asked = Asked::Describe(scope);
            let case = format!("{row} / {operation}");
            let described = ask(&router, asked, CLUSTER, &update_id).await;
            assert_refused_as_missing_update(&case, asked, &described, &update_id);

            // Same id, same cluster, same 404 class — and NOT the same sentence: each
            // operation words the absence itself, so a client matching the read's text
            // never sees the cancellation's, nor the other way round.
            let read_sentence = described.message();
            assert_ne!(
                read_sentence, cancel_sentence,
                "case {case:?}: the read and the cancellation must not share one \
                 sentence, both answered {read_sentence:?}"
            );
            assert_ne!(
                read_sentence,
                expected_sentence(Asked::Cancel, &update_id).0,
                "case {case:?}: the read answered the cancellation's sentence, \
                 {read_sentence:?}"
            );
            assert_ne!(
                cancel_sentence,
                expected_sentence(asked, &update_id).0,
                "case {case:?}: the cancellation answered the read's sentence, \
                 {cancel_sentence:?}"
            );
        }
    }

    // The id that EXISTS but is asked for under a scope that cannot hold it is the
    // third way the lookup misses, beside an id never minted and one in the wrong case.
    // It is a read-only row: `CancelUpdate` takes no scoping member, so there is no id
    // it resolves for one operation and not the other.
    let scoped = DESCRIBE_SCOPES
        .into_iter()
        .filter(|(_, scope)| !scope.is_empty());
    for (operation, scope) in scoped {
        let asked = Asked::Describe(scope);
        let case = format!("an existing cluster-plane id / {operation}");
        let described = ask(&router, asked, CLUSTER, &minted).await;
        assert_refused_as_missing_update(&case, asked, &described, &minted);
    }
}

#[tokio::test]
async fn a_refused_update_lookup_leaves_the_cluster_s_updates_as_they_were() {
    const CLUSTER: &str = "refused-update-lookup-writes-nothing";
    let router = emulator();
    let minted = cluster_with_one_update(&router, "setup", CLUSTER).await;

    for (row, update_id) in unresolvable_ids(&minted) {
        for (operation, scope) in DESCRIBE_SCOPES {
            let asked = Asked::Describe(scope);
            let answer = ask(&router, asked, CLUSTER, &update_id).await;
            let case = format!("{row} / {operation}");
            assert_refused_as_missing_update(&case, asked, &answer, &update_id);
        }
        let answer = ask(&router, Asked::Cancel, CLUSTER, &update_id).await;
        let case = format!("{row} / CancelUpdate");
        assert_refused_as_missing_update(&case, Asked::Cancel, &answer, &update_id);

        // Neither refusal registered the id it could not resolve: the listing reports
        // the minted update and nothing else, and the minted id still reads.
        let listed = list_updates(&router, CLUSTER).await;
        assert_eq!(
            listed.status, 200,
            "case {row:?}: ListUpdates expected 200, got {listed:?}"
        );
        let ids: Vec<&str> = listed.body["updateIds"]
            .as_array()
            .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            ids,
            vec![minted.as_str()],
            "case {row:?}: the refusals must add no update, got {listed:?}"
        );
        let served = describe_update(&router, REGION, CLUSTER, &minted).await;
        assert_eq!(
            served.status, 200,
            "case {row:?}: the minted update must still read, got {served:?}"
        );
    }
}
