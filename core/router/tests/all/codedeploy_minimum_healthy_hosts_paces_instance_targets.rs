// SPDX-License-Identifier: CC0-1.0

//! What a client SEES of a CodeDeploy EC2/on-premises deployment's own fleet: which Amazon EC2
//! instances it deploys to, and how many of them it has out of service at one instant.
//!
//! RULE: the targets of a `Server`-platform deployment are the RUNNING Amazon EC2 instances of
//! the account that its deployment group's EC2 tag selectors identify, each reported under its
//! instance id, and the deployment takes at most (fleet size - the resolved deployment
//! configuration's `minimumHealthyHosts`) of them out of service at one instant — never fewer
//! than one — so it works through the fleet in waves of that size, one wave at a time. At every
//! instant its targets therefore PARTITION the fleet (`deploymentOverview`'s six counts add up
//! to it, and each target reports exactly one status), no target moves backwards through
//! Pending -> InProgress -> Succeeded, and a deployment that needs more than one wave is held
//! open — `createTime` to `completeTime`, as the service itself reports those instants — longer
//! than the same revision deployed to the whole fleet at once.
//!
//! `minimumHealthyHosts` is "the minimum number or percentage of instances that should be
//! healthy at any time during a deployment", stated as `HOST_COUNT` ("the minimum number of
//! healthy instances as an absolute value") or `FLEET_PERCENT` ("as a percentage of the total
//! number of instances in the deployment"), and the model states the arithmetic itself: "In an
//! example of nine instances, if a HOST_COUNT of six is specified, deploy to up to three
//! instances at a time […] If a FLEET_PERCENT of 40 is specified, deploy to up to five
//! instances at a time"
//! (`smithy_query(query="shape",shape="MinimumHealthyHosts")`). `ec2TagFilters` is "the Amazon
//! EC2 tags on which to filter. The deployment group includes Amazon EC2 instances with any of
//! the specified tags" and an `ec2TagSet` is "a list that contains other lists of Amazon EC2
//! instance tag groups. For an instance to be included in the deployment group, it must be
//! identified by all of the tag groups in the list"
//! (`smithy_query(query="doc",shape="CreateDeploymentGroupInput",member="ec2TagFilters")`,
//! `smithy_query(query="shape",shape="EC2TagSet")`).
//!
//! This suite lives beside the router because the fleet belongs to ANOTHER service: every host
//! below is launched through Amazon EC2's own `RunInstances` on the SAME engine that serves
//! CodeDeploy, with its tags in the launch's `TagSpecification`, so the instance ids a
//! deployment reports are ids EC2 handed out and the instance that is not running is one EC2
//! itself stopped. (The CodeDeploy crate also roots its integration tests at a hand-declared
//! `tests/all.rs`, which a fix may not edit; the router's `tests/all/` picks a new suite up
//! with nothing to declare.)
//!
//! The rows differ along what the rule generalises over: the selector form that identifies the
//! fleet (a filter list, a tag set), whether an instance is running at all, and the bound the
//! resolved configuration states — a built-in and a client-created one, `HOST_COUNT` and
//! `FLEET_PERCENT`, bounds that leave the whole fleet deployable and bounds that leave only one
//! host deployable.
//!
//! Every wait here polls for the CONDITION it is waiting on and nothing asserts how long
//! anything took on this host: the two windows compared at the end are the `createTime` and
//! `completeTime` the service itself reports.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode};

/// The Region every request below is signed for: a deployment resolves the fleet of the Region
/// it was created in, and the launches use the same one.
const REGION: &str = "us-east-1";

/// The EC2 API version an ec2Query body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the launches below run from, in the long form EC2 issues AMI ids in; `RunInstances`
/// only has to accept it, and nothing here reads anything out of the image.
const AMI: &str = "ami-0123456789abcdef0";

/// The instance type every host below is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`), which is the account whose
/// fleet is resolved and whose instance ARNs the targets carry.
const ACCOUNT: &str = "000000000000";

/// The service role every deployment group here is created with. No IAM is mounted beside
/// these two services, so nothing can be shown about this role and the group is stored: what
/// these cases are about is the FLEET, not the role gate.
const SERVICE_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/CodeDeployServiceRole";

/// The tag key the fleet carries, and the value that identifies the hosts a deployment group
/// here selects.
const FLEET_KEY: &str = "Fleet";
/// See [`FLEET_KEY`].
const FLEET_VALUE: &str = "deployable";

/// A second tag key, so a tag SET can name two groups that have to be satisfied together.
const ROLE_KEY: &str = "Role";
/// See [`ROLE_KEY`].
const ROLE_VALUE: &str = "web";

/// The built-in EC2/on-premises configurations named below, from the catalogue CodeDeploy
/// ships: `minimumHealthyHosts` HOST_COUNT 0 for the first (every host at once) and
/// FLEET_PERCENT 99 for the second (one host at a time), which is also the configuration a
/// deployment group that names none runs under.
const ALL_AT_ONCE: &str = "CodeDeployDefault.AllAtOnce";
/// See [`ALL_AT_ONCE`].
const ONE_AT_A_TIME: &str = "CodeDeployDefault.OneAtATime";

/// How long a poll waits for a condition the clock owes — an instance reaching a state, a
/// deployment reaching an outcome. Generous by design: it is a deadline, not a measurement.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(120);

/// How long a poll waits between two reads. Fine enough that the window each wave of targets
/// is out of service for is sampled many times over.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The six counts `deploymentOverview` partitions a deployment's targets into
/// (`smithy_query(query="shape",shape="DeploymentOverview")`).
const OVERVIEW_COUNTS: &[&str] = &[
    "Pending",
    "InProgress",
    "Succeeded",
    "Failed",
    "Skipped",
    "Ready",
];

/// One emulator carrying Amazon EC2 and CodeDeploy over ONE shared engine, as the assembled
/// router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_codedeploy_codedeploy_20141006::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and which Region it is served in; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form field of an ec2Query body.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// POST one ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {answered}"
    );
    answered
}

/// Call one CodeDeploy operation over the awsJson1_1 wire its model declares.
async fn codedeploy(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("codedeploy.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CodeDeploy_20141006.{operation}"))
        .header("authorization", authorization("codedeploy"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Call one CodeDeploy operation, requiring HTTP 200.
async fn codedeploy_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, answered) = codedeploy(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup: CodeDeploy {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// Launch ONE instance carrying `tags` through EC2's own `RunInstances`, with the tags in the
/// launch's `TagSpecification`, and answer its instance id.
async fn launch(router: &MultiServiceRouter, tags: &[(&str, &str)]) -> String {
    let mut params = vec![
        param("ImageId", AMI),
        param("InstanceType", INSTANCE_TYPE),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    if !tags.is_empty() {
        params.push(param("TagSpecification.1.ResourceType", "instance"));
        for (position, (key, value)) in tags.iter().enumerate() {
            let index = position + 1;
            params.push(param(&format!("TagSpecification.1.Tag.{index}.Key"), key));
            params.push(param(&format!("TagSpecification.1.Tag.{index}.Value"), value));
        }
    }
    let launched = ec2(router, "RunInstances", &params).await;
    element(&launched, "instanceId")
}

/// Poll EC2 until it reports `instance_id` in `state`, which is the condition the wait ends on.
async fn poll_until_state(router: &MultiServiceRouter, instance_id: &str, state: &str) {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let described = ec2(
            router,
            "DescribeInstances",
            &[param("InstanceId.1", instance_id)],
        )
        .await;
        let block = block_of(&described, "instanceState").unwrap_or_default();
        let names = elements(block, "name");
        if names.iter().any(|name| name == state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: EC2 must report {instance_id} {state} within {CONVERGENCE_BUDGET:?}, got \
             {names:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Launch `hosts` instances tagged with the fleet tag and wait until EC2 reports each running,
/// answering their ids in launch order.
async fn given_running_fleet(router: &MultiServiceRouter, hosts: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(hosts);
    for _ in 0..hosts {
        ids.push(launch(router, &[(FLEET_KEY, FLEET_VALUE), (ROLE_KEY, ROLE_VALUE)]).await);
    }
    for id in &ids {
        poll_until_state(router, id, "running").await;
    }
    ids
}

/// Create a `Server` application and one deployment group of it selecting the fleet with
/// `selector`, under `deployment_config` when it names one.
async fn given_group(
    router: &MultiServiceRouter,
    application: &str,
    group: &str,
    selector: &Value,
    deployment_config: Option<&str>,
) {
    codedeploy_ok(
        router,
        "CreateApplication",
        &json!({"applicationName": application, "computePlatform": "Server"}),
    )
    .await;
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "serviceRoleArn": SERVICE_ROLE_ARN,
    });
    for (member, value) in selector.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    if let Some(config) = deployment_config {
        request["deploymentConfigName"] = json!(config);
    }
    codedeploy_ok(router, "CreateDeploymentGroup", &request).await;
}

/// The `ec2TagFilters` selector that takes in the launched fleet.
fn by_tag_filter() -> Value {
    json!({
        "ec2TagFilters": [
            {"Key": FLEET_KEY, "Value": FLEET_VALUE, "Type": "KEY_AND_VALUE"},
        ],
    })
}

/// An `s3Location` revision, the form a `Server` deployment's revision travels in. No Amazon S3
/// is mounted beside these two services, so nothing can be shown about the object and the
/// deployment runs: what these cases are about is the fleet it runs over.
fn s3_revision(key: &str) -> Value {
    json!({
        "revisionType": "S3",
        "s3Location": {"bucket": "revisions", "key": key, "bundleType": "zip"},
    })
}

/// Deploy a revision to `application`/`group`, answering the deployment id CodeDeploy minted.
async fn deploy(router: &MultiServiceRouter, application: &str, group: &str) -> String {
    codedeploy_ok(
        router,
        "CreateDeployment",
        &json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "revision": s3_revision("app.zip"),
            "fileExistsBehavior": "OVERWRITE",
        }),
    )
    .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The stored `deploymentInfo` of `deployment_id`.
async fn deployment_info(router: &MultiServiceRouter, deployment_id: &str) -> Value {
    codedeploy_ok(
        router,
        "GetDeployment",
        &json!({"deploymentId": deployment_id}),
    )
    .await["deploymentInfo"]
        .clone()
}

/// The ids `ListDeploymentTargets` answers for `deployment_id`.
async fn listed_targets(router: &MultiServiceRouter, deployment_id: &str) -> Vec<String> {
    codedeploy_ok(
        router,
        "ListDeploymentTargets",
        &json!({"deploymentId": deployment_id}),
    )
    .await["targetIds"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
}

/// Wait until `deployment_id` holds a terminal `DeploymentStatus`, answering its
/// `deploymentInfo`.
async fn terminal_deployment(router: &MultiServiceRouter, deployment_id: &str, case: &str) -> Value {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let info = deployment_info(router, deployment_id).await;
        if matches!(
            info["status"].as_str(),
            Some("Succeeded" | "Failed" | "Stopped")
        ) {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployment to reach an outcome within \
             {CONVERGENCE_BUDGET:?}, last {info}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a `Server` deployment's targets are the RUNNING Amazon EC2 instances the group's EC2
/// selectors identify — reported under their instance ids, with EC2's own instance ARN — and an
/// instance the selectors do not identify or that is not running is no target of it.
#[tokio::test]
async fn an_ec2_deployment_targets_the_running_instances_its_group_selects() {
    let router = emulator();
    // The fleet: two hosts carrying both tags, one carrying only the fleet tag, one decoy
    // carrying the key with another value, one carrying no tag at all, and one carrying both
    // tags that EC2 is then asked to STOP.
    let both_tags = given_running_fleet(&router, 2).await;
    let fleet_tag_only = launch(&router, &[(FLEET_KEY, FLEET_VALUE)]).await;
    let other_value = launch(&router, &[(FLEET_KEY, "retired")]).await;
    let untagged = launch(&router, &[]).await;
    let stopped = launch(&router, &[(FLEET_KEY, FLEET_VALUE), (ROLE_KEY, ROLE_VALUE)]).await;
    for id in [&fleet_tag_only, &other_value, &untagged, &stopped] {
        poll_until_state(&router, id, "running").await;
    }
    ec2(
        &router,
        "StopInstances",
        &[param("InstanceId.1", &stopped)],
    )
    .await;
    poll_until_state(&router, &stopped, "stopped").await;

    // Rows differ along the selector form: a filter list, which takes in an instance carrying
    // ANY of its filters, and a tag set, which takes in one carrying a filter from EVERY group.
    let by_tag_set = json!({
        "ec2TagSet": {
            "ec2TagSetList": [
                [{"Key": FLEET_KEY, "Value": FLEET_VALUE, "Type": "KEY_AND_VALUE"}],
                [{"Key": ROLE_KEY, "Value": ROLE_VALUE, "Type": "KEY_AND_VALUE"}],
            ],
        },
    });
    let mut with_fleet_tag = both_tags.clone();
    with_fleet_tag.push(fleet_tag_only.clone());
    with_fleet_tag.sort();
    let mut with_both_tags = both_tags.clone();
    with_both_tags.sort();
    let cases: &[(&str, Value, &Vec<String>)] = &[
        (
            "a filter list on the fleet tag",
            by_tag_filter(),
            &with_fleet_tag,
        ),
        ("a tag set naming two groups", by_tag_set, &with_both_tags),
    ];

    for (index, (case, selector, expected)) in cases.iter().enumerate() {
        let application = format!("ec2-fleet-{index:02}");
        let group = format!("{application}-dg");
        given_group(&router, &application, &group, selector, Some(ALL_AT_ONCE)).await;
        let deployment_id = deploy(&router, &application, &group).await;
        let info = terminal_deployment(&router, &deployment_id, case).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: expected the deployment over a fleet it resolved to succeed, got \
             {info}"
        );

        let mut listed = listed_targets(&router, &deployment_id).await;
        listed.sort();
        assert_eq!(
            &listed, *expected,
            "case {case:?}: expected the targets {expected:?}, got {listed:?}"
        );
        // Neither the decoys nor the stopped host is a target of it.
        for outsider in [&other_value, &untagged, &stopped] {
            assert!(
                !listed.contains(outsider),
                "case {case:?}: expected {outsider:?} not to be a target, got {listed:?}"
            );
        }

        // Each target reads back as an instance target of the instance EC2 holds: the id it was
        // launched under, EC2's own instance ARN for it, and the status the finished deployment
        // gave it.
        let read = codedeploy_ok(
            &router,
            "BatchGetDeploymentTargets",
            &json!({"deploymentId": deployment_id, "targetIds": listed.clone()}),
        )
        .await;
        let targets = read["deploymentTargets"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            targets.len(),
            listed.len(),
            "case {case:?}: expected one record per target, got {read}"
        );
        for target in &targets {
            let instance = &target["instanceTarget"];
            let target_id = instance["targetId"].as_str().unwrap_or_default();
            assert_eq!(
                (
                    target["deploymentTargetType"].as_str(),
                    instance["status"].as_str(),
                    instance["targetArn"].as_str(),
                ),
                (
                    Some("InstanceTarget"),
                    Some("Succeeded"),
                    Some(format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/{target_id}").as_str()),
                ),
                "case {case:?}: expected the instance target of {target_id:?} to report EC2's \
                 own instance, got {target}"
            );
        }
    }
}

/// One row of the pacing table.
struct Paced {
    /// What the row is about.
    case: &'static str,
    /// The `minimumHealthyHosts` a configuration created for this row states, or `None` to name
    /// a built-in (or no configuration at all) instead.
    minimum_healthy_hosts: Option<(&'static str, i32)>,
    /// The built-in the group names, for a row that creates no configuration of its own;
    /// `None` leaves the group without one, which runs it under the service's own default.
    builtin: Option<&'static str>,
    /// How many of the fleet the bound leaves deployable at one instant.
    concurrency: usize,
}

/// The counts `deploymentOverview` reports, as (status, count) pairs, asserting that each is a
/// number and that together they partition `fleet`.
fn assert_partitions(case: &str, overview: &Value, fleet: usize) -> Vec<(String, i64)> {
    let mut counted = Vec::with_capacity(OVERVIEW_COUNTS.len());
    let mut total = 0;
    for name in OVERVIEW_COUNTS {
        let count = overview[*name].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: expected deploymentOverview.{name} as a number, got {overview}")
        });
        total += count;
        counted.push(((*name).to_string(), count));
    }
    assert_eq!(
        usize::try_from(total).unwrap_or_default(),
        fleet,
        "case {case:?}: expected the summary to partition the fleet of {fleet}, got {overview}"
    );
    counted
}

/// How far along `status` is, so a sample can be checked for a target moving BACKWARDS.
fn progress_of(case: &str, status: &str) -> usize {
    match status {
        "Pending" => 0,
        "InProgress" => 1,
        "Succeeded" => 2,
        other => panic!("case {case:?}: expected a paced target's status, got {other:?}"),
    }
}

/// RULE: the deployment has at most `concurrency` of its targets InProgress at any instant, its
/// summary partitions the fleet at every instant, no target moves backwards, and every target
/// ends `Succeeded`. Answers the window the service reported for it: `completeTime` -
/// `createTime`.
async fn run_paced_deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    group: &str,
    fleet: &[String],
    concurrency: usize,
) -> f64 {
    let deployment_id = deploy(router, application, group).await;
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    let mut most_in_progress = 0_usize;
    let mut furthest: Vec<usize> = vec![0; fleet.len()];
    loop {
        let info = deployment_info(router, &deployment_id).await;
        let status = info["status"].as_str().unwrap_or_default().to_string();
        let overview = assert_partitions(case, &info["deploymentOverview"], fleet.len());
        let in_progress = overview
            .iter()
            .find(|(name, _)| name == "InProgress")
            .map_or(0, |(_, count)| usize::try_from(*count).unwrap_or_default());
        assert!(
            in_progress <= concurrency,
            "case {case:?}: expected at most {concurrency} target(s) out of service at once, got \
             {in_progress} in {info}"
        );
        most_in_progress = most_in_progress.max(in_progress);

        // The per-target records a client samples, which have to agree with that summary: a
        // deployment that has not started holds no readable target, which is the window the
        // not-started refusal covers.
        if status != "Created" {
            let (code, read) = codedeploy(
                router,
                "BatchGetDeploymentTargets",
                &json!({"deploymentId": deployment_id, "targetIds": fleet.to_vec()}),
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected the started deployment's targets to be readable, got \
                 {code} with {read}"
            );
            let mut sampled = 0;
            for target in read["deploymentTargets"].as_array().unwrap_or(&Vec::new()) {
                let instance = &target["instanceTarget"];
                let target_id = instance["targetId"].as_str().unwrap_or_default();
                let reported = instance["status"].as_str().unwrap_or_default();
                let position = fleet
                    .iter()
                    .position(|id| id == target_id)
                    .unwrap_or_else(|| panic!("case {case:?}: unknown target {target_id:?}"));
                let reached = progress_of(case, reported);
                assert!(
                    reached >= furthest[position],
                    "case {case:?}: expected {target_id:?} not to move backwards, it was at \
                     {} and now reports {reported:?}",
                    furthest[position]
                );
                furthest[position] = reached;
                if reached == 1 {
                    sampled += 1;
                }
            }
            assert!(
                sampled <= concurrency,
                "case {case:?}: expected at most {concurrency} target(s) reported InProgress, \
                 got {sampled} in {read}"
            );
        }

        if matches!(status.as_str(), "Succeeded" | "Failed" | "Stopped") {
            assert_eq!(
                status, "Succeeded",
                "case {case:?}: expected the deployment over its resolved fleet to succeed, got \
                 {info}"
            );
            assert_eq!(
                most_in_progress, concurrency,
                "case {case:?}: expected a wave of {concurrency} target(s) to have been out of \
                 service at once, the most seen was {most_in_progress}"
            );
            let create_time = info["createTime"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: expected a createTime, got {info}")
            });
            let complete_time = info["completeTime"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: expected a completeTime, got {info}")
            });
            return complete_time - create_time;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployment to reach an outcome within \
             {CONVERGENCE_BUDGET:?}, last {info}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: `minimumHealthyHosts` bounds how many of a deployment's targets are out of service at
/// one instant, and a deployment that has to pace its fleet is held open longer than one that
/// takes the whole fleet at once.
#[tokio::test]
async fn minimum_healthy_hosts_bounds_the_targets_out_of_service_at_once() {
    let router = emulator();
    // Three hosts, so a percentage that rounds up and a count that leaves two deployable are
    // both distinguishable from "all at once" and from "one at a time".
    let fleet = given_running_fleet(&router, 3).await;

    let cases: &[Paced] = &[
        Paced {
            case: "the built-in that takes the whole fleet at once",
            minimum_healthy_hosts: None,
            builtin: Some(ALL_AT_ONCE),
            concurrency: 3,
        },
        Paced {
            case: "the built-in that takes one host at a time",
            minimum_healthy_hosts: None,
            builtin: Some(ONE_AT_A_TIME),
            concurrency: 1,
        },
        Paced {
            case: "no configuration on the group, which is the service's own default",
            minimum_healthy_hosts: None,
            builtin: None,
            concurrency: 1,
        },
        Paced {
            case: "a HOST_COUNT of one, which leaves two of three deployable",
            minimum_healthy_hosts: Some(("HOST_COUNT", 1)),
            builtin: None,
            concurrency: 2,
        },
        Paced {
            case: "a HOST_COUNT of two, which leaves one of three deployable",
            minimum_healthy_hosts: Some(("HOST_COUNT", 2)),
            builtin: None,
            concurrency: 1,
        },
        Paced {
            case: "a FLEET_PERCENT of 50, rounded up to two healthy of three",
            minimum_healthy_hosts: Some(("FLEET_PERCENT", 50)),
            builtin: None,
            concurrency: 1,
        },
        Paced {
            case: "a FLEET_PERCENT of 0, which keeps no host back",
            minimum_healthy_hosts: Some(("FLEET_PERCENT", 0)),
            builtin: None,
            concurrency: 3,
        },
    ];

    // The window of the row that takes the whole fleet at once, which every paced row's window
    // is compared against: one wave is the fewest a deployment can run.
    let mut all_at_once_window: Option<f64> = None;
    for (index, case) in cases.iter().enumerate() {
        let application = format!("paced-{index:02}");
        let group = format!("{application}-dg");
        let config = case.minimum_healthy_hosts.map(|(kind, value)| {
            let name = format!("{application}-config");
            (name, kind, value)
        });
        if let Some((name, kind, value)) = config.as_ref() {
            codedeploy_ok(
                &router,
                "CreateDeploymentConfig",
                &json!({
                    "deploymentConfigName": name,
                    "computePlatform": "Server",
                    "minimumHealthyHosts": {"type": kind, "value": value},
                }),
            )
            .await;
        }
        let named = config
            .as_ref()
            .map(|(name, _, _)| name.as_str())
            .or(case.builtin);
        given_group(&router, &application, &group, &by_tag_filter(), named).await;

        let window = run_paced_deployment(
            &router,
            case.case,
            &application,
            &group,
            &fleet,
            case.concurrency,
        )
        .await;
        if case.concurrency >= fleet.len() {
            all_at_once_window = Some(window);
        } else {
            let unpaced = all_at_once_window.unwrap_or_else(|| {
                panic!(
                    "case {:?}: the unpaced row runs first, so its window is known",
                    case.case
                )
            });
            assert!(
                window > unpaced,
                "case {:?}: expected the deployment to be held open longer than the unpaced \
                 {unpaced} seconds the service reported, got {window}",
                case.case
            );
        }
    }
}
