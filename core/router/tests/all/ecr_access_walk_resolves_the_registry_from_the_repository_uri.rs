// SPDX-License-Identifier: CC0-1.0

//! What an Amazon ECR-access walk can RESOLVE out of the URI reported for the repository
//! behind a Fargate service's image: the registry that holds it, read off the URI's own
//! authority.
//!
//! RULE: the authority of `Repository$repositoryUri` ("The URI for the repository. You can use
//! this URI for container image `push` and `pull` operations") is the holding registry's own
//! DNS name — the owning account's 12-digit id as its first label, then the two fixed labels
//! the registry URL format states (`AuthorizationData$proxyEndpoint`: "The Amazon ECR registry
//! URL format is `https://aws_account_id.dkr.ecr.region.amazonaws.com`"), then the Region that
//! registry serves, then the zone the registry is served in — followed by `/` and the
//! repository's own name. So a reader that splits the value on the first `/` RECOVERS the
//! account id and the Region from the authority alone, with nothing but the URI in hand, and
//! confirms the repository it names is the one of that account in that Region.
//!
//! That is what makes the value actionable to a consumer: the account id and the Region are
//! what an `ecr:` resource ARN, a `docker login` target and a cross-account check are composed
//! from, and a bare address of this process — which carries neither label — leaves every one
//! of them unresolvable even when the path after the `/` is right. The zone behind the Region
//! is this emulator's own rather than one AWS publishes its endpoints in: a name there resolves
//! at genuine Amazon ECR rather than at this process, so a client handed one would leave the
//! emulated environment carrying the credential its `docker login` was issued.
//!
//! Three services have to agree before a walk reaches that conclusion, which is why this suite
//! lives beside the router: Amazon ECS holds the cluster, the service and the revision whose
//! container image is the only reference the walk starts with, IAM holds the roles the
//! revision names and whatever is in force on them, and Amazon ECR holds the repository and
//! reports its URI. The trajectory driven below is the frozen one an operator (or an agent
//! following AWS's ECR-access runbook) takes — `ecs:ListServices`, `ecs:DescribeServices`,
//! `ecs:DescribeTaskDefinition`, `iam:ListAttachedRolePolicies`, `iam:ListRolePolicies`,
//! `ecr:DescribeRepositories` — and every fact is created through the owning service's own API,
//! so what is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: the Region (and so the partition)
//! whose registry holds the repository, the form of the repository's name — the shortest
//! `RepositoryName`'s pattern allows, a flat one, a namespaced one, a twice-namespaced one and
//! one carrying the punctuation the pattern allows — and how the revision's image addresses it
//! (by tag, by digest, and with no tag at all). The estate the walk reads is the one a diagnosis
//! is pointed at when it finds NOTHING in force: the revision's task role carries no attached
//! and no inline policy, so both IAM reads answer empty and the registry the URI names is
//! recovered all the same — what the URI resolves to is a property of the repository's own
//! record, not of the roles that reference it. A second test reads the same registry through
//! the whole-registry form of the read, where the walk is handed every repository of the
//! registry at once rather than the one it asked for by name.
//!
//! Nothing here composes the expected authority from a constant: every row reads the labels off
//! the value and compares them against the identity the SAME response reports (`registryId` and
//! the Region `repositoryArn` names) and against the account the Amazon ECS record it walked
//! from is in. The fixed labels and the zones an announced name must stay out of are spelled
//! out below from the documented sources named on them, so a value that agrees with another
//! suite's copy of a mistake still fails here.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region the cluster, the services and the revisions live in, and the one every Amazon ECS
/// and IAM request below is signed for. The repositories live in the Regions the rows name, so
/// a URI that took its Region from the caller rather than from the record fails every row but
/// the first.
const HOME_REGION: &str = "us-east-2";

/// The API version an IAM awsQuery request carries.
const IAM_VERSION: &str = "2010-05-08";

/// The cluster every row's service runs in, and the container name every revision declares.
const CLUSTER: &str = "ecrwalk";
const CONTAINER: &str = "app";

/// The network the Fargate services are placed in. The ids are named literally by the requests
/// below, so they are held in the account's Amazon EC2 store before the first call.
const VPC: &str = "vpc-00000000000000b01";
const SUBNETS: [&str; 2] = ["subnet-00000000000000b01", "subnet-00000000000000b02"];

/// The security group the placements name. No task is placed — every service is created wanting
/// none — so nothing here reads a rule of it.
const SECURITY_GROUP: &str = "sg-00000000000000b01";

/// The roles the revisions declare: a task execution role, which a Fargate revision pulling an
/// Amazon ECR image has to name, and the task role the containers themselves run as. NEITHER
/// carries an attached or an inline policy, which is the estate an access diagnosis is pointed
/// at when the pull it is explaining was refused.
const EXECUTION_ROLE: &str = "ecrwalkexecution";
const TASK_ROLE: &str = "ecrwalktask";

/// The tag the images below are pushed under.
const TAG: &str = "release";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is the URI the
/// registry reports for the repository, not which blobs an image is built from, so the layer
/// array its media type requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// The two fixed labels every Amazon ECR registry authority carries between the owning
/// account's id and the Region it serves, as the registry URL format the model documents
/// spells them — "The Amazon ECR registry URL format is
/// `https://aws_account_id.dkr.ecr.region.amazonaws.com`":
///
/// ```text
/// smithy_query(query="doc",shape="AuthorizationData",member="proxyEndpoint",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
///
/// Spelled out here rather than imported so a value that spells them, or orders them, any other
/// way fails these rows.
const FIXED_LABELS: [&str; 2] = ["dkr", "ecr"];

/// How many digits an account id — and so the first label of a registry authority — carries, as
/// `Repository$registryId`'s own pattern (`^[0-9]{12}$`) bounds it:
///
/// ```text
/// smithy_query(query="shape",shape="RegistryId",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const ACCOUNT_ID_DIGITS: usize = 12;

/// The DNS suffixes AWS publishes its OWN endpoints under, spelled out here rather than
/// imported so an address a service of this emulator announces slipping into one of them fails
/// this suite.
///
/// Source: `service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`, the tree's one
/// list of them.
const AWS_ENDPOINT_DNS_SUFFIXES: [&str; 4] =
    [".amazonaws.com", ".amazonaws.com.cn", ".api.aws", ".on.aws"];

/// How a row's revision addresses the image in the repository behind it — the three forms a
/// container reference composed on a reported URI takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    /// `<repositoryUri>:<tag>`.
    Tag,
    /// `<repositoryUri>@<digest>`.
    Digest,
    /// `<repositoryUri>` on its own, which a container client reads as the default tag.
    Untagged,
}

/// One row: a repository in one Region's registry, and the revision and service that name it.
struct Row {
    case: &'static str,
    /// The Region whose registry holds the repository — the Region the authority of its URI
    /// has to name, and the one every call about it is made in.
    region: &'static str,
    repository: &'static str,
    family: &'static str,
    service: &'static str,
    addressed: Addressed,
}

const ROWS: [Row; 5] = [
    Row {
        case: "a repository in the service's own Region, addressed by tag",
        region: HOME_REGION,
        repository: "ecrwalk/app",
        family: "ecrwalkhome",
        service: "home",
        addressed: Addressed::Tag,
    },
    Row {
        case: "a repository in another Region of the same partition, pinned by digest",
        region: "eu-west-1",
        repository: "ecrwalkimages",
        family: "ecrwalkfar",
        service: "far",
        addressed: Addressed::Digest,
    },
    Row {
        case: "a twice-namespaced repository in another Region, addressed by tag",
        region: "ap-northeast-1",
        repository: "org/team/app",
        family: "ecrwalknested",
        service: "nested",
        addressed: Addressed::Tag,
    },
    Row {
        case: "a repository of another partition, named with the punctuation the pattern allows",
        region: "cn-northwest-1",
        repository: "a.b_c--d",
        family: "ecrwalkpartition",
        service: "partition",
        addressed: Addressed::Untagged,
    },
    Row {
        case: "a GovCloud repository named with the shortest name the pattern allows, by digest",
        region: "us-gov-west-1",
        repository: "ab",
        family: "ecrwalkgov",
        service: "gov",
        addressed: Addressed::Digest,
    },
];

/// Amazon ECS, Amazon ECR and IAM over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them, with the network the placements name held in the account's own Amazon EC2 store —
/// which Amazon ECS resolves a subnet against.
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

/// One Amazon ECR call made in `region`: a repository lives in the registry of ONE Region, so
/// every call about it is made there.
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

/// What a reader RECOVERS from a reported repository URI: the registry's account and Region,
/// read off the authority's own labels, and the repository name its path carries.
#[derive(Debug, PartialEq, Eq)]
struct Resolved {
    account: String,
    region: String,
    repository: String,
    /// The whole authority ahead of the first `/`, so two values can be compared as addresses.
    authority: String,
}

/// Resolve `uri` the way a consumer of the member does: split it on the FIRST `/`, read the
/// registry out of the authority's labels, and take the path as the repository name.
///
/// Every structural requirement of the value is asserted here — the path, the absence of a
/// port, the account label's shape, the two fixed labels, a Region label and a non-empty zone
/// behind it that is not one AWS publishes its own endpoints in — so a caller is handed only
/// what a reader can actually derive from the value.
fn resolved(case: &str, uri: &str) -> Resolved {
    let (authority, path) = uri
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected <authority>/<name>, got {uri:?}"));
    assert!(
        !path.is_empty(),
        "case {case:?}: expected a repository name behind the first '/' of {uri:?}"
    );
    // The value AWS publishes carries no port: a container client resolves the authority and
    // dials the registry on the port its scheme implies.
    assert!(
        !authority.contains(':'),
        "case {case:?}: expected no port in the authority of {uri:?}, got {authority:?}"
    );

    let labels: Vec<&str> = authority.split('.').collect();
    let [dkr, ecr] = FIXED_LABELS;
    assert!(
        labels.len() >= 5,
        "case {case:?}: expected <account>.{dkr}.{ecr}.<region>.<zone> in {authority:?}, got {} \
         labels",
        labels.len()
    );
    let account = labels[0];
    assert!(
        account.len() == ACCOUNT_ID_DIGITS && account.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected a {ACCOUNT_ID_DIGITS}-digit account id as the first label of \
         {authority:?}, got {account:?}"
    );
    assert_eq!(
        (labels[1], labels[2]),
        (dkr, ecr),
        "case {case:?}: expected the fixed labels {dkr:?} and {ecr:?} behind the account in \
         {authority:?}, got {:?} and {:?}",
        labels[1],
        labels[2]
    );
    let region = labels[3];
    assert!(
        !region.is_empty(),
        "case {case:?}: expected a Region label behind {dkr}.{ecr} in {authority:?}"
    );

    let zone = labels[4..].join(".");
    assert!(
        labels[4..].iter().all(|label| !label.is_empty()),
        "case {case:?}: expected a served zone behind the Region in {authority:?}, got {zone:?}"
    );
    let lowered = zone.to_ascii_lowercase();
    assert!(
        !AWS_ENDPOINT_DNS_SUFFIXES
            .iter()
            .any(|suffix| lowered == suffix.trim_start_matches('.') || lowered.ends_with(suffix)),
        "case {case:?}: expected a zone this emulator answers in behind {region:?}, got \
         {zone:?}, which AWS publishes its own endpoints in"
    );

    Resolved {
        account: account.to_string(),
        region: region.to_string(),
        repository: path.to_string(),
        authority: authority.to_string(),
    }
}

/// The repository name a container reference addresses: the path after the first `/` with any
/// tag or digest removed, the way a reader of `ContainerDefinition$image` takes it.
fn referenced_repository(case: &str, reference: &str) -> String {
    let (_, path) = reference.split_once('/').unwrap_or_else(|| {
        panic!("case {case:?}: expected a registry host in the reference {reference:?}")
    });
    let without_digest = path.split('@').next().unwrap_or(path);
    match without_digest.rsplit_once(':') {
        Some((head, _)) => head.to_string(),
        None => without_digest.to_string(),
    }
}

/// Create one role trusted by the Amazon ECS tasks principal, carrying NO policy of any kind,
/// and answer its ARN.
async fn seed_role(router: &MultiServiceRouter, name: &str, case: &str) -> String {
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
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
        case,
    )
    .await;
    element(&created, "Arn")
}

/// One row's repository, created in its own Region with one image pushed into it: the URI the
/// registry announced at create time, and the container reference the revision will carry,
/// composed on that very URI the way a client composes one.
struct Deployed {
    announced_uri: String,
    image: String,
}

/// Create the row's repository and push one image into it.
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
/// because what follows is a READ of what these three services hold.
async fn deploy_service(
    router: &MultiServiceRouter,
    row: &Row,
    image: &str,
    execution_role: &str,
    task_role: &str,
) {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role,
            "taskRoleArn": task_role,
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

/// The roles, the cluster and every row's estate, deployed through the owning services' own
/// APIs.
async fn deployed_estate(router: &MultiServiceRouter) -> Vec<Deployed> {
    let setup = "the account's own records";
    let execution_role = seed_role(router, EXECUTION_ROLE, setup).await;
    let task_role = seed_role(router, TASK_ROLE, setup).await;
    ecs(
        router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        setup,
    )
    .await;

    let mut deployed = Vec::new();
    for row in &ROWS {
        let seeded = seed_repository(router, row).await;
        deploy_service(router, row, &seeded.image, &execution_role, &task_role).await;
        deployed.push(seeded);
    }
    deployed
}

/// RULE: the walk recovers the holding registry — the owning account and its Region — out of
/// the authority of the URI Amazon ECR reports for the repository, and that authority addresses
/// the repository the path names.
#[tokio::test]
async fn the_ecr_access_walk_recovers_the_account_and_region_from_the_reported_uri() {
    let router = emulator();
    let deployed = deployed_estate(&router).await;
    let setup = "the account's own records";

    // Step one of the walk: the services the cluster holds, which is where a diagnosis given
    // nothing but the cluster starts.
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

        // Step two: the service record, which names the revision its tasks run. The account
        // the walk is inspecting is read off that record rather than written here.
        let described = ecs(
            &router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [arn] }),
            case,
        )
        .await;
        let service_arn = string(case, &described, "/services/0/serviceArn");
        let revision = string(case, &described, "/services/0/taskDefinition");
        let caller_account = service_arn
            .split(':')
            .nth(4)
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {service_arn:?}"))
            .to_string();

        // Step three: the revision, whose container image is the only reference the walk has
        // to the repository behind the service.
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
            "case {case:?}: expected the revision to hold the reference it was registered with, \
             got {image:?}"
        );
        let task_role = string(case, &revision_body, "/taskDefinition/taskRoleArn");
        let role_name = task_role
            .rsplit('/')
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected a role ARN, got {task_role:?}"))
            .to_string();

        // Steps four and five: what is in force on the role the containers run as. This estate
        // carries nothing on it, which is what a diagnosis of a refused pull finds, and both
        // reads have to report that rather than refuse.
        let attached = iam(
            &router,
            "ListAttachedRolePolicies",
            &[("RoleName", role_name.as_str())],
            case,
        )
        .await;
        let attached_arns = elements(&attached, "PolicyArn");
        assert!(
            attached_arns.is_empty(),
            "case {case:?}: expected no attached policy on {role_name:?}, got {attached_arns:?}"
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
            inline_names.is_empty(),
            "case {case:?}: expected no inline policy on {role_name:?}, got {inline_names:?}"
        );

        // Step six: the repository behind the image, read in the Region whose registry holds
        // it, named by the path the walk split out of the reference.
        let referenced = referenced_repository(case, &image);
        assert_eq!(
            referenced, row.repository,
            "case {case:?}: expected the reference {image:?} to name the repository \
             {:?}, got {referenced:?}",
            row.repository
        );
        let answered = ecr(
            &router,
            row.region,
            "DescribeRepositories",
            json!({ "repositoryNames": [referenced] }),
            case,
        )
        .await;
        let reported_uri = string(case, &answered, "/repositories/0/repositoryUri");
        let reported_registry = string(case, &answered, "/repositories/0/registryId");
        let reported_arn = string(case, &answered, "/repositories/0/repositoryArn");
        let arn_region = reported_arn
            .split(':')
            .nth(3)
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {reported_arn:?}"))
            .to_string();

        // What the walk concludes, read out of the URI ALONE: the account, the Region and the
        // repository it addresses.
        let read = resolved(case, &reported_uri);
        assert_eq!(
            read.repository, row.repository,
            "case {case:?}: expected {reported_uri:?} to address the repository {:?}, got {:?}",
            row.repository, read.repository
        );
        assert_eq!(
            read.account, reported_registry,
            "case {case:?}: expected {reported_uri:?} to resolve to the registry this response \
             reports, {reported_registry:?}, got {:?}",
            read.account
        );
        assert_eq!(
            read.account, caller_account,
            "case {case:?}: expected {reported_uri:?} to resolve to the account the inspected \
             service is in, {caller_account:?}, got {:?}",
            read.account
        );
        assert_eq!(
            read.region, row.region,
            "case {case:?}: expected {reported_uri:?} to resolve to the Region holding the \
             repository, {:?}, got {:?}",
            row.region, read.region
        );
        assert_eq!(
            read.region, arn_region,
            "case {case:?}: expected {reported_uri:?} to resolve to the Region its own ARN \
             names, {arn_region:?}, got {:?}",
            read.region
        );
        // The same conclusion stated the way a consumer composing an `ecr:` ARN or a `docker
        // login` target reaches it: the authority BEGINS with the account and the Region.
        let [dkr, ecr] = FIXED_LABELS;
        let addresses = format!("{}.{dkr}.{ecr}.{}.", read.account, read.region);
        assert!(
            reported_uri.starts_with(&addresses),
            "case {case:?}: expected {reported_uri:?} to begin {addresses:?}"
        );

        // And it is one registry whichever path reported it: the URI the create announced, the
        // authority the revision's reference was composed on, and the one the read answers.
        assert_eq!(
            reported_uri, seeded.announced_uri,
            "case {case:?}: expected the read to report the URI the create announced ({:?}), \
             got {reported_uri:?}",
            seeded.announced_uri
        );
        let reference_registry = resolved(case, &image);
        assert_eq!(
            (
                reference_registry.authority.as_str(),
                reference_registry.account.as_str(),
                reference_registry.region.as_str()
            ),
            (
                read.authority.as_str(),
                read.account.as_str(),
                read.region.as_str()
            ),
            "case {case:?}: expected the reference {image:?} and the reported URI \
             {reported_uri:?} to resolve to one registry"
        );
    }
}

/// RULE: the same registry is recovered from every entry of the WHOLE-registry read — the form
/// of `DescribeRepositories` a walk uses when it was given no name to ask for — so the account
/// and the Region a consumer derives do not depend on how the repository was looked up.
#[tokio::test]
async fn every_entry_of_the_whole_registry_read_resolves_to_the_registry_it_was_read_from() {
    let router = emulator();
    let deployed = deployed_estate(&router).await;

    // One Region at a time: a registry is one account's in ONE Region, so the whole-registry
    // read of a Region reports exactly the repositories created there.
    for (row, seeded) in ROWS.iter().zip(&deployed) {
        let case = row.case;
        let answered = ecr(&router, row.region, "DescribeRepositories", json!({}), case).await;
        let repositories = answered["repositories"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a repository list, got {answered}"));
        assert!(
            !repositories.is_empty(),
            "case {case:?}: expected the registry of {} to hold the repository it was given, \
             got {answered}",
            row.region
        );

        let mut addressed = Vec::new();
        for (index, repository) in repositories.iter().enumerate() {
            let entry = format!("{case}, entry {index}");
            let name = string(&entry, repository, "/repositoryName");
            let uri = string(&entry, repository, "/repositoryUri");
            let registry = string(&entry, repository, "/registryId");
            let read = resolved(&entry, &uri);
            assert_eq!(
                read.repository, name,
                "case {entry:?}: expected {uri:?} to address the repository this entry reports, \
                 {name:?}, got {:?}",
                read.repository
            );
            assert_eq!(
                (read.account.as_str(), read.region.as_str()),
                (registry.as_str(), row.region),
                "case {entry:?}: expected {uri:?} to resolve to the registry of {registry:?} in \
                 {:?}, got {:?} in {:?}",
                row.region,
                read.account,
                read.region
            );
            addressed.push((name, uri));
        }

        let held = addressed
            .iter()
            .find(|(name, _)| name == row.repository)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the whole-registry read of {} to hold {:?}, got \
                     {addressed:?}",
                    row.region, row.repository
                )
            });
        assert_eq!(
            held.1, seeded.announced_uri,
            "case {case:?}: expected the whole-registry read to report the URI the create \
             announced ({:?}), got {:?}",
            seeded.announced_uri, held.1
        );
    }
}
