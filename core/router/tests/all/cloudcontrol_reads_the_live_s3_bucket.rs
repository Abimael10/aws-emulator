// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API over a bucket that Amazon S3's OWN API created.
//!
//! RULE: Cloud Control API is a uniform read surface over the LIVE resource rather than an
//! inventory of its own, so `GetResource` resolves a resource of a registry type by the
//! primary identifier the OWNING service holds it under — "you can use this action to return
//! information about an existing resource in your account and Amazon Web Services Region,
//! whether those resources were provisioned using Cloud Control API" (`GetResource`) — and
//! once the owning service no longer holds that resource the same call is refused with the
//! modelled `ResourceNotFoundException` carrying the owning handler's verdict, whichever API
//! removed it.
//!
//! Two crates have to agree for that to hold — Amazon S3 holds, configures and removes the
//! bucket, Cloud Control API reads it — so the suite lives here rather than in either of
//! them.
//!
//! The rows differ along what the rule generalises over: the FORM of the bucket's name (the
//! shortest and the longest the published naming rules admit, a name carrying periods, a name
//! carrying digits and hyphens), WHICH properties the owning service then carries for it (none
//! beyond the identity, a tag set, a versioning state, both), and the FORM the request
//! addresses it in (the primary identifier as a string, and as the JSON object the model also
//! admits). Nothing here is provisioned through Cloud Control API: every bucket under test is
//! one that service never minted.
//!
//! Catalog source for the names: the *General purpose bucket naming rules* of the Amazon S3
//! User Guide — 3 to 63 characters of lower-case letters, digits, periods and hyphens,
//! beginning and ending with a letter or a digit.
//!
//! Overlap, stated rather than hidden: the cloudcontrol crate's own `s3_bucket_provisioning`
//! suite already holds this rule for ONE row (a bucket `s3:CreateBucket` made, read through
//! Cloud Control API, then removed with `s3:DeleteBucket`), and `native_holds` below asks the
//! owning service the same way that suite's helper of the same name does. What is new here is
//! the breadth — the name forms, the JSON identifier form, the natively applied tag sets and
//! versioning states, the paged inventory and the create/delete/create cycle — which cannot be
//! added as rows there while that crate's hand-written `tests/all.rs` is a file the merge gate
//! refuses to edit.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The Region every call here is signed for and addressed in, and the ONE spelling of a Cloud
// Control API call in this test target: a sibling suite that drives this service declares both
// `pub(crate)`, so neither is written twice. `us-east-1`, so a bucket create carries no
// location constraint of its own: where a bucket is placed is not what any row is about.
use crate::cloudformation_apigatewayv2_api_schema_properties::{cloud_control, REGION};

/// The XML namespace a client's Amazon S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The registry type under test, whose resources live in the Amazon S3 service's own store.
const BUCKET_TYPE: &str = "AWS::S3::Bucket";

/// The property that carries this type's primary identifier, which its registry schema's
/// `primaryIdentifier` points at (`/properties/BucketName`).
const IDENTIFIER_PROPERTY: &str = "BucketName";

/// The modelled error a resource the owning service does not hold is refused with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// Amazon S3's own diagnostic for a bucket it holds none of, which the owning handler's
/// verdict carries. Written out rather than read off a constant: it is the sentence a client
/// sees, so a literal is what catches a change in it.
const BUCKET_NOT_FOUND: &str = "Bucket not found";

/// Amazon S3 and Cloud Control API over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them — so the buckets the S3 front door creates are the ones Cloud Control API reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudcontrol_cloudapiservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes the
/// request and what a service reads the caller's Region from. The signature is never checked:
/// this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket and any
/// subresource live in the URI and the body is the payload.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "application/xml")
        .body(Bytes::copy_from_slice(body.as_bytes()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// A string member, or `""` when absent — so a missing member fails an assertion rather than
/// panicking the suite.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The error SHAPE an error body names: the `__type` of the awsJson1_0 envelope with the
/// namespace that may precede it and the URI that may follow it taken off, so a differently
/// namespaced or suffixed code is told from the modelled shape rather than matched by it.
fn error_shape(body: &Value) -> String {
    let rendered = text(body, "__type");
    let after_namespace = rendered
        .rsplit_once('#')
        .map_or(rendered.as_str(), |(_, shape)| shape);
    after_namespace
        .split_once(':')
        .map_or(after_namespace, |(shape, _)| shape)
        .to_string()
}

/// The `message` an error body carries, under either casing the envelope may use.
fn message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// `s3:CreateBucket` for `name`, required to succeed.
async fn create_bucket(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = s3(router, "PUT", &format!("/{name}"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: s3:CreateBucket {name:?} must answer 200, got {status} with body \
         {body:?}"
    );
}

/// `s3:DeleteBucket` for `name`, required to succeed — the owning service answers 204.
async fn delete_bucket(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = s3(router, "DELETE", &format!("/{name}"), "").await;
    assert_eq!(
        status, 204,
        "case {case:?}: s3:DeleteBucket {name:?} must answer 204, got {status} with body \
         {body:?}"
    );
}

/// Whether the owning service holds a bucket called `name`, as its own `s3:HeadBucket`
/// answers.
async fn native_holds(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let (status, body) = s3(router, "HEAD", &format!("/{name}"), "").await;
    assert!(
        status == 200 || status == 404,
        "case {case:?}: s3:HeadBucket {name:?} must answer 200 or 404, got {status} with body \
         {body:?}"
    );
    status == 200
}

/// `s3:PutBucketTagging` for `name`, required to succeed.
async fn put_bucket_tagging(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    tags: &[(&str, &str)],
) {
    let mut payload = format!("<Tagging xmlns=\"{S3_XMLNS}\"><TagSet>");
    for (key, value) in tags {
        payload.push_str(&format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"));
    }
    payload.push_str("</TagSet></Tagging>");
    let (status, body) = s3(router, "PUT", &format!("/{name}?tagging"), &payload).await;
    assert_eq!(
        status, 204,
        "case {case:?}: s3:PutBucketTagging {name:?} must answer 204, got {status} with body \
         {body:?}"
    );
}

/// `s3:PutBucketVersioning` for `name`, required to succeed.
async fn put_bucket_versioning(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    status_value: &str,
) {
    let payload = format!(
        "<VersioningConfiguration xmlns=\"{S3_XMLNS}\"><Status>{status_value}</Status>\
         </VersioningConfiguration>"
    );
    let (status, body) = s3(router, "PUT", &format!("/{name}?versioning"), &payload).await;
    assert_eq!(
        status, 200,
        "case {case:?}: s3:PutBucketVersioning {name:?} must answer 200, got {status} with \
         body {body:?}"
    );
}

/// `GetResource` for one bucket, as a `(status, body)` pair.
async fn get_resource(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    cloud_control(
        router,
        "GetResource",
        json!({ "TypeName": BUCKET_TYPE, "Identifier": identifier }),
    )
    .await
}

/// Every identifier `ListResources` reports for this type, walking the pages at `per_page` per
/// page in the order the pages reported them.
async fn listed_identifiers(router: &MultiServiceRouter, per_page: i64) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = Map::new();
        request.insert("TypeName".to_string(), json!(BUCKET_TYPE));
        request.insert("MaxResults".to_string(), json!(per_page));
        if let Some(next) = &token {
            request.insert("NextToken".to_string(), json!(next));
        }
        let (status, body) = cloud_control(router, "ListResources", Value::Object(request)).await;
        assert_eq!(
            status, 200,
            "ListResources must answer 200, got {status} with body {body:?}"
        );
        identifiers.extend(
            body["ResourceDescriptions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|description| text(description, "Identifier")),
        );
        match body["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return identifiers,
        }
        assert!(
            identifiers.len() <= 1_000,
            "ListResources: the pages never ran out, got {identifiers:?}"
        );
    }
}

/// The resource model a `ResourceDescription` carries, parsed back from the JSON string its
/// `Properties` member holds.
fn properties_of(description: &Value) -> Value {
    serde_json::from_str(&text(description, "Properties")).unwrap_or(Value::Null)
}

/// The `PublicAccessBlockConfiguration` every bucket the owning service creates carries without
/// any client input: "Amazon S3 automatically enables S3 Block Public Access … for new
/// buckets", so all four settings are on and every read of the model reports them.
fn blocked_public_access() -> Value {
    json!({
        "BlockPublicAcls": true,
        "BlockPublicPolicy": true,
        "IgnorePublicAcls": true,
        "RestrictPublicBuckets": true,
    })
}

/// The `BucketEncryption` every bucket carries without any client input: "by default, all
/// buckets have a default encryption configuration that uses server-side encryption with
/// Amazon S3 managed keys (SSE-S3)", and "by default, S3 Bucket Key is not enabled".
fn default_encryption() -> Value {
    json!({
        "ServerSideEncryptionConfiguration": [{
            "ServerSideEncryptionByDefault": { "SSEAlgorithm": "AES256" },
            "BucketKeyEnabled": false,
        }],
    })
}

/// One case: a bucket the owning service's own API creates and configures, and the form the
/// Cloud Control request addresses it in.
struct Row {
    case: &'static str,
    name: String,
    /// The tag set `s3:PutBucketTagging` then puts on it, if any.
    tags: &'static [(&'static str, &'static str)],
    /// The versioning state `s3:PutBucketVersioning` then puts on it, if any.
    versioning: Option<&'static str>,
    /// Whether the request addresses it by the primary identifier as JSON rather than as the
    /// bare string: "Primary identifiers can be specified as a string or JSON"
    /// (`GetResource$Identifier`).
    as_json: bool,
}

impl Row {
    /// The reference a request carries for this row's bucket.
    fn identifier(&self) -> String {
        if self.as_json {
            json!({ IDENTIFIER_PROPERTY: self.name }).to_string()
        } else {
            self.name.clone()
        }
    }

    /// The resource model a read must report: the schema's properties the OWNING record
    /// carries, and only those it has — the identity, the read-only ARN that service's own
    /// builder mints, the tag set and versioning state where it has them, and the Block Public
    /// Access and default encryption configurations it applies to every bucket.
    fn expected_model(&self) -> Value {
        let mut model = Map::new();
        model.insert(IDENTIFIER_PROPERTY.to_string(), json!(self.name));
        model.insert(
            "Arn".to_string(),
            json!(service_com_amazonaws_s3_amazons3::state::bucket_arn(
                &self.name
            )),
        );
        if !self.tags.is_empty() {
            model.insert(
                "Tags".to_string(),
                Value::Array(
                    self.tags
                        .iter()
                        .map(|(key, value)| json!({ "Key": key, "Value": value }))
                        .collect(),
                ),
            );
        }
        if let Some(status) = self.versioning {
            model.insert("VersioningConfiguration".to_string(), json!({ "Status": status }));
        }
        model.insert(
            "PublicAccessBlockConfiguration".to_string(),
            blocked_public_access(),
        );
        model.insert("BucketEncryption".to_string(), default_encryption());
        Value::Object(model)
    }
}

/// The rows: see this suite's header for what they differ along.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the shortest name the naming rules admit, with nothing beyond its identity",
            name: "abc".to_string(),
            tags: &[],
            versioning: None,
            as_json: false,
        },
        Row {
            case: "the longest name the naming rules admit",
            name: "n".repeat(63),
            tags: &[],
            versioning: None,
            as_json: false,
        },
        Row {
            case: "a name carrying the periods the naming rules admit, and a tag set",
            name: "native.bucket.one".to_string(),
            tags: &[("team", "web")],
            versioning: None,
            as_json: false,
        },
        Row {
            case: "a versioned bucket addressed by the primary identifier as JSON",
            name: "native-versioned-2".to_string(),
            tags: &[],
            versioning: Some("Enabled"),
            as_json: true,
        },
        Row {
            case: "a bucket carrying both a tag set and a suspended versioning state",
            name: "9native-both".to_string(),
            tags: &[("owner", "platform"), ("env", "test")],
            versioning: Some("Suspended"),
            as_json: false,
        },
    ]
}

#[tokio::test]
async fn a_bucket_the_owning_service_holds_is_the_one_cloud_control_reports() {
    let router = emulator();
    let rows = rows();

    // Created and configured over the OWNING service's own API alone: nothing of these
    // buckets was ever named to Cloud Control API.
    for row in &rows {
        create_bucket(&router, row.case, &row.name).await;
        if !row.tags.is_empty() {
            put_bucket_tagging(&router, row.case, &row.name, row.tags).await;
        }
        if let Some(status) = row.versioning {
            put_bucket_versioning(&router, row.case, &row.name, status).await;
        }
    }

    for row in &rows {
        let case = row.case;
        let (status, body) = get_resource(&router, &row.identifier()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetResource must resolve a bucket the owning service holds, got \
             {status} with body {body:?}"
        );
        assert_eq!(
            text(&body, "TypeName"),
            BUCKET_TYPE,
            "case {case:?}: expected the type the request named, got {body:?}"
        );
        let description = &body["ResourceDescription"];
        assert_eq!(
            text(description, "Identifier"),
            row.name,
            "case {case:?}: expected the bucket's own primary identifier, got {body:?}"
        );
        let reported = properties_of(description);
        assert_eq!(
            reported,
            row.expected_model(),
            "case {case:?}: expected the model the owning service holds, {:?}, got {reported:?}",
            row.expected_model()
        );
        // And the read-only `Arn` carries the PUBLISHED form of a general purpose bucket's ARN
        // beside being the owning service's own value: "arn:aws:s3:::<bucket>", whose Region and
        // account-id fields are empty because a bucket name is globally unique (Amazon S3 User
        // Guide, *Amazon S3 resources*).
        assert_eq!(
            reported["Arn"],
            json!(format!("arn:aws:s3:::{}", row.name)),
            "case {case:?}: expected the published form of a bucket ARN, got {reported:?}"
        );
    }

    // And the inventory of the type is that service's own: every bucket it holds, in the
    // identifier order its own reader reports them in, paged small enough that the walk really
    // follows a continuation token.
    let mut expected: Vec<String> = rows.iter().map(|row| row.name.clone()).collect();
    expected.sort();
    let listed = listed_identifiers(&router, 2).await;
    assert_eq!(
        listed, expected,
        "ListResources must report the buckets the owning service holds, expected \
         {expected:?}, got {listed:?}"
    );
}

#[tokio::test]
async fn a_bucket_the_owning_service_no_longer_holds_is_reported_as_not_found() {
    let router = emulator();
    let rows = rows();

    for row in &rows {
        create_bucket(&router, row.case, &row.name).await;
        if !row.tags.is_empty() {
            put_bucket_tagging(&router, row.case, &row.name, row.tags).await;
        }
        if let Some(status) = row.versioning {
            put_bucket_versioning(&router, row.case, &row.name, status).await;
        }
    }

    for row in &rows {
        let case = row.case;
        let identifier = row.identifier();
        // It resolves while the owning service holds it...
        let (status, body) = get_resource(&router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetResource must resolve the created bucket, got {status} with body \
             {body:?}"
        );

        // ...and stops resolving the moment that service's own API removes it.
        delete_bucket(&router, case, &row.name).await;
        assert!(
            !native_holds(&router, case, &row.name).await,
            "case {case:?}: the owning service must hold the deleted bucket no longer"
        );

        let (status, body) = get_resource(&router, &identifier).await;
        assert_eq!(
            status, 400,
            "case {case:?}: GetResource after the native delete: expected HTTP 400, got {status} \
             with body {body:?}"
        );
        assert_eq!(
            error_shape(&body),
            RESOURCE_NOT_FOUND,
            "case {case:?}: expected {RESOURCE_NOT_FOUND}, got {:?}",
            text(&body, "__type")
        );
        // A not-found is the OWNING handler's verdict, so the refusal carries that handler's
        // whole answer: its own diagnostic, the handler error code it failed with and the token
        // of the invocation that failed — which is minted per call, so the case pins the text on
        // both sides of it and requires a token between them.
        let stated = message(&body);
        let opening = format!(
            "{BUCKET_TYPE} Handler returned status FAILED: {BUCKET_NOT_FOUND} \
             (HandlerErrorCode: NotFound, RequestToken: "
        );
        assert!(
            stated.starts_with(&opening) && stated.ends_with(')') && stated.len() > opening.len() + 1,
            "case {case:?}: expected a message opening {opening:?} and naming the failing \
             invocation before \")\", got {stated:?}"
        );
    }

    let listed = listed_identifiers(&router, 2).await;
    assert!(
        listed.is_empty(),
        "every deleted bucket must be gone from the inventory, got {listed:?}"
    );
}

#[tokio::test]
async fn a_read_follows_the_owning_store_rather_than_a_record_of_its_own() {
    let router = emulator();
    let name = "relived-bucket";
    let forms = [
        ("the primary identifier as a string", name.to_string()),
        (
            "the primary identifier as JSON",
            json!({ IDENTIFIER_PROPERTY: name }).to_string(),
        ),
    ];

    // A bucket nothing ever created resolves to nothing, in either admitted form of its
    // reference.
    for (case, identifier) in &forms {
        let (status, body) = get_resource(&router, identifier).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a bucket nothing created must resolve to nothing, got {status} with \
             body {body:?}"
        );
        assert_eq!(
            error_shape(&body),
            RESOURCE_NOT_FOUND,
            "case {case:?}: expected {RESOURCE_NOT_FOUND}, got {:?}",
            text(&body, "__type")
        );
    }

    // Created, deleted and created AGAIN over the owning service's own API: each read answers
    // from that service's live state, so the second create resolves exactly as the first did
    // and the delete between them leaves no record that outlives it.
    for round in 0_u8..2 {
        create_bucket(&router, "a native create", name).await;
        for (form, identifier) in &forms {
            let case = format!("round {round} ({form})");
            let (status, body) = get_resource(&router, identifier).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetResource must resolve the created bucket, got {status} with \
                 body {body:?}"
            );
            let reported = properties_of(&body["ResourceDescription"]);
            assert_eq!(
                reported[IDENTIFIER_PROPERTY],
                json!(name),
                "case {case:?}: expected the bucket's own identity in the model, got {reported:?}"
            );
            assert_eq!(
                reported["Arn"],
                json!(service_com_amazonaws_s3_amazons3::state::bucket_arn(name)),
                "case {case:?}: expected the ARN the owning service builds, got {reported:?}"
            );
        }
        delete_bucket(&router, "a native delete", name).await;
        for (form, identifier) in &forms {
            let case = format!("round {round} after the delete ({form})");
            let (status, body) = get_resource(&router, identifier).await;
            assert_eq!(
                status, 400,
                "case {case:?}: the deleted bucket must resolve to nothing, got {status} with \
                 body {body:?}"
            );
            assert_eq!(
                error_shape(&body),
                RESOURCE_NOT_FOUND,
                "case {case:?}: expected {RESOURCE_NOT_FOUND}, got {:?}",
                text(&body, "__type")
            );
        }
    }
}
