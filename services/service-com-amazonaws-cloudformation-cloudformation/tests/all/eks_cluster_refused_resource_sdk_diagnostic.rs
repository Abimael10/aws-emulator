// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::EKS::Cluster` AMAZON EKS refused reports on its own failed event, end to end:
//! the CloudFormation calls over this service's awsQuery wire and the read-back over Amazon
//! EKS's own `restJson1` wire, through the one router `harness.rs` builds.
//!
//! RULE under test: **a resource provider reports a refused call as the exception its SDK
//! raised — the owning service's own sentence followed by that SDK's diagnostic of the call,
//! naming the service as its SDK names it, the HTTP status that service answered, the id of the
//! call and how many attempts the SDK made at it — whichever call of that service was refused
//! and whatever class the refusal falls in.**
//!
//! Why it matters: a client reading a rolled-back deployment diagnoses it from the failed
//! resource's `ResourceStatusReason`. The diagnostic is what attributes the failure to the
//! service that refused it and carries the id of the call, which is how the deploy is
//! correlated with that service's own log of it; a reason that stops at the service's sentence
//! leaves a client with nothing to attribute or correlate, and rollback-analysis tooling that
//! keys on the originating service cannot see the refusal at all.
//!
//! The rows differ along what the rule generalises over: WHICH refusal of the owning service
//! the declaration earned — subnets that cannot place a control plane in two Availability Zones
//! (named as one subnet, and as two subnets of one zone), both paths to the API server endpoint
//! turned off, public access CIDRs that are not CIDR blocks — and, in the second test, a
//! refusal of ANOTHER class answered with another HTTP status, so the diagnostic is asserted
//! over more than one status and class rather than over one recorded failure. Every expected
//! sentence is the constant Amazon EKS's own crate words that refusal with, never a copy of it.

use serde_json::{json, Value};
use service_com_amazonaws_eks_awswesleyfrontend::state::{
    invalid_public_access_cidrs_message, ENDPOINT_ACCESS_BOTH_DISABLED_MESSAGE,
    SUBNETS_IN_TWO_AVAILABILITY_ZONES_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, create_stack, failed_resource_reason, quoted_and_class,
    router, settled_stack_status, stack_event_reasons, REGION,
};

/// The SDK's own name for Amazon EKS, as the exception its SDK raises states it: that model's
/// own `sdkId` (`EKS`, `smithy_query(query="service_info")` on that service's model) with only
/// the initial capitalised.
const EKS_SERVICE: &str = "Eks";

/// The status Amazon EKS answers a request it refuses with — the `httpError` of the
/// `InvalidParameterException`/`InvalidRequestException` its `CreateCluster` models — and the
/// one it answers a name it already holds with (`ResourceInUseException`).
const REFUSED_STATUS: u16 = 400;
const IN_USE_STATUS: u16 = 409;

/// The `HandlerErrorCode` classes the rows fall in: a refusal of the REQUEST, and a refusal
/// about a resource that is already there, each one of the model's own `HandlerErrorCode`
/// values.
const INVALID_REQUEST: &str = "InvalidRequest";
const RESOURCE_CONFLICT: &str = "ResourceConflict";

/// The `StackStatus` a create that could not be completed settles at once CloudFormation has
/// rolled it back, and the one a completed create reports.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The resource status a resource whose create its provider could not carry out records.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The status Amazon EKS answers for a cluster it does not hold (the modelled 404 of
/// `DescribeCluster`).
const NOT_FOUND_STATUS: u16 = 404;

/// The logical ids the templates below declare their resources under.
const VPC_ID: &str = "Net";
const SUBNET_IDS: [&str; 3] = ["SubnetA", "SubnetB", "SubnetC"];
const CLUSTER_ID: &str = "Cluster";

/// Two zones of the Region the stacks deploy in, and the pair that places two subnets in ONE
/// zone — which is the same refusal a single subnet earns, since the rule is on the ZONES.
const TWO_ZONES: [&str; 2] = ["us-east-1a", "us-east-1b"];
const ONE_ZONE_TWICE: [&str; 2] = ["us-east-1a", "us-east-1a"];

/// A value that is not a CIDR block, so it allows and denies nothing.
const NOT_A_CIDR: &str = "everywhere";

/// One Amazon EKS call over the `restJson1` wire its model declares, on the method and URI
/// that operation's own `@http` binding names, signed for `eks` — that model's own `sigv4Name`
/// — in the Region the stacks deploy in.
async fn eks(router: &MultiServiceRouter, method: &str, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://eks.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("eks.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/eks/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from_static(b""))
        .expect("a well-formed Amazon EKS request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A role ARN of the account this emulator serves, which is what a cluster's `RoleArn` is.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// A template declaring the network a cluster sits in — one VPC and one subnet per zone, each
/// created in Amazon EC2 by the stack itself, so the subnet ids the cluster names are ids that
/// service assigned — and the cluster, under `name`, with whatever `ResourcesVpcConfig`
/// members the case adds beside its `SubnetIds`.
fn cluster_template(name: &str, zones: &[&str], added: &Value) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        VPC_ID.to_string(),
        json!({
            "Type": "AWS::EC2::VPC",
            "Properties": { "CidrBlock": "10.92.0.0/16" }
        }),
    );
    for (index, zone) in zones.iter().enumerate() {
        resources.insert(
            SUBNET_IDS[index].to_string(),
            json!({
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": VPC_ID },
                    "CidrBlock": format!("10.92.{}.0/24", index + 1),
                    "AvailabilityZone": zone
                }
            }),
        );
    }
    let mut vpc_config = json!({
        "SubnetIds": Value::Array(
            (0..zones.len())
                .map(|index| json!({ "Ref": SUBNET_IDS[index] }))
                .collect(),
        )
    });
    for (property, value) in added.as_object().into_iter().flatten() {
        vpc_config[property] = value.clone();
    }
    resources.insert(
        CLUSTER_ID.to_string(),
        json!({
            "Type": "AWS::EKS::Cluster",
            "Properties": {
                "Name": name,
                "RoleArn": role_arn("eksClusterRole"),
                "ResourcesVpcConfig": vpc_config
            }
        }),
    );
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// Hold one quoted exception to the diagnostic Amazon EKS's SDK appends: that service's own
/// sentence, then the service as its SDK names it, the status it answered, the id of the call
/// and the attempts the SDK made — each of the FORM that SDK words rather than one recorded
/// value.
///
/// The whole reading is the shape every SDK that counts its attempts words, so it is held to
/// the one assertion this target has for it ([`assert_sdk_diagnostic_with_attempts`]) under
/// this service's own name and the status the case's refusal was answered with.
fn assert_eks_diagnostic(case: &str, quoted: &str, status: u16) {
    assert_sdk_diagnostic_with_attempts(case, quoted, EKS_SERVICE, status);
}

/// Hold one failed resource's reason to the whole envelope: the owning service's `sentence`,
/// that service's SDK diagnostic after it, and the `class` the refusal was filed under.
fn assert_reported_refusal(case: &str, reason: &str, sentence: &str, status: u16, class: &str) {
    let (quoted, reported) = quoted_and_class(case, reason);
    assert!(
        quoted.starts_with(sentence),
        "case {case:?}: expected the owning service's own sentence {sentence:?} ahead of the \
         diagnostic, got {quoted:?}"
    );
    assert_eq!(
        reported, class,
        "case {case:?}: expected the {class} class, got {reported:?} in {reason:?}"
    );
    assert_eks_diagnostic(case, &quoted, status);
}

/// One declaration Amazon EKS refuses: the network it asks for, the `ResourcesVpcConfig`
/// members it adds, and the sentence that service words the refusal with.
struct Row {
    case: &'static str,
    stack: &'static str,
    zones: &'static [&'static str],
    added: Value,
    sentence: String,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a cluster whose declaration names a single subnet",
            stack: "eks-diag-one-subnet",
            zones: &TWO_ZONES[..1],
            added: Value::Object(serde_json::Map::new()),
            sentence: SUBNETS_IN_TWO_AVAILABILITY_ZONES_MESSAGE.to_string(),
        },
        Row {
            case: "a cluster whose two subnets sit in one Availability Zone",
            stack: "eks-diag-one-zone",
            zones: &ONE_ZONE_TWICE,
            added: Value::Object(serde_json::Map::new()),
            sentence: SUBNETS_IN_TWO_AVAILABILITY_ZONES_MESSAGE.to_string(),
        },
        Row {
            case: "a cluster that turns both paths to its API server endpoint off",
            stack: "eks-diag-no-endpoint",
            zones: &TWO_ZONES,
            added: json!({
                "EndpointPublicAccess": false,
                "EndpointPrivateAccess": false
            }),
            sentence: ENDPOINT_ACCESS_BOTH_DISABLED_MESSAGE.to_string(),
        },
        Row {
            case: "a cluster whose public access CIDRs are not CIDR blocks",
            stack: "eks-diag-bad-cidr",
            zones: &TWO_ZONES,
            added: json!({ "PublicAccessCidrs": [NOT_A_CIDR] }),
            sentence: invalid_public_access_cidrs_message(&[NOT_A_CIDR.to_string()]),
        },
    ]
}

#[tokio::test]
async fn a_refused_cluster_reports_the_sdk_exception_of_the_call() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let cluster = format!("{}-cluster", row.stack);
        let template = cluster_template(&cluster, row.zones, &row.added);
        let (status, created) = create_stack(&router, row.stack, &template, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — the declaration is one a \
             template may state — got {status} with body {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            reached, ROLLBACK_COMPLETE,
            "case {case:?}: expected the refused create to roll back, got {reached} with \
             failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        let reason =
            failed_resource_reason(case, &router, REGION, row.stack, CLUSTER_ID, CREATE_FAILED)
                .await;
        assert_reported_refusal(case, &reason, &row.sentence, REFUSED_STATUS, INVALID_REQUEST);

        // The refusal was real: Amazon EKS holds no cluster for the declaration it refused.
        let (status, read) = eks(&router, "GET", &format!("/clusters/{cluster}")).await;
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {case:?}: expected no cluster for a refused declaration, got {status} with \
             body {read}"
        );
    }
}

#[tokio::test]
async fn a_cluster_name_the_owning_service_already_holds_reports_its_own_status_and_class() {
    let case = "a cluster whose declared name Amazon EKS already holds";
    let router = router();
    let cluster = "eks-diag-taken-cluster";
    let held = "eks-diag-taken-holder";
    let claimant = "eks-diag-taken-claimant";
    let template = cluster_template(cluster, &TWO_ZONES, &Value::Object(serde_json::Map::new()));

    // The holder: one deployment that really creates the cluster, so the name is taken in the
    // owning service rather than only in a template.
    let (status, created) = create_stack(&router, held, &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the first CreateStack, got {status} with body \
         {created}"
    );
    let reached = settled_stack_status(case, &router, REGION, held).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the first create to complete, got {reached} with failure \
         reasons {:?}",
        stack_event_reasons(&router, REGION, held).await
    );

    // And a second deployment declaring the same name, which that service refuses with a
    // status and a class of its own — reported in the same envelope, with the same diagnostic.
    let (status, created) = create_stack(&router, claimant, &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the second CreateStack, got {status} with body \
         {created}"
    );
    let reached = settled_stack_status(case, &router, REGION, claimant).await;
    assert_eq!(
        reached, ROLLBACK_COMPLETE,
        "case {case:?}: expected the second create to roll back, got {reached} with failure \
         reasons {:?}",
        stack_event_reasons(&router, REGION, claimant).await
    );
    let reason =
        failed_resource_reason(case, &router, REGION, claimant, CLUSTER_ID, CREATE_FAILED).await;
    let (quoted, class) = quoted_and_class(case, &reason);
    assert!(
        quoted.contains(cluster),
        "case {case:?}: expected the owning service's sentence naming the cluster that holds \
         the name, got {quoted:?}"
    );
    assert_eq!(
        class, RESOURCE_CONFLICT,
        "case {case:?}: expected the {RESOURCE_CONFLICT} class, got {class:?} in {reason:?}"
    );
    assert_eks_diagnostic(case, &quoted, IN_USE_STATUS);

    // And the cluster the first stack created is still the one that service serves.
    let (status, read) = eks(&router, "GET", &format!("/clusters/{cluster}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the holder's cluster to survive the refused create, got \
         {status} with body {read}"
    );
}
