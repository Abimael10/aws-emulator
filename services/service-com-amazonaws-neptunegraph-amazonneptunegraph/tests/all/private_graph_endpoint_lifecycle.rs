// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the private graph endpoint resource:
//! `CreatePrivateGraphEndpoint`, the `GetPrivateGraphEndpoint` read it round-trips
//! through, and `DeletePrivateGraphEndpoint`, driven over the wire (`restJson1`)
//! through the same router the emulator serves from.
//!
//! The rules every assertion here is about:
//!
//! * A private graph endpoint is the private route into ONE graph from ONE VPC, so
//!   `(graphIdentifier, vpcId)` is its identity: a second endpoint in a VPC that
//!   already has one conflicts (409), the same `vpcId` on another graph names another
//!   endpoint, and an endpoint cannot outlive the graph it reaches.
//! * `CreatePrivateGraphEndpoint` persists the record and reports it — the VPC and
//!   subnets the request named, a minted `^vpce-[0-9a-f]{17}$` endpoint id, and a
//!   `CREATING` status the clock settles to `AVAILABLE`.
//! * `DeletePrivateGraphEndpoint` reports the stored record on its way out with the
//!   modelled `DELETING` status, keeps answering reads with it until the clock makes
//!   the removal due, and then the endpoint is gone: a read answers
//!   `ResourceNotFoundException` (404) and the VPC is free to take a new endpoint.
//! * An identifier naming no stored resource is refused 404, an endpoint with a
//!   transition still outstanding (`CREATING`, `DELETING`) is refused 409, and a value
//!   outside the modelled `VpcId` / `SubnetIds` / `SecurityGroupIds` constraints is
//!   refused 400 rather than served.
//! * `vpcId` and `subnetIds` are OPTIONAL input and REQUIRED output, so a member the
//!   request leaves out is RESOLVED from the account's own network inventory in the
//!   Region the request was signed for — the Region's default VPC, and the subnets the
//!   VPC the endpoint is created in holds — and the endpoint reports the ids the service
//!   that OWNS VPCs and subnets answers its own reads with.
//!
//! So each table below spans what its rule generalises over — the subnet count at the
//! published bound and one past it, the presence and count of security groups, the form
//! of the VPC id, which graph an endpoint was created on, and which identifier of the
//! addressing pair names nothing — rather than re-running one recorded request.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
// The account's VPCs and subnets live in the service that owns them, mounted beside this
// one on the same engine in the rows that drive the resolution of an absent member.
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_neptunegraph_amazonneptunegraph::state::{
    DELETION_SPAN, ENDPOINT_DELETION_SPAN, MAX_ENDPOINT_SUBNET_IDS, PROVISIONING_SPAN,
};
use service_com_amazonaws_neptunegraph_amazonneptunegraph::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's SigV4 credential scope names.
const REGION: &str = "us-east-1";

/// The smallest `provisionedMemory` the model allows (`range 16..24576`), which is all
/// these tests need from a graph.
const MIN_PROVISIONED_MEMORY: i64 = 16;

/// The most subnets one endpoint may name: `SubnetIds` is `@length(1..6)`
/// (`smithy_query(query="shape",shape="SubnetIds")`), as the crate itself states the
/// bound ([`MAX_ENDPOINT_SUBNET_IDS`]).
const MAX_SUBNET_IDS: usize = MAX_ENDPOINT_SUBNET_IDS;

/// The most security groups one endpoint may name: `SecurityGroupIds` is
/// `@length(1..10)` (`smithy_query(query="shape",shape="SecurityGroupIds")`).
const MAX_SECURITY_GROUP_IDS: usize = 10;

/// An emulator holding only this service, on its own engine, so each test starts from
/// an empty store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(engine));
    router
}

/// Dispatch one `restJson1` request signed for [`REGION`] and return its status and
/// parsed JSON body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_in(router, REGION, method, path, body).await
}

/// Dispatch one `restJson1` request signed for `region`.
///
/// The Region of the credential scope is the Region this regional control plane answers
/// for, so a row about what the account holds in ONE Region sends its whole exchange
/// through here.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(&value).expect("a JSON request body serializes")
    });
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/neptune-graph/\
                 aws4_request, SignedHeaders=host, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code of a failure body (`__type`), without its namespace.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The text of one member of a response body, or the empty string when it carries none.
fn text_of(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// Whether `value` matches the modelled `VpcEndpointId` pattern `^vpce-[0-9a-f]{17}$`.
fn is_vpc_endpoint_id(value: &str) -> bool {
    value.strip_prefix("vpce-").is_some_and(|suffix| {
        suffix.len() == 17 && suffix.chars().all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    })
}

/// `POST /graphs/{graphIdentifier}/endpoints/`.
async fn create_endpoint(
    router: &MultiServiceRouter,
    graph_id: &str,
    body: Value,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/graphs/{graph_id}/endpoints/"),
        Some(body),
    )
    .await
}

/// `GET /graphs/{graphIdentifier}/endpoints/{vpcId}`.
async fn get_endpoint(router: &MultiServiceRouter, graph_id: &str, vpc_id: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/graphs/{graph_id}/endpoints/{vpc_id}"),
        None,
    )
    .await
}

/// `DELETE /graphs/{graphIdentifier}/endpoints/{vpcId}`.
async fn delete_endpoint(
    router: &MultiServiceRouter,
    graph_id: &str,
    vpc_id: &str,
) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/graphs/{graph_id}/endpoints/{vpc_id}"),
        None,
    )
    .await
}

/// Poll one resource until it reports `AVAILABLE`. No handler ever sleeps; only this
/// test does, because it is asking about a state the clock reaches on its own.
async fn wait_until_available(router: &MultiServiceRouter, path: &str) -> Value {
    for _ in 0..40 {
        let (status, body) = call(router, "GET", path, None).await;
        assert_eq!(
            status, 200,
            "case {path}: expected 200 from the read, got {status}: {body}"
        );
        if body["status"] == json!("AVAILABLE") {
            return body;
        }
        tokio::time::sleep(PROVISIONING_SPAN / 4).await;
    }
    let (_, body) = call(router, "GET", path, None).await;
    panic!(
        "case {path}: never became AVAILABLE, last status {}",
        body["status"]
    );
}

/// Poll one endpoint until the read no longer finds it, and report the last body.
async fn wait_until_gone(router: &MultiServiceRouter, graph_id: &str, vpc_id: &str) -> Value {
    for _ in 0..40 {
        let (status, body) = get_endpoint(router, graph_id, vpc_id).await;
        if status == 404 {
            return body;
        }
        assert_eq!(
            status, 200,
            "case {graph_id}/{vpc_id}: expected 200 or 404 while the deletion is \
             outstanding, got {status}: {body}"
        );
        tokio::time::sleep(ENDPOINT_DELETION_SPAN / 4).await;
    }
    panic!("case {graph_id}/{vpc_id}: the deleted endpoint was never reclaimed");
}

/// Create a graph and wait it into service, returning its id.
async fn graph_in_service(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "graphName": name,
        "provisionedMemory": MIN_PROVISIONED_MEMORY,
    });
    let (status, created) = call(router, "POST", "/graphs", Some(body.clone())).await;
    assert_eq!(
        status, 201,
        "case {body}: expected 201 from CreateGraph, got {status}: {created}"
    );
    let id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {body}: no id in {created}"))
        .to_string();
    wait_until_available(router, &format!("/graphs/{id}")).await;
    id
}

/// `n` distinct subnet ids matching the modelled `SubnetId` pattern
/// `^subnet-[a-z0-9]+$`.
fn subnets(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("subnet-0a1b2c3d4e5f{i:05}")).collect()
}

/// `n` distinct security group ids matching the modelled `SecurityGroupId` pattern
/// `^sg-[a-z0-9]+$`.
fn security_groups(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("sg-0f1e2d3c4b5a{i:05}")).collect()
}

/// One row of the create-and-read-back table: the case name, the `vpcId` the request
/// carries, the subnets it names, and the security groups it attaches.
type EndpointCase = (&'static str, &'static str, Vec<String>, Vec<String>);

/// One row of the modelled-constraint table: [`EndpointCase`] plus whether the request
/// it describes is accepted.
type ConstraintCase = (
    &'static str,
    &'static str,
    Vec<String>,
    Vec<String>,
    bool,
);

#[tokio::test]
async fn create_private_graph_endpoint_persists_the_record_and_reads_it_back() {
    let router = emulator();
    let graph_id = graph_in_service(&router, "endpoint-source").await;

    // (case, vpcId, subnetIds, vpcSecurityGroupIds) — the rows differ in the form of
    // the VPC id, the subnet count from one up to the published bound, and whether (and
    // how many) security groups are attached.
    let cases: Vec<EndpointCase> = vec![
        (
            "one subnet, no security group",
            "vpc-abc123",
            subnets(1),
            Vec::new(),
        ),
        (
            "three subnets, one security group",
            "vpc-0a1b2c3d4e5f67890",
            subnets(3),
            security_groups(1),
        ),
        (
            "subnets at the published bound, security groups at theirs",
            "vpc-9",
            subnets(MAX_SUBNET_IDS),
            security_groups(MAX_SECURITY_GROUP_IDS),
        ),
    ];

    let mut minted = Vec::new();
    for (case, vpc_id, subnet_ids, security_group_ids) in cases {
        let mut body = json!({"vpcId": vpc_id, "subnetIds": subnet_ids});
        if !security_group_ids.is_empty() {
            body["vpcSecurityGroupIds"] = json!(security_group_ids);
        }
        let (status, created) = create_endpoint(&router, &graph_id, body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201, got {status}: {created}"
        );
        assert_eq!(
            created["vpcId"],
            json!(vpc_id),
            "case {case:?}: expected vpcId {vpc_id:?}, got {}",
            created["vpcId"]
        );
        assert_eq!(
            created["subnetIds"],
            json!(subnet_ids),
            "case {case:?}: expected subnetIds {}, got {}",
            json!(subnet_ids),
            created["subnetIds"]
        );
        assert_eq!(
            created["status"],
            json!("CREATING"),
            "case {case:?}: an endpoint is not reachable when the call returns, expected \
             CREATING, got {}",
            created["status"]
        );
        let vpc_endpoint_id = created["vpcEndpointId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no vpcEndpointId in {created}"))
            .to_string();
        assert!(
            is_vpc_endpoint_id(&vpc_endpoint_id),
            "case {case:?}: expected an id matching ^vpce-[0-9a-f]{{17}}$, got \
             {vpc_endpoint_id:?}"
        );
        assert!(
            !minted.contains(&vpc_endpoint_id),
            "case {case:?}: vpcEndpointId {vpc_endpoint_id:?} was minted twice, already \
             in {minted:?}"
        );
        minted.push(vpc_endpoint_id.clone());

        // The round trip: an independent read reports the record that was stored, not
        // the response that was rendered.
        let (read_status, read) = get_endpoint(&router, &graph_id, vpc_id).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected 200 from GetPrivateGraphEndpoint, got {read_status}: \
             {read}"
        );
        for member in ["vpcId", "subnetIds", "vpcEndpointId"] {
            assert_eq!(
                read[member], created[member],
                "case {case:?}: GetPrivateGraphEndpoint's {member} must be the stored \
                 value {}, got {}",
                created[member], read[member]
            );
        }

        // The clock owes the move to AVAILABLE; nothing else about the record changes.
        let settled =
            wait_until_available(&router, &format!("/graphs/{graph_id}/endpoints/{vpc_id}")).await;
        for member in ["vpcId", "subnetIds", "vpcEndpointId"] {
            assert_eq!(
                settled[member], created[member],
                "case {case:?}: settling must not change {member}, expected {}, got {}",
                created[member], settled[member]
            );
        }
    }
}

#[tokio::test]
async fn a_second_endpoint_in_a_vpc_that_already_has_one_conflicts() {
    let router = emulator();
    let first_graph = graph_in_service(&router, "conflict-graph-a").await;
    let second_graph = graph_in_service(&router, "conflict-graph-b").await;

    let (status, created) = create_endpoint(
        &router,
        &first_graph,
        json!({"vpcId": "vpc-taken1", "subnetIds": subnets(2)}),
    )
    .await;
    assert_eq!(
        status, 201,
        "case the first endpoint: expected 201, got {status}: {created}"
    );

    // (case, the graph the next endpoint is created on, its vpcId, expected status)
    let cases: Vec<(&str, &str, &str, u16)> = vec![
        (
            "the same vpc on the same graph",
            &first_graph,
            "vpc-taken1",
            409,
        ),
        (
            "the same vpc id on another graph",
            &second_graph,
            "vpc-taken1",
            201,
        ),
        ("a free vpc on the same graph", &first_graph, "vpc-free1", 201),
        (
            "a free vpc on another graph",
            &second_graph,
            "vpc-free2",
            201,
        ),
    ];
    for (case, graph_id, vpc_id, expected) in cases {
        let (status, body) = create_endpoint(
            &router,
            graph_id,
            json!({"vpcId": vpc_id, "subnetIds": subnets(1)}),
        )
        .await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected}, got {status}: {body}"
        );
        if expected == 409 {
            let code = error_code(&body);
            assert_eq!(
                code, "ConflictException",
                "case {case:?}: expected ConflictException, got {code:?}: {body}"
            );
        }
    }

    // The same vpc id on two graphs named two independent endpoints, each with its own
    // minted VPC endpoint id.
    let (_, first) = get_endpoint(&router, &first_graph, "vpc-taken1").await;
    let (_, second) = get_endpoint(&router, &second_graph, "vpc-taken1").await;
    assert_ne!(
        first["vpcEndpointId"], second["vpcEndpointId"],
        "case the same vpc on two graphs: expected two distinct endpoints, got {} on \
         both",
        first["vpcEndpointId"]
    );
}

#[tokio::test]
async fn deleting_an_endpoint_reports_it_deleting_and_then_reclaims_it() {
    let router = emulator();
    let graph_id = graph_in_service(&router, "delete-source").await;

    // (case, vpcId, subnetIds) — the rows differ in the VPC id's form and the subnet
    // count, so the rule is asserted over the class and not over one request.
    let cases: Vec<(&str, &str, Vec<String>)> = vec![
        ("one subnet", "vpc-del1", subnets(1)),
        ("two subnets", "vpc-0123456789abcdef0", subnets(2)),
        (
            "subnets at the published bound",
            "vpc-del3",
            subnets(MAX_SUBNET_IDS),
        ),
    ];

    for (case, vpc_id, subnet_ids) in cases {
        let (status, created) = create_endpoint(
            &router,
            &graph_id,
            json!({"vpcId": vpc_id, "subnetIds": subnet_ids}),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from the create, got {status}: {created}"
        );
        let stored =
            wait_until_available(&router, &format!("/graphs/{graph_id}/endpoints/{vpc_id}")).await;

        // The delete reports the STORED record, with the modelled transient status.
        let (status, deleted) = delete_endpoint(&router, &graph_id, vpc_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the delete, got {status}: {deleted}"
        );
        assert_eq!(
            deleted["status"],
            json!("DELETING"),
            "case {case:?}: expected DELETING, got {}",
            deleted["status"]
        );
        for member in ["vpcId", "subnetIds", "vpcEndpointId"] {
            assert_eq!(
                deleted[member], stored[member],
                "case {case:?}: the delete must report the stored {member} {}, got {}",
                stored[member], deleted[member]
            );
        }

        // A caller acting straight after the call still sees the transient status, and a
        // second delete of an endpoint already on its way out conflicts.
        let (read_status, read) = get_endpoint(&router, &graph_id, vpc_id).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected 200 from the read while the deletion is \
             outstanding, got {read_status}: {read}"
        );
        assert_eq!(
            read["status"],
            json!("DELETING"),
            "case {case:?}: expected DELETING from the read, got {}",
            read["status"]
        );
        let (again_status, again) = delete_endpoint(&router, &graph_id, vpc_id).await;
        assert_eq!(
            again_status, 409,
            "case {case:?}: expected 409 from a second delete, got {again_status}: {again}"
        );
        let code = error_code(&again);
        assert_eq!(
            code, "ConflictException",
            "case {case:?}: expected ConflictException from a second delete, got \
             {code:?}: {again}"
        );

        // Once the clock makes the removal due the endpoint is GONE: the read answers
        // the modelled not-found, and the VPC is free to take a new endpoint — which
        // gets its own minted id rather than the reclaimed one.
        let gone = wait_until_gone(&router, &graph_id, vpc_id).await;
        let code = error_code(&gone);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException once reclaimed, got \
             {code:?}: {gone}"
        );
        let (status, recreated) = create_endpoint(
            &router,
            &graph_id,
            json!({"vpcId": vpc_id, "subnetIds": subnets(1)}),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected the vpc to be free again, got {status}: {recreated}"
        );
        assert_ne!(
            recreated["vpcEndpointId"], stored["vpcEndpointId"],
            "case {case:?}: a new endpoint must carry a newly minted id, got the \
             reclaimed {}",
            stored["vpcEndpointId"]
        );
        // Leave the store without an endpoint in this vpc for the next row.
        let (status, body) = delete_endpoint(&router, &graph_id, vpc_id).await;
        assert!(
            status == 200 || status == 409,
            "case {case:?}: expected the cleanup delete to be accepted or conflict, got \
             {status}: {body}"
        );
    }
}

#[tokio::test]
async fn an_endpoint_with_a_transition_outstanding_cannot_be_deleted() {
    let router = emulator();
    let graph_id = graph_in_service(&router, "transient-source").await;

    let (status, created) = create_endpoint(
        &router,
        &graph_id,
        json!({"vpcId": "vpc-transient", "subnetIds": subnets(2)}),
    )
    .await;
    assert_eq!(status, 201, "the create: got {status}: {created}");
    assert_eq!(
        created["status"],
        json!("CREATING"),
        "case a freshly created endpoint: expected CREATING, got {}",
        created["status"]
    );

    // The refusal is taken while the provisioning transition is still outstanding.
    let (status, refused) = delete_endpoint(&router, &graph_id, "vpc-transient").await;
    assert_eq!(
        status, 409,
        "case an endpoint still provisioning: expected 409, got {status}: {refused}"
    );
    let code = error_code(&refused);
    assert_eq!(
        code, "ConflictException",
        "case an endpoint still provisioning: expected ConflictException, got {code:?}: \
         {refused}"
    );

    // The refusal persisted nothing: once the endpoint is reachable, the SAME call is
    // accepted.
    wait_until_available(&router, &format!("/graphs/{graph_id}/endpoints/vpc-transient")).await;
    let (status, accepted) = delete_endpoint(&router, &graph_id, "vpc-transient").await;
    assert_eq!(
        status, 200,
        "case the same call once the endpoint is available: expected 200, got {status}: \
         {accepted}"
    );
}

#[tokio::test]
async fn an_identifier_that_names_no_stored_endpoint_is_refused() {
    let router = emulator();
    let graph_id = graph_in_service(&router, "identifier-source").await;
    let other_graph = graph_in_service(&router, "identifier-other").await;
    let (status, created) = create_endpoint(
        &router,
        &graph_id,
        json!({"vpcId": "vpc-stored", "subnetIds": subnets(1)}),
    )
    .await;
    assert_eq!(status, 201, "the fixture endpoint: got {status}: {created}");
    wait_until_available(&router, &format!("/graphs/{graph_id}/endpoints/vpc-stored")).await;

    // (case, graphIdentifier, vpcId, expected status, expected error code) — one row
    // per way the addressing pair can fail to name a stored endpoint, for the read and
    // for the delete alike.
    let cases: Vec<(&str, &str, &str, u16, &str)> = vec![
        ("the stored endpoint", &graph_id, "vpc-stored", 200, ""),
        (
            "a stored graph with no endpoint in that vpc",
            &graph_id,
            "vpc-never",
            404,
            "ResourceNotFoundException",
        ),
        (
            "the stored vpc id on another graph",
            &other_graph,
            "vpc-stored",
            404,
            "ResourceNotFoundException",
        ),
        (
            "a well-formed graph id that was never created",
            "g-zzzzzzzzzz",
            "vpc-stored",
            404,
            "ResourceNotFoundException",
        ),
        (
            "a graph identifier outside the modelled pattern",
            "not-a-graph-id",
            "vpc-stored",
            400,
            "ValidationException",
        ),
        (
            "a vpc id outside the modelled pattern",
            &graph_id,
            "VPC-Stored",
            400,
            "ValidationException",
        ),
    ];

    for (case, graph_identifier, vpc_id, expected, expected_code) in cases {
        let (status, body) = get_endpoint(&router, graph_identifier, vpc_id).await;
        assert_eq!(
            status, expected,
            "case {case:?} (read {graph_identifier}/{vpc_id}): expected {expected}, got \
             {status}: {body}"
        );
        if !expected_code.is_empty() {
            let code = error_code(&body);
            assert_eq!(
                code, expected_code,
                "case {case:?} (read {graph_identifier}/{vpc_id}): expected \
                 {expected_code}, got {code:?}: {body}"
            );
        }
        if expected == 200 {
            // The accepted row is exercised against the delete last, so the rows that
            // must fail are not answered out of a store this one emptied.
            continue;
        }
        let (status, body) = delete_endpoint(&router, graph_identifier, vpc_id).await;
        assert_eq!(
            status, expected,
            "case {case:?} (delete {graph_identifier}/{vpc_id}): expected {expected}, \
             got {status}: {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, expected_code,
            "case {case:?} (delete {graph_identifier}/{vpc_id}): expected \
             {expected_code}, got {code:?}: {body}"
        );
    }

    // The stored endpoint was never touched by any refusal above.
    let (status, body) = delete_endpoint(&router, &graph_id, "vpc-stored").await;
    assert_eq!(
        status, 200,
        "case the stored endpoint: expected 200 from the delete, got {status}: {body}"
    );
}

/// How a missing private graph endpoint is reported: the modelled
/// `ResourceNotFoundException` is rendered from the addressing pair the REQUEST
/// carried — the graph the endpoint would reach, then the VPC it would be reached
/// from — so the failure is a function of the request and never of which stored
/// record happens to be absent.
fn endpoint_not_found_message(graph_id: &str, vpc_id: &str) -> String {
    format!("Private Graph Endpoint to Graph: {graph_id} from VPC: {vpc_id} does not exist")
}

#[tokio::test]
async fn a_missing_endpoint_is_reported_naming_the_graph_and_the_vpc() {
    let router = emulator();
    let graph_id = graph_in_service(&router, "message-source").await;
    let other_graph = graph_in_service(&router, "message-other").await;

    // One pair is made to hold an endpoint and then reclaimed, so the table below asks
    // about a pair that once WAS an endpoint beside pairs that never were.
    let (status, created) = create_endpoint(
        &router,
        &graph_id,
        json!({"vpcId": "vpc-reclaimed", "subnetIds": subnets(1)}),
    )
    .await;
    assert_eq!(status, 201, "the fixture endpoint: got {status}: {created}");
    wait_until_available(
        &router,
        &format!("/graphs/{graph_id}/endpoints/vpc-reclaimed"),
    )
    .await;
    let (status, deleted) = delete_endpoint(&router, &graph_id, "vpc-reclaimed").await;
    assert_eq!(status, 200, "the fixture delete: got {status}: {deleted}");
    let gone = wait_until_gone(&router, &graph_id, "vpc-reclaimed").await;
    let expected = endpoint_not_found_message(&graph_id, "vpc-reclaimed");
    assert_eq!(
        gone["message"],
        json!(expected),
        "case the endpoint the clock reclaimed: expected message {expected:?}, got {}",
        gone["message"]
    );

    // (case, graphIdentifier, vpcId) — every row names a stored graph and a VPC that
    // holds no endpoint, differing in the form and length of the VPC id, in which graph
    // the lookup is resolved against, and in whether the pair ever held an endpoint.
    let cases: Vec<(&str, &str, &str)> = vec![
        ("a vpc that never held an endpoint", &graph_id, "vpc-never"),
        ("a long vpc id", &graph_id, "vpc-0000000000000001e"),
        ("the reclaimed pair", &graph_id, "vpc-reclaimed"),
        (
            "a graph that never had an endpoint",
            &other_graph,
            "vpc-reclaimed",
        ),
    ];

    for (case, graph_identifier, vpc_id) in cases {
        let expected = endpoint_not_found_message(graph_identifier, vpc_id);
        let answers = [
            (
                "GetPrivateGraphEndpoint",
                get_endpoint(&router, graph_identifier, vpc_id).await,
            ),
            (
                "DeletePrivateGraphEndpoint",
                delete_endpoint(&router, graph_identifier, vpc_id).await,
            ),
        ];
        for (op, (status, body)) in answers {
            assert_eq!(
                status, 404,
                "case {case:?} ({op} {graph_identifier}/{vpc_id}): expected 404, got \
                 {status}: {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, "ResourceNotFoundException",
                "case {case:?} ({op} {graph_identifier}/{vpc_id}): expected \
                 ResourceNotFoundException, got {code:?}: {body}"
            );
            assert_eq!(
                body["message"],
                json!(expected),
                "case {case:?} ({op} {graph_identifier}/{vpc_id}): expected message \
                 {expected:?}, got {}",
                body["message"]
            );
        }
    }
}

#[tokio::test]
async fn an_endpoint_does_not_outlive_the_graph_it_reaches() {
    let router = emulator();
    let graph_id = graph_in_service(&router, "doomed-graph").await;
    let survivor = graph_in_service(&router, "surviving-graph").await;

    for (graph, vpc_id) in [(&graph_id, "vpc-doomed"), (&survivor, "vpc-doomed")] {
        let (status, created) = create_endpoint(
            &router,
            graph,
            json!({"vpcId": vpc_id, "subnetIds": subnets(1)}),
        )
        .await;
        assert_eq!(
            status, 201,
            "case the endpoint on {graph}: expected 201, got {status}: {created}"
        );
        wait_until_available(&router, &format!("/graphs/{graph}/endpoints/{vpc_id}")).await;
    }

    let (status, body) = call(
        &router,
        "DELETE",
        &format!("/graphs/{graph_id}?skipSnapshot=true"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case DeleteGraph: expected 200, got {status}: {body}"
    );

    // The graph is on its way out, so its endpoint is still reachable for as long as the
    // graph itself is...
    let (status, body) = get_endpoint(&router, &graph_id, "vpc-doomed").await;
    assert_eq!(
        status, 200,
        "case the deleting graph's endpoint: expected 200 while the graph is DELETING, \
         got {status}: {body}"
    );

    // ...and goes with it once the deletion the clock owes falls due; the other graph's
    // endpoint in the same VPC is untouched.
    tokio::time::sleep(DELETION_SPAN + Duration::from_millis(150)).await;
    let (status, body) = get_endpoint(&router, &graph_id, "vpc-doomed").await;
    assert_eq!(
        status, 404,
        "case the deleted graph's endpoint: expected 404, got {status}: {body}"
    );
    let (status, body) = get_endpoint(&router, &survivor, "vpc-doomed").await;
    assert_eq!(
        status, 200,
        "case the surviving graph's endpoint: expected 200, got {status}: {body}"
    );
    assert_eq!(
        body["status"],
        json!("AVAILABLE"),
        "case the surviving graph's endpoint: expected AVAILABLE, got {}",
        body["status"]
    );
}

#[tokio::test]
async fn a_request_outside_the_modelled_list_constraints_is_refused() {
    let router = emulator();
    let graph_id = graph_in_service(&router, "constraint-source").await;

    // One row per clause of the modelled `SubnetIds` (`@length(1..6)`, `@uniqueItems`,
    // member `@pattern("^subnet-[a-z0-9]+$")`) and `SecurityGroupIds`
    // (`@length(1..10)`, `@uniqueItems`, member `@pattern("^sg-[a-z0-9]+$")`)
    // constraints, including each published bound and one entry past it.
    let duplicated = vec![subnets(1)[0].clone(), subnets(1)[0].clone()];
    let cases: Vec<ConstraintCase> = vec![
        (
            "subnets at the published bound",
            "vpc-ok1",
            subnets(MAX_SUBNET_IDS),
            Vec::new(),
            true,
        ),
        (
            "one subnet past the published bound",
            "vpc-bad1",
            subnets(MAX_SUBNET_IDS + 1),
            Vec::new(),
            false,
        ),
        ("an empty subnet list", "vpc-bad2", Vec::new(), Vec::new(), false),
        ("a repeated subnet id", "vpc-bad3", duplicated, Vec::new(), false),
        (
            "a subnet id outside the member pattern",
            "vpc-bad4",
            vec!["subnet_0a1b".to_string()],
            Vec::new(),
            false,
        ),
        (
            "security groups at the published bound",
            "vpc-ok2",
            subnets(1),
            security_groups(MAX_SECURITY_GROUP_IDS),
            true,
        ),
        (
            "one security group past the published bound",
            "vpc-bad5",
            subnets(1),
            security_groups(MAX_SECURITY_GROUP_IDS + 1),
            false,
        ),
        (
            "a security group id outside the member pattern",
            "vpc-bad6",
            subnets(1),
            vec!["SG-0F1E".to_string()],
            false,
        ),
        (
            "a vpc id outside the modelled pattern",
            "vpc_bad7",
            subnets(1),
            Vec::new(),
            false,
        ),
    ];

    for (case, vpc_id, subnet_ids, security_group_ids, accepted) in cases {
        let mut body = json!({"vpcId": vpc_id, "subnetIds": subnet_ids});
        if !security_group_ids.is_empty() {
            body["vpcSecurityGroupIds"] = json!(security_group_ids);
        }
        let (status, response) = create_endpoint(&router, &graph_id, body).await;
        if accepted {
            assert_eq!(
                status, 201,
                "case {case:?}: expected 201, got {status}: {response}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {response}"
        );
        let code = error_code(&response);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?}: {response}"
        );
        // A refused request persisted nothing, so the vpc it named still has no
        // endpoint.
        let (read_status, read) = get_endpoint(&router, &graph_id, vpc_id).await;
        assert_ne!(
            read_status, 200,
            "case {case:?}: a refused create must persist nothing, but the read \
             answered {read}"
        );
    }
}

// ---------------------------------------------------------------------------
// The network the service resolves when the request leaves it out
// ---------------------------------------------------------------------------

/// The EC2 API version an `ec2Query` body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The Regions the regional row of the resolution runs over: the one the rest of this
/// suite uses and two more, so "the network of the Region the request was signed for" is
/// asserted over Regions rather than over one.
const INVENTORY_REGIONS: [&str; 3] = [REGION, "us-west-2", "eu-central-1"];

/// A Region code that is well formed but names no Region: an account holds a default VPC
/// in the Regions that EXIST, so a scope naming this one resolves to no network.
const REGION_OUTSIDE_THE_CATALOG: &str = "zz-nowhere-1";

/// An emulator holding this service AND the service that owns the account's VPCs and
/// subnets, on ONE engine — the way the assembled router mounts every service, and what
/// lets a row read the network back out of the service that owns it.
fn emulator_with_vpc_inventory() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2::EmulatedService::new(engine));
    router
}

/// One `ec2Query` call signed for `region` — an `application/x-www-form-urlencoded` POST
/// whose body carries `Action=`, answered with XML — asserted successful, as its body.
async fn ec2_call(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut form = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        form.push('&');
        form.push_str(name);
        form.push('=');
        form.push_str(value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ec2.{region}.amazonaws.com/"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/ec2/aws4_request, \
                 SignedHeaders=host, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(form))
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {region}/{action}: expected 200 from the VPC inventory, got {status}: {body}"
    );
    body
}

/// The text of every `<name>…</name>` element of an XML body, in document order.
fn xml_elements(xml: &str, name: &str) -> Vec<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        match rest.find(&close) {
            Some(end) => {
                found.push(rest[..end].to_string());
                rest = &rest[end + close.len()..];
            }
            None => break,
        }
    }
    found
}

/// The subnets the account holds in `vpc_id`, read through the OWNING service's own
/// `DescribeSubnets` narrowed by its documented `vpc-id` filter.
async fn subnets_of_vpc(router: &MultiServiceRouter, region: &str, vpc_id: &str) -> Vec<String> {
    let body = ec2_call(
        router,
        region,
        "DescribeSubnets",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc_id)],
    )
    .await;
    xml_elements(&body, "subnetId")
}

/// The account's default VPC in `region` and the subnets it holds, read through the
/// owning service's own reads: `DescribeVpcs` narrowed by the documented `is-default`
/// filter, then [`subnets_of_vpc`]. What a resolved endpoint reports has to be exactly
/// this.
async fn default_network(router: &MultiServiceRouter, region: &str) -> (String, Vec<String>) {
    let body = ec2_call(
        router,
        region,
        "DescribeVpcs",
        &[("Filter.1.Name", "is-default"), ("Filter.1.Value.1", "true")],
    )
    .await;
    let reported = xml_elements(&body, "vpcId");
    assert_eq!(
        reported.len(),
        1,
        "case {region}: expected exactly one default VPC of the Region, got {reported:?} \
         from {body}"
    );
    let vpc_id = reported[0].clone();
    let subnet_ids = subnets_of_vpc(router, region, &vpc_id).await;
    (vpc_id, subnet_ids)
}

/// A VPC of the account in `region` at `cidr_block`, created through the owning service's
/// own `CreateVpc`, as its `VpcId`.
async fn created_vpc(router: &MultiServiceRouter, region: &str, cidr_block: &str) -> String {
    let body = ec2_call(router, region, "CreateVpc", &[("CidrBlock", cidr_block)]).await;
    xml_elements(&body, "vpcId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {region}/{cidr_block}: expected a vpcId in {body}"))
}

/// A subnet of `vpc_id` at `cidr_block`, created through the owning service's own
/// `CreateSubnet`.
async fn created_subnet(
    router: &MultiServiceRouter,
    region: &str,
    vpc_id: &str,
    cidr_block: &str,
) -> String {
    let body = ec2_call(
        router,
        region,
        "CreateSubnet",
        &[("VpcId", vpc_id), ("CidrBlock", cidr_block)],
    )
    .await;
    xml_elements(&body, "subnetId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {region}/{cidr_block}: expected a subnetId in {body}"))
}

/// Create a graph in `region` and wait it into service, returning its id.
async fn graph_in_service_in(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "graphName": name,
        "provisionedMemory": MIN_PROVISIONED_MEMORY,
    });
    let (status, created) = call_in(router, region, "POST", "/graphs", Some(body.clone())).await;
    assert_eq!(
        status, 201,
        "case {region}/{name}: expected 201 from CreateGraph, got {status}: {created}"
    );
    let id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {region}/{name}: no id in {created}"))
        .to_string();
    // The graph's provisioning is owed by the clock; no handler sleeps, this row only
    // looks again until the condition it is waiting for holds.
    for _ in 0..40 {
        let (status, graph) = call_in(router, region, "GET", &format!("/graphs/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {region}/{name}: expected 200 from GetGraph, got {status}: {graph}"
        );
        if graph["status"] == json!("AVAILABLE") {
            return id;
        }
        tokio::time::sleep(PROVISIONING_SPAN / 4).await;
    }
    panic!("case {region}/{name}: the graph never became AVAILABLE");
}

/// One row of the resolution table: the case, the request body, the `vpcId` the answer
/// must carry and the `subnetIds` it must carry.
type ResolutionCase = (&'static str, Value, String, Vec<String>);

#[tokio::test]
async fn a_create_resolves_the_members_it_is_not_given_from_the_account_inventory() {
    let router = emulator_with_vpc_inventory();
    let (default_vpc, default_subnets) = default_network(&router, REGION).await;
    assert!(
        (1..=MAX_SUBNET_IDS).contains(&default_subnets.len()),
        "case the account's default network: expected between one and {MAX_SUBNET_IDS} \
         subnets, got {default_subnets:?}"
    );

    // A SECOND VPC of the account, with subnets of its own, so a row that names a VPC is
    // resolved against THAT VPC's subnets and not against the default VPC's.
    let other_vpc = created_vpc(&router, REGION, "10.20.0.0/16").await;
    let mut other_subnets = Vec::new();
    for index in 0..2 {
        let cidr_block = format!("10.20.{index}.0/24");
        other_subnets.push(created_subnet(&router, REGION, &other_vpc, &cidr_block).await);
    }
    assert_eq!(
        subnets_of_vpc(&router, REGION, &other_vpc).await,
        other_subnets,
        "case the account's second VPC: the subnets it holds are the ones just created"
    );

    // The rows differ in WHICH member the request leaves to the service, and in which VPC
    // of the account the resolution has to read: the recorded form (neither member) is one
    // row of it.
    let cases: Vec<ResolutionCase> = vec![
        (
            "neither member",
            json!({}),
            default_vpc.clone(),
            default_subnets.clone(),
        ),
        (
            "no vpcId, one subnet of the default VPC named",
            json!({"subnetIds": default_subnets[..1]}),
            default_vpc.clone(),
            default_subnets[..1].to_vec(),
        ),
        (
            "no subnetIds, the default VPC named",
            json!({"vpcId": default_vpc}),
            default_vpc.clone(),
            default_subnets.clone(),
        ),
        (
            "no subnetIds, another VPC of the account named",
            json!({"vpcId": other_vpc}),
            other_vpc.clone(),
            other_subnets.clone(),
        ),
        (
            // A subnet belongs to ONE VPC, so naming the subnets of a non-default VPC
            // names that VPC: the resolved `vpcId` must be their holder rather than the
            // account's default VPC.
            "no vpcId, the subnets of another VPC of the account named",
            json!({"subnetIds": other_subnets}),
            other_vpc.clone(),
            other_subnets.clone(),
        ),
        (
            "neither member, security groups attached",
            json!({"vpcSecurityGroupIds": security_groups(2)}),
            default_vpc.clone(),
            default_subnets.clone(),
        ),
    ];

    for (index, (case, body, expected_vpc, expected_subnets)) in cases.into_iter().enumerate() {
        // One endpoint per (graph, vpc), so each row gets its own graph and the rows that
        // land in the same VPC do not conflict with one another.
        let case_graph = format!("resolution-source-{index}");
        let graph_id = graph_in_service_in(&router, REGION, &case_graph).await;
        let (status, created) = create_endpoint(&router, &graph_id, body.clone()).await;
        assert_eq!(
            status, 201,
            "case {case:?} ({body}): expected 201, got {status}: {created}"
        );
        assert_eq!(
            created["vpcId"],
            json!(expected_vpc),
            "case {case:?}: expected vpcId {expected_vpc:?}, got {}",
            created["vpcId"]
        );
        assert_eq!(
            created["subnetIds"],
            json!(expected_subnets),
            "case {case:?}: expected subnetIds {}, got {}",
            json!(expected_subnets),
            created["subnetIds"]
        );
        assert_eq!(
            created["status"],
            json!("CREATING"),
            "case {case:?}: an endpoint is not reachable when the call returns, expected \
             CREATING, got {}",
            created["status"]
        );
        let vpc_endpoint_id = created["vpcEndpointId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no vpcEndpointId in {created}"))
            .to_string();
        assert!(
            is_vpc_endpoint_id(&vpc_endpoint_id),
            "case {case:?}: expected an id matching ^vpce-[0-9a-f]{{17}}$, got \
             {vpc_endpoint_id:?}"
        );

        // The resolved network is one the ACCOUNT holds: every subnet reported is a subnet
        // the owning service reports in the VPC reported beside it.
        let held = subnets_of_vpc(&router, REGION, &expected_vpc).await;
        for subnet_id in &expected_subnets {
            assert!(
                held.contains(subnet_id),
                "case {case:?}: the endpoint reports the subnet {subnet_id:?}, which the \
                 account does not hold in {expected_vpc}: {held:?}"
            );
        }

        // The round trip: the read reports the resolved record, so what was stored is what
        // the create answered.
        let (read_status, read) = get_endpoint(&router, &graph_id, &expected_vpc).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected 200 from GetPrivateGraphEndpoint for the resolved \
             {expected_vpc}, got {read_status}: {read}"
        );
        for member in ["vpcId", "subnetIds", "vpcEndpointId"] {
            assert_eq!(
                read[member], created[member],
                "case {case:?}: GetPrivateGraphEndpoint's {member} must be the stored value \
                 {}, got {}",
                created[member], read[member]
            );
        }
    }
}

#[tokio::test]
async fn the_resolved_network_is_the_one_the_account_holds_in_the_signed_region() {
    let router = emulator_with_vpc_inventory();

    let mut resolved: Vec<(&str, String, Vec<String>)> = Vec::new();
    for region in INVENTORY_REGIONS {
        // A graph name is unique per Region, so the same name in each Region is one graph
        // of that Region.
        let graph_id = graph_in_service_in(&router, region, "regional-endpoint-source").await;
        let (status, created) = call_in(
            &router,
            region,
            "POST",
            &format!("/graphs/{graph_id}/endpoints/"),
            Some(json!({})),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {region:?}: expected 201, got {status}: {created}"
        );
        // Read the account's network AFTER the create, so the ids the create answered have
        // to be the ids the owning service's own reads report.
        let (expected_vpc, expected_subnets) = default_network(&router, region).await;
        assert_eq!(
            created["vpcId"],
            json!(expected_vpc),
            "case {region:?}: expected the Region's default VPC {expected_vpc:?}, got {}",
            created["vpcId"]
        );
        assert_eq!(
            created["subnetIds"],
            json!(expected_subnets),
            "case {region:?}: expected the subnets of {expected_vpc}, {}, got {}",
            json!(expected_subnets),
            created["subnetIds"]
        );

        for (other_region, other_vpc, other_subnets) in &resolved {
            assert_ne!(
                &expected_vpc, other_vpc,
                "case {region:?} vs {other_region:?}: each Region resolves to its OWN \
                 default VPC, got {expected_vpc} twice"
            );
            let shared: Vec<&String> = expected_subnets
                .iter()
                .filter(|id| other_subnets.contains(id))
                .collect();
            assert!(
                shared.is_empty(),
                "case {region:?} vs {other_region:?}: a subnet belongs to one Region, got \
                 {shared:?}"
            );
        }
        resolved.push((region, expected_vpc, expected_subnets));
    }
}

#[tokio::test]
async fn a_vpc_holding_more_subnets_than_the_member_carries_resolves_to_the_bound() {
    let router = emulator_with_vpc_inventory();
    let graph_id = graph_in_service_in(&router, REGION, "crowded-vpc-source").await;

    // A VPC of the account holding one subnet more than `SubnetIds` may carry.
    let vpc_id = created_vpc(&router, REGION, "10.30.0.0/16").await;
    for index in 0..=MAX_SUBNET_IDS {
        created_subnet(&router, REGION, &vpc_id, &format!("10.30.{index}.0/24")).await;
    }
    let held = subnets_of_vpc(&router, REGION, &vpc_id).await;
    assert_eq!(
        held.len(),
        MAX_SUBNET_IDS + 1,
        "case the crowded VPC: expected {} subnets in the account, got {held:?}",
        MAX_SUBNET_IDS + 1
    );

    let (status, created) = create_endpoint(&router, &graph_id, json!({"vpcId": vpc_id})).await;
    assert_eq!(
        status, 201,
        "case the crowded VPC: expected 201, got {status}: {created}"
    );
    assert_eq!(
        created["subnetIds"],
        json!(held[..MAX_SUBNET_IDS]),
        "case the crowded VPC: a resolved list carries at most the modelled {MAX_SUBNET_IDS} \
         subnets, expected {}, got {}",
        json!(held[..MAX_SUBNET_IDS]),
        created["subnetIds"]
    );
}

#[tokio::test]
async fn a_network_the_account_does_not_hold_is_refused_rather_than_invented() {
    let router = emulator_with_vpc_inventory();
    let graph_id = graph_in_service_in(&router, REGION, "empty-vpc-source").await;

    // A VPC of the account with no subnet in it: `subnetIds` is `@length(1..6)`, so there
    // is nothing for the service to resolve the member from, and the endpoint is refused
    // rather than created with a subnet nothing holds.
    let empty_vpc = created_vpc(&router, REGION, "10.40.0.0/16").await;
    assert!(
        subnets_of_vpc(&router, REGION, &empty_vpc).await.is_empty(),
        "case the empty VPC: this row needs a VPC the account holds no subnet in"
    );
    let (status, response) =
        create_endpoint(&router, &graph_id, json!({"vpcId": empty_vpc})).await;
    assert_eq!(
        status, 400,
        "case the empty VPC: expected 400, got {status}: {response}"
    );
    let code = error_code(&response);
    assert_eq!(
        code, "ValidationException",
        "case the empty VPC: expected ValidationException, got {code:?}: {response}"
    );
    assert_eq!(
        response["reason"],
        json!("BAD_REQUEST"),
        "case the empty VPC: a well-formed request the account's state leaves nothing to \
         resolve from is the bad request it is, got {}",
        response["reason"]
    );
    let (read_status, read) = get_endpoint(&router, &graph_id, &empty_vpc).await;
    assert_ne!(
        read_status, 200,
        "case the empty VPC: a refused create must persist nothing, but the read answered \
         {read}"
    );
    let empty_vpc_message = text_of(&response, "message");

    // A VPC the account does NOT hold, with the subnets left to the service: there is no
    // VPC to read them off at all, which is a different condition and is worded apart so a
    // caller learns which of the two it sent.
    let absent_vpc = "vpc-0000000000nosuch";
    assert!(
        subnets_of_vpc(&router, REGION, absent_vpc).await.is_empty(),
        "case a VPC the account does not hold: this row needs an id naming no VPC"
    );
    let (status, response) =
        create_endpoint(&router, &graph_id, json!({"vpcId": absent_vpc})).await;
    assert_eq!(
        status, 400,
        "case a VPC the account does not hold: expected 400, got {status}: {response}"
    );
    let code = error_code(&response);
    assert_eq!(
        code, "ValidationException",
        "case a VPC the account does not hold: expected ValidationException, got {code:?}: \
         {response}"
    );
    let absent_vpc_message = text_of(&response, "message");
    assert!(
        absent_vpc_message.contains(absent_vpc),
        "case a VPC the account does not hold: the refusal must name the VPC the request \
         sent, got {absent_vpc_message:?}"
    );
    assert_ne!(
        absent_vpc_message, empty_vpc_message,
        "case a VPC the account does not hold: a VPC that does not exist and a VPC holding \
         no subnet are two conditions, so they cannot share one sentence"
    );

    // And a request signed for a Region code that names no Region at all: an account has
    // a default VPC in the Regions that EXIST, so caller-controlled scope text resolves to
    // no network and the request is refused rather than answered with ids nothing holds.
    let unknown_region_graph =
        graph_in_service_in(&router, REGION_OUTSIDE_THE_CATALOG, "unknown-region-source").await;
    let (status, response) = call_in(
        &router,
        REGION_OUTSIDE_THE_CATALOG,
        "POST",
        &format!("/graphs/{unknown_region_graph}/endpoints/"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a Region outside the catalog: expected 400, got {status}: {response}"
    );
    let code = error_code(&response);
    assert_eq!(
        code, "ValidationException",
        "case a Region outside the catalog: expected ValidationException, got {code:?}: \
         {response}"
    );
    assert_eq!(
        response["reason"],
        json!("BAD_REQUEST"),
        "case a Region outside the catalog: expected the bad-request reason, got {}",
        response["reason"]
    );
}
