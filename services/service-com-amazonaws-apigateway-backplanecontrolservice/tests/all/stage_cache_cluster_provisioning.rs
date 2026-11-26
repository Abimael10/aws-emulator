// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PROVISIONING of a stage's response cache cluster: what
//! `cacheClusterStatus` a stage reports once a call has asked for a cluster, and what the
//! stage's per-method response caching does from then on.
//!
//! THE RULE UNDER TEST: a cache cluster a call asks for is provisioned by the time the
//! caller's NEXT request arrives — the answer that ACCEPTED the provisioning reports the
//! non-terminal `CREATE_IN_PROGRESS` it accepted, every later read reports the terminal
//! `AVAILABLE`, and from that point the stage's per-method response caching is in effect.
//! Nothing a client has to do makes it settle, and no reader sees it move back: a client
//! polling `GetStage` for a terminal `CacheClusterStatus` before shifting traffic is
//! answered by its first poll.
//!
//! Every case drives the real wire path — a `restJson1` request through the multi-service
//! router for the control plane, and an HTTP request on the api's own
//! `{rest-api-id}.execute-api.{region}.amazonaws.com` host for the data plane — so what is
//! asserted is what a client sees.
//!
//! The rows span what the rule generalises over: which CALL asks for the cluster
//! (`CreateStage`, `CreateDeployment`'s own `stageName` stage, an `UpdateStage` patch that
//! enables one, an `UpdateStage` patch that re-sizes one), the capacity asked for (four of
//! the sizes `CacheClusterSize` publishes), and which reader is asked afterwards
//! (`GetStage` and `GetStages`) — beside the control row of a stage that asked for no
//! cluster at all, which reports `NOT_AVAILABLE` to every reader.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    CACHE_CLUSTER_AVAILABLE, CACHE_CLUSTER_CREATE_IN_PROGRESS, CACHE_CLUSTER_NOT_AVAILABLE,
};

use crate::execute_api_data_plane::{
    deploy, integration_response, invoke, json_template, method_response, put_integration,
    put_method,
};
use crate::flush_stage_cache::flush_cache;
use crate::rest_api_lifecycle::{create_api, resource_under, string_member, Gateway};
use crate::stage_lifecycle::get_stage;

/// The resource the cached method sits on, and the stage every case deploys to.
const RESOURCE: &str = "items";
const STAGE: &str = "prod";

/// The `methodSettings` pointer that turns caching on for `GET /items`: the method path
/// `Stage$methodSettings` documents, with the resource path's `/` escaped as a JSON-Patch
/// reference token.
const CACHED_METHOD_POINTER: &str = "/~1items/GET";

/// The serial the method's integration ANSWERS, and the serial it answers once that answer
/// has been rewritten: what a request the integration answers reads, and what a request
/// answered from the stage's cluster must not.
const FIRST: &str = "first";
const SECOND: &str = "second";

/// The response template the cached method carries: it renders the serial the INTEGRATION
/// answered, so what a client reads says which answer was mapped — the one the cluster
/// holds or a fresh one.
const ECHO_TEMPLATE: &str = "{\"serial\":\"$input.path('$.serial')\"}";

/// The `requestTemplates` of a `MOCK` whose answer carries `serial`: a `MOCK` is "for
/// testing without actually invoking the backend" (`IntegrationType`), so its rendered
/// integration REQUEST is what it answers and the `statusCode` that rendering names selects
/// the integration response mapping it.
fn answers(serial: &str) -> Value {
    json!({ "application/json": format!("{{\"statusCode\":200,\"serial\":\"{serial}\"}}") })
}

/// `CreateDeployment` of `api` carrying `request`, answering the deployment's id.
async fn create_deployment(gateway: &Gateway, api: &str, case: &str, request: Value) -> String {
    let answer = gateway
        .call("POST", &format!("/restapis/{api}/deployments"), Some(request))
        .await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected 201 from CreateDeployment, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "id", case)
}

/// `CreateStage` of `stage_name` on `api` from `request`, answering the created stage's
/// body — the answer that ACCEPTS whatever cluster the request asked for.
async fn create_stage(gateway: &Gateway, api: &str, case: &str, request: Value) -> Value {
    let answer = gateway
        .call("POST", &format!("/restapis/{api}/stages"), Some(request))
        .await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected 201 from CreateStage, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// `UpdateStage` of `stage_name` on `api` with `operations`, answering the patched stage's
/// body.
async fn update_stage(
    gateway: &Gateway,
    api: &str,
    stage_name: &str,
    case: &str,
    operations: Value,
) -> Value {
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage_name}"),
            Some(json!({ "patchOperations": operations })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from UpdateStage, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The `cacheClusterStatus` of `body`, named for the case.
fn status_of(body: &Value, case: &str) -> String {
    body["cacheClusterStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a cacheClusterStatus, got {body:?}"))
        .to_string()
}

/// The `cacheClusterStatus` `GetStages` reports for `stage_name`: the OTHER reader of a
/// stage, which must agree with `GetStage` about the same record.
async fn listed_status(gateway: &Gateway, api: &str, stage_name: &str, case: &str) -> String {
    let listed = gateway
        .call("GET", &format!("/restapis/{api}/stages"), None)
        .await;
    assert_eq!(
        listed.status, 200,
        "case {case}: expected 200 from GetStages, got {} with {:?}",
        listed.status, listed.body
    );
    let item = listed.body["item"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a stage list, got {:?}", listed.body))
        .iter()
        .find(|stage| stage["stageName"].as_str() == Some(stage_name))
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected {stage_name} in the listing, got {:?}",
                listed.body
            )
        })
        .clone();
    status_of(&item, case)
}

/// An api carrying one deployable method on its root resource, and a deployment of it:
/// what a stage of these cases points at.
async fn api_with_a_deployment(gateway: &Gateway, name: &str) -> (String, String) {
    let (api, root) = create_api(gateway, name).await;
    put_method(
        gateway,
        &api,
        &root,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &root,
        "GET",
        json!({ "type": "MOCK", "requestTemplates": answers(FIRST) }),
    )
    .await;
    let deployment = create_deployment(
        gateway,
        &api,
        &format!("CreateDeployment for {name}"),
        json!({}),
    )
    .await;
    (api, deployment)
}

/// How the row's cluster is asked for.
enum AskedBy {
    /// `CreateStage` carries `cacheClusterEnabled`.
    CreateStage,
    /// `CreateDeployment` carries `stageName` with `cacheClusterEnabled`, so the stage it
    /// creates has the cluster. Its own answer is a `Deployment` and reports no stage
    /// member at all, so only the readers of the stage are asserted for this row.
    CreateDeployment,
    /// An `UpdateStage` patch enables one on a stage created without it.
    PatchEnabled,
    /// An `UpdateStage` patch re-sizes a cluster the stage already has, which provisions a
    /// cluster of the new capacity.
    PatchResized,
    /// Nothing asks for one: the control row.
    NotAskedFor,
}

#[tokio::test]
async fn a_cache_cluster_a_call_asks_for_is_up_for_the_callers_next_request() {
    // Rows differ along the CALL that asks for the cluster and the capacity it asks for.
    // The sizes are four of the eight the `CacheClusterSize` enum publishes
    // (`smithy_query(query="shape", shape="com.amazonaws.apigateway#CacheClusterSize")`:
    // 0.5 | 1.6 | 6.1 | 13.5 | 28.4 | 58.2 | 118 | 237), including the smallest and one
    // the issue's own request did not name.
    let cases: Vec<(&str, &str, AskedBy, &str, &str)> = vec![
        (
            "CreateStage asks for the smallest published capacity",
            "created_small",
            AskedBy::CreateStage,
            "0.5",
            CACHE_CLUSTER_AVAILABLE,
        ),
        (
            "CreateStage asks for a bigger published capacity",
            "created_big",
            AskedBy::CreateStage,
            "237",
            CACHE_CLUSTER_AVAILABLE,
        ),
        (
            "CreateDeployment's own stage asks for one",
            "deployed",
            AskedBy::CreateDeployment,
            "1.6",
            CACHE_CLUSTER_AVAILABLE,
        ),
        (
            "an UpdateStage patch enables one",
            "patched",
            AskedBy::PatchEnabled,
            "13.5",
            CACHE_CLUSTER_AVAILABLE,
        ),
        (
            "an UpdateStage patch re-sizes the one it has",
            "resized",
            AskedBy::PatchResized,
            "28.4",
            CACHE_CLUSTER_AVAILABLE,
        ),
        (
            "no cluster is asked for at all",
            "plain",
            AskedBy::NotAskedFor,
            "0.5",
            CACHE_CLUSTER_NOT_AVAILABLE,
        ),
    ];

    let gateway = Gateway::new();
    let (api, deployment) = api_with_a_deployment(&gateway, "cluster-provisioning").await;

    for (case, stage_name, asked_by, size, expected) in &cases {
        // The answer that ACCEPTS the provisioning reports the provisioning it accepted:
        // a cluster is accepted `CREATE_IN_PROGRESS`, and a stage that asked for none has
        // no cluster to report.
        let accepted = match asked_by {
            AskedBy::CreateStage => Some(
                create_stage(
                    &gateway,
                    &api,
                    case,
                    json!({
                        "stageName": stage_name,
                        "deploymentId": deployment,
                        "cacheClusterEnabled": true,
                        "cacheClusterSize": size
                    }),
                )
                .await,
            ),
            AskedBy::CreateDeployment => {
                create_deployment(
                    &gateway,
                    &api,
                    case,
                    json!({
                        "stageName": stage_name,
                        "cacheClusterEnabled": true,
                        "cacheClusterSize": size
                    }),
                )
                .await;
                None
            }
            AskedBy::PatchEnabled => {
                create_stage(
                    &gateway,
                    &api,
                    case,
                    json!({ "stageName": stage_name, "deploymentId": deployment }),
                )
                .await;
                Some(
                    update_stage(
                        &gateway,
                        &api,
                        stage_name,
                        case,
                        json!([
                            { "op": "replace", "path": "/cacheClusterEnabled", "value": "true" },
                            { "op": "replace", "path": "/cacheClusterSize", "value": size }
                        ]),
                    )
                    .await,
                )
            }
            AskedBy::PatchResized => {
                create_stage(
                    &gateway,
                    &api,
                    case,
                    json!({
                        "stageName": stage_name,
                        "deploymentId": deployment,
                        "cacheClusterEnabled": true,
                        "cacheClusterSize": "0.5"
                    }),
                )
                .await;
                Some(
                    update_stage(
                        &gateway,
                        &api,
                        stage_name,
                        case,
                        json!([
                            { "op": "replace", "path": "/cacheClusterSize", "value": size }
                        ]),
                    )
                    .await,
                )
            }
            AskedBy::NotAskedFor => Some(
                create_stage(
                    &gateway,
                    &api,
                    case,
                    json!({ "stageName": stage_name, "deploymentId": deployment }),
                )
                .await,
            ),
        };
        if let Some(accepted) = &accepted {
            let accepted_status = status_of(accepted, case);
            let expected_accept = match asked_by {
                AskedBy::NotAskedFor => CACHE_CLUSTER_NOT_AVAILABLE,
                _ => CACHE_CLUSTER_CREATE_IN_PROGRESS,
            };
            assert_eq!(
                accepted_status, expected_accept,
                "case {case}: expected {expected_accept} from the answer that accepted the \
                 provisioning, got {accepted_status}"
            );
        }

        // The caller's NEXT request reads the terminal value, with nothing waited out —
        // and so does every request after it, for every reader of the record.
        for round in 0..3 {
            let read = get_stage(&gateway, &api, stage_name).await;
            assert_eq!(
                read.status, 200,
                "case {case} (round {round}): expected 200 from GetStage, got {} with {:?}",
                read.status, read.body
            );
            let status = status_of(&read.body, case);
            assert_eq!(
                status, *expected,
                "case {case} (round {round}): expected {expected} from the next GetStage, got \
                 {status}"
            );
            // The capacity the request asked for is reported as it was asked for, whichever
            // call asked.
            if !matches!(asked_by, AskedBy::NotAskedFor) {
                assert_eq!(
                    read.body["cacheClusterSize"].as_str(),
                    Some(*size),
                    "case {case} (round {round}): expected the requested capacity back, got {:?}",
                    read.body["cacheClusterSize"]
                );
            }
        }
        let listed = listed_status(&gateway, &api, stage_name, case).await;
        assert_eq!(
            listed, *expected,
            "case {case}: GetStages must report what GetStage does, expected {expected}, got \
             {listed}"
        );
    }
}

#[tokio::test]
async fn a_cluster_that_is_up_for_the_next_request_caches_that_requests_response() {
    // Rows differ along whether the stage HAS a cache cluster: the one that asked for one
    // holds the response its first request earned — with nothing waited out between the
    // create and that request — and the one that did not is answered by its integration
    // every time. `FlushStageCache` then empties the cluster inside the live TTL, so the
    // request after it reads the integration's current answer again.
    let cases: &[(&str, &str, bool)] = &[
        ("a stage with a cache cluster", "cluster-caching", true),
        ("a stage with no cache cluster", "cluster-less", false),
    ];

    for (case, name, cluster) in cases {
        let gateway = Gateway::new();
        let (api, root) = create_api(&gateway, name).await;
        let resource = resource_under(&gateway, &api, &root, RESOURCE).await;
        put_method(
            &gateway,
            &api,
            &resource,
            "GET",
            json!({ "authorizationType": "NONE" }),
        )
        .await;
        put_integration(
            &gateway,
            &api,
            &resource,
            "GET",
            json!({ "type": "MOCK", "requestTemplates": answers(FIRST) }),
        )
        .await;
        method_response(&gateway, &api, &resource, "GET", "200", json!({})).await;
        integration_response(
            &gateway,
            &api,
            &resource,
            "GET",
            "200",
            json!({ "responseTemplates": json_template(ECHO_TEMPLATE) }),
        )
        .await;

        let deployment = create_deployment(
            &gateway,
            &api,
            &format!("CreateDeployment for {case}"),
            json!({}),
        )
        .await;
        let mut request = json!({ "stageName": STAGE, "deploymentId": deployment });
        if *cluster {
            request["cacheClusterEnabled"] = json!(true);
            request["cacheClusterSize"] = json!("0.5");
        }
        create_stage(&gateway, &api, case, request).await;
        // The stage's per-method caching, turned on in the caller's next request after the
        // create: "a cache cluster must be enabled on the stage for responses to be
        // cached" (`MethodSetting$cachingEnabled`), and this is the request that proves the
        // cluster already is.
        update_stage(
            &gateway,
            &api,
            STAGE,
            case,
            json!([
                {
                    "op": "replace",
                    "path": format!("{CACHED_METHOD_POINTER}/caching/enabled"),
                    "value": "true"
                },
                {
                    "op": "replace",
                    "path": format!("{CACHED_METHOD_POINTER}/caching/ttlInSeconds"),
                    "value": "300"
                }
            ]),
        )
        .await;

        // The first request fills the cluster, and consecutive requests read one response:
        // the serial each carries is the serial of the answer the cluster holds.
        for round in 0..5 {
            let answered = invoke(&gateway, &api, "GET", &format!("/{STAGE}/{RESOURCE}"), &[]).await;
            assert_eq!(
                answered.status, 200,
                "case {case} (round {round}): expected 200 from the stage, got {} with {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.body,
                format!("{{\"serial\":\"{FIRST}\"}}"),
                "case {case} (round {round}): expected the serial the integration answers, got \
                 {:?}",
                answered.body
            );
        }

        // What the integration ANSWERS is rewritten and redeployed: a stage holding the
        // first answer keeps serving it, and one holding nothing reads the new one.
        let patched = gateway
            .call(
                "PATCH",
                &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
                Some(json!({
                    "patchOperations": [{
                        "op": "replace",
                        "path": "/requestTemplates/application~1json",
                        "value": answers(SECOND)["application/json"]
                    }]
                })),
            )
            .await;
        assert_eq!(
            patched.status, 200,
            "case {case}: expected 200 from UpdateIntegration, got {} with {:?}",
            patched.status, patched.body
        );
        deploy(&gateway, &api, STAGE).await;

        let held = if *cluster { FIRST } else { SECOND };
        let after_rewrite =
            invoke(&gateway, &api, "GET", &format!("/{STAGE}/{RESOURCE}"), &[]).await;
        assert_eq!(
            after_rewrite.body,
            format!("{{\"serial\":\"{held}\"}}"),
            "case {case}: expected the serial {held} after the rewrite, got {:?}",
            after_rewrite.body
        );

        // A flush empties the cluster inside the live TTL, so the next request is answered
        // by the integration again — the same answer the cluster-less stage already reads.
        let flushed = flush_cache(&gateway, &api, STAGE).await;
        assert_eq!(
            flushed.status, 202,
            "case {case}: expected 202 from FlushStageCache, got {} with {:?}",
            flushed.status, flushed.body
        );
        let after_flush = invoke(&gateway, &api, "GET", &format!("/{STAGE}/{RESOURCE}"), &[]).await;
        assert_eq!(
            after_flush.body,
            format!("{{\"serial\":\"{SECOND}\"}}"),
            "case {case}: expected the integration's current answer after the flush, got {:?}",
            after_flush.body
        );
    }
}
