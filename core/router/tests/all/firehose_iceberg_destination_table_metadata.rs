// SPDX-License-Identifier: CC0-1.0

//! The FIRST Iceberg metadata file of the table a Firehose Iceberg destination delivers into:
//! the object a client building an Iceberg lake on Firehose opens before any record has
//! flowed, and the catalog pointer that names it.
//!
//! RULE: the initial Iceberg metadata document of an open-table-format table is materialised
//! by the CATALOG CREATE that marks the table as Iceberg — a `CreateTable` carrying an
//! `OpenTableFormatInput$IcebergInput` writes it under the table's own location, at
//! `<location>/metadata/00000-<uuid>.metadata.json`, records that key in the row's
//! `metadata_location` parameter, states the `format-version` the input asked for (2 when it
//! asked for none: "The table version for the Iceberg table. Defaults to 2",
//! `IcebergInput$Version`) and carries one schema whose fields are the table's Glue columns
//! written as their Iceberg types. A table marked by the `table_type` parameter ALONE is
//! labelled and nothing more: no metadata location is minted for it and no object is written.
//! Creating a Firehose stream whose `IcebergDestinationConfiguration` names those tables
//! delivers nothing by itself, so it neither mints a metadata file nor moves a pointer: the
//! document the catalog create wrote is exactly what the destination's first reader resolves.
//!
//! Only a cross-service suite can pin this on the path the client drives: the flow spans four
//! services mounted on ONE engine the way `core/router/src/lib.rs` mounts them — Glue for the
//! Data Catalog the destination tables live in, Amazon S3 for the metadata object the row
//! points at, IAM for the delivery role the destination names, and Firehose for the stream
//! created over the tables.
//!
//! The sibling suite `firehose_iceberg_destination_commits_its_kinesis_source` pins what a
//! COMMIT of delivered records writes; this one pins the state the table is in before any
//! record flows, which is what the destination's `metadata_location` resolves to on a fresh
//! lake.
//!
//! The rows differ along what the rule generalises over: the format version the input asks for
//! (none, 1, 2), the Glue types the columns declare, the form of the table's location (a
//! trailing slash or none, one segment or several), and the route the create takes (the Iceberg
//! input, or the `table_type` parameter alone).
//!
//! Nothing here asserts a value this emulator invented: the pointer is read back through Glue's
//! own `GetTable`, the object through Amazon S3's own `GetObject` and `ListObjectsV2`, and the
//! stream's state through Firehose's own `DescribeDeliveryStream`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// The Region every call below is signed for, and the one the bucket is created in.
const REGION: &str = "us-west-2";

/// How long a stream is waited on before a case fails. Generous rather than tight: a stream is
/// usable only a clock-owed span after its create, so this is the point at which the stream is
/// declared never to have settled and not a measurement of how fast the host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a status, a listing or a catalog row is looked at again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long the tables are watched, by looking again every [`POLL_INTERVAL`], once every
/// stream is demonstrably `ACTIVE`: a stream that minted or moved anything of its own fails the
/// case whenever in the window it did so.
const WATCH: Duration = Duration::from_millis(1_500);

/// The bucket every destination table is located in, and where the error output goes.
const BUCKET: &str = "firehose-iceberg-table-metadata";

/// The Glue database every destination table lives in.
const DATABASE: &str = "firehose_iceberg_table_metadata_db";

/// The table parameter an Iceberg table's current metadata file is named by, the parameter
/// marking a row as an open-table-format table and the value an Iceberg table carries in it,
/// and the parameter carrying the table's format version. Read back here through `GetTable`.
const METADATA_LOCATION: &str = "metadata_location";
const TABLE_TYPE: &str = "table_type";
const ICEBERG: &str = "ICEBERG";
const FORMAT_VERSION: &str = "format-version";

/// The trust policy the delivery role carries: Firehose has to be able to assume the role a
/// destination names.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// What the delivery role is allowed: the catalog reads and the table update an Iceberg commit
/// makes, and the object write those commits are.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["glue:GetTable","glue:UpdateTable","s3:PutObject"],"Resource":"*"}]}"#
);

/// One destination-table case.
struct Row {
    case: &'static str,
    /// The destination table's name, which also names its delivery stream and its own location
    /// inside [`BUCKET`].
    table: &'static str,
    /// `IcebergInput$Version`, or `None` to leave the member out — which is the route the
    /// documented default decides.
    version: Option<&'static str>,
    /// The `format-version` the minted document and the stored row must state, or `None` for a
    /// route that mints no metadata at all.
    format_version: Option<u64>,
    /// Whether the create carries an `OpenTableFormatInput$IcebergInput`; a row that does not
    /// carries the `table_type` parameter alone.
    iceberg_input: bool,
    /// `(column name, the declared Glue type, the Iceberg type the document must state)`.
    ///
    /// The declared names are Hive's own data type names, which is what a Glue column's `Type`
    /// carries (`Column$Type`); the expected names are Iceberg's primitive types, which share
    /// most spellings with Hive's and differ on the ones this table exercises.
    columns: &'static [(&'static str, &'static str, &'static str)],
    /// The table's location inside [`BUCKET`], exactly as the create submits it.
    location_suffix: &'static str,
}

impl Row {
    /// `StorageDescriptor$Location`: the table's own location, which its metadata file is
    /// written under.
    fn location(&self) -> String {
        format!("s3://{BUCKET}/{}", self.location_suffix)
    }

    /// The key prefix of that location inside [`BUCKET`], with the one trailing slash a prefix
    /// listing needs whichever way the location was submitted.
    fn prefix(&self) -> String {
        format!("{}/", self.location_suffix.trim_end_matches('/'))
    }

    /// The Firehose stream whose Iceberg destination names this table.
    fn stream(&self) -> String {
        format!("{}-stream", self.table)
    }

    /// Where a record this destination could not place would be written.
    fn error_prefix(&self) -> String {
        format!("errors/{}/", self.table)
    }
}

/// The create cases.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no version named, so the documented default decides",
            table: "default_version",
            version: None,
            format_version: Some(2),
            iceberg_input: true,
            columns: &[
                ("id", "string", "string"),
                ("payload", "string", "string"),
                ("observed_at", "timestamp", "timestamp"),
            ],
            location_suffix: "iceberg/default_version/",
        },
        Row {
            case: "version 2 named explicitly, at a nested location with no trailing slash",
            table: "explicit_v2",
            version: Some("2"),
            format_version: Some(2),
            iceberg_input: true,
            columns: &[("count", "bigint", "long"), ("ratio", "double", "double")],
            location_suffix: "iceberg/nested/explicit_v2",
        },
        Row {
            case: "version 1",
            table: "explicit_v1",
            version: Some("1"),
            format_version: Some(1),
            iceberg_input: true,
            columns: &[("flag", "boolean", "boolean"), ("label", "char(8)", "string")],
            location_suffix: "iceberg/explicit_v1/",
        },
        Row {
            // The Hive spellings an Iceberg schema does NOT share, beside a parameterised one
            // whose parameter list both vocabularies keep.
            case: "Hive type names the Iceberg vocabulary spells differently",
            table: "typed_columns",
            version: None,
            format_version: Some(2),
            iceberg_input: true,
            columns: &[
                ("c_tinyint", "tinyint", "int"),
                ("c_integer", "integer", "int"),
                ("c_varchar", "varchar(32)", "string"),
                ("c_decimal", "decimal(10,2)", "decimal(10,2)"),
                ("c_binary", "binary", "binary"),
                ("c_date", "date", "date"),
            ],
            location_suffix: "iceberg/typed_columns/",
        },
        Row {
            case: "the table_type parameter alone, with no Iceberg input",
            table: "labelled_only",
            version: None,
            format_version: None,
            iceberg_input: false,
            columns: &[("id", "string", "string")],
            location_suffix: "iceberg/labelled_only/",
        },
    ]
}

/// The four services this flow spans, on ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The signature is
/// never checked: this router runs with authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name`; answers its status and body bytes.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One `awsJson1_1` operation of `signing_name` that must succeed, answering its parsed body.
async fn call(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> Value {
    let target = format!("{target_prefix}.{operation}");
    let (status, answered) = send(
        router,
        signing_name,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target.as_str()),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {signing_name} {operation} must succeed, got {status} {text}"
    );
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

/// One Glue operation that must succeed.
async fn glue(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    call(router, case, "glue", "AWSGlue", operation, body).await
}

/// One Firehose operation that must succeed.
async fn firehose(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    call(router, case, "firehose", "Firehose_20150804", operation, body).await
}

/// One `awsQuery` IAM action that must succeed.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in parameters {
        body.push('&');
        body.push_str(key);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = send(
        router,
        "iam",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "IAM {action} must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// Create the delivery role, trusted by the Firehose service principal and allowed the catalog
/// reads and object writes a delivery makes, and answer its ARN.
async fn given_role(router: &MultiServiceRouter, name: &str) -> String {
    iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", TRUST_POLICY)],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "iceberg-delivery"),
            ("PolicyDocument", ROLE_POLICY),
        ],
    )
    .await;
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// Create the bucket every destination table is located in.
async fn given_bucket(router: &MultiServiceRouter) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "the destination bucket {BUCKET} must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// The `CreateTable` request a row submits: its Hive definition, and the Iceberg input or the
/// `table_type` parameter that marks it.
fn create_table_request(row: &Row) -> Value {
    let declared: Vec<Value> = row
        .columns
        .iter()
        .map(|(name, declared, _)| json!({ "Name": name, "Type": declared }))
        .collect();
    let mut input = json!({
        "Name": row.table,
        "TableType": "EXTERNAL_TABLE",
        "StorageDescriptor": { "Columns": declared, "Location": row.location() },
    });
    let mut request = json!({ "DatabaseName": DATABASE, "TableInput": input.clone() });
    if row.iceberg_input {
        let mut iceberg = json!({ "MetadataOperation": "CREATE" });
        if let Some(version) = row.version {
            iceberg["Version"] = json!(version);
        }
        request["OpenTableFormatInput"] = json!({ "IcebergInput": iceberg });
    } else {
        input["Parameters"] = json!({ TABLE_TYPE: ICEBERG });
        request["TableInput"] = input;
    }
    request
}

/// One table parameter of the stored catalog row, read back through `GetTable`.
async fn table_parameter(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    parameter: &str,
) -> Option<String> {
    read_table(router, case, table).await["Table"]["Parameters"][parameter]
        .as_str()
        .map(str::to_string)
}

/// The stored catalog row, read back through `GetTable`.
async fn read_table(router: &MultiServiceRouter, case: &str, table: &str) -> Value {
    glue(
        router,
        case,
        "GetTable",
        json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await
}

/// Every key [`BUCKET`] holds under `prefix`, read through `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        // Every prefix of this suite is made of unreserved characters and `/`, so it travels in
        // the query string as written.
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing {BUCKET}/{prefix} must succeed, got {status} {text}"
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// The bytes [`BUCKET`] holds at `key`.
async fn object(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<u8> {
    let (status, body) = send(router, "s3", "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new())
        .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: reading {BUCKET}/{key} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// The key inside [`BUCKET`] an `s3://` pointer names.
fn key_of(case: &str, pointer: &str) -> String {
    pointer
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the pointer {pointer} must name an object of {BUCKET}")
        })
        .to_string()
}

/// The document stored at the `s3://` location `pointer` — the read an Iceberg client makes of a
/// table's `metadata_location`.
async fn document_at(router: &MultiServiceRouter, case: &str, pointer: &str) -> Value {
    let body = object(router, case, &key_of(case, pointer)).await;
    serde_json::from_slice(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: the metadata file {pointer} must be JSON, got {error}")
    })
}

/// Whether `id` is spelled the way a UUID is: 32 hexadecimal digits grouped 8-4-4-4-12.
fn is_uuid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups
        .iter()
        .map(|group| group.len())
        .eq([8usize, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|digit| digit.is_ascii_hexdigit()))
}

/// Create the row's Firehose stream, delivering into the row's destination table.
async fn create_stream(router: &MultiServiceRouter, row: &Row, role_arn: &str) {
    let stream = row.stream();
    let answered = firehose(
        router,
        row.case,
        "CreateDeliveryStream",
        json!({
            "DeliveryStreamName": stream,
            "DeliveryStreamType": "DirectPut",
            "IcebergDestinationConfiguration": {
                "RoleARN": role_arn,
                "CatalogConfiguration": {
                    "CatalogARN": format!(
                        "arn:aws:glue:{REGION}:{}:catalog",
                        engine::emulator_account_id()
                    ),
                },
                "DestinationTableConfigurationList": [{
                    "DestinationDatabaseName": DATABASE,
                    "DestinationTableName": row.table,
                }],
                "S3Configuration": {
                    "RoleARN": role_arn,
                    "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                    "ErrorOutputPrefix": row.error_prefix(),
                },
            },
        }),
    )
    .await;
    assert!(
        answered["DeliveryStreamARN"]
            .as_str()
            .is_some_and(|arn| arn.ends_with(&format!("deliverystream/{stream}"))),
        "case {:?}: the create answers {stream}'s ARN, got {answered}",
        row.case
    );
}

/// Wait for the row's stream to report `ACTIVE`, and assert it reports the destination table it
/// was created over.
///
/// A poll loop on the STATUS rather than a wait for the clock-owed span, so how fast the host
/// runs cannot decide a case.
async fn await_active(router: &MultiServiceRouter, row: &Row) {
    let stream = row.stream();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = firehose(
            router,
            row.case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
        let description = &described["DeliveryStreamDescription"];
        if description["DeliveryStreamStatus"] == json!("ACTIVE") {
            let destination = &description["Destinations"][0]["IcebergDestinationDescription"];
            assert_eq!(
                destination["DestinationTableConfigurationList"][0]["DestinationTableName"],
                json!(row.table),
                "case {:?}: {stream} reports the destination table it delivers into, got \
                 {described}",
                row.case
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: {stream} must reach ACTIVE, got {described}",
            row.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert the document `pointer` names is the table's FIRST metadata file: the version asked
/// for, an identity of its own, no snapshot, the table's own location and one schema holding the
/// declared columns as Iceberg fields. Answers the table's `table-uuid`.
async fn assert_initial_document(router: &MultiServiceRouter, row: &Row, pointer: &str) -> String {
    let case = row.case;
    let expected_version = row
        .format_version
        .unwrap_or_else(|| panic!("case {case:?}: this route mints no document"));
    let key = key_of(case, pointer);
    let minted = key
        .strip_prefix(&format!("{}metadata/00000-", row.prefix()))
        .and_then(|rest| rest.strip_suffix(".metadata.json"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the first metadata file of a table is \
                 <location>/metadata/00000-<uuid>.metadata.json, got {key}"
            )
        });
    assert!(
        is_uuid(minted),
        "case {case:?}: the metadata file is named by a UUID, got {minted:?}"
    );

    let document = document_at(router, case, pointer).await;
    assert_eq!(
        document["format-version"].as_u64(),
        Some(expected_version),
        "case {case:?}: expected format-version {expected_version}, got {document}"
    );
    let uuid = document["table-uuid"].as_str().unwrap_or_default();
    assert!(
        is_uuid(uuid),
        "case {case:?}: the table-uuid is spelled as a UUID, got {uuid:?}"
    );
    assert_eq!(
        document["snapshots"],
        json!([]),
        "case {case:?}: a table nothing has delivered into holds no snapshot, got {document}"
    );
    assert_eq!(
        document["current-snapshot-id"].as_i64(),
        Some(-1),
        "case {case:?}: such a table reports no current snapshot, got {document}"
    );
    assert_eq!(
        document["location"].as_str(),
        Some(row.location().trim_end_matches('/')),
        "case {case:?}: the document states the table's own location, got {document}"
    );
    assert_eq!(
        document["last-column-id"].as_u64(),
        u64::try_from(row.columns.len()).ok(),
        "case {case:?}: the document accounts for every column it numbered, got {document}"
    );

    let fields = document["schemas"][0]["fields"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        fields.len(),
        row.columns.len(),
        "case {case:?}: expected {} field(s), got {fields:?}",
        row.columns.len()
    );
    for (index, (column, _, iceberg)) in row.columns.iter().enumerate() {
        assert_eq!(
            &fields[index],
            &json!({
                "id": index + 1,
                "name": column,
                "required": false,
                "type": iceberg,
            }),
            "case {case:?}: field {column} must be numbered {} and typed {iceberg}, got {}",
            index + 1,
            fields[index]
        );
    }
    uuid.to_string()
}

/// RULE: the catalog create that marks a table as Iceberg materialises the table's first
/// metadata document under the table's own location and points the row at it; a table marked by
/// the `table_type` parameter alone gets neither; and creating a Firehose Iceberg destination
/// over either, which delivers nothing by itself, mints no metadata file and moves no pointer.
#[tokio::test]
async fn a_firehose_iceberg_destinations_table_carries_the_metadata_its_create_minted() {
    let router = emulator();
    let rows = rows();
    given_bucket(&router).await;
    glue(
        &router,
        "setup",
        "CreateDatabase",
        json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    let role_arn = given_role(&router, "firehose-iceberg-table-metadata-role").await;

    // THE CREATE MATERIALISES THE TABLE: the pointer on the row, and the document it names.
    let mut pointers: Vec<Option<String>> = Vec::new();
    let mut identities: Vec<String> = Vec::new();
    for row in &rows {
        glue(&router, row.case, "CreateTable", create_table_request(row)).await;
        let pointer = table_parameter(&router, row.case, row.table, METADATA_LOCATION).await;
        assert_eq!(
            pointer.is_some(),
            row.format_version.is_some(),
            "case {:?}: the catalog holds a metadata location for this table exactly when its \
             create minted one, got {pointer:?}",
            row.case
        );
        let stored_version = table_parameter(&router, row.case, row.table, FORMAT_VERSION).await;
        assert_eq!(
            stored_version,
            row.format_version.map(|version| version.to_string()),
            "case {:?}: the row reports the format version its create asked for, got \
             {stored_version:?}",
            row.case
        );

        match pointer.as_deref() {
            Some(pointer) => {
                identities.push(assert_initial_document(&router, row, pointer).await);
                // The catalog keeps the GLUE type names: the Iceberg spelling lives in the
                // document alone.
                let read = read_table(&router, row.case, row.table).await;
                let declared: Vec<String> = read["Table"]["StorageDescriptor"]["Columns"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|column| column["Type"].as_str().unwrap_or_default().to_string())
                    .collect();
                let expected: Vec<String> = row
                    .columns
                    .iter()
                    .map(|(_, glue_type, _)| (*glue_type).to_string())
                    .collect();
                assert_eq!(
                    declared, expected,
                    "case {:?}: GetTable keeps the Glue type names, got {declared:?}",
                    row.case
                );
            }
            None => {
                let keys = keys_under(&router, row.case, &row.prefix()).await;
                assert!(
                    keys.is_empty(),
                    "case {:?}: a table marked by the table_type parameter alone has no \
                     metadata materialised for it, got {keys:?}",
                    row.case
                );
            }
        }
        pointers.push(pointer);
    }

    // Each table got an identity of its own.
    let unique: std::collections::BTreeSet<&String> = identities.iter().collect();
    assert_eq!(
        unique.len(),
        identities.len(),
        "each Iceberg table gets its own table-uuid, got {identities:?}"
    );

    // THE STREAMS: every create is made before any status is waited for, so the clock-owed
    // spans of the whole suite overlap.
    for row in &rows {
        create_stream(&router, row, &role_arn).await;
    }
    for row in &rows {
        await_active(&router, row).await;
    }

    // A STREAM THAT DELIVERED NOTHING CHANGES NOTHING: the pointer each row carries is the one
    // its create minted, the only object under a table's location is that document, and the
    // labelled table still has neither. Watched, so a late write fails the case whenever it
    // happens.
    let until = Instant::now() + WATCH;
    loop {
        for (row, minted) in rows.iter().zip(&pointers) {
            let pointer = table_parameter(&router, row.case, row.table, METADATA_LOCATION).await;
            assert_eq!(
                &pointer, minted,
                "case {:?}: a stream that delivered nothing neither mints a metadata location \
                 nor moves one",
                row.case
            );
            let keys = keys_under(&router, row.case, &row.prefix()).await;
            let expected: Vec<String> = minted
                .iter()
                .map(|pointer| key_of(row.case, pointer))
                .collect();
            assert_eq!(
                keys, expected,
                "case {:?}: the table's location holds exactly the document its create wrote",
                row.case
            );
            let errors = keys_under(&router, row.case, &row.error_prefix()).await;
            assert!(
                errors.is_empty(),
                "case {:?}: nothing was delivered, so the error output stays empty, got \
                 {errors:?}",
                row.case
            );
        }
        if Instant::now() >= until {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // A DELETE REMOVES THE CATALOG ROW AND NOTHING IN AMAZON S3, so the document survives it and
    // a re-create of the same name mints a NEW one beside it, with a new identity and a new
    // pointer — while the stream created over the table stays what it was.
    let row = &rows[0];
    let first = pointers[0]
        .clone()
        .expect("the first row's create mints a metadata location");
    glue(
        &router,
        row.case,
        "DeleteTable",
        json!({ "DatabaseName": DATABASE, "Name": row.table }),
    )
    .await;
    let kept = object(&router, "after the delete", &key_of(row.case, &first)).await;
    assert!(
        !kept.is_empty(),
        "the delete removes only the catalog row, so the metadata document stays"
    );

    glue(&router, row.case, "CreateTable", create_table_request(row)).await;
    let second = table_parameter(&router, row.case, row.table, METADATA_LOCATION)
        .await
        .expect("the re-create mints a metadata location");
    assert_ne!(
        second, first,
        "a re-create mints a NEW metadata document, got {second}"
    );
    let second_uuid = assert_initial_document(&router, row, &second).await;
    assert!(
        !identities.contains(&second_uuid),
        "a re-created table is a new table, so its identity differs; got {second_uuid}"
    );
    let keys = keys_under(&router, row.case, &format!("{}metadata/", row.prefix())).await;
    let mut both = vec![key_of(row.case, &first), key_of(row.case, &second)];
    both.sort();
    assert_eq!(
        keys, both,
        "the surviving document and the new one must both be there"
    );
    await_active(&router, row).await;
}
