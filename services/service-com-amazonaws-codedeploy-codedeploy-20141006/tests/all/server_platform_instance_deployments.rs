// SPDX-License-Identifier: CC0-1.0

//! What an EC2/on-premises (`Server`) deployment DOES, driven over the wire.
//!
//! RULE under test: a deployment of that compute platform deploys to the Amazon EC2
//! instances its deployment group's tag selectors name, and what it does on each of
//! them is the CodeDeploy agent's work — it takes the host through the lifecycle
//! events of an in-place deployment in order (ApplicationStop, DownloadBundle,
//! BeforeInstall, Install, AfterInstall, ApplicationStart, ValidateService), copying
//! the files its revision's AppSpec `files` section declares to their destinations
//! and running the script its `hooks` section names for each event in the
//! deployment's own environment, and the instance target it reports ends as that run
//! ended. A failed script fails the target and skips the events after it; a
//! destination the host already holds and the group's last deployment did not put
//! there is what `fileExistsBehavior` decides; the ApplicationStop of a REDEPLOY is
//! the previously deployed revision's script; and a deployment whose group selects
//! no instance fails with `NO_INSTANCES`.
//!
//! Every call goes through the front door a client uses: an `ec2Query` form POST for
//! every Amazon EC2 call, S3's `restXml` API for the revision's bundle, and
//! CodeDeploy's awsJson envelope for the application, the group, the deployment and
//! its targets — all three mounted on ONE shared engine, as the assembled router
//! mounts them.
//!
//! What a host HOLDS afterwards is read from the service that owns the guest, which
//! is where that storage lives (`ComAmazonawsEc2Amazonec2State::guest_files`, the
//! very map the instance's own boot script writes): the file the deployment
//! installed is on the instance, not in a CodeDeploy record.
//!
//! Nothing here waits a fixed time: every span is a stored due instant, so a case
//! LOOKS AGAIN until the condition holds or [`BUDGET`] passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: a deployment selects the instances
/// of the Region it runs in, and the launches use the same one.
const REGION: &str = "us-east-1";

/// The EC2 API version an `ec2Query` body carries beside its `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the launches below run from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// The service role every deployment group here is created with; CodeDeploy only
/// stores it, and no IAM store is mounted beside these cases.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The bucket every revision below is uploaded to.
const BUCKET: &str = "codedeploy-revisions";

/// How long a poll waits for a condition another component's clock owes — an
/// instance reaching `running`, a deployment reaching its outcome. Generous by
/// design: it is a deadline, not a measurement, and nothing below asserts how long
/// anything took.
const BUDGET: Duration = Duration::from_secs(90);

/// How long one look-again cycle pauses before polling again.
const POLL: Duration = Duration::from_millis(25);

/// The lifecycle events an in-place EC2/on-premises deployment runs, in order, as
/// the AWS CodeDeploy User Guide's "Lifecycle event hook availability" lists them
/// for a deployment with no load balancer. Spelled out here so a change to the
/// sequence fails this suite.
const LIFECYCLE_EVENTS: &[&str] = &[
    "ApplicationStop",
    "DownloadBundle",
    "BeforeInstall",
    "Install",
    "AfterInstall",
    "ApplicationStart",
    "ValidateService",
];

/// One emulator carrying CodeDeploy, Amazon EC2 and Amazon S3 over ONE shared
/// engine, plus that engine: the deployment reads the instances and writes their
/// guests through the stores that live in it.
struct Fleet {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Fleet {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing; the SigV4 credential scope is what routes each
        // request and names the Region it is served in.
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
        Self { router, engine }
    }

    /// A SigV4 `Authorization` header whose credential scope names `signing_name`.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
             /aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// Call one CodeDeploy `operation`, answering its HTTP status and parsed body.
    async fn call(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("codedeploy.{REGION}.amazonaws.com"))
            .header("x-amz-target", format!("CodeDeploy_20141006.{operation}"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("authorization", Self::authorization("codedeploy"))
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

    /// Call one CodeDeploy `operation` that must succeed, answering its body.
    async fn ok(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must succeed, got {status} with {parsed:?}"
        );
        parsed
    }

    /// POST one `ec2Query` action that must succeed, answered with its XML body.
    async fn ec2(&self, case: &str, action: &str, params: &[(String, String)]) -> String {
        let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("ec2.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("ec2"))
            .body(bytes::Bytes::from(body))
            .expect("the request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: EC2 {action} must succeed, got {status} with {answered}"
        );
        answered
    }

    /// One Amazon S3 request through the same front door; S3's model declares
    /// `restXml` with path-style addressing, so the bucket and the key ride the URI.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> u16 {
        let host = format!("s3.{REGION}.amazonaws.com");
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("authorization", Self::authorization("s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the request is well formed");
        self.router.dispatch(request).await.status().as_u16()
    }

    /// Upload `bundle` at `key`, creating the bucket the first time, through S3's
    /// own API — the way a client uploads a revision.
    async fn upload(&self, case: &str, key: &str, bundle: &[u8]) {
        let created = self.s3("PUT", &format!("/{BUCKET}"), b"").await;
        assert!(
            created == 200 || created == 409,
            "case {case:?}: CreateBucket must succeed or report the bucket, got {created}"
        );
        let written = self.s3("PUT", &format!("/{BUCKET}/{key}"), bundle).await;
        assert_eq!(
            written, 200,
            "case {case:?}: PutObject {key} must succeed, got {written}"
        );
    }

    /// Launch ONE instance carrying `tags`, with `user_data` as its boot program
    /// when the case gives it one, through EC2's own `RunInstances`; answers the
    /// instance id.
    async fn launch(&self, case: &str, tags: &[(&str, &str)], user_data: Option<&str>) -> String {
        let mut params = vec![
            param("ImageId", AMI),
            param("InstanceType", "t3.micro"),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ];
        if let Some(user_data) = user_data {
            // "The user data to make available to the instance", which the API
            // carries base64-encoded.
            params.push(param(
                "UserData",
                &::base64::Engine::encode(
                    &::base64::engine::general_purpose::STANDARD,
                    user_data,
                ),
            ));
        }
        if !tags.is_empty() {
            params.push(param("TagSpecification.1.ResourceType", "instance"));
            for (position, (key, value)) in tags.iter().enumerate() {
                let index = position + 1;
                params.push(param(&format!("TagSpecification.1.Tag.{index}.Key"), key));
                params.push(param(
                    &format!("TagSpecification.1.Tag.{index}.Value"),
                    value,
                ));
            }
        }
        let launched = self.ec2(case, "RunInstances", &params).await;
        element(&launched, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an id: {launched}"))
    }

    /// Look again until EC2 reports `instance_id` running, which is when it is a
    /// host a deployment can go to.
    async fn wait_until_running(&self, case: &str, instance_id: &str) {
        let deadline = Instant::now() + BUDGET;
        loop {
            let described = self
                .ec2(
                    case,
                    "DescribeInstances",
                    &[param("InstanceId.1", instance_id)],
                )
                .await;
            if described.contains("<name>running</name>") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {instance_id} to reach running, got {described}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// What the guest of `instance_id` holds at `path`, read out of the service that
    /// owns that storage.
    fn guest_file(&self, instance_id: &str, path: &str) -> Option<String> {
        let state = ::service_com_amazonaws_ec2_amazonec2::state::service_state(&*self.engine)
            .expect("the instance store is on this engine");
        let store = state.lock();
        store.read_in_instance_guest(instance_id, path)
    }

    /// An application and a deployment group of it carrying `selectors`, answering
    /// `(application name, group name)`.
    async fn group(&self, case: &str, name: &str, selectors: Value) -> (String, String) {
        let application = format!("app-{name}");
        let group = format!("dg-{name}");
        self.ok(
            case,
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": "Server"}),
        )
        .await;
        let mut request = json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "serviceRoleArn": SERVICE_ROLE,
        });
        for (member, value) in selectors.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        self.ok(case, "CreateDeploymentGroup", request).await;
        (application, group)
    }

    /// Create a deployment of the revision at `key` and answer its id; `extras`
    /// carries the members the case sets beside the revision.
    async fn deploy(
        &self,
        case: &str,
        application: &str,
        group: &str,
        key: &str,
        extras: Value,
    ) -> String {
        let mut request = json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "revision": {
                "revisionType": "S3",
                "s3Location": {"bucket": BUCKET, "key": key, "bundleType": "zip"},
            },
        });
        for (member, value) in extras.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        let created = self.ok(case, "CreateDeployment", request).await;
        created["deploymentId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateDeployment reports an id: {created}"))
            .to_string()
    }

    /// Look again until `deployment_id` holds a terminal status, answering its
    /// `deploymentInfo`.
    async fn terminal(&self, case: &str, deployment_id: &str) -> Value {
        let deadline = Instant::now() + BUDGET;
        loop {
            let info = self
                .ok(case, "GetDeployment", json!({"deploymentId": deployment_id}))
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
                "case {case:?}: the deployment never reached an outcome, last read {info:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// The ids of the targets `deployment_id` reports, in the order it lists them.
    async fn target_ids(&self, case: &str, deployment_id: &str) -> Vec<String> {
        let listed = self
            .ok(
                case,
                "ListDeploymentTargets",
                json!({"deploymentId": deployment_id}),
            )
            .await;
        listed["targetIds"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `instanceTarget` `deployment_id` reports for `instance_id`.
    async fn instance_target(&self, case: &str, deployment_id: &str, instance_id: &str) -> Value {
        self.ok(
            case,
            "GetDeploymentTarget",
            json!({"deploymentId": deployment_id, "targetId": instance_id}),
        )
        .await["deploymentTarget"]["instanceTarget"]
            .clone()
    }

    /// The id CodeDeploy minted for the deployment group `group`, which is the
    /// `DEPLOYMENT_GROUP_ID` its deployments' hooks read.
    async fn group_id(&self, case: &str, application: &str, group: &str) -> String {
        self.ok(
            case,
            "GetDeploymentGroup",
            json!({"applicationName": application, "deploymentGroupName": group}),
        )
        .await["deploymentGroupInfo"]["deploymentGroupId"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

/// One form field of an `ec2Query` body.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// Percent-encode one form field the way an AWS SDK encodes an `ec2Query` body.
fn form_encode(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<tag>` of an XML body, if it carries one.
fn element(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// The `(name, status)` of each lifecycle event a target reports, in the order it
/// reports them.
fn events_of(target: &Value) -> Vec<(String, String)> {
    target["lifecycleEvents"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    (
                        event["lifecycleEventName"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        event["status"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The lifecycle event `name` of a target, if it reports one.
fn event_named(target: &Value, name: &str) -> Option<Value> {
    target["lifecycleEvents"]
        .as_array()?
        .iter()
        .find(|event| event["lifecycleEventName"].as_str() == Some(name))
        .cloned()
}

/// An AppSpec document for the `Server` platform: its `files` entries as
/// `(source, destination)` and its `hooks` entries as `(lifecycle event, script
/// location)`, in the form the AWS CodeDeploy User Guide's AppSpec reference writes
/// them.
fn app_spec(files: &[(&str, &str)], hooks: &[(&str, &str)]) -> String {
    let mut document = String::from("version: 0.0\nos: linux\n");
    if !files.is_empty() {
        document.push_str("files:\n");
        for (source, destination) in files {
            document.push_str(&format!(
                "  - source: {source}\n    destination: {destination}\n"
            ));
        }
    }
    if !hooks.is_empty() {
        document.push_str("hooks:\n");
        for (event, location) in hooks {
            document.push_str(&format!(
                "  {event}:\n    - location: {location}\n      timeout: 180\n"
            ));
        }
    }
    document
}

/// A zip revision bundle of `entries`, the way a client packages a revision.
fn bundle(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = ::zip::ZipWriter::new(::std::io::Cursor::new(Vec::new()));
    let options: ::zip::write::FileOptions<'_, '_, ()> = ::zip::write::FileOptions::default();
    for (path, contents) in entries {
        writer
            .start_file(*path, options)
            .expect("the entry is well formed");
        ::std::io::Write::write_all(&mut writer, contents.as_bytes())
            .expect("the entry is written");
    }
    writer
        .finish()
        .expect("the archive is finished")
        .into_inner()
}

/// The hook script every row below runs for ApplicationStart: it writes the
/// deployment's own environment variables where the case reads them back off the
/// host, which is what proves the agent handed the hook this deployment's
/// identifiers ("CodeDeploy makes the following environment variables available
/// during each deployment lifecycle event").
///
/// Written the way a hook script writes a file out of several lines — one brace
/// group whose standard output is redirected once — beside the one-line form, so a
/// case covers both shapes a client's hook takes.
const ENVIRONMENT_SCRIPT: &str = "#!/bin/bash\n\
     set -eo pipefail\n\
     mkdir -p /tmp/hook\n\
     {\n\
     \x20 printf '%s=%s\\n' 'DEPLOYMENT_ID' \"$DEPLOYMENT_ID\"\n\
     \x20 printf '%s=%s\\n' 'APPLICATION_NAME' \"$APPLICATION_NAME\"\n\
     \x20 printf '%s=%s\\n' 'DEPLOYMENT_GROUP_NAME' \"$DEPLOYMENT_GROUP_NAME\"\n\
     \x20 printf '%s=%s\\n' 'DEPLOYMENT_GROUP_ID' \"$DEPLOYMENT_GROUP_ID\"\n\
     } > /tmp/hook/environment\n\
     echo $LIFECYCLE_EVENT > /tmp/hook-lifecycle-event\n";

/// RULE: a `Server` deployment deploys to the instances its group's selectors name,
/// takes each of them through the lifecycle events in order, installs the files its
/// AppSpec declares on the host and runs its hook scripts there with the
/// deployment's own environment; a group that selects no instance fails the
/// deployment with `NO_INSTANCES`.
///
/// The rows differ along WHAT NAMES the fleet — a key-and-value tag filter, a tag
/// set of two groups, a key-only filter that names two hosts, and a filter that
/// names none — which is what the rule generalises over.
#[tokio::test]
async fn a_server_deployment_installs_its_revision_on_the_instances_its_group_selects() {
    let fleet = Fleet::new();
    // Two hosts: one carrying both tags, one carrying the same tag KEY with another
    // value, so a key-only filter names both and a key-and-value filter names one.
    let web = fleet
        .launch("setup", &[("Name", "web"), ("Stage", "prod")], None)
        .await;
    let api = fleet.launch("setup", &[("Name", "api")], None).await;
    fleet.wait_until_running("setup", &web).await;
    fleet.wait_until_running("setup", &api).await;

    /// One row: how the group names its fleet, and which hosts that names.
    struct Row {
        case: &'static str,
        selectors: Value,
        /// The instances the deployment must go to, or none for a row whose group
        /// selects nothing.
        expected: Vec<String>,
    }
    let rows: [Row; 4] = [
        Row {
            case: "a key-and-value tag filter",
            selectors: json!({
                "ec2TagFilters": [{"Key": "Name", "Value": "web", "Type": "KEY_AND_VALUE"}],
            }),
            expected: vec![web.clone()],
        },
        Row {
            case: "a tag set whose two groups both have to match",
            selectors: json!({
                "ec2TagSet": {"ec2TagSetList": [
                    [{"Key": "Name", "Value": "web", "Type": "KEY_AND_VALUE"}],
                    [{"Key": "Stage", "Value": "prod", "Type": "KEY_AND_VALUE"}],
                ]},
            }),
            expected: vec![web.clone()],
        },
        Row {
            case: "a key-only tag filter naming two hosts",
            selectors: json!({
                "ec2TagFilters": [{"Key": "Name", "Type": "KEY_ONLY"}],
            }),
            expected: {
                let mut both = vec![web.clone(), api.clone()];
                both.sort();
                both
            },
        },
        Row {
            case: "a tag filter naming no host at all",
            selectors: json!({
                "ec2TagFilters": [{"Key": "Name", "Value": "absent", "Type": "KEY_AND_VALUE"}],
            }),
            expected: Vec::new(),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("select-{index}");
        // Each row installs into a directory of its OWN: a destination another
        // deployment group's deployment put there is a file this one did not, which
        // is what `fileExistsBehavior` is about and what the table below pins.
        let destination = format!("/var/www/{name}");
        let key = format!("{name}.zip");
        fleet
            .upload(
                case,
                &key,
                &bundle(&[
                    (
                        "appspec.yml",
                        &app_spec(
                            &[("site/index.html", &destination)],
                            &[("ApplicationStart", "scripts/start.sh")],
                        ),
                    ),
                    ("site/index.html", &format!("<p>{name}</p>")),
                    ("scripts/start.sh", ENVIRONMENT_SCRIPT),
                ]),
            )
            .await;
        let (application, group) = fleet.group(case, &name, row.selectors.clone()).await;
        let deployment = fleet.deploy(case, &application, &group, &key, json!({})).await;
        let info = fleet.terminal(case, &deployment).await;

        if row.expected.is_empty() {
            assert_eq!(
                info["status"].as_str(),
                Some("Failed"),
                "case {case:?}: a group that selects no instance must fail the deployment, got \
                 {info:?}"
            );
            assert_eq!(
                info["errorInformation"]["code"].as_str(),
                Some("NO_INSTANCES"),
                "case {case:?}: expected the NO_INSTANCES code, got {:?}",
                info["errorInformation"]
            );
            continue;
        }

        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: expected the deployment to succeed, got {info:?}"
        );
        let mut reported = fleet.target_ids(case, &deployment).await;
        reported.sort();
        assert_eq!(
            reported, row.expected,
            "case {case:?}: expected the targets {:?}, got {reported:?}",
            row.expected
        );
        // The overview counts the deployment's OWN targets under the status each of
        // them reported, so a fleet of two hosts is summarised as two.
        let counted = i64::try_from(row.expected.len()).expect("the fleet is small");
        assert_eq!(
            info["deploymentOverview"]["Succeeded"].as_i64(),
            Some(counted),
            "case {case:?}: expected {counted} succeeded in the overview, got {:?}",
            info["deploymentOverview"]
        );
        for instance in &row.expected {
            let target = fleet.instance_target(case, &deployment, instance).await;
            assert_eq!(
                target["status"].as_str(),
                Some("Succeeded"),
                "case {case:?}: expected {instance} to succeed, got {target:?}"
            );
            assert_eq!(
                target["targetArn"].as_str(),
                Some(
                    format!(
                        "arn:aws:ec2:{REGION}:{}:instance/{instance}",
                        ::engine::emulator_account_id()
                    )
                    .as_str()
                ),
                "case {case:?}: expected {instance}'s own ARN, got {target:?}"
            );
            let events = events_of(&target);
            let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(
                names, LIFECYCLE_EVENTS,
                "case {case:?}: expected the lifecycle events in order, got {names:?}"
            );
            for (name, status) in &events {
                // Nothing had been deployed to this host for this group before, so
                // there was no previous revision to stop — and the event still
                // completed, having had nothing to do.
                assert_eq!(
                    status, "Succeeded",
                    "case {case:?}: expected {name} to have completed on {instance}, got \
                     {status}"
                );
            }
            assert_eq!(
                fleet
                    .guest_file(instance, &format!("{destination}/index.html"))
                    .as_deref(),
                Some(format!("<p>{name}</p>").as_str()),
                "case {case:?}: expected the revision's file installed on {instance}"
            );
            // The hook ran on the host, in the environment CodeDeploy publishes to
            // it: every value below is this deployment's own. The four the brace
            // group wrote are one file, and the one the line after it wrote is its
            // own, so both shapes of hook write are read back.
            let group_id = fleet.group_id(case, &application, &group).await;
            let environment = fleet.guest_file(instance, "/tmp/hook/environment");
            let expected = format!(
                "DEPLOYMENT_ID={deployment}\nAPPLICATION_NAME={application}\n\
                 DEPLOYMENT_GROUP_NAME={group}\nDEPLOYMENT_GROUP_ID={group_id}\n"
            );
            assert_eq!(
                environment.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected the hook's environment file on {instance} to carry \
                 {expected:?}, got {environment:?}"
            );
            let event = fleet.guest_file(instance, "/tmp/hook-lifecycle-event");
            assert_eq!(
                event.as_deref().map(str::trim),
                Some("ApplicationStart"),
                "case {case:?}: expected the lifecycle event the hook ran for, got {event:?}"
            );
        }
    }
}

/// RULE: the ApplicationStop of a REDEPLOY runs the previously deployed revision's
/// script, which is the one the host still holds — so the first deployment to a host
/// skips that event and the next one runs the script of the revision it replaces.
#[tokio::test]
async fn a_redeploy_runs_the_previous_revisions_application_stop_script() {
    let fleet = Fleet::new();
    let case = "a redeploy of a host the group already deployed to";
    let host = fleet
        .launch(case, &[("Name", "redeploy-host")], None)
        .await;
    fleet.wait_until_running(case, &host).await;
    let selectors = json!({
        "ec2TagFilters": [{"Key": "Name", "Value": "redeploy-host", "Type": "KEY_AND_VALUE"}],
    });
    let (application, group) = fleet.group(case, "redeploy", selectors).await;

    // Revision A declares an ApplicationStop script that leaves a mark on the host;
    // revision B declares none at all, so a mark after B's deployment can only have
    // been left by A's script.
    let stop_script = "#!/bin/bash\necho stopped-by-a > /tmp/application-stop\n";
    let first = app_spec(
        &[("site/index.html", "/var/www/redeploy")],
        &[("ApplicationStop", "scripts/stop.sh")],
    );
    fleet
        .upload(
            case,
            "revision-a.zip",
            &bundle(&[
                ("appspec.yml", &first),
                ("site/index.html", "<p>A</p>"),
                ("scripts/stop.sh", stop_script),
            ]),
        )
        .await;
    let second = app_spec(&[("site/index.html", "/var/www/redeploy")], &[]);
    fleet
        .upload(
            case,
            "revision-b.zip",
            &bundle(&[("appspec.yml", &second), ("site/index.html", "<p>B</p>")]),
        )
        .await;

    let one = fleet
        .deploy(case, &application, &group, "revision-a.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &one).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the first deployment must succeed, got {info:?}"
    );
    let target = fleet.instance_target(case, &one, &host).await;
    assert_eq!(
        event_named(&target, "ApplicationStop")
            .and_then(|event| event["status"].as_str().map(str::to_string)),
        Some("Succeeded".to_string()),
        "case {case:?}: the first deployment to a host has no revision to stop, got {target:?}"
    );
    assert_eq!(
        fleet.guest_file(&host, "/tmp/application-stop"),
        None,
        "case {case:?}: the first deployment must not run the stop script of a revision \
         nothing deployed"
    );

    // The redeploy: revision B declares no ApplicationStop, so the event runs A's.
    let two = fleet
        .deploy(case, &application, &group, "revision-b.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &two).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the redeploy must succeed, got {info:?}"
    );
    let target = fleet.instance_target(case, &two, &host).await;
    assert_eq!(
        event_named(&target, "ApplicationStop")
            .and_then(|event| event["status"].as_str().map(str::to_string)),
        Some("Succeeded".to_string()),
        "case {case:?}: the redeploy runs the previous revision's stop script, got {target:?}"
    );
    assert_eq!(
        fleet
            .guest_file(&host, "/tmp/application-stop")
            .as_deref()
            .map(str::trim),
        Some("stopped-by-a"),
        "case {case:?}: expected the mark the PREVIOUS revision's script leaves"
    );
    assert_eq!(
        fleet
            .guest_file(&host, "/var/www/redeploy/index.html")
            .as_deref(),
        Some("<p>B</p>"),
        "case {case:?}: expected the redeployed revision's file on the host"
    );
}

/// RULE: a hook script that fails fails the lifecycle event it ran for, skips every
/// event after it, fails the instance target and fails the deployment — and the
/// revision's files are not installed by an event the deployment never reached.
#[tokio::test]
async fn a_failing_hook_fails_the_target_and_skips_the_events_after_it() {
    let fleet = Fleet::new();
    let case = "a BeforeInstall script that exits non-zero";
    let host = fleet.launch(case, &[("Name", "hook-failure")], None).await;
    fleet.wait_until_running(case, &host).await;
    let selectors = json!({
        "ec2TagFilters": [{"Key": "Name", "Value": "hook-failure", "Type": "KEY_AND_VALUE"}],
    });
    let (application, group) = fleet.group(case, "hook-failure", selectors).await;
    fleet
        .upload(
            case,
            "failing.zip",
            &bundle(&[
                (
                    "appspec.yml",
                    &app_spec(
                        &[("site/index.html", "/var/www/failing")],
                        &[("BeforeInstall", "scripts/before.sh")],
                    ),
                ),
                ("site/index.html", "<p>never installed</p>"),
                ("scripts/before.sh", "#!/bin/bash\nexit 1\n"),
            ]),
        )
        .await;
    let deployment = fleet
        .deploy(case, &application, &group, "failing.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &deployment).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Failed"),
        "case {case:?}: expected the deployment to fail, got {info:?}"
    );
    assert_eq!(
        info["errorInformation"]["code"].as_str(),
        Some("HEALTH_CONSTRAINTS"),
        "case {case:?}: expected the health-constraints code, got {:?}",
        info["errorInformation"]
    );
    let target = fleet.instance_target(case, &deployment, &host).await;
    assert_eq!(
        target["status"].as_str(),
        Some("Failed"),
        "case {case:?}: expected the instance target to fail, got {target:?}"
    );
    assert_eq!(
        info["deploymentOverview"]["Failed"].as_i64(),
        Some(1),
        "case {case:?}: expected the failed host counted in the overview, got {:?}",
        info["deploymentOverview"]
    );
    let events = events_of(&target);
    // (the event, the status it must hold): the events before the failure ran, the
    // one that failed carries the failure, and the ones after it never ran.
    let expected: &[(&str, &str)] = &[
        ("ApplicationStop", "Succeeded"),
        ("DownloadBundle", "Succeeded"),
        ("BeforeInstall", "Failed"),
        ("Install", "Skipped"),
        ("AfterInstall", "Skipped"),
        ("ApplicationStart", "Skipped"),
        ("ValidateService", "Skipped"),
    ];
    for (name, status) in expected {
        let held = events
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, status)| status.clone());
        assert_eq!(
            held.as_deref(),
            Some(*status),
            "case {case:?}: expected {name} to be {status}, got {held:?} in {events:?}"
        );
    }
    let failed = event_named(&target, "BeforeInstall")
        .unwrap_or_else(|| panic!("case {case:?}: the failed event is reported"));
    assert_eq!(
        failed["diagnostics"]["errorCode"].as_str(),
        Some("ScriptFailed"),
        "case {case:?}: expected the ScriptFailed error code, got {failed:?}"
    );
    assert_eq!(
        failed["diagnostics"]["scriptName"].as_str(),
        Some("scripts/before.sh"),
        "case {case:?}: expected the script the AppSpec named, got {failed:?}"
    );
    assert_eq!(
        failed["diagnostics"]["message"].as_str(),
        Some("Script at specified location: scripts/before.sh failed with exit code 1"),
        "case {case:?}: expected the agent's own sentence, got {failed:?}"
    );
    assert_eq!(
        fleet.guest_file(&host, "/var/www/failing/index.html"),
        None,
        "case {case:?}: an Install the deployment never reached installs nothing"
    );
}

/// RULE: `fileExistsBehavior` decides what a deployment does with a destination the
/// host already holds and the group's last deployment did not put there — DISALLOW
/// fails the deployment, OVERWRITE replaces the file, RETAIN keeps it — and the
/// instance's own file says which of the three happened.
#[tokio::test]
async fn file_exists_behavior_decides_what_happens_to_a_file_already_on_the_host() {
    let fleet = Fleet::new();
    /// One row: the `fileExistsBehavior` the deployment states, the outcome it
    /// reaches, and what the host holds at the destination afterwards.
    struct Row {
        case: &'static str,
        behavior: Option<&'static str>,
        status: &'static str,
        error_code: Option<&'static str>,
        install: &'static str,
        held: &'static str,
    }
    // The file the host already holds, written by its own boot program — a file no
    // deployment of the group put there, which is exactly what the member is about.
    const FOREIGN: &str = "<p>foreign</p>";
    const REVISION: &str = "<p>revision</p>";
    let rows: &[Row] = &[
        Row {
            case: "a deployment stating no behavior at all",
            behavior: None,
            status: "Failed",
            error_code: Some("HEALTH_CONSTRAINTS"),
            install: "Failed",
            held: FOREIGN,
        },
        Row {
            case: "DISALLOW",
            behavior: Some("DISALLOW"),
            status: "Failed",
            error_code: Some("HEALTH_CONSTRAINTS"),
            install: "Failed",
            held: FOREIGN,
        },
        Row {
            case: "OVERWRITE",
            behavior: Some("OVERWRITE"),
            status: "Succeeded",
            error_code: None,
            install: "Succeeded",
            held: REVISION,
        },
        Row {
            case: "RETAIN",
            behavior: Some("RETAIN"),
            status: "Succeeded",
            error_code: None,
            install: "Succeeded",
            held: FOREIGN,
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("exists-{index}");
        let tag = format!("exists-{index}");
        let destination = "/var/www/exists";
        // The host comes up with the foreign file already on it, put there by its
        // own user-data program.
        let host = fleet
            .launch(
                case,
                &[("Name", &tag)],
                Some(&format!(
                    "#!/bin/bash\nmkdir -p {destination}\necho '{FOREIGN}' > \
                     {destination}/index.html\n"
                )),
            )
            .await;
        fleet.wait_until_running(case, &host).await;
        let deadline = Instant::now() + BUDGET;
        loop {
            if fleet
                .guest_file(&host, &format!("{destination}/index.html"))
                .is_some()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the host's own boot program never wrote the file the case \
                 is about"
            );
            tokio::time::sleep(POLL).await;
        }
        let key = format!("{name}.zip");
        fleet
            .upload(
                case,
                &key,
                &bundle(&[
                    (
                        "appspec.yml",
                        &app_spec(&[("site/index.html", destination)], &[]),
                    ),
                    ("site/index.html", REVISION),
                ]),
            )
            .await;
        let selectors = json!({
            "ec2TagFilters": [{"Key": "Name", "Value": tag, "Type": "KEY_AND_VALUE"}],
        });
        let (application, group) = fleet.group(case, &name, selectors).await;
        let extras = match row.behavior {
            Some(behavior) => json!({"fileExistsBehavior": behavior}),
            None => json!({}),
        };
        let deployment = fleet
            .deploy(case, &application, &group, &key, extras)
            .await;
        let info = fleet.terminal(case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some(row.status),
            "case {case:?}: expected the deployment to end {}, got {info:?}",
            row.status
        );
        assert_eq!(
            info["errorInformation"]["code"].as_str(),
            row.error_code,
            "case {case:?}: expected the error code {:?}, got {:?}",
            row.error_code,
            info["errorInformation"]
        );
        let target = fleet.instance_target(case, &deployment, &host).await;
        let install = event_named(&target, "Install")
            .unwrap_or_else(|| panic!("case {case:?}: the Install event is reported"));
        assert_eq!(
            install["status"].as_str(),
            Some(row.install),
            "case {case:?}: expected Install to be {}, got {install:?}",
            row.install
        );
        if row.install == "Failed" {
            assert_eq!(
                install["diagnostics"]["message"].as_str(),
                Some(
                    format!(
                        "The deployment failed because a specified file already exists at this \
                         location: {destination}/index.html"
                    )
                    .as_str()
                ),
                "case {case:?}: expected the agent's own sentence, got {install:?}"
            );
        }
        assert_eq!(
            fleet
                .guest_file(&host, &format!("{destination}/index.html"))
                .as_deref()
                .map(str::trim),
            Some(row.held),
            "case {case:?}: expected the host to hold {:?} afterwards",
            row.held
        );
    }
}

/// RULE: the agent CLEARS the files the group's last deployment installed before it
/// copies the new revision's, so an Install that then fails on a file it may not
/// touch leaves the cleared ones gone rather than putting the old revision back.
#[tokio::test]
async fn an_install_clears_the_last_deployments_files_before_it_copies() {
    let fleet = Fleet::new();
    let case = "an Install refused on one file after clearing another";
    let destination = "/opt/app";
    // The host comes up holding a file of its own under the same destination
    // directory, which no deployment of the group put there.
    let host = fleet
        .launch(
            case,
            &[("Name", "clearing-host")],
            Some(&format!(
                "#!/bin/bash\nmkdir -p {destination}\necho host-own > {destination}/extra.txt\n"
            )),
        )
        .await;
    fleet.wait_until_running(case, &host).await;
    let selectors = json!({
        "ec2TagFilters": [{"Key": "Name", "Value": "clearing-host", "Type": "KEY_AND_VALUE"}],
    });
    let (application, group) = fleet.group(case, "clearing", selectors).await;
    fleet
        .upload(
            case,
            "clearing-a.zip",
            &bundle(&[
                (
                    "appspec.yml",
                    &app_spec(&[("site/app.txt", destination)], &[]),
                ),
                ("site/app.txt", "from-a"),
            ]),
        )
        .await;
    // Revision B declares the file the last deployment installed AND the one the
    // host holds of its own, which is the file it may not touch.
    fleet
        .upload(
            case,
            "clearing-b.zip",
            &bundle(&[
                (
                    "appspec.yml",
                    &app_spec(
                        &[
                            ("site/app.txt", destination),
                            ("site/extra.txt", destination),
                        ],
                        &[],
                    ),
                ),
                ("site/app.txt", "from-b"),
                ("site/extra.txt", "from-b"),
            ]),
        )
        .await;

    let first = fleet
        .deploy(case, &application, &group, "clearing-a.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &first).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the first deployment must succeed, got {info:?}"
    );
    assert_eq!(
        fleet
            .guest_file(&host, &format!("{destination}/app.txt"))
            .as_deref(),
        Some("from-a"),
        "case {case:?}: expected the first revision's file on the host"
    );

    let second = fleet
        .deploy(case, &application, &group, "clearing-b.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &second).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Failed"),
        "case {case:?}: the deployment must fail on the file it may not touch, got {info:?}"
    );
    assert_eq!(
        fleet.guest_file(&host, &format!("{destination}/app.txt")),
        None,
        "case {case:?}: expected the file the last deployment installed to be cleared"
    );
    assert_eq!(
        fleet
            .guest_file(&host, &format!("{destination}/extra.txt"))
            .as_deref()
            .map(str::trim),
        Some("host-own"),
        "case {case:?}: expected the file the deployment may not touch left as it was"
    );
}

/// RULE: a revision whose object this emulator cannot read an AppSpec file out of
/// FAILS the deployment under the `INVALID_REVISION` code rather than reporting a
/// deployment that installed nothing — an object that is no archive it unpacks, a
/// bundle whose root holds no AppSpec file, and an AppSpec that is no document it
/// reads.
#[tokio::test]
async fn a_revision_whose_app_spec_cannot_be_read_fails_the_deployment() {
    let fleet = Fleet::new();
    let host = fleet
        .launch("setup", &[("Name", "unreadable-host")], None)
        .await;
    fleet.wait_until_running("setup", &host).await;
    // The fixed sentence CodeDeploy answers a revision it cannot locate the AppSpec
    // of with; spelled out here so a change to the wording fails this suite.
    const UNLOCATABLE: &str = "The AppSpec file cannot be located in the specified S3 bucket. \
         Verify your AppSpec file is present and that the name and key value pair specified \
         for your S3 bucket are correct. The S3 bucket must be in your current region";
    // (the case, the object the revision names)
    let rows: [(&str, Vec<u8>); 3] = [
        (
            "an object that is no archive this emulator unpacks",
            b"\x7fELF\x02\x01\x01\x00not an archive".to_vec(),
        ),
        (
            "a bundle whose root holds no AppSpec file",
            bundle(&[("revision/appspec.yml", &app_spec(&[], &[]))]),
        ),
        (
            "a bundle whose AppSpec states neither its version nor its os",
            bundle(&[("appspec.yml", "not: an appspec\n")]),
        ),
    ];
    for (index, (case, object)) in rows.iter().enumerate() {
        let name = format!("unreadable-{index}");
        let key = format!("{name}.zip");
        fleet.upload(case, &key, object).await;
        let selectors = json!({
            "ec2TagFilters": [
                {"Key": "Name", "Value": "unreadable-host", "Type": "KEY_AND_VALUE"},
            ],
        });
        let (application, group) = fleet.group(case, &name, selectors).await;
        let deployment = fleet.deploy(case, &application, &group, &key, json!({})).await;
        let info = fleet.terminal(case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Failed"),
            "case {case:?}: expected the deployment to fail, got {info:?}"
        );
        assert_eq!(
            info["errorInformation"]["code"].as_str(),
            Some("INVALID_REVISION"),
            "case {case:?}: expected the INVALID_REVISION code, got {:?}",
            info["errorInformation"]
        );
        assert_eq!(
            info["errorInformation"]["message"].as_str(),
            Some(UNLOCATABLE),
            "case {case:?}: expected CodeDeploy's own sentence, got {:?}",
            info["errorInformation"]
        );
    }
}

/// RULE: a deployment that FAILED on its hosts and whose configuration asks for a
/// rollback on a deployment failure is followed by a rollback deployment of the
/// revision the group was on, which puts that revision's files back on the host.
#[tokio::test]
async fn an_automatic_rollback_reinstalls_the_previous_revision_on_the_host() {
    let fleet = Fleet::new();
    let case = "a failed deployment of a group that rolls back on failure";
    let host = fleet.launch(case, &[("Name", "rollback-host")], None).await;
    fleet.wait_until_running(case, &host).await;
    let selectors = json!({
        "ec2TagFilters": [{"Key": "Name", "Value": "rollback-host", "Type": "KEY_AND_VALUE"}],
        "autoRollbackConfiguration": {"enabled": true, "events": ["DEPLOYMENT_FAILURE"]},
    });
    let (application, group) = fleet.group(case, "rollback", selectors).await;
    let destination = "/var/www/rollback";

    // Revision A installs its page and succeeds; revision B installs its own and
    // then fails a later hook, which is what the rollback undoes.
    fleet
        .upload(
            case,
            "good.zip",
            &bundle(&[
                (
                    "appspec.yml",
                    &app_spec(&[("site/index.html", destination)], &[]),
                ),
                ("site/index.html", "<p>A</p>"),
            ]),
        )
        .await;
    fleet
        .upload(
            case,
            "bad.zip",
            &bundle(&[
                (
                    "appspec.yml",
                    &app_spec(
                        &[("site/index.html", destination)],
                        &[("ApplicationStart", "scripts/start.sh")],
                    ),
                ),
                ("site/index.html", "<p>B</p>"),
                ("scripts/start.sh", "#!/bin/bash\nexit 7\n"),
            ]),
        )
        .await;

    let good = fleet
        .deploy(case, &application, &group, "good.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &good).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the first deployment must succeed, got {info:?}"
    );
    assert_eq!(
        fleet
            .guest_file(&host, &format!("{destination}/index.html"))
            .as_deref(),
        Some("<p>A</p>"),
        "case {case:?}: expected the first revision's page on the host"
    );

    let bad = fleet
        .deploy(
            case,
            &application,
            &group,
            "bad.zip",
            json!({"fileExistsBehavior": "OVERWRITE"}),
        )
        .await;
    let info = fleet.terminal(case, &bad).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Failed"),
        "case {case:?}: the second deployment must fail, got {info:?}"
    );
    let rollback = info["rollbackInfo"]["rollbackDeploymentId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a rollback deployment is reported: {info:?}"))
        .to_string();
    let rolled_back = fleet.terminal(case, &rollback).await;
    assert_eq!(
        rolled_back["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the rollback deployment must succeed, got {rolled_back:?}"
    );
    assert_eq!(
        rolled_back["creator"].as_str(),
        Some("codeDeployRollback"),
        "case {case:?}: the rollback is the service's own deployment, got {rolled_back:?}"
    );
    assert_eq!(
        fleet
            .guest_file(&host, &format!("{destination}/index.html"))
            .as_deref(),
        Some("<p>A</p>"),
        "case {case:?}: expected the rollback to put the previous revision's page back"
    );
}
