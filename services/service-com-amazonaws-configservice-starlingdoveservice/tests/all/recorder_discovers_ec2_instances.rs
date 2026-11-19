// SPDX-License-Identifier: CC0-1.0

//! The Amazon EC2 instances a running configuration recorder discovers.
//!
//! The rule pinned here: while a configuration recorder is recording and its recording
//! group covers a resource type, every resource of that type the account holds in the
//! owning service is admitted to Config's discovered-resource inventory as that service
//! reports it — the resource going away in its own service is recorded as its DELETION —
//! and Config's discovered-resource reads project that inventory however the request
//! narrows it, with no Config write ever naming the resource. The rule is one rule over
//! every observable type; this suite drives it over the type an EC2 instance is recorded
//! under, which no `PutResourceConfig` call below ever mentions.
//!
//! Config and Amazon EC2 are mounted on ONE engine: the recorder reads the instances it
//! records from EC2's own API, so without EC2 mounted there is nothing to discover. Config
//! is driven over the wire the way a client drives it, and EC2 through the AWS SDK over the
//! same in-process callback a handler uses.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The Region every request below is signed for, and the Region the instances are launched
/// in: a configuration recorder records the resources of its own Region.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to.
const ACCOUNT_ID: &str = "000000000000";

/// The resource type Config records an Amazon EC2 instance under — an entry of
/// `com.amazonaws.configservice#ResourceType`, from
/// `smithy_query(query="shape", shape="ResourceType")`.
const INSTANCE_RESOURCE_TYPE: &str = "AWS::EC2::Instance";

/// Another entry of that same enum, for the rows that ask about a type the account holds
/// nothing of and for the recording groups that name a type other than the instance one.
const VOLUME_RESOURCE_TYPE: &str = "AWS::EC2::Volume";

/// The AMI every launch below runs from: an id in the long form EC2 issues AMI ids in, so
/// the launch is accepted on the id's own form.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance types the launches below name, so the rows differ in the configuration
/// each recorded item carries (`RunInstancesRequest$InstanceType`).
const INSTANCE_TYPES: [&str; 3] = ["t3.micro", "m5.large", "c5.xlarge"];

/// The recorder every row writes.
const RECORDER_NAME: &str = "the-recorder";

/// The `Source$SourceIdentifier` of the AWS managed rule whose check is about a resource's
/// TAGS: "Checks if your resources have the tags that you specify".
const REQUIRED_TAGS_RULE: &str = "REQUIRED_TAGS";

/// An id no instance of the account carries, for the rows that narrow to a resource the
/// account does not hold. It is in the short form EC2 also issues instance ids in, so a
/// read narrowing by it is a read for an instance that could exist and does not.
const ABSENT_INSTANCE_ID: &str = "i-0abcdef01";

/// The state Amazon EC2 answers a LAUNCH with, and the one state EC2's own clock moves a
/// launched instance out of without anybody asking: `pending`, the first member of
/// `InstanceStateName`, from
/// `smithy_query(model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/\
/// com-amazonaws-ec2-amazonec2.json", query="shape", shape="InstanceStateName")` — whose
/// members are `pending`, `running`, `shutting-down`, `terminated`, `stopping` and
/// `stopped`.
///
/// A poll that waits for EC2 to report an instance PAST this state is waiting for EC2's own
/// clock, never for a span, and the state EC2 then has the instance in is one no call below
/// moves again — so the recorded item and EC2's own report can be compared without either
/// moving under the comparison.
const EC2_LAUNCHING_STATE: &str = "pending";

/// How long a poll waits for the discovery pass the clock owes. A pass falls due a
/// fraction of a second after the call that makes it due, and an instance reaches its
/// launched state a fraction of a second after the launch, so this is a hundred passes'
/// worth of slack and not a span the behaviour depends on: the deadline is here only so a
/// condition that never holds reports which read answered what instead of hanging, and it
/// is generous enough that a host running the whole suite at once never reaches it.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(60);

/// How long a row waits while asserting that nothing is admitted — several passes' worth of
/// time, so an admission that was going to happen would have happened.
const QUIET_BUDGET: Duration = Duration::from_millis(2_500);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Config and Amazon EC2 over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
            Arc::clone(&engine),
        ));
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self { router, callback }
    }

    /// One Config call (`awsJson1_1`), required to answer 200.
    async fn config(&self, operation: &str, body: &Value) -> Value {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/config/\
             aws4_request, SignedHeaders=host;x-amz-date, Signature=deadbeef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("config.{REGION}.amazonaws.com"))
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("StarlingDoveService.{operation}"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// An EC2 client over the in-process callback, addressed at [`REGION`] exactly as the
    /// recorder's own client is.
    fn ec2(&self) -> aws_sdk_ec2::Client {
        let config = aws_sdk_ec2::Config::builder()
            .behavior_version(aws_sdk_ec2::config::BehaviorVersion::latest())
            .region(aws_sdk_ec2::config::Region::new(REGION.to_string()))
            .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
            .credentials_provider(self.callback.aws_credentials_provider())
            .http_client(self.callback.http_client())
            .retry_config(aws_sdk_ec2::config::retry::RetryConfig::disabled())
            .timeout_config(aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
            .identity_cache(aws_sdk_ec2::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        aws_sdk_ec2::Client::from_conf(config)
    }

    /// Launch ONE instance of `instance_type` through EC2 itself, tagged with `tags`, and
    /// answer the id EC2 minted for it. Config is told nothing about it.
    async fn launch(&self, instance_type: &str, tags: &[(&str, &str)]) -> String {
        let tag_specifications = (!tags.is_empty()).then(|| {
            vec![aws_sdk_ec2::types::TagSpecification::builder()
                .resource_type(aws_sdk_ec2::types::ResourceType::Instance)
                .set_tags(Some(
                    tags.iter()
                        .map(|(key, value)| {
                            aws_sdk_ec2::types::Tag::builder()
                                .key(*key)
                                .value(*value)
                                .build()
                        })
                        .collect(),
                ))
                .build()]
        });
        let launched = self
            .ec2()
            .run_instances()
            .image_id(IMAGE_ID)
            .instance_type(aws_sdk_ec2::types::InstanceType::from(instance_type))
            .min_count(1)
            .max_count(1)
            .set_tag_specifications(tag_specifications)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("setup: EC2 must launch a {instance_type} instance, got {error:?}")
            });
        launched
            .instances()
            .iter()
            .find_map(|instance| instance.instance_id().map(str::to_string))
            .unwrap_or_else(|| panic!("setup: EC2 must report the id it launched"))
    }

    /// Terminate `instance_id` through EC2 itself.
    async fn terminate(&self, instance_id: &str) {
        self.ec2()
            .terminate_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("setup: EC2 must terminate {instance_id}, got {error:?}")
            });
    }

    /// Put a recorder with `group` as its recording group, and the delivery channel a
    /// recorder needs, starting it only when `start` says so.
    async fn recorder_with_group(&self, group: &Value, start: bool) {
        self.config(
            "PutConfigurationRecorder",
            &json!({
                "ConfigurationRecorder": {
                    "name": RECORDER_NAME,
                    "roleARN": format!("arn:aws:iam::{ACCOUNT_ID}:role/config-role"),
                    "recordingGroup": group,
                }
            }),
        )
        .await;
        self.config(
            "PutDeliveryChannel",
            &json!({ "DeliveryChannel": { "s3BucketName": "config-delivery" } }),
        )
        .await;
        if start {
            self.config(
                "StartConfigurationRecorder",
                &json!({ "ConfigurationRecorderName": RECORDER_NAME }),
            )
            .await;
        }
    }

    /// A recorder recording every supported resource type.
    async fn recording(&self) {
        self.recorder_with_group(&json!({ "allSupported": true }), true)
            .await;
    }

    /// Write the `REQUIRED_TAGS` rule `name`, scoped to the instance type and stating
    /// `parameters` as its requirements.
    async fn required_tags_rule(&self, name: &str, parameters: &Value) {
        self.config(
            "PutConfigRule",
            &json!({
                "ConfigRule": {
                    "ConfigRuleName": name,
                    "Source": { "Owner": "AWS", "SourceIdentifier": REQUIRED_TAGS_RULE },
                    "Scope": { "ComplianceResourceTypes": [INSTANCE_RESOURCE_TYPE] },
                    "InputParameters": parameters.to_string(),
                }
            }),
        )
        .await;
    }

    /// The verdict the rule `name` holds for `resource_id`, as
    /// `GetComplianceDetailsByConfigRule` reports it, or `None` while it holds none.
    async fn recorded_verdict(&self, name: &str, resource_id: &str) -> Option<String> {
        let body = self
            .config(
                "GetComplianceDetailsByConfigRule",
                &json!({ "ConfigRuleName": name }),
            )
            .await;
        body["EvaluationResults"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .find(|result| {
                result["EvaluationResultIdentifier"]["EvaluationResultQualifier"]["ResourceId"]
                    .as_str()
                    == Some(resource_id)
            })
            .and_then(|result| result["ComplianceType"].as_str().map(str::to_string))
    }
}

/// One discovered-resource read.
async fn listing(emulator: &Emulator, request: &Value) -> Value {
    emulator.config("ListDiscoveredResources", request).await
}

/// The `resourceId` of every identifier a read reported, in the order reported.
fn reported_ids(case: &str, body: &Value) -> Vec<String> {
    body["resourceIdentifiers"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected resourceIdentifiers, got {body}"))
        .iter()
        .map(|identifier| {
            identifier["resourceId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every identifier reports a resourceId, got {body}")
                })
                .to_string()
        })
        .collect()
}

/// Read `request` until it reports exactly `wanted`, and answer the response that did.
///
/// What is being waited for is the pass the recorder owes, never a value: the assertion
/// that fails at the deadline names the ids the read did report.
async fn await_ids(case: &str, emulator: &Emulator, request: &Value, wanted: &[&str]) -> Value {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let body = listing(emulator, request).await;
        let ids = reported_ids(case, &body);
        if ids.iter().map(String::as_str).collect::<Vec<&str>>() == wanted {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the discovered resource ids {wanted:?}, got {ids:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that `request` reports nothing for as long as [`QUIET_BUDGET`].
async fn stays_empty(case: &str, emulator: &Emulator, request: &Value) {
    let deadline = Instant::now() + QUIET_BUDGET;
    loop {
        let body = listing(emulator, request).await;
        let ids = reported_ids(case, &body);
        assert!(
            ids.is_empty(),
            "case {case:?}: nothing must be admitted to the inventory, got {ids:?}"
        );
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn an_instance_amazon_ec2_holds_is_discovered_however_the_read_narrows_to_it() {
    let emulator = Emulator::new();
    emulator.recording().await;

    // Every instance is launched through EC2's own API, each of a different type and with
    // its own tags. Config is told nothing about any of them.
    let first = emulator.launch(INSTANCE_TYPES[0], &[("stage", "prod")]).await;
    let second = emulator.launch(INSTANCE_TYPES[1], &[]).await;
    let third = emulator.launch(INSTANCE_TYPES[2], &[("stage", "test")]).await;
    // An instance id is minted from a counter that only grows, so the launch order is the
    // order the inventory reports them in.
    let live: Vec<&str> = vec![first.as_str(), second.as_str(), third.as_str()];
    await_ids(
        "every instance the account holds",
        &emulator,
        &json!({ "resourceType": INSTANCE_RESOURCE_TYPE }),
        &live,
    )
    .await;

    // An instance that goes away in its own service goes away from the live inventory, and
    // the inventory keeps it as deleted.
    emulator.terminate(&third).await;
    await_ids(
        "an instance terminated in its own service",
        &emulator,
        &json!({ "resourceType": INSTANCE_RESOURCE_TYPE }),
        &[first.as_str(), second.as_str()],
    )
    .await;

    // The rows differ along what the rule generalises over: whether the read narrows by
    // resource ids (one, several, one the account never held, the terminated one), by a
    // resource name, by nothing at all, whether deleted resources are asked for, and which
    // resource type is asked about.
    let cases: [(&str, Value, Vec<&str>); 9] = [
        (
            "no narrowing at all",
            json!({ "resourceType": INSTANCE_RESOURCE_TYPE }),
            vec![first.as_str(), second.as_str()],
        ),
        (
            "one resource id",
            json!({ "resourceType": INSTANCE_RESOURCE_TYPE, "resourceIds": [second] }),
            vec![second.as_str()],
        ),
        (
            "every live resource id, in the opposite order",
            json!({ "resourceType": INSTANCE_RESOURCE_TYPE, "resourceIds": [second, first] }),
            vec![first.as_str(), second.as_str()],
        ),
        (
            "several resource ids, one the account never held",
            json!({
                "resourceType": INSTANCE_RESOURCE_TYPE,
                "resourceIds": [first, ABSENT_INSTANCE_ID],
            }),
            vec![first.as_str()],
        ),
        (
            "the terminated instance's id, deleted resources excluded",
            json!({ "resourceType": INSTANCE_RESOURCE_TYPE, "resourceIds": [third] }),
            vec![],
        ),
        (
            "the terminated instance's id, deleted resources included",
            json!({
                "resourceType": INSTANCE_RESOURCE_TYPE,
                "resourceIds": [third],
                "includeDeletedResources": true,
            }),
            vec![third.as_str()],
        ),
        (
            "deleted resources included, with no other narrowing",
            json!({
                "resourceType": INSTANCE_RESOURCE_TYPE,
                "includeDeletedResources": true,
            }),
            vec![first.as_str(), second.as_str(), third.as_str()],
        ),
        (
            // An instance carries no custom name, so a read narrowing by one reports no
            // instance at all.
            "a resource name, which no instance carries",
            json!({ "resourceType": INSTANCE_RESOURCE_TYPE, "resourceName": "prod" }),
            vec![],
        ),
        (
            "a resource type the account holds nothing of",
            json!({ "resourceType": VOLUME_RESOURCE_TYPE }),
            vec![],
        ),
    ];

    for (case, request, expected) in cases {
        let body = await_ids(case, &emulator, &request, &expected).await;

        // Each identifier reports the instance as EC2 names it: the type it is recorded
        // under, the instance id EC2 minted, no custom name, and a deletion time for the
        // terminated instance and only for it.
        for identifier in body["resourceIdentifiers"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected resourceIdentifiers, got {body}"))
        {
            let resource_id = identifier["resourceId"].as_str().unwrap_or_default();
            assert_eq!(
                identifier["resourceType"],
                json!(INSTANCE_RESOURCE_TYPE),
                "case {case:?}: expected the type the resource is recorded under, got \
                 {identifier}"
            );
            assert!(
                identifier["resourceName"].is_null(),
                "case {case:?}: an instance carries no custom name, got {identifier}"
            );
            assert_eq!(
                identifier["resourceDeletionTime"].is_null(),
                resource_id != third,
                "case {case:?}: a deletion time is reported for the terminated instance and \
                 only for it, got {identifier}"
            );
        }

        // The read is a report: reading again reports the same resources.
        let again = listing(&emulator, &request).await;
        assert_eq!(
            reported_ids(case, &again),
            reported_ids(case, &body),
            "case {case:?}: a second read must report the same resources, got {again}"
        );
    }
}

#[tokio::test]
async fn a_discovered_instance_is_read_back_as_the_configuration_item_ec2_reports() {
    let emulator = Emulator::new();
    emulator.recording().await;

    // Two instances, so the item read back is the one the request named rather than
    // whichever the inventory holds first.
    let other = emulator.launch(INSTANCE_TYPES[0], &[]).await;
    let instance = emulator
        .launch(INSTANCE_TYPES[1], &[("stage", "prod"), ("owner", "guardrails")])
        .await;
    await_ids(
        "the instances the account holds",
        &emulator,
        &json!({ "resourceType": INSTANCE_RESOURCE_TYPE }),
        &[other.as_str(), instance.as_str()],
    )
    .await;

    // The ROUND TRIP: the resource the discovered-resource read reported is read back as
    // the configuration item the recorder captured for it — the item whose recorded state
    // is the one EC2 has the instance in, which is what a recorder that keeps recording
    // converges on as EC2's own clock moves the instance along.
    let item = await_item_in_ec2s_state(&emulator, &instance).await;
    assert_eq!(
        item["resourceId"].as_str(),
        Some(instance.as_str()),
        "the item reports the resource it is about, got {item}"
    );
    assert_eq!(
        item["arn"].as_str(),
        Some(format!("arn:aws:ec2:{REGION}:{ACCOUNT_ID}:instance/{instance}").as_str()),
        "the item carries the instance's own ARN, got {item}"
    );
    assert_eq!(
        item["tags"]["stage"].as_str(),
        Some("prod"),
        "the item carries the tags EC2 reports for the instance, got {item}"
    );

    // The recorded configuration is the instance as EC2 reports it: the launch's own AMI
    // and instance type, the id EC2 minted, and the state EC2 has the instance in.
    let configuration: Value = serde_json::from_str(
        item["configuration"]
            .as_str()
            .unwrap_or_else(|| panic!("the item carries a configuration document, got {item}")),
    )
    .unwrap_or_else(|error| panic!("the configuration document parses, got {error:?}"));
    assert_eq!(
        configuration["instanceId"].as_str(),
        Some(instance.as_str()),
        "the configuration names the instance, got {configuration}"
    );
    assert_eq!(
        configuration["imageId"].as_str(),
        Some(IMAGE_ID),
        "the configuration carries the AMI the instance was launched from, got \
         {configuration}"
    );
    assert_eq!(
        configuration["instanceType"].as_str(),
        Some(INSTANCE_TYPES[1]),
        "the configuration carries the instance type EC2 reports, got {configuration}"
    );

    // The state the item records is the one EC2 itself reports for the instance: read from
    // the owner rather than expected here, and read once the poll above has EC2 past the
    // state it moves the instance out of on its own, so the state this reads is the one EC2
    // settled the instance in rather than one its clock is about to replace. That the item
    // records the SETTLED state — a recorder that keeps recording re-records the instance as
    // its owner's clock moves it, rather than keeping the state it was discovered in — is
    // what the poll above converges on.
    let recorded_state = configuration["state"]["name"]
        .as_str()
        .unwrap_or_else(|| panic!("the configuration carries a state, got {configuration}"))
        .to_string();
    assert_eq!(
        Some(recorded_state.as_str()),
        ec2_state(&emulator, &instance).await.as_deref(),
        "the recorded state must be the one EC2 reports for the instance, got \
         {configuration}"
    );
}

/// The `InstanceState$Name` EC2 reports for `instance_id`, or `None` when EC2 reports no
/// state for it at all.
///
/// Read from EC2 itself, so a test asserting about a recorded state compares it with the
/// owner's own answer rather than with a state named here.
async fn ec2_state(emulator: &Emulator, instance_id: &str) -> Option<String> {
    emulator
        .ec2()
        .describe_instances()
        .instance_ids(instance_id)
        .send()
        .await
        .unwrap_or_else(|error| panic!("EC2 must describe {instance_id}, got {error:?}"))
        .reservations()
        .iter()
        .flat_map(aws_sdk_ec2::types::Reservation::instances)
        .filter(|held| held.instance_id() == Some(instance_id))
        .find_map(|held| held.state()?.name().map(|name| name.as_str().to_string()))
}

/// The configuration item Config holds for `instance_id` once EC2 has the instance past its
/// launching state and the item records the state EC2 has it in.
///
/// What is being waited for is EC2's own clock taking the instance out of
/// [`EC2_LAUNCHING_STATE`] and the recording pass the recorder owes after it, never a span:
/// the assertion that fails at the deadline names both states. Waiting for the launching
/// state to be BEHIND the instance is what makes the comparison deterministic — an item that
/// records a state EC2 is about to leave agrees with EC2 only until EC2's clock moves, so a
/// caller of this helper that reads EC2 again would race that move on a host slow enough to
/// cross it.
async fn await_item_in_ec2s_state(emulator: &Emulator, instance_id: &str) -> Value {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let reported = ec2_state(emulator, instance_id).await;
        // The instance is past its launch once EC2 reports it in any other state, which is
        // the point from which EC2's own clock stops moving it.
        let past_launch = reported
            .as_deref()
            .is_some_and(|state| state != EC2_LAUNCHING_STATE);
        let history = emulator
            .config(
                "GetResourceConfigHistory",
                &json!({
                    "resourceType": INSTANCE_RESOURCE_TYPE,
                    "resourceId": instance_id,
                }),
            )
            .await;
        let item = history["configurationItems"]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        let recorded = item["configuration"]
            .as_str()
            .and_then(|configuration| {
                serde_json::from_str::<Value>(configuration).ok()
            })
            .and_then(|configuration| {
                configuration["state"]["name"].as_str().map(str::to_string)
            });
        if past_launch && recorded == reported {
            return item;
        }
        assert!(
            Instant::now() < deadline,
            "the recorded configuration item of {instance_id} must reach the state EC2 \
             reports once EC2 has the instance past {EC2_LAUNCHING_STATE:?}: EC2 reports \
             {reported:?}, the item records {recorded:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_recording_group_and_the_recorder_decide_whether_an_instance_is_discovered() {
    // The rows differ along which resource types the recorder's group covers — all of
    // them, an inclusion group naming this type or another, an exclusion group excluding
    // this type or another — and along whether the recorder is recording at all.
    let cases: [(&str, Value, bool, bool); 6] = [
        (
            "a group of all supported types",
            json!({ "allSupported": true }),
            true,
            true,
        ),
        (
            "an inclusion group naming this type",
            json!({ "resourceTypes": [INSTANCE_RESOURCE_TYPE] }),
            true,
            true,
        ),
        (
            "an inclusion group naming another type only",
            json!({ "resourceTypes": [VOLUME_RESOURCE_TYPE] }),
            true,
            false,
        ),
        (
            "an exclusion group excluding this type",
            json!({
                "exclusionByResourceTypes": { "resourceTypes": [INSTANCE_RESOURCE_TYPE] },
            }),
            true,
            false,
        ),
        (
            "an exclusion group excluding another type",
            json!({
                "exclusionByResourceTypes": { "resourceTypes": [VOLUME_RESOURCE_TYPE] },
            }),
            true,
            true,
        ),
        (
            "a recorder that was never started",
            json!({ "allSupported": true }),
            false,
            false,
        ),
    ];

    for (case, group, recording, admitted) in cases {
        let emulator = Emulator::new();
        emulator.recorder_with_group(&group, recording).await;
        let instance = emulator.launch(INSTANCE_TYPES[0], &[]).await;

        let request = json!({
            "resourceType": INSTANCE_RESOURCE_TYPE,
            "resourceIds": [instance],
        });
        if admitted {
            await_ids(case, &emulator, &request, &[instance.as_str()]).await;
        } else {
            stays_empty(case, &emulator, &request).await;
        }
    }
}

/// Read the verdict `rule` holds for `resource_id` until it is `wanted`.
///
/// What is being waited for is the evaluation pass the rule is owed, never a span: the
/// assertion that fails at the deadline names the verdict the rule did hold.
async fn await_verdict(
    case: &str,
    emulator: &Emulator,
    rule: &str,
    resource_id: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let held = emulator.recorded_verdict(rule, resource_id).await;
        if held.as_deref() == Some(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {rule} to place {resource_id} at {wanted}, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_tag_rule_places_the_discovered_instance_from_the_tags_the_recorder_recorded() {
    // A discovered instance is a resource Config's rules evaluate: the rows differ along
    // what the rule REQUIRES — a tag key the instance carries or one it does not, a key
    // with one acceptable value, a key with several (of which the instance's is one, or is
    // not), several keys of which one is missing, and a rule requiring nothing at all.
    //
    // Parameter names and the comma-separated value list are the managed rule's own
    // (`tag1Key`/`tag1Value` … `tag6Key`/`tag6Value`, "separate multiple values with
    // commas"); the fourth pair is one the issue did not cite.
    let cases: [(&str, Value, &str); 7] = [
        (
            "a tag key the instance carries",
            json!({ "tag1Key": "stage" }),
            "COMPLIANT",
        ),
        (
            "a tag key the instance does not carry",
            json!({ "tag1Key": "costCentre" }),
            "NON_COMPLIANT",
        ),
        (
            "a key with the one value the instance carries",
            json!({ "tag1Key": "stage", "tag1Value": "prod" }),
            "COMPLIANT",
        ),
        (
            "a key with several values, one of them the instance's",
            json!({ "tag1Key": "stage", "tag1Value": "dev,prod,test" }),
            "COMPLIANT",
        ),
        (
            "a key with values none of which is the instance's",
            json!({ "tag1Key": "stage", "tag1Value": "dev,test" }),
            "NON_COMPLIANT",
        ),
        (
            "two keys, the fourth pair naming one the instance does not carry",
            json!({ "tag1Key": "stage", "tag4Key": "costCentre" }),
            "NON_COMPLIANT",
        ),
        (
            "a rule requiring no tag at all",
            json!({}),
            "COMPLIANT",
        ),
    ];

    let emulator = Emulator::new();
    emulator.recording().await;
    // The tags the rule's requirements are judged against are the ones EC2 reports for the
    // instance: no Config call below carries them.
    let instance = emulator
        .launch(INSTANCE_TYPES[0], &[("stage", "prod"), ("owner", "guardrails")])
        .await;
    await_ids(
        "the instance the account holds",
        &emulator,
        &json!({ "resourceType": INSTANCE_RESOURCE_TYPE }),
        &[instance.as_str()],
    )
    .await;

    for (index, (case, parameters, expected)) in cases.into_iter().enumerate() {
        // One rule per row, so a row's verdict is its own rule's.
        let rule = format!("required-tags-{index}");
        emulator.required_tags_rule(&rule, &parameters).await;
        await_verdict(case, &emulator, &rule, &instance, expected).await;
    }
}
