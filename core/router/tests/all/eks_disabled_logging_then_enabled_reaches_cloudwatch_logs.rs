// SPDX-License-Identifier: CC0-1.0

//! The DISABLED/ENABLED contrast of an Amazon EKS cluster's control plane logging, walked on
//! the router the emulator serves traffic with: every service mounted, and every request
//! SIGNED and verified.
//!
//! RULE: `LogSetup$enabled` is "If a log type is enabled, that log type exports its control
//! plane logs to CloudWatch Logs. If a log type isn't enabled, that log type doesn't export
//! its control plane logs", so the state of a cluster's log types is observable in CloudWatch
//! Logs and nowhere else: while EVERY type is disabled the derived group
//! `/aws/eks/<cluster name>/cluster` does not exist, and once a configuration enabling at
//! least one type has been applied — here by an `UpdateClusterConfig` whose update reports
//! `Successful` — the service itself serves that group, found the way a client finds it,
//! through `DescribeLogGroups` on that name as a prefix.
//!
//! What this suite adds to the ones beside it (`eks_control_plane_logs.rs`,
//! `eks_logging_update_reaches_cloudwatch_logs.rs`, which mount EKS and CloudWatch Logs alone
//! over fabricated subnet ids and with authentication off) are the three axes none of them
//! can see:
//!
//! * **The contrast is walked on a cluster whose inputs all resolve**: a VPC and two subnets
//!   EC2 holds, an IAM role whose trust policy admits the Amazon EKS service principal, and a
//!   create carrying an explicitly ALL-DISABLED configuration of every declared log type,
//!   with the absence of the group asserted while it holds.
//! * **Every service the emulator serves is mounted** (`build_router`), so the service that
//!   claims each call is the one that claims it in the served emulator.
//! * **The signatures are verified.** Every call below is signed with the keys this process
//!   publishes once authentication is on — and so is the export Amazon EKS makes THROUGH THE
//!   EMULATOR'S OWN CALLBACK, which a router that authenticates every request has to accept
//!   before CloudWatch Logs ever holds the group.
//!
//! The rows differ along what the rule generalises over: how many types one enabling update
//! turns on (one, two, every declared type), WHICH ones they are, the shape of the
//! configuration carrying them (one entry, two entries, an enabling entry beside a disabling
//! one), and — the control row — an update that enables nothing, after which the group is
//! still absent.
//!
//! The second rule here is about WHO owes the export: the configuration the update puts on the
//! cluster, not the one call that asked for it. Its rows walk an enabling update whose own
//! export could not land — CloudWatch Logs not mounted beside Amazon EKS when the call was
//! made, and a call answered from the update already recorded under its
//! `clientRequestToken`, which is what an SDK's retry of it is answered with — and require
//! the group once that update reports `Successful` either way.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread. The drivers are this suite's own rather than
//! `eks_wire`'s, for the reason the authenticating suites beside it give: `eks_wire::send`
//! signs nothing (its suites run with authentication off) and carries no `X-Amz-Target`
//! driver, which is how CloudWatch Logs is reached. The form encoder and the XML readers come
//! from `query_wire.rs`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in.
const REGION: &str = "us-west-2";

/// The signing names the router picks each service by.
const EKS: &str = "eks";
const EC2: &str = "ec2";
const IAM: &str = "iam";
const LOGS: &str = "logs";

/// The API version a client sends in each query-protocol form body.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";

/// The `X-Amz-Target` prefix CloudWatch Logs names an operation under: its service shape's
/// name. (EKS is `restJson1` and carries none.)
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// Every member of `LogType`, in the order the model declares them
/// (`smithy_query(query="shape",shape="LogType",
/// model_path="services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json")`).
const DECLARED_LOG_TYPES: [&str; 5] = [
    "api",
    "audit",
    "authenticator",
    "controllerManager",
    "scheduler",
];

/// The `UpdateStatus` an update still running reports, and the terminal one a configuration
/// change that was applied settles on.
const UPDATE_IN_PROGRESS: &str = "InProgress";
const UPDATE_SUCCESSFUL: &str = "Successful";

/// The cluster status an update may be started on.
const CLUSTER_ACTIVE: &str = "ACTIVE";

/// How long to wait between two looks at a condition the clock owes (a cluster's
/// `CREATING` -> `ACTIVE` move, an update's completion), and how many looks bound such a
/// wait. Generous on purpose: the condition, never the clock, is what these rows assert on.
const POLL_WAIT: Duration = Duration::from_millis(100);
const POLL_ATTEMPTS: usize = 600;

/// The managed policy AWS publishes for a cluster's service role.
const CLUSTER_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonEKSClusterPolicy";

/// A trust policy letting the Amazon EKS service principal assume the cluster's role.
const EKS_TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"eks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The key pair this process publishes once request authentication is on, which every call
/// below is signed with.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One answer, reduced to what these rows branch on.
#[derive(Debug)]
struct Answer {
    status: u16,
    text: String,
    body: Value,
}

/// The emulator these rows drive: one router, the keys it accepts, and the runtime the calls
/// are driven on.
struct Emulator {
    router: MultiServiceRouter,
    signer: Signer,
    runtime: tokio::runtime::Runtime,
}

impl Emulator {
    /// Drive `router` with request authentication ON, signing every call with the keys this
    /// process publishes.
    fn driving(mut router: MultiServiceRouter) -> Self {
        router.set_request_authentication(true);
        let published = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning authentication on publishes this process's keys");
        Self {
            router,
            signer: Signer {
                key_id: published.access_key_id.clone(),
                secret: published.secret_access_key.clone(),
            },
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime"),
        }
    }

    /// One SIGNED request at the service `signing_name` claims.
    fn send(
        &self,
        signing_name: &str,
        method: &str,
        path: &str,
        content_type: &str,
        target: Option<String>,
        body: Vec<u8>,
    ) -> Answer {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", &host)
            .header("content-type", content_type);
        if let Some(target) = target {
            builder = builder.header("x-amz-target", target);
        }
        let built = builder.body(body).expect("the test request is well formed");
        let signed = self.signer.sign(built, signing_name);
        let (parts, payload) = signed.into_parts();
        let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
        let response = self.runtime.block_on(self.router.dispatch(request));
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Answer { status, text, body }
    }

    /// One EKS call (restJson1).
    fn eks(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let payload = body.map(|json| json.to_string()).unwrap_or_default();
        self.send(
            EKS,
            method,
            path,
            "application/json",
            None,
            payload.into_bytes(),
        )
    }

    /// One query-protocol action: `POST /` with `Action`, `Version` and the parameters in the
    /// form body, each component encoded by the one encoder this test crate owns.
    fn query(
        &self,
        signing_name: &str,
        action: &str,
        version: &str,
        params: &[(&str, &str)],
    ) -> Answer {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push_str(&format!("&{name}={}", form_encode(value)));
        }
        self.send(
            signing_name,
            "POST",
            "/",
            "application/x-www-form-urlencoded",
            None,
            body.into_bytes(),
        )
    }

    /// One CloudWatch Logs call (`awsJson1_1`, named by its `X-Amz-Target`).
    fn logs(&self, operation: &str, body: Value) -> Answer {
        self.send(
            LOGS,
            "POST",
            "/",
            "application/x-amz-json-1.1",
            Some(format!("{LOGS_TARGET_PREFIX}.{operation}")),
            body.to_string().into_bytes(),
        )
    }

    /// Whether CloudWatch Logs serves the cluster's log group, found as a client finds it:
    /// `DescribeLogGroups` narrowed to that name as a prefix.
    fn holds_log_group(&self, case: &str, cluster: &str) -> bool {
        let group = cluster_log_group(cluster);
        let answer = self.logs("DescribeLogGroups", json!({ "logGroupNamePrefix": &group }));
        assert_eq!(
            answer.status, 200,
            "case {case:?}: DescribeLogGroups must answer 200, got {answer:?}"
        );
        answer.body["logGroups"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|held| held["logGroupName"] == json!(group))
    }

    /// The streams the cluster's log group holds, by name.
    fn log_streams(&self, case: &str, cluster: &str) -> Vec<String> {
        let answer = self.logs(
            "DescribeLogStreams",
            json!({ "logGroupName": cluster_log_group(cluster) }),
        );
        assert_eq!(
            answer.status, 200,
            "case {case:?}: DescribeLogStreams must answer 200, got {answer:?}"
        );
        answer.body["logStreams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_owned))
            .collect()
    }

    /// The cluster as `DescribeCluster` reports it.
    fn describe_cluster(&self, case: &str, cluster: &str) -> Value {
        let answer = self.eks("GET", &format!("/clusters/{cluster}"), None);
        assert_eq!(
            answer.status, 200,
            "case {case:?}: DescribeCluster must answer 200, got {answer:?}"
        );
        answer.body["cluster"].clone()
    }

    /// Wait for the cluster to report `ACTIVE`, the status an update needs.
    fn wait_until_active(&self, case: &str, cluster: &str) {
        for _ in 0..POLL_ATTEMPTS {
            if self.describe_cluster(case, cluster)["status"] == json!(CLUSTER_ACTIVE) {
                return;
            }
            std::thread::sleep(POLL_WAIT);
        }
        panic!("case {case:?}: the cluster {cluster} never reached {CLUSTER_ACTIVE}");
    }

    /// Poll `DescribeUpdate` until the update leaves `InProgress`, and answer the status it
    /// settled on — the step a client performs before it goes looking for the logs.
    fn update_status_when_settled(&self, case: &str, cluster: &str, update_id: &str) -> String {
        for _ in 0..POLL_ATTEMPTS {
            let answer = self.eks(
                "GET",
                &format!("/clusters/{cluster}/updates/{update_id}"),
                None,
            );
            assert_eq!(
                answer.status, 200,
                "case {case:?}: DescribeUpdate must answer 200, got {answer:?}"
            );
            let status = answer.body["update"]["status"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if status != UPDATE_IN_PROGRESS {
                return status;
            }
            std::thread::sleep(POLL_WAIT);
        }
        panic!("case {case:?}: update {update_id} never left {UPDATE_IN_PROGRESS}");
    }
}

/// The log group one cluster's control plane logs are exported to.
fn cluster_log_group(cluster: &str) -> String {
    format!("/aws/eks/{cluster}/cluster")
}

/// One `logging.clusterLogging` payload: one entry per `(types, enabled)` pair, as a client
/// writes it.
fn cluster_logging(entries: &[(&[&str], bool)]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|(types, enabled)| json!({ "types": types, "enabled": enabled }))
            .collect(),
    )
}

/// A role ARN of the account this emulator serves, under the default path.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// One row: the `logging` an `UpdateClusterConfig` carries, and the types it turns on (in the
/// order the model declares them).
struct Row {
    case: &'static str,
    cluster: &'static str,
    logging: Value,
    exporting: Vec<&'static str>,
}

#[test]
fn an_enabled_log_type_creates_the_clusters_group_a_disabled_one_leaves_absent() {
    std::thread::Builder::new()
        .name("eks-logging-contrast".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            // The whole suite runs with request authentication ON, as the emulator serves
            // traffic — including the export the service makes through its own callback.
            let emulator = Emulator::driving(build_router(engine));

            // ---------------------------------------------------------------
            // Setup, through the services that OWN these inputs: a VPC with
            // two subnets in two Availability Zones, and a role IAM holds
            // whose trust policy admits the Amazon EKS service principal.
            // ---------------------------------------------------------------
            let created = emulator.query(
                EC2,
                "CreateVpc",
                EC2_API_VERSION,
                &[("CidrBlock", "10.42.0.0/16")],
            );
            assert_eq!(
                created.status, 200,
                "setup: CreateVpc must succeed, got {created:?}"
            );
            let vpc_id = opt_element(&created.text, "vpcId")
                .unwrap_or_else(|| panic!("setup: no <vpcId> in {}", created.text));
            let mut subnets: Vec<String> = Vec::new();
            let zones = [format!("{REGION}a"), format!("{REGION}b")];
            for (cidr_block, zone) in ["10.42.1.0/24", "10.42.2.0/24"].into_iter().zip(&zones) {
                let answer = emulator.query(
                    EC2,
                    "CreateSubnet",
                    EC2_API_VERSION,
                    &[
                        ("VpcId", vpc_id.as_str()),
                        ("CidrBlock", cidr_block),
                        ("AvailabilityZone", zone.as_str()),
                    ],
                );
                assert_eq!(
                    answer.status, 200,
                    "setup: CreateSubnet {cidr_block} must succeed, got {answer:?}"
                );
                subnets.push(
                    opt_element(&answer.text, "subnetId")
                        .unwrap_or_else(|| panic!("setup: no <subnetId> in {}", answer.text)),
                );
            }
            let role_name = "eksLoggingContrastRole";
            let role = emulator.query(
                IAM,
                "CreateRole",
                IAM_API_VERSION,
                &[
                    ("RoleName", role_name),
                    ("AssumeRolePolicyDocument", EKS_TRUST_POLICY),
                ],
            );
            assert_eq!(
                role.status, 200,
                "setup: CreateRole must succeed, got {role:?}"
            );
            let attached = emulator.query(
                IAM,
                "AttachRolePolicy",
                IAM_API_VERSION,
                &[("RoleName", role_name), ("PolicyArn", CLUSTER_POLICY_ARN)],
            );
            assert_eq!(
                attached.status, 200,
                "setup: AttachRolePolicy must succeed, got {attached:?}"
            );

            let rows = vec![
                Row {
                    case: "the type the report cited, enabled on its own",
                    cluster: "logging-contrast-api",
                    logging: cluster_logging(&[(&["api"], true)]),
                    exporting: vec!["api"],
                },
                Row {
                    case: "two types the report did not cite, enabled in one entry",
                    cluster: "logging-contrast-audit-scheduler",
                    logging: cluster_logging(&[(&["audit", "scheduler"], true)]),
                    exporting: vec!["audit", "scheduler"],
                },
                Row {
                    case: "one type enabled beside an entry disabling the other four",
                    cluster: "logging-contrast-mixed",
                    logging: cluster_logging(&[
                        (&["authenticator"], true),
                        (&["api", "audit", "controllerManager", "scheduler"], false),
                    ]),
                    exporting: vec!["authenticator"],
                },
                Row {
                    case: "every declared type enabled at once",
                    cluster: "logging-contrast-every-type",
                    logging: cluster_logging(&[(&DECLARED_LOG_TYPES, true)]),
                    exporting: DECLARED_LOG_TYPES.to_vec(),
                },
                Row {
                    case: "an update that enables nothing leaves the group absent",
                    cluster: "logging-contrast-still-disabled",
                    logging: cluster_logging(&[(&["api", "audit"], false)]),
                    exporting: vec![],
                },
            ];

            for row in rows {
                let case = row.case;
                let cluster = row.cluster;

                // The create carries an explicitly ALL-DISABLED configuration of every
                // declared type, which is the arm that must leave CloudWatch Logs untouched.
                let created = emulator.eks(
                    "POST",
                    "/clusters",
                    Some(json!({
                        "name": cluster,
                        "roleArn": role_arn(role_name),
                        "resourcesVpcConfig": { "subnetIds": subnets },
                        "logging": {
                            "clusterLogging": cluster_logging(&[(&DECLARED_LOG_TYPES, false)]),
                        },
                    })),
                );
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: CreateCluster must answer 200, got {created:?}"
                );
                emulator.wait_until_active(case, cluster);
                assert!(
                    !emulator.holds_log_group(case, cluster),
                    "case {case:?}: with every log type disabled, CloudWatch Logs must serve no \
                     {}",
                    cluster_log_group(cluster)
                );

                // Then the update, polled to the status a client waits for before it goes
                // looking for the logs.
                let started = emulator.eks(
                    "POST",
                    &format!("/clusters/{cluster}/update-config"),
                    Some(json!({ "logging": { "clusterLogging": row.logging } })),
                );
                assert_eq!(
                    started.status, 200,
                    "case {case:?}: UpdateClusterConfig must answer 200, got {started:?}"
                );
                let update_id = started.body["update"]["id"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the update must carry an id, got {started:?}")
                    })
                    .to_string();
                let status = emulator.update_status_when_settled(case, cluster, &update_id);
                assert_eq!(
                    status, UPDATE_SUCCESSFUL,
                    "case {case:?}: the logging update must settle on {UPDATE_SUCCESSFUL}, got \
                     {status:?}"
                );

                let held = emulator.holds_log_group(case, cluster);
                if row.exporting.is_empty() {
                    assert!(
                        !held,
                        "case {case:?}: an update that enables no type must leave {} absent",
                        cluster_log_group(cluster)
                    );
                } else {
                    assert!(
                        held,
                        "case {case:?}: once {:?} is enabled CloudWatch Logs must serve {}, got \
                         no such group",
                        row.exporting,
                        cluster_log_group(cluster)
                    );
                    // And one stream per exporting type under it, which is what carries the
                    // logs the client turned on.
                    let streams = emulator.log_streams(case, cluster);
                    assert_eq!(
                        streams.len(),
                        row.exporting.len(),
                        "case {case:?}: expected one stream per exporting type {:?}, got \
                         {streams:?}",
                        row.exporting
                    );
                }
            }
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// One row of the second rule: what the call that STARTED the enabling update was answered
/// with, which must not decide whether the group ends up there.
struct OwedRow {
    case: &'static str,
    cluster: &'static str,
    /// The `clientRequestToken` the call carries, and so whether the second call is the
    /// RETRY of the first one (the same token) or a change of its own (`None`).
    token: Option<&'static str>,
    /// The types the configuration enables.
    enabling: Vec<&'static str>,
}

#[test]
fn a_completed_logging_update_gets_its_group_whatever_answered_the_call_that_started_it() {
    // The RULE this pins: the export belongs to the configuration the update puts ON THE
    // CLUSTER, not to the one call that asked for it. "Cluster updates are asynchronous...
    // When the update is complete... the cluster status moves to Active", so the types a
    // `logging` update enables are the cluster's from the instant the clock completes that
    // update, and the group and streams `LogSetup$enabled` owes them are owed from then —
    // whatever became of the call that started it.
    //
    // Two things that become of such a call, both of which leave its own export unmade:
    //
    // * CloudWatch Logs could not take it when the call was made (here: no CloudWatch Logs
    //   was mounted beside Amazon EKS at all, which is also what a write it refused leaves);
    // * the call was a RETRY, answered from the update already recorded under the same
    //   `clientRequestToken` ("a unique, case-sensitive identifier that you provide to ensure
    //   the idempotency of the request" — an SDK resends it when it retries), so it started
    //   nothing and exported nothing.
    //
    // Either way the client is told the update succeeded, so either way the group has to be
    // there once it does. Both arms are driven over ONE engine, as one emulator's state: the
    // first router mounts Amazon EKS alone, the second mounts CloudWatch Logs beside it.
    std::thread::Builder::new()
        .name("eks-logging-owed".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut alone = MultiServiceRouter::new();
            alone.register_service(
                service_com_amazonaws_eks_awswesleyfrontend::EmulatedService::new(Arc::clone(
                    &engine,
                )),
            );
            let without_logs = Emulator::driving(alone);
            let mut beside = MultiServiceRouter::new();
            beside.register_service(
                service_com_amazonaws_eks_awswesleyfrontend::EmulatedService::new(Arc::clone(
                    &engine,
                )),
            );
            beside.register_service(
                service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
            );
            let with_logs = Emulator::driving(beside);

            let rows = vec![
                OwedRow {
                    case: "the call's own export could not be taken",
                    cluster: "logging-owed-unmounted",
                    token: None,
                    enabling: vec!["api"],
                },
                OwedRow {
                    case: "the second call is the retry of the first, under its token",
                    cluster: "logging-owed-retried",
                    token: Some("a3f1c0de-1111-4222-8333-444455556666"),
                    enabling: vec!["api", "audit"],
                },
                OwedRow {
                    case: "a retry of a change enabling every declared type",
                    cluster: "logging-owed-retried-all",
                    token: Some("b4e2d1ef-5555-4666-8777-888899990000"),
                    enabling: DECLARED_LOG_TYPES.to_vec(),
                },
            ];

            for row in rows {
                let case = row.case;
                let cluster = row.cluster;

                // The cluster is created with no logging at all, and while Amazon EKS stands
                // alone: nothing it does can reach CloudWatch Logs.
                let created = without_logs.eks(
                    "POST",
                    "/clusters",
                    Some(json!({
                        "name": cluster,
                        "roleArn": role_arn("eksClusterRole"),
                        "resourcesVpcConfig": {
                            "subnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"],
                        },
                    })),
                );
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: CreateCluster must answer 200, got {created:?}"
                );
                without_logs.wait_until_active(case, cluster);

                // The enabling update, started where its own export cannot land.
                let mut body = json!({
                    "logging": { "clusterLogging": cluster_logging(&[(&row.enabling, true)]) },
                });
                if let Some(token) = row.token {
                    body["clientRequestToken"] = json!(token);
                }
                let started = without_logs.eks(
                    "POST",
                    &format!("/clusters/{cluster}/update-config"),
                    Some(body.clone()),
                );
                assert_eq!(
                    started.status, 200,
                    "case {case:?}: UpdateClusterConfig must answer 200, got {started:?}"
                );
                let update_id = started.body["update"]["id"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the update must carry an id, got {started:?}")
                    })
                    .to_string();

                // A row carrying a token resends the SAME call, now where CloudWatch Logs IS
                // mounted: it is answered with the update it already started, so it starts
                // nothing and exports nothing of its own.
                if row.token.is_some() {
                    let retried = with_logs.eks(
                        "POST",
                        &format!("/clusters/{cluster}/update-config"),
                        Some(body),
                    );
                    assert_eq!(
                        retried.status, 200,
                        "case {case:?}: the retry must answer 200, got {retried:?}"
                    );
                    assert_eq!(
                        retried.body["update"]["id"],
                        json!(update_id),
                        "case {case:?}: the retry must be answered with the update its token \
                         started, got {retried:?}"
                    );
                }

                // And then the one thing a client does before it goes looking for the logs:
                // poll the update to its terminal status.
                let status = with_logs.update_status_when_settled(case, cluster, &update_id);
                assert_eq!(
                    status, UPDATE_SUCCESSFUL,
                    "case {case:?}: the logging update must settle on {UPDATE_SUCCESSFUL}, got \
                     {status:?}"
                );
                assert!(
                    with_logs.holds_log_group(case, cluster),
                    "case {case:?}: an update that reports {UPDATE_SUCCESSFUL} must have its \
                     group, got no {}",
                    cluster_log_group(cluster)
                );
                let streams = with_logs.log_streams(case, cluster);
                assert_eq!(
                    streams.len(),
                    row.enabling.len(),
                    "case {case:?}: expected one stream per enabled type {:?}, got {streams:?}",
                    row.enabling
                );
            }
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}
