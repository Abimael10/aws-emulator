// SPDX-License-Identifier: CC0-1.0

//! What an Amazon ECR-access diagnosis of a Fargate service reads out of the repository
//! behind its image: a `repositoryUri` that ADDRESSES that repository in the account and the
//! Region that hold it.
//!
//! RULE: `Repository$repositoryUri` ("The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations") is an ADDRESS a container client DIALS, so
//! its authority is one this emulator answers registry requests on — never a name under a
//! zone AWS publishes its own endpoints in, which resolves nowhere near this process and
//! would send a push or a pull out of the emulated environment with the credential the client
//! holds — followed by `/` and the repository's own name, which is exactly `repositoryName`.
//! WHICH registry holds the repository is reported beside the URI on the very response that
//! carries it, as labels built from the stored record: `registryId` is "the Amazon Web
//! Services account ID associated with the registry that contains the repository" and
//! `repositoryArn` names the Region. That is what makes the read self-describing: whoever
//! reads it — here a diagnosis walking from a service to the repository behind its image —
//! recovers the owning account, the Region and the repository name, and composes a `docker`
//! reference on an address it can actually reach.
//!
//! The owning service states that rule once, with the reasoning behind it, in
//! `service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/state/registry_address.rs`,
//! and the sibling suites beside this one read the same one
//! (`cloudformation_registry_backed_task_definition.rs`'s `announced_repository_path`, which
//! `cloudformation_published_repository_uri_agrees_with_ecr.rs` reads the stack properties
//! through).
//!
//! Three services have to agree for a diagnosis to reach that conclusion, which is why this
//! suite lives beside the router: Amazon ECS holds the service and the revision whose
//! container image names the repository, IAM holds the task execution role and the policies
//! in force on it, and Amazon ECR holds the repository and reports its URI. The trajectory
//! driven below is the one an operator (or an agent following AWS's own ECR-access runbook)
//! takes — `ecs:ListServices`, `ecs:DescribeServices`, `ecs:DescribeTaskDefinition`,
//! `iam:ListAttachedRolePolicies`, `iam:ListRolePolicies`, `ecr:DescribeRepositories` — and
//! every fact is created through the owning service's own API, so what is asserted is what
//! each service persisted.
//!
//! The rows differ along what the rule generalises over: which Region's registry holds the
//! repository (the service's own Region, another Region of the same partition, a Region of
//! another partition, and a GovCloud Region), the form of the repository's name
//! (namespaced, flat, the shortest the pattern allows, and one carrying the punctuation it
//! allows), and how the revision's image addresses it (a tag, a digest, and no tag at all).
//! Every service is created wanting no task, because what is under test is the read path a
//! diagnosis takes and not a placement.
//!
//! The expectation — the zones an announced address must stay out of — is written out below
//! rather than taken from a sibling suite's helper, for the reason each of those states too:
//! it is spelled from the documented source named on it, so an address that slips into a zone
//! AWS serves fails this suite instead of agreeing with another suite's copy of the same
//! mistake. No suite here depends on another suite's expectations.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region the cluster, the services and the revisions below live in, and the one every
/// IAM and Amazon ECS request is signed for.
const HOME_REGION: &str = "us-west-2";

/// The API version an IAM awsQuery request carries.
const IAM_VERSION: &str = "2010-05-08";

/// The ECS cluster every row's service runs in, and the container every revision declares.
const CLUSTER: &str = "ecraccess";
const CONTAINER: &str = "app";

/// The network the Fargate services are placed in: the ids are named literally by the
/// requests below, so they are held in the account's Amazon EC2 store before the first call.
const VPC: &str = "vpc-00000000000000a01";
const SUBNETS: [&str; 2] = ["subnet-00000000000000a01", "subnet-00000000000000a02"];

/// The security group the placements name. Nothing here reads a rule of it — no task is
/// placed — so it is only an id the configuration carries.
const SECURITY_GROUP: &str = "sg-00000000000000a01";

/// The task execution role the revisions pull as, the managed policy AWS publishes for one
/// ("AmazonECSTaskExecutionRolePolicy", the task execution role of the Amazon ECS Developer
/// Guide) and an inline policy beside it — so both IAM reads of the diagnosis have something
/// of their own to report.
const EXECUTION_ROLE: &str = "ecraccessexecution";
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";
const INLINE_POLICY: &str = "ecraccessinline";

/// The tag the images below are pushed under.
const TAG: &str = "release";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is the URI the
/// registry reports for the repository, not which blobs an image is built from — so the layer
/// array its media type requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// The DNS suffixes AWS publishes its OWN endpoints under, spelled out here rather than
/// imported so that an address a service of this emulator announces slipping into one of them
/// fails this suite.
///
/// Source: `service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`, the tree's one
/// list of them.
const AWS_ENDPOINT_DNS_SUFFIXES: [&str; 4] =
    [".amazonaws.com", ".amazonaws.com.cn", ".api.aws", ".on.aws"];

/// How a row's revision addresses the image in the repository behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    /// `<repositoryUri>:<tag>`.
    Tag,
    /// `<repositoryUri>@<digest>`.
    Digest,
    /// `<repositoryUri>` on its own, which a container client reads as the default tag.
    Untagged,
}

/// One row: a repository in one Region's registry, the revision and the service that name it.
struct Row {
    case: &'static str,
    /// The Region whose registry holds the repository — the one its URI must name.
    region: &'static str,
    repository: &'static str,
    family: &'static str,
    service: &'static str,
    addressed: Addressed,
}

const ROWS: [Row; 4] = [
    Row {
        case: "the recorded shape: a repository in the service's own Region, addressed by tag",
        region: HOME_REGION,
        repository: "ecraccess/app",
        family: "ecraccesshome",
        service: "home",
        addressed: Addressed::Tag,
    },
    Row {
        case: "a repository in another Region of the same partition, pinned by digest",
        region: "ap-southeast-2",
        repository: "ecraccessimages",
        family: "ecraccessfar",
        service: "far",
        addressed: Addressed::Digest,
    },
    Row {
        case: "a repository in another partition, whose registries carry its own DNS suffix",
        region: "cn-north-1",
        repository: "a.b_c--d",
        family: "ecraccesspartition",
        service: "partition",
        addressed: Addressed::Tag,
    },
    Row {
        case: "a GovCloud repository named with the shortest name the pattern allows, untagged",
        region: "us-gov-east-1",
        repository: "ab",
        family: "ecraccessgov",
        service: "gov",
        addressed: Addressed::Untagged,
    },
];

/// Amazon ECS, Amazon ECR and IAM over ONE shared engine, as `router/src/lib.rs` mounts them,
/// with the network the placements name held in the account's own Amazon EC2 store — which
/// Amazon ECS resolves a subnet against.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::ec2_network::register_network(&engine, VPC, &SUBNETS, &format!("{HOME_REGION}a"));
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call signed for `region` that must succeed, answered with its parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    region: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} in {region} to answer 200, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon ECS call in the Region the cluster lives in.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    json_call(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        HOME_REGION,
        operation,
        body,
        case,
    )
    .await
}

/// One Amazon ECR call made against `region`: a repository lives in the registry of ONE
/// Region, so every call about it is made there.
async fn ecr(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    json_call(
        router,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        region,
        operation,
        body,
        case,
    )
    .await
}

/// One IAM awsQuery action that must succeed, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
    case: &str,
) -> String {
    let mut body = format!("Action={action}&Version={IAM_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{HOME_REGION}/iam\
                 /aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {text}"
    );
    text
}

/// A required string at `pointer` of a response body, or a panic naming the case.
fn string(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string at {pointer}, got {body}"))
        .to_string()
}

/// The authority of an announced repository URI, panicking with the case and the value when
/// it is not one a client of this emulator can dial: an authority followed by a path, whose
/// host is NOT under a zone AWS publishes its own endpoints in
/// ([`AWS_ENDPOINT_DNS_SUFFIXES`]) — such a name resolves nowhere near this process, or at
/// genuine AWS infrastructure, which is the one thing the announced address must never be.
fn announced_authority(case: &str, authority: &str) -> String {
    assert!(
        !authority.is_empty(),
        "case {case:?}: expected an announced registry authority, got {authority:?}"
    );
    let host = authority.to_ascii_lowercase();
    let names_aws = AWS_ENDPOINT_DNS_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix));
    assert!(
        !names_aws,
        "case {case:?}: expected an authority this emulator answers on, got the AWS-served \
         name {authority:?}"
    );
    authority.to_string()
}

/// Split a container image reference the way a reader of the revision's `image` does: the
/// registry authority ahead of the first `/`, and the repository name after it with any tag
/// or digest removed.
fn reference_parts(case: &str, reference: &str) -> (String, String) {
    let (authority, path) = reference.split_once('/').unwrap_or_else(|| {
        panic!("case {case:?}: expected an image reference with a registry host, got {reference:?}")
    });
    // A digest is appended with `@`, a tag with `:` after the last path segment; a repository
    // name carries neither character.
    let without_digest = path.split('@').next().unwrap_or(path);
    let name = match without_digest.rsplit_once(':') {
        Some((head, _)) => head,
        None => without_digest,
    };
    (authority.to_string(), name.to_string())
}

/// Create the task execution role the revisions pull as — trusted by the Amazon ECS tasks
/// principal, carrying the published managed policy and one inline policy — and answer its
/// ARN.
async fn seed_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", trust.as_str()),
        ],
        case,
    )
    .await;
    iam(
        router,
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyArn", EXECUTION_ROLE_POLICY),
        ],
        case,
    )
    .await;
    let inline = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["ecr:GetAuthorizationToken", "ecr:BatchGetImage", "ecr:DescribeRepositories"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", inline.as_str()),
        ],
        case,
    )
    .await;
    element(&created, "Arn")
}

/// One row, deployed: the URI Amazon ECR announced for its repository at create time, and the
/// reference the revision was given, composed on that very URI the way a client composes one.
struct Deployed {
    announced_uri: String,
    image: String,
}

/// Create the row's repository in its own Region, push one image into it, and answer what the
/// registry announced beside the reference the revision will carry.
async fn seed_repository(router: &MultiServiceRouter, row: &Row) -> Deployed {
    let created = ecr(
        router,
        row.region,
        "CreateRepository",
        json!({ "repositoryName": row.repository }),
        row.case,
    )
    .await;
    let announced_uri = string(row.case, &created, "/repository/repositoryUri");
    let pushed = ecr(
        router,
        row.region,
        "PutImage",
        json!({
            "repositoryName": row.repository,
            "imageManifest": MANIFEST,
            "imageTag": TAG,
        }),
        row.case,
    )
    .await;
    let digest = string(row.case, &pushed, "/image/imageId/imageDigest");
    let image = match row.addressed {
        Addressed::Tag => format!("{announced_uri}:{TAG}"),
        Addressed::Digest => format!("{announced_uri}@{digest}"),
        Addressed::Untagged => announced_uri.clone(),
    };
    Deployed {
        announced_uri,
        image,
    }
}

/// Register the row's revision and create its service: a Fargate service wanting no task,
/// because the diagnosis below is a READ of what these services hold.
async fn deploy_service(router: &MultiServiceRouter, row: &Row, image: &str, role_arn: &str) {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": image,
                "essential": true,
            }],
        }),
        row.case,
    )
    .await;
    ecs(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service,
            "taskDefinition": row.family,
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": SUBNETS,
                    "securityGroups": [SECURITY_GROUP],
                }
            },
        }),
        row.case,
    )
    .await;
}

#[tokio::test]
async fn the_ecr_access_diagnosis_reads_a_uri_that_addresses_the_repository_it_named() {
    let router = emulator();
    let setup = "the account's own records";
    let role_arn = seed_execution_role(&router, setup).await;
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        setup,
    )
    .await;

    let mut deployed = Vec::new();
    for row in &ROWS {
        let seeded = seed_repository(&router, row).await;
        deploy_service(&router, row, &seeded.image, &role_arn).await;
        deployed.push(seeded);
    }

    // Step one of the trajectory: the services the cluster holds, which is where a diagnosis
    // that was given nothing but the cluster starts.
    let listed = ecs(&router, "ListServices", json!({ "cluster": CLUSTER }), setup).await;
    let service_arns: Vec<String> = listed["serviceArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect();

    for (row, seeded) in ROWS.iter().zip(&deployed) {
        let case = row.case;
        let arn = service_arns
            .iter()
            .find(|arn| arn.ends_with(&format!("/{}", row.service)))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected ListServices to hold the service {:?}, got \
                     {service_arns:?}",
                    row.service
                )
            });

        // Step two: the service record, which names the revision its tasks run.
        let described = ecs(
            &router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [arn] }),
            case,
        )
        .await;
        let service_arn = string(case, &described, "/services/0/serviceArn");
        let revision = string(case, &described, "/services/0/taskDefinition");
        // The account the CALLER's own resources are in, read off the record Amazon ECS
        // persisted rather than written here: the registry a repository of this account is
        // held in is that same account's.
        let caller_account = service_arn
            .split(':')
            .nth(4)
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {service_arn:?}"))
            .to_string();

        // Step three: the revision, whose container image is the only reference the
        // diagnosis has to the repository behind the service.
        let revision_body = ecs(
            &router,
            "DescribeTaskDefinition",
            json!({ "taskDefinition": revision }),
            case,
        )
        .await;
        let image = string(
            case,
            &revision_body,
            "/taskDefinition/containerDefinitions/0/image",
        );
        assert_eq!(
            image, seeded.image,
            "case {case:?}: expected the revision to hold the reference it was registered \
             with, got {image:?}"
        );
        let execution_role = string(case, &revision_body, "/taskDefinition/executionRoleArn");
        let role_name = execution_role
            .rsplit('/')
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected a role ARN, got {execution_role:?}"))
            .to_string();

        // Steps four and five: what is in force on the role the pull is made as. Both reads
        // are part of the trajectory, and both must report what IAM holds for that role.
        let attached = iam(
            &router,
            "ListAttachedRolePolicies",
            &[("RoleName", role_name.as_str())],
            case,
        )
        .await;
        let attached_arns = elements(&attached, "PolicyArn");
        assert!(
            attached_arns.iter().any(|arn| arn == EXECUTION_ROLE_POLICY),
            "case {case:?}: expected the attached policy {EXECUTION_ROLE_POLICY:?} on \
             {role_name:?}, got {attached_arns:?}"
        );
        let inline = iam(
            &router,
            "ListRolePolicies",
            &[("RoleName", role_name.as_str())],
            case,
        )
        .await;
        let inline_names = elements(&inline, "member");
        assert!(
            inline_names.iter().any(|name| name == INLINE_POLICY),
            "case {case:?}: expected the inline policy {INLINE_POLICY:?} on {role_name:?}, \
             got {inline_names:?}"
        );

        // Step six: the repository behind the image, read in the Region whose registry holds
        // it — the Region the row's create was made in, and the one the repository's own ARN
        // must then name.
        let (reference_authority, reference_name) = reference_parts(case, &image);
        let reference_host = announced_authority(case, &reference_authority);
        assert_eq!(
            reference_name, row.repository,
            "case {case:?}: expected the reference's path to be the repository name, got \
             {image:?}"
        );
        let answered = ecr(
            &router,
            row.region,
            "DescribeRepositories",
            json!({ "repositoryNames": [reference_name] }),
            case,
        )
        .await;
        let reported_name = string(case, &answered, "/repositories/0/repositoryName");
        let reported_registry = string(case, &answered, "/repositories/0/registryId");
        let reported_uri = string(case, &answered, "/repositories/0/repositoryUri");
        let reported_arn = string(case, &answered, "/repositories/0/repositoryArn");

        // What the read has to establish: the URI ADDRESSES this repository — an authority a
        // client of this emulator can dial, carrying the repository's own name as its path —
        // and the labels reported beside it name the account and the Region that hold it.
        assert_eq!(
            reported_name, row.repository,
            "case {case:?}: expected the read to report the repository it was asked for, got \
             {answered}"
        );
        let (authority, name) = reference_parts(case, &reported_uri);
        let host = announced_authority(case, &authority);
        assert_eq!(
            name, row.repository,
            "case {case:?}: expected the URI's path to be the repository name, got \
             {reported_uri:?}"
        );
        assert_eq!(
            reported_registry, caller_account,
            "case {case:?}: expected the repository's registryId to be the caller's account \
             {caller_account:?}, got {reported_registry:?}"
        );
        // The ARN is the label that says WHICH registry holds it: the account whose registry
        // it is, the Region that registry serves, and the repository's own name.
        let arn_fields: Vec<&str> = reported_arn.split(':').collect();
        assert_eq!(
            arn_fields.get(3).copied(),
            Some(row.region),
            "case {case:?}: expected the ARN to name the Region holding the repository, got \
             {reported_arn:?}"
        );
        assert_eq!(
            arn_fields.get(4).copied(),
            Some(caller_account.as_str()),
            "case {case:?}: expected the ARN to name the caller's account \
             {caller_account:?}, got {reported_arn:?}"
        );
        assert!(
            reported_arn.ends_with(&format!("repository/{}", row.repository)),
            "case {case:?}: expected the ARN to name the repository, got {reported_arn:?}"
        );

        // And it is the same URI whichever path reported it: the one the create announced,
        // the one the revision's reference was composed on, and the one the read answers.
        assert_eq!(
            reported_uri, seeded.announced_uri,
            "case {case:?}: expected the read to report the URI the create announced \
             ({:?}), got {reported_uri:?}",
            seeded.announced_uri
        );
        assert!(
            image.starts_with(&reported_uri),
            "case {case:?}: expected the revision's reference {image:?} to be composed on the \
             reported URI {reported_uri:?}"
        );
        assert_eq!(
            reference_host, host,
            "case {case:?}: expected the reference and the reported URI to be addressed to one \
             registry, got {image:?} and {reported_uri:?}"
        );
    }
}
