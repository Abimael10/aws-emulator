// SPDX-License-Identifier: CC0-1.0

//! STANDARD LOGGING v2: the access-log record a served viewer request leaves in the
//! CloudWatch Logs log group a vended-log delivery points the distribution at — end to end
//! over HTTP, with CloudFront and CloudWatch Logs mounted on one router.
//!
//! The rules under test:
//!
//! * a client wires standard logging v2 in CloudWatch Logs — `PutDeliverySource` naming the
//!   distribution's ARN with the log type CloudFront sends (`ACCESS_LOGS`),
//!   `PutDeliveryDestination` naming a log group, `CreateDelivery` pairing the two — and from
//!   then on EVERY viewer request the edge serves for that distribution appends ONE access-log
//!   record to that log group, readable through `FilterLogEvents`;
//! * every value in a record is read off the request the edge served, the answer it served and
//!   the distribution that served it, so a record names the request it is about;
//! * a record carries the delivery's own record fields, in the delivery's own order, in the
//!   destination's `outputFormat`: the structured format as one JSON object, and the text
//!   formats as the values joined by the delivery's `fieldDelimiter` ("the field delimiter that
//!   is used between record fields when the final output format of a delivery is in Plain, W3C,
//!   or Raw format");
//! * a distribution whose access logs no delivery carries — no pipeline at all, a source no
//!   delivery pairs, or a delivery to a destination that is not a log group — leaves nothing in
//!   any group;
//! * a destination is "a log group in CloudWatch Logs, an Amazon S3 bucket, or a delivery stream
//!   in Firehose" (`PutDeliveryDestination`), so the records fan out to EVERY destination the
//!   distribution's deliveries name, whichever of those kinds each is: a bucket destination
//!   receives the gzipped log file a reader of a bucket of vended logs fetches under the
//!   published object layout, a delivery-stream destination receives one put record, which that
//!   stream then delivers to its own destination, and the records of ONE request carry the same
//!   `x-edge-request-id` — the identifier the edge answer handed the viewer as `X-Amz-Cf-Id` —
//!   so a consumer can join them.
//!
//! Every record read back here is read through CloudWatch Logs' own `FilterLogEvents`, so a
//! record this suite sees is a record a client polling that group sees. The tables span what
//! the rules generalise over — the request's method, path and query string; the delivery's
//! record-field selection, output format and delimiter — rather than one recorded request.
//!
//! The origin is this service's own control-plane endpoint, whose
//! `GET /2020-05-31/distribution` answers a `DistributionList`: an origin this emulator really
//! serves, so a served request has an origin answer to carry into the record.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, call, API_VERSION, HOST, POLL_INTERVAL, READY_DEADLINE,
};

/// The Region every call below is signed for. CloudFront's vended-log control plane is reached
/// through the partition's one global endpoint, which is this Region's.
const REGION: &str = "us-east-1";

/// CloudWatch Logs' own endpoint, and the `X-Amz-Target` prefix its `awsJson1_1` operations are
/// named under (that model's `targetPrefix`).
const LOGS_HOST: &str = "logs.amazonaws.com";
const LOGS_TARGET: &str = "Logs_20140328";

/// The one `logType` a CloudFront delivery source sends ("For CloudFront, the valid value is
/// ACCESS_LOGS").
const ACCESS_LOGS: &str = "ACCESS_LOGS";

/// The `DeliveryDestinationType` of a log-group destination, which a create reports.
const CWL: &str = "CWL";

/// One router carrying CloudFront and CloudWatch Logs over one shared engine: the delivery
/// pipeline lives in CloudWatch Logs and the records travel from CloudFront, so the two have to
/// be mounted together for a record to have anywhere to land.
fn emulator_with_logs() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `signing_name`, which is what routes a request to that
/// service.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// Call one CloudWatch Logs operation and answer its JSON body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", LOGS_HOST)
        .header("authorization", authorization("logs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET}.{operation}"))
        .body(::bytes::Bytes::from(body.to_string()))
        .expect("the CloudWatch Logs request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    assert_eq!(
        status, 200,
        "case {operation}: expected HTTP 200 from CloudWatch Logs, got {status} with {text}"
    );
    if text.trim().is_empty() {
        return Value::Null;
    }
    ::serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("case {operation}: CloudWatch Logs answered no JSON ({error}): {text}")
    })
}

/// Create one log group and answer the ARN CloudWatch Logs reports for it, without the
/// `:*` suffix a client strips before naming it as a destination resource.
async fn log_group(router: &MultiServiceRouter, name: &str) -> String {
    logs(router, "CreateLogGroup", json!({"logGroupName": name})).await;
    let described = logs(
        router,
        "DescribeLogGroups",
        json!({"logGroupNamePrefix": name}),
    )
    .await;
    let arn = described["logGroups"]
        .as_array()
        .and_then(|groups| groups.first())
        .and_then(|group| group["arn"].as_str())
        .unwrap_or_else(|| panic!("case {name}: no log group ARN reported: {described}"))
        .to_string();
    arn.strip_suffix(":*").unwrap_or(&arn).to_string()
}

/// One vended-log delivery of a distribution's access logs, as a client wires it: the delivery
/// source over the distribution, the delivery destination over `destination_arn`, and the
/// delivery pairing them. Answers the record fields the delivery reports — which is the layout
/// its records are owed in.
async fn delivery(
    router: &MultiServiceRouter,
    name: &str,
    distribution_arn: &str,
    destination_arn: &str,
    output_format: &str,
    record_fields: Option<&[&str]>,
    field_delimiter: Option<&str>,
) -> Vec<String> {
    let source = logs(
        router,
        "PutDeliverySource",
        json!({"name": name, "resourceArn": distribution_arn, "logType": ACCESS_LOGS}),
    )
    .await;
    assert_eq!(
        source["deliverySource"]["service"].as_str(),
        Some("cloudfront"),
        "case {name}: expected the source to report cloudfront as the sender, got {source}"
    );
    let destination = logs(
        router,
        "PutDeliveryDestination",
        json!({
            "name": name,
            "outputFormat": output_format,
            "deliveryDestinationConfiguration": {"destinationResourceArn": destination_arn},
        }),
    )
    .await;
    let destination_logical_arn = destination["deliveryDestination"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: no destination ARN reported: {destination}"))
        .to_string();
    let mut request = json!({
        "deliverySourceName": name,
        "deliveryDestinationArn": destination_logical_arn,
    });
    if let Some(fields) = record_fields {
        request["recordFields"] = json!(fields);
    }
    if let Some(delimiter) = field_delimiter {
        request["fieldDelimiter"] = json!(delimiter);
    }
    let created = logs(router, "CreateDelivery", request).await;
    created["delivery"]["recordFields"]
        .as_array()
        .unwrap_or_else(|| panic!("case {name}: the delivery reports no recordFields: {created}"))
        .iter()
        .map(|field| field.as_str().unwrap_or_default().to_string())
        .collect()
}

/// One `DistributionConfig` over the served origin, with nothing but what every distribution
/// needs: standard logging v2 is configured in CloudWatch Logs, not here.
fn config_xml(reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{HOST}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         </DefaultCacheBehavior>\
         <Comment>{reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// One distribution this suite created: the id its deployment is polled by, the ARN a delivery
/// source names it by, and the domain name a viewer dials it at.
struct Distribution {
    id: String,
    arn: String,
    domain: String,
}

/// Create one distribution, WITHOUT waiting for its configuration to reach the edge locations
/// ([`awaited_deployment`] does that): the clock owes each distribution its deployment from the
/// moment it was created, so a suite with several of them creates them all and waits once.
async fn distribution(router: &MultiServiceRouter, reference: &str) -> Distribution {
    let answer = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&config_xml(reference)),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "case {reference}: expected HTTP 201 creating the distribution, got {} with {}",
        answer.status, answer.raw
    );
    let text = |name: &str| {
        answer
            .root()
            .find_text(name)
            .unwrap_or_else(|| {
                panic!(
                    "case {reference}: the create carries no {name}: {}",
                    answer.raw
                )
            })
            .to_string()
    };
    Distribution {
        id: text("Id"),
        arn: text("ARN"),
        domain: text("DomainName"),
    }
}

/// One VIEWER request, addressed host-style at the name the distribution published and carrying
/// no credential — which is what a viewer is. Answers the status the edge served.
async fn view(router: &MultiServiceRouter, domain: &str, method: &str, target: &str) -> u16 {
    let request = ::http::Request::builder()
        .method(method)
        .uri(target)
        .header("host", domain)
        .body(::bytes::Bytes::new())
        .expect("the viewer request is well formed");
    router.dispatch(request).await.status().as_u16()
}

/// One log event of the destination group: which stream it landed in and what it says.
#[derive(Debug, Clone)]
struct Event {
    stream: String,
    message: String,
}

/// Every event the group `name` holds, as `FilterLogEvents` reports them.
async fn group_events(router: &MultiServiceRouter, name: &str) -> Vec<Event> {
    let page = logs(router, "FilterLogEvents", json!({"logGroupName": name})).await;
    page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("FilterLogEvents reports an events list, got {page}"))
        .iter()
        .map(|event| Event {
            stream: event["logStreamName"].as_str().unwrap_or_default().to_string(),
            message: event["message"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// Look again until the group `name` holds `expected` events — the records the requests already
/// served owe it — or a generous deadline passes, which fails the case.
///
/// The CONDITION is what is waited for, never a span of the host's clock.
async fn awaited_events(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: usize,
) -> Vec<Event> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let events = group_events(router, name).await;
        if events.len() >= expected {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected {expected} access-log record(s) in {name}, got {events:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One record read as the JSON object the structured output format writes.
fn parsed(case: &str, message: &str) -> Value {
    ::serde_json::from_str(message)
        .unwrap_or_else(|error| panic!("case {case}: the record is no JSON ({error}): {message}"))
}

/// One viewer request a row drives, and what the record of it has to say.
struct Probe {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    query: &'static str,
    status: u16,
}

/// Every served viewer request appends one access-log record to the delivery's log group, and
/// each record names THAT request: its path, its method, its query string, the status the edge
/// served it with and the host the viewer addressed.
#[tokio::test]
async fn each_served_viewer_request_appears_as_one_record_in_the_deliverys_log_group() {
    let router = emulator_with_logs();
    let group_arn = log_group(&router, "/aws/vendedlogs/cloudfront-access").await;
    let distribution = distribution(&router, "standard-logging-v2").await;
    let fields = delivery(
        &router,
        "cf-access-logs",
        &distribution.arn,
        &group_arn,
        "json",
        None,
        None,
    )
    .await;
    for mandatory in ["date", "time", "cs-uri-stem", "sc-status", "x-host-header"] {
        assert!(
            fields.iter().any(|field| field == mandatory),
            "case the default record layout: expected the delivery to carry {mandatory}, got \
             {fields:?}"
        );
    }
    awaited_deployment(&router, "standard-logging-v2", &distribution.id).await;

    // Rows differ along what the rule generalises over: the request's method, the path it
    // addressed (a path the origin serves and one it does not), and whether it carried a query
    // string.
    let probes: [Probe; 4] = [
        Probe {
            case: "a read the origin serves",
            method: "GET",
            path: "/2020-05-31/distribution",
            query: "",
            status: 200,
        },
        Probe {
            case: "the same read with a query string",
            method: "GET",
            path: "/2020-05-31/distribution",
            query: "MaxItems=1&Marker=none",
            status: 200,
        },
        Probe {
            case: "a head naming no action of the origin's API",
            method: "HEAD",
            path: "/2020-05-31/distribution",
            query: "",
            status: 404,
        },
        Probe {
            case: "a path the origin answers no object for",
            method: "GET",
            path: "/2020-05-31/distribution/E0NOTTHERE0000",
            query: "",
            status: 404,
        },
    ];

    for probe in &probes {
        let target = if probe.query.is_empty() {
            probe.path.to_string()
        } else {
            format!("{}?{}", probe.path, probe.query)
        };
        let status = view(&router, &distribution.domain, probe.method, &target).await;
        assert_eq!(
            status, probe.status,
            "case {}: expected the edge to answer {}, got {status}",
            probe.case, probe.status
        );
    }

    let events = awaited_events(
        &router,
        "the whole table",
        "/aws/vendedlogs/cloudfront-access",
        probes.len(),
    )
    .await;
    assert_eq!(
        events.len(),
        probes.len(),
        "case the whole table: expected one record per served request, got {events:?}"
    );
    for event in &events {
        assert_eq!(
            event.stream, distribution.id,
            "case the whole table: expected the records in the sending distribution's own \
             stream, got {event:?}"
        );
    }
    for probe in &probes {
        let case = probe.case;
        // WHICH record describes this probe is what the record itself says: the request it
        // names. So each probe is matched by the values that identify its own request rather
        // than by the position the group reports it in.
        let expected_query = if probe.query.is_empty() {
            "-".to_string()
        } else {
            probe.query.to_string()
        };
        let matched: Vec<Value> = events
            .iter()
            .map(|event| parsed(case, &event.message))
            .filter(|record| {
                record["cs-uri-stem"].as_str() == Some(probe.path)
                    && record["cs-method"].as_str() == Some(probe.method)
                    && record["cs-uri-query"].as_str() == Some(expected_query.as_str())
            })
            .collect();
        assert_eq!(
            matched.len(),
            1,
            "case {case}: expected exactly one record naming this request, got {matched:?} \
             among {events:?}"
        );
        let record = matched.into_iter().next().unwrap_or(Value::Null);
        for (field, expected) in [
            ("sc-status", probe.status.to_string()),
            ("x-host-header", distribution.domain.clone()),
            ("cs-host", distribution.domain.clone()),
        ] {
            let got = record[field].as_str().unwrap_or_default();
            assert_eq!(
                got, expected,
                "case {case}: expected {field} {expected:?} in the record, got {got:?} from \
                 {record}"
            );
        }
        // The two calendar halves of the instant the edge finished responding, each in the form
        // the standard access log writes it in.
        let date = record["date"].as_str().unwrap_or_default();
        let time = record["time"].as_str().unwrap_or_default();
        assert!(
            date.len() == 10 && date.split('-').count() == 3,
            "case {case}: expected a date of the form 2019-06-26, got {date:?}"
        );
        assert!(
            time.len() == 8 && time.split(':').count() == 3,
            "case {case}: expected a time of the form 01:42:39, got {time:?}"
        );
        // Every field the delivery carries is in the record, and nothing else is.
        let keys: Vec<String> = record
            .as_object()
            .unwrap_or_else(|| panic!("case {case}: the record is no JSON object: {record}"))
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys.len(),
            fields.len(),
            "case {case}: expected the delivery's {} field(s) in the record, got {keys:?}",
            fields.len()
        );
        for field in &fields {
            assert!(
                record.get(field).is_some(),
                "case {case}: expected the field {field} in the record, got {record}"
            );
        }
    }
}

/// One row of the record-layout table: how the delivery is configured, and the record its
/// served request is then owed.
struct Layout {
    case: &'static str,
    output_format: &'static str,
    record_fields: Option<&'static [&'static str]>,
    field_delimiter: Option<&'static str>,
}

/// A record carries the delivery's own record fields, in its own order, in its destination's
/// output format: the structured format as one JSON object, and each text format as the values
/// joined by the delivery's own field delimiter.
#[tokio::test]
async fn a_record_carries_the_fields_and_output_format_the_delivery_states() {
    let router = emulator_with_logs();
    // Rows differ along what the rule generalises over: the output format (the structured one
    // and each delimited one), whether the delivery selects its record fields or takes the
    // layout the log type publishes, the ORDER a selection is stated in, and the delimiter a
    // delimited format joins with.
    let rows: [Layout; 5] = [
        Layout {
            case: "the structured format over the published layout",
            output_format: "json",
            record_fields: None,
            field_delimiter: None,
        },
        Layout {
            case: "the structured format over a selection, in the caller's order",
            output_format: "json",
            record_fields: Some(&["sc-status", "cs-uri-stem", "date", "time"]),
            field_delimiter: None,
        },
        Layout {
            case: "the plain format, tab delimited",
            output_format: "plain",
            record_fields: Some(&["date", "time", "cs-uri-stem", "sc-status"]),
            field_delimiter: Some("\t"),
        },
        Layout {
            case: "the w3c format, space delimited",
            output_format: "w3c",
            record_fields: Some(&["cs-method", "cs-uri-stem", "sc-status"]),
            field_delimiter: Some(" "),
        },
        Layout {
            case: "the raw format, comma delimited",
            output_format: "raw",
            record_fields: Some(&["cs-uri-stem", "sc-status"]),
            field_delimiter: Some(","),
        },
    ];

    struct Staged {
        case: &'static str,
        group: String,
        fields: Vec<String>,
        delimiter: Option<&'static str>,
        structured: bool,
        distribution: Distribution,
    }
    let mut staged: Vec<Staged> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("cf-layout-{index}");
        let group = format!("/aws/vendedlogs/{name}");
        let group_arn = log_group(&router, &group).await;
        let distribution = distribution(&router, &name).await;
        let fields = delivery(
            &router,
            &name,
            &distribution.arn,
            &group_arn,
            row.output_format,
            row.record_fields,
            row.field_delimiter,
        )
        .await;
        if let Some(selected) = row.record_fields {
            let expected: Vec<String> =
                selected.iter().map(|field| (*field).to_string()).collect();
            assert_eq!(
                fields, expected,
                "case {}: expected the delivery to carry the selection in the order it was \
                 stated, got {fields:?}",
                row.case
            );
        }
        staged.push(Staged {
            case: row.case,
            group,
            fields,
            delimiter: row.field_delimiter,
            structured: row.output_format == "json",
            distribution,
        });
    }
    for row in &staged {
        awaited_deployment(&router, row.case, &row.distribution.id).await;
    }

    let path = format!("/{API_VERSION}/distribution");
    for row in &staged {
        let case = row.case;
        let status = view(&router, &row.distribution.domain, "GET", &path).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the origin's answer to the viewer request, got {status}"
        );
        let events = awaited_events(&router, case, &row.group, 1).await;
        assert_eq!(
            events.len(),
            1,
            "case {case}: expected one record for one served request, got {events:?}"
        );
        let message = &events[0].message;
        if row.structured {
            let record = parsed(case, message);
            let keys: Vec<String> = record
                .as_object()
                .unwrap_or_else(|| panic!("case {case}: the record is no JSON object: {record}"))
                .keys()
                .cloned()
                .collect();
            assert_eq!(
                keys.len(),
                row.fields.len(),
                "case {case}: expected the delivery's {} field(s), got {keys:?}",
                row.fields.len()
            );
            let got = record["cs-uri-stem"].as_str().unwrap_or_default();
            assert_eq!(
                got, path,
                "case {case}: expected the served path in the record, got {got:?} from {record}"
            );
        } else {
            let delimiter = row.delimiter.unwrap_or_default();
            let values: Vec<&str> = message.split(delimiter).collect();
            assert_eq!(
                values.len(),
                row.fields.len(),
                "case {case}: expected one value per record field {:?} joined by \
                 {delimiter:?}, got {message:?}",
                row.fields
            );
            for (field, value) in row.fields.iter().zip(&values) {
                let expected = match field.as_str() {
                    "cs-uri-stem" => Some(path.clone()),
                    "sc-status" => Some("200".to_string()),
                    "cs-method" => Some("GET".to_string()),
                    _ => None,
                };
                if let Some(expected) = expected {
                    assert_eq!(
                        *value, expected,
                        "case {case}: expected {field} {expected:?} in {message:?}, got {value:?}"
                    );
                }
            }
        }
    }
}

/// One row of the no-delivery table: which half of the pipeline is missing.
struct Unwired {
    case: &'static str,
    /// Whether a delivery source for the distribution is created at all.
    source: bool,
    /// Whether a delivery pairs that source with the log group.
    delivery: bool,
    /// Whether the delivery's destination is a log group rather than a bucket.
    log_group_destination: bool,
}

/// A distribution whose access logs no delivery carries into a log group leaves nothing in it:
/// a pipeline that was never wired, a source no delivery pairs, and a delivery whose
/// destination is not a log group all deliver no record.
#[tokio::test]
async fn a_distribution_no_log_group_delivery_names_leaves_the_group_empty() {
    let router = emulator_with_logs();
    let rows: [Unwired; 3] = [
        Unwired {
            case: "no delivery pipeline at all",
            source: false,
            delivery: false,
            log_group_destination: true,
        },
        Unwired {
            case: "a delivery source no delivery pairs",
            source: true,
            delivery: false,
            log_group_destination: true,
        },
        Unwired {
            case: "a delivery whose destination is a bucket, not a log group",
            source: true,
            delivery: true,
            log_group_destination: false,
        },
    ];

    struct Staged {
        case: &'static str,
        group: String,
        distribution: Distribution,
    }
    let mut staged: Vec<Staged> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("cf-unwired-{index}");
        let group = format!("/aws/vendedlogs/{name}");
        let group_arn = log_group(&router, &group).await;
        let distribution = distribution(&router, &name).await;
        if row.source && row.delivery {
            let destination_arn = if row.log_group_destination {
                group_arn.clone()
            } else {
                format!("arn:aws:s3:::{name}")
            };
            delivery(
                &router,
                &name,
                &distribution.arn,
                &destination_arn,
                "json",
                None,
                None,
            )
            .await;
        } else if row.source {
            logs(
                &router,
                "PutDeliverySource",
                json!({
                    "name": name,
                    "resourceArn": distribution.arn,
                    "logType": ACCESS_LOGS,
                }),
            )
            .await;
        }
        staged.push(Staged {
            case: row.case,
            group,
            distribution,
        });
    }
    for row in &staged {
        awaited_deployment(&router, row.case, &row.distribution.id).await;
    }

    let path = format!("/{API_VERSION}/distribution");
    for row in &staged {
        let case = row.case;
        let status = view(&router, &row.distribution.domain, "GET", &path).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the origin's answer to the viewer request, got {status}"
        );
        // The write a delivery owes is made while the request is served, so a group that is
        // empty once the answer is in hand is a group nothing was delivered to.
        let events = group_events(&router, &row.group).await;
        assert!(
            events.is_empty(),
            "case {case}: expected no access-log record in {}, got {events:?}",
            row.group
        );
    }
}

/// `x-host-header` is "the value of the Host header in the viewer request": the name the
/// viewer addressed the DISTRIBUTION by. A viewer that reached the edge through an addressing
/// of this emulator's own — the reserved data-plane path, or the distribution id as the first
/// segment — addressed the distribution by the name it publishes, so that is what the record
/// states however the request arrived.
#[tokio::test]
async fn a_record_names_the_distribution_the_viewer_addressed_however_the_request_arrived() {
    let router = emulator_with_logs();
    let group = "/aws/vendedlogs/cloudfront-addressing";
    let group_arn = log_group(&router, group).await;
    let distribution = distribution(&router, "addressing").await;
    delivery(
        &router,
        "cf-addressing",
        &distribution.arn,
        &group_arn,
        "json",
        Some(&["date", "time", "cs-uri-stem", "x-host-header", "cs-host"]),
        None,
    )
    .await;
    awaited_deployment(&router, "addressing", &distribution.id).await;

    let path = format!("/{API_VERSION}/distribution");
    // Rows differ along the ADDRESSING the viewer reached the edge by: the domain name the
    // distribution published, the reserved data-plane path on this emulator's own endpoint, and
    // the distribution id as the first path segment.
    let cases: [(&str, String, String); 3] = [
        (
            "host-style, at the domain name CloudFront assigned",
            distribution.domain.clone(),
            path.clone(),
        ),
        (
            "the reserved data-plane path, on the emulator's own endpoint",
            "127.0.0.1:9297".to_string(),
            format!("/_aws/cloudfront/{}{path}", distribution.id),
        ),
        (
            "path-style, the distribution id as the first segment",
            "127.0.0.1:9297".to_string(),
            format!("/{}{path}", distribution.id),
        ),
    ];

    for (index, (case, host, target)) in cases.iter().enumerate() {
        let status = view(&router, host, "GET", target).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the origin's answer to the viewer request, got {status}"
        );
        let events = awaited_events(&router, case, group, index + 1).await;
        let record = parsed(case, &events[index].message);
        for field in ["x-host-header", "cs-host"] {
            let got = record[field].as_str().unwrap_or_default();
            assert_eq!(
                got, distribution.domain,
                "case {case}: expected {field} to name the distribution's own domain {:?}, got \
                 {got:?} from {record}",
                distribution.domain
            );
        }
        let served = record["cs-uri-stem"].as_str().unwrap_or_default();
        assert_eq!(
            served, path,
            "case {case}: expected the viewer path in the record, got {served:?} from {record}"
        );
    }
}

/// A delivery to a log group reports the log-group destination type, which is what a client
/// reads to tell where its records go.
#[tokio::test]
async fn a_log_group_delivery_reports_the_log_group_destination_type() {
    let router = emulator_with_logs();
    let group_arn = log_group(&router, "/aws/vendedlogs/cloudfront-destination-type").await;
    let distribution = distribution(&router, "destination-type").await;
    logs(
        &router,
        "PutDeliverySource",
        json!({
            "name": "cf-destination-type",
            "resourceArn": distribution.arn,
            "logType": ACCESS_LOGS,
        }),
    )
    .await;
    let destination = logs(
        &router,
        "PutDeliveryDestination",
        json!({
            "name": "cf-destination-type",
            "outputFormat": "json",
            "deliveryDestinationConfiguration": {"destinationResourceArn": group_arn},
        }),
    )
    .await;
    let created = logs(
        &router,
        "CreateDelivery",
        json!({
            "deliverySourceName": "cf-destination-type",
            "deliveryDestinationArn": destination["deliveryDestination"]["arn"],
        }),
    )
    .await;
    assert_eq!(
        created["delivery"]["deliveryDestinationType"].as_str(),
        Some(CWL),
        "case a log-group destination: expected the delivery to report {CWL}, got {created}"
    );
}

// ---------------------------------------------------------------------------
// The other two kinds of destination a delivery can name
// ---------------------------------------------------------------------------

/// The `DeliveryDestinationType` of a destination that is an Amazon S3 bucket, and of one that
/// is a Firehose delivery stream (`smithy_query(query="shape",shape="DeliveryDestinationType")`).
const S3: &str = "S3";
const FH: &str = "FH";

/// Amazon S3's and Firehose's own endpoints of this Region, and the `X-Amz-Target` prefix
/// Firehose's `awsJson1_1` operations are named under (that model's `targetPrefix`). IAM is a
/// partition-global endpoint, so its host carries no Region.
const S3_HOST: &str = "s3.us-east-1.amazonaws.com";
const FIREHOSE_HOST: &str = "firehose.us-east-1.amazonaws.com";
const FIREHOSE_TARGET: &str = "Firehose_20150804";
const IAM_HOST: &str = "iam.amazonaws.com";

/// The record layout of the delivery that writes OBJECTS, and of the one that puts records on a
/// delivery stream: two deliveries of one source whose `recordFields` differ in which fields
/// they name, how many, and in what order.
const BUCKET_RECORD_FIELDS: &[&str] = &[
    "date",
    "time",
    "cs-uri-stem",
    "sc-status",
    "x-edge-result-type",
    "x-edge-request-id",
];
const STREAM_RECORD_FIELDS: &[&str] = &["date", "time", "x-edge-request-id", "cs-method", "sc-status"];

/// The prefix the delivery stream's own Amazon S3 destination writes its objects under.
const STREAM_DESTINATION_PREFIX: &str = "from-the-stream/";

/// The status a Firehose delivery stream that can take a record reports
/// (`DeliveryStreamStatus`).
const STREAM_ACTIVE: &str = "ACTIVE";

/// The trust policy the delivery role carries: Firehose makes the delivery under it, so that
/// service principal may assume it.
const FIREHOSE_TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"#,
    r#""firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The role's permissions: the object write the stream's delivery makes.
const FIREHOSE_ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Action":["s3:PutObject"],"Resource":"*"}]}"#
);

/// One router carrying CloudFront, CloudWatch Logs, Amazon S3, Firehose and IAM over one shared
/// engine: the delivery pipeline lives in CloudWatch Logs, the records travel from CloudFront,
/// and the destinations a delivery can name are resources of the other three — so all five have
/// to be mounted together for a record to have anywhere to land.
fn emulator_with_delivery_destinations() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One request to the service `signing_name` serves, addressed to `host`; answers its status
/// and its body bytes.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(::bytes::Bytes::from(body))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body().to_vec())
}

/// `s3:CreateBucket` for a bucket of this Region, which a destination then names.
async fn created_bucket(router: &MultiServiceRouter, bucket: &str) {
    let (status, body) = send(router, "s3", S3_HOST, "PUT", &format!("/{bucket}"), &[], Vec::new())
        .await;
    assert_eq!(
        status,
        200,
        "case creating {bucket}: expected HTTP 200, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
}

/// Every key the bucket holds under `prefix`, as `ListObjectsV2` reports them, in key order.
async fn keys_under(router: &MultiServiceRouter, bucket: &str, prefix: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        S3_HOST,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case listing {bucket} under {prefix:?}: expected HTTP 200, got {status} with {text}"
    );
    let mut keys: Vec<String> = text
        .split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</Key>"))
        .map(|(key, _after)| key.to_string())
        .collect();
    keys.sort();
    keys
}

/// The bytes the bucket holds at `key`, as `GetObject` answers them.
async fn object_bytes(router: &MultiServiceRouter, bucket: &str, key: &str) -> Vec<u8> {
    let (status, body) = send(
        router,
        "s3",
        S3_HOST,
        "GET",
        &format!("/{bucket}/{key}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case reading {key}: expected HTTP 200, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// One `awsQuery` IAM action that must succeed.
async fn iam_action(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(
            &::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        );
    }
    let (status, answered) = send(
        router,
        "iam",
        IAM_HOST,
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case IAM {action}: expected HTTP 200, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}

/// One Firehose operation, answering its JSON body.
async fn firehose_call(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answered) = send(
        router,
        "firehose",
        FIREHOSE_HOST,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("{FIREHOSE_TARGET}.{operation}")),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "case Firehose {operation}: expected HTTP 200, got {status} with {text}"
    );
    if text.trim().is_empty() {
        return Value::Null;
    }
    ::serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("case Firehose {operation}: no JSON ({error}): {text}"))
}

/// Look again until Firehose reports the stream `name` is [`STREAM_ACTIVE`] — the clock owes it
/// that status from the moment it was created — or a generous deadline passes.
///
/// The CONDITION is what is waited for, never a span of the host's clock.
async fn awaited_delivery_stream(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let described = firehose_call(
            router,
            "DescribeDeliveryStream",
            json!({"DeliveryStreamName": name}),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == STREAM_ACTIVE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case the delivery stream {name}: expected {STREAM_ACTIVE}, got {status:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One VIEWER request, addressed host-style at the name the distribution published and carrying
/// no credential. Answers the status the edge served and the `X-Amz-Cf-Id` it stamped on that
/// answer, which is the identifier the record of this request is joined to it by.
async fn viewed(
    router: &MultiServiceRouter,
    domain: &str,
    method: &str,
    target: &str,
) -> (u16, String) {
    let request = ::http::Request::builder()
        .method(method)
        .uri(target)
        .header("host", domain)
        .body(::bytes::Bytes::new())
        .expect("the viewer request is well formed");
    let answer = router.dispatch(request).await;
    let status = answer.status().as_u16();
    let request_id = answer
        .headers()
        .get("x-amz-cf-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, request_id)
}

/// Every record the bucket holds under `prefix`, in key order: each object is read, inflated
/// where it is a gzip stream, and split into the one-record-per-line form a delivered log file
/// carries.
async fn delivered_records(router: &MultiServiceRouter, bucket: &str, prefix: &str) -> Vec<Value> {
    use ::std::io::Read as _;

    let mut records = Vec::new();
    for key in keys_under(router, bucket, prefix).await {
        let bytes = object_bytes(router, bucket, &key).await;
        let text = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut inflated = String::new();
            ::flate2::read::GzDecoder::new(bytes.as_slice())
                .read_to_string(&mut inflated)
                .unwrap_or_else(|error| panic!("case reading {key}: not a gzip stream: {error}"));
            inflated
        } else {
            String::from_utf8_lossy(&bytes).to_string()
        };
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            records.push(::serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("case reading {key}: a record is no JSON ({error}): {line}")
            }));
        }
    }
    records
}

/// Look again until the bucket holds `wanted` records under `prefix` — the records the requests
/// already served owe it — or a generous deadline passes, which fails the case.
///
/// The CONDITION is what is waited for, never a span of the host's clock.
async fn awaited_records(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let held = delivered_records(router, bucket, prefix).await;
        if held.len() >= wanted {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected {wanted} record(s) under {bucket}/{prefix}, got {held:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The record of `request_id` among `records`, which is the one record naming that request.
fn record_of<'r>(case: &str, records: &'r [Value], request_id: &str) -> &'r Value {
    let matched: Vec<&Value> = records
        .iter()
        .filter(|record| record["x-edge-request-id"].as_str() == Some(request_id))
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "case {case}: expected exactly one record naming the request {request_id:?}, got \
         {matched:?} among {records:?}"
    );
    matched[0]
}

/// Every field of `record`, in the order the record carries them.
fn fields_of(case: &str, record: &Value) -> Vec<String> {
    record
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: the record is no JSON object: {record}"))
        .keys()
        .cloned()
        .collect()
}

/// A delivery destination is "an Amazon Web Services resource that represents an Amazon Web
/// Services service that logs can be sent to", and `PutDeliveryDestination` names three that
/// receive log records: "a log group in CloudWatch Logs, an Amazon S3 bucket, or a delivery
/// stream in Firehose". So a distribution's access logs fan out to EVERY destination its
/// deliveries name, whichever of those kinds each is: a served viewer request produces one
/// record in the destination BUCKET, as the gzipped log file a reader of a bucket of vended logs
/// fetches under the published object layout, and one record on the destination DELIVERY STREAM,
/// which that stream then delivers to its own Amazon S3 destination — and the two records of one
/// request carry the same `x-edge-request-id`, the identifier the edge answer handed the viewer
/// as `X-Amz-Cf-Id`, so a consumer can join them.
#[tokio::test]
async fn a_served_request_reaches_the_bucket_and_the_delivery_stream_the_deliveries_name() {
    let router = emulator_with_delivery_destinations();
    let account = ::engine::emulator_account_id();
    let vended_bucket = "cloudfront-vended-access-logs";
    let stream_bucket = "cloudfront-stream-access-logs";
    let stream = "cloudfront-access-logs";
    let role = "cloudfront-access-logs-delivery";

    // The two buckets, and the role the delivery stream's own delivery is made under.
    created_bucket(&router, vended_bucket).await;
    created_bucket(&router, stream_bucket).await;
    iam_action(
        &router,
        "CreateRole",
        &[
            ("RoleName", role),
            ("AssumeRolePolicyDocument", FIREHOSE_TRUST_POLICY),
        ],
    )
    .await;
    iam_action(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "deliver-objects"),
            ("PolicyDocument", FIREHOSE_ROLE_POLICY),
        ],
    )
    .await;
    firehose_call(
        &router,
        "CreateDeliveryStream",
        json!({
            "DeliveryStreamName": stream,
            "DeliveryStreamType": "DirectPut",
            "ExtendedS3DestinationConfiguration": {
                "RoleARN": format!("arn:aws:iam::{account}:role/{role}"),
                "BucketARN": format!("arn:aws:s3:::{stream_bucket}"),
                "Prefix": STREAM_DESTINATION_PREFIX,
                // The shortest buffering the hints admit, so the stream's own delivery waits
                // for nothing but the emulator's own flush span.
                "BufferingHints": {"SizeInMBs": 1, "IntervalInSeconds": 0},
            },
        }),
    )
    .await;

    let distribution = distribution(&router, "logging-v2-fan-out").await;
    // The ONE source both deliveries below are paired with: the distribution is what sends the
    // records.
    logs(
        &router,
        "PutDeliverySource",
        json!({
            "name": "cf-fan-out",
            "resourceArn": distribution.arn,
            "logType": ACCESS_LOGS,
        }),
    )
    .await;
    // One delivery per kind of destination, each with its own record layout.
    let bucket_destination_arn = format!("arn:aws:s3:::{vended_bucket}");
    let stream_destination_arn =
        format!("arn:aws:firehose:{REGION}:{account}:deliverystream/{stream}");
    let wiring: [(&str, &str, &str, &[&str]); 2] = [
        (
            "the destination bucket",
            S3,
            &bucket_destination_arn,
            BUCKET_RECORD_FIELDS,
        ),
        (
            "the destination delivery stream",
            FH,
            &stream_destination_arn,
            STREAM_RECORD_FIELDS,
        ),
    ];
    for (index, (case, destination_type, resource_arn, fields)) in wiring.iter().enumerate() {
        let destination = logs(
            &router,
            "PutDeliveryDestination",
            json!({
                "name": format!("cf-fan-out-{index}"),
                "outputFormat": "json",
                "deliveryDestinationConfiguration": {"destinationResourceArn": resource_arn},
            }),
        )
        .await;
        assert_eq!(
            destination["deliveryDestination"]["deliveryDestinationType"].as_str(),
            Some(*destination_type),
            "case {case}: expected the destination to report {destination_type}, got {destination}"
        );
        let created = logs(
            &router,
            "CreateDelivery",
            json!({
                "deliverySourceName": "cf-fan-out",
                "deliveryDestinationArn": destination["deliveryDestination"]["arn"],
                "recordFields": fields,
            }),
        )
        .await;
        assert_eq!(
            created["delivery"]["deliveryDestinationType"].as_str(),
            Some(*destination_type),
            "case {case}: expected the delivery to report {destination_type}, got {created}"
        );
    }

    awaited_deployment(&router, "logging-v2-fan-out", &distribution.id).await;
    awaited_delivery_stream(&router, stream).await;

    // Rows differ along what the rule generalises over: the path the viewer addressed, whether
    // it carried a query string, and the status the edge served it with.
    let probes: [Probe; 3] = [
        Probe {
            case: "a read the origin serves",
            method: "GET",
            path: "/2020-05-31/distribution",
            query: "",
            status: 200,
        },
        Probe {
            case: "the same read with a query string",
            method: "GET",
            path: "/2020-05-31/distribution",
            query: "MaxItems=1",
            status: 200,
        },
        Probe {
            case: "a path the origin answers no object for",
            method: "GET",
            path: "/2020-05-31/distribution/E0NOTTHERE0000",
            query: "",
            status: 404,
        },
    ];
    let mut served: Vec<(&Probe, String)> = Vec::new();
    for probe in &probes {
        let target = if probe.query.is_empty() {
            probe.path.to_string()
        } else {
            format!("{}?{}", probe.path, probe.query)
        };
        let (status, request_id) = viewed(&router, &distribution.domain, probe.method, &target).await;
        assert_eq!(
            status, probe.status,
            "case {}: expected the edge to answer {}, got {status}",
            probe.case, probe.status
        );
        assert!(
            !request_id.is_empty(),
            "case {}: expected the answer to carry an X-Amz-Cf-Id",
            probe.case
        );
        assert!(
            served.iter().all(|(_probe, earlier)| *earlier != request_id),
            "case {}: expected a request id of its own, got {request_id:?} again",
            probe.case
        );
        served.push((probe, request_id));
    }

    // The BUCKET destination: one gzipped log file per served request, under the published
    // object layout.
    let layout = format!("AWSLogs/{account}/cloudfront/");
    let keys = {
        let case = "the destination bucket";
        let records = awaited_records(&router, case, vended_bucket, &layout, probes.len()).await;
        assert_eq!(
            records.len(),
            probes.len(),
            "case {case}: expected one record per served request, got {records:?}"
        );
        for (probe, request_id) in &served {
            let case = probe.case;
            let record = record_of(case, &records, request_id);
            assert_eq!(
                fields_of(case, record),
                BUCKET_RECORD_FIELDS
                    .iter()
                    .map(|field| (*field).to_string())
                    .collect::<Vec<String>>(),
                "case {case}: expected the delivery's own fields in its own order, got {record}"
            );
            for (field, expected) in [
                ("cs-uri-stem", probe.path.to_string()),
                ("sc-status", probe.status.to_string()),
            ] {
                let got = record[field].as_str().unwrap_or_default();
                assert_eq!(
                    got, expected,
                    "case {case}: expected {field} {expected:?} in the delivered object, got \
                     {got:?} from {record}"
                );
            }
            let result_type = record["x-edge-result-type"].as_str().unwrap_or_default();
            assert!(
                !result_type.is_empty(),
                "case {case}: expected the result type of the answer in the record, got {record}"
            );
        }
        keys_under(&router, vended_bucket, &layout).await
    };
    for key in &keys {
        assert!(
            key.starts_with(&format!(
                "{layout}{REGION}/{}/",
                distribution.id
            )) && key.ends_with(".log.gz"),
            "case the destination bucket: expected the published object layout under \
             {layout}{REGION}/{}/ and a gzipped log file, got {key:?}",
            distribution.id
        );
        let bytes = object_bytes(&router, vended_bucket, key).await;
        assert_eq!(
            bytes.get(..2),
            Some([0x1f, 0x8b].as_slice()),
            "case the destination bucket: expected a gzip stream at {key:?}, got {:?}",
            bytes.get(..2)
        );
    }

    // The DELIVERY STREAM destination: the records the stream took, delivered to the stream's
    // own Amazon S3 destination.
    let case = "the destination delivery stream";
    let records = awaited_records(
        &router,
        case,
        stream_bucket,
        STREAM_DESTINATION_PREFIX,
        probes.len(),
    )
    .await;
    assert_eq!(
        records.len(),
        probes.len(),
        "case {case}: expected one record per served request, got {records:?}"
    );
    for (probe, request_id) in &served {
        let case = probe.case;
        let record = record_of(case, &records, request_id);
        assert_eq!(
            fields_of(case, record),
            STREAM_RECORD_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect::<Vec<String>>(),
            "case {case}: expected the delivery's own fields in its own order, got {record}"
        );
        for (field, expected) in [
            ("cs-method", probe.method.to_string()),
            ("sc-status", probe.status.to_string()),
        ] {
            let got = record[field].as_str().unwrap_or_default();
            assert_eq!(
                got, expected,
                "case {case}: expected {field} {expected:?} in the record the stream delivered, \
                 got {got:?} from {record}"
            );
        }
    }
}
