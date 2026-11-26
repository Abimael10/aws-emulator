// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `FlushStageCache`: emptying the cache of one stage of a REST
//! API (`DELETE /restapis/{restApiId}/stages/{stageName}/cache/data`).
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees. The emulator, the request builder and the `CreateRestApi` / `CreateDeployment`
//! helpers are this crate's existing harness, reused from `rest_api_lifecycle` and
//! `stage_lifecycle`.
//!
//! The rule under test, over a TABLE of stages rather than one recorded call: a flush of
//! a stage's cache data is accepted (202) for every stage that exists, empties only the
//! addressed stage's cache cluster — which is then reported `FLUSH_IN_PROGRESS` until the
//! flush span has passed and `AVAILABLE` after it — changes no other member of the
//! stage, is a successful no-op on a stage that has no cache cluster (whose status stays
//! `NOT_AVAILABLE`), and is a 404 `NotFoundException` when either path label names no
//! record.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    CACHE_CLUSTER_AVAILABLE, CACHE_CLUSTER_FLUSH_IN_PROGRESS, CACHE_CLUSTER_NOT_AVAILABLE,
};

use crate::rest_api_lifecycle::{assert_failure, create_api, without, Answer, Gateway};
use crate::stage_lifecycle::{create_deployment, get_stage};

/// How long a poll for a transition the clock owes keeps looking, and how often it looks.
/// Generous: nothing here is timed, and the deadline is only what stops a test whose
/// condition will never hold.
const WAIT: Duration = Duration::from_secs(30);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// Read `stage_name` back until its `cacheClusterStatus` is `wanted`, answering the stage
/// body it reported then: what the stage looks like once the transition the clock owes has
/// been materialised. Looks again until [`WAIT`] has passed, so nothing here depends on how
/// fast the host runs.
pub(crate) async fn polled_stage(
    gateway: &Gateway,
    api: &str,
    stage_name: &str,
    wanted: &str,
    case: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let read = get_stage(gateway, api, stage_name).await;
        if read.body["cacheClusterStatus"].as_str() == Some(wanted) {
            return read.body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case}: expected {wanted} from GetStage within {WAIT:?}, last body {:?}",
            read.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `FlushStageCache` for `stage_name` of `api`. Shared with the `delete_stage` suite,
/// which flushes a deleted stage's cache to show the record is gone for every reader.
pub(crate) async fn flush_cache(gateway: &Gateway, api: &str, stage_name: &str) -> Answer {
    gateway
        .call(
            "DELETE",
            &format!("/restapis/{api}/stages/{stage_name}/cache/data"),
            None,
        )
        .await
}

/// The `cacheClusterStatus` a stage body reports, named for the case. Shared with the
/// `flush_stage_authorizers_cache` suite, which checks an AUTHORIZER-cache flush leaves
/// the cluster where it was.
pub(crate) fn cluster_status(body: &Value, case: &str) -> String {
    body["cacheClusterStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a cacheClusterStatus, got {body:?}"))
        .to_string()
}

/// The stage body with its `cacheClusterStatus` dropped: what a flush must leave alone.
fn without_cluster_status(body: &Value) -> Value {
    without(body, &["cacheClusterStatus"])
}

/// Create `stage_name` on `api` from `extra` (the cache members and whatever else the row
/// sets), answering the created stage's body. A stage that asked for a cluster is read back
/// until that cluster is `AVAILABLE` — a settled cluster — before the flush under test.
/// Shared with the `flush_stage_authorizers_cache` suite, which needs the same settled
/// stages.
pub(crate) async fn settled_stage(
    gateway: &Gateway,
    api: &str,
    deployment: &str,
    case: &str,
    stage_name: &str,
    extra: &Value,
) -> Value {
    let mut request = json!({ "stageName": stage_name, "deploymentId": deployment });
    if let (Some(target), Some(source)) = (request.as_object_mut(), extra.as_object()) {
        for (member, value) in source {
            target.insert(member.clone(), value.clone());
        }
    }
    let created = gateway
        .call("POST", &format!("/restapis/{api}/stages"), Some(request))
        .await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected 201 from CreateStage, got {} with {:?}",
        created.status, created.body
    );
    if extra["cacheClusterEnabled"] == json!(true) {
        return polled_stage(gateway, api, stage_name, CACHE_CLUSTER_AVAILABLE, case).await;
    }
    get_stage(gateway, api, stage_name).await.body
}

#[tokio::test]
async fn flushing_a_stage_cache_empties_that_cluster_and_changes_nothing_else() {
    // Rows differ along what the rule generalises over: whether the stage HAS a cache
    // cluster at all, the cluster's size (two of the sizes `CacheClusterSize` publishes:
    // 0.5 | 1.6 | 118 | 13.5 | 237 | 28.4 | 58.2 | 6.1), and which other stage members
    // the record carries — none of which the flush may touch.
    //
    // `expected_during` is the status a read taken straight after the flush must report,
    // `expected_after` the one it reports once the flush span has passed.
    let cases: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "no cache cluster",
            "plain",
            json!({}),
            CACHE_CLUSTER_NOT_AVAILABLE,
            CACHE_CLUSTER_NOT_AVAILABLE,
        ),
        (
            "no cache cluster, with variables and tags",
            "plain_configured",
            json!({
                "description": "no cache here",
                "variables": { "table": "pets" },
                "tags": { "team": "api" }
            }),
            CACHE_CLUSTER_NOT_AVAILABLE,
            CACHE_CLUSTER_NOT_AVAILABLE,
        ),
        (
            "the smallest published cluster size",
            "cached-small",
            json!({ "cacheClusterEnabled": true, "cacheClusterSize": "0.5" }),
            CACHE_CLUSTER_FLUSH_IN_PROGRESS,
            CACHE_CLUSTER_AVAILABLE,
        ),
        (
            "a bigger published cluster size, with other members set",
            "cached_13_5",
            json!({
                "cacheClusterEnabled": true,
                "cacheClusterSize": "13.5",
                "description": "the cached stage",
                "tracingEnabled": true,
                "variables": { "region": "us-east-1" }
            }),
            CACHE_CLUSTER_FLUSH_IN_PROGRESS,
            CACHE_CLUSTER_AVAILABLE,
        ),
    ];

    let gateway = Gateway::new();
    let (api, _) = create_api(&gateway, "flush-stage-cache").await;
    let deployment = create_deployment(
        &gateway,
        &api,
        "CreateDeployment for the flush cases",
        json!({}),
    )
    .await;

    for (case, stage_name, extra, expected_during, expected_after) in &cases {
        let before = settled_stage(&gateway, &api, &deployment, case, stage_name, extra).await;

        let flushed = flush_cache(&gateway, &api, stage_name).await;
        assert_eq!(
            flushed.status, 202,
            "case {case}: expected 202 from FlushStageCache, got {} with {:?}",
            flushed.status, flushed.body
        );
        assert_eq!(
            flushed.body,
            Value::Null,
            "case {case}: FlushStageCache answers no body, got {:?}",
            flushed.body
        );

        // Straight after the call the addressed cluster is being emptied; a stage with
        // no cluster had nothing to empty and reports what it did before.
        let during = get_stage(&gateway, &api, stage_name).await;
        assert_eq!(
            during.status, 200,
            "case {case}: expected 200 from GetStage after the flush, got {} with {:?}",
            during.status, during.body
        );
        let during_status = cluster_status(&during.body, case);
        assert_eq!(
            during_status, *expected_during,
            "case {case}: expected {expected_during} straight after the flush, got {during_status}"
        );
        // The flush empties the cache; it is not a stage update, so every other member
        // of the record reads back exactly as it did before the call.
        assert_eq!(
            without_cluster_status(&during.body),
            without_cluster_status(&before),
            "case {case}: the flush must change only the cluster status, expected {:?}, got {:?}",
            without_cluster_status(&before),
            without_cluster_status(&during.body)
        );

        // A second flush while one is outstanding is accepted too: the call is
        // idempotent, there is no conflict to report.
        let again = flush_cache(&gateway, &api, stage_name).await;
        assert_eq!(
            again.status, 202,
            "case {case}: expected 202 from a repeated FlushStageCache, got {} with {:?}",
            again.status, again.body
        );

        // The return to a settled cluster is one the clock owes, and it is owed to every
        // reader, not just the first.
        polled_stage(&gateway, &api, stage_name, expected_after, case).await;
        for round in 0..2 {
            let settled = get_stage(&gateway, &api, stage_name).await;
            let status = cluster_status(&settled.body, case);
            assert_eq!(
                status, *expected_after,
                "case {case} (round {round}): expected {expected_after} once the flush span \
                 has passed, got {status}"
            );
            assert_eq!(
                without_cluster_status(&settled.body),
                without_cluster_status(&before),
                "case {case} (round {round}): the settled stage must be the stored record, \
                 expected {:?}, got {:?}",
                without_cluster_status(&before),
                without_cluster_status(&settled.body)
            );
        }
    }
}

#[tokio::test]
async fn a_flush_empties_only_the_stage_it_addresses() {
    let gateway = Gateway::new();
    let (api, _) = create_api(&gateway, "flush-one-stage").await;
    let other_api_name = "flush-one-stage-other";
    let (other_api, _) = create_api(&gateway, other_api_name).await;
    let deployment =
        create_deployment(&gateway, &api, "CreateDeployment for the targets", json!({})).await;
    let other_deployment = create_deployment(
        &gateway,
        &other_api,
        "CreateDeployment on the other API",
        json!({}),
    )
    .await;

    let cached = json!({ "cacheClusterEnabled": true, "cacheClusterSize": "0.5" });
    // Two cached stages of one API, and a cached stage of ANOTHER API under the SAME
    // stage name: a flush is addressed by (API, stage), so neither of the other two may
    // move.
    for (case, stage) in [("the flushed stage", "prod"), ("a sibling stage", "beta")] {
        settled_stage(&gateway, &api, &deployment, case, stage, &cached).await;
    }
    settled_stage(
        &gateway,
        &other_api,
        &other_deployment,
        "the same stage name on another API",
        "prod",
        &cached,
    )
    .await;

    let flushed = flush_cache(&gateway, &api, "prod").await;
    assert_eq!(
        flushed.status, 202,
        "case FlushStageCache(prod): expected 202, got {} with {:?}",
        flushed.status, flushed.body
    );

    for (case, target_api, stage, expected) in [
        ("the addressed stage", &api, "prod", CACHE_CLUSTER_FLUSH_IN_PROGRESS),
        ("the sibling stage", &api, "beta", CACHE_CLUSTER_AVAILABLE),
        (
            "the same name on another API",
            &other_api,
            "prod",
            CACHE_CLUSTER_AVAILABLE,
        ),
    ] {
        let read = get_stage(&gateway, target_api, stage).await;
        let status = cluster_status(&read.body, case);
        assert_eq!(
            status, expected,
            "case {case}: expected {expected} after flushing {api}/prod, got {status}"
        );
    }
}

#[tokio::test]
async fn flushing_a_cache_the_request_cannot_resolve_is_a_not_found() {
    let gateway = Gateway::new();
    let (api, _) = create_api(&gateway, "flush-notfound").await;
    let (other_api, _) = create_api(&gateway, "flush-notfound-other").await;
    let deployment =
        create_deployment(&gateway, &api, "CreateDeployment for the refusals", json!({})).await;
    let cached = json!({ "cacheClusterEnabled": true, "cacheClusterSize": "0.5" });
    settled_stage(
        &gateway,
        &api,
        &deployment,
        "the stage the refusals are measured against",
        "prod",
        &cached,
    )
    .await;

    // Rows span each identifier the call resolves, one unresolvable at a time.
    let missing = "zzzzzzzzzz";
    let cases: Vec<(&str, String)> = vec![
        ("an unknown restApiId", format!("{missing}/prod")),
        ("an unknown stageName", format!("{api}/{missing}")),
        (
            "a stage name stored on another API",
            format!("{other_api}/prod"),
        ),
        ("both labels unknown", format!("{missing}/{missing}")),
    ];

    for (case, target) in &cases {
        let (target_api, stage) = target
            .split_once('/')
            .expect("every row names an API and a stage");
        assert_failure(
            &flush_cache(&gateway, target_api, stage).await,
            404,
            "NotFoundException",
            case,
        );
    }

    // A refusal writes nothing: the stage that does exist is still settled.
    let read = get_stage(&gateway, &api, "prod").await;
    let status = cluster_status(&read.body, "the stored stage after the refusals");
    assert_eq!(
        status, CACHE_CLUSTER_AVAILABLE,
        "case the stored stage after the refusals: expected {CACHE_CLUSTER_AVAILABLE}, got {status}"
    );
}

#[tokio::test]
async fn a_flush_asked_for_right_after_the_create_is_outstanding_on_its_own() {
    let gateway = Gateway::new();
    let (api, _) = create_api(&gateway, "flush-right-after-create").await;
    let deployment = create_deployment(
        &gateway,
        &api,
        "CreateDeployment for the flush right after the create",
        json!({}),
    )
    .await;

    // The flush is asked for in the caller's very next request after the create, with
    // nothing waited out in between.
    let created = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/stages"),
            Some(json!({
                "stageName": "creating",
                "deploymentId": deployment,
                "cacheClusterEnabled": true,
                "cacheClusterSize": "0.5"
            })),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case CreateStage(cacheClusterEnabled): expected 201, got {} with {:?}",
        created.status, created.body
    );

    let flushed = flush_cache(&gateway, &api, "creating").await;
    assert_eq!(
        flushed.status, 202,
        "case FlushStageCache(right after the create): expected 202, got {} with {:?}",
        flushed.status, flushed.body
    );

    // The cluster the create accepted is up by this next call, so what is outstanding is
    // the flush itself: it is not swallowed by the creation it followed.
    let during_flush = get_stage(&gateway, &api, "creating").await;
    let flushing = cluster_status(&during_flush.body, "right after the flush");
    assert_eq!(
        flushing, CACHE_CLUSTER_FLUSH_IN_PROGRESS,
        "case right after the flush: expected {CACHE_CLUSTER_FLUSH_IN_PROGRESS}, got {flushing}"
    );

    // And the return to a settled cluster is the transition the clock then owes.
    polled_stage(
        &gateway,
        &api,
        "creating",
        CACHE_CLUSTER_AVAILABLE,
        "once the flush span has passed",
    )
    .await;
}
