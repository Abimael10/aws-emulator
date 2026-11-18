// SPDX-License-Identifier: CC0-1.0

//! WHERE a flow log would publish, resolved against the service that OWNS that destination.
//!
//! The rule under test: `CreateFlowLogs` points the log delivery service at a resource of
//! another service — "specify the ARN of an S3 bucket", "specify the ARN of a Kinesis Data
//! Firehose delivery stream" (`CreateFlowLogsRequest$LogDestination`) — so a create is only
//! served once that service reports holding it. A create naming a destination its owner does
//! not hold turns logging on for NOTHING and is answered the partial-success way this
//! operation reports what it could not create: HTTP 200, an EMPTY `flowLogIdSet`, one
//! `unsuccessful` item per monitored resource the request named — error code `400`, message
//! `LogDestination: <the destination> does not exist`, `resourceId` the monitored resource —
//! and the echoed `clientToken`. Nothing is stored, so no later `DescribeFlowLogs` reports a
//! flow log asserting a delivery nothing performs.
//!
//! The ONE destination type with nothing to resolve is CloudWatch Logs: its destination is "a
//! new or existing CloudWatch Logs log group" (`CreateFlowLogsRequest$LogGroupName`), so a
//! group nobody has created yet is still a destination the create accepts.
//!
//! Also here, because it is the other thing a client reads off a flow log's destination: an
//! Amazon S3 flow log reports the DESTINATION OPTIONS in force — "The default is plain-text"
//! (`DestinationOptionsRequest$FileFormat`), "The default is false"
//! (`$HiveCompatiblePartitions`, `$PerHourPartition`) — whether or not the request spelled
//! any of them out, while a flow log of another destination type reports none at all: the
//! options describe the objects an Amazon S3 delivery writes, so they are that destination's
//! shape.
//!
//! The rows span what each rule generalises over rather than one recorded request: the
//! destination TYPE, the form of the destination (a bucket, a bucket with a subfolder, a
//! delivery stream ARN separating its name with `:` and with `/`), whether its owner never
//! held it or held it and then let it go, how many resources one call monitors, and which
//! destination options the request carried.
//!
//! Every service is mounted on ONE shared engine, exactly as the assembled emulator mounts
//! them, so each destination is created through its own service's wire and resolved through
//! the state that service serves its own reads from.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_query::{call, element, unescaped};
use crate::wire::{self, param, Param};

/// The Region every call here is signed for — and the Region an Amazon S3 `CreateBucket` with
/// no `CreateBucketConfiguration` places a bucket in.
const REGION: &str = "us-east-1";

/// The IAM role a CloudWatch Logs flow log publishes under: "This parameter is required if the
/// destination type is cloud-watch-logs" (`CreateFlowLogsRequest$DeliverLogsPermissionArn`).
const DELIVER_LOGS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/flow-logs-delivery";

/// The role the held delivery stream assumes. No IAM service is mounted here, so this names
/// the role form Firehose documents without asserting anything about IAM.
const STREAM_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/firehose-delivery";

/// A bucket Amazon S3 really holds, and the bucket the held delivery stream delivers to.
const HELD_BUCKET: &str = "ec2-flow-logs-held.2026";
const STREAM_SINK_BUCKET: &str = "ec2-flow-logs-firehose-sink";

/// A bucket Amazon S3 held and then let go, so a destination that resolved once stops
/// resolving.
const DELETED_BUCKET: &str = "ec2-flow-logs-deleted";

/// A bucket nothing ever created.
const ABSENT_BUCKET: &str = "ec2-flow-logs-never-created";

/// A delivery stream Amazon Data Firehose really holds, and two it never held.
const HELD_STREAM: &str = "ec2_flow_logs_held_stream";
const ABSENT_STREAM: &str = "ec2-flow-logs-never-created-stream";
const ABSENT_SLASH_STREAM: &str = "ec2.flow.logs.absent";

/// A CloudWatch Logs log group nothing ever created: the destination type whose group the
/// delivery brings into existence.
const ABSENT_LOG_GROUP: &str = "/aws/vpc/never-created-flow-logs";

/// The `LogDestinationType` members, spelled as the model spells them
/// (`smithy_query(query="shape",shape="LogDestinationType")`).
const S3: &str = "s3";
const KINESIS_DATA_FIREHOSE: &str = "kinesis-data-firehose";
const CLOUD_WATCH_LOGS: &str = "cloud-watch-logs";

/// The error code a per-resource flow-log failure carries.
const UNSUCCESSFUL_CODE: &str = "400";

/// EC2 and the three services that own a flow-log destination, over ONE shared engine, exactly
/// as the assembled router mounts them.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the emulator's own concern and is exercised elsewhere;
        // these cases are about these services' behaviour.
        router.set_request_authentication(false);
        router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// The SigV4 `Authorization` header of a request signed for `service`, which is how the
    /// router picks the service a request belongs to.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTEC2/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host;x-amz-date, Signature=00"
        )
    }

    /// One of EC2's own ec2Query actions.
    async fn ec2_call(&self, action: &str, params: &[Param]) -> (u16, String) {
        call(&self.router, REGION, action, params).await
    }

    /// One awsJson operation of `service`, whose `X-Amz-Target` prefix is `target_prefix`.
    async fn json_rpc(
        &self,
        service: &str,
        target_prefix: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{service}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", Self::authorization(service))
            .body(Bytes::from(body.to_string()))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One VPC of the account, through EC2's own `CreateVpc` — the resource a flow log
    /// monitors has to be one this account holds.
    async fn given_vpc(&self, cidr_block: &str) -> String {
        let (status, body) = self
            .ec2_call("CreateVpc", &[param("CidrBlock", cidr_block)])
            .await;
        assert_eq!(
            status, 200,
            "CreateVpc({cidr_block}) must answer 200, got {status} with body {body}"
        );
        element(&body, "vpcId")
            .unwrap_or_else(|| panic!("CreateVpc({cidr_block}) reported no vpcId in {body}"))
    }

    /// Create a bucket through Amazon S3's own (REST) wire, path style and with no
    /// `CreateBucketConfiguration`: an absent `LocationConstraint` places it in [`REGION`].
    async fn given_bucket(&self, name: &str) {
        let (status, payload) = self.bucket_request("PUT", name).await;
        assert_eq!(
            status, 200,
            "CreateBucket({name}) must answer 200, got {status} with {payload}"
        );
    }

    /// Remove a bucket, so Amazon S3 holds it no longer.
    async fn delete_bucket(&self, name: &str) {
        let (status, payload) = self.bucket_request("DELETE", name).await;
        assert_eq!(
            status, 204,
            "DeleteBucket({name}) must answer 204, got {status} with {payload}"
        );
    }

    async fn bucket_request(&self, method: &str, name: &str) -> (u16, String) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("/{name}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("s3"))
            .body(Bytes::new())
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// One delivery stream of the account, through Amazon Data Firehose's own wire.
    async fn given_delivery_stream(&self, name: &str) {
        let (status, payload) = self
            .json_rpc(
                "firehose",
                "Firehose_20150804",
                "CreateDeliveryStream",
                &json!({
                    "DeliveryStreamName": name,
                    "ExtendedS3DestinationConfiguration": {
                        "RoleARN": STREAM_ROLE_ARN,
                        "BucketARN": format!("arn:aws:s3:::{STREAM_SINK_BUCKET}"),
                    },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDeliveryStream({name}) must answer 200, got {status} with {payload}"
        );
    }

    /// Whether CloudWatch Logs holds a log group under `name` — asked of that service's own
    /// read, so a row about a group nobody created states what CloudWatch Logs says.
    async fn holds_log_group(&self, name: &str) -> bool {
        let (status, payload) = self
            .json_rpc(
                "logs",
                "Logs_20140328",
                "DescribeLogGroups",
                &json!({ "logGroupNamePrefix": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "DescribeLogGroups({name}) must answer 200, got {status} with {payload}"
        );
        payload
            .get("logGroups")
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|group| {
                    group.get("logGroupName").and_then(Value::as_str) == Some(name)
                })
            })
    }

    /// Every flow log `DescribeFlowLogs` reports for the monitored resource `resource_id`, by
    /// `flowLogId` — the read a client audits its flow-log coverage with.
    async fn described_for_resource(&self, case: &str, resource_id: &str) -> Vec<String> {
        let (status, body) = self
            .ec2_call(
                "DescribeFlowLogs",
                &[
                    param("Filter.1.Name", "resource-id"),
                    param("Filter.1.Value.1", resource_id),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeFlowLogs for {resource_id} must answer 200, got {status} \
             with body {body}"
        );
        let document = wire::document(case, &body);
        document
            .descendants()
            .filter(|node| node.is_element() && wire::child(*node, "flowLogId").is_some())
            .filter_map(|node| wire::child_text(node, "flowLogId"))
            .collect()
    }

    /// The `(fileFormat, hiveCompatiblePartitions, perHourPartition)` the one flow log of the
    /// monitored resource `resource_id` reports under `destinationOptions`, and the body it
    /// was read from — `None` when the flow log carries no destination options at all.
    ///
    /// Read off `DescribeFlowLogs`, so what is asserted is what a client reads back rather
    /// than what the create answered.
    async fn reported_destination_options(
        &self,
        case: &str,
        resource_id: &str,
    ) -> (Option<[String; 3]>, String) {
        let (status, body) = self
            .ec2_call(
                "DescribeFlowLogs",
                &[
                    param("Filter.1.Name", "resource-id"),
                    param("Filter.1.Value.1", resource_id),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeFlowLogs for {resource_id} must answer 200, got {status} \
             with body {body}"
        );
        let document = wire::document(case, &body);
        let flow_log = document
            .descendants()
            .find(|node| node.is_element() && wire::child(*node, "flowLogId").is_some())
            .unwrap_or_else(|| {
                panic!("case {case:?}: no flow log reported for {resource_id} in {body}")
            });
        let options = wire::child(flow_log, "destinationOptions");
        let reported = options.map(|options| {
            ["fileFormat", "hiveCompatiblePartitions", "perHourPartition"].map(|member| {
                wire::child_text(options, member).unwrap_or_else(|| {
                    panic!("case {case:?}: no <{member}> in the destination options of {body}")
                })
            })
        });
        (reported, body)
    }
}

/// The `unsuccessful` items of a `CreateFlowLogs` answer, as
/// `(error.code, error.message, resourceId)` triples in the order the answer reported them.
fn unsuccessful_items(case: &str, body: &str) -> Vec<(String, String, String)> {
    let document = wire::document(case, body);
    let root = document.root_element();
    let Some(set) = wire::child(root, "unsuccessful") else {
        return Vec::new();
    };
    set.children()
        .filter(roxmltree::Node::is_element)
        .map(|item| {
            let error = wire::child(item, "error");
            (
                error
                    .and_then(|error| wire::child_text(error, "code"))
                    .unwrap_or_default(),
                unescaped(
                    &error
                        .and_then(|error| wire::child_text(error, "message"))
                        .unwrap_or_default(),
                ),
                wire::child_text(item, "resourceId").unwrap_or_default(),
            )
        })
        .collect()
}

/// The ids of the `flowLogIdSet` a `CreateFlowLogs` answer carries.
fn created_ids(case: &str, body: &str) -> Vec<String> {
    let document = wire::document(case, body);
    let root = document.root_element();
    wire::set_items(root, "flowLogIdSet")
        .into_iter()
        .map(|item| item.text().unwrap_or_default().to_string())
        .collect()
}

/// The sentence a create answers for a destination its owner does not hold.
fn absent_message(destination_name: &str) -> String {
    format!("LogDestination: {destination_name} does not exist")
}

/// One row of the destination table.
struct Row {
    case: &'static str,
    log_destination_type: &'static str,
    /// The `LogDestination` the request carries.
    log_destination: String,
    /// The destination's own name when its owner holds no such destination, and `None` when
    /// the create is to be served.
    absent_name: Option<&'static str>,
    /// How many monitored resources one call names.
    resources: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an s3 bucket Amazon S3 holds",
            log_destination_type: S3,
            log_destination: format!("arn:aws:s3:::{HELD_BUCKET}"),
            absent_name: None,
            resources: 1,
        },
        Row {
            case: "an s3 bucket Amazon S3 holds, with a subfolder",
            log_destination_type: S3,
            log_destination: format!("arn:aws:s3:::{HELD_BUCKET}/vpc/"),
            absent_name: None,
            resources: 1,
        },
        Row {
            case: "an s3 bucket that was never created",
            log_destination_type: S3,
            log_destination: format!("arn:aws:s3:::{ABSENT_BUCKET}"),
            absent_name: Some(ABSENT_BUCKET),
            resources: 1,
        },
        Row {
            case: "an s3 bucket that was created and then deleted",
            log_destination_type: S3,
            log_destination: format!("arn:aws:s3:::{DELETED_BUCKET}/vpc/"),
            absent_name: Some(DELETED_BUCKET),
            resources: 1,
        },
        Row {
            case: "an s3 bucket that was never created, for two monitored resources",
            log_destination_type: S3,
            log_destination: format!("arn:aws:s3:::{ABSENT_BUCKET}"),
            absent_name: Some(ABSENT_BUCKET),
            resources: 2,
        },
        Row {
            case: "a delivery stream Amazon Data Firehose holds",
            log_destination_type: KINESIS_DATA_FIREHOSE,
            log_destination: format!(
                "arn:aws:firehose:{REGION}:000000000000:deliverystream:{HELD_STREAM}"
            ),
            absent_name: None,
            resources: 1,
        },
        Row {
            case: "a delivery stream that was never created",
            log_destination_type: KINESIS_DATA_FIREHOSE,
            log_destination: format!(
                "arn:aws:firehose:{REGION}:000000000000:deliverystream:{ABSENT_STREAM}"
            ),
            absent_name: Some(ABSENT_STREAM),
            resources: 1,
        },
        Row {
            case: "a delivery stream ARN separating its name with a slash, never created",
            log_destination_type: KINESIS_DATA_FIREHOSE,
            log_destination: format!(
                "arn:aws:firehose:{REGION}:000000000000:deliverystream/{ABSENT_SLASH_STREAM}"
            ),
            absent_name: Some(ABSENT_SLASH_STREAM),
            resources: 1,
        },
        Row {
            case: "a delivery stream that was never created, for two monitored resources",
            log_destination_type: KINESIS_DATA_FIREHOSE,
            log_destination: format!(
                "arn:aws:firehose:{REGION}:000000000000:deliverystream:{ABSENT_STREAM}"
            ),
            absent_name: Some(ABSENT_STREAM),
            resources: 2,
        },
        Row {
            case: "a cloud-watch-logs group nobody created, which the delivery creates",
            log_destination_type: CLOUD_WATCH_LOGS,
            log_destination: format!(
                "arn:aws:logs:{REGION}:000000000000:log-group:{ABSENT_LOG_GROUP}"
            ),
            absent_name: None,
            resources: 1,
        },
    ]
}

#[tokio::test]
async fn a_create_resolves_its_destination_against_the_service_that_owns_it() {
    let emulator = Emulator::new();
    emulator.given_bucket(HELD_BUCKET).await;
    emulator.given_bucket(STREAM_SINK_BUCKET).await;
    emulator.given_bucket(DELETED_BUCKET).await;
    emulator.delete_bucket(DELETED_BUCKET).await;
    emulator.given_delivery_stream(HELD_STREAM).await;
    // The one destination type the create does not resolve: the group is not there, and the
    // row below is still served.
    assert!(
        !emulator.holds_log_group(ABSENT_LOG_GROUP).await,
        "the cloud-watch-logs row must name a group CloudWatch Logs does not hold"
    );

    for (index, row) in rows().into_iter().enumerate() {
        // Every row monitors resources of its own, so no row is refused for a flow log
        // another row created.
        let mut resource_ids: Vec<String> = Vec::with_capacity(row.resources);
        for resource in 0..row.resources {
            resource_ids.push(
                emulator
                    .given_vpc(&format!("10.{index}.{resource}.0/24"))
                    .await,
            );
        }

        let client_token = format!("token-{index}");
        let mut params = vec![
            param("ResourceType", "VPC"),
            param("TrafficType", "ALL"),
            param("LogDestinationType", row.log_destination_type),
            param("LogDestination", &row.log_destination),
            param("ClientToken", &client_token),
        ];
        for (at, resource_id) in resource_ids.iter().enumerate() {
            params.push(param(&format!("ResourceId.{}", at + 1), resource_id));
        }
        if row.log_destination_type == CLOUD_WATCH_LOGS {
            params.push(param("DeliverLogsPermissionArn", DELIVER_LOGS_ROLE_ARN));
        }

        let case = row.case;
        let (status, body) = emulator.ec2_call("CreateFlowLogs", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateFlowLogs is a partial-success API, so it must answer 200, \
             got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "clientToken").as_deref(),
            Some(client_token.as_str()),
            "case {case:?}: expected the clientToken echoed, got {body}"
        );

        let created = created_ids(case, &body);
        let unsuccessful = unsuccessful_items(case, &body);
        match row.absent_name {
            Some(absent) => {
                assert!(
                    created.is_empty(),
                    "case {case:?}: no destination, so no flow log may be created, got \
                     {created:?}"
                );
                let expected: Vec<(String, String, String)> = resource_ids
                    .iter()
                    .map(|resource_id| {
                        (
                            UNSUCCESSFUL_CODE.to_string(),
                            absent_message(absent),
                            resource_id.clone(),
                        )
                    })
                    .collect();
                assert_eq!(
                    unsuccessful, expected,
                    "case {case:?}: expected one unsuccessful item per monitored resource, \
                     got {unsuccessful:?} in {body}"
                );
            }
            None => {
                assert_eq!(
                    created.len(),
                    row.resources,
                    "case {case:?}: the destination is there, so each monitored resource gets \
                     a flow log, got {created:?} in {body}"
                );
                assert!(
                    unsuccessful.is_empty(),
                    "case {case:?}: every flow log was created, so nothing failed, got \
                     {unsuccessful:?}"
                );
            }
        }

        // What the store holds, read back through the implemented reader: a refused
        // destination leaves the monitored resource with no flow log at all, and an accepted
        // one is reported for it.
        for resource_id in &resource_ids {
            let described = emulator.described_for_resource(case, resource_id).await;
            let expected = usize::from(row.absent_name.is_none());
            assert_eq!(
                described.len(),
                expected,
                "case {case:?}: expected {expected} flow log(s) reported for {resource_id}, \
                 got {described:?}"
            );
        }
    }
}

/// One row of the destination-options table: the case, the options the request carries, and
/// the `(fileFormat, hiveCompatiblePartitions, perHourPartition)` the flow log then reports
/// — `None` where it reports no `destinationOptions` element at all.
type OptionsRow = (
    &'static str,
    Vec<(&'static str, &'static str)>,
    Option<[&'static str; 3]>,
);

fn options_rows() -> Vec<OptionsRow> {
    vec![
        (
            "no destination options at all",
            vec![],
            // An Amazon S3 delivery HAS an object layout whether or not the create spelled
            // it out, so every member is reported at its own documented default.
            Some(["plain-text", "false", "false"]),
        ),
        (
            "a file format alone",
            vec![("DestinationOptions.FileFormat", "parquet")],
            // "The default is plain-text" (`DestinationOptionsRequest$FileFormat`), "The
            // default is false" (`$HiveCompatiblePartitions`, `$PerHourPartition`) — the
            // members the create left out of the options it DID name.
            Some(["parquet", "false", "false"]),
        ),
        (
            "Hive-compatible prefixes alone",
            vec![("DestinationOptions.HiveCompatiblePartitions", "true")],
            Some(["plain-text", "true", "false"]),
        ),
        (
            "every member spelled out",
            vec![
                ("DestinationOptions.FileFormat", "parquet"),
                ("DestinationOptions.HiveCompatiblePartitions", "true"),
                ("DestinationOptions.PerHourPartition", "true"),
            ],
            Some(["parquet", "true", "true"]),
        ),
    ]
}

#[tokio::test]
async fn an_s3_flow_log_reports_the_destination_options_in_force() {
    let emulator = Emulator::new();
    emulator.given_bucket(HELD_BUCKET).await;

    for (index, (case, options, expected)) in options_rows().into_iter().enumerate() {
        let vpc_id = emulator.given_vpc(&format!("10.1{index}.0.0/24")).await;
        let mut params = vec![
            param("ResourceType", "VPC"),
            param("ResourceId.1", &vpc_id),
            param("TrafficType", "ALL"),
            param("LogDestinationType", S3),
            param("LogDestination", &format!("arn:aws:s3:::{HELD_BUCKET}")),
        ];
        for (name, value) in options {
            params.push(param(name, value));
        }
        let (status, body) = emulator.ec2_call("CreateFlowLogs", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateFlowLogs must answer 200, got {status} with body {body}"
        );

        let (reported, described) =
            emulator.reported_destination_options(case, &vpc_id).await;
        assert_eq!(
            reported,
            expected.map(|values| values.map(String::from)),
            "case {case:?}: expected the (fileFormat, hiveCompatiblePartitions, \
             perHourPartition) in force, got {reported:?} in {described}"
        );
    }
}

#[tokio::test]
async fn a_flow_log_of_another_destination_type_reports_no_destination_options() {
    let emulator = Emulator::new();
    let vpc_id = emulator.given_vpc("10.200.0.0/24").await;
    // A request may not carry destination options for this destination type at all, the
    // options being the Amazon S3 shape — so the flow log reports none.
    let case = "a cloud-watch-logs flow log";

    let (status, body) = emulator
        .ec2_call(
            "CreateFlowLogs",
            &[
                param("ResourceType", "VPC"),
                param("ResourceId.1", &vpc_id),
                param("TrafficType", "ALL"),
                param("LogGroupName", "/aws/vpc/flow-logs"),
                param("DeliverLogsPermissionArn", DELIVER_LOGS_ROLE_ARN),
            ],
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateFlowLogs must answer 200, got {status} with body {body}"
    );

    let (reported, described) = emulator.reported_destination_options(case, &vpc_id).await;
    assert_eq!(
        reported, None,
        "case {case:?}: the destination options describe the objects an Amazon S3 delivery \
         writes, so a flow log of another type reports none, got {reported:?} in {described}"
    );
}
