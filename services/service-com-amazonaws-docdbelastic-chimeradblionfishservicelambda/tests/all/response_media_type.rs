// SPDX-License-Identifier: CC0-1.0

//! The media type this service announces its JSON payloads with, over the wire.
//!
//! The rule this suite asserts: a response of this service that carries a JSON payload
//! announces it with the JSON media type the REQUEST named for its own payload, and with
//! this service's native `application/x-amz-json-1.1` when the request carried no payload
//! and so named none — the same media type for a success and for a refusal of one
//! request, whatever the operation and whatever the status.
//!
//! The ARN-ADDRESSED TAGGING SURFACE is the one surface that does not negotiate: every
//! response of the three operations bound to `/tags/{resourceArn}` — `TagResource`,
//! `UntagResource` and `ListTagsForResource`, the only operations of this service bound
//! under `/tags` (`smithy_query(query="digest")`) — announces its JSON payload as the
//! generic `application/json`, whatever media type the request named and whatever the
//! status. The last suite below is that rule; the tables above it stay scoped to this
//! service's own resource operations.
//!
//! The rule generalises over the media type the request names, over the operation, over
//! the status class and over the layer that rendered the response, so the tables below
//! span: requests naming no media type, the generic JSON one, a parameterised one, an
//! upper-case one, another AWS JSON version and one that is not JSON at all; successes of
//! a create, a read and a listing; refusals from a handler (not found, conflict, a
//! service-rule validation); and a refusal the framework raised before any handler ran
//! (an absent required member). Every row carries a JSON body, which the suite also
//! parses, so each assertion is about how a real payload is announced.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The region the suite signs with.
const REGION: &str = "eu-west-2";

/// The canonical placeholder account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The media type this service announces a JSON payload with when the request named none.
///
/// Recorded AWS behavior for a request carrying no payload (a read, a delete), on a 2xx
/// payload and on a refusal alike.
const DEFAULT_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// The media type a request carrying a JSON payload names for it, and so the one its
/// answer is announced with — and the one the ARN-addressed tagging surface announces
/// every payload with, whatever the request named.
///
/// Recorded AWS behavior for a request that names it (a create, an update), and for every
/// response of the tagging surface.
const GENERIC_JSON: &str = "application/json";

/// A SigV4 `Authorization` header for this service in [`REGION`].
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/docdb-elastic/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One emulator: the shared engine plus a router carrying only this service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_docdbelastic_chimeradblionfishservicelambda::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Percent-encode one `@httpLabel` value, the way an AWS SDK does.
fn label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Send one request, naming `content_type` for its own payload when it names one at all,
/// and return the answer's status, the media type it announced, and its parsed JSON body
/// (`Value::Null` when the body is empty).
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
    content_type: Option<&str>,
) -> (u16, Option<String>, Value) {
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://docdb-elastic.{REGION}.amazonaws.com{path}"))
        .header("host", format!("docdb-elastic.{REGION}.amazonaws.com"))
        .header("authorization", authorization());
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let response = router
        .dispatch(
            request
                .body(bytes::Bytes::from(payload))
                .expect("a well-formed request"),
        )
        .await;
    let status = response.status().as_u16();
    let media_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "{method} {path}: body is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, media_type, parsed)
}

/// A `CreateCluster` body with every required member and nothing else.
fn minimal_create(name: &str) -> Value {
    json!({
        "clusterName": name,
        "authType": "PLAIN_TEXT",
        "adminUserName": "admin",
        "adminUserPassword": "Secret123!",
        "shardCapacity": 2,
        "shardCount": 1,
    })
}

/// Create one cluster, naming no media type, and return its ARN.
async fn create_cluster(router: &MultiServiceRouter, name: &str) -> String {
    let (status, _, created) = call(
        router,
        "POST",
        "/cluster",
        Some(minimal_create(name)),
        Some(GENERIC_JSON),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture cluster {name:?} must be created, got {status} with {created}"
    );
    created["cluster"]["clusterArn"]
        .as_str()
        .expect("the created cluster carries its ARN")
        .to_string()
}

/// Read one cluster back until it reports `ACTIVE`, and return the media type that read
/// was announced with.
///
/// A cluster takes a modification only from a settled status, so the update row below
/// waits here first: the move to `ACTIVE` is owed by the clock, so polling a read is all
/// it takes.
async fn wait_until_active(router: &MultiServiceRouter, cluster_arn: &str) -> Option<String> {
    for _ in 0..100 {
        let (status, media_type, cluster) = call(
            router,
            "GET",
            &format!("/cluster/{}", label(cluster_arn)),
            None,
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {cluster_arn}: the cluster must read back, got {status} with {cluster}"
        );
        if cluster["cluster"]["status"].as_str() == Some("ACTIVE") {
            return media_type;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("case {cluster_arn}: the cluster never settled to ACTIVE");
}

/// One call, and the status its answer must carry.
struct Answer {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
}

#[tokio::test]
async fn a_request_naming_no_media_type_is_answered_in_this_services_own() {
    let router = emulator();
    let cluster_arn = create_cluster(&router, "announced").await;
    let absent_cluster = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/11111111-2222-3333-4444-555555555555"
    );
    let absent_snapshot = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster-snapshot/99999999-8888-7777-6666-555555555555"
    );

    // Every row carries NO payload, so none of them names a media type: the answer is
    // this service's own, at every status and from every layer.
    let answers = vec![
        Answer {
            case: "GetCluster of a present cluster (200)",
            method: "GET",
            path: format!("/cluster/{}", label(&cluster_arn)),
            body: None,
            status: 200,
        },
        Answer {
            case: "ListClusters (200)",
            method: "GET",
            path: "/clusters".to_string(),
            body: None,
            status: 200,
        },
        Answer {
            case: "ListClusterSnapshots (200)",
            method: "GET",
            path: "/cluster-snapshots".to_string(),
            body: None,
            status: 200,
        },
        Answer {
            case: "ListClusterSnapshots with an unusable token (400)",
            method: "GET",
            path: "/cluster-snapshots?nextToken=not-a-token".to_string(),
            body: None,
            status: 400,
        },
        Answer {
            case: "GetCluster of an absent cluster (404)",
            method: "GET",
            path: format!("/cluster/{}", label(&absent_cluster)),
            body: None,
            status: 404,
        },
        Answer {
            case: "DeleteCluster of an absent cluster (404)",
            method: "DELETE",
            path: format!("/cluster/{}", label(&absent_cluster)),
            body: None,
            status: 404,
        },
        Answer {
            case: "GetClusterSnapshot of an absent snapshot (404)",
            method: "GET",
            path: format!("/cluster-snapshot/{}", label(&absent_snapshot)),
            body: None,
            status: 404,
        },
        Answer {
            case: "DeleteClusterSnapshot of an absent snapshot (404)",
            method: "DELETE",
            path: format!("/cluster-snapshot/{}", label(&absent_snapshot)),
            body: None,
            status: 404,
        },
    ];

    let mut announced = BTreeSet::new();
    for answer in answers {
        let case = answer.case;
        let (status, media_type, body) =
            call(&router, answer.method, &answer.path, answer.body, None).await;
        assert_eq!(
            status, answer.status,
            "case {case:?}: expected HTTP {}, got {status} with {body}",
            answer.status
        );
        assert!(
            !body.is_null(),
            "case {case:?}: the row must carry a JSON payload to announce, got an empty body"
        );
        assert_eq!(
            media_type.as_deref(),
            Some(DEFAULT_MEDIA_TYPE),
            "case {case:?}: expected the media type {DEFAULT_MEDIA_TYPE:?}, got {media_type:?}"
        );
        announced.insert(media_type.unwrap_or_default());
    }

    assert_eq!(
        announced,
        [DEFAULT_MEDIA_TYPE.to_string()].into_iter().collect(),
        "every operation and every status must announce ONE media type, got {announced:?}"
    );
}

#[tokio::test]
async fn a_request_naming_a_json_media_type_is_answered_in_that_one() {
    let router = emulator();
    // The cluster whose name the conflict arm below collides with.
    let _ = create_cluster(&router, "negotiated").await;
    let absent_cluster = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/00000000-0000-0000-0000-000000000000"
    );

    // The rows differ along the media type the request names for its own payload — the
    // generic JSON one, a parameterised one, an upper-case one, another AWS JSON version
    // — and along whether the answer is a success, a handler refusal or a framework one.
    let named: Vec<(&str, &str, &str)> = vec![
        ("the generic JSON media type", GENERIC_JSON, GENERIC_JSON),
        (
            "a JSON media type with a parameter",
            "application/json; charset=utf-8",
            GENERIC_JSON,
        ),
        ("an upper-case media type", "Application/JSON", GENERIC_JSON),
        (
            "another AWS JSON version",
            "application/x-amz-json-1.0",
            "application/x-amz-json-1.0",
        ),
        ("this service's own", DEFAULT_MEDIA_TYPE, DEFAULT_MEDIA_TYPE),
        // Not a JSON media type, and no payload of this wire is anything else, so the
        // answer falls back to this service's own.
        ("a media type that is not JSON", "application/xml", DEFAULT_MEDIA_TYPE),
    ];

    for (index, (case, request_media_type, expected)) in named.into_iter().enumerate() {
        // A success: a create, whose payload the request names the media type of. Each
        // row asks for its own cluster name, so no row collides with another's.
        let (status, media_type, created) = call(
            &router,
            "POST",
            "/cluster",
            Some(minimal_create(&format!("negotiated{index}"))),
            Some(request_media_type),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateCluster must succeed, got {status} with {created}"
        );
        assert_eq!(
            media_type.as_deref(),
            Some(expected),
            "case {case:?} / CreateCluster (200): expected {expected:?}, got {media_type:?}"
        );

        // A handler refusal of a request that names the same media type.
        let (status, media_type, refused) = call(
            &router,
            "POST",
            "/cluster-snapshot",
            Some(json!({"clusterArn": absent_cluster, "snapshotName": format!("ghost{index}")})),
            Some(request_media_type),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: CreateClusterSnapshot of an absent cluster is HTTP 404, got \
             {status} with {refused}"
        );
        assert_eq!(
            media_type.as_deref(),
            Some(expected),
            "case {case:?} / CreateClusterSnapshot (404): expected {expected:?}, got \
             {media_type:?}"
        );

        // A conflict, a service-rule validation and a framework refusal raised before any
        // handler ran: all three announce the media type the request named.
        let arms: Vec<(&str, Value, u16)> = vec![
            (
                "a name already in use (409)",
                minimal_create("negotiated"),
                409,
            ),
            (
                "a shardCapacity outside its domain (400)",
                json!({
                    "clusterName": format!("refused{index}"),
                    "authType": "PLAIN_TEXT",
                    "adminUserName": "admin",
                    "adminUserPassword": "Secret123!",
                    "shardCapacity": 3,
                    "shardCount": 1,
                }),
                400,
            ),
            (
                "an absent required member (400)",
                json!({"clusterName": format!("incomplete{index}")}),
                400,
            ),
        ];
        for (arm, body, expected_status) in arms {
            let (status, media_type, response) = call(
                &router,
                "POST",
                "/cluster",
                Some(body),
                Some(request_media_type),
            )
            .await;
            assert_eq!(
                status, expected_status,
                "case {case:?} / {arm}: expected HTTP {expected_status}, got {status} with \
                 {response}"
            );
            assert_eq!(
                media_type.as_deref(),
                Some(expected),
                "case {case:?} / {arm}: expected {expected:?}, got {media_type:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_announced_media_type_does_not_change_the_payload() {
    // The media type is the only thing restated: the body a client parses, and the error
    // shape a failure names, come through exactly as the renderer wrote them.
    let router = emulator();
    let cluster_arn = create_cluster(&router, "intact").await;

    let (status, media_type, read) = call(
        &router,
        "GET",
        &format!("/cluster/{}", label(&cluster_arn)),
        None,
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the cluster must read back, got {status} with {read}"
    );
    assert_eq!(
        media_type.as_deref(),
        Some(DEFAULT_MEDIA_TYPE),
        "case \"a read\": expected {DEFAULT_MEDIA_TYPE:?}, got {media_type:?}"
    );
    assert_eq!(
        read["cluster"]["clusterName"].as_str(),
        Some("intact"),
        "case \"a read\": the payload must be the created cluster, got {read}"
    );

    let absent = format!("arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster-snapshot/absent");
    let (status, media_type, refused) = call(
        &router,
        "DELETE",
        &format!("/cluster-snapshot/{}", label(&absent)),
        None,
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "an absent snapshot is HTTP 404, got {status} with {refused}"
    );
    assert_eq!(
        media_type.as_deref(),
        Some(DEFAULT_MEDIA_TYPE),
        "case \"a refusal\": expected {DEFAULT_MEDIA_TYPE:?}, got {media_type:?}"
    );
    let named = refused["__type"].as_str().unwrap_or_default();
    assert_eq!(
        named.rsplit('#').next().unwrap_or(named),
        "ResourceNotFoundException",
        "case \"a refusal\": the body must still name its error shape, got {refused}"
    );

    // An update, which names the media type of the payload it carries: the answer is
    // announced in that media type and still carries the cluster it changed. A cluster
    // takes a modification only once the clock has settled it.
    let settled = wait_until_active(&router, &cluster_arn).await;
    assert_eq!(
        settled.as_deref(),
        Some(DEFAULT_MEDIA_TYPE),
        "case \"a settled read\": expected {DEFAULT_MEDIA_TYPE:?}, got {settled:?}"
    );
    let (status, media_type, updated) = call(
        &router,
        "PUT",
        &format!("/cluster/{}", label(&cluster_arn)),
        Some(json!({"shardCount": 2})),
        Some(GENERIC_JSON),
    )
    .await;
    assert_eq!(
        status, 200,
        "the cluster must take the update, got {status} with {updated}"
    );
    assert_eq!(
        media_type.as_deref(),
        Some(GENERIC_JSON),
        "case \"an update\": expected {GENERIC_JSON:?}, got {media_type:?}"
    );
    assert_eq!(
        updated["cluster"]["shardCount"].as_i64(),
        Some(2),
        "case \"an update\": the payload must be the changed cluster, got {updated}"
    );
}

/// One call of one operation of this service, as it addresses the wire: the route the
/// model binds the operation to, the method it binds it with, the body that route requires,
/// and the status this call answers with.
struct SurfaceCall {
    operation: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    /// Whether the operation is bound under `/tags/{resourceArn}` — the one surface that
    /// does not negotiate, announcing every payload as the generic JSON media type.
    tagging: bool,
}

/// EVERY operation this service binds, one call each: the nineteen operations of
/// `smithy_query(query="digest")`, at the route and method that digest states for each.
///
/// Sixteen refusals of an ARN no resource carries, the three listings (which answer a
/// payload whether or not anything is held) and a create that succeeds — so the sweep spans
/// both status classes as well as the whole operation surface. `index` makes the created
/// cluster's name its own, so repeating the sweep for another media type collides with
/// nothing.
fn surface(index: usize, absent_cluster: &str, absent_snapshot: &str) -> Vec<SurfaceCall> {
    let own = |operation, method, path: String, body, status| SurfaceCall {
        operation,
        method,
        path,
        body,
        status,
        tagging: false,
    };
    let tagging = |operation, method, path: String, body, status| SurfaceCall {
        operation,
        method,
        path,
        body,
        status,
        tagging: true,
    };
    vec![
        // The listings first: on the first media type of the sweep nothing is held yet, so
        // each answers its empty page, and on every later one the clusters the create row
        // below left behind.
        own("ListClusters", "GET", "/clusters".to_string(), None, 200),
        own(
            "ListClusterSnapshots",
            "GET",
            "/cluster-snapshots".to_string(),
            None,
            200,
        ),
        own(
            "ListPendingMaintenanceActions",
            "GET",
            "/pending-actions".to_string(),
            None,
            200,
        ),
        own(
            "GetCluster",
            "GET",
            format!("/cluster/{}", label(absent_cluster)),
            None,
            404,
        ),
        own(
            "UpdateCluster",
            "PUT",
            format!("/cluster/{}", label(absent_cluster)),
            Some(json!({"shardCount": 2})),
            404,
        ),
        own(
            "DeleteCluster",
            "DELETE",
            format!("/cluster/{}", label(absent_cluster)),
            None,
            404,
        ),
        own(
            "StartCluster",
            "POST",
            format!("/cluster/{}/start", label(absent_cluster)),
            None,
            404,
        ),
        own(
            "StopCluster",
            "POST",
            format!("/cluster/{}/stop", label(absent_cluster)),
            None,
            404,
        ),
        own(
            "CreateClusterSnapshot",
            "POST",
            "/cluster-snapshot".to_string(),
            Some(json!({"clusterArn": absent_cluster, "snapshotName": format!("swept{index}")})),
            404,
        ),
        own(
            "GetClusterSnapshot",
            "GET",
            format!("/cluster-snapshot/{}", label(absent_snapshot)),
            None,
            404,
        ),
        own(
            "DeleteClusterSnapshot",
            "DELETE",
            format!("/cluster-snapshot/{}", label(absent_snapshot)),
            None,
            404,
        ),
        own(
            "CopyClusterSnapshot",
            "POST",
            format!("/cluster-snapshot/{}/copy", label(absent_snapshot)),
            Some(json!({"targetSnapshotName": format!("swept-copy{index}")})),
            404,
        ),
        own(
            "RestoreClusterFromSnapshot",
            "POST",
            format!("/cluster-snapshot/{}/restore", label(absent_snapshot)),
            Some(json!({"clusterName": format!("swept-restored{index}")})),
            404,
        ),
        own(
            "GetPendingMaintenanceAction",
            "GET",
            format!("/pending-action/{}", label(absent_cluster)),
            None,
            404,
        ),
        own(
            "ApplyPendingMaintenanceAction",
            "POST",
            "/pending-action".to_string(),
            // `applyAction` is one of the documented actions and `optInType` one of the
            // modelled enum's values, so the request holds together and the ARN is what
            // the call is refused for (`smithy_query(query="operation",
            // operation="ApplyPendingMaintenanceAction")`).
            Some(json!({
                "resourceArn": absent_cluster,
                "applyAction": "ENGINE_UPDATE",
                "optInType": "IMMEDIATE",
            })),
            404,
        ),
        tagging(
            "TagResource",
            "POST",
            format!("/tags/{}", label(absent_cluster)),
            Some(json!({"tags": {"Project": "lionfish"}})),
            404,
        ),
        tagging(
            "UntagResource",
            "DELETE",
            format!("/tags/{}?tagKeys=Project", label(absent_cluster)),
            None,
            404,
        ),
        tagging(
            "ListTagsForResource",
            "GET",
            format!("/tags/{}", label(absent_cluster)),
            None,
            404,
        ),
        own(
            "CreateCluster",
            "POST",
            "/cluster".to_string(),
            Some(minimal_create(&format!("swept{index}"))),
            200,
        ),
    ]
}

#[tokio::test]
async fn every_operation_of_this_service_announces_the_negotiated_media_type() {
    // The rule generalises over the OPERATION, so this sweep is every operation this
    // service binds rather than a chosen few: each is called at the route the model binds
    // it to, under each JSON media type a request can name for its own payload (including
    // naming none), and the media type its answer announces must be the negotiated one —
    // this service's own `application/x-amz-json-1.1` for a request that named none, the
    // one the request named otherwise, and the generic JSON media type for the three
    // operations of the ARN-addressed tagging surface. The recorded cases (a `ListClusters`
    // 200 and a `ListClusterSnapshots` refusal, neither naming a media type) are two rows
    // of it.
    let router = emulator();
    let absent_cluster = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    );
    let absent_snapshot = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster-snapshot/ffffffff-eeee-dddd-cccc-bbbbbbbbbbbb"
    );

    let named: Vec<(&str, Option<&str>)> = vec![
        ("naming no media type", None),
        ("naming the generic JSON media type", Some(GENERIC_JSON)),
        (
            "naming another AWS JSON version",
            Some("application/x-amz-json-1.0"),
        ),
    ];

    for (index, (named_case, request_media_type)) in named.into_iter().enumerate() {
        let negotiated = request_media_type.unwrap_or(DEFAULT_MEDIA_TYPE);
        let mut announced_own = BTreeSet::new();
        let mut announced_tagging = BTreeSet::new();
        let mut swept = BTreeSet::new();

        for row in surface(index, &absent_cluster, &absent_snapshot) {
            let expected = if row.tagging { GENERIC_JSON } else { negotiated };
            let case = format!("{} {named_case}", row.operation);
            let (status, media_type, body) =
                call(&router, row.method, &row.path, row.body, request_media_type).await;
            assert_eq!(
                status, row.status,
                "case {case:?}: expected HTTP {}, got {status} with {body}",
                row.status
            );
            assert!(
                !body.is_null(),
                "case {case:?}: expected a JSON payload to announce, got an empty body"
            );
            assert_eq!(
                media_type.as_deref(),
                Some(expected),
                "case {case:?}: expected the media type {expected:?}, got {media_type:?}"
            );
            swept.insert(row.operation);
            if row.tagging {
                announced_tagging.insert(media_type.unwrap_or_default());
            } else {
                announced_own.insert(media_type.unwrap_or_default());
            }
        }

        // One media type per surface, over every operation and both status classes: the
        // sweep cannot pass by announcing a different one per operation.
        assert_eq!(
            announced_own,
            [negotiated.to_string()].into_iter().collect(),
            "case {named_case:?}: this service's own operations must announce ONE media \
             type, got {announced_own:?}"
        );
        assert_eq!(
            announced_tagging,
            [GENERIC_JSON.to_string()].into_iter().collect(),
            "case {named_case:?}: the tagging surface must announce ONE media type, got \
             {announced_tagging:?}"
        );
        // The surface is the whole modelled one: nineteen operations are bound
        // (`smithy_query(query="digest")`), so a sweep that quietly stopped covering one
        // of them fails here rather than passing on the rest.
        assert_eq!(
            swept.len(),
            19,
            "case {named_case:?}: every bound operation must be swept, got {swept:?}"
        );
    }
}

/// One operation of the ARN-addressed tagging surface, as it addresses an ARN on the wire.
///
/// `tags` travels in the BODY of `TagResource` and `tagKeys` as repeated `@httpQuery`
/// parameters of `UntagResource`, so each row carries its own path and body builder
/// (`smithy_query(query="operation",
/// operation="TagResource,UntagResource,ListTagsForResource")`).
struct TagCall {
    operation: &'static str,
    method: &'static str,
    path: fn(&str) -> String,
    body: fn() -> Option<Value>,
}

/// All three operations bound to `/tags/{resourceArn}`, which are the whole tagging
/// surface of this service (`smithy_query(query="digest")`).
fn tag_calls() -> Vec<TagCall> {
    vec![
        TagCall {
            operation: "TagResource",
            method: "POST",
            path: |arn| format!("/tags/{}", label(arn)),
            body: || Some(json!({"tags": {"Project": "lionfish"}})),
        },
        TagCall {
            operation: "UntagResource",
            method: "DELETE",
            path: |arn| format!("/tags/{}?tagKeys=Project", label(arn)),
            body: || None,
        },
        TagCall {
            operation: "ListTagsForResource",
            method: "GET",
            path: |arn| format!("/tags/{}", label(arn)),
            body: || None,
        },
    ]
}

#[tokio::test]
async fn the_tagging_surface_announces_the_generic_json_media_type() {
    // The rows span all three tagging operations, the media types a request can name for
    // its own payload (including naming none and naming one that is not JSON), and both
    // status classes: a success against an ARN this emulator holds, and the 404 against
    // one it holds nothing under. The recorded case — one tag read of an ARN no resource
    // carries — is one row of it.
    let router = emulator();
    let held = create_cluster(&router, "generically-announced").await;
    let unheld = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/00000000-0000-0000-0000-000000000000"
    );

    let named: Vec<(&str, Option<&str>)> = vec![
        ("naming no media type", None),
        ("naming the generic JSON media type", Some(GENERIC_JSON)),
        ("naming this service's own", Some(DEFAULT_MEDIA_TYPE)),
        ("naming another AWS JSON version", Some("application/x-amz-json-1.0")),
        ("naming a media type that is not JSON", Some("application/xml")),
    ];

    let mut announced = BTreeSet::new();
    for (named_case, request_media_type) in named {
        for tag_call in tag_calls() {
            for (addressed, arn, expected_status) in [
                ("an ARN this emulator holds", held.as_str(), 200),
                ("an ARN no resource carries", unheld.as_str(), 404),
            ] {
                let case = format!("{} {named_case} / {addressed}", tag_call.operation);
                let (status, media_type, body) = call(
                    &router,
                    tag_call.method,
                    &(tag_call.path)(arn),
                    (tag_call.body)(),
                    request_media_type,
                )
                .await;
                assert_eq!(
                    status, expected_status,
                    "case {case:?}: expected HTTP {expected_status}, got {status} with {body}"
                );
                // Every answer of this surface carries a JSON payload to announce: the
                // tag read answers its `tags` map, a refusal its error envelope, and a
                // tag write — whose output models no members at all — still answers the
                // empty JSON object. So each row is asserted to carry one, rather than
                // the row being let off whenever the implementation answered none.
                assert!(
                    !body.is_null(),
                    "case {case:?}: expected a JSON payload to announce, got an empty body"
                );
                assert_eq!(
                    media_type.as_deref(),
                    Some(GENERIC_JSON),
                    "case {case:?}: expected {GENERIC_JSON:?}, got {media_type:?}"
                );
                announced.insert(media_type.unwrap_or_default());
            }
        }
    }

    assert_eq!(
        announced,
        [GENERIC_JSON.to_string()].into_iter().collect(),
        "case \"the whole tagging surface\": one media type must serve every payload it \
         answers, got {announced:?}"
    );

    // A refusal the FRAMEWORK raises on that route before any handler runs — `tags` is
    // @required on TagResource — is announced the same way.
    let (status, media_type, body) = call(
        &router,
        "POST",
        &format!("/tags/{}", label(&unheld)),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"an omitted required member\": expected HTTP 400, got {status} with {body}"
    );
    assert_eq!(
        media_type.as_deref(),
        Some(GENERIC_JSON),
        "case \"an omitted required member\": expected {GENERIC_JSON:?}, got {media_type:?}"
    );
}
