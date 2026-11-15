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
//!   any group.
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
