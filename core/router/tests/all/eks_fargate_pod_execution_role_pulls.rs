// SPDX-License-Identifier: CC0-1.0

//! A pod AWS Fargate runs has its container image PULLED as the Pod execution role
//! of the Fargate profile that selected it.
//!
//! The RULE under test: **a container runs an image, and that image is fetched from
//! its registry before the container is created — by the kubelet of the capacity the
//! pod was placed on, AS THE IAM ROLE that capacity runs under. For a pod a Fargate
//! profile selected that role is the profile's Pod execution role, which "allows
//! Fargate infrastructure to register with your cluster as a node, and it provides
//! read access to Amazon ECR image repositories"
//! (`CreateFargateProfileRequest$podExecutionRoleArn`) — so the events the cluster
//! records for the pod report the pull Amazon ECR served for that role (`Pulling`,
//! then `Pulled`, with the digest the registry resolved reported as the container's
//! `imageID`) or the one it refused (`Pulling`, then the `Failed` naming what the
//! registry answered, `Error: ErrImagePull` and the back-off), with the Fargate node
//! the pod runs on named as the host that recorded them.**
//!
//! It is the same rule a node group's node is held to
//! (`core/router/tests/all/eks_pod_image_pull_events.rs`), asked of the OTHER
//! capacity a cluster has and the other role that capacity acts as — so the rows here
//! differ along who the pull is made as (a Pod execution role granted the calls a pull
//! makes, and one no policy grants them to) and, over that, what the registry holds
//! for the reference (the image, and a tag it does not hold).
//!
//! Four crates have to agree for it to hold — Amazon EKS serves the Kubernetes API and
//! places the pod on the capacity its profile provides, Amazon ECR holds the repository
//! and decides the pull, IAM holds the trust policy and the documents in force on the
//! Pod execution role, and Amazon EC2 owns the subnets the cluster is created in — so
//! the suite lives here rather than in any one of them.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::eks_wire::{cluster_body, ec2, eks, iam, role_arn, Answer, REGION};
use crate::query_wire::elements;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// How often a clock-owed read is looked at again, and how many times in all:
/// nothing inside the emulator sleeps for a due instant, so a condition is polled
/// rather than waited for, with a deadline far above every span this emulator owes.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`]: 600 looks at 50ms is a 30-second deadline.
const POLLS: usize = 600;

/// The AWS-managed policy AWS documents for the pull half of an Amazon ECR registry,
/// which is what AWS's own Fargate Pod execution role carries.
const ECR_READ_POLICY: &str = "arn:aws:iam::aws:policy/AmazonEKSFargatePodExecutionRolePolicy";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is who
/// may pull an image and whether the registry holds one, not which blobs an image is
/// built from.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#
);

/// The repository the rows pull from, and the tag it holds.
const REPOSITORY: &str = "fargate-app";
const TAG: &str = "1";

/// The namespace the profiles of this suite select, and the profile's own name.
const NAMESPACE: &str = "default";
const PROFILE: &str = "pod-pullers";

/// One answer from a cluster's published Kubernetes API address.
#[derive(Debug)]
struct ApiAnswer {
    status: u16,
    body: Value,
}

/// One request to that address, the way a kubeconfig-built client makes it: one
/// HTTP/1.1 exchange on the port the cluster published, presenting the ACCOUNT's own
/// credential — the principal that created the cluster.
fn k8s(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> ApiAnswer {
    let authority = endpoint
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("{what}: expected an https endpoint, got {endpoint:?}"))
        .to_string();
    let (_, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("{what}: the endpoint must carry a port, got {authority:?}"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("{what}: {port:?} must be a port: {error}"));
    let payload = body.map(Value::to_string).unwrap_or_default();
    runtime.block_on(async move {
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap_or_else(|error| panic!("{what}: the published address must accept: {error}"));
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n\
             {payload}",
            crate::eks_wire::account_kubernetes_bearer_token(),
            payload.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("{what}: the request must be written: {error}"));
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .await
            .unwrap_or_else(|error| panic!("{what}: the answer must be read: {error}"));
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, answered) = text
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("{what}: expected an HTTP answer, got {text:?}"));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("{what}: expected a status line, got {head:?}"));
        ApiAnswer {
            status,
            body: serde_json::from_str(answered).unwrap_or(Value::Null),
        }
    })
}

/// One Amazon ECR call (`awsJson1_1`, `AmazonEC2ContainerRegistry_V20150921.<Op>`).
fn ecr(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: &Value,
) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ecr.{REGION}.amazonaws.com/"))
        .header("host", format!("ecr.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ecr/aws4_request, \
                 SignedHeaders=host, Signature=deadbeef"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerRegistry_V20150921.{operation}"),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let parsed = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "setup: Amazon ECR must answer {operation}, got {status} with {text}"
    );
    Answer {
        status,
        error_type: None,
        text,
        body: parsed,
    }
}

/// Poll `read` until `holds` answers true, and answer the last read; panic naming
/// `what` when the deadline passes first.
fn read_until(
    what: &str,
    mut read: impl FnMut() -> Answer,
    holds: impl Fn(&Answer) -> bool,
) -> Answer {
    let mut last = read();
    for _ in 0..POLLS {
        if holds(&last) {
            return last;
        }
        std::thread::sleep(POLL_INTERVAL);
        last = read();
    }
    panic!("expected {what}; the last read answered {last:?}");
}

/// The account's own default subnets, read from EC2 rather than invented.
fn default_subnets(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Vec<String> {
    let defaults = ec2(
        router,
        runtime,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
    );
    assert_eq!(
        defaults.status, 200,
        "setup: DescribeSubnets must succeed, got {defaults:?}"
    );
    let available = elements(&defaults.text, "subnetId");
    assert!(
        available.len() >= 2,
        "setup: the account must have default subnets, got {available:?}"
    );
    vec![available[0].clone(), available[1].clone()]
}

/// Create the IAM role `name` with the trust policy a Fargate Pod execution role
/// carries — the Fargate Pod plane's own service principal, which is the principal
/// AWS publishes for it — carrying the AWS-managed policies `policies`, and answer its
/// ARN.
fn pod_execution_role(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    policies: &[&str],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "eks-fargate-pods.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }]
    })
    .to_string();
    let created = iam(
        router,
        runtime,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    );
    assert_eq!(
        created.status, 200,
        "setup: the Pod execution role {name:?} must be created, got {created:?}"
    );
    for policy in policies {
        let attached = iam(
            router,
            runtime,
            "AttachRolePolicy",
            &[("RoleName", name), ("PolicyArn", policy)],
        );
        assert_eq!(
            attached.status, 200,
            "setup: {policy} must attach to {name:?}, got {attached:?}"
        );
    }
    role_arn(name)
}

/// Create the cluster `name` whose ONLY capacity is a Fargate profile selecting
/// [`NAMESPACE`] and running under `role`, wait for both to be reported ready, and
/// answer the address the cluster published.
fn cluster_with_fargate_capacity(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    role: &str,
    subnets: &[String],
) -> String {
    let created = eks(
        router,
        runtime,
        "POST",
        "/clusters",
        Some(cluster_body(name, subnets)),
    );
    assert_eq!(
        created.status, 200,
        "setup: the cluster {name:?} must be created, got {created:?} ({})",
        created.message()
    );
    let active = read_until(
        "the cluster to reach ACTIVE with its API server address published",
        || eks(router, runtime, "GET", &format!("/clusters/{name}"), None),
        |answer| {
            answer.body["cluster"]["status"] == json!("ACTIVE")
                && answer.body["cluster"]["endpoint"].is_string()
        },
    );
    let endpoint = active.body["cluster"]["endpoint"]
        .as_str()
        .expect("the published API server address")
        .to_string();
    // The profile is created with no subnet of its own: Amazon EKS launches a
    // profile's Pods into private subnets, and whether a subnet is private is EC2's
    // to answer — a question this suite does not put, because what is under test is
    // the role the pull is made as.
    let profile = eks(
        router,
        runtime,
        "POST",
        &format!("/clusters/{name}/fargate-profiles"),
        Some(json!({
            "fargateProfileName": PROFILE,
            "podExecutionRoleArn": role,
            "selectors": [{ "namespace": NAMESPACE }],
        })),
    );
    assert_eq!(
        profile.status, 200,
        "setup: the Fargate profile of {name:?} must be created, got {profile:?} ({})",
        profile.message()
    );
    read_until(
        "the Fargate profile to reach ACTIVE",
        || {
            eks(
                router,
                runtime,
                "GET",
                &format!("/clusters/{name}/fargate-profiles/{PROFILE}"),
                None,
            )
        },
        |answer| answer.body["fargateProfile"]["status"] == json!("ACTIVE"),
    );
    endpoint
}

/// The events of [`NAMESPACE`] about the pod `pod`, as a client asks for them: the
/// collection narrowed by the field the server indexes an event's object by.
fn pod_events(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    pod: &str,
) -> Vec<Value> {
    let answered = k8s(
        runtime,
        endpoint,
        what,
        "GET",
        &format!(
            "/api/v1/namespaces/{NAMESPACE}/events?fieldSelector=involvedObject.name%3D{pod}"
        ),
        None,
    );
    assert_eq!(
        answered.status, 200,
        "{what}: the pod's events must be served, got {answered:?}"
    );
    answered.body["items"].as_array().cloned().unwrap_or_default()
}

/// The event of `pod` carrying `reason` whose message starts with `starts_with`,
/// polled for until the clock-owed pull has been made; `None` once the deadline
/// passes.
fn event_until(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    pod: &str,
    reason: &str,
    starts_with: &str,
) -> Option<Value> {
    for _ in 0..POLLS {
        let found = pod_events(runtime, endpoint, what, pod).into_iter().find(|event| {
            event["reason"] == json!(reason)
                && event["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with(starts_with))
        });
        if found.is_some() {
            return found;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    None
}

/// One row: the pod, the image its container runs, the cluster (and so the Pod
/// execution role) it is created on, and whether the registry served the pull.
struct Row {
    case: &'static str,
    pod: &'static str,
    image: String,
    granted: bool,
    served: bool,
}

#[test]
fn a_fargate_pods_image_is_pulled_as_the_profiles_pod_execution_role() {
    std::thread::Builder::new()
        .name("eks-fargate-pod-pull".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            // THE REGISTRY'S OWN RECORDS: one repository holding one tagged image.
            let repository = ecr(
                &router,
                &runtime,
                "CreateRepository",
                &json!({ "repositoryName": REPOSITORY }),
            );
            let repository_uri = repository.body["repository"]["repositoryUri"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("setup: the repository must publish a URI, got {repository:?}")
                })
                .to_string();
            let pushed = ecr(
                &router,
                &runtime,
                "PutImage",
                &json!({
                    "repositoryName": REPOSITORY,
                    "imageManifest": MANIFEST,
                    "imageTag": TAG,
                }),
            );
            let digest = pushed.body["image"]["imageId"]["imageDigest"]
                .as_str()
                .unwrap_or_else(|| panic!("setup: the push must report a digest, got {pushed:?}"))
                .to_string();

            // Two Pod execution roles, and one Fargate-only cluster under each: a pod
            // is placed on the capacity of its own cluster's profile, so the role the
            // pull is made as is the one that profile was created with.
            let subnets = default_subnets(&router, &runtime);
            let granted_role = pod_execution_role(
                &router,
                &runtime,
                "eksFargatePullingRole",
                &[ECR_READ_POLICY],
            );
            let withheld_role =
                pod_execution_role(&router, &runtime, "eksFargateNoPullRole", &[]);
            let granted_endpoint = cluster_with_fargate_capacity(
                &router,
                &runtime,
                "fargate-pulling-cluster",
                &granted_role,
                &subnets,
            );
            let withheld_endpoint = cluster_with_fargate_capacity(
                &router,
                &runtime,
                "fargate-withheld-cluster",
                &withheld_role,
                &subnets,
            );

            let account = engine::emulator_account_id();
            let published = |path: &str| format!("{account}.dkr.ecr.{REGION}.amazonaws.com/{path}");
            let rows = [
                Row {
                    case: "the image the registry holds, pulled by a Pod execution role granted \
                           the pull",
                    pod: "fargate-pull-served",
                    image: published(&format!("{REPOSITORY}:{TAG}")),
                    granted: true,
                    served: true,
                },
                Row {
                    case: "the same image addressed by the URI the registry published",
                    pod: "fargate-pull-published-uri",
                    image: format!("{repository_uri}:{TAG}"),
                    granted: true,
                    served: true,
                },
                Row {
                    case: "a tag the repository does not hold",
                    pod: "fargate-pull-absent-tag",
                    image: published(&format!("{REPOSITORY}:absent")),
                    granted: true,
                    served: false,
                },
                Row {
                    case: "the image, pulled by a Pod execution role no policy grants the pull",
                    pod: "fargate-pull-withheld",
                    image: published(&format!("{REPOSITORY}:{TAG}")),
                    granted: false,
                    served: false,
                },
            ];

            for row in &rows {
                let Row {
                    case,
                    pod,
                    image,
                    granted,
                    served,
                } = row;
                let endpoint = if *granted {
                    &granted_endpoint
                } else {
                    &withheld_endpoint
                };
                let created = k8s(
                    &runtime,
                    endpoint,
                    case,
                    "POST",
                    &format!("/api/v1/namespaces/{NAMESPACE}/pods"),
                    Some(&json!({
                        "metadata": { "name": pod },
                        "spec": { "containers": [{ "name": "app", "image": image }] },
                    })),
                );
                assert_eq!(
                    created.status, 201,
                    "case {case:?}: the pod must be created, got {created:?}"
                );
                // THE CAPACITY: the cluster has no node group at all, so the node the
                // pod was placed on is the Fargate capacity its profile provided —
                // and that node is a node of the cluster, carrying the profile's name.
                let node = created.body["spec"]["nodeName"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the pod must be placed on the profile's capacity, \
                             got {created:?}"
                        )
                    })
                    .to_string();
                let read_node = k8s(
                    &runtime,
                    endpoint,
                    case,
                    "GET",
                    &format!("/api/v1/nodes/{node}"),
                    None,
                );
                assert_eq!(
                    read_node.status, 200,
                    "case {case:?}: the capacity the pod runs on must be a node of the cluster, \
                     got {read_node:?}"
                );
                assert_eq!(
                    read_node.body["metadata"]["labels"]["eks.amazonaws.com/fargate-profile"],
                    json!(PROFILE),
                    "case {case:?}: the node names the profile that provisioned it, got \
                     {read_node:?}"
                );

                // THE SCHEDULER'S RECORD: the pod was assigned to that capacity.
                let scheduled = event_until(
                    &runtime,
                    endpoint,
                    case,
                    pod,
                    "Scheduled",
                    &format!("Successfully assigned {NAMESPACE}/{pod} to {node}"),
                )
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the scheduler's record, got {:?}",
                        pod_events(&runtime, endpoint, case, pod)
                    )
                });
                assert_eq!(
                    scheduled["involvedObject"]["name"], json!(pod),
                    "case {case:?}: the record is about that pod, got {scheduled}"
                );

                if *served {
                    let pulled = event_until(
                        &runtime,
                        endpoint,
                        case,
                        pod,
                        "Pulled",
                        &format!("Successfully pulled image \"{image}\""),
                    )
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected the pull the registry served for the Pod \
                             execution role, got {:?}",
                            pod_events(&runtime, endpoint, case, pod)
                        )
                    });
                    assert_eq!(
                        pulled["type"], json!("Normal"),
                        "case {case:?}: a pull that was served is normal, got {pulled}"
                    );
                    assert_eq!(
                        (
                            pulled["source"]["component"].clone(),
                            pulled["source"]["host"].clone(),
                        ),
                        (json!("kubelet"), json!(node)),
                        "case {case:?}: the kubelet of the Fargate capacity made the pull, got \
                         {pulled}"
                    );
                    assert_eq!(
                        pulled["involvedObject"]["fieldPath"],
                        json!("spec.containers{app}"),
                        "case {case:?}: the event names the container it is about, got {pulled}"
                    );
                    // The pull was ANNOUNCED before it was made.
                    assert!(
                        event_until(
                            &runtime,
                            endpoint,
                            case,
                            pod,
                            "Pulling",
                            &format!("Pulling image \"{image}\"")
                        )
                        .is_some(),
                        "case {case:?}: the kubelet records the pull it began, got {:?}",
                        pod_events(&runtime, endpoint, case, pod)
                    );
                    // WHAT THE REGISTRY RESOLVED: the container reports the manifest
                    // digest that registry holds the image at.
                    let read = k8s(
                        &runtime,
                        endpoint,
                        case,
                        "GET",
                        &format!("/api/v1/namespaces/{NAMESPACE}/pods/{pod}"),
                        None,
                    );
                    let status = read.body["status"]["containerStatuses"][0].clone();
                    let image_id = status["imageID"].as_str().unwrap_or_default().to_string();
                    assert!(
                        image_id.ends_with(&format!("@{digest}")),
                        "case {case:?}: the container must report the digest the registry \
                         resolved ({digest}), got {image_id:?}"
                    );
                    continue;
                }

                let failed = event_until(
                    &runtime,
                    endpoint,
                    case,
                    pod,
                    "Failed",
                    &format!("Failed to pull image \"{image}\": "),
                )
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the pull the registry refused, got {:?}",
                        pod_events(&runtime, endpoint, case, pod)
                    )
                });
                assert_eq!(
                    failed["type"], json!("Warning"),
                    "case {case:?}: a refused pull is a warning, got {failed}"
                );
                assert_eq!(
                    failed["source"]["host"], json!(node),
                    "case {case:?}: the kubelet of the Fargate capacity made the pull, got \
                     {failed}"
                );
                assert!(
                    event_until(&runtime, endpoint, case, pod, "Failed", "Error: ErrImagePull")
                        .is_some(),
                    "case {case:?}: the container is left in the pull's own error, got {:?}",
                    pod_events(&runtime, endpoint, case, pod)
                );
                assert!(
                    event_until(
                        &runtime,
                        endpoint,
                        case,
                        pod,
                        "BackOff",
                        &format!("Back-off pulling image \"{image}\"")
                    )
                    .is_some(),
                    "case {case:?}: the kubelet waits before pulling again, got {:?}",
                    pod_events(&runtime, endpoint, case, pod)
                );
                // A CONTAINER WITH NO IMAGE IS NOT CREATED: it waits on the pull.
                let read = k8s(
                    &runtime,
                    endpoint,
                    case,
                    "GET",
                    &format!("/api/v1/namespaces/{NAMESPACE}/pods/{pod}"),
                    None,
                );
                let status = read.body["status"]["containerStatuses"][0].clone();
                assert_eq!(
                    status["state"]["waiting"]["reason"], json!("ImagePullBackOff"),
                    "case {case:?}: the container waits on the pull, got {status}"
                );
                assert_eq!(
                    status["imageID"], json!(""),
                    "case {case:?}: no digest was resolved for it, got {status}"
                );
            }

            // The pulls read the registry rather than writing it: the image is still
            // the one it held, under the digest it resolved.
            let described = ecr(
                &router,
                &runtime,
                "DescribeImages",
                &json!({ "repositoryName": REPOSITORY }),
            );
            let details = described.body["imageDetails"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                details.len(),
                1,
                "the pulls must leave the repository as it was, got {described:?}"
            );
            assert_eq!(
                details[0]["imageDigest"], json!(digest),
                "the registry still holds the image at the digest it resolved, got {described:?}"
            );
        })
        .expect("the suite thread spawns")
        .join()
        .expect("the suite thread completes");
}
