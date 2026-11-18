// SPDX-License-Identifier: CC0-1.0

//! What an Amazon OpenSearch Service domain's SNAPSHOT API puts in — and takes out of — the
//! account's object store, read through Amazon S3's OWN wire.
//!
//! RULE: a domain's data-plane endpoint serves the cluster's snapshot API, so a repository
//! registration naming an S3 bucket is ACCEPTED (`200 {"acknowledged": true}`, never the
//! engine's no-handler refusal), a snapshot taken into that repository writes its blobs into
//! that bucket under the `base_path` the registration declared and nowhere else, and deleting
//! one snapshot removes exactly the blobs that snapshot wrote — leaving every blob of every
//! other repository of the same bucket byte for byte as it was, and leaving those snapshots
//! still readable and still restorable.
//!
//! Two crates have to agree for that to hold — Amazon OpenSearch Service takes the snapshot
//! and Amazon S3 holds what it wrote — so the suite lives here, and every object assertion is
//! made through `s3:ListObjectsV2` and `s3:GetObject` rather than against an in-process store:
//! what the snapshot wrote is only really in the bucket if an S3 client reads it there. The
//! Amazon OpenSearch Service crate's own suites pin the layout one repository writes and what
//! the API refuses, which needs no second service.
//!
//! The rows differ along what the rule generalises over: the forms a declared base path takes
//! (two siblings under one prefix, a repository at the bucket ROOT beside one under a prefix,
//! and a path written with leading and trailing separators whose TEXT is a prefix of the other
//! repository's), how many indexes one snapshot copies, and how many documents each holds.
//!
//! Every wait here is for a CONDITION — the domain answering on its data plane at all — looked
//! at again until it holds or a generous deadline passes, so nothing the rows assert can be
//! decided by how fast or how loaded this host happens to be.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::elements;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The domain every row takes its snapshots from.
const DOMAIN: &str = "xsvc-snapshot-objects";

/// The repository whose blobs must survive another repository's snapshot and its delete.
const KEPT_REPOSITORY: &str = "kept-repo";

/// The repository whose snapshot is deleted.
const DROPPED_REPOSITORY: &str = "dropped-repo";

/// The snapshot each repository holds.
const KEPT_SNAPSHOT: &str = "kept-1";
const DROPPED_SNAPSHOT: &str = "dropped-1";

/// How long this suite keeps looking for the domain's data plane to answer, and how long it
/// pauses between two looks. The deadline is far above the span the service itself owes that
/// transition, because what is waited for is the CONDITION and never an elapsed time.
const READY_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The emulator for this suite: Amazon OpenSearch Service and Amazon S3 on ONE engine, exactly
/// as the router mounts them, with request authentication off so each call carries a credential
/// scope (which names the Region and the service) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// Dispatch one request addressed to `signing_name`, the way a signed SDK call is.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Bytes,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = router
        .dispatch(request.body(body).expect("a well-formed request"))
        .await;
    (response.status().as_u16(), response.into_body().to_vec())
}

/// One call to this service's own control plane, answered with the status and the JSON body.
async fn control_plane(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, bytes) = send(
        router,
        "es",
        method,
        path,
        &[("content-type", "application/json".to_string())],
        Bytes::from(payload),
    )
    .await;
    (status, parsed(&bytes))
}

/// One REST call to the domain's data plane, under the path-style addressing a caller inside
/// this emulator reaches a domain by.
async fn data_plane(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    control_plane(router, method, &format!("/_aws/es/{DOMAIN}{path}"), body).await
}

/// The JSON an answer carries, or `Value::Null` for one that carries no body.
fn parsed(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).unwrap_or(Value::Null)
    }
}

/// The `error.type` a refused data-plane answer carries.
fn error_type(body: &Value) -> String {
    body["error"]["type"].as_str().unwrap_or_default().to_string()
}

/// `PUT /<bucket>` — S3's own `CreateBucket`. The bucket is in `us-east-1`, which carries no
/// location constraint.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let (status, body) = send(router, "s3", "PUT", &format!("/{bucket}"), &[], Bytes::new()).await;
    assert_eq!(
        status,
        200,
        "setup: CreateBucket({bucket:?}) must succeed, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
}

/// Every object the bucket holds, as the bucket's own `ListObjectsV2` and `GetObject` report
/// them: key -> the bytes that key holds.
async fn held_objects(router: &MultiServiceRouter, bucket: &str) -> BTreeMap<String, Vec<u8>> {
    let (status, listed) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}?list-type=2"),
        &[],
        Bytes::new(),
    )
    .await;
    let listed = String::from_utf8_lossy(&listed).to_string();
    assert_eq!(
        status, 200,
        "ListObjectsV2({bucket:?}): expected 200, got {status} with {listed}"
    );
    let mut held = BTreeMap::new();
    for key in elements(&listed, "Key") {
        let (status, body) = send(
            router,
            "s3",
            "GET",
            &format!("/{bucket}/{key}"),
            &[],
            Bytes::new(),
        )
        .await;
        assert_eq!(
            status, 200,
            "GetObject({bucket:?}, {key:?}): expected the listed object to be readable, got \
             {status} with {}",
            String::from_utf8_lossy(&body)
        );
        held.insert(key, body);
    }
    held
}

/// The domain every row works on, created through its own `CreateDomain` and looked at again
/// until its data plane answers.
async fn given_domain(router: &MultiServiceRouter) {
    let (status, body) = control_plane(
        router,
        "POST",
        "/2021-01-01/opensearch/domain",
        Some(json!({ "DomainName": DOMAIN })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateDomain({DOMAIN:?}) must succeed, got {status} with {body:?}"
    );
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let (status, body) = data_plane(router, "GET", "/_cluster/health", None).await;
        if status == 200 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the domain's data plane never answered its cluster health, last {status} \
             with {body:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// An index of the domain holding `documents`, written the way a client loads them.
async fn given_index(router: &MultiServiceRouter, index: &str, documents: &[(&str, Value)]) {
    let (status, body) = data_plane(
        router,
        "PUT",
        &format!("/{index}"),
        Some(json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": { "properties": { "title": { "type": "text" } } },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PUT /{index} must succeed, got {status} with {body:?}"
    );
    for (id, source) in documents {
        let (status, body) = data_plane(
            router,
            "PUT",
            &format!("/{index}/_doc/{id}"),
            Some(source.clone()),
        )
        .await;
        assert!(
            status == 200 || status == 201,
            "setup: PUT /{index}/_doc/{id} must be accepted, got {status} with {body:?}"
        );
    }
}

/// The `s3` repository definition a row registers: the bucket, the base path it declares (one
/// that declares none omits the setting entirely) and the role the domain copies with.
fn s3_definition(bucket: &str, base_path: Option<&str>) -> Value {
    let mut settings = json!({
        "bucket": bucket,
        "role_arn": format!(
            "arn:aws:iam::{}:role/opensearch-snapshot",
            ::engine::emulator_account_id()
        ),
    });
    if let Some(base_path) = base_path {
        settings["base_path"] = json!(base_path);
    }
    json!({ "type": "s3", "settings": settings })
}

/// `PUT /_snapshot/{repository}`, asserting the registration is the one AWS answers — and not
/// the no-handler refusal a cluster with no snapshot API answers.
async fn registered(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    definition: Value,
) {
    let (status, body) = data_plane(
        router,
        "PUT",
        &format!("/_snapshot/{repository}"),
        Some(definition),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected registering {repository:?} to answer 200, got {status} with \
         {body:?}"
    );
    assert_eq!(
        body["acknowledged"].as_bool(),
        Some(true),
        "case {case:?}: expected the registration of {repository:?} to be acknowledged, got \
         {body:?}"
    );
    assert!(
        body.get("error").is_none(),
        "case {case:?}: expected the registration of {repository:?} to carry no error, got \
         {body:?}"
    );
}

/// `PUT /_snapshot/{repository}/{snapshot}?wait_for_completion=true` over `indexes`, answering
/// the uuid the completed snapshot's blobs are named by.
async fn taken(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    snapshot: &str,
    indexes: &str,
) -> String {
    let (status, body) = data_plane(
        router,
        "PUT",
        &format!("/_snapshot/{repository}/{snapshot}?wait_for_completion=true"),
        Some(json!({ "indices": indexes })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the snapshot {snapshot:?} to answer 200, got {status} with \
         {body:?}"
    );
    assert_eq!(
        body["snapshot"]["state"].as_str(),
        Some("SUCCESS"),
        "case {case:?}: expected {snapshot:?} to have been copied, got {body:?}"
    );
    let uuid = body["snapshot"]["uuid"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !uuid.is_empty(),
        "case {case:?}: expected {snapshot:?} to report the uuid its blobs are named by, got \
         {body:?}"
    );
    uuid
}

/// One row: the two repositories of one bucket, the prefixes their blobs must live under, and
/// what the snapshots copy.
struct Row {
    case: &'static str,
    /// The base path the surviving repository declares, and the key prefix that earns.
    kept_base_path: Option<&'static str>,
    kept_prefix: &'static str,
    /// The base path the repository whose snapshot is deleted declares, and its prefix.
    dropped_base_path: Option<&'static str>,
    dropped_prefix: &'static str,
    /// The indexes the row loads, each with the documents it holds.
    indexes: Vec<(&'static str, Vec<(&'static str, Value)>)>,
}

/// The rows: the base-path forms two repositories of one bucket take, and what one snapshot
/// copies.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "two sibling base paths under one prefix, one index of one document",
            kept_base_path: Some("backups/kept"),
            kept_prefix: "backups/kept/",
            dropped_base_path: Some("backups/dropped"),
            dropped_prefix: "backups/dropped/",
            indexes: vec![(
                "orders",
                vec![("1", json!({ "title": "first order", "total": 10 }))],
            )],
        },
        Row {
            case: "a repository at the bucket root beside one under a prefix, two indexes",
            kept_base_path: None,
            kept_prefix: "",
            dropped_base_path: Some("weekly"),
            dropped_prefix: "weekly/",
            indexes: vec![
                ("orders", vec![("1", json!({ "title": "an order" }))]),
                ("customers", vec![("c1", json!({ "title": "a customer" }))]),
            ],
        },
        Row {
            case: "a base path written with leading and trailing separators, whose text is a \
                   prefix of the other repository's, three documents",
            kept_base_path: Some("/wrapped/kept/"),
            kept_prefix: "wrapped/kept/",
            dropped_base_path: Some("wrapped/kept-extra"),
            dropped_prefix: "wrapped/kept-extra/",
            indexes: vec![(
                "orders",
                vec![
                    ("1", json!({ "title": "first order" })),
                    ("2", json!({ "title": "second order" })),
                    ("3", json!({ "title": "third order", "total": 3 })),
                ],
            )],
        },
    ]
}

/// RULE: a domain's snapshot API is served on its data plane, a snapshot's blobs live in the
/// bucket its repository names under the base path that repository declared, and deleting one
/// snapshot removes exactly the blobs it wrote — every blob of another repository of the same
/// bucket is left byte for byte as it was, and its snapshot is still readable and restorable.
#[tokio::test]
async fn a_deleted_snapshot_takes_only_its_own_objects_out_of_the_bucket() {
    let router = emulator();
    given_domain(&router).await;

    for (ordinal, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let bucket = format!("xsvc-snapshot-objects-{ordinal}");
        create_bucket(&router, &bucket).await;

        // Each row loads its own indexes, so one domain serves every row and no row reads
        // another's documents.
        let indexes: Vec<(String, Vec<(&str, Value)>)> = row
            .indexes
            .iter()
            .map(|(name, documents)| (format!("{name}-{ordinal}"), documents.clone()))
            .collect();
        for (index, documents) in &indexes {
            given_index(&router, index, documents).await;
        }
        // Each snapshot copies the row's OWN indexes, so what one row snapshots and restores
        // is never another row's index.
        let copied = indexes
            .iter()
            .map(|(index, _)| index.as_str())
            .collect::<Vec<_>>()
            .join(",");

        // The repository whose blobs must survive, and its snapshot: every object the bucket
        // now holds is the repository's own, under the base path it declared.
        registered(
            &router,
            case,
            KEPT_REPOSITORY,
            s3_definition(&bucket, row.kept_base_path),
        )
        .await;
        let kept_uuid = taken(&router, case, KEPT_REPOSITORY, KEPT_SNAPSHOT, &copied).await;
        let kept_objects = held_objects(&router, &bucket).await;
        assert!(
            !kept_objects.is_empty(),
            "case {case:?}: expected the snapshot to have written objects into {bucket:?}, got \
             none"
        );
        for key in kept_objects.keys() {
            assert!(
                key.starts_with(row.kept_prefix),
                "case {case:?}: expected every object of {KEPT_REPOSITORY:?} under {:?}, got \
                 {key:?}",
                row.kept_prefix
            );
        }

        // A second repository of the SAME bucket, under its own base path: what its snapshot
        // writes lives under that path, and the first repository's objects are untouched.
        registered(
            &router,
            case,
            DROPPED_REPOSITORY,
            s3_definition(&bucket, row.dropped_base_path),
        )
        .await;
        let dropped_uuid =
            taken(&router, case, DROPPED_REPOSITORY, DROPPED_SNAPSHOT, &copied).await;
        assert_ne!(
            dropped_uuid, kept_uuid,
            "case {case:?}: expected each snapshot its own uuid, got {dropped_uuid:?} twice"
        );
        let after_both = held_objects(&router, &bucket).await;
        let dropped_objects: BTreeMap<&String, &Vec<u8>> = after_both
            .iter()
            .filter(|(key, _)| !kept_objects.contains_key(*key))
            .collect();
        assert!(
            !dropped_objects.is_empty(),
            "case {case:?}: expected the second repository's snapshot to have written objects \
             of its own, got only {:?}",
            after_both.keys().collect::<Vec<_>>()
        );
        for key in dropped_objects.keys() {
            assert!(
                key.starts_with(row.dropped_prefix),
                "case {case:?}: expected every object of {DROPPED_REPOSITORY:?} under {:?}, got \
                 {key:?}",
                row.dropped_prefix
            );
        }
        for (key, body) in &kept_objects {
            assert_eq!(
                after_both.get(key),
                Some(body),
                "case {case:?}: expected the second repository's snapshot to leave {key:?} as \
                 it was, got {:?}",
                after_both.get(key).map(Vec::len)
            );
        }

        // The DELETE removes exactly the blobs the deleted snapshot wrote.
        let (status, body) = data_plane(
            &router,
            "DELETE",
            &format!("/_snapshot/{DROPPED_REPOSITORY}/{DROPPED_SNAPSHOT}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to answer 200, got {status} with {body:?}"
        );
        assert_eq!(
            body["acknowledged"].as_bool(),
            Some(true),
            "case {case:?}: expected the delete to be acknowledged, got {body:?}"
        );
        let left = held_objects(&router, &bucket).await;
        for key in left.keys() {
            assert!(
                !key.contains(&dropped_uuid),
                "case {case:?}: expected no blob of the deleted snapshot to be left, got {key:?}"
            );
        }
        for (key, body) in &kept_objects {
            assert_eq!(
                left.get(key),
                Some(body),
                "case {case:?}: expected {key:?} to survive the other repository's delete byte \
                 for byte, got {:?}",
                left.get(key).map(Vec::len)
            );
        }

        // The deleted snapshot is gone from its own repository; the other repository still
        // reports its own.
        let (status, body) = data_plane(
            &router,
            "GET",
            &format!("/_snapshot/{DROPPED_REPOSITORY}/{DROPPED_SNAPSHOT}"),
            None,
        )
        .await;
        assert_eq!(
            (status, error_type(&body).as_str()),
            (404, "snapshot_missing_exception"),
            "case {case:?}: expected the deleted snapshot to be missing, got {status} with \
             {body:?}"
        );
        let (status, body) = data_plane(
            &router,
            "GET",
            &format!("/_snapshot/{KEPT_REPOSITORY}/{KEPT_SNAPSHOT}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the surviving snapshot to be readable, got {status} with \
             {body:?}"
        );
        assert_eq!(
            body["snapshots"][0]["state"].as_str(),
            Some("SUCCESS"),
            "case {case:?}: expected the surviving snapshot to still report its copy, got \
             {body:?}"
        );

        // And it still RESTORES: the blobs the delete left are the documents the snapshot
        // copied, read back under a renamed target.
        let (status, body) = data_plane(
            &router,
            "POST",
            &format!("/_snapshot/{KEPT_REPOSITORY}/{KEPT_SNAPSHOT}/_restore"),
            Some(json!({
                "rename_pattern": "(.+)",
                "rename_replacement": format!("restored-{ordinal}-$1"),
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the restore to answer 200, got {status} with {body:?}"
        );
        for (index, documents) in &indexes {
            let restored = format!("restored-{ordinal}-{index}");
            let (status, counted) =
                data_plane(&router, "GET", &format!("/{restored}/_count"), None).await;
            assert_eq!(
                (status, counted["count"].as_i64()),
                (200, Some(documents.len() as i64)),
                "case {case:?}: expected the restored {restored:?} to hold every copied \
                 document, got {status} with {counted:?}"
            );
        }
    }
}
