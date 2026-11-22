// SPDX-License-Identifier: CC0-1.0

//! A SimpleDBv2 domain export WRITES the exported domain's items into the caller's Amazon
//! S3 bucket, where a client lists and reads them.
//!
//! RULE: `StartDomainExport` "initiates the export of a SimpleDB domain to an S3 bucket"
//! whose `s3KeyPrefix` is "the prefix string to be used to generate the S3 object keys for
//! export artifacts" (`smithy_query(query="operation",operation="StartDomainExport")`), so
//! the export IS the write: the items the domain holds — the ones a client wrote with the
//! Amazon SimpleDB 2009-04-15 `PutAttributes` this same endpoint serves — are serialized
//! into data objects inside that one export's key space, beside the manifest summary the
//! finished export reports as its `exportManifest` ("the name of the manifest summary file
//! for the export"). That key space is the prefix the start named (verbatim, joined with
//! one separator), the service's fixed `AmazonSimpleDB` segment, the exported domain name
//! and the export's own ARN, so a client that reads the reported key back recovers both.
//! An export pointed at a bucket the object store does not hold writes nothing and reaches
//! `FAILED` carrying that refusal instead of `SUCCEEDED`.
//!
//! The write goes through the in-process callback into this emulator's own object store, so
//! a single-service router has no Amazon S3 to write into: that is why this suite lives
//! here rather than in the service's own crate, whose suites pin the halves of the rule
//! that need no object store.
//!
//! The rows span what the rule generalises over: the shape of the key prefix (a trailing
//! slash, a nested prefix, none at all), how many items the domain holds and whether their
//! attributes are multi-valued, and whether the destination bucket exists at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder an AWS Query form body needs is the one
// `all/cloudfront_key_value_store_handshake.rs` already owns: a form component is escaped
// exactly as a URI path segment is, so the codec is shared rather than written again.
use crate::cloudfront_key_value_store_handshake::segment as form_encode;

/// The Region every call here is signed for, which is where the bucket is created and the
/// Region the export addresses the object store in.
const REGION: &str = "us-west-2";

/// The bucket the exports are written into, and one no create ever makes.
const BUCKET: &str = "simpledb-export-bucket";
const ABSENT_BUCKET: &str = "simpledb-export-bucket-missing";

/// The Amazon SimpleDB API version a 2009-04-15 request names.
const SDB_API_VERSION: &str = "2009-04-15";

/// How long a case polls an export before it concludes the lifecycle is stuck: generous
/// next to the spans the service publishes for it, room for a slow machine rather than a
/// bound on how fast the host runs.
const POLL_BUDGET: Duration = Duration::from_secs(45);

/// How long a case waits between two reads of an export while it polls it.
const POLL_STEP: Duration = Duration::from_millis(50);

/// Amazon S3 and SimpleDBv2 over ONE shared engine, as `router/src/lib.rs` mounts them —
/// so the export's in-process callback reaches this very object store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_simpledbv2_simpledbv2::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes
/// the request and what a service reads the caller's Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket and
/// key live in the URI.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "application/xml")
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One Amazon SimpleDB 2009-04-15 call: `POST "/"` on the `sdb` endpoint with the
/// parameters in the form body, which is the wire that API publishes.
async fn sdb(
    router: &MultiServiceRouter,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut form = format!("Version={SDB_API_VERSION}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sdb.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sdb"))
        .header(
            "content-type",
            "application/x-www-form-urlencoded; charset=utf-8",
        )
        .body(Bytes::from(form))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One modelled SimpleDBv2 call: restJson1 under `/v2/`, as the model binds them.
async fn simpledbv2(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/v2/{operation}"))
        .header("host", format!("sdb.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sdb"))
        .header("content-type", "application/json")
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// `PUT /<bucket>` with the location constraint a bucket outside `us-east-1` carries.
async fn given_bucket(router: &MultiServiceRouter) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the export bucket must be created, got {status} with {body:?}"
    );
}

/// Every key the bucket holds under `prefix`, as `ListObjectsV2` reports them — the call a
/// client makes to find a finished export's artifacts.
async fn listed_keys(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the export bucket must be listable, got {status} with {body:?}"
    );
    body.split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// Poll `GetExport` the way a client does until the export reports a terminal status, and
/// hand back the document that first reported it.
async fn poll_until_terminal(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let started = Instant::now();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (status, document) = simpledbv2(router, "GetExport", &json!({"exportArn": arn})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the export must stay readable while it is polled, got {status} \
             with {document:?}"
        );
        let reported = document
            .get("exportStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if ["SUCCEEDED", "FAILED"].contains(&reported.as_str()) {
            return document;
        }
        if seen.last() != Some(&reported) {
            seen.push(reported);
        }
        assert!(
            started.elapsed() < POLL_BUDGET,
            "case {case:?}: a client polling the export must read a terminal status inside \
             {POLL_BUDGET:?}, saw {seen:?}"
        );
        ::tokio::time::sleep(POLL_STEP).await;
    }
}

/// One row: the domain to populate and export, where it is exported to, and what the
/// export must leave behind.
struct Row {
    case: &'static str,
    domain: &'static str,
    /// The items to write with `PutAttributes`, each with its own attribute pairs.
    items: Vec<(&'static str, Vec<(&'static str, &'static str)>)>,
    /// `s3KeyPrefix`, as the start names it — `None` names none at all.
    prefix: Option<&'static str>,
    /// The bucket the start names.
    bucket: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one item under a prefix ending in a separator",
            domain: "behav-export-domain",
            items: vec![("item-widget", vec![("color", "blue")])],
            prefix: Some("exports/"),
            bucket: BUCKET,
        },
        Row {
            case: "two items, one multi-valued, under a nested prefix",
            domain: "orders-domain",
            items: vec![
                (
                    "order-1",
                    vec![("colour", "blue"), ("colour", "green"), ("size", "large")],
                ),
                ("order-2", vec![("colour", "red")]),
            ],
            prefix: Some("exports/2026/q1/"),
            bucket: BUCKET,
        },
        Row {
            case: "an export naming no key prefix at all",
            domain: "unprefixed-domain",
            items: vec![("item-a", vec![("name", "value")])],
            prefix: None,
            bucket: BUCKET,
        },
        Row {
            case: "an export pointed at a bucket the object store does not hold",
            domain: "absent-bucket-domain",
            items: vec![("item-a", vec![("name", "value")])],
            prefix: Some("exports/"),
            bucket: ABSENT_BUCKET,
        },
    ]
}

#[tokio::test]
async fn a_domain_export_writes_the_items_the_domain_holds_into_the_bucket() {
    let router = emulator();
    given_bucket(&router).await;

    for row in rows() {
        let case = row.case;
        // The domain and its items, written over the API that owns them: the 2009-04-15
        // `CreateDomain` and `PutAttributes` this same endpoint serves.
        let (status, body) = sdb(
            &router,
            &[("Action", "CreateDomain"), ("DomainName", row.domain)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the domain must be created on the sdb wire, got {status} with \
             {body}"
        );
        for (item, pairs) in &row.items {
            let mut parameters: Vec<(String, String)> = vec![
                ("Action".to_string(), "PutAttributes".to_string()),
                ("DomainName".to_string(), row.domain.to_string()),
                ("ItemName".to_string(), (*item).to_string()),
            ];
            for (index, (name, value)) in pairs.iter().enumerate() {
                parameters.push((format!("Attribute.{index}.Name"), (*name).to_string()));
                parameters.push((format!("Attribute.{index}.Value"), (*value).to_string()));
            }
            let borrowed: Vec<(&str, &str)> = parameters
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let (status, body) = sdb(&router, &borrowed).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the item {item:?} must be written, got {status} with {body}"
            );
        }

        let mut start = json!({"domainName": row.domain, "s3Bucket": row.bucket});
        if let Some(prefix) = row.prefix {
            start["s3KeyPrefix"] = json!(prefix);
        }
        let (status, accepted) = simpledbv2(&router, "StartDomainExport", &start).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the export must be accepted, got {status} with {accepted:?}"
        );
        let arn = accepted
            .get("exportArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !arn.is_empty(),
            "case {case:?}: the start answers the export's ARN, got {accepted:?}"
        );
        let export_id = arn.rsplit('/').next().unwrap_or_default().to_string();
        let prefix = row.prefix.unwrap_or("");

        let settled = poll_until_terminal(&router, case, &arn).await;
        let reported = settled
            .get("exportStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if row.bucket != BUCKET {
            assert_eq!(
                reported, "FAILED",
                "case {case:?}: an export that cannot write its data reaches FAILED, got \
                 {settled:?}"
            );
            assert_eq!(
                settled.get("failureCode").and_then(Value::as_str),
                Some("NoSuchBucket"),
                "case {case:?}: the failure names what the object store answered, got \
                 {settled:?}"
            );
            assert!(
                settled
                    .get("failureMessage")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains(ABSENT_BUCKET)),
                "case {case:?}: the failure message names the bucket it is about, got \
                 {settled:?}"
            );
            assert!(
                settled.get("exportManifest").is_none(),
                "case {case:?}: an export that wrote nothing reports no manifest, got \
                 {settled:?}"
            );
            // And nothing of it reached the bucket that does exist.
            let keys = listed_keys(&router, case, prefix).await;
            assert!(
                keys.iter().all(|key| !key.contains(&export_id)),
                "case {case:?}: a failed export writes no object, got {keys:?}"
            );
            continue;
        }

        assert_eq!(
            reported, "SUCCEEDED",
            "case {case:?}: an export whose data was written reaches SUCCEEDED, got \
             {settled:?}"
        );
        assert_eq!(
            settled.get("itemsCount").and_then(Value::as_i64),
            Some(row.items.len() as i64),
            "case {case:?}: the finished export counts the items it exported, got {settled:?}"
        );

        // What the client does next: list the prefix it named, and read the export's data
        // out of the objects it finds there.
        let keys = listed_keys(&router, case, prefix).await;
        let manifest_key = settled
            .get("exportManifest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            keys.contains(&manifest_key),
            "case {case:?}: the manifest the export reports ({manifest_key:?}) must be an \
             object of the bucket, got {keys:?}"
        );
        // The key space one export's objects sit in: the prefix the start named (verbatim,
        // joined to the fixed segment with one separator), the service's own fixed
        // segment, the exported domain and the export's ARN.
        let domain = row.domain;
        let export_space = match row.prefix {
            Some(prefix) => format!("{prefix}/AmazonSimpleDB/{domain}/{arn}/"),
            None => format!("AmazonSimpleDB/{domain}/{arn}/"),
        };
        let data_keys: Vec<&String> = keys
            .iter()
            .filter(|key| key.starts_with(&export_space) && **key != manifest_key)
            .collect();
        assert!(
            !data_keys.is_empty(),
            "case {case:?}: the export writes its data under {export_space:?}, got {keys:?}"
        );
        assert_eq!(
            manifest_key,
            format!("{export_space}manifest-summary.json"),
            "case {case:?}: the reported manifest is the summary file of the export's own \
             key space {export_space:?}, got {manifest_key:?}"
        );

        let mut exported: Vec<Value> = Vec::new();
        for key in &data_keys {
            let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the data object {key:?} must be readable, got {status} with \
                 {body:?}"
            );
            let records: Vec<Value> = serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("case {case:?}: the data object {key:?} is not JSON: {error}: {body}")
            });
            exported.extend(records);
        }
        assert_eq!(
            exported.len(),
            row.items.len(),
            "case {case:?}: the export carries one record per item of the domain, got \
             {exported:?}"
        );
        for (item, pairs) in &row.items {
            let record = exported
                .iter()
                .find(|record| record["itemName"] == json!(item))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the item {item:?} must be exported, got {exported:?}")
                });
            for (name, value) in pairs {
                assert!(
                    record["attributes"].as_array().is_some_and(|written| written
                        .iter()
                        .any(|attribute| attribute["name"] == json!(name)
                            && attribute["values"]
                                .as_array()
                                .is_some_and(|values| values.contains(&json!(value))))),
                    "case {case:?}: the pair {name:?}={value:?} the client wrote on {item:?} \
                     must be exported, got {record}"
                );
            }
        }

        // The manifest the export reports describes the very objects that were written.
        let (status, summary) = s3(&router, "GET", &format!("/{BUCKET}/{manifest_key}"), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the manifest object must be readable, got {status} with {summary:?}"
        );
        let summary: Value = serde_json::from_str(&summary)
            .unwrap_or_else(|error| panic!("case {case:?}: the manifest is not JSON: {error}"));
        assert_eq!(
            summary["domainName"], json!(row.domain),
            "case {case:?}: the manifest names the domain it is an export of, got {summary}"
        );
        assert_eq!(
            summary["itemCount"],
            json!(row.items.len()),
            "case {case:?}: the manifest counts the exported items, got {summary}"
        );
        let named: Vec<String> = summary["dataFileS3Keys"]
            .as_array()
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| key.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for key in &named {
            assert!(
                keys.contains(key),
                "case {case:?}: every data file the manifest names must be an object of the \
                 bucket, got {keys:?} for {summary}"
            );
        }
    }
}
