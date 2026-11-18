// SPDX-License-Identifier: CC0-1.0

//! The Amazon EBS volume an ECS launch CONFIGURES AT LAUNCH, as Amazon EC2 holds it.
//!
//! RULE: a volume a task definition declares `configuredAtLaunch` is configured by the
//! LAUNCH, matched by name — `Volume$configuredAtLaunch`: "To configure a volume at launch
//! time, use this task definition revision and specify a `volumeConfigurations` object when
//! calling the `CreateService`, `UpdateService`, `RunTask` or `StartTask` APIs", and
//! `RunTaskRequest$volumeConfigurations`: "The `name` of the volume must match the `name`
//! from the task definition". So a launch that carries no configuration for such a volume,
//! or one whose configuration names no such volume, is refused with
//! `InvalidParameterException` and places nothing; and a launch that carries it is launched
//! with a REAL Amazon EBS volume of the account — "one volume created for each task"
//! (`TaskManagedEBSVolumeConfiguration`) — which Amazon EC2's own `DescribeVolumes`
//! resolves, carrying the tags the configuration asked for, attached to the task Amazon ECS
//! created it for (`VolumeAttachment$AssociatedResource` /
//! `InstanceOwningService`), and reported on the task as an `AmazonElasticBlockStorage`
//! attachment whose `details` are the ones `Attachment$details` lists for that type
//! ("`roleArn`, `deleteOnTermination`, `volumeName`, `volumeId`").
//!
//! This suite lives beside the router because the volume is EC2's: it is read back through
//! that service's own `DescribeVolumes` on the SAME engine that serves ECS, so the id in
//! every assertion is one EC2 minted rather than one this suite made up — and so is the
//! subnet whose Availability Zone the volume has to be created in.
//!
//! Member, default and detail sources:
//! `smithy_query(query="shape", shape="com.amazonaws.ecs#Volume")`,
//! `smithy_query(query="shape", shape="com.amazonaws.ecs#TaskManagedEBSVolumeConfiguration")`,
//! `smithy_query(query="shape", shape="com.amazonaws.ecs#EBSTagSpecification")` (the two
//! reserved tag keys), `smithy_query(query="shape", shape="com.amazonaws.ecs#Attachment")`
//! (the `details` list for Elastic Block Storage) and
//! `smithy_query(query="shape", shape="com.amazonaws.ec2#VolumeAttachment")` (what EC2
//! reports for a volume attached to an Amazon Web Services-managed resource).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, items_of, opt_element};

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The zone the one subnet below is in, which is where every volume of a task placed into it
/// has to be created.
const ZONE: &str = "us-west-2a";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The ECS cluster every case here launches into.
const CLUSTER: &str = "ebsatlaunch";

/// The revision that declares one volume `configuredAtLaunch`, under this name.
const AT_LAUNCH_FAMILY: &str = "ebsatlaunchfamily";
const AT_LAUNCH_VOLUME: &str = "data";

/// The revision that declares a volume the launch does NOT configure (a bind mount host
/// volume), under this name.
const HOST_FAMILY: &str = "ebshostfamily";
const HOST_VOLUME: &str = "scratch";

/// The revision that declares BOTH: a host volume beside the one configured at launch.
const MIXED_FAMILY: &str = "ebsmixedfamily";

/// The image the one container of each family starts from; irrelevant to the rule.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// The infrastructure role each configuration associates with its volume
/// (`TaskManagedEBSVolumeConfiguration$roleArn`, which is required).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/ecsInfrastructureRole";

/// `Attachment$type` of the volume a launch configured at launch, and the `details` keys
/// that attachment carries — `Attachment$details`: "For Elastic Block Storage, this includes
/// `roleArn`, `deleteOnTermination`, `volumeName`, `volumeId`".
const EBS_ATTACHMENT_TYPE: &str = "AmazonElasticBlockStorage";
const DETAIL_VOLUME_NAME: &str = "volumeName";
const DETAIL_VOLUME_ID: &str = "volumeId";
const DETAIL_ROLE_ARN: &str = "roleArn";
const DETAIL_DELETE_ON_TERMINATION: &str = "deleteOnTermination";

/// The tag keys Amazon ECS applies to the volume itself, which a caller's own tag
/// specification may not use (`EBSTagSpecification$tags`: "`AmazonECSCreated` and
/// `AmazonECSManaged` are reserved tags that can't be used").
const RESERVED_CREATED_TAG: &str = "AmazonECSCreated";
const RESERVED_MANAGED_TAG: &str = "AmazonECSManaged";

/// The service principal EC2 reports for a volume attached to an ECS task
/// (`VolumeAttachment$InstanceOwningService`).
const ECS_PRINCIPAL: &str = "ecs.amazonaws.com";

/// The volume type EC2 creates a volume with when the configuration names none —
/// `CreateVolumeRequest$VolumeType`: "Default: gp2".
const DEFAULT_VOLUME_TYPE: &str = "gp2";

/// One emulator carrying EC2 and ECS over ONE shared engine, as the production router mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
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
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One awsJson1_1 call to ECS, answered with the HTTP status and the parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One ECS call that must answer 200, as its body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = ecs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// The subnet every launch below places its task in, minted by EC2 itself so the zone the
/// volume is created in is one EC2 holds.
async fn ec2_subnet(router: &MultiServiceRouter) -> String {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.90.1.0/24"),
                ("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    )
}

/// Create the cluster and register the three revisions the cases launch: one declaring a
/// volume `configuredAtLaunch`, one declaring a host volume instead, and one declaring both.
async fn seed_ecs(router: &MultiServiceRouter) {
    ecs_ok(router, "CreateCluster", json!({ "clusterName": CLUSTER }), "setup").await;
    let container = json!({ "name": "app", "image": IMAGE, "memory": 128 });
    for (family, volumes) in [
        (
            AT_LAUNCH_FAMILY,
            json!([{ "name": AT_LAUNCH_VOLUME, "configuredAtLaunch": true }]),
        ),
        (HOST_FAMILY, json!([{ "name": HOST_VOLUME, "host": {} }])),
        (
            MIXED_FAMILY,
            json!([
                { "name": HOST_VOLUME, "host": {} },
                { "name": AT_LAUNCH_VOLUME, "configuredAtLaunch": true },
            ]),
        ),
    ] {
        ecs_ok(
            router,
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "networkMode": "awsvpc",
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "256",
                "memory": "512",
                "volumes": volumes,
                "containerDefinitions": [container],
            }),
            "setup",
        )
        .await;
    }
}

/// The `RunTask` body a case sends: a Fargate placement into `subnet`, carrying
/// `volume_configurations` when the case names any.
fn run_task_body(
    family: &str,
    subnet: &str,
    volume_configurations: Option<Value>,
    count: u32,
) -> Value {
    let mut body = json!({
        "cluster": CLUSTER,
        "taskDefinition": family,
        "launchType": "FARGATE",
        "count": count,
        "networkConfiguration": { "awsvpcConfiguration": { "subnets": [subnet] } },
    });
    if let Some(configurations) = volume_configurations {
        body["volumeConfigurations"] = configurations;
    }
    body
}

/// One managed-volume configuration, carrying only the members the case names.
fn managed_volume(
    name: &str,
    size_in_gib: Option<u32>,
    volume_type: Option<&str>,
    tags: &[(&str, &str)],
) -> Value {
    let mut managed = json!({ "roleArn": ROLE_ARN });
    if let Some(size) = size_in_gib {
        managed["sizeInGiB"] = json!(size);
    }
    if let Some(volume_type) = volume_type {
        managed["volumeType"] = json!(volume_type);
    }
    if !tags.is_empty() {
        managed["tagSpecifications"] = json!([{
            "resourceType": "volume",
            "tags": tags
                .iter()
                .map(|(key, value)| json!({ "key": key, "value": value }))
                .collect::<Vec<Value>>(),
        }]);
    }
    json!([{ "name": name, "managedEBSVolume": managed }])
}

/// The sentence a launch earns for a `configuredAtLaunch` volume it configures nothing for —
/// worded as Amazon ECS words the same class of refusal for the same operation ("Network
/// Configuration must be provided when networkMode 'awsvpc' is specified.").
fn missing_configuration(volume_name: &str) -> String {
    format!(
        "Volume Configuration must be provided when the volume '{volume_name}' is \
         configuredAtLaunch."
    )
}

/// The sentence a launch earns for a configuration that names no `configuredAtLaunch`
/// volume of the revision it runs — `RunTaskRequest$volumeConfigurations`: "The `name` of
/// the volume must match the `name` from the task definition".
fn unmatched_configuration(volume_name: &str) -> String {
    format!(
        "The name of the volume '{volume_name}' must match the name of a volume that was \
         configuredAtLaunch in the task definition."
    )
}

/// The sentence a launch earns for a managed volume that states neither size nor snapshot:
/// `TaskManagedEBSVolumeConfiguration$sizeInGiB`'s own rule.
const SIZE_REQUIRED: &str = "You must specify either a volume size or a snapshot ID.";

/// The sentence a launch earns for a tag specification using one of the reserved keys:
/// `EBSTagSpecification$tags`'s own rule.
const RESERVED_TAG: &str =
    "AmazonECSCreated and AmazonECSManaged are reserved tags that can't be used.";

/// The sentence a launch earns for more volume configurations than one launch may carry —
/// `Volume`: "only one volume configured at launch is supported" — in this API's wording for
/// a bounded list.
const TOO_MANY_CONFIGURATIONS: &str = "volumeConfigurations can have at most 1 items.";

/// The `details` of a task attachment, as the key/value pairs a client reads them as.
fn details_of(attachment: &Value) -> Vec<(String, String)> {
    attachment["details"]
        .as_array()
        .map(|details| {
            details
                .iter()
                .map(|detail| {
                    (
                        detail["name"].as_str().unwrap_or_default().to_string(),
                        detail["value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The value the attachment's `details` entry named `name` carries.
fn detail(attachment: &Value, name: &str) -> Option<String> {
    details_of(attachment)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// The `AmazonElasticBlockStorage` attachments of one task view.
fn ebs_attachments(task: &Value) -> Vec<Value> {
    task["attachments"]
        .as_array()
        .map(|attachments| {
            attachments
                .iter()
                .filter(|attachment| attachment["type"].as_str() == Some(EBS_ATTACHMENT_TYPE))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The tasks `DescribeTasks` reports for `task_arns`.
async fn describe_tasks(
    router: &MultiServiceRouter,
    task_arns: &[String],
    case: &str,
) -> Vec<Value> {
    let read = ecs_ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": task_arns }),
        case,
    )
    .await;
    read["tasks"].as_array().cloned().unwrap_or_default()
}

/// The `DescribeVolumes` answer for the volumes carrying the tag `key`=`value`.
async fn volumes_tagged(router: &MultiServiceRouter, key: &str, value: &str) -> String {
    ec2(
        router,
        "DescribeVolumes",
        &[
            ("Filter.1.Name", &format!("tag:{key}")),
            ("Filter.1.Value.1", value),
        ],
    )
    .await
}

/// The `DescribeVolumes` answer for ONE volume, which is what every per-volume assertion
/// reads: the volume list's own `<item>`s carry further `<item>`s (the volume's tags, its
/// attachments), so a flat reader cannot scope a read to one entry of a multi-volume
/// answer — asking for one volume at a time is what makes each read unambiguous.
async fn describe_volume(router: &MultiServiceRouter, volume_id: &str) -> String {
    ec2(router, "DescribeVolumes", &[("VolumeId.1", volume_id)]).await
}

/// How many volumes a `DescribeVolumes` answer reports: `Volume$CreateTime` is a member of
/// the volume itself and of nothing nested inside it (an attachment carries `attachTime`),
/// so counting it counts the entries of the list.
fn reported_volumes(answer: &str) -> usize {
    elements(answer, "createTime").len()
}

/// The volume ids a `DescribeVolumes` answer names, deduplicated: each volume reports its
/// own id, and each attachment of it repeats that same id.
fn reported_volume_ids(answer: &str) -> Vec<String> {
    let mut ids = elements(answer, "volumeId");
    ids.sort();
    ids.dedup();
    ids
}

/// The tags one volume's answer carries, as key/value pairs. The `tagSet` holds flat
/// `<item><key/><value/></item>` entries, so the entries are read off that block alone.
fn volume_tags(answer: &str) -> Vec<(String, String)> {
    let Some(tag_set) = opt_element(answer, "tagSet") else {
        return Vec::new();
    };
    items_of(&tag_set)
        .into_iter()
        .filter_map(|tag| {
            let key = elements(tag, "key").into_iter().next()?;
            let value = elements(tag, "value").into_iter().next()?;
            Some((key, value))
        })
        .collect()
}

#[tokio::test]
async fn a_launch_that_does_not_configure_its_at_launch_volume_is_refused() {
    let router = emulator();
    let subnet = ec2_subnet(&router).await;
    seed_ecs(&router).await;

    // (case, the revision it runs, the `volumeConfigurations` it carries, the sentence)
    let rows: Vec<(&str, &str, Option<Value>, String)> = vec![
        (
            "no volumeConfigurations at all",
            AT_LAUNCH_FAMILY,
            None,
            missing_configuration(AT_LAUNCH_VOLUME),
        ),
        (
            "an empty volumeConfigurations list",
            AT_LAUNCH_FAMILY,
            Some(json!([])),
            missing_configuration(AT_LAUNCH_VOLUME),
        ),
        (
            "a configuration naming another volume",
            AT_LAUNCH_FAMILY,
            Some(managed_volume("other", Some(4), None, &[])),
            missing_configuration(AT_LAUNCH_VOLUME),
        ),
        (
            "a configuration for a revision whose volume is not configured at launch",
            HOST_FAMILY,
            Some(managed_volume(HOST_VOLUME, Some(4), None, &[])),
            unmatched_configuration(HOST_VOLUME),
        ),
        (
            "a configuration naming the host volume of the mixed revision",
            MIXED_FAMILY,
            Some(managed_volume(HOST_VOLUME, Some(4), None, &[])),
            // The volume the revision DID configure at launch is unconfigured either way,
            // and that is the first thing the launch is refused for.
            missing_configuration(AT_LAUNCH_VOLUME),
        ),
        (
            "a managed volume with neither a size nor a snapshot",
            AT_LAUNCH_FAMILY,
            Some(managed_volume(AT_LAUNCH_VOLUME, None, None, &[])),
            SIZE_REQUIRED.to_string(),
        ),
        (
            "a configuration carrying no managedEBSVolume at all",
            AT_LAUNCH_FAMILY,
            Some(json!([{ "name": AT_LAUNCH_VOLUME }])),
            missing_configuration(AT_LAUNCH_VOLUME),
        ),
        (
            "more configurations than one launch may carry",
            AT_LAUNCH_FAMILY,
            Some(json!([
                managed_volume(AT_LAUNCH_VOLUME, Some(4), None, &[])[0].clone(),
                managed_volume("other", Some(4), None, &[])[0].clone(),
            ])),
            TOO_MANY_CONFIGURATIONS.to_string(),
        ),
        (
            "a tag specification using the reserved AmazonECSCreated key",
            AT_LAUNCH_FAMILY,
            Some(managed_volume(
                AT_LAUNCH_VOLUME,
                Some(4),
                None,
                &[(RESERVED_CREATED_TAG, "mine")],
            )),
            RESERVED_TAG.to_string(),
        ),
        (
            "a tag specification using the reserved AmazonECSManaged key",
            AT_LAUNCH_FAMILY,
            Some(managed_volume(
                AT_LAUNCH_VOLUME,
                Some(4),
                None,
                &[("Owner", "me"), (RESERVED_MANAGED_TAG, "true")],
            )),
            RESERVED_TAG.to_string(),
        ),
    ];

    for (case, family, configurations, expected) in rows {
        let (status, body) = ecs(
            &router,
            "RunTask",
            run_task_body(family, &subnet, configurations, 1),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the launch to be refused, got {status} with {body}"
        );
        assert!(
            body["__type"]
                .as_str()
                .is_some_and(|wire| wire.contains("InvalidParameterException")),
            "case {case:?}: expected an InvalidParameterException, got {body}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the refusal {expected:?}, got {body}"
        );
    }

    // A refused launch places NOTHING: no task of the cluster, and no volume in the account.
    let listed = ecs_ok(
        &router,
        "ListTasks",
        json!({ "cluster": CLUSTER }),
        "the refused launches",
    )
    .await;
    assert!(
        listed["taskArns"]
            .as_array()
            .is_none_or(|arns| arns.is_empty()),
        "case \"the refused launches\": a refused launch must place no task, got {listed}"
    );
    let volumes = ec2(&router, "DescribeVolumes", &[]).await;
    assert!(
        elements(&volumes, "volumeId").is_empty(),
        "case \"the refused launches\": a refused launch must create no volume, got {volumes}"
    );
}

/// One accepted row: what the launch configures, and what the account must then hold.
struct AcceptedCase {
    name: &'static str,
    /// The revision the row launches — the one that declares only the at-launch volume, or
    /// the one that declares a host volume beside it.
    family: &'static str,
    /// The size the configuration asks for.
    size_in_gib: u32,
    /// The volume type it asks for, or `None` to take EC2's own default.
    volume_type: Option<&'static str>,
    /// The type every created volume must then report.
    expected_volume_type: &'static str,
    /// The tags the configuration asks for, beside the unique one every row carries.
    extra_tags: &'static [(&'static str, &'static str)],
    /// How many tasks the launch places — "one volume created for each task".
    count: u32,
}

/// The rows: they differ along the size, whether the configuration names a volume type at
/// all, how many tags it asks for, whether the revision declares another volume beside the
/// one configured at launch, and how many tasks the one launch places.
fn accepted_cases() -> Vec<AcceptedCase> {
    vec![
        AcceptedCase {
            name: "one task, the default volume type, one tag",
            family: AT_LAUNCH_FAMILY,
            size_in_gib: 1,
            volume_type: None,
            expected_volume_type: DEFAULT_VOLUME_TYPE,
            extra_tags: &[],
            count: 1,
        },
        AcceptedCase {
            name: "one task, a gp3 volume, two tags",
            family: AT_LAUNCH_FAMILY,
            size_in_gib: 10,
            volume_type: Some("gp3"),
            expected_volume_type: "gp3",
            extra_tags: &[("Owner", "me")],
            count: 1,
        },
        AcceptedCase {
            name: "one task, an st1 volume at its smallest published size",
            family: AT_LAUNCH_FAMILY,
            // `TaskManagedEBSVolumeConfiguration$sizeInGiB`: "st1 and sc1: 125-16,384".
            size_in_gib: 125,
            volume_type: Some("st1"),
            expected_volume_type: "st1",
            extra_tags: &[],
            count: 1,
        },
        AcceptedCase {
            name: "a revision declaring a host volume beside the one configured at launch",
            family: MIXED_FAMILY,
            size_in_gib: 2,
            volume_type: None,
            expected_volume_type: DEFAULT_VOLUME_TYPE,
            extra_tags: &[],
            count: 1,
        },
        AcceptedCase {
            name: "two tasks of one launch, one volume each",
            family: AT_LAUNCH_FAMILY,
            size_in_gib: 3,
            volume_type: None,
            expected_volume_type: DEFAULT_VOLUME_TYPE,
            extra_tags: &[],
            count: 2,
        },
    ]
}

#[tokio::test]
async fn a_configured_at_launch_volume_is_created_in_ec2_and_attached_to_its_task() {
    let router = emulator();
    let subnet = ec2_subnet(&router).await;
    seed_ecs(&router).await;

    for (index, test) in accepted_cases().iter().enumerate() {
        let case = test.name;
        // The tag this row finds its own volumes by, which is also what proves the
        // configuration's tags reached the volume EC2 holds.
        let run_tag = format!("ebsrun{index}");
        let mut tags: Vec<(&str, &str)> = vec![("Run", run_tag.as_str())];
        tags.extend_from_slice(test.extra_tags);
        let launched = ecs_ok(
            &router,
            "RunTask",
            run_task_body(
                test.family,
                &subnet,
                Some(managed_volume(
                    AT_LAUNCH_VOLUME,
                    Some(test.size_in_gib),
                    test.volume_type,
                    &tags,
                )),
                test.count,
            ),
            case,
        )
        .await;
        let placed = launched["tasks"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            placed.len(),
            test.count as usize,
            "case {case:?}: expected {} task(s) placed, got {launched}",
            test.count
        );
        let task_arns: Vec<String> = placed
            .iter()
            .map(|task| task["taskArn"].as_str().unwrap_or_default().to_string())
            .collect();

        // Each task reports ONE Elastic Block Storage attachment, carrying the details the
        // member's own doc lists for that type — the volume id among them, so a client can
        // resolve the volume the task runs with.
        let described = describe_tasks(&router, &task_arns, case).await;
        assert_eq!(
            described.len(),
            test.count as usize,
            "case {case:?}: expected every placed task to be described, got {described:?}"
        );
        let mut attached_volume_ids = Vec::new();
        for task in &described {
            let attachments = ebs_attachments(task);
            assert_eq!(
                attachments.len(),
                1,
                "case {case:?}: expected one {EBS_ATTACHMENT_TYPE} attachment, got {task}"
            );
            let attachment = &attachments[0];
            assert_eq!(
                detail(attachment, DETAIL_VOLUME_NAME),
                Some(AT_LAUNCH_VOLUME.to_string()),
                "case {case:?}: expected the attachment to name the volume of the task \
                 definition, got {:?}",
                details_of(attachment)
            );
            assert_eq!(
                detail(attachment, DETAIL_ROLE_ARN),
                Some(ROLE_ARN.to_string()),
                "case {case:?}: expected the attachment to carry the configuration's role, \
                 got {:?}",
                details_of(attachment)
            );
            assert_eq!(
                detail(attachment, DETAIL_DELETE_ON_TERMINATION),
                Some("true".to_string()),
                "case {case:?}: a configuration stating no termination policy takes the \
                 documented default, got {:?}",
                details_of(attachment)
            );
            let volume_id = detail(attachment, DETAIL_VOLUME_ID).unwrap_or_default();
            assert!(
                volume_id.starts_with("vol-"),
                "case {case:?}: expected the attachment to name the EBS volume EC2 minted, \
                 got {:?}",
                details_of(attachment)
            );
            attached_volume_ids.push(volume_id);
        }
        attached_volume_ids.sort();
        attached_volume_ids.dedup();
        assert_eq!(
            attached_volume_ids.len(),
            test.count as usize,
            "case {case:?}: one volume is created for each task, got {attached_volume_ids:?}"
        );

        // And the volumes the account holds under this row's tag are exactly those: EC2's own
        // DescribeVolumes resolves the ids the tasks report, with the configuration the
        // launch asked for.
        let held = volumes_tagged(&router, "Run", &run_tag).await;
        assert_eq!(
            reported_volumes(&held),
            test.count as usize,
            "case {case:?}: expected {} volume(s) tagged {run_tag:?}, got {held}",
            test.count
        );
        assert_eq!(
            reported_volume_ids(&held),
            attached_volume_ids,
            "case {case:?}: the volumes EC2 holds are the ones the tasks are attached to, \
             got {held}"
        );
        for volume_id in &attached_volume_ids {
            let item = describe_volume(&router, volume_id).await;
            assert_eq!(
                elements(&item, "size").into_iter().next(),
                Some(test.size_in_gib.to_string()),
                "case {case:?}: expected a volume of {} GiB, got {item}",
                test.size_in_gib
            );
            assert_eq!(
                elements(&item, "volumeType").into_iter().next(),
                Some(test.expected_volume_type.to_string()),
                "case {case:?}: expected a {} volume, got {item}",
                test.expected_volume_type
            );
            assert_eq!(
                elements(&item, "availabilityZone").into_iter().next(),
                Some(ZONE.to_string()),
                "case {case:?}: a task's volume is created in the zone the task runs in, \
                 got {item}"
            );
            // The caller's tags, and the two Amazon ECS applies itself.
            let carried = volume_tags(&item);
            for (key, value) in &tags {
                assert!(
                    carried
                        .iter()
                        .any(|(held_key, held_value)| held_key == key && held_value == value),
                    "case {case:?}: expected the tag {key}={value} on the volume, got \
                     {carried:?}"
                );
            }
            assert_eq!(
                carried
                    .iter()
                    .find(|(key, _)| key == RESERVED_MANAGED_TAG)
                    .map(|(_, value)| value.as_str()),
                Some("true"),
                "case {case:?}: expected the service-managed marker on the volume, got \
                 {carried:?}"
            );
            let created_for = carried
                .iter()
                .find(|(key, _)| key == RESERVED_CREATED_TAG)
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            assert!(
                task_arns.contains(&created_for),
                "case {case:?}: expected the volume to be tagged with the task that created \
                 it, got {carried:?}"
            );
            // EC2 reports it attached to that task rather than to an instance: "if the volume
            // is attached to an Amazon Web Services-managed resource, this parameter returns
            // null" of `InstanceId` and `Device`.
            let attachment = opt_element(&item, "attachmentSet").unwrap_or_default();
            let attachment_items = items_of(&attachment);
            assert_eq!(
                attachment_items.len(),
                1,
                "case {case:?}: expected the volume to report one attachment, got {item}"
            );
            let associated = elements(attachment_items[0], "associatedResource")
                .into_iter()
                .next()
                .unwrap_or_default();
            assert!(
                task_arns.contains(&associated),
                "case {case:?}: expected the attachment to name the task it was created for, \
                 got {item}"
            );
            assert_eq!(
                elements(attachment_items[0], "instanceOwningService")
                    .into_iter()
                    .next(),
                Some(ECS_PRINCIPAL.to_string()),
                "case {case:?}: expected the owning service principal, got {item}"
            );
            assert_eq!(
                opt_element(attachment_items[0], "instanceId"),
                None,
                "case {case:?}: a volume attached to a managed resource reports no instance, \
                 got {item}"
            );
            assert_eq!(
                opt_element(attachment_items[0], "device"),
                None,
                "case {case:?}: a volume attached to a managed resource reports no device, \
                 got {item}"
            );
        }
    }
}
