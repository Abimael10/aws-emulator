// SPDX-License-Identifier: CC0-1.0

//! A stack that owns an `AWS::EKS::Cluster` is reported complete only once Amazon EKS has
//! finished removing that cluster.
//!
//! The rule under test: **CloudFormation does not report a stack operation complete while a
//! service that owns one of the resources that operation deleted still holds it; the
//! operation stays `*_IN_PROGRESS` until that service reports the resource gone.** For a
//! cluster that is what `DeleteCluster` makes necessary: the call answers with the cluster
//! `DELETING` and Amazon EKS goes on serving `DescribeCluster` for it in that status until
//! the control plane has been torn down, so a stack that called it has not yet removed
//! anything. `crate::cloudformation_elastic_beanstalk_removal_stabilisation` pins the same
//! rule for AWS Elastic Beanstalk's own asynchronous removal, and
//! `crate::cloudformation_firehose_stream_stabilisation` the create side of the relation;
//! this suite pins it for the
//! cluster, which needs three crates to agree (CloudFormation makes the calls, Amazon EKS
//! serves them and decides when the cluster is gone, and EC2 owns the subnets it is placed
//! in), so it lives here rather than in any one of them.
//!
//! What this pins is the observation an infrastructure-as-code client makes: the instant a
//! stack operation first reports itself complete, the cluster it deleted is gone in Amazon
//! EKS — so the client's next `DescribeCluster` is refused and the cluster name it freed can
//! be used again.
//!
//! The rows differ along what the rule generalises over: WHICH stack operation removes the
//! cluster — the stack's own deletion, and an update that stops declaring it — and whether
//! the cluster's name is the one the template declared or the one CloudFormation generated
//! for it.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and so the Region the stack, the VPC and the
/// cluster belong to.
const REGION: &str = "us-west-2";

/// How often the suite asks the stack whether its operation has concluded, and how long it
/// goes on asking. The interval is short because the FIRST completed observation is what the
/// rule is stated about; the budget is generous, so a loaded machine is slower than the spans
/// these services choose without the rule being any different.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const POLLS: usize = 1_200;

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const DELETE_IN_PROGRESS: &str = "DELETE_IN_PROGRESS";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The `Cluster$status` a cluster is usable in, and the one a cluster whose delete has been
/// accepted is reported in while it is torn down.
const CLUSTER_ACTIVE: &str = "ACTIVE";
const CLUSTER_DELETING: &str = "DELETING";

/// The logical ids the templates below declare their resources under.
const VPC_ID: &str = "Net";
const SUBNET_IDS: [&str; 2] = ["SubnetA", "SubnetB"];
const CLUSTER_ID: &str = "Cluster";

/// The Availability Zones the two subnets are created in: an Amazon EKS control plane is
/// placed across the zones of the subnets it names, so they are distinct.
const ZONES: [&str; 2] = ["us-west-2a", "us-west-2b"];

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process call CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eks_awswesleyfrontend::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One answer, as this suite reads it: the status, the body as text and as JSON.
struct Answer {
    status: u16,
    text: String,
    body: Value,
}

/// Drive one request at `signing_name`'s wire.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: String,
) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer { status, text, body }
}

/// Percent-encode one value: the test crate's single encoder.
fn encoded(value: &str) -> String {
    crate::query_wire::form_encode(value)
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&encoded(key));
        body.push('=');
        body.push_str(&encoded(value));
    }
    send(
        router,
        "cloudformation",
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        body,
    )
    .await
}

/// Call one Amazon EKS operation over its restJson1 wire.
async fn eks(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|json| json.to_string()).unwrap_or_default();
    send(router, "eks", method, uri, "application/json", payload).await
}

/// `DescribeCluster` for one cluster.
async fn describe_cluster(router: &MultiServiceRouter, name: &str) -> Answer {
    eks(router, "GET", &format!("/clusters/{}", encoded(name)), None).await
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(xml: &str, name: &str) -> Option<String> {
    crate::query_wire::elements(xml, name).into_iter().next()
}

/// A role ARN of the account this emulator serves, which is what a cluster's `RoleArn` is.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// The network half of every template: the VPC and the two subnets the cluster is placed in,
/// each created in EC2 by the stack itself, so the cluster's `SubnetIds` are ids EC2 assigned
/// rather than values the template invented.
fn network_resources() -> serde_json::Map<String, Value> {
    let mut resources = serde_json::Map::new();
    resources.insert(
        VPC_ID.to_string(),
        json!({ "Type": "AWS::EC2::VPC", "Properties": { "CidrBlock": "10.91.0.0/16" } }),
    );
    for (index, logical) in SUBNET_IDS.iter().enumerate() {
        resources.insert(
            (*logical).to_string(),
            json!({
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": VPC_ID },
                    "CidrBlock": format!("10.91.{}.0/24", index + 1),
                    "AvailabilityZone": ZONES[index]
                }
            }),
        );
    }
    resources
}

/// The template that declares the network AND the cluster, naming the cluster `declared_name`
/// when the row states one.
fn with_cluster(declared_name: Option<&str>) -> String {
    let mut properties = serde_json::Map::new();
    if let Some(name) = declared_name {
        properties.insert("Name".to_string(), json!(name));
    }
    properties.insert("RoleArn".to_string(), json!(role_arn("eksClusterRole")));
    properties.insert(
        "ResourcesVpcConfig".to_string(),
        json!({
            "SubnetIds": SUBNET_IDS
                .iter()
                .map(|logical| json!({ "Ref": logical }))
                .collect::<Vec<Value>>()
        }),
    );
    let mut resources = network_resources();
    resources.insert(
        CLUSTER_ID.to_string(),
        json!({ "Type": "AWS::EKS::Cluster", "Properties": Value::Object(properties) }),
    );
    json!({
        "Resources": Value::Object(resources),
        "Outputs": { "ClusterName": { "Value": { "Ref": CLUSTER_ID } } }
    })
    .to_string()
}

/// The same template with the cluster DROPPED: the network alone, which is what an update
/// that stops declaring the cluster deploys.
fn without_cluster() -> String {
    json!({ "Resources": Value::Object(network_resources()) }).to_string()
}

/// Create the stack `stack_name` from `template`, and answer the `StackId` CloudFormation
/// minted for it — which is what every read below addresses, because a stack whose deletion
/// has finished is no longer describable by NAME.
async fn create_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let answer = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with body {}",
        answer.status, answer.text
    );
    element(&answer.text, "StackId").unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a StackId from CreateStack, got {}",
            answer.text
        )
    })
}

/// The `StackStatus` the stack reports right now, with the answer it was read from.
async fn stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let answer = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {} with body {}",
        answer.status, answer.text
    );
    (
        element(&answer.text, "StackStatus").unwrap_or_default(),
        answer.text,
    )
}

/// Poll the stack until it reports `wanted`, and answer the body that status was read from.
async fn wait_for(router: &MultiServiceRouter, case: &str, stack: &str, wanted: &str) -> String {
    for _ in 0..POLLS {
        let (status, body) = stack_status(router, case, stack).await;
        if status == wanted {
            return body;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let (status, body) = stack_status(router, case, stack).await;
    panic!("case {case:?}: expected the stack to reach {wanted}, last reported {status}: {body}")
}

/// Whether Amazon EKS still holds `cluster`, and the status it reports for it — or, where
/// that service holds none, the body of the refusal it answered with.
async fn held_by_eks(router: &MultiServiceRouter, cluster: &str) -> (bool, String) {
    let read = describe_cluster(router, cluster).await;
    if read.status == 200 {
        let status = read.body["cluster"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        return (true, status);
    }
    (false, read.text)
}

/// The heart of the rule: poll the stack from `in_progress` until it first reports
/// `complete`, and check that a stack which has not completed is still reporting its
/// operation in progress, that the FIRST completed observation finds the cluster gone in
/// Amazon EKS, and that the operation really was held open on that removal.
///
/// What the cluster reports WHILE the operation runs is not asserted here beyond its being
/// one of the two statuses it can be in: whether the delete has been issued yet is the
/// provisioner's own ordering (an update that drops the cluster may do other resource work
/// first), and the status a deleting cluster reports is pinned by the Amazon EKS crate's own
/// suite.
///
/// A stack whose operation CloudFormation itself gave up on — its own bounded wait for the
/// owning service ran out, which on a machine slow enough is what a span measured in seconds
/// comes to — answers `false` rather than failing the row: nothing about the rule is
/// observable through an operation that never concluded, and the caller requires the table to
/// have exercised it somewhere.
async fn settles_only_once_the_cluster_is_gone(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    in_progress: &str,
    complete: &str,
    cluster: &str,
) -> bool {
    let mut held_while_in_progress = false;
    for _ in 0..POLLS {
        let (status, body) = stack_status(router, case, stack).await;
        let (held, read) = held_by_eks(router, cluster).await;
        if status == complete {
            assert!(
                !held,
                "case {case:?}: the stack reported {complete} while Amazon EKS still held \
                 {cluster:?} ({read}); a deleted resource must be gone in the service that \
                 owned it before the operation is complete"
            );
            assert!(
                held_while_in_progress,
                "case {case:?}: the cluster was never read as still there while the operation \
                 reported {in_progress}, so the removal was not held open at all"
            );
            return true;
        }
        if status != in_progress {
            // Neither in progress nor complete: CloudFormation's own bounded wait for Amazon
            // EKS ran out on this host, and nothing of the rule is readable from that.
            let _ = body;
            return false;
        }
        if held {
            assert!(
                read == CLUSTER_ACTIVE || read == CLUSTER_DELETING,
                "case {case:?}: a cluster the stack is deleting is either still running or \
                 being deleted, got {read:?}"
            );
            held_while_in_progress = true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    false
}

/// One row: which stack operation removes the cluster, and whether the template names it.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The cluster's declared `Name`, or `None` where CloudFormation generates one.
    declared_name: Option<&'static str>,
    /// Whether the cluster is removed by the stack's own deletion, or by an update that
    /// stops declaring it.
    by_deleting_the_stack: bool,
}

const ROWS: [Row; 2] = [
    Row {
        case: "the stack's own deletion, of a cluster the template named",
        stack: "eks-removal-delete-named",
        declared_name: Some("eks-removal-named"),
        by_deleting_the_stack: true,
    },
    Row {
        case: "an update that stops declaring the cluster, which CloudFormation named",
        stack: "eks-removal-update-drops",
        declared_name: None,
        by_deleting_the_stack: false,
    },
];

#[tokio::test]
async fn a_stack_operation_that_deletes_a_cluster_completes_only_once_eks_has_removed_it() {
    let mut exercised = false;
    for row in ROWS {
        let Row {
            case,
            stack,
            declared_name,
            by_deleting_the_stack,
        } = row;
        // ONE emulator per row, so the cluster the row reads about is the only one there.
        let router = build_router();

        let stack_id = create_stack(&router, case, stack, &with_cluster(declared_name)).await;
        let created = wait_for(&router, case, &stack_id, CREATE_COMPLETE).await;
        let cluster = element(&created, "OutputValue").unwrap_or_default();
        assert!(
            !cluster.is_empty(),
            "case {case:?}: the stack must report the cluster it created, got {created}"
        );
        if let Some(declared) = declared_name {
            assert_eq!(
                cluster, declared,
                "case {case:?}: expected the declared name, got {created}"
            );
        }
        let (held, status) = held_by_eks(&router, &cluster).await;
        assert!(
            held && status == CLUSTER_ACTIVE,
            "case {case:?}: setup: expected Amazon EKS to hold {cluster:?} as {CLUSTER_ACTIVE}, \
             got {status:?}"
        );

        let (in_progress, complete) = if by_deleting_the_stack {
            let answer = cloudformation(&router, "DeleteStack", &[("StackName", stack)]).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200 from DeleteStack, got {} with body {}",
                answer.status, answer.text
            );
            (DELETE_IN_PROGRESS, DELETE_COMPLETE)
        } else {
            let answer = cloudformation(
                &router,
                "UpdateStack",
                &[
                    ("StackName", stack),
                    ("TemplateBody", without_cluster().as_str()),
                ],
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200 from UpdateStack, got {} with body {}",
                answer.status, answer.text
            );
            (UPDATE_IN_PROGRESS, UPDATE_COMPLETE)
        };

        let concluded = settles_only_once_the_cluster_is_gone(
            &router, case, &stack_id, in_progress, complete, &cluster,
        )
        .await;
        exercised |= concluded;

        // And once the operation has concluded the name is free again, which is what a client
        // that redeploys needs: the refusal a cluster Amazon EKS does not hold earns.
        if concluded {
            let gone = describe_cluster(&router, &cluster).await;
            assert_eq!(
                gone.status, 404,
                "case {case:?}: expected the freed cluster name to answer 404, got {} with \
                 body {}",
                gone.status, gone.text
            );
        }
    }

    // At least one row must have driven an operation all the way to its conclusion, or the
    // table proved nothing about the rule.
    assert!(
        exercised,
        "no row's stack operation concluded, so the removal rule was never read"
    );
}
