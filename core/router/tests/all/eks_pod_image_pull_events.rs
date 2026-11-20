// SPDX-License-Identifier: CC0-1.0

//! A pod's container image is PULLED by the kubelet of the node the cluster
//! scheduled it onto, and the cluster records what the registry answered.
//!
//! The RULE under test: **a container runs an image, and that image is fetched from
//! its registry before the container is created — on Amazon EKS as the NODE'S IAM
//! role ("your nodes need permissions to pull images from Amazon ECR", the node role
//! of the node group the pod was scheduled onto) — so the events the cluster records
//! for the pod report the pull Amazon ECR served (`Pulling`, then `Pulled`, with the
//! manifest digest the registry resolved reported as the container's `imageID`) or
//! the one it refused (`Pulling`, then the `Failed` naming what the registry
//! answered, `Error: ErrImagePull` and the back-off), and a container whose image was
//! refused waits on it rather than being created.** Whose registry a reference names
//! is Amazon ECR's own reading of it: a reference into a registry this emulator does
//! not hold is nobody's here to answer, so no pull is made and no pull event is
//! recorded for it.
//!
//! Four crates have to agree for that to hold — Amazon EKS serves the Kubernetes API
//! and schedules the pod, Amazon EC2 Auto Scaling and Amazon EC2 own the instance the
//! node is, Amazon ECR holds the repository and decides the pull, and IAM holds the
//! documents in force on the node role — so the suite lives here rather than in any
//! one of them.
//!
//! The rows differ along what the rule generalises over: WHICH registry the reference
//! names (this account's private Amazon ECR registry in AWS's published spelling, the
//! same registry under the URI that service published for the repository, and a
//! registry this emulator does not hold), WHAT that registry holds for it (the image,
//! a tag it does not hold, a repository it does not hold), and WHO the pull is made
//! as (a node role granted the calls a pull makes, and one no policy grants them to).

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
/// which is what a node role carries to pull a private image.
const ECR_READ_POLICY: &str = "arn:aws:iam::aws:policy/AmazonEC2ContainerRegistryReadOnly";

/// The AWS-managed policy AWS documents for the node role itself, which carries no
/// Amazon ECR grant at all.
const WORKER_NODE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonEKSWorkerNodePolicy";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is who
/// may pull an image and whether the registry holds one, not which blobs an image is
/// built from.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#
);

/// The repository the rows pull from, and the tag it holds.
const REPOSITORY: &str = "pulled-app";
const TAG: &str = "1";

/// One answer from a cluster's published Kubernetes API address.
#[derive(Debug)]
struct ApiAnswer {
    status: u16,
    body: Value,
}

/// One request to that address, the way a kubeconfig-built client makes it: one
/// HTTP/1.1 exchange on the port the cluster published, carrying no credential —
/// which is the account's own published one, the principal that created the cluster.
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
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
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

/// Create the IAM role `name`, trusted by Amazon EC2 as a node role is, carrying the
/// AWS-managed policies `policies` — and answer its ARN.
fn node_role(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    policies: &[&str],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ec2.amazonaws.com" },
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
        "setup: the node role {name:?} must be created, got {created:?}"
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

/// Create the cluster `name` with one node group running under `role`, wait for the
/// node group to be ACTIVE and for the cluster's API server to report one node, and
/// answer the address the cluster published.
fn cluster_with_a_node(
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
    let nodegroup = eks(
        router,
        runtime,
        "POST",
        &format!("/clusters/{name}/node-groups"),
        Some(json!({
            "nodegroupName": "pullers",
            "nodeRole": role,
            "subnets": subnets,
            "scalingConfig": { "minSize": 1, "maxSize": 1, "desiredSize": 1 },
        })),
    );
    assert_eq!(
        nodegroup.status, 200,
        "setup: the node group of {name:?} must be created, got {nodegroup:?} ({})",
        nodegroup.message()
    );
    read_until(
        "the node group to reach ACTIVE",
        || {
            eks(
                router,
                runtime,
                "GET",
                &format!("/clusters/{name}/node-groups/pullers"),
                None,
            )
        },
        |answer| answer.body["nodegroup"]["status"] == json!("ACTIVE"),
    );
    // The node the pods below are scheduled onto: the instance the node group
    // launched, as the cluster's own API server reports it.
    for _ in 0..POLLS {
        let nodes = k8s(
            runtime,
            &endpoint,
            "the cluster's nodes",
            "GET",
            "/api/v1/nodes",
            None,
        );
        if nodes.body["items"].as_array().is_some_and(|items| !items.is_empty()) {
            return endpoint;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("setup: the cluster {name:?} must report the node its node group launched");
}

/// What the registry answered for one row's image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pull {
    /// The registry served the image, under the digest it holds it at.
    Served,
    /// The registry would not serve it.
    Refused,
    /// No registry this emulator holds answers for the reference, so no pull is made.
    Undecided,
}

/// One row: the pod, the image its container runs, the cluster (and so the node role)
/// it is created on, and what the registry must have answered.
struct Row {
    case: &'static str,
    pod: &'static str,
    image: String,
    granted: bool,
    expected: Pull,
}

/// The events of `namespace` about the pod `pod`, as a client asks for them: the
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
        &format!("/api/v1/namespaces/default/events?fieldSelector=involvedObject.name%3D{pod}"),
        None,
    );
    assert_eq!(
        answered.status, 200,
        "{what}: the pod's events must be served, got {answered:?}"
    );
    answered.body["items"].as_array().cloned().unwrap_or_default()
}

/// The event of `pod` carrying `reason` whose message starts with `starts_with`, polled
/// for until the clock-owed pull has been made; `None` once the deadline passes.
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

#[test]
fn a_pods_image_is_pulled_as_its_nodes_role_and_the_cluster_records_what_the_registry_answered() {
    std::thread::Builder::new()
        .name("eks-pod-image-pull".to_string())
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

            // The two node roles, and one cluster under each: a pod is scheduled
            // onto the node of its own cluster, so the role the pull is made as is
            // the one that cluster's node group was created with.
            let subnets = default_subnets(&router, &runtime);
            let granted_role = node_role(
                &router,
                &runtime,
                "eksPullingNodeRole",
                &[WORKER_NODE_POLICY, ECR_READ_POLICY],
            );
            let withheld_role = node_role(
                &router,
                &runtime,
                "eksNoPullNodeRole",
                &[WORKER_NODE_POLICY],
            );
            let granted_endpoint = cluster_with_a_node(
                &router,
                &runtime,
                "pulling-cluster",
                &granted_role,
                &subnets,
            );
            let withheld_endpoint = cluster_with_a_node(
                &router,
                &runtime,
                "withheld-cluster",
                &withheld_role,
                &subnets,
            );

            let account = engine::emulator_account_id();
            let published = |path: &str| format!("{account}.dkr.ecr.{REGION}.amazonaws.com/{path}");
            let rows = [
                Row {
                    case: "the image the registry holds, pulled by a role granted the pull",
                    pod: "pull-served",
                    image: published(&format!("{REPOSITORY}:{TAG}")),
                    granted: true,
                    expected: Pull::Served,
                },
                Row {
                    case: "the same image addressed by the URI the registry published",
                    pod: "pull-published-uri",
                    image: format!("{repository_uri}:{TAG}"),
                    granted: true,
                    expected: Pull::Served,
                },
                Row {
                    case: "a tag the repository does not hold",
                    pod: "pull-absent-tag",
                    image: published(&format!("{REPOSITORY}:absent")),
                    granted: true,
                    expected: Pull::Refused,
                },
                Row {
                    case: "a repository the registry does not hold",
                    pod: "pull-absent-repository",
                    image: published(&format!("no-such-repository:{TAG}")),
                    granted: true,
                    expected: Pull::Refused,
                },
                Row {
                    case: "the image, pulled by a role no policy grants the pull",
                    pod: "pull-withheld",
                    image: published(&format!("{REPOSITORY}:{TAG}")),
                    granted: false,
                    expected: Pull::Refused,
                },
                Row {
                    case: "a registry this emulator does not hold",
                    pod: "pull-foreign",
                    image: "registry.example.com/app:1".to_string(),
                    granted: true,
                    expected: Pull::Undecided,
                },
            ];

            for row in &rows {
                let Row {
                    case,
                    pod,
                    image,
                    granted,
                    expected,
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
                    "/api/v1/namespaces/default/pods",
                    Some(&json!({
                        "metadata": { "name": pod },
                        "spec": { "containers": [{ "name": "app", "image": image }] },
                    })),
                );
                assert_eq!(
                    created.status, 201,
                    "case {case:?}: the pod must be created, got {created:?}"
                );

                // THE SCHEDULER'S RECORD: the pod was assigned to the node its
                // cluster's node group launched.
                let scheduled = event_until(
                    &runtime,
                    endpoint,
                    case,
                    pod,
                    "Scheduled",
                    &format!("Successfully assigned default/{pod} to "),
                )
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the scheduler's record, got {:?}",
                        pod_events(&runtime, endpoint, case, pod)
                    )
                });
                assert_eq!(
                    scheduled["source"]["component"], json!("default-scheduler"),
                    "case {case:?}: the scheduler recorded it, got {scheduled}"
                );

                match expected {
                    Pull::Served => {
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
                                "case {case:?}: expected the pull the registry served, got {:?}",
                                pod_events(&runtime, endpoint, case, pod)
                            )
                        });
                        assert_eq!(
                            pulled["type"], json!("Normal"),
                            "case {case:?}: a pull that was served is normal, got {pulled}"
                        );
                        assert_eq!(
                            pulled["source"]["component"], json!("kubelet"),
                            "case {case:?}: the node's kubelet made the pull, got {pulled}"
                        );
                        assert_eq!(
                            pulled["involvedObject"]["fieldPath"],
                            json!("spec.containers{app}"),
                            "case {case:?}: the event names the container it is about, got \
                             {pulled}"
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
                        // WHAT THE REGISTRY RESOLVED: the container reports the
                        // manifest digest that registry holds the image at.
                        let read = k8s(
                            &runtime,
                            endpoint,
                            case,
                            "GET",
                            &format!("/api/v1/namespaces/default/pods/{pod}"),
                            None,
                        );
                        let status = read.body["status"]["containerStatuses"][0].clone();
                        let image_id = status["imageID"].as_str().unwrap_or_default().to_string();
                        assert!(
                            image_id.ends_with(&format!("@{digest}")),
                            "case {case:?}: the container must report the digest the registry \
                             resolved ({digest}), got {image_id:?}"
                        );
                    }
                    Pull::Refused => {
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
                            failed["source"]["component"], json!("kubelet"),
                            "case {case:?}: the node's kubelet made the pull, got {failed}"
                        );
                        assert!(
                            event_until(
                                &runtime,
                                endpoint,
                                case,
                                pod,
                                "Failed",
                                "Error: ErrImagePull"
                            )
                            .is_some(),
                            "case {case:?}: the container is left in the pull's own error, got \
                             {:?}",
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
                        // A CONTAINER WITH NO IMAGE IS NOT CREATED: it waits on the
                        // pull, and the pod stays in the phase a pod waiting on one
                        // is in.
                        let read = k8s(
                            &runtime,
                            endpoint,
                            case,
                            "GET",
                            &format!("/api/v1/namespaces/default/pods/{pod}"),
                            None,
                        );
                        let status = read.body["status"]["containerStatuses"][0].clone();
                        assert_eq!(
                            status["state"]["waiting"]["reason"], json!("ImagePullBackOff"),
                            "case {case:?}: the container waits on the pull, got {status}"
                        );
                        assert_eq!(
                            read.body["status"]["phase"], json!("Pending"),
                            "case {case:?}: a pod whose image was refused has started nothing, \
                             got {}",
                            read.body
                        );
                        assert_eq!(
                            status["imageID"], json!(""),
                            "case {case:?}: no digest was resolved for it, got {status}"
                        );
                    }
                    Pull::Undecided => {
                        // Nothing here answers for that registry, so no pull was made
                        // — and the pod still reaches the end a pod of this emulator
                        // reaches, which is what says no pull is being waited on.
                        let concluded = read_until(
                            "the pod to reach the phase it ends in",
                            || {
                                let read = k8s(
                                    &runtime,
                                    endpoint,
                                    case,
                                    "GET",
                                    &format!("/api/v1/namespaces/default/pods/{pod}"),
                                    None,
                                );
                                Answer {
                                    status: read.status,
                                    error_type: None,
                                    text: read.body.to_string(),
                                    body: read.body,
                                }
                            },
                            |answer| answer.body["status"]["phase"] == json!("Failed"),
                        );
                        assert_eq!(
                            concluded.body["status"]["containerStatuses"][0]["imageID"],
                            json!(""),
                            "case {case:?}: no registry here resolved a digest for it, got {}",
                            concluded.body
                        );
                        for event in pod_events(&runtime, endpoint, case, pod) {
                            let message = event["message"].as_str().unwrap_or_default();
                            assert!(
                                !message.starts_with("Pulling image")
                                    && !message.starts_with("Successfully pulled image")
                                    && !message.starts_with("Failed to pull image"),
                                "case {case:?}: no pull was made, so none is recorded, got \
                                 {event}"
                            );
                        }
                    }
                }
            }

            // The events of one pod are the events OF THAT POD: the selector the rows
            // read them through answers no event of another.
            for row in &rows {
                let endpoint = if row.granted {
                    &granted_endpoint
                } else {
                    &withheld_endpoint
                };
                for event in pod_events(&runtime, endpoint, row.case, row.pod) {
                    assert_eq!(
                        event["involvedObject"]["name"], json!(row.pod),
                        "case {:?}: every event the selector answers is about that pod, got \
                         {event}",
                        row.case
                    );
                }
            }

            // The pull reads the registry rather than writing it: the image is still
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

            // The pull is made through the registry, so a pull of an image of ANOTHER
            // account's registry is that registry's to answer and nothing here serves
            // it: the reference is refused rather than resolved against this one.
            let foreign = format!("000000000001.dkr.ecr.{REGION}.amazonaws.com/{REPOSITORY}:{TAG}");
            let created = k8s(
                &runtime,
                &granted_endpoint,
                "another account's registry",
                "POST",
                "/api/v1/namespaces/default/pods",
                Some(&json!({
                    "metadata": { "name": "pull-cross-account" },
                    "spec": { "containers": [{ "name": "app", "image": foreign }] },
                })),
            );
            assert_eq!(
                created.status, 201,
                "case another account's registry: the pod must be created, got {created:?}"
            );
            assert!(
                event_until(
                    &runtime,
                    &granted_endpoint,
                    "another account's registry",
                    "pull-cross-account",
                    "Failed",
                    &format!("Failed to pull image \"{foreign}\": ")
                )
                .is_some(),
                "case another account's registry: the pull must be refused, got {:?}",
                pod_events(
                    &runtime,
                    &granted_endpoint,
                    "another account's registry",
                    "pull-cross-account"
                )
            );
        })
        .expect("the suite thread spawns")
        .join()
        .expect("the suite thread completes");
}
