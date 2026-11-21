// SPDX-License-Identifier: CC0-1.0

//! What a CLIENT can read off the host after an EC2/on-premises (`Server`) deployment, read
//! the way a client reads a machine it has no shell on: Systems Manager Run Command.
//!
//! RULE under test: a deployment that reports `Succeeded` has installed the revision's files
//! on the instance, so the agent inside that instance's guest sees them — `cat` of a declared
//! destination through `AWS-RunShellScript` reports the revision's own content — and a
//! deployment that FAILED on its host is followed, where the configuration asks for a rollback
//! on a deployment failure, by a rollback deployment of the revision the group was on which
//! REINSTALLS that revision's files, so the same read reports the previous revision's content
//! again rather than the failed one's.
//!
//! This is the same fact the sibling suite `server_platform_instance_deployments.rs` asserts
//! against the store that owns the guest; what it adds is the path a client really has to the
//! host — Systems Manager delivering a document's script to the agent in that guest — so the
//! install has to be visible THERE, through a second service, and not only in the record
//! CodeDeploy keeps. Five services are mounted on ONE engine for it: CodeDeploy, Amazon EC2,
//! Amazon S3, Systems Manager and IAM (a node registers only as a role its instance profile
//! carries, which is a fact IAM holds).
//!
//! Nothing here waits a fixed time: every span is a stored due instant — the launch reaching
//! `running`, the node registering, each deployment reaching its outcome, the invocation
//! moving off `Pending` — so each step LOOKS AGAIN until the condition holds or [`BUDGET`]
//! passes, and nothing asserts on elapsed time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::server_platform_instance_deployments::{app_spec, bundle, element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The EC2 API version an `ec2Query` body carries beside its `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The IAM API version an `awsQuery` body carries beside its `Action`.
const IAM_API_VERSION: &str = "2010-05-08";

/// The AMI the launch below runs from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// The service role the deployment group is created with; nothing resolves it here.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The bucket the two revisions are uploaded to.
const BUCKET: &str = "codedeploy-rollback-revisions";

/// The instance profile the host runs as, and the role it carries: a managed node registers
/// as that role, which is why the agent in the guest can be sent a command at all.
const PROFILE: &str = "codedeploy-host-profile";
/// See [`PROFILE`] — deliberately not the profile's own name.
const ROLE: &str = "codedeploy-host-role";

/// The AWS-managed policy a client attaches to that role so the agent may register its node.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// The service principal the role's trust policy admits, so Amazon EC2 may act as it.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The document Run Command delivers a shell script to a Linux node with.
const SHELL_DOCUMENT: &str = "AWS-RunShellScript";

/// The tag the host carries and the deployment group selects on.
const FLEET_TAG: (&str, &str) = ("Name", "rollback-run-command-host");

/// Where the revisions below install their payload on the host.
const DESTINATION: &str = "/opt/rollback-read";

/// What the first revision's payload carries, and what the rollback has to put back.
const MARKER_A: &str = "revision-a-payload";
/// What the failing revision's payload carries.
const MARKER_B: &str = "revision-b-payload";

/// How long any one condition is given. Generous by design: it is a deadline, not a
/// measurement.
const BUDGET: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL: Duration = Duration::from_millis(25);

/// One emulator carrying the five services this suite drives over ONE shared engine.
///
/// A harness of its own rather than the sibling suite's `Fleet`: this one needs Systems
/// Manager and IAM mounted beside the three that suite mounts, and the two readers a client
/// has to a host — that suite reads the guest through the store that owns it, this one through
/// the agent inside it — are the very thing the two suites are there to keep apart. What does
/// not differ is shared ([`element`], [`form_encode`], [`app_spec`], [`bundle`]).
struct Estate {
    router: MultiServiceRouter,
}

impl Estate {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing; the SigV4 credential scope is what routes each request
        // and names the Region it is served in.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_codedeploy_codedeploy_20141006::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` header whose credential scope names `signing_name`.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
             /aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// One awsJson1_1 call of `signing_name`, whose `x-amz-target` names `prefix.operation`.
    async fn json(
        &self,
        signing_name: &str,
        prefix: &str,
        operation: &str,
        body: Value,
    ) -> (u16, Value) {
        let request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("x-amz-target", format!("{prefix}.{operation}"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("authorization", Self::authorization(signing_name))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One CodeDeploy operation that must succeed.
    async fn codedeploy(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, parsed) = self
            .json("codedeploy", "CodeDeploy_20141006", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CodeDeploy {operation} must succeed, got {status} with {parsed:?}"
        );
        parsed
    }

    /// One Systems Manager operation that must succeed.
    async fn ssm(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.json("ssm", "AmazonSSM", operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: Systems Manager {operation} must succeed, got {status} with \
             {parsed:?}"
        );
        parsed
    }

    /// One form-encoded query call (`ec2Query`, `awsQuery`) that must succeed, as its XML
    /// body.
    async fn form(
        &self,
        case: &str,
        signing_name: &str,
        action: &str,
        version: &str,
        params: &[(&str, &str)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization(signing_name))
            .body(bytes::Bytes::from(body))
            .expect("the request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: {signing_name} {action} must succeed, got {status} with {answered}"
        );
        answered
    }

    /// Upload `payload` at `key` through S3's own API, creating the bucket the first time.
    async fn upload(&self, case: &str, key: &str, payload: &[u8]) {
        for (method, path, body) in [
            ("PUT", format!("/{BUCKET}"), Vec::new()),
            ("PUT", format!("/{BUCKET}/{key}"), payload.to_vec()),
        ] {
            let host = format!("s3.{REGION}.amazonaws.com");
            let request = ::http::Request::builder()
                .method(method)
                .uri(format!("https://{host}{path}"))
                .header("host", host)
                .header("authorization", Self::authorization("s3"))
                .body(bytes::Bytes::from(body))
                .expect("the request is well formed");
            let status = self.router.dispatch(request).await.status().as_u16();
            assert!(
                status == 200 || status == 409,
                "case {case:?}: S3 {method} {path} must be served, got {status}"
            );
        }
    }
}

/// A trust policy admitting `principal` and nothing else, with none of `&`, `=` or `+` in it.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// The revision bundle of one leg: a `files` section copying its payload to [`DESTINATION`]
/// and a hook that records the lifecycle event it ran for, plus — where the leg is the one
/// that must FAIL — a hook script that exits non-zero.
fn revision(marker: &str, failing: bool) -> Vec<u8> {
    let mut hooks: Vec<(&str, &str)> = vec![("AfterInstall", "scripts/after.sh")];
    if failing {
        hooks.push(("ApplicationStart", "scripts/start.sh"));
    }
    let document = app_spec(&[("payload.txt", DESTINATION)], &hooks);
    let mut entries: Vec<(&str, &str)> = vec![
        ("appspec.yml", document.as_str()),
        ("payload.txt", marker),
        (
            "scripts/after.sh",
            "#!/bin/bash\nset -e\nmkdir -p /opt/rollback-read-hooks\n\
             echo \"$LIFECYCLE_EVENT\" >> /opt/rollback-read-hooks/ran\n",
        ),
    ];
    if failing {
        entries.push(("scripts/start.sh", "#!/bin/bash\nexit 7\n"));
    }
    bundle(&entries)
}

/// Look again until Amazon EC2 reports `instance` running.
async fn when_running(estate: &Estate, case: &str, instance: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = estate
            .form(
                case,
                "ec2",
                "DescribeInstances",
                EC2_API_VERSION,
                &[("InstanceId.1", instance)],
            )
            .await;
        if described.contains("<name>running</name>") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {instance} to reach running, last read {described}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Look again until Systems Manager reports `instance` as a managed node whose agent is
/// responding, which is when a command can be delivered to it.
async fn when_managed(estate: &Estate, case: &str, instance: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let reported = estate
            .ssm(
                case,
                "DescribeInstanceInformation",
                json!({"Filters": [{"Key": "InstanceIds", "Values": [instance]}]}),
            )
            .await;
        let online = reported["InstanceInformationList"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|node| node["PingStatus"].as_str() == Some("Online"));
        if online {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {instance} to become an Online managed node, last read \
             {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Look again until `deployment` holds a terminal status, answering its `deploymentInfo`.
async fn when_terminal(estate: &Estate, case: &str, deployment: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let info = estate
            .codedeploy(case, "GetDeployment", json!({"deploymentId": deployment}))
            .await["deploymentInfo"]
            .clone();
        if matches!(
            info["status"].as_str(),
            Some("Succeeded" | "Failed" | "Stopped")
        ) {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {deployment} never reached an outcome, last read {info:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Run `command` in the guest of `instance` the way a client does — `SendCommand` of the
/// shell document, then `GetCommandInvocation` until the invocation is terminal — and answer
/// that invocation.
async fn run_command(estate: &Estate, case: &str, instance: &str, command: &str) -> Value {
    let sent = estate
        .ssm(
            case,
            "SendCommand",
            json!({
                "DocumentName": SHELL_DOCUMENT,
                "InstanceIds": [instance],
                "Parameters": {"commands": [command]},
            }),
        )
        .await;
    let command_id = sent["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: SendCommand reports a CommandId: {sent:?}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let invocation = estate
            .ssm(
                case,
                "GetCommandInvocation",
                json!({"CommandId": command_id, "InstanceId": instance}),
            )
            .await;
        if !matches!(
            invocation["Status"].as_str(),
            Some("Pending" | "InProgress" | "Delayed")
        ) {
            return invocation;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the invocation of {command:?} on {instance} never finished, last \
             read {invocation:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What `cat` of the installed payload reports on the host, with the lines stripped.
async fn installed_payload(estate: &Estate, case: &str, instance: &str) -> String {
    let invocation = run_command(
        estate,
        case,
        instance,
        &format!("cat {DESTINATION}/payload.txt"),
    )
    .await;
    assert_eq!(
        invocation["Status"].as_str(),
        Some("Success"),
        "case {case:?}: reading the installed payload off {instance} must succeed, got \
         {invocation:?}"
    );
    invocation["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// RULE: see the module docs — the revision a deployment reports as deployed is on the host
/// for the agent in its guest to read, and the rollback of a failed deployment puts the
/// previous revision's own file back there.
#[tokio::test]
async fn the_installed_revision_and_the_one_a_rollback_reinstalls_are_readable_through_run_command()
{
    let estate = Estate::new();
    let case = "a rollback after a failed deployment, read through Run Command";

    // The instance profile the host runs as: a managed node registers as the role IAM holds
    // for it, so the agent can be sent a command.
    let trust = trust_policy(EC2_PRINCIPAL);
    for (action, params) in [
        (
            "CreateRole",
            vec![("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust[..])],
        ),
        (
            "AttachRolePolicy",
            vec![("RoleName", ROLE), ("PolicyArn", SSM_CORE_POLICY)],
        ),
        ("CreateInstanceProfile", vec![("InstanceProfileName", PROFILE)]),
        (
            "AddRoleToInstanceProfile",
            vec![("InstanceProfileName", PROFILE), ("RoleName", ROLE)],
        ),
    ] {
        estate
            .form(case, "iam", action, IAM_API_VERSION, &params)
            .await;
    }

    // The host: one tagged instance running as that profile.
    let launched = estate
        .form(
            case,
            "ec2",
            "RunInstances",
            EC2_API_VERSION,
            &[
                ("ImageId", AMI),
                ("InstanceType", "t3.micro"),
                ("MinCount", "1"),
                ("MaxCount", "1"),
                ("IamInstanceProfile.Name", PROFILE),
                ("TagSpecification.1.ResourceType", "instance"),
                ("TagSpecification.1.Tag.1.Key", FLEET_TAG.0),
                ("TagSpecification.1.Tag.1.Value", FLEET_TAG.1),
            ],
        )
        .await;
    let host = element(&launched, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an id: {launched}"));
    when_running(&estate, case, &host).await;
    when_managed(&estate, case, &host).await;

    // The two revisions, each under its own key: the rollback redeploys the first one's
    // object, so the second one cannot be allowed to replace it.
    estate
        .upload(case, "revision-a.zip", &revision(MARKER_A, false))
        .await;
    estate
        .upload(case, "revision-b.zip", &revision(MARKER_B, true))
        .await;

    // The application and a deployment group that selects the host and rolls back on a
    // deployment failure.
    estate
        .codedeploy(
            case,
            "CreateApplication",
            json!({"applicationName": "rollback-read", "computePlatform": "Server"}),
        )
        .await;
    estate
        .codedeploy(
            case,
            "CreateDeploymentGroup",
            json!({
                "applicationName": "rollback-read",
                "deploymentGroupName": "fleet",
                "serviceRoleArn": SERVICE_ROLE,
                "ec2TagFilters": [
                    {"Key": FLEET_TAG.0, "Value": FLEET_TAG.1, "Type": "KEY_AND_VALUE"},
                ],
                "autoRollbackConfiguration": {
                    "enabled": true,
                    "events": ["DEPLOYMENT_FAILURE"],
                },
            }),
        )
        .await;

    // The first deployment succeeds, and its payload is on the host for the agent to read.
    let first = estate
        .codedeploy(
            case,
            "CreateDeployment",
            json!({
                "applicationName": "rollback-read",
                "deploymentGroupName": "fleet",
                "revision": {
                    "revisionType": "S3",
                    "s3Location": {
                        "bucket": BUCKET,
                        "key": "revision-a.zip",
                        "bundleType": "zip",
                    },
                },
                "fileExistsBehavior": "OVERWRITE",
            }),
        )
        .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let info = when_terminal(&estate, case, &first).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the first deployment must succeed, got {info:?}"
    );
    let read = installed_payload(&estate, case, &host).await;
    assert_eq!(
        read, MARKER_A,
        "case {case:?}: the agent in the guest must read the revision this deployment \
         installed, got {read:?}"
    );

    // The second fails on the host, and the rollback it triggers reinstalls the first one.
    let second = estate
        .codedeploy(
            case,
            "CreateDeployment",
            json!({
                "applicationName": "rollback-read",
                "deploymentGroupName": "fleet",
                "revision": {
                    "revisionType": "S3",
                    "s3Location": {
                        "bucket": BUCKET,
                        "key": "revision-b.zip",
                        "bundleType": "zip",
                    },
                },
                "fileExistsBehavior": "OVERWRITE",
            }),
        )
        .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let info = when_terminal(&estate, case, &second).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Failed"),
        "case {case:?}: the second deployment must fail on its host, got {info:?}"
    );
    let rollback = info["rollbackInfo"]["rollbackDeploymentId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a rollback deployment must be reported: {info:?}")
        })
        .to_string();
    let rolled_back = when_terminal(&estate, case, &rollback).await;
    assert_eq!(
        rolled_back["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the rollback deployment must succeed, got {rolled_back:?}"
    );
    let read = installed_payload(&estate, case, &host).await;
    assert_eq!(
        read, MARKER_A,
        "case {case:?}: the rollback must put the previous revision's own payload back on \
         the host, got {read:?}"
    );
}
