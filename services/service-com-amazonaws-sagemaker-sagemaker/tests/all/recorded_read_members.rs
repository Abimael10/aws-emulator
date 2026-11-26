// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the members a SageMaker READ answers with that this service's
//! on-disk Smithy model does not declare — a feature group's online store, a hub's
//! provenance, a training job's run and a Studio domain's app network access — and for the
//! two modelled members of a training job's read that the service RESOLVES rather than
//! echoes.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, on the
//! harness `tests/all/inference_deployment_lifecycle.rs` owns, so what these tests exercise
//! is what a real SDK client gets.
//!
//! The rule under test, over TABLES of inputs rather than one recorded call: a read reports
//! every member the real service's response carries for the resource it read — the ones the
//! on-disk model declares and the ones it does not — and each such member is read off the
//! stored resource, so it follows the resource's own configuration rather than being
//! constant across it.

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{str_field, Emulator, ACCOUNT};
use crate::training_job_admission::{image_specification, training_body};

/// A role ARN matching `RoleArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`).
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/FeatureStoreRole")
}

/// A `TrainingPlanArn` matching `ResourceConfig$TrainingPlanArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:training-plan/`, len 50..2048): the
/// reservation a job draws its capacity from.
fn training_plan_arn() -> String {
    format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:training-plan/reserved-capacity")
}

/// A valid `CreateFeatureGroup` body for `name`, with `extra`'s members laid over it; a
/// member of `extra` whose value is `null` is REMOVED, which is how a row drops an
/// otherwise-present member.
fn feature_group_body(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "FeatureGroupName": name,
        "RecordIdentifierFeatureName": "customer-id",
        "EventTimeFeatureName": "event-time",
        "FeatureDefinitions": [
            { "FeatureName": "customer-id", "FeatureType": "String" },
            { "FeatureName": "event-time", "FeatureType": "String" },
        ],
        "OnlineStoreConfig": { "EnableOnlineStore": true },
    });
    let object = body.as_object_mut().expect("a JSON object body");
    for (key, value) in extra.as_object().expect("a JSON object of extra members") {
        if value.is_null() {
            object.remove(key);
        } else {
            object.insert(key.clone(), value.clone());
        }
    }
    body
}

/// RULE: `DescribeFeatureGroup` reports the state of the ONLINE STORE a group keeps — the
/// mode it may be read and written in, the stores that mirror it and how many items it
/// holds — and a group that keeps no online store reports none of them, exactly as it
/// reports no online-store size.
#[tokio::test]
async fn describe_feature_group_reports_the_online_store_a_group_keeps() {
    // Rows differ along what the rule generalises over: whether the group keeps an online
    // store at all (enabled, disabled, no configuration), which low-latency tier it keeps
    // it in, whether its records expire, and whether it also keeps an offline store.
    let rows: Vec<(&str, Value)> = vec![
        ("online-store-on", json!({})),
        (
            "online-store-in-memory-with-a-ttl",
            json!({
                "OnlineStoreConfig": {
                    "EnableOnlineStore": true,
                    "StorageType": "InMemory",
                    "TtlDuration": { "Unit": "Days", "Value": 7 },
                },
            }),
        ),
        (
            "online-store-and-offline-store",
            json!({
                "OnlineStoreConfig": { "EnableOnlineStore": true, "StorageType": "Standard" },
                "OfflineStoreConfig": { "S3StorageConfig": { "S3Uri": "s3://features/offline/" } },
                "RoleArn": role_arn(),
            }),
        ),
        (
            "online-store-off",
            json!({ "OnlineStoreConfig": { "EnableOnlineStore": false } }),
        ),
        (
            "no-online-store-config",
            json!({
                "OnlineStoreConfig": null,
                "OfflineStoreConfig": { "S3StorageConfig": { "S3Uri": "s3://features/offline/" } },
                "RoleArn": role_arn(),
            }),
        ),
    ];

    for (case, extra) in rows {
        let emulator = Emulator::new();
        let request = feature_group_body(case, extra);
        emulator.ok("CreateFeatureGroup", &request).await;

        let described = emulator
            .ok("DescribeFeatureGroup", &json!({ "FeatureGroupName": case }))
            .await;
        // The group keeps an online store exactly when its configuration enabled one.
        let keeps_online_store = request
            .get("OnlineStoreConfig")
            .and_then(|config| config.get("EnableOnlineStore"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        assert_eq!(
            described
                .get("OnlineStoreReadWriteType")
                .and_then(Value::as_str),
            keeps_online_store.then_some("ReadWrite"),
            "case {case}: OnlineStoreReadWriteType follows whether the group keeps an \
             online store, got {described:?}"
        );
        assert_eq!(
            described
                .get("OnlineStoreReplicas")
                .and_then(Value::as_array),
            keeps_online_store.then_some(&Vec::new()),
            "case {case}: a group with no replica configured reports the EMPTY replica \
             list, got {described:?}"
        );
        assert_eq!(
            described
                .get("OnlineStoreTotalItemCount")
                .and_then(Value::as_i64),
            keeps_online_store.then_some(0),
            "case {case}: OnlineStoreTotalItemCount follows whether the group keeps an \
             online store, got {described:?}"
        );
        // The item count and the size report the same (empty) store, so they appear
        // together: no reader can see one and not the other.
        assert_eq!(
            described
                .get("OnlineStoreTotalSizeBytes")
                .and_then(Value::as_i64),
            keeps_online_store.then_some(0),
            "case {case}: OnlineStoreTotalSizeBytes, got {described:?}"
        );
        // Every member the model DOES declare is still reported beside the three above.
        assert_eq!(
            str_field(&described, "FeatureGroupName"),
            case,
            "case {case}: the modelled members must survive, got {described:?}"
        );
        assert_eq!(
            described.get("OnlineStoreConfig"),
            request.get("OnlineStoreConfig"),
            "case {case}: OnlineStoreConfig must read back as created, got {described:?}"
        );
        assert_eq!(
            described.get("NextToken").and_then(Value::as_str),
            Some(""),
            "case {case}: NextToken, got {described:?}"
        );
    }
}

/// RULE: `DescribeTrainingJob` reports the members the real service carries for the job it
/// RUNS: the Elastic Fabric Adapter is not switched off, the run uploads its model
/// artifact under a prefix carrying the job's name, and the job draws on reserved capacity
/// exactly when its resource configuration names a training plan — all beside the modelled
/// members of the two configurations they sit in.
#[tokio::test]
async fn describe_training_job_reports_the_recorded_members_of_the_job_it_runs() {
    // Rows differ along what the rule generalises over: whether the resource configuration
    // names a training plan (the reservation), what the output configuration states, and
    // how much ML compute the job asks for.
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "on-demand-compute",
            json!({}),
            json!({ "InstanceType": "ml.m5.large", "InstanceCount": 1, "VolumeSizeInGB": 10 }),
        ),
        (
            "reserved-capacity",
            json!({}),
            json!({
                "InstanceType": "ml.m5.large",
                "InstanceCount": 2,
                "VolumeSizeInGB": 20,
                "TrainingPlanArn": training_plan_arn(),
            }),
        ),
        (
            "output-configuration-stated",
            json!({
                "OutputDataConfig": {
                    "S3OutputPath": "s3://demo/output/",
                    "CompressionType": "NONE",
                    "KmsKeyId": "alias/training-output",
                },
            }),
            json!({ "InstanceType": "ml.m5.large", "InstanceCount": 1, "VolumeSizeInGB": 10 }),
        ),
        (
            "warm-pool-kept-alive",
            json!({}),
            json!({
                "InstanceType": "ml.m5.large",
                "InstanceCount": 1,
                "VolumeSizeInGB": 10,
                "KeepAlivePeriodInSeconds": 600,
            }),
        ),
    ];

    for (case, extra, resources) in rows {
        let emulator = Emulator::new();
        let mut body = training_body(case, image_specification(), "s3://demo/output/");
        body["ResourceConfig"] = resources.clone();
        for (key, value) in extra.as_object().expect("a JSON object of extra members") {
            body[key.as_str()] = value.clone();
        }
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": case }))
            .await;

        assert_eq!(
            described.get("DisableEFA").and_then(Value::as_bool),
            Some(false),
            "case {case}: DisableEFA, got {described:?}"
        );
        for member in ["DisableModelUpload", "RemoveJobNameFromS3OutputPath"] {
            assert_eq!(
                described
                    .pointer(&format!("/OutputDataConfig/{member}"))
                    .and_then(Value::as_bool),
                Some(false),
                "case {case}: the run uploads its artifact under the job's name, so \
                 OutputDataConfig.{member} is false, got {described:?}"
            );
        }
        assert_eq!(
            described
                .pointer("/ResourceConfig/UseReservedCapacity")
                .and_then(Value::as_bool),
            Some(resources.get("TrainingPlanArn").is_some()),
            "case {case}: UseReservedCapacity follows whether the configuration names a \
             training plan, got {described:?}"
        );
        // The configurations the recorded members sit in still carry every member the
        // model declares, each exactly as the service resolved it: a member written
        // beside them replaces none of them.
        let output = body
            .get("OutputDataConfig")
            .and_then(Value::as_object)
            .expect("an output configuration")
            .clone();
        for (key, value) in output {
            assert_eq!(
                described.pointer(&format!("/OutputDataConfig/{key}")),
                Some(&value),
                "case {case}: OutputDataConfig.{key} must be replayed as stored, \
                 got {described:?}"
            );
        }
        for (key, value) in resources.as_object().expect("a resource configuration") {
            assert_eq!(
                described.pointer(&format!("/ResourceConfig/{key}")),
                Some(value),
                "case {case}: ResourceConfig.{key} must be replayed as stored, \
                 got {described:?}"
            );
        }
        assert_eq!(
            str_field(&described, "TrainingJobName"),
            case,
            "case {case}: the modelled members must survive, got {described:?}"
        );
    }
}

/// RULE: a training job's input channels are reported as the service resolved them, so a
/// channel that states no record wrapping runs — and reads back — with the `RecordWrapper`
/// value that wraps nothing, while one that states a wrapping keeps exactly that.
#[tokio::test]
async fn a_training_job_channel_reports_the_record_wrapping_it_runs_under() {
    // The rows span the `RecordWrapper` catalogue the model declares
    // (`smithy_query(query="shape",shape="RecordWrapper")`: `None` | `RecordIO`), each
    // stated and absent, over one and several channels.
    let rows: Vec<(&str, Vec<Value>)> = vec![
        (
            "no-wrapping-stated",
            vec![channel("train", None)],
        ),
        (
            "record-io-stated",
            vec![channel("train", Some("RecordIO"))],
        ),
        (
            "none-stated",
            vec![channel("train", Some("None"))],
        ),
        (
            "several-channels-mixed",
            vec![
                channel("train", Some("RecordIO")),
                channel("validation", None),
                channel("test", Some("None")),
            ],
        ),
    ];

    for (case, channels) in rows {
        let emulator = Emulator::new();
        let mut body = training_body(case, image_specification(), "s3://demo/output/");
        body["InputDataConfig"] = Value::Array(channels.clone());
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": case }))
            .await;
        let reported = described
            .get("InputDataConfig")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case}: expected InputDataConfig in {described}"));
        assert_eq!(
            reported.len(),
            channels.len(),
            "case {case}: every channel must be reported, got {described:?}"
        );
        for (index, (sent, reported)) in channels.iter().zip(reported).enumerate() {
            assert_eq!(
                reported.get("RecordWrapperType").and_then(Value::as_str),
                Some(
                    sent.get("RecordWrapperType")
                        .and_then(Value::as_str)
                        .unwrap_or("None")
                ),
                "case {case} channel {index}: RecordWrapperType, got {reported:?}"
            );
            // The channel reads in the mode the job states, which is `File` here, so it is
            // not read in fast file mode. The whole catalogue, and a channel that overrides
            // the job's mode, are spanned by
            // `a_training_job_channel_reports_whether_it_reads_in_fast_file_mode`.
            assert_eq!(
                reported.get("EnableFFM").and_then(Value::as_bool),
                Some(false),
                "case {case} channel {index}: EnableFFM, got {reported:?}"
            );
            assert_eq!(
                reported.get("ChannelName"),
                sent.get("ChannelName"),
                "case {case} channel {index}: ChannelName, got {reported:?}"
            );
        }
    }
}

/// One `InputDataConfig` channel named `name`, with `wrapping` as its `RecordWrapperType`
/// when the row states one.
fn channel(name: &str, wrapping: Option<&str>) -> Value {
    let mut channel = json!({
        "ChannelName": name,
        "DataSource": {
            "S3DataSource": { "S3DataType": "S3Prefix", "S3Uri": format!("s3://demo/{name}/") },
        },
        "ContentType": "text/csv",
    });
    if let Some(wrapping) = wrapping {
        channel["RecordWrapperType"] = json!(wrapping);
    }
    channel
}

/// RULE: a channel is reported as reading in FAST FILE mode exactly when that is the mode it
/// RUNS in — its own `InputMode` when it states one, and otherwise the mode the algorithm
/// specification sets for the job.
#[tokio::test]
async fn a_training_job_channel_reports_whether_it_reads_in_fast_file_mode() {
    // The rows span the `TrainingInputMode` catalogue the model declares
    // (`smithy_query(query="shape",shape="TrainingInputMode")`: `Pipe` | `File` | `FastFile`)
    // at the specification level, each of them with the channel stating no mode of its own,
    // and then the override in both directions — so neither the specification's mode nor the
    // channel's can be ignored.
    // (case, the specification's TrainingInputMode, the channel's InputMode, EnableFFM)
    let rows: Vec<(&str, &str, Option<&str>, bool)> = vec![
        ("spec-file", "File", None, false),
        ("spec-pipe", "Pipe", None, false),
        ("spec-fastfile", "FastFile", None, true),
        ("channel-overrides-to-fastfile", "File", Some("FastFile"), true),
        ("channel-overrides-to-file", "FastFile", Some("File"), false),
        ("channel-overrides-to-pipe", "FastFile", Some("Pipe"), false),
        ("channel-states-the-same-mode", "FastFile", Some("FastFile"), true),
    ];

    for (case, spec_mode, channel_mode, expected) in rows {
        let emulator = Emulator::new();
        let mut body = training_body(case, image_specification(), "s3://demo/output/");
        body["AlgorithmSpecification"]["TrainingInputMode"] = json!(spec_mode);
        let mut only = channel("train", None);
        if let Some(mode) = channel_mode {
            only["InputMode"] = json!(mode);
        }
        body["InputDataConfig"] = json!([only]);
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": case }))
            .await;
        assert_eq!(
            described
                .pointer("/InputDataConfig/0/EnableFFM")
                .and_then(Value::as_bool),
            Some(expected),
            "case {case}: the channel runs in {}, so EnableFFM must be {expected}, \
             got {described:?}",
            channel_mode.unwrap_or(spec_mode)
        );
        // The mode the channel OVERRIDES with is reported as stated, and a channel that
        // overrides nothing reports none: the resolution above is the reader's, not a value
        // written onto the stored channel.
        assert_eq!(
            described
                .pointer("/InputDataConfig/0/InputMode")
                .and_then(Value::as_str),
            channel_mode,
            "case {case}: the channel's own InputMode, got {described:?}"
        );
        assert_eq!(
            described
                .pointer("/AlgorithmSpecification/TrainingInputMode")
                .and_then(Value::as_str),
            Some(spec_mode),
            "case {case}: the specification's TrainingInputMode, got {described:?}"
        );
    }
}

/// RULE: the metric definitions a training job is reported with are the ALGORITHM's: a job
/// that names a registered algorithm and states none of its own runs with that algorithm's
/// metric set, a job that states its own keeps exactly those, and a job whose algorithm
/// declares no metrics is reported with none.
#[tokio::test]
async fn a_training_job_reports_the_metric_set_of_the_algorithm_it_runs() {
    let algorithm_metrics = json!([
        { "Name": "train:rmse", "Regex": ".*\\[[0-9]+\\].*#011train-rmse:(\\S+).*" },
        { "Name": "validation:rmse", "Regex": ".*\\[[0-9]+\\].*#011validation-rmse:(\\S+).*" },
    ]);
    let stated_metrics = json!([{ "Name": "objective", "Regex": "objective=(\\S+)" }]);

    // Rows differ along what the rule generalises over: whether the job names a registered
    // algorithm or a container image, whether that algorithm declares a metric set, which
    // `ArnOrName` form the reference takes, and whether the request states metrics of its
    // own. The last column is the metric set the read must report.
    let rows: Vec<(&str, Option<Value>, bool, Option<Value>)> = vec![
        (
            "algorithm-with-metrics",
            Some(algorithm_metrics.clone()),
            false,
            Some(algorithm_metrics.clone()),
        ),
        (
            "algorithm-with-metrics-by-arn",
            Some(algorithm_metrics.clone()),
            true,
            Some(algorithm_metrics.clone()),
        ),
        ("algorithm-without-metrics", None, false, None),
        ("image-only", None, false, None),
    ];

    for (case, declared, by_arn, expected) in rows {
        let emulator = Emulator::new();
        let specification = match case {
            "image-only" => image_specification(),
            _ => {
                let arn = create_algorithm(&emulator, "metric-algo", declared.clone()).await;
                let reference = if by_arn {
                    arn
                } else {
                    "metric-algo".to_string()
                };
                json!({ "AlgorithmName": reference, "TrainingInputMode": "File" })
            }
        };
        let body = training_body(case, specification, "s3://demo/output/");
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": case }))
            .await;
        assert_eq!(
            described.pointer("/AlgorithmSpecification/MetricDefinitions"),
            expected.as_ref(),
            "case {case}: the metric set reported is the algorithm's, got {described:?}"
        );
    }

    // A job that states metrics of its own keeps exactly those, whatever the algorithm it
    // names declares.
    let emulator = Emulator::new();
    create_algorithm(&emulator, "metric-algo", Some(algorithm_metrics)).await;
    let mut specification = json!({
        "AlgorithmName": "metric-algo",
        "TrainingInputMode": "File",
    });
    specification["MetricDefinitions"] = stated_metrics.clone();
    let body = training_body("stated-metrics", specification, "s3://demo/output/");
    emulator.ok("CreateTrainingJob", &body).await;
    let described = emulator
        .ok(
            "DescribeTrainingJob",
            &json!({ "TrainingJobName": "stated-metrics" }),
        )
        .await;
    assert_eq!(
        described.pointer("/AlgorithmSpecification/MetricDefinitions"),
        Some(&stated_metrics),
        "case stated-metrics: the request's own metric set must be kept, got {described:?}"
    );
}

/// An execution role inside `RoleArn`'s modelled pattern, for the `DefaultUserSettings`
/// every `CreateDomain` carries.
const STUDIO_EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/StudioRole";

/// A `CreateDomain` request: the members a SageMaker AI domain must carry — the name, the
/// authentication mode, the execution role and the network its traffic goes through — with
/// `extra`'s members laid over it.
fn domain_body(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "DomainName": name,
        "AuthMode": "IAM",
        "DefaultUserSettings": { "ExecutionRole": STUDIO_EXECUTION_ROLE },
        "VpcId": "vpc-0a1b2c3d",
        "SubnetIds": ["subnet-0a1b2c3d"],
    });
    for (member, value) in extra.as_object().expect("a JSON object of extra members") {
        body[member.as_str()] = value.clone();
    }
    body
}

/// The access a domain whose `AppNetworkAccessType` is `access_type` is routed with, as the
/// read must report it; `None` is a create that stated no type, which the member's own doc
/// documents as defaulting to `PublicInternetOnly`.
///
/// Written here rather than asked of the implementation, over the whole catalogue
/// `AppNetworkAccessType` declares (`smithy_query(query="shape",
/// shape="AppNetworkAccessType")`: `PublicInternetOnly` | `VpcOnly`).
fn expected_access(access_type: Option<&str>) -> &'static str {
    match access_type {
        // All traffic is through the VPC and subnets the create named, so nothing reaches
        // the internet directly.
        Some("VpcOnly") => "VpcOnly",
        // Non-EFS traffic is through the VPC SageMaker manages, which allows direct
        // internet access.
        _ => "DirectInternetOnly",
    }
}

/// RULE: `DescribeDomain` reports how a domain's non-EFS traffic is routed under the member
/// the service serves for it, `AppNetworkAccess`, beside the modelled `AppNetworkAccessType`
/// that names the mode routing it that way — projected from the access type the domain
/// carries, which is the one its create named or, for a create that named none, the type the
/// member documents as its default.
#[tokio::test]
async fn describe_domain_reports_the_access_the_domain_is_routed_with() {
    // Rows span the whole `AppNetworkAccessType` catalogue, stated and left unsaid, and
    // differ along what else the create carries: the subnet count, the KMS key, the tag
    // propagation and the security-group management the VPC-only mode admits.
    let rows: Vec<(&str, Option<&str>, Value)> = vec![
        ("no-access-type-stated", None, json!({})),
        (
            "public-internet-only",
            Some("PublicInternetOnly"),
            json!({ "AppNetworkAccessType": "PublicInternetOnly" }),
        ),
        (
            "vpc-only",
            Some("VpcOnly"),
            json!({ "AppNetworkAccessType": "VpcOnly" }),
        ),
        (
            "vpc-only-with-service-managed-security-groups",
            Some("VpcOnly"),
            json!({
                "AppNetworkAccessType": "VpcOnly",
                "AppSecurityGroupManagement": "Service",
                "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d", "subnet-2b3c4d5e"],
            }),
        ),
        (
            "no-access-type-stated-with-every-other-optional-member",
            None,
            json!({
                "HomeEfsFileSystemCreation": "Enabled",
                "TagPropagation": "ENABLED",
                "KmsKeyId": "alias/studio-efs",
                "Tags": [{ "Key": "team", "Value": "ml" }],
            }),
        ),
    ];

    for (case, access_type, extra) in rows {
        let emulator = Emulator::new();
        let created = emulator.ok("CreateDomain", &domain_body(case, extra)).await;
        let domain_id = str_field(&created, "DomainId");

        let described = emulator
            .ok("DescribeDomain", &json!({ "DomainId": domain_id }))
            .await;
        assert_eq!(
            described.get("AppNetworkAccess").and_then(Value::as_str),
            Some(expected_access(access_type)),
            "case {case:?}: AppNetworkAccess must report the access the domain is routed \
             with, got {described:?}"
        );
        // The member the model DOES declare is still reported beside it, carrying the type
        // the create named or the documented default.
        assert_eq!(
            described.get("AppNetworkAccessType").and_then(Value::as_str),
            Some(access_type.unwrap_or("PublicInternetOnly")),
            "case {case:?}: AppNetworkAccessType, got {described:?}"
        );
        // And no other member of the projection is displaced by it.
        assert_eq!(
            str_field(&described, "DomainName"),
            case,
            "case {case:?}: the modelled members must survive, got {described:?}"
        );
        assert_eq!(
            str_field(&described, "DomainId"),
            domain_id,
            "case {case:?}: the domain reads back under its own id, got {described:?}"
        );
    }
}

/// RULE: the access a domain is reported as routed with follows the access type the domain
/// CARRIES for the whole life of the domain, so an `UpdateDomain` that moves the domain from
/// one mode to the other moves the reported access with it, and an update that changes
/// something else leaves it as it stands.
#[tokio::test]
async fn an_updated_domain_reports_the_access_its_new_mode_routes_it_with() {
    // Rows cross the catalogue in both directions, add the update that re-states the mode
    // the domain already has, and the update that touches another member entirely. The last
    // column is the access type the domain carries after the update.
    let rows: Vec<(&str, Option<&str>, Value, Option<&str>)> = vec![
        (
            "default-to-vpc-only",
            None,
            json!({ "AppNetworkAccessType": "VpcOnly" }),
            Some("VpcOnly"),
        ),
        (
            "vpc-only-to-public-internet-only",
            Some("VpcOnly"),
            json!({ "AppNetworkAccessType": "PublicInternetOnly" }),
            Some("PublicInternetOnly"),
        ),
        (
            "public-internet-only-to-vpc-only",
            Some("PublicInternetOnly"),
            json!({ "AppNetworkAccessType": "VpcOnly" }),
            Some("VpcOnly"),
        ),
        (
            "mode-restated",
            Some("VpcOnly"),
            json!({ "AppNetworkAccessType": "VpcOnly" }),
            Some("VpcOnly"),
        ),
        (
            "another-member-updated",
            Some("VpcOnly"),
            json!({ "TagPropagation": "ENABLED" }),
            Some("VpcOnly"),
        ),
    ];

    for (case, created_with, update, after) in rows {
        let emulator = Emulator::new();
        let extra = match created_with {
            Some(access_type) => json!({ "AppNetworkAccessType": access_type }),
            None => json!({}),
        };
        let created = emulator.ok("CreateDomain", &domain_body(case, extra)).await;
        let domain_id = str_field(&created, "DomainId");

        let mut request = json!({ "DomainId": domain_id });
        for (member, value) in update.as_object().expect("a JSON object of members") {
            request[member.as_str()] = value.clone();
        }
        emulator.ok("UpdateDomain", &request).await;

        let described = emulator
            .ok("DescribeDomain", &json!({ "DomainId": domain_id }))
            .await;
        assert_eq!(
            described.get("AppNetworkAccess").and_then(Value::as_str),
            Some(expected_access(after)),
            "case {case:?}: the updated domain's AppNetworkAccess, got {described:?}"
        );
        assert_eq!(
            described.get("AppNetworkAccessType").and_then(Value::as_str),
            after,
            "case {case:?}: the updated domain's AppNetworkAccessType, got {described:?}"
        );
    }
}

/// Register `name` through the implemented `CreateAlgorithm`, with `metrics` as its
/// training specification's metric set when the row declares one, and answer its ARN.
async fn create_algorithm(emulator: &Emulator, name: &str, metrics: Option<Value>) -> String {
    let mut specification = json!({
        "TrainingImage": format!("{ACCOUNT}.dkr.ecr.us-east-1.amazonaws.com/trainer:1"),
        "SupportedTrainingInstanceTypes": ["ml.m5.large"],
        "TrainingChannels": [{
            "Name": "train",
            "SupportedContentTypes": ["text/csv"],
            "SupportedInputModes": ["File"],
        }],
    });
    if let Some(metrics) = metrics {
        specification["MetricDefinitions"] = metrics;
    }
    let created = emulator
        .ok(
            "CreateAlgorithm",
            &json!({ "AlgorithmName": name, "TrainingSpecification": specification }),
        )
        .await;
    str_field(&created, "AlgorithmArn")
}
