// SPDX-License-Identifier: CC0-1.0

//! The tables an S3 Metadata configuration provisions really are Amazon S3 Tables resources,
//! so that service resolves and deletes them.
//!
//! The rule under test: **a resource one service provisions in another on the account's
//! behalf exists in the store of the service that OWNS it, so that service's own reads
//! resolve it by the identifiers published for it and its deletes take it away. The journal
//! and inventory tables of a V2 metadata configuration are created in the Amazon Web Services
//! managed table bucket, under the namespace and names `GetBucketMetadataConfiguration`
//! publishes — so S3 Tables answers `GetTable` and `ListTables` about them, `DeleteTable` on
//! the `(tableBucketARN, namespace, name)` triple answers HTTP 204 with an empty body and
//! takes the table away (a second delete answering the modelled `NotFoundException`, 404),
//! and `DeleteNamespace` answers 204 with an empty body once that namespace holds no table
//! while a namespace that still holds one is refused.**
//!
//! Two crates have to agree for that to hold — Amazon S3 provisions the tables and Amazon S3
//! Tables serves them — so the suite lives here rather than in either of them.
//!
//! The rows differ along what the rule generalises over: which tables the configuration asks
//! for, the Region the general purpose bucket is created in, the form of that bucket's name
//! (what the namespace holding its tables is derived from), whether the tables are deleted
//! while they are still being created or after they have come into service, and the order the
//! tables are deleted in.
//!
//! WHICH code the refusal of a delete of a namespace that still holds a table carries is the
//! non-empty-container rule's own, pinned by the suite that owns it in the S3 Tables crate
//! (`tests/all/non_empty_container_delete_class.rs`); what this suite asserts about that call
//! is that it is refused and that the table is still there afterwards.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `TableStatus` a metadata table reports once it has come into service.
const ACTIVE: &str = "ACTIVE";

/// How long a row waits for a table to come into service before it gives up: generous, since
/// what it waits for is a span the store records and a loaded machine may take much longer to
/// get there.
const SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// How often it looks again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The two containers of a configuration's reader this suite reads tables out of.
const JOURNAL: &str = "JournalTableConfigurationResult";
const INVENTORY: &str = "InventoryTableConfigurationResult";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_s3tables_s3tablebuckets::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The body as a JSON document, or `Null` for a body that is not one (an empty success).
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// One string member of that document, when it carries one.
    fn member(&self, name: &str) -> Option<String> {
        self.json()
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The error code of an error body: the `__type` the restJson1 envelope carries, or a
    /// `code` member for an envelope that spells it that way instead.
    fn error_code(&self) -> Option<String> {
        self.member("__type").or_else(|| self.member("code"))
    }
}

/// The inner XML of the first `<name>…</name>` container of `fragment`, or `None` when it
/// carries none — the one reader this suite slices an S3 response with, used both for a
/// container and for a leaf element, so a member repeated once per table is read out of its
/// own container.
fn inside<'a>(fragment: &'a str, name: &str) -> Option<&'a str> {
    let (_, rest) = fragment.split_once(&format!("<{name}>"))?;
    let (inner, _) = rest.split_once(&format!("</{name}>"))?;
    Some(inner)
}

/// Call one Amazon S3 operation over its restXml wire, addressed to `region`'s endpoint.
async fn s3(router: &MultiServiceRouter, region: &str, method: &str, uri: &str, body: &str) -> Answer {
    let host = format!("s3.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.as_str())
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3", region))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// Call one Amazon S3 Tables operation over its restJson1 wire, on the method and URI its own
/// `@http` binding declares. Every call this suite makes is a read or a delete, which take no
/// body.
async fn s3tables(router: &MultiServiceRouter, region: &str, method: &str, uri: &str) -> Answer {
    let host = format!("s3tables.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host.as_str())
        .header("content-type", "application/json")
        .header("authorization", authorization("s3tables", region))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// A value as a single path segment or query value: its `:` and `/` travel percent-encoded, or
/// the request addresses a different route.
fn label(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// `PUT /<bucket>` — `CreateBucket`, with the location constraint a Region other than
/// `us-east-1` needs.
async fn create_bucket(router: &MultiServiceRouter, region: &str, bucket: &str) -> Answer {
    let body = match region {
        "us-east-1" => String::new(),
        other => format!(
            "<CreateBucketConfiguration xmlns=\"{XMLNS}\">\
             <LocationConstraint>{other}</LocationConstraint>\
             </CreateBucketConfiguration>"
        ),
    };
    s3(router, region, "PUT", &format!("/{bucket}"), &body).await
}

/// The `MetadataConfiguration` container a row creates: the required journal table, and the
/// inventory table when the row asks for one.
fn payload(inventory: bool) -> String {
    let mut body = format!("<MetadataConfiguration xmlns=\"{XMLNS}\">");
    body.push_str(
        "<JournalTableConfiguration><RecordExpiration>\
         <Expiration>ENABLED</Expiration><Days>7</Days>\
         </RecordExpiration></JournalTableConfiguration>",
    );
    if inventory {
        body.push_str(
            "<InventoryTableConfiguration>\
             <ConfigurationState>ENABLED</ConfigurationState>\
             </InventoryTableConfiguration>",
        );
    }
    body.push_str("</MetadataConfiguration>");
    body
}

/// `POST /<bucket>?metadataConfiguration` — `CreateBucketMetadataConfiguration`.
async fn create_metadata_configuration(
    router: &MultiServiceRouter,
    region: &str,
    bucket: &str,
    inventory: bool,
) -> Answer {
    s3(
        router,
        region,
        "POST",
        &format!("/{bucket}?metadataConfiguration"),
        &payload(inventory),
    )
    .await
}

/// `GET /<bucket>?metadataConfiguration` — `GetBucketMetadataConfiguration`.
async fn get_metadata_configuration(
    router: &MultiServiceRouter,
    region: &str,
    bucket: &str,
) -> Answer {
    s3(
        router,
        region,
        "GET",
        &format!("/{bucket}?metadataConfiguration"),
        "",
    )
    .await
}

/// `DELETE /<bucket>?metadataConfiguration` — `DeleteBucketMetadataConfiguration`.
async fn delete_metadata_configuration(
    router: &MultiServiceRouter,
    region: &str,
    bucket: &str,
) -> Answer {
    s3(
        router,
        region,
        "DELETE",
        &format!("/{bucket}?metadataConfiguration"),
        "",
    )
    .await
}

/// `GetTable` addressed by the triple a client holding the configuration's own answer has: the
/// table bucket ARN, the namespace and the table's name, which travel as query members.
async fn get_table(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    namespace: &str,
    name: &str,
) -> Answer {
    let uri = format!(
        "/get-table?tableBucketARN={}&namespace={}&name={}",
        label(arn),
        label(namespace),
        label(name)
    );
    s3tables(router, region, "GET", &uri).await
}

/// `ListTables` for one namespace of the table bucket.
async fn list_tables(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    namespace: &str,
) -> Answer {
    let uri = format!("/tables/{}?namespace={}", label(arn), label(namespace));
    s3tables(router, region, "GET", &uri).await
}

/// `DeleteTable` on that same triple.
async fn delete_table(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    namespace: &str,
    name: &str,
) -> Answer {
    let uri = format!(
        "/tables/{}/{}/{}",
        label(arn),
        label(namespace),
        label(name)
    );
    s3tables(router, region, "DELETE", &uri).await
}

/// `GetNamespace` on the namespace a configuration's tables live in.
async fn get_namespace(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    namespace: &str,
) -> Answer {
    let uri = format!("/namespaces/{}/{}", label(arn), label(namespace));
    s3tables(router, region, "GET", &uri).await
}

/// `DeleteNamespace` on it.
async fn delete_namespace(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
    namespace: &str,
) -> Answer {
    let uri = format!("/namespaces/{}/{}", label(arn), label(namespace));
    s3tables(router, region, "DELETE", &uri).await
}

/// `GetTableBucket` on the managed table bucket's own ARN.
async fn get_table_bucket(router: &MultiServiceRouter, region: &str, arn: &str) -> Answer {
    let uri = format!("/buckets/{}", label(arn));
    s3tables(router, region, "GET", &uri).await
}

/// Where a configuration's tables are, as the configuration's own reader publishes it.
#[derive(Debug)]
struct Published {
    /// The managed table bucket's ARN.
    table_bucket_arn: String,
    /// The namespace inside it that holds this bucket's tables.
    namespace: String,
    /// One `(container, table name, table ARN)` per table the configuration has.
    tables: Vec<(String, String, String)>,
}

/// Read that out of a `GetBucketMetadataConfiguration` body.
fn read_published(body: &str, containers: &[&str]) -> Published {
    let destination = inside(body, "DestinationResult")
        .unwrap_or_else(|| panic!("the configuration reports a DestinationResult: {body:?}"));
    let table_bucket_arn = inside(destination, "TableBucketArn")
        .unwrap_or_else(|| panic!("the destination reports a TableBucketArn: {body:?}"))
        .to_string();
    let namespace = inside(destination, "TableNamespace")
        .unwrap_or_else(|| panic!("the destination reports a TableNamespace: {body:?}"))
        .to_string();
    let mut tables = Vec::new();
    for container in containers {
        let fragment = inside(body, container)
            .unwrap_or_else(|| panic!("the configuration reports a {container}: {body:?}"));
        let name = inside(fragment, "TableName")
            .unwrap_or_else(|| panic!("{container} reports a TableName: {body:?}"))
            .to_string();
        let arn = inside(fragment, "TableArn")
            .unwrap_or_else(|| panic!("{container} reports a TableArn: {body:?}"))
            .to_string();
        tables.push(((*container).to_string(), name, arn));
    }
    Published {
        table_bucket_arn,
        namespace,
        tables,
    }
}

/// The `TableStatus` the container `container` of `body` reports, or `None` when the body
/// carries no such container.
fn status_of(body: &str, container: &str) -> Option<String> {
    inside(body, container).and_then(|fragment| inside(fragment, "TableStatus").map(str::to_string))
}

/// Look again until every one of `containers` reports [`ACTIVE`], or the budget runs out — the
/// CONDITION a row waits for, never a fixed pause.
async fn wait_until_in_service(
    router: &MultiServiceRouter,
    region: &str,
    bucket: &str,
    containers: &[&str],
    case: &str,
) {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let answered = get_metadata_configuration(router, region, bucket).await;
        let in_service = containers
            .iter()
            .all(|container| status_of(&answered.body, container).as_deref() == Some(ACTIVE));
        if in_service {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the configuration's tables never came into service: {:?}",
            answered.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The containers a configuration asking for `inventory` or not publishes a table in.
fn containers_of(inventory: bool) -> Vec<&'static str> {
    match inventory {
        false => vec![JOURNAL],
        true => vec![JOURNAL, INVENTORY],
    }
}

/// Every table name `ListTables` reports for one namespace, sorted.
fn listed_names(answer: &Answer) -> Vec<String> {
    let document = answer.json();
    let mut names: Vec<String> = document
        .get("tables")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|table| table.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// One row: a configuration to provision, and how its tables are then taken away.
#[derive(Debug)]
struct Row {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The Region the general purpose bucket is created in.
    region: &'static str,
    /// That bucket's name — what the namespace holding its tables is derived from.
    bucket: &'static str,
    /// Whether the configuration asks for an inventory table beside the journal one.
    inventory: bool,
    /// Whether the row waits for the tables to come into service before deleting them.
    in_service: bool,
    /// Whether the tables are deleted in the order they are published or in reverse.
    reversed: bool,
}

/// The rows, spanning the forms the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the journal table alone, deleted once it is in service",
            region: "us-east-1",
            bucket: "metadata-tables-journal",
            inventory: false,
            in_service: true,
            reversed: false,
        },
        Row {
            case: "a journal and an inventory table in another Region",
            region: "us-west-2",
            bucket: "metadata-tables-inventory",
            inventory: true,
            in_service: true,
            reversed: false,
        },
        Row {
            case: "tables deleted while they are still being created",
            region: "eu-west-1",
            bucket: "metadata-tables-creating",
            inventory: true,
            in_service: false,
            reversed: false,
        },
        Row {
            case: "a bucket name carrying dots, with the tables deleted in reverse",
            region: "us-east-1",
            bucket: "metadata.tables.2026",
            inventory: true,
            in_service: true,
            reversed: true,
        },
    ]
}

#[tokio::test]
async fn a_metadata_configurations_tables_are_resolved_and_deleted_by_s3_tables() {
    let router = build_router();

    for row in rows() {
        let Row {
            case,
            region,
            bucket,
            inventory,
            in_service,
            reversed,
        } = row;
        let containers = containers_of(inventory);

        let created = create_bucket(&router, region, bucket).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created, got {} with body {:?}",
            created.status, created.body
        );
        let configured = create_metadata_configuration(&router, region, bucket, inventory).await;
        assert_eq!(
            configured.status, 200,
            "case {case:?}: the metadata configuration must be created, got {} with body {:?}",
            configured.status, configured.body
        );
        if in_service {
            wait_until_in_service(&router, region, bucket, &containers, case).await;
        }

        // Where the configuration says its tables are — the only coordinates a client has.
        let answered = get_metadata_configuration(&router, region, bucket).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the configuration must read back, got {} with body {:?}",
            answered.status, answered.body
        );
        let published = read_published(&answered.body, &containers);
        assert_eq!(
            published.table_bucket_arn,
            format!("arn:aws:s3tables:{region}:{ACCOUNT}:bucket/aws-s3"),
            "case {case:?}: expected the managed table bucket's ARN in {region}, got {:?}",
            published.table_bucket_arn
        );

        // The managed table bucket is a table bucket of that service, and one Amazon Web
        // Services created on the account's behalf.
        let table_bucket = get_table_bucket(&router, region, &published.table_bucket_arn).await;
        assert_eq!(
            table_bucket.status, 200,
            "case {case:?}: S3 Tables must hold the managed table bucket, got {} with body {:?}",
            table_bucket.status, table_bucket.body
        );
        assert_eq!(
            table_bucket.member("type").as_deref(),
            Some("aws"),
            "case {case:?}: expected the managed table bucket's type `aws`, got {:?}",
            table_bucket.body
        );

        // The namespace holding this bucket's tables is a namespace of that same service.
        let namespace = get_namespace(
            &router,
            region,
            &published.table_bucket_arn,
            &published.namespace,
        )
        .await;
        assert_eq!(
            namespace.status, 200,
            "case {case:?}: S3 Tables must hold the namespace {:?}, got {} with body {:?}",
            published.namespace, namespace.status, namespace.body
        );

        // Each table reads back through the service that owns tables, by the triple the
        // configuration published, with the ARN that configuration published for it.
        for (container, name, arn) in &published.tables {
            let table = get_table(
                &router,
                region,
                &published.table_bucket_arn,
                &published.namespace,
                name,
            )
            .await;
            assert_eq!(
                table.status, 200,
                "case {case:?}: S3 Tables must resolve the {container} table {name:?}, got {} \
                 with body {:?}",
                table.status, table.body
            );
            assert_eq!(
                table.member("tableARN").as_deref(),
                Some(arn.as_str()),
                "case {case:?}: expected the {container} table's ARN {arn:?}, got {:?}",
                table.body
            );
            assert_eq!(
                table.member("type").as_deref(),
                Some("aws"),
                "case {case:?}: expected the {container} table's type `aws`, got {:?}",
                table.body
            );
            assert_eq!(
                table.member("managedByService").as_deref(),
                Some("s3.amazonaws.com"),
                "case {case:?}: expected the {container} table to be managed by Amazon S3, got \
                 {:?}",
                table.body
            );
        }

        // And the namespace lists exactly them.
        let listed = list_tables(
            &router,
            region,
            &published.table_bucket_arn,
            &published.namespace,
        )
        .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListTables must answer for the namespace, got {} with body {:?}",
            listed.status, listed.body
        );
        let mut expected: Vec<String> = published
            .tables
            .iter()
            .map(|(_, name, _)| name.clone())
            .collect();
        expected.sort();
        assert_eq!(
            listed_names(&listed),
            expected,
            "case {case:?}: expected the namespace to list {expected:?}, got {:?}",
            listed.body
        );

        // The deletes themselves: each answers the modelled 204 with an empty body, the table
        // is gone afterwards, and a second delete answers the modelled not-found.
        let mut order: Vec<(String, String)> = published
            .tables
            .iter()
            .map(|(container, name, _)| (container.clone(), name.clone()))
            .collect();
        if reversed {
            order.reverse();
        }
        let mut left = order.len();
        for (container, name) in &order {
            // While a table is still in it, the namespace is not deleted — which code that
            // refusal carries is the non-empty rule's own (see this suite's header).
            let refused = delete_namespace(
                &router,
                region,
                &published.table_bucket_arn,
                &published.namespace,
            )
            .await;
            assert!(
                (400..500).contains(&refused.status),
                "case {case:?}: a namespace still holding {left} table(s) must be refused, got \
                 {} with body {:?}",
                refused.status,
                refused.body
            );

            let deleted = delete_table(
                &router,
                region,
                &published.table_bucket_arn,
                &published.namespace,
                name,
            )
            .await;
            assert_eq!(
                deleted.status, 204,
                "case {case:?}: deleting the {container} table {name:?} must answer 204, got {} \
                 with body {:?}",
                deleted.status, deleted.body
            );
            assert!(
                deleted.body.is_empty(),
                "case {case:?}: a delete answers an empty body, got {:?}",
                deleted.body
            );

            let gone = get_table(
                &router,
                region,
                &published.table_bucket_arn,
                &published.namespace,
                name,
            )
            .await;
            assert_eq!(
                gone.status, 404,
                "case {case:?}: the deleted {container} table must be gone, got {} with body \
                 {:?}",
                gone.status, gone.body
            );
            let again = delete_table(
                &router,
                region,
                &published.table_bucket_arn,
                &published.namespace,
                name,
            )
            .await;
            assert_eq!(
                again.status, 404,
                "case {case:?}: a second delete of {name:?} must answer the modelled not-found, \
                 got {} with body {:?}",
                again.status, again.body
            );
            assert_eq!(
                again.error_code().as_deref(),
                Some("NotFoundException"),
                "case {case:?}: expected NotFoundException, got {:?}",
                again.body
            );
            left -= 1;
        }

        // The emptied namespace deletes too, and is gone afterwards.
        let emptied = delete_namespace(
            &router,
            region,
            &published.table_bucket_arn,
            &published.namespace,
        )
        .await;
        assert_eq!(
            emptied.status, 204,
            "case {case:?}: deleting the emptied namespace must answer 204, got {} with body \
             {:?}",
            emptied.status, emptied.body
        );
        assert!(
            emptied.body.is_empty(),
            "case {case:?}: the namespace delete answers an empty body, got {:?}",
            emptied.body
        );
        let absent = get_namespace(
            &router,
            region,
            &published.table_bucket_arn,
            &published.namespace,
        )
        .await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: the deleted namespace must be gone, got {} with body {:?}",
            absent.status, absent.body
        );
    }
}

#[tokio::test]
async fn a_configuration_created_after_the_leftover_table_was_deleted_provisions_afresh() {
    // The other half of the rule: the published failure for a table name already in the managed
    // table bucket's namespace asks the caller to "delete the journal table, and then try
    // again", so a configuration created after that deletion must provision under the name
    // again rather than report it taken. Each case provisions, deletes the configuration, and
    // differs in whether the leftover table is then deleted through S3 Tables — which is the
    // only thing that frees the name — and in which tables the configuration had.
    let router = build_router();
    let region = "us-east-1";
    let cases: [(&str, &str, bool, bool); 3] = [
        (
            "the journal table deleted through S3 Tables",
            "metadata-retry-journal",
            false,
            true,
        ),
        (
            "both tables deleted through S3 Tables",
            "metadata-retry-both",
            true,
            true,
        ),
        (
            "the leftover table left where it is",
            "metadata-retry-left",
            false,
            false,
        ),
    ];

    for (case, bucket, inventory, delete_the_tables) in cases {
        let containers = containers_of(inventory);
        let created = create_bucket(&router, region, bucket).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created, got {} with body {:?}",
            created.status, created.body
        );
        let configured = create_metadata_configuration(&router, region, bucket, inventory).await;
        assert_eq!(
            configured.status, 200,
            "case {case:?}: the metadata configuration must be created, got {} with body {:?}",
            configured.status, configured.body
        );
        // Waited for, so the tables really came into service: only then does the published
        // already-exists failure apply to a create that follows.
        wait_until_in_service(&router, region, bucket, &containers, case).await;
        let answered = get_metadata_configuration(&router, region, bucket).await;
        let published = read_published(&answered.body, &containers);
        let dropped = delete_metadata_configuration(&router, region, bucket).await;
        assert_eq!(
            dropped.status, 204,
            "case {case:?}: the configuration delete must succeed, got {} with body {:?}",
            dropped.status, dropped.body
        );

        if delete_the_tables {
            for (container, name, _) in &published.tables {
                let deleted = delete_table(
                    &router,
                    region,
                    &published.table_bucket_arn,
                    &published.namespace,
                    name,
                )
                .await;
                assert_eq!(
                    deleted.status, 204,
                    "case {case:?}: deleting the leftover {container} table {name:?} must answer \
                     204, got {} with body {:?}",
                    deleted.status, deleted.body
                );
            }
        }

        let again = create_metadata_configuration(&router, region, bucket, inventory).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the second create must be accepted, got {} with body {:?}",
            again.status, again.body
        );
        let reread = get_metadata_configuration(&router, region, bucket).await;
        let journal = status_of(&reread.body, JOURNAL);
        match delete_the_tables {
            // The name is free again, so the journal table is being created or already in
            // service — never FAILED.
            true => assert_ne!(
                journal.as_deref(),
                Some("FAILED"),
                "case {case:?}: the journal table must provision afresh, got {:?}",
                reread.body
            ),
            // Nothing freed the name, so the create reports it taken.
            false => assert_eq!(
                journal.as_deref(),
                Some("FAILED"),
                "case {case:?}: the journal table's name is still held, so it must report \
                 FAILED, got {:?}",
                reread.body
            ),
        }
    }
}
