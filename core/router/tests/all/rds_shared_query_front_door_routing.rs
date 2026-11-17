// SPDX-License-Identifier: CC0-1.0

//! Amazon DocumentDB's subnet-group and cluster-parameter-group paths, through the WHOLE
//! assembled router.
//!
//! The shared `rds` signing name is Amazon RDS's — three models publish one wire identity
//! (`AmazonRDSv19`, `2014-10-31`, `awsQuery`, endpointPrefix and sigv4 name `rds`) and the
//! key resolves to the service whose own identity it is, which
//! `all/rds_docdb_neptune_signing_name_routing.rs` pins. Each peer is reachable under the
//! key that names IT, and this suite pins what Amazon DocumentDB ANSWERS there: before the
//! change behind it, every one of these requests was the router's `500 AmbiguousService` as
//! `application/x-amz-json-1.1` — a code in no AWS model, in a body an awsQuery client
//! cannot parse — because all three models claimed the key.
//!
//! The rules the rows assert, each over the whole router rather than one mounted service:
//!
//! * a request that names DocumentDB reaches exactly ONE service (never
//!   `AmbiguousService`, never `UnknownService`) and is answered in that family's own
//!   `text/xml` envelope;
//! * a subnet group created there reads back there, and a name state does not hold is the
//!   modelled `404 DBSubnetGroupNotFoundFault` — on the describe and on the delete;
//! * a cluster parameter group the model publishes reads back, and a name it does not hold
//!   is the modelled `404 DBParameterGroupNotFound`;
//! * an `Action` the model does not declare is the query protocol's own `400 InvalidAction`
//!   as `text/xml`, never a JSON router refusal.
//!
//! A subnet is not DocumentDB's resource but Amazon EC2's, and that front door resolves
//! every `SubnetIds` member against the service that owns subnets, so the group this suite
//! creates is built out of subnets MINTED through EC2 on the very router under test — which
//! is also the second thing assembly has to survive: the in-process hop from one mounted
//! service to another.
//!
//! The crate's own suite (`services/service-com-amazonaws-docdb-amazonrdsv19/tests/all/`)
//! states these rules over tables of inputs; this one proves they survive assembly with
//! every other service mounted. Assembling the router costs stack (see `boot_serve.rs`), so
//! it is built — and the requests driven — on an explicitly sized thread, and the harness
//! for this family comes from the sibling suite rather than a second copy.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::rds_docdb_neptune_signing_name_routing::{authorization, API_VERSION, BOOT_STACK_SIZE};

/// The ec2Query API version Amazon EC2 publishes, for the calls that mint the network the
/// subnet group below is made of.
const EC2_API_VERSION: &str = "2016-11-15";

/// The form-urlencoded body of a query-protocol call: the `Action`, the `Version`, and the
/// members, each value percent-encoded.
fn body(action: &str, version: &str, params: &[(String, String)]) -> bytes::Bytes {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    body.push(char::from(byte));
                }
                other => body.push_str(&format!("%{other:02X}")),
            }
        }
    }
    bytes::Bytes::from(body)
}

/// The members of one call, as the rows below write them.
fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// One call through the assembled router, signed for `signing_name`: the HTTP status, the
/// `content-type` it was answered under, and the body.
fn call(
    runtime: &tokio::runtime::Runtime,
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(String, String)],
) -> (u16, String, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body(action, version, members))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    (status, content_type, rendered)
}

/// The text of the first `<name>` element of a response, or `None`.
fn element(rendered: &str, name: &str) -> Option<String> {
    let (_, rest) = rendered.split_once(&format!("<{name}>"))?;
    rest.split_once(&format!("</{name}>"))
        .map(|(text, _)| text.to_string())
}

/// One ec2Query call that has to succeed, answering the `member` element it reported — the
/// id EC2 MINTED, which is the only kind of subnet or VPC id another service can resolve.
fn ec2_minted(
    runtime: &tokio::runtime::Runtime,
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
    member: &str,
) -> String {
    let (status, _content_type, rendered) =
        call(runtime, router, "ec2", EC2_API_VERSION, action, members);
    assert_eq!(
        status, 200,
        "the network this suite names must be minted through EC2: {action} answered {status} \
         with {rendered}"
    );
    element(&rendered, member).unwrap_or_else(|| {
        panic!("{action} must report the <{member}> it minted, got {rendered}")
    })
}

/// The `<Code>` of an awsQuery `<ErrorResponse>`, or of the router's JSON refusal.
fn error_code(rendered: &str) -> Option<String> {
    if let Some((_, rest)) = rendered.split_once("<Code>") {
        return rest.split_once("</Code>").map(|(code, _)| code.to_string());
    }
    serde_json::from_str::<serde_json::Value>(rendered)
        .ok()
        .and_then(|json| {
            json.get("__type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

/// What a row expects of the answer, beyond "it reached exactly one service".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The service answered successfully.
    Ok,
    /// The service answered this wire error code, with a 4xx.
    Code(&'static str),
}

/// One row: the case, the awsQuery `Action`, its members, and what must come back.
type Row = (&'static str, &'static str, Vec<(String, String)>, Expect);

#[test]
fn documentdb_serves_its_subnet_and_parameter_group_paths_through_the_router() {
    std::thread::Builder::new()
        .name("docdb-group-paths".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);

            let runtime = tokio::runtime::Builder::new_current_thread()
                // IO and timers are on because the calls below reach services that open a
                // core listener, and because one mounted service calls another in process.
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // The network the subnet group is made of, minted through the owner of subnets
            // on this same router: a VPC and two subnets in two Availability Zones of the
            // Region every request here is signed for.
            let vpc_id = ec2_minted(
                &runtime,
                &router,
                "CreateVpc",
                &params(&[("CidrBlock", "10.71.0.0/16")]),
                "vpcId",
            );
            let subnets: Vec<String> = ["us-east-1a", "us-east-1b"]
                .iter()
                .enumerate()
                .map(|(index, zone)| {
                    ec2_minted(
                        &runtime,
                        &router,
                        "CreateSubnet",
                        &params(&[
                            ("VpcId", vpc_id.as_str()),
                            ("CidrBlock", &format!("10.71.{}.0/24", index + 1)),
                            ("AvailabilityZone", zone),
                        ]),
                        "subnetId",
                    )
                })
                .collect();

            // The rows walk one lifecycle and then the not-found answers either side of it,
            // so each later row depends on the state an earlier one left: the create, the
            // read-back, the cluster that references the group, the refusals, the delete,
            // and the read that must no longer find it.
            let rows: [Row; 12] = [
                (
                    "create a subnet group",
                    "CreateDBSubnetGroup",
                    params(&[
                        ("DBSubnetGroupName", "router-subnets"),
                        ("DBSubnetGroupDescription", "created through the router"),
                        ("SubnetIds.SubnetIdentifier.1", subnets[0].as_str()),
                        ("SubnetIds.SubnetIdentifier.2", subnets[1].as_str()),
                    ]),
                    Expect::Ok,
                ),
                (
                    "read the group back",
                    "DescribeDBSubnetGroups",
                    params(&[("DBSubnetGroupName", "router-subnets")]),
                    Expect::Ok,
                ),
                (
                    // A subnet id the account has no subnet for reaches the owner of
                    // subnets through the assembled router and is refused there: awsQuery's
                    // documented common code for a value that cannot be used.
                    "create a subnet group over a subnet nobody minted",
                    "CreateDBSubnetGroup",
                    params(&[
                        ("DBSubnetGroupName", "unheld-subnets"),
                        ("DBSubnetGroupDescription", "a group of unheld subnets"),
                        ("SubnetIds.SubnetIdentifier.1", subnets[0].as_str()),
                        ("SubnetIds.SubnetIdentifier.2", "subnet-00000000000000000"),
                    ]),
                    Expect::Code("InvalidParameterValue"),
                ),
                (
                    // The issue's own step: a subnet group that does not exist is the
                    // modelled 404, not a 500 refusal.
                    "describe a subnet group that does not exist",
                    "DescribeDBSubnetGroups",
                    params(&[("DBSubnetGroupName", "no-such-group")]),
                    Expect::Code("DBSubnetGroupNotFoundFault"),
                ),
                (
                    "delete a subnet group that does not exist",
                    "DeleteDBSubnetGroup",
                    params(&[("DBSubnetGroupName", "no-such-group")]),
                    Expect::Code("DBSubnetGroupNotFoundFault"),
                ),
                (
                    // A cluster create that names the stored group: the reference resolves.
                    "create a cluster in the group",
                    "CreateDBCluster",
                    params(&[
                        ("DBClusterIdentifier", "router-cluster"),
                        ("Engine", "docdb"),
                        ("MasterUsername", "admin"),
                        ("MasterUserPassword", "secret99"),
                        ("DBSubnetGroupName", "router-subnets"),
                    ]),
                    Expect::Ok,
                ),
                (
                    // …and one that names a group nothing holds is the modelled 404.
                    "create a cluster in a group that does not exist",
                    "CreateDBCluster",
                    params(&[
                        ("DBClusterIdentifier", "refused-cluster"),
                        ("Engine", "docdb"),
                        ("MasterUsername", "admin"),
                        ("MasterUserPassword", "secret99"),
                        ("DBSubnetGroupName", "no-such-group"),
                    ]),
                    Expect::Code("DBSubnetGroupNotFoundFault"),
                ),
                (
                    // An engine version the engine does not publish: awsQuery's documented
                    // common code, not a success and not a 5xx.
                    "create a cluster on a version the engine does not publish",
                    "CreateDBCluster",
                    params(&[
                        ("DBClusterIdentifier", "refused-version"),
                        ("Engine", "docdb"),
                        ("MasterUsername", "admin"),
                        ("MasterUserPassword", "secret99"),
                        ("EngineVersion", "99.9.9"),
                    ]),
                    Expect::Code("InvalidParameterCombination"),
                ),
                (
                    // The group is in use by the cluster created above.
                    "delete the group while a cluster is in it",
                    "DeleteDBSubnetGroup",
                    params(&[("DBSubnetGroupName", "router-subnets")]),
                    Expect::Code("InvalidDBSubnetGroupStateFault"),
                ),
                (
                    "a cluster parameter group the model publishes",
                    "DescribeDBClusterParameterGroups",
                    params(&[("DBClusterParameterGroupName", "default.docdb5.0")]),
                    Expect::Ok,
                ),
                (
                    // The answer a waiter polls a deleted parameter group for.
                    "a cluster parameter group that does not exist",
                    "DescribeDBClusterParameterGroups",
                    params(&[("DBClusterParameterGroupName", "no-such-group")]),
                    // The model's `awsQueryError` code for `DBParameterGroupNotFoundFault`.
                    Expect::Code("DBParameterGroupNotFound"),
                ),
                (
                    // No operation of this model is named, so the query protocol's own
                    // routing error answers — as text/xml, which is the second half of the
                    // issue: the refusal must be parseable by an awsQuery client.
                    "an Action this model does not declare",
                    "DescribeNothingAtAll",
                    Vec::new(),
                    Expect::Code("InvalidAction"),
                ),
            ];

            for (case, action, members, expect) in rows {
                // The key that names Amazon DocumentDB itself; the shared `rds` name is
                // Amazon RDS's (see the module docs).
                let (status, content_type, rendered) =
                    call(&runtime, &router, "docdb", API_VERSION, action, &members);
                let code = error_code(&rendered);

                // No request may be a routing failure: `AmbiguousService` (two or more
                // claimants) or `UnknownService` (none).
                assert!(
                    code.as_deref() != Some("AmbiguousService")
                        && code.as_deref() != Some("UnknownService"),
                    "case {case:?} ({action}): expected exactly one claimant, got {status} \
                     {content_type} with {rendered}"
                );
                // Every answer is this family's own envelope, never the router's JSON.
                assert!(
                    content_type.starts_with("text/xml"),
                    "case {case:?} ({action}): an awsQuery service answers text/xml, got \
                     {content_type} with {rendered}"
                );

                match expect {
                    Expect::Ok => assert_eq!(
                        status, 200,
                        "case {case:?} ({action}): expected 200, got {status} with {rendered}"
                    ),
                    Expect::Code(expected) => assert_eq!(
                        (status < 500, code.as_deref()),
                        (true, Some(expected)),
                        "case {case:?} ({action}): expected the 4xx {expected}, got {status} \
                         with {rendered}"
                    ),
                }
            }

            // The group the rows created holds the subnets EC2 minted, with the VPC and the
            // zones EC2 reports for them: the in-process resolution survives assembly.
            let (status, _content_type, rendered) = call(
                &runtime,
                &router,
                "docdb",
                API_VERSION,
                "DescribeDBSubnetGroups",
                &params(&[("DBSubnetGroupName", "router-subnets")]),
            );
            assert_eq!(
                status, 200,
                "the stored group must read back through the router, got {status} with {rendered}"
            );
            assert_eq!(
                element(&rendered, "VpcId").as_deref(),
                Some(vpc_id.as_str()),
                "the group must report the VPC EC2 holds its subnets in, got {rendered}"
            );
            for subnet_id in &subnets {
                assert!(
                    rendered.contains(subnet_id.as_str()),
                    "the group must report the subnet {subnet_id} it was created over, got \
                     {rendered}"
                );
            }
            for zone in ["us-east-1a", "us-east-1b"] {
                assert!(
                    rendered.contains(zone),
                    "the group must report the Availability Zone {zone} EC2 holds one of its \
                     subnets in, got {rendered}"
                );
            }
        })
        .expect("spawn the routing thread")
        .join()
        .expect("DocumentDB's group paths must be served through the assembled router");
}
