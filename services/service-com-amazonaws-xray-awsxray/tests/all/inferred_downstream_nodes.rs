// SPDX-License-Identifier: CC0-1.0

//! The DOWNSTREAM nodes X-Ray infers from the calls a trace's own documents made
//! OUT, as `BatchGetTraces` reports them.
//!
//! The rule under test: **an entity whose `namespace` makes it a call out of the
//! service that recorded it — `aws` for a call through the AWS SDK, `remote` for
//! any other call that left the process — names a downstream service, and X-Ray
//! synthesises one segment for that service hanging from the call's own id,
//! carrying `inferred: true` and typed by what the call named: the type of the
//! RESOURCE its `aws` block addressed where it addressed one, the service itself
//! where it addressed none, and "a downstream service of indeterminate type" for
//! a `remote` call. A call that reached a service which reports segments of its
//! OWN infers nothing: that service's own segment is the node.**
//!
//! The rows differ along what the rule generalises over: WHICH resource the call
//! addressed (a table, a bucket, a queue, a topic, a stream, and none at all),
//! WHICH namespace it recorded, whether the call was recorded as a subsegment
//! carried INLINE in its segment or as a subsegment submitted as its OWN
//! document, and how deeply it sits in the segment's subsegment tree.

use serde_json::{json, Value};

use crate::xray_wire::{array, root_document, trace_id, Xray};

/// The service that submits the documents, and so reports for itself.
const CALLER: &str = "frontend";

/// The window the seeded traces fall inside, and the times every document below
/// carries.
const STARTED: f64 = 1_000.0;
const ENDED: f64 = 1_021.0;

/// The id of the segment every row's caller submits.
const CALLER_SPAN: &str = "1111111111111111";

/// The id of the call out every row records.
const CALL_SPAN: &str = "2222222222222222";

/// The type of a node inferred from a `remote` call: `Service$Type`'s "a
/// downstream service of indeterminate type".
const REMOTE_TYPE: &str = "remote";

/// One call out, as a subsegment of the caller's segment.
fn call_out(name: &str, namespace: Option<&str>, aws: Option<Value>) -> Value {
    let mut entity = json!({
        "id": CALL_SPAN,
        "name": name,
        "start_time": STARTED + 0.1,
        "end_time": ENDED - 0.1,
    });
    if let Some(namespace) = namespace {
        entity["namespace"] = json!(namespace);
    }
    if let Some(aws) = aws {
        entity["aws"] = aws;
    }
    entity
}

/// Every document `BatchGetTraces` holds for `trace`, parsed.
async fn documents_of(xray: &Xray, trace: &str) -> Vec<Value> {
    let answered = xray.batch_get(&[trace.to_string()], None).await;
    assert_eq!(
        answered.status, 200,
        "BatchGetTraces must answer, got {answered:?}"
    );
    array(&answered.body, "Traces")
        .iter()
        .flat_map(|held| array(held, "Segments").to_vec())
        .filter_map(|segment| {
            segment
                .get("Document")
                .and_then(Value::as_str)
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
        })
        .collect()
}

/// The documents of `trace` that X-Ray synthesised, as
/// `(name, parent_id, origin)` — an absent member read as the empty string so a
/// missing one is a visible difference.
async fn inferred_of(xray: &Xray, trace: &str) -> Vec<(String, String, String)> {
    documents_of(xray, trace)
        .await
        .iter()
        .filter(|document| document.get("inferred").and_then(Value::as_bool) == Some(true))
        .map(|document| {
            let member = |name: &str| {
                document
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            (member("name"), member("parent_id"), member("origin"))
        })
        .collect()
}

/// How the row's call out reaches the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// Carried inline in the caller's own segment document.
    Inline,
    /// Nested one level deeper, inside an in-process subsegment of it.
    Nested,
    /// Submitted as its own `type: "subsegment"` document.
    OwnDocument,
}

/// One row: the call the caller recorded, and the node it must infer.
struct Row {
    case: &'static str,
    recorded: Recorded,
    name: &'static str,
    namespace: Option<&'static str>,
    aws: Option<fn() -> Value>,
    /// The `origin` the inferred document must carry, `""` for none.
    origin: &'static str,
    /// Whether a node is inferred at all.
    inferred: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an AWS SDK call that addressed a DynamoDB table",
        recorded: Recorded::Inline,
        name: "DynamoDB",
        namespace: Some("aws"),
        aws: Some(|| json!({ "operation": "PutItem", "table_name": "probe-items" })),
        origin: "AWS::DynamoDB::Table",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call that addressed an S3 bucket",
        recorded: Recorded::Inline,
        name: "S3",
        namespace: Some("aws"),
        aws: Some(|| json!({ "operation": "PutObject", "bucket_name": "probe-bucket" })),
        origin: "AWS::S3::Bucket",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call that addressed an SQS queue",
        recorded: Recorded::Inline,
        name: "SQS",
        namespace: Some("aws"),
        aws: Some(|| {
            json!({
                "operation": "SendMessage",
                "queue_url": "https://sqs.us-east-1.amazonaws.com/000000000000/probe",
            })
        }),
        origin: "AWS::SQS::Queue",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call that addressed an SNS topic",
        recorded: Recorded::Inline,
        name: "SNS",
        namespace: Some("aws"),
        aws: Some(|| {
            json!({
                "operation": "Publish",
                "topic_arn": "arn:aws:sns:us-east-1:000000000000:probe",
            })
        }),
        origin: "AWS::SNS::Topic",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call that addressed a Kinesis stream",
        recorded: Recorded::Inline,
        name: "Kinesis",
        namespace: Some("aws"),
        aws: Some(|| json!({ "operation": "PutRecord", "stream_name": "probe-stream" })),
        origin: "AWS::Kinesis::Stream",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call that addressed no resource of its own",
        recorded: Recorded::Inline,
        name: "Lambda",
        namespace: Some("aws"),
        aws: Some(|| json!({ "operation": "Invoke" })),
        origin: "AWS::Lambda",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call carrying no aws block at all",
        recorded: Recorded::Inline,
        name: "STS",
        namespace: Some("aws"),
        aws: None,
        origin: "AWS::STS",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call whose resource member is empty",
        recorded: Recorded::Inline,
        name: "DynamoDB",
        namespace: Some("aws"),
        aws: Some(|| json!({ "operation": "ListTables", "table_name": "" })),
        origin: "AWS::DynamoDB",
        inferred: true,
    },
    Row {
        case: "a call out of the process that is no AWS SDK call",
        recorded: Recorded::Inline,
        name: "payments.example.test",
        namespace: Some("remote"),
        aws: None,
        origin: "",
        inferred: true,
    },
    Row {
        case: "in-process work, which left the process nowhere",
        recorded: Recorded::Inline,
        name: "InternalPipeline",
        namespace: None,
        aws: None,
        origin: "",
        inferred: false,
    },
    Row {
        case: "an AWS SDK call nested inside in-process work",
        recorded: Recorded::Nested,
        name: "DynamoDB",
        namespace: Some("aws"),
        aws: Some(|| json!({ "operation": "GetItem", "table_name": "probe-items" })),
        origin: "AWS::DynamoDB::Table",
        inferred: true,
    },
    Row {
        case: "an AWS SDK call submitted as its own subsegment document",
        recorded: Recorded::OwnDocument,
        name: "S3",
        namespace: Some("aws"),
        aws: Some(|| json!({ "operation": "PutObject", "bucket_name": "probe-bucket" })),
        origin: "AWS::S3::Bucket",
        inferred: true,
    },
];

#[tokio::test]
async fn a_call_out_infers_one_downstream_node_typed_by_what_it_named() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let xray = Xray::new();
        let nth = u32::try_from(index).expect("the row count fits a u32") + 1;
        let trace = trace_id(nth);
        let call = call_out(row.name, row.namespace, row.aws.map(|build| build()));

        let mut documents = vec![root_document(
            CALLER_SPAN,
            &trace,
            CALLER,
            STARTED,
            Some(ENDED),
        )];
        match row.recorded {
            Recorded::Inline => documents[0]["subsegments"] = json!([call]),
            Recorded::Nested => {
                let mut local = json!({
                    "id": "3333333333333333",
                    "name": "InnerStage",
                    "start_time": STARTED + 0.05,
                    "end_time": ENDED - 0.05,
                });
                local["subsegments"] = json!([call]);
                documents[0]["subsegments"] = json!([local]);
            }
            Recorded::OwnDocument => {
                let mut own = call;
                own["trace_id"] = json!(trace);
                own["type"] = json!("subsegment");
                own["parent_id"] = json!(CALLER_SPAN);
                documents.push(own);
            }
        }

        let put = xray
            .put_segments(
                &documents
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<String>>(),
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: seeding must succeed, got {put:?}"
        );
        assert!(
            array(&put.body, "UnprocessedTraceSegments").is_empty(),
            "case {case:?}: every document must be accepted, got {put:?}"
        );

        let inferred = inferred_of(&xray, &trace).await;
        let expected: Vec<(String, String, String)> = if row.inferred {
            vec![(
                row.name.to_string(),
                CALL_SPAN.to_string(),
                row.origin.to_string(),
            )]
        } else {
            Vec::new()
        };
        assert_eq!(
            inferred, expected,
            "case {case:?}: expected the inferred documents {expected:?}, got {inferred:?}"
        );
    }
}

#[tokio::test]
async fn an_inferred_nodes_type_is_the_one_every_surface_reports_for_it() {
    // The same derivation reaches a trace summary's service list, so a reader
    // that never opens the documents is told the same type — the resource's for
    // an AWS SDK call that addressed one, and `Service$Type`'s own word for a
    // call out of the process that is no AWS SDK call.
    /// One surface row: the case, the call's name, its namespace, its `aws`
    /// block where it carries one, and the type the surface must report.
    type Surface = (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<Value>,
        &'static str,
    );
    let surfaces: [Surface; 2] = [
        (
            "an AWS SDK call that addressed a table",
            "DynamoDB",
            Some("aws"),
            Some(json!({ "operation": "PutItem", "table_name": "probe-items" })),
            "AWS::DynamoDB::Table",
        ),
        (
            "a call out of the process that is no AWS SDK call",
            "payments.example.test",
            Some("remote"),
            None,
            REMOTE_TYPE,
        ),
    ];
    for (index, (case, name, namespace, aws, kind)) in surfaces.into_iter().enumerate() {
        let xray = Xray::new();
        let nth = u32::try_from(index).expect("the row count fits a u32") + 64;
        let trace = trace_id(nth);
        let mut caller = root_document(CALLER_SPAN, &trace, CALLER, STARTED, Some(ENDED));
        caller["subsegments"] = json!([call_out(name, namespace, aws)]);
        let put = xray.put_segments(&[caller.to_string()]).await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: seeding must succeed, got {put:?}"
        );

        let got = xray.summaries(STARTED - 1.0, ENDED + 1.0, None).await;
        let summaries = array(&got.body, "TraceSummaries");
        assert_eq!(
            summaries.len(),
            1,
            "case {case:?}: one trace was stored, got {got:?}"
        );
        let reported: Vec<(String, String)> = array(&summaries[0], "ServiceIds")
            .iter()
            .map(|service| {
                let member = |member: &str| {
                    service
                        .get(member)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                (member("Name"), member("Type"))
            })
            .collect();
        assert!(
            reported.contains(&(name.to_string(), kind.to_string())),
            "case {case:?}: expected the downstream service typed {kind:?} beside the caller, \
             got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_call_whose_service_reports_for_itself_infers_no_node() {
    // The node of a call that reached a service recording for ITSELF is that
    // service's own segment, whichever of the two documents arrives first — so
    // the rule cannot depend on the order a producer submits them in.
    let orders = [
        ("the caller's document first", false),
        ("the downstream service's document first", true),
    ];
    for (index, (case, downstream_first)) in orders.into_iter().enumerate() {
        let xray = Xray::new();
        let nth = u32::try_from(index).expect("the row count fits a u32") + 128;
        let trace = trace_id(nth);
        let mut caller = root_document(CALLER_SPAN, &trace, CALLER, STARTED, Some(ENDED));
        caller["subsegments"] = json!([call_out(
            "Lambda",
            Some("aws"),
            Some(json!({ "operation": "Invoke", "function_name": "probe" })),
        )]);
        // The downstream service's own segment hangs from the call that reached
        // it, which is what makes it the node for that call.
        let mut downstream = root_document(
            "4444444444444444",
            &trace,
            "probe",
            STARTED + 0.2,
            Some(ENDED - 0.2),
        );
        downstream["parent_id"] = json!(CALL_SPAN);
        downstream["origin"] = json!("AWS::Lambda");

        let ordered = if downstream_first {
            vec![downstream.to_string(), caller.to_string()]
        } else {
            vec![caller.to_string(), downstream.to_string()]
        };
        for document in ordered {
            let put = xray.put_segments(&[document]).await;
            assert_eq!(
                put.status, 200,
                "case {case:?}: seeding must succeed, got {put:?}"
            );
        }

        let inferred = inferred_of(&xray, &trace).await;
        assert!(
            inferred.is_empty(),
            "case {case:?}: the reporting service is the node, so nothing is inferred for the \
             call, got {inferred:?}"
        );
        // And the service's own document is still there: nothing was dropped
        // but the node this service had synthesised.
        let origins: Vec<String> = documents_of(&xray, &trace)
            .await
            .iter()
            .filter_map(|document| document.get("origin").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert_eq!(
            origins,
            vec!["AWS::Lambda".to_string()],
            "case {case:?}: expected the downstream service's own segment to stand, got \
             {origins:?}"
        );
    }
}

#[tokio::test]
async fn a_submitted_child_of_a_call_out_is_no_report_for_it() {
    // A SUBSEGMENT submitted under a call out is work inside the service that
    // made the call — an instrumented client's own child of its AWS SDK
    // subsegment is exactly that shape — so it is no report by the downstream
    // service, and the node for that call is still inferred. The two arrival
    // orders again, because neither may decide it.
    let orders = [
        ("the call out's document first", false),
        ("the child's document first", true),
    ];
    for (index, (case, child_first)) in orders.into_iter().enumerate() {
        let xray = Xray::new();
        let nth = u32::try_from(index).expect("the row count fits a u32") + 192;
        let trace = trace_id(nth);
        let mut caller = root_document(CALLER_SPAN, &trace, CALLER, STARTED, Some(ENDED));
        caller["subsegments"] = json!([call_out(
            "DynamoDB",
            Some("aws"),
            Some(json!({ "operation": "PutItem", "table_name": "probe-items" })),
        )]);
        // The client's own attempt of that call, under the call out itself.
        let child = json!({
            "trace_id": trace,
            "id": "7777777777777777",
            "name": "attempt",
            "type": "subsegment",
            "parent_id": CALL_SPAN,
            "start_time": STARTED + 0.15,
            "end_time": ENDED - 0.15,
        });
        let ordered = if child_first {
            vec![child.to_string(), caller.to_string()]
        } else {
            vec![caller.to_string(), child.to_string()]
        };
        for document in ordered {
            let put = xray.put_segments(&[document]).await;
            assert_eq!(
                put.status, 200,
                "case {case:?}: seeding must succeed, got {put:?}"
            );
        }

        let inferred = inferred_of(&xray, &trace).await;
        assert_eq!(
            inferred,
            vec![(
                "DynamoDB".to_string(),
                CALL_SPAN.to_string(),
                "AWS::DynamoDB::Table".to_string(),
            )],
            "case {case:?}: expected the table's node still inferred for the call, got \
             {inferred:?}"
        );
    }
}
