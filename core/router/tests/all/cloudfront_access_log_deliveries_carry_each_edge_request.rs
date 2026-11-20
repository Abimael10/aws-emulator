// SPDX-License-Identifier: CC0-1.0

//! STANDARD LOGGING v2 end to end: what a CloudFront distribution's edge answers a viewer, and
//! the access-log record EVERY live vended-log delivery of that distribution writes for it.
//!
//! RULE: a vended-log pipeline is a fan-out of the SENDING resource — `PutDeliverySource`
//! represents "the resource that is actually sending the logs", a `CreateDelivery` is "a
//! delivery [...] pairing exactly one delivery source and one delivery destination", and
//! several deliveries may name one source — so each request the sender serves becomes ONE
//! record in EVERY live delivery's destination, carrying exactly that delivery's own
//! `recordFields` ("the list of record fields to be delivered to the destination, in order")
//! and no others, with each field's value read off that request and the answer it was given.
//! The identifier the answer carried to the client is what joins the two: a CloudFront edge
//! answer carries its own `X-Amz-Cf-Id`, and the record's `x-edge-request-id` is "an encrypted
//! string that uniquely identifies a request" — the same request. `DeleteDelivery` "deletes a
//! delivery [...] only deletes the connection between the delivery source and delivery
//! destination", so it severs exactly the one delivery it names: that
//! delivery's id resolves to nothing afterwards (`ResourceNotFoundException`, for `GetDelivery`
//! and for a repeat `DeleteDelivery` alike, which is therefore not idempotent), records already
//! written to its destination stay where they are, no later request is delivered there, and
//! every sibling delivery of the same source keeps carrying every later request.
//!
//! Only a cross-service suite can pin this: the sending side is CloudFront's edge over a
//! private Amazon S3 origin and the delivery side is CloudWatch Logs' own store and log
//! groups, so the record and the answer it describes exist in different services of one
//! engine. The cloudwatch logs crate's own `delivery_creation`, `delivery_region_isolation` and
//! `delivery_configuration_updates` suites pin the control plane of a delivery on its own, and
//! [`super::cloudfront_oac_s3_origin_edge`] pins what the edge serves over such an origin;
//! neither can state that a served request becomes a record in each live delivery, nor what a
//! delete leaves behind.
//!
//! The rows span what the rule generalises over: the ANSWER each request got (a readable
//! object, the same object again, which the zero-TTL cache policy sends to the origin once
//! more, a readable object
//! under a nested prefix, and a key the bucket does not hold — two statuses and two result
//! classifications), and, per delivery, the record layout (two deliveries on one source whose
//! `recordFields` differ in which fields they name, how many, and in what order).
//!
//! The harness is [`super::cloudfront_oac_s3_origin_edge`]'s, reused rather than repeated: the
//! same two routers over one engine (setup calls through a router with request authentication
//! off, so each carries a credential scope without a computed signature; VIEWER requests
//! through an authenticating router carrying no credential at all, as a browser reaches a
//! distribution), with CloudWatch Logs mounted beside CloudFront and Amazon S3 because the
//! delivery leg lives there.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The deployment poll is the one the sibling edge suites already share, so no two suites of
// this target wait for a distribution differently.
use super::cloudfront_apigateway_origin_edge::wait_until_deployed;
use super::cloudfront_oac_s3_origin_edge::{
    create_bucket, create_distribution, create_origin_access_control, policy, put_bucket_policy,
    put_object, view, CLOUDFRONT_PRINCIPAL, REGION,
};
use super::logs_delivery_source_cloudfront_resource::{error_code, logs};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each one is a request of its own to be
/// logged rather than an answer the edge cache repeated.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The one `logType` a CloudFront delivery source sends ("For CloudFront, the valid value is
/// ACCESS_LOGS", `smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`).
const LOG_TYPE: &str = "ACCESS_LOGS";

/// `DeliveryDestination$outputFormat` the destinations here are created under: one of
/// `OutputFormat`'s own values (`smithy_query(query="shape",shape="OutputFormat")`), and the
/// structured one, so each record is a JSON object of its fields.
const OUTPUT_FORMAT: &str = "json";

/// `DeliveryDestination$deliveryDestinationType` for a destination that is a log group
/// (`smithy_query(query="shape",shape="DeliveryDestinationType")`).
const CWL: &str = "CWL";

/// The service a delivery source over a CloudFront distribution represents, as
/// `DeliverySource$service` reports it.
const CLOUDFRONT_SERVICE: &str = "cloudfront";

/// The refusal, and its wording, that a delivery id the account no longer holds is answered
/// with — `ResourceNotFoundException` is among both `GetDelivery`'s and `DeleteDelivery`'s
/// errors (`smithy_query(query="operation",operation="GetDelivery,DeleteDelivery")`).
const NOT_FOUND: &str = "ResourceNotFoundException";
const DELIVERY_NOT_FOUND: &str = "Requested Delivery does not exist in this account.";

/// The suffix `X-Cache` ends in, whichever classification it reports.
pub(crate) const FROM_CLOUDFRONT: &str = " from cloudfront";

/// How long a look for records that ARE due keeps looking, and how long it waits between
/// looks. Generous: a loaded machine must not fail a row the delivery leg would have written.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);

/// The record layout of the PRIMARY delivery: the six fields of CloudFront's standard
/// access-log record this suite joins on, as the edge reports them (the field catalogue of
/// `services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/service/edge/realtime_logs.rs`,
/// whose arms carry each field's own published definition).
pub(crate) const PRIMARY_FIELDS: &[&str] = &[
    "date",
    "time",
    "cs-uri-stem",
    "sc-status",
    "x-edge-result-type",
    "x-edge-request-id",
];

/// The record layout of the BARRIER delivery on the same source: fewer fields, in another
/// order, and one field the primary does not name at all (`cs-method`, "the HTTP method of the
/// viewer request").
pub(crate) const BARRIER_FIELDS: &[&str] = &["x-edge-request-id", "cs-method", "sc-status"];

/// One delivery of the wired pipeline: where its records land and the layout they land in.
#[derive(Debug)]
pub(crate) struct Wired {
    pub(crate) case: &'static str,
    /// `Delivery$id`.
    pub(crate) id: String,
    /// The log group its destination names.
    pub(crate) log_group: String,
    /// `Delivery$recordFields`, in the order the delivery was created with.
    pub(crate) fields: Vec<String>,
    /// The ARN of the delivery destination it is paired with.
    destination_arn: String,
}

/// A whole wired pipeline: the two routers, the distribution's edge domain, and every delivery
/// created over it.
pub(crate) struct Pipeline {
    pub(crate) setup: MultiServiceRouter,
    viewers: MultiServiceRouter,
    pub(crate) domain: String,
    pub(crate) deliveries: Vec<Wired>,
}

/// The three services this suite exercises, mounted on `engine` exactly as the router mounts
/// them, with request authentication as `authenticated` says.
fn emulator(engine: &Arc<engine::InMemoryEngine>, authenticated: bool) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(authenticated);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router
}

/// An ARN of a log group of this Region, which is what a delivery destination names.
fn log_group_arn(group: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}",
        engine::emulator_account_id()
    )
}

/// Build the whole pipeline the rule is about: a private bucket holding `objects`, a
/// distribution over it through an origin access control, and one delivery per entry of
/// `layouts` — all on the SAME delivery source, which is what makes them siblings.
pub(crate) async fn wire(
    bucket: &str,
    objects: &[(&str, &str)],
    layouts: &[(&'static str, &'static [&str])],
) -> Pipeline {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let setup = emulator(&engine, false);
    let viewers = emulator(&engine, true);
    let case = "the wired pipeline";

    create_bucket(&setup, case, bucket).await;
    for (key, body) in objects {
        put_object(&setup, case, bucket, key, body, &[]).await;
    }
    let control =
        create_origin_access_control(&setup, case, &format!("{bucket}-control"), "always").await;
    let (id, domain, arn) =
        create_distribution(&setup, case, bucket, bucket, &control, CACHING_DISABLED).await;
    // The grant the origin bucket carries for this distribution, written after the create
    // because it is scoped to the distribution's own ARN.
    put_bucket_policy(
        &setup,
        case,
        bucket,
        &policy(
            CLOUDFRONT_PRINCIPAL,
            "s3:GetObject",
            &format!("arn:aws:s3:::{bucket}/*"),
            Some(&format!(
                "{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}"
            )),
        ),
    )
    .await;

    // The ONE source every delivery below is paired with: the distribution is what sends the
    // records.
    let source = format!("{bucket}-source");
    let (status, answered) = logs(
        &setup,
        "PutDeliverySource",
        &json!({"name": &source, "resourceArn": &arn, "logType": LOG_TYPE}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutDeliverySource must be accepted, got {status} {answered}"
    );
    assert_eq!(
        answered["deliverySource"]["service"].as_str(),
        Some(CLOUDFRONT_SERVICE),
        "case {case:?}: expected the source to represent {CLOUDFRONT_SERVICE:?}, got {answered}"
    );
    assert_eq!(
        answered["deliverySource"]["resourceArns"],
        json!([arn]),
        "case {case:?}: expected the source to represent {arn:?}, got {answered}"
    );

    let mut deliveries = Vec::new();
    for (index, (layout, fields)) in layouts.iter().enumerate() {
        let group = format!("/aws/vendedlogs/{bucket}-{index}");
        let (status, answered) = logs(
            &setup,
            "CreateLogGroup",
            &json!({"logGroupName": &group}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {layout:?}: CreateLogGroup({group:?}) must succeed, got {status} {answered}"
        );
        let (status, answered) = logs(
            &setup,
            "PutDeliveryDestination",
            &json!({
                "name": format!("{bucket}-destination-{index}"),
                "outputFormat": OUTPUT_FORMAT,
                "deliveryDestinationConfiguration": {
                    "destinationResourceArn": log_group_arn(&group),
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {layout:?}: PutDeliveryDestination must be accepted, got {status} {answered}"
        );
        assert_eq!(
            answered["deliveryDestination"]["deliveryDestinationType"].as_str(),
            Some(CWL),
            "case {layout:?}: expected a {CWL} destination, got {answered}"
        );
        let destination_arn = answered["deliveryDestination"]["arn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !destination_arn.is_empty(),
            "case {layout:?}: the created destination reports no ARN: {answered}"
        );
        let (status, answered) = logs(
            &setup,
            "CreateDelivery",
            &json!({
                "deliverySourceName": &source,
                "deliveryDestinationArn": &destination_arn,
                "recordFields": fields,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {layout:?}: CreateDelivery must be accepted, got {status} {answered}"
        );
        let delivery_id = answered["delivery"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !delivery_id.is_empty(),
            "case {layout:?}: the created delivery reports no id: {answered}"
        );
        deliveries.push(Wired {
            case: layout,
            id: delivery_id,
            log_group: group,
            fields: fields.iter().map(|field| (*field).to_string()).collect(),
            destination_arn,
        });
    }

    wait_until_deployed(&setup, case, &id).await;
    Pipeline {
        setup,
        viewers,
        domain,
        deliveries,
    }
}

/// Every record the log group `group` holds, each parsed from the event's own message.
pub(crate) async fn records(router: &MultiServiceRouter, group: &str) -> Vec<Value> {
    let (status, answered) = logs(
        router,
        "FilterLogEvents",
        &json!({"logGroupName": group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case reading {group:?}: FilterLogEvents must succeed, got {status} {answered}"
    );
    answered["events"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|event| {
            let message = event["message"].as_str().unwrap_or_default();
            serde_json::from_str(message).unwrap_or_else(|error| {
                panic!("case reading {group:?}: a record is not JSON ({error}): {message:?}")
            })
        })
        .collect()
}

/// Look for records in `group` until it holds `wanted` of them or [`DEADLINE`] passes, then
/// answer whatever it holds. Waits on the CONDITION, never on a span.
pub(crate) async fn records_until(
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = records(router, group).await;
        if held.len() >= wanted || Instant::now() >= deadline {
            return held;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// One viewer request of the table: the path dialled, and what the edge has to answer it.
///
/// `pub(crate)` so the sibling suite that dials the published address on a socket
/// ([`super::cloudfront_dialled_address_vends_each_access_log_record`]) declares its own
/// requests in the same terms rather than restating them.
pub(crate) struct Row {
    pub(crate) case: &'static str,
    pub(crate) path: &'static str,
    /// `sc-status` — the status the viewer was answered with.
    pub(crate) status: u16,
    /// `x-edge-result-type` — the first word of the answer's `X-Cache`.
    pub(crate) result_type: &'static str,
}

/// Whether `value` is written as the `date` field is ("for example, 2019-06-26") or as the
/// `time` field is ("for example, 01:42:39"): the half is put back into the whole-second UTC
/// `date-time` form and READ by this workspace's own timestamp facility, so a value that is
/// digits and separators but no calendar date or wall-clock time is refused. It is the FORM of
/// the two halves that is read, which is what a record states about them; neither is compared
/// against a clock reading.
/// `pub(crate)` so the sibling suite that dials the published address on a socket holds these
/// two fields to the same written form rather than to a weaker shape of its own.
pub(crate) fn is_date(value: &str) -> bool {
    value.len() == 10 && is_instant(&format!("{value}T00:00:00Z"))
}

pub(crate) fn is_time(value: &str) -> bool {
    value.len() == 8 && is_instant(&format!("1970-01-01T{value}Z"))
}

fn is_instant(stamp: &str) -> bool {
    ::aws_smithy_types::DateTime::from_str(stamp, ::aws_smithy_types::date_time::Format::DateTime)
        .is_ok()
}

/// What a record of `row` must state for ONE field, for a request whose answer carried
/// `request_id`; `None` for a field whose value is checked by its form instead.
///
/// `pub(crate)` for the same reason [`Row`] is: one reading of what a record owes a request.
pub(crate) fn expected_value(row: &Row, request_id: &str, field: &str) -> Option<String> {
    match field {
        "cs-uri-stem" => Some(row.path.to_string()),
        "sc-status" => Some(row.status.to_string()),
        "x-edge-result-type" => Some(row.result_type.to_string()),
        "x-edge-request-id" => Some(request_id.to_string()),
        "cs-method" => Some("GET".to_string()),
        _ => None,
    }
}

#[tokio::test]
async fn every_live_delivery_writes_one_record_per_served_edge_request() {
    // Rows differ along the answer each request got: the status, who produced it, and the key
    // form it addressed.
    let rows: &[Row] = &[
        Row {
            case: "a readable object at a bare key",
            path: "/present.txt",
            status: 200,
            result_type: "Miss",
        },
        Row {
            case: "the same object again, which the zero-TTL policy sends to the origin once \
                   more",
            path: "/present.txt",
            status: 200,
            result_type: "Miss",
        },
        Row {
            case: "a readable object under a nested prefix",
            path: "/reports/2026/held.csv",
            status: 200,
            result_type: "Miss",
        },
        Row {
            case: "a key the bucket does not hold, which the grant does not let the edge list",
            path: "/absent.txt",
            status: 403,
            result_type: "Error",
        },
    ];

    let pipeline = wire(
        "cloudfront-access-log-join",
        &[
            ("present.txt", "the readable object"),
            ("reports/2026/held.csv", "the nested object"),
        ],
        &[
            ("the primary delivery", PRIMARY_FIELDS),
            ("the barrier delivery", BARRIER_FIELDS),
        ],
    )
    .await;

    // Each request, and the identifier the edge stamped on the answer it got.
    let mut request_ids: Vec<String> = Vec::new();
    for row in rows {
        let viewed = view(&pipeline.viewers, &pipeline.domain, row.path).await;
        assert_eq!(
            viewed.status, row.status,
            "case {:?}: expected the edge to answer {}, got {} with {:?}",
            row.case,
            row.status,
            viewed.status,
            viewed.text()
        );
        let cache = viewed.header("x-cache");
        assert_eq!(
            cache,
            format!("{}{FROM_CLOUDFRONT}", row.result_type),
            "case {:?}: expected X-Cache {:?}, got {cache:?}",
            row.case,
            format!("{}{FROM_CLOUDFRONT}", row.result_type)
        );
        let request_id = viewed.header("x-amz-cf-id");
        assert!(
            !request_id.is_empty(),
            "case {:?}: the edge answered no X-Amz-Cf-Id (status {})",
            row.case,
            viewed.status
        );
        request_ids.push(request_id);
    }
    let distinct: BTreeSet<&String> = request_ids.iter().collect();
    assert_eq!(
        distinct.len(),
        rows.len(),
        "case the whole phase: expected {} distinct X-Amz-Cf-Id values, got {request_ids:?}",
        rows.len()
    );

    // Both deliveries are live, so BOTH destinations carry every one of those requests — each
    // in its own layout.
    for delivery in &pipeline.deliveries {
        let held = records_until(&pipeline.setup, &delivery.log_group, rows.len()).await;
        assert_eq!(
            held.len(),
            rows.len(),
            "case {:?}: expected one record per served request ({}), got {held:?}",
            delivery.case,
            rows.len()
        );
        let wanted_keys: BTreeSet<&str> =
            delivery.fields.iter().map(String::as_str).collect();
        let mut by_request: BTreeMap<String, &Value> = BTreeMap::new();
        for record in &held {
            let keys: BTreeSet<&str> = record
                .as_object()
                .map(|fields| fields.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                keys, wanted_keys,
                "case {:?}: expected exactly the delivery's record fields, got {record}",
                delivery.case
            );
            let request_id = record["x-edge-request-id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            by_request.insert(request_id, record);
        }
        assert_eq!(
            by_request.len(),
            rows.len(),
            "case {:?}: expected one record per distinct request, got {held:?}",
            delivery.case
        );

        for (row, request_id) in rows.iter().zip(&request_ids) {
            let record = by_request.get(request_id).unwrap_or_else(|| {
                panic!(
                    "case {:?} / {:?}: no record joins the answer's X-Amz-Cf-Id {request_id:?}, \
                     got {held:?}",
                    delivery.case, row.case
                )
            });
            for field in &delivery.fields {
                let got = record[field.as_str()].as_str().unwrap_or_default();
                if let Some(expected) = expected_value(row, request_id, field) {
                    assert_eq!(
                        got, expected,
                        "case {:?} / {:?}: expected {field:?} to be {expected:?}, got {got:?} \
                         in {record}",
                        delivery.case, row.case
                    );
                    continue;
                }
                let well_formed = match field.as_str() {
                    "date" => is_date(got),
                    "time" => is_time(got),
                    _ => !got.is_empty(),
                };
                assert!(
                    well_formed,
                    "case {:?} / {:?}: {field:?} is not written as that field is written, got \
                     {got:?} in {record}",
                    delivery.case, row.case
                );
            }
        }
    }
}

#[tokio::test]
async fn deleting_one_delivery_severs_that_delivery_alone() {
    let pipeline = wire(
        "cloudfront-access-log-sever",
        &[("present.txt", "the readable object")],
        &[
            ("the primary delivery", PRIMARY_FIELDS),
            ("the barrier delivery", BARRIER_FIELDS),
        ],
    )
    .await;
    let primary = &pipeline.deliveries[0];
    let barrier = &pipeline.deliveries[1];

    // One request before the delete: both destinations carry it.
    let first = view(&pipeline.viewers, &pipeline.domain, "/present.txt").await;
    assert_eq!(
        first.status,
        200,
        "case before the delete: expected the object through the edge, got {} with {:?}",
        first.status,
        first.text()
    );
    let first_id = first.header("x-amz-cf-id");
    for delivery in &pipeline.deliveries {
        let held = records_until(&pipeline.setup, &delivery.log_group, 1).await;
        assert_eq!(
            held.len(),
            1,
            "case {:?} before the delete: expected the served request's record, got {held:?}",
            delivery.case
        );
        assert_eq!(
            held[0]["x-edge-request-id"].as_str(),
            Some(first_id.as_str()),
            "case {:?} before the delete: expected the record of {first_id:?}, got {:?}",
            delivery.case,
            held[0]
        );
    }

    // The delete, and what the deleted id resolves to afterwards: `GetDelivery` and a REPEAT
    // `DeleteDelivery` both refuse it, in the same words.
    let (status, answered) = logs(
        &pipeline.setup,
        "DeleteDelivery",
        &json!({"id": &primary.id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the delete itself: expected DeleteDelivery to be accepted, got {status} {answered}"
    );
    for (case, operation) in [
        ("GetDelivery of the deleted id", "GetDelivery"),
        ("a repeat DeleteDelivery of the same id", "DeleteDelivery"),
    ] {
        let (status, answered) =
            logs(&pipeline.setup, operation, &json!({"id": &primary.id})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted delivery to be refused, got {status} {answered}"
        );
        let code = error_code(&answered);
        assert_eq!(
            code, NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND}, got {code} {answered}"
        );
        assert_eq!(
            answered["message"].as_str(),
            Some(DELIVERY_NOT_FOUND),
            "case {case:?}: expected {DELIVERY_NOT_FOUND:?}, got {answered}"
        );
    }

    // The SIBLING delivery on the same source is untouched.
    let (status, answered) = logs(
        &pipeline.setup,
        "GetDelivery",
        &json!({"id": &barrier.id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the sibling delivery: expected it to still resolve, got {status} {answered}"
    );
    assert_eq!(
        answered["delivery"]["deliveryDestinationArn"].as_str(),
        Some(barrier.destination_arn.as_str()),
        "case the sibling delivery: expected it to still name {:?}, got {answered}",
        barrier.destination_arn
    );
    assert_eq!(
        answered["delivery"]["recordFields"],
        json!(barrier.fields),
        "case the sibling delivery: expected its own record fields {:?}, got {answered}",
        barrier.fields
    );

    // Two more requests: the sibling keeps carrying every one of them.
    let mut later_ids = Vec::new();
    for (case, path, status) in [
        ("after the delete, the readable object", "/present.txt", 200),
        ("after the delete, a key the bucket does not hold", "/absent.txt", 403),
    ] {
        let viewed = view(&pipeline.viewers, &pipeline.domain, path).await;
        assert_eq!(
            viewed.status,
            status,
            "case {case:?}: expected the edge to answer {status}, got {} with {:?}",
            viewed.status,
            viewed.text()
        );
        later_ids.push(viewed.header("x-amz-cf-id"));
    }
    let carried = records_until(&pipeline.setup, &barrier.log_group, 3).await;
    assert_eq!(
        carried.len(),
        3,
        "case the sibling delivery after the delete: expected the later requests to keep \
         landing, got {carried:?}"
    );
    for later in &later_ids {
        assert!(
            carried
                .iter()
                .any(|record| record["x-edge-request-id"].as_str() == Some(later.as_str())),
            "case the sibling delivery after the delete: expected a record for {later:?}, got \
             {carried:?}"
        );
    }

    // And the severed delivery's destination keeps the record it already had, and takes no
    // record of a request served after the delete — read at a point where the sibling has
    // already taken both of them, so this is not a look taken too early.
    let left = records(&pipeline.setup, &primary.log_group).await;
    assert_eq!(
        left.len(),
        1,
        "case the severed delivery: expected the already-delivered record to stay and nothing \
         to be added, got {left:?}"
    );
    assert_eq!(
        left[0]["x-edge-request-id"].as_str(),
        Some(first_id.as_str()),
        "case the severed delivery: expected the record of {first_id:?} to be the one left, got \
         {:?}",
        left[0]
    );
}
