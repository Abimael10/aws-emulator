// SPDX-License-Identifier: CC0-1.0

//! The Realtime script contract, over the wire: `CreateScript` persists a script record
//! for the zip file the request provides, mints its id and ARN, records the size and the
//! storage location the SOURCE of that zip file decides, and `DescribeScript` serves that
//! one stored record back — selected by script id or by script ARN.
//!
//! The harness (`GameLift`, `gamelift_arn`) is the one `hosting_loop` already drives the
//! production dispatch path with.

use serde_json::{json, Value};

use super::hosting_loop::{error_code, error_message, gamelift_arn, GameLift};
use service_com_amazonaws_gamelift_gamelift::state::DEFAULT_NODE_JS_VERSION;

/// Every `NodeJsVersion` a Realtime script may declare, from the member's own doc — "The
/// valid values are `10.x | 24.x`" (`smithy_query shape=Script` member `NodeJsVersion`) —
/// which is also what the handler enforces (`state::NODE_JS_VERSIONS`).
const NODE_JS_VERSIONS: [&str; 2] = ["10.x", "24.x"];

/// The `NodeJsVersion` the rows below that are not ABOUT the runtime version declare: the
/// one the service's own default is NOT, so a row that states it says which version it meant
/// (the runtime-version default has its own test at the end of this suite).
const SOME_NODE_JS_VERSION: &str = NODE_JS_VERSIONS[1];

/// The modelled bounds of `CreateScriptInput$Name` and `$Version`
/// (`smithy_query operation=CreateScript`: `NonZeroAndMaxString`, len 1..1024).
const MAX_NAME_LENGTH: usize = 1024;

/// The modelled bounds of `CreateScriptInput$ZipFile` (`ZipBlob`, len 0..5000000) — "the
/// maximum size of a zip file is 5 MB".
const MAX_ZIP_BYTES: usize = 5_000_000;

/// The S3 bucket the rows below that upload from the CALLER's own bucket name.
pub(crate) const CALLER_BUCKET: &str = "my-own-game-scripts";

/// `len` deterministic bytes standing in for a zipped Realtime script, opening with the
/// local-file signature a zip archive carries (`PK\x03\x04`) when there is room for it.
pub(crate) fn zip_bytes(len: usize) -> Vec<u8> {
    let mut bytes: Vec<u8> = b"PK\x03\x04".to_vec();
    bytes.truncate(len);
    while bytes.len() < len {
        bytes.push((bytes.len() % 251) as u8);
    }
    bytes
}

/// A blob travels base64-encoded on awsJson1_1, which is what the wire serializer does
/// with it (`aws_smithy_types::base64`, the codec the runtime itself uses).
pub(crate) fn zip_file_member(bytes: &[u8]) -> Value {
    json!(aws_smithy_types::base64::encode(bytes))
}

/// A caller-owned S3 storage location naming the three members the operation documents as
/// required: "the storage location must specify the Amazon S3 bucket name, the zip file
/// name (the "key"), and a role ARN that allows Amazon GameLift Servers to access the
/// Amazon S3 storage location".
pub(crate) fn caller_location(key: &str) -> Value {
    json!({
        "Bucket": CALLER_BUCKET,
        "Key": key,
        "RoleArn": "arn:aws:iam::123456789012:role/GameLiftScriptAccess",
    })
}

/// Where the zip file a create request provides comes from: the two alternatives
/// CreateScript documents.
enum Source {
    /// A local zip file of this many bytes, sent in `ZipFile`.
    LocalZip(usize),
    /// An S3 object the caller owns, named by `StorageLocation`.
    CallerBucket(Value),
}

/// The `ScriptId` of a created script.
pub(crate) fn script_id_of(script: &Value) -> String {
    script["ScriptId"]
        .as_str()
        .expect("CreateScript must report a ScriptId")
        .to_string()
}

/// The ARN a script with `script_id` is assigned:
/// `arn:aws:gamelift:<region>:<account>:script/<script id>` — the model's
/// `^arn:.*:script\/script-\S+$` with "the resource ID matches the `ScriptId` value"
/// (`smithy_query shape=Script` member `ScriptArn`).
pub(crate) fn script_arn(script_id: &str) -> String {
    gamelift_arn(&format!("script/{script_id}"))
}

/// `CreateScript` stores the script the request configures and reports the stored record;
/// `DescribeScript` serves the same record back, member for member, selected by id or by
/// ARN.
///
/// The rows differ along what the rule generalises over: which of the two documented
/// sources provides the zip file, the size of that zip file (including each modelled
/// bound), which optional members the request sets, each documented `NodeJsVersion`, and
/// how many tags the request assigns.
#[tokio::test]
async fn a_created_script_is_reported_as_stored_by_create_and_by_describe() {
    struct Case {
        label: &'static str,
        /// The request, without its source members.
        request: Value,
        source: Source,
        /// The members the created script must report beyond its id, ARN, creation time
        /// and storage location.
        expected: Value,
    }

    let cases = [
        Case {
            label: "a local zip upload with a name and a version",
            request: json!({
                "Name": "realtime-core",
                "Version": "1.0.0",
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
            source: Source::LocalZip(512),
            expected: json!({
                "Name": "realtime-core",
                "Version": "1.0.0",
                "SizeOnDisk": 512,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a one-byte local zip upload with only a name",
            request: json!({"Name": "tiny", "NodeJsVersion": SOME_NODE_JS_VERSION}),
            source: Source::LocalZip(1),
            expected: json!({
                "Name": "tiny",
                "SizeOnDisk": 1,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a zip upload at the modelled minimum size, carrying no name at all",
            request: json!({"NodeJsVersion": SOME_NODE_JS_VERSION}),
            source: Source::LocalZip(0),
            expected: json!({
                "SizeOnDisk": 0,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a zip upload at the modelled maximum size",
            request: json!({
                "Name": "five-megabytes",
                "Version": "max",
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
            source: Source::LocalZip(MAX_ZIP_BYTES),
            expected: json!({
                "Name": "five-megabytes",
                "Version": "max",
                "SizeOnDisk": MAX_ZIP_BYTES,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a zip upload that picks the older documented Node.js version",
            request: json!({"Name": "node-ten", "NodeJsVersion": NODE_JS_VERSIONS[0]}),
            source: Source::LocalZip(2048),
            expected: json!({
                "Name": "node-ten",
                "SizeOnDisk": 2048,
                "NodeJsVersion": NODE_JS_VERSIONS[0],
            }),
        },
        Case {
            label: "a zip upload that picks the newer documented Node.js version",
            request: json!({"Name": "node-twentyfour", "NodeJsVersion": NODE_JS_VERSIONS[1]}),
            source: Source::LocalZip(4096),
            expected: json!({
                "Name": "node-twentyfour",
                "SizeOnDisk": 4096,
                "NodeJsVersion": NODE_JS_VERSIONS[1],
            }),
        },
        Case {
            label: "a name and a version at the modelled maximum length",
            request: json!({
                "Name": "n".repeat(MAX_NAME_LENGTH),
                "Version": "v".repeat(MAX_NAME_LENGTH),
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
            source: Source::LocalZip(64),
            expected: json!({
                "Name": "n".repeat(MAX_NAME_LENGTH),
                "Version": "v".repeat(MAX_NAME_LENGTH),
                "SizeOnDisk": 64,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a zip upload with one tag",
            request: json!({
                "Name": "tagged-once",
                "NodeJsVersion": SOME_NODE_JS_VERSION,
                "Tags": [{"Key": "owner", "Value": "liveops"}],
            }),
            source: Source::LocalZip(128),
            // `Script` carries no `Tags` member — tags are read back through
            // `ListTagsForResource` — so a tagged script reports what an untagged one does.
            expected: json!({
                "Name": "tagged-once",
                "SizeOnDisk": 128,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a zip upload with three tags",
            request: json!({
                "Name": "tagged-thrice",
                "NodeJsVersion": SOME_NODE_JS_VERSION,
                "Tags": [
                    {"Key": "owner", "Value": "liveops"},
                    {"Key": "env", "Value": "prod"},
                    {"Key": "cost-centre", "Value": "games"},
                ],
            }),
            source: Source::LocalZip(256),
            expected: json!({
                "Name": "tagged-thrice",
                "SizeOnDisk": 256,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a script pulled from the caller's own S3 bucket",
            request: json!({
                "Name": "from-s3",
                "Version": "2.1",
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
            source: Source::CallerBucket(caller_location("scripts/realtime.zip")),
            expected: json!({
                "Name": "from-s3",
                "Version": "2.1",
                // "When files are uploaded from an S3 location, this value remains at 0."
                "SizeOnDisk": 0,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        },
        Case {
            label: "a script pulled from a versioned S3 object, on the newer Node.js version",
            request: json!({"Name": "from-s3-versioned", "NodeJsVersion": NODE_JS_VERSIONS[1]}),
            source: Source::CallerBucket(json!({
                "Bucket": CALLER_BUCKET,
                "Key": "scripts/pinned.zip",
                "RoleArn": "arn:aws:iam::123456789012:role/GameLiftScriptAccess",
                "ObjectVersion": "3HL4kqtJlcpXroDTDmjVBH40Nrjfkd",
            })),
            expected: json!({
                "Name": "from-s3-versioned",
                "SizeOnDisk": 0,
                "NodeJsVersion": NODE_JS_VERSIONS[1],
            }),
        },
    ];

    let gamelift = GameLift::new();
    let mut minted_ids: Vec<String> = Vec::new();
    for case in &cases {
        let label = case.label;
        let mut request = case.request.clone();
        match &case.source {
            Source::LocalZip(len) => {
                request["ZipFile"] = zip_file_member(&zip_bytes(*len));
            }
            Source::CallerBucket(location) => {
                request["StorageLocation"] = location.clone();
            }
        }

        let created = gamelift.ok("CreateScript", request).await;
        let script = created["Script"].clone();
        let script_id = script_id_of(&script);
        assert!(
            script_id.starts_with("script-") && script_id.len() > "script-".len(),
            "case {label:?}: the minted ScriptId must match the modelled ^script-\\S+$, got {script_id:?}"
        );
        assert!(
            !minted_ids.contains(&script_id),
            "case {label:?}: a script ID is unique, got the repeated {script_id:?}"
        );
        minted_ids.push(script_id.clone());

        // The storage location the SOURCE decides: the request's own location for an S3
        // upload, a service-owned one for a local zip file.
        let location = script["StorageLocation"].clone();
        match &case.source {
            Source::CallerBucket(requested) => assert_eq!(
                &location, requested,
                "case {label:?}: a script pulled from the caller's bucket must report the \
                 location the request provided, got {location}"
            ),
            Source::LocalZip(_) => {
                let bucket = location["Bucket"].as_str().unwrap_or_default();
                let key = location["Key"].as_str().unwrap_or_default();
                assert!(
                    !bucket.is_empty() && bucket != CALLER_BUCKET,
                    "case {label:?}: an uploaded zip file must be reported in a service-owned \
                     bucket, got {location}"
                );
                assert!(
                    key.contains(&script_id),
                    "case {label:?}: the service-owned storage location must name the script it \
                     holds, got {location}"
                );
            }
        }

        let mut expected = case.expected.clone();
        expected["ScriptId"] = json!(&script_id);
        expected["ScriptArn"] = json!(script_arn(&script_id));
        expected["StorageLocation"] = location;
        expected["CreationTime"] = script["CreationTime"].clone();
        assert!(
            script["CreationTime"].is_number(),
            "case {label:?}: a created script must carry a CreationTime, got {script}"
        );
        assert_eq!(
            script, expected,
            "case {label:?}: CreateScript must report the stored script, got {script}"
        );

        // The describe path serves the SAME stored record, member for member — selected by
        // the id and by the ARN the create minted, which name the same script.
        for selector in [script_id.clone(), script_arn(&script_id)] {
            let described = gamelift
                .ok("DescribeScript", json!({"ScriptId": &selector}))
                .await;
            assert_eq!(
                described["Script"], expected,
                "case {label:?}: a describe naming {selector:?} must report the created record, got {}",
                described["Script"]
            );
        }
    }

    // Reading a script changed nothing: the same describe, run again, reports exactly what
    // it reported before.
    let first = gamelift
        .ok("DescribeScript", json!({"ScriptId": &minted_ids[0]}))
        .await;
    let second = gamelift
        .ok("DescribeScript", json!({"ScriptId": &minted_ids[0]}))
        .await;
    assert_eq!(
        first, second,
        "describing a script must not change what a later describe reports, got {second}"
    );
}

/// "Script names do not need to be unique": two scripts created under one name are two
/// resources, each with its own id, its own size and its own storage location.
#[tokio::test]
async fn script_names_need_not_be_unique() {
    let gamelift = GameLift::new();
    let mut created: Vec<Value> = Vec::new();
    for len in [16_usize, 32] {
        let script = gamelift
            .ok(
                "CreateScript",
                json!({
                    "Name": "shared",
                    "ZipFile": zip_file_member(&zip_bytes(len)),
                    "NodeJsVersion": SOME_NODE_JS_VERSION,
                }),
            )
            .await;
        created.push(script["Script"].clone());
    }

    assert_ne!(
        script_id_of(&created[0]),
        script_id_of(&created[1]),
        "two scripts sharing a name must be distinct resources, got {} and {}",
        created[0],
        created[1]
    );
    for (len, script) in [16, 32].iter().zip(created.iter()) {
        assert_eq!(
            script["SizeOnDisk"],
            json!(len),
            "case zip length {len}: each script must report its own size, got {script}"
        );
        let described = gamelift
            .ok("DescribeScript", json!({"ScriptId": script_id_of(script)}))
            .await;
        assert_eq!(
            described["Script"], *script,
            "case zip length {len}: each script sharing the name must read back as itself, got {}",
            described["Script"]
        );
    }
}

/// A create that does not name exactly one documented source for the zip file, names an
/// incomplete storage location, names NO Node.js version, or asks for a Node.js version the
/// service does not run, is refused with the modelled client error — and stores nothing.
#[tokio::test]
async fn a_create_outside_the_documented_contract_is_refused() {
    struct Case {
        label: &'static str,
        request: Value,
        code: &'static str,
    }

    let zip = zip_file_member(&zip_bytes(64));
    let cases = vec![
        Case {
            label: "both documented sources at once",
            request: json!({
                "Name": "two-sources",
                "ZipFile": zip.clone(),
                "StorageLocation": caller_location("scripts/realtime.zip"),
            }),
            code: "InvalidRequestException",
        },
        Case {
            label: "neither documented source",
            request: json!({"Name": "no-source"}),
            code: "InvalidRequestException",
        },
        Case {
            label: "a storage location naming no bucket",
            request: json!({
                "Name": "no-bucket",
                "StorageLocation": {
                    "Key": "scripts/realtime.zip",
                    "RoleArn": "arn:aws:iam::123456789012:role/GameLiftScriptAccess",
                },
            }),
            code: "InvalidRequestException",
        },
        Case {
            label: "a storage location naming no key",
            request: json!({
                "Name": "no-key",
                "StorageLocation": {
                    "Bucket": CALLER_BUCKET,
                    "RoleArn": "arn:aws:iam::123456789012:role/GameLiftScriptAccess",
                },
            }),
            code: "InvalidRequestException",
        },
        Case {
            label: "a storage location naming no role",
            request: json!({
                "Name": "no-role",
                "StorageLocation": {"Bucket": CALLER_BUCKET, "Key": "scripts/realtime.zip"},
            }),
            code: "InvalidRequestException",
        },
        Case {
            label: "a Node.js version the service does not run, in the modelled form",
            request: json!({"Name": "node-twelve", "ZipFile": zip.clone(), "NodeJsVersion": "12.x"}),
            code: "InvalidRequestException",
        },
        Case {
            label: "a Node.js version one minor past a documented one",
            request: json!({"Name": "node-ten-one", "ZipFile": zip.clone(), "NodeJsVersion": "10.1"}),
            code: "InvalidRequestException",
        },
        Case {
            label: "a Node.js version outside the modelled pattern",
            request: json!({"Name": "node-latest", "ZipFile": zip.clone(), "NodeJsVersion": "latest"}),
            code: "ValidationException",
        },
        Case {
            label: "a name one past the modelled maximum length",
            request: json!({"Name": "n".repeat(MAX_NAME_LENGTH + 1), "ZipFile": zip.clone()}),
            code: "ValidationException",
        },
        Case {
            label: "a zip file one byte past the modelled maximum size",
            request: json!({
                "Name": "too-large",
                "ZipFile": zip_file_member(&zip_bytes(MAX_ZIP_BYTES + 1)),
            }),
            code: "ValidationException",
        },
        Case {
            label: "a tag set that assigns one key twice",
            request: json!({
                "Name": "repeated-tag-key",
                "ZipFile": zip.clone(),
                "NodeJsVersion": SOME_NODE_JS_VERSION,
                "Tags": [
                    {"Key": "owner", "Value": "liveops"},
                    {"Key": "owner", "Value": "platform"},
                ],
            }),
            code: "TaggingFailedException",
        },
    ];

    let gamelift = GameLift::new();
    for case in &cases {
        let label = case.label;
        let (status, code) = gamelift.err("CreateScript", case.request.clone()).await;
        assert_eq!(
            (status, code.as_str()),
            (400, case.code),
            "case {label:?}: the request must be refused with {}, got {status} {code}",
            case.code
        );
    }

    // Every refused create stored nothing: the next accepted create mints the FIRST script
    // of the store, and it is the only one a describe can find.
    let created = gamelift
        .ok(
            "CreateScript",
            json!({
                "Name": "the-first-stored-script",
                "ZipFile": zip,
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        )
        .await;
    let described = gamelift
        .ok(
            "DescribeScript",
            json!({"ScriptId": script_id_of(&created["Script"])}),
        )
        .await;
    assert_eq!(
        described["Script"]["Name"],
        json!("the-first-stored-script"),
        "the one stored script must be the one the accepted create made, got {}",
        described["Script"]
    );
}

/// The account's script quota bounds the stored collection: once it is reached, the next
/// create answers the modelled `ConflictException` and stores nothing.
#[tokio::test]
async fn the_script_quota_is_the_bound_on_the_stored_collection() {
    let quota = service_com_amazonaws_gamelift_gamelift::state::MAX_SCRIPTS_PER_ACCOUNT;
    let gamelift = GameLift::new();
    let mut last_id = String::new();
    for index in 0..quota {
        let created = gamelift
            .ok(
                "CreateScript",
                json!({
                    "Name": format!("quota-{index}"),
                    "ZipFile": zip_file_member(&zip_bytes(8)),
                    "NodeJsVersion": SOME_NODE_JS_VERSION,
                }),
            )
            .await;
        last_id = script_id_of(&created["Script"]);
    }

    let (status, code) = gamelift
        .err(
            "CreateScript",
            json!({
                "Name": "one-script-too-many",
                "ZipFile": zip_file_member(&zip_bytes(8)),
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        )
        .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ConflictException"),
        "a create past the quota must be refused, got {status} {code}"
    );

    // The refused create stored nothing, and the script that filled the quota is intact.
    let described = gamelift
        .ok("DescribeScript", json!({"ScriptId": &last_id}))
        .await;
    assert_eq!(
        described["Script"]["Name"],
        json!(format!("quota-{}", quota - 1)),
        "the script that filled the quota must be unchanged, got {}",
        described["Script"]
    );
}

/// `DescribeScript` names ONE resource, so an identifier that names no stored script is
/// the modelled `NotFoundException` rather than an empty success.
#[tokio::test]
async fn describing_a_script_that_is_not_stored_is_refused() {
    let gamelift = GameLift::new();
    let created = gamelift
        .ok(
            "CreateScript",
            json!({
                "Name": "the-only-script",
                "ZipFile": zip_file_member(&zip_bytes(96)),
                "NodeJsVersion": SOME_NODE_JS_VERSION,
            }),
        )
        .await;
    let script = created["Script"].clone();
    let stored_id = script_id_of(&script);

    let cases = vec![
        ("an id no script carries", json!("script-does-not-exist")),
        (
            "an ARN no script carries",
            json!(script_arn("script-does-not-exist")),
        ),
        (
            "an ARN in another account for a stored id",
            json!(format!(
                "arn:aws:gamelift:us-west-2:999999999999:script/{stored_id}"
            )),
        ),
    ];
    for (label, selector) in cases {
        let (status, code) = gamelift
            .err("DescribeScript", json!({"ScriptId": selector}))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "NotFoundException"),
            "case {label:?}: an identifier naming no stored script must be refused, got {status} {code}"
        );
    }

    // The stored script is untouched by those refusals.
    let described = gamelift
        .ok("DescribeScript", json!({"ScriptId": &stored_id}))
        .await;
    assert_eq!(
        described["Script"], script,
        "the stored script must still read back as created, got {}",
        described["Script"]
    );
}

/// One row of the runtime-version table: the case, the `NodeJsVersion` the create states
/// (`None` = no member at all), the rest of the create body, and the version the stored
/// script must report — `""` where the create is refused instead.
struct RuntimeRow {
    case: &'static str,
    stated: Option<&'static str>,
    source: Value,
    tags: Option<Value>,
    expected: &'static str,
}

fn runtime_rows() -> Vec<RuntimeRow> {
    let zip = zip_file_member(&zip_bytes(146));
    vec![
        RuntimeRow {
            case: "a local zip file and no runtime version at all",
            stated: None,
            source: json!({ "ZipFile": zip.clone() }),
            tags: None,
            expected: DEFAULT_NODE_JS_VERSION,
        },
        RuntimeRow {
            case: "a tagged create of a local zip file naming no runtime version",
            stated: None,
            source: json!({ "ZipFile": zip.clone() }),
            tags: Some(json!([{ "Key": "owner", "Value": "liveops" }])),
            expected: DEFAULT_NODE_JS_VERSION,
        },
        RuntimeRow {
            case: "an empty local zip file naming no runtime version",
            stated: None,
            source: json!({ "ZipFile": zip_file_member(&zip_bytes(0)) }),
            tags: None,
            expected: DEFAULT_NODE_JS_VERSION,
        },
        RuntimeRow {
            case: "a caller-owned storage location and no runtime version at all",
            stated: None,
            source: json!({ "StorageLocation": caller_location("scripts/realtime.zip") }),
            tags: None,
            expected: "",
        },
        RuntimeRow {
            case: "a tagged create from a caller-owned location naming no runtime version",
            stated: None,
            source: json!({ "StorageLocation": caller_location("scripts/tagged.zip") }),
            tags: Some(json!([{ "Key": "owner", "Value": "liveops" }])),
            expected: "",
        },
        RuntimeRow {
            case: "the default version stated explicitly, from a local zip file",
            stated: Some(NODE_JS_VERSIONS[0]),
            source: json!({ "ZipFile": zip.clone() }),
            tags: None,
            expected: NODE_JS_VERSIONS[0],
        },
        RuntimeRow {
            case: "the default version stated explicitly, from a caller-owned location",
            stated: Some(NODE_JS_VERSIONS[0]),
            source: json!({ "StorageLocation": caller_location("scripts/stated.zip") }),
            tags: None,
            expected: NODE_JS_VERSIONS[0],
        },
        RuntimeRow {
            case: "the other documented version, which no default overrides",
            stated: Some(NODE_JS_VERSIONS[1]),
            source: json!({ "StorageLocation": caller_location("scripts/other.zip") }),
            tags: None,
            expected: NODE_JS_VERSIONS[1],
        },
    ]
}

/// The create body one runtime-version row asks for.
fn runtime_create_body(row: &RuntimeRow, name: &str) -> Value {
    let mut body = json!({ "Name": name, "Version": "1.0.0" });
    if let Value::Object(members) = row.source.clone() {
        for (member, value) in members {
            body[member] = value;
        }
    }
    if let Some(stated) = row.stated {
        body["NodeJsVersion"] = json!(stated);
    }
    if let Some(tags) = &row.tags {
        body["Tags"] = tags.clone();
    }
    body
}

/// Where a create NAMES no runtime version, which of the two documented file sources it names
/// is what decides the outcome: the create that hands the service the file itself records the
/// script on the documented default, and the create that leaves the file in the caller's own
/// S3 bucket is refused with the service's own sentence, recording nothing.
///
/// Recorded behaviour on both halves: a `CreateScript` carrying a `ZipFile` and no
/// `NodeJsVersion` is served and the script reports `10.x` ("the Node.js version used for
/// execution of your Realtime script. The valid values are `10.x | 24.x`. By default,
/// `NodeJsVersion` is `10.x`", `smithy_query(query="doc", shape="CreateScriptInput",
/// member="NodeJsVersion")`), while a create naming a `StorageLocation` and no
/// `NodeJsVersion` is refused `InvalidRequestException` with the sentence
/// `state::NODE_JS_VERSION_REQUIRED_MESSAGE` records the source of.
///
/// The rows span what the rule generalises over: which file source the create names (a local
/// zip file, including an empty one, and a caller-owned S3 location), whether the create states
/// the member at all (absent, each documented value), and whether the script carries tags —
/// every served row read back through `DescribeScript`, so the version the create applied is
/// the one the record holds, and every refused row leaving no script behind.
#[tokio::test]
async fn a_create_naming_no_runtime_version_is_decided_by_the_file_source_it_names() {
    let gamelift = GameLift::new();
    for (index, row) in runtime_rows().iter().enumerate() {
        let case = row.case;
        let name = format!("runtime-default-{index}");
        if row.expected.is_empty() {
            let (status, payload) = gamelift
                .call("CreateScript", &runtime_create_body(row, &name))
                .await;
            assert_eq!(
                (status, error_code(&payload).as_str()),
                (400, "InvalidRequestException"),
                "case {case:?}: expected the create to be refused, got {status} with {payload}"
            );
            assert_eq!(
                error_message(&payload),
                "The NodeJsVersion parameter is required. See the GameLift API Reference for \
                 CreateScript() for a list of supported runtime versions.",
                "case {case:?}: expected the service's own sentence, got {payload}"
            );
            // A refused create records nothing: no script answers to the name it asked for.
            let (status, listed) = gamelift
                .call("DescribeScript", &json!({ "ScriptId": format!("script-{name}") }))
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: a refused create records no script, got {status} with {listed}"
            );
            continue;
        }
        let created = gamelift
            .ok("CreateScript", runtime_create_body(row, &name))
            .await;
        let script = &created["Script"];
        assert_eq!(
            script["NodeJsVersion"].as_str(),
            Some(row.expected),
            "case {case:?}: expected the create to report NodeJsVersion {:?}, got {created}",
            row.expected
        );
        let script_id = script["ScriptId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateScript minted no ScriptId"))
            .to_string();

        // ROUND TRIP: the record the create left behind reports the same version.
        let described = gamelift
            .ok("DescribeScript", json!({ "ScriptId": script_id }))
            .await;
        assert_eq!(
            described["Script"]["NodeJsVersion"].as_str(),
            Some(row.expected),
            "case {case:?}: expected DescribeScript to report NodeJsVersion {:?}, got \
             {described}",
            row.expected
        );
        assert_eq!(
            described["Script"]["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: DescribeScript must report the created script, got {described}"
        );
    }
}

/// The source rule decides only the create that NAMES no version: a version outside the
/// documented catalog is still refused from either source, and records nothing.
///
/// The rows span the class: a major the service does not run, a minor past a documented
/// value, and a documented major with another minor — each from both file sources.
#[tokio::test]
async fn a_runtime_version_outside_the_documented_catalog_is_still_refused() {
    let gamelift = GameLift::new();
    let zip = zip_file_member(&zip_bytes(64));
    for (case, version) in [
        ("a major the service does not run", "12.x"),
        ("a minor past a documented version", "10.1"),
        ("a documented major with another minor", "24.1"),
    ] {
        for (source_case, source) in [
            ("a local zip file", json!({ "ZipFile": zip.clone() })),
            (
                "a caller-owned location",
                json!({ "StorageLocation": caller_location("scripts/refused.zip") }),
            ),
        ] {
            let mut body = json!({ "Name": "refused-runtime", "NodeJsVersion": version });
            if let Value::Object(members) = source {
                for (member, value) in members {
                    body[member] = value;
                }
            }
            let (status, code) = gamelift.err("CreateScript", body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidRequestException"),
                "case {case:?} from {source_case:?}: expected the runtime version {version:?} \
                 to be refused, got {status} {code}"
            );
        }
    }

    // And a create naming NO version at all, from the source that takes the default, is
    // served — so the refusals above are about the VERSION and not about the member being
    // absent.
    let created = gamelift
        .ok(
            "CreateScript",
            json!({ "Name": "served-without-a-runtime", "ZipFile": zip }),
        )
        .await;
    assert_eq!(
        created["Script"]["NodeJsVersion"].as_str(),
        Some(DEFAULT_NODE_JS_VERSION),
        "a create naming no runtime version is served on the default, got {created}"
    );
    let script_id = created["Script"]["ScriptId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateScript minted no ScriptId, got {created}"))
        .to_string();
    let described = gamelift
        .ok("DescribeScript", json!({ "ScriptId": script_id }))
        .await;
    assert_eq!(
        described["Script"]["NodeJsVersion"].as_str(),
        Some(DEFAULT_NODE_JS_VERSION),
        "the stored script reports the default runtime version, got {described}"
    );
}
