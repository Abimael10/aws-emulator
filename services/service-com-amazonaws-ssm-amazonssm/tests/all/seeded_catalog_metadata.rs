// SPDX-License-Identifier: CC0-1.0

//! The METADATA of the catalogs Systems Manager seeds in every account and Region, read the
//! way a client discovers them: `ListDocuments`, `DescribeDocument` and
//! `ListDocumentVersions` over the Amazon-owned document catalogue, and `GetInventorySchema`
//! over the default inventory types.
//!
//! The rules under test:
//!
//! 1. Every member a read reports of a published document is the catalogue's own fact about
//!    THAT document, derived from the body the catalogue carries for it: the schema version
//!    the body declares and the platforms its steps leave it compatible with
//!    (`DocumentIdentifier$SchemaVersion`, `$PlatformTypes`), the format the body is authored
//!    in (`$DocumentFormat`), and the kind of resource the catalogue declares the document
//!    acts on (`$TargetType`). So a document Amazon publishes for the agent's legacy plugin
//!    schema is reported at THAT schema with the platforms that plugin runs on, a body
//!    Amazon authored in YAML is reported as YAML, and a runbook that acts on a volume or a
//!    snapshot reports that resource's type rather than none.
//! 2. Every read of one published document agrees: the listing, the describe and the version
//!    listing report the same schema version, platforms and format, because all three
//!    project the one catalogue entry.
//! 3. `GetInventorySchema` reports, for a default inventory type, the whole attribute list
//!    and the schema version Systems Manager publishes for it — so a client composing an
//!    inventory filter on a published attribute finds that attribute.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct.
//!
//! The tables span what each rule generalises over — the schema versions the catalogue
//! publishes, both authored formats, a declared target type and none, and inventory types
//! the issue's reproduction did not cite beside the one it did.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, router};

/// The `PlatformType` values the model's enum declares, in the order it declares them
/// (`smithy_query(query="shape", shape="PlatformType")`), which is what a document whose
/// body restricts it to no platform is compatible with.
const EVERY_PLATFORM: &[&str] = &["Windows", "Linux", "MacOS"];

/// The data type an attribute whose values are TEXT carries
/// (`InventoryItemAttribute$DataType`, of the modelled `InventoryAttributeDataType` enum).
const DATA_TYPE_STRING: &str = "STRING";

/// The data type an attribute whose values are NUMBERS carries — the other value of that
/// enum.
const DATA_TYPE_NUMBER: &str = "NUMBER";

/// The attributes Systems Manager publishes for `AWS:ComplianceItem`, in the order the
/// published schema states them.
///
/// Source: the `AWS:ComplianceItem` table of "Metadata collected by Systems Manager
/// Inventory" in the AWS Systems Manager User Guide — the compliance type and execution that
/// produced the finding, the finding's own identity, status and severity, and the detail a
/// patch or association finding carries.
const COMPLIANCE_ITEM_ATTRIBUTES: &[&str] = &[
    "ComplianceType",
    "ExecutionId",
    "ExecutionType",
    "ExecutionTime",
    "Id",
    "Title",
    "Status",
    "Severity",
    "DocumentName",
    "DocumentVersion",
    "Classification",
    "PatchBaselineId",
    "PatchSeverity",
    "PatchState",
    "PatchGroup",
    "InstalledTime",
    "InstallOverrideList",
    "DetailedText",
    "DetailedLink",
    "CVEIds",
    "DocumentExecutionTime",
];

/// The `DocumentIdentifier` an unfiltered-by-anything-else `ListDocuments` reports for the
/// document `name` names, asserting the listing resolved it without any create.
async fn identifier(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, page) = call(
        router,
        "ListDocuments",
        &json!({ "Filters": [{ "Key": "Name", "Values": [name] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: ListDocuments must report the catalogue, got {status} {page}"
    );
    page["DocumentIdentifiers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|identifier| identifier["Name"] == json!(name))
        .unwrap_or_else(|| {
            panic!("case {case}: a listing must report the published document {name}, got {page}")
        })
}

/// `DescribeDocument`'s `Document` for `name`, asserting the read succeeded.
async fn described(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, response) = call(router, "DescribeDocument", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case}: DescribeDocument {name} must resolve a document AWS publishes, got \
         {status} {response}"
    );
    response["Document"].clone()
}

/// The `DocumentVersionInfo` entries `ListDocumentVersions` reports for `name`, asserting the
/// name resolved.
async fn versions(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let (status, response) = call(router, "ListDocumentVersions", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case}: ListDocumentVersions {name} must resolve a document AWS publishes, got \
         {status} {response}"
    );
    response["DocumentVersions"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case}: a version listing reports DocumentVersions, got {response}")
        })
}

/// The `PlatformTypes` of a reported identity, as owned strings.
fn platforms(reported: &Value) -> Vec<String> {
    reported["PlatformTypes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|platform| platform.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `Schemas` a `GetInventorySchema` call reports, asserting it succeeded.
async fn schemas(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<Value> {
    let (status, response) = call(router, "GetInventorySchema", body).await;
    assert_eq!(
        status, 200,
        "case {case}: GetInventorySchema {body} must succeed, got {status} {response}"
    );
    response["Schemas"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: a schema read reports Schemas, got {response}"))
}

/// The `(Name, DataType)` pairs of one reported `InventoryItemSchema`, in order.
fn attributes(schema: &Value) -> Vec<(String, String)> {
    schema["Attributes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|attribute| {
            (
                attribute["Name"].as_str().unwrap_or_default().to_string(),
                attribute["DataType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// RULE: a published document's schema version and platform list are read off the body the
/// catalogue carries for it, so a document Amazon publishes at the agent's legacy schema is
/// reported at that schema, and the platforms are the ones its plugins run on — and the
/// listing and the describe report the same pair.
///
/// The rows span every schema version the published command and session documents carry, the
/// legacy and current plugin models, and each platform restriction a body can express: one
/// plugin's own, a step precondition's, and none at all.
#[tokio::test]
async fn a_published_documents_schema_version_and_platforms_come_from_its_body() {
    /// One row: the document, the schema version every read must report for it, and the
    /// platforms it must be reported compatible with.
    struct Row {
        name: &'static str,
        schema_version: &'static str,
        platforms: &'static [&'static str],
    }

    let rows = &[
        // The command documents Systems Manager publishes for the agent's LEGACY plugin
        // model, which declare their plugins under `runtimeConfig` at schema 1.2.
        Row {
            name: "AWS-FindWindowsUpdates",
            schema_version: "1.2",
            platforms: &["Windows"],
        },
        Row {
            name: "AWS-RefreshAssociation",
            schema_version: "1.2",
            platforms: EVERY_PLATFORM,
        },
        Row {
            name: "AWS-JoinDirectoryServiceDomain",
            schema_version: "1.2",
            platforms: &["Windows", "Linux"],
        },
        // An entry of the same family the issue's reproduction did not cite.
        Row {
            name: "AWS-ConfigureCloudWatch",
            schema_version: "1.2",
            platforms: &["Windows"],
        },
        // Two more of the same family, which the catalogue already carried at that schema:
        // the shell and PowerShell command documents.
        Row {
            name: "AWS-RunShellScript",
            schema_version: "1.2",
            platforms: &["Linux", "MacOS"],
        },
        Row {
            name: "AWS-RunPowerShellScript",
            schema_version: "1.2",
            platforms: &["Windows"],
        },
        // The current plugin model: a step's own plugin decides its platforms.
        Row {
            name: "AWS-RunDocument",
            schema_version: "2.2",
            platforms: &["Windows", "Linux"],
        },
        Row {
            name: "AWS-RunRemoteScript",
            schema_version: "2.2",
            platforms: &["Windows", "Linux"],
        },
        // A step precondition narrows the document further than its plugin alone would.
        Row {
            name: "AWS-ApplyAnsiblePlaybooks",
            schema_version: "2.2",
            platforms: &["Linux"],
        },
        // An Automation runbook's actions run in the service, so nothing restricts it; a
        // session document declares no step at all.
        Row {
            name: "AWS-CreateSnapshot",
            schema_version: "0.3",
            platforms: EVERY_PLATFORM,
        },
        Row {
            name: "AWS-StartSSHSession",
            schema_version: "1.0",
            platforms: EVERY_PLATFORM,
        },
    ];

    let router = router();
    for row in rows {
        let case = row.name;
        let expected: Vec<String> = row.platforms.iter().map(|p| (*p).to_string()).collect();

        let listed = identifier(&router, case, row.name).await;
        assert_eq!(
            listed["SchemaVersion"].as_str(),
            Some(row.schema_version),
            "case {case}: ListDocuments must report SchemaVersion {}, got {listed}",
            row.schema_version
        );
        assert_eq!(
            platforms(&listed),
            expected,
            "case {case}: ListDocuments must report PlatformTypes {expected:?}, got {listed}"
        );

        let document = described(&router, case, row.name).await;
        assert_eq!(
            document["SchemaVersion"].as_str(),
            Some(row.schema_version),
            "case {case}: DescribeDocument must report the same SchemaVersion {}, got {document}",
            row.schema_version
        );
        assert_eq!(
            platforms(&document),
            expected,
            "case {case}: DescribeDocument must report the same PlatformTypes {expected:?}, \
             got {document}"
        );
    }

    // The rows carry more than one schema version and more than one platform set, so none of
    // the above is satisfied by one value the service reports for every document.
    let mut seen_versions: Vec<&str> = rows.iter().map(|row| row.schema_version).collect();
    seen_versions.sort_unstable();
    seen_versions.dedup();
    assert!(
        seen_versions.len() > 1,
        "the rows must span more than one published schema version, got {seen_versions:?}"
    );
}

/// RULE: the format a read reports for a published document is the format Amazon AUTHORED
/// its body in — not the one a create that declares none falls back to — and the target type
/// is the kind of resource the catalogue declares the document acts on, or absent where it
/// declares none. The listing and the version listing report the same pair.
///
/// The rows span both authored formats, three declared target types and none at all.
#[tokio::test]
async fn an_authored_format_and_a_declared_target_type_are_reported_as_the_catalogue_holds_them() {
    /// One row: the document, the format every read must report, and the target type it must
    /// report (`None` for a document the catalogue declares none for).
    struct Row {
        name: &'static str,
        document_format: &'static str,
        target_type: Option<&'static str>,
    }

    let rows = &[
        // The runbooks that act on an Amazon EBS volume and on a snapshot of one, which
        // Amazon publishes in YAML.
        Row {
            name: "AWS-CreateSnapshot",
            document_format: "YAML",
            target_type: Some("/AWS::EC2::Volume"),
        },
        Row {
            name: "AWS-DeleteSnapshot",
            document_format: "YAML",
            target_type: Some("/AWS::EC2::Snapshot"),
        },
        Row {
            name: "AWS-ResizeInstance",
            document_format: "YAML",
            target_type: Some("/AWS::EC2::Instance"),
        },
        // A command document authored in YAML, which declares no target type.
        Row {
            name: "AWS-InstallWindowsUpdates",
            document_format: "YAML",
            target_type: None,
        },
        // Entries authored in JSON, one per declared target type and one with none: the last
        // two the issue's reproduction did not cite.
        Row {
            name: "AWS-CreateImage",
            document_format: "JSON",
            target_type: Some("/AWS::EC2::Instance"),
        },
        Row {
            name: "AWS-ConfigureS3BucketVersioning",
            document_format: "JSON",
            target_type: Some("/AWS::S3::Bucket"),
        },
        Row {
            name: "AWS-RunShellScript",
            document_format: "JSON",
            target_type: None,
        },
    ];

    let router = router();
    for row in rows {
        let case = row.name;

        let listed = identifier(&router, case, row.name).await;
        assert_eq!(
            listed["DocumentFormat"].as_str(),
            Some(row.document_format),
            "case {case}: ListDocuments must report the authored format {}, got {listed}",
            row.document_format
        );
        assert_eq!(
            listed["TargetType"].as_str(),
            row.target_type,
            "case {case}: ListDocuments must report TargetType {:?}, got {listed}",
            row.target_type
        );

        let reported = versions(&router, case, row.name).await;
        let default = reported
            .iter()
            .find(|version| version["IsDefaultVersion"] == json!(true))
            .unwrap_or_else(|| {
                panic!("case {case}: the version listing names a default version, got {reported:?}")
            });
        assert_eq!(
            default["DocumentFormat"].as_str(),
            Some(row.document_format),
            "case {case}: ListDocumentVersions must report the same authored format {}, got \
             {default}",
            row.document_format
        );
    }

    // A `TargetType` filter selects on the member the rows pin, so the declared type is the
    // document's own rather than a value only this one read renders.
    for (case, target_type, expected) in [
        ("the volume runbook", "/AWS::EC2::Volume", "AWS-CreateSnapshot"),
        (
            "the snapshot runbook",
            "/AWS::EC2::Snapshot",
            "AWS-DeleteSnapshot",
        ),
    ] {
        let (status, page) = call(
            &router,
            "ListDocuments",
            &json!({ "Filters": [{ "Key": "TargetType", "Values": [target_type] }] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: a TargetType filter must be served, got {status} {page}"
        );
        let names: Vec<String> = page["DocumentIdentifiers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|identifier| identifier["Name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            names.contains(&expected.to_string()),
            "case {case}: a filter for {target_type} must report {expected}, got {names:?}"
        );
    }
}

/// RULE: a schema read reports the WHOLE attribute list and the schema version Systems
/// Manager publishes for a default inventory type, each attribute carrying the data type of
/// the values reported under it — the number one where the attribute counts or measures
/// something, the text one otherwise — so a client composing a filter or an aggregation on a
/// published attribute finds that attribute, with the type its values have.
///
/// The rows are the type the issue's reproduction cites beside types it does not, and span a
/// type whose attributes are all text, one that counts, and one that measures.
#[tokio::test]
async fn a_default_inventory_type_reports_its_whole_published_schema() {
    /// One row: the type a read names, the version it must report, its published attributes
    /// in order, and which of them carry numbers rather than text.
    struct Row {
        type_name: &'static str,
        version: &'static str,
        attributes: &'static [&'static str],
        numeric: &'static [&'static str],
    }

    impl Row {
        /// The `(Name, DataType)` pairs this row's schema must report, in order.
        fn expected(&self) -> Vec<(String, String)> {
            self.attributes
                .iter()
                .map(|name| {
                    let data_type = if self.numeric.contains(name) {
                        DATA_TYPE_NUMBER
                    } else {
                        DATA_TYPE_STRING
                    };
                    ((*name).to_string(), data_type.to_string())
                })
                .collect()
        }
    }

    let rows = &[
        Row {
            type_name: "AWS:ComplianceItem",
            version: "1.1",
            attributes: COMPLIANCE_ITEM_ATTRIBUTES,
            numeric: &[],
        },
        Row {
            type_name: "AWS:Application",
            version: "1.1",
            attributes: &[
                "Name",
                "ApplicationType",
                "Publisher",
                "Version",
                "Release",
                "Epoch",
                "InstalledTime",
                "Architecture",
                "URL",
                "Summary",
                "PackageId",
            ],
            numeric: &[],
        },
        Row {
            type_name: "AWS:ResourceGroup",
            version: "1.0",
            attributes: &["Name", "Arn"],
            numeric: &[],
        },
        // A type that MEASURES: the node's processor detail, whose counts and clock speed
        // are numbers while its model and kernel strings are text.
        Row {
            type_name: "AWS:InstanceDetailedInformation",
            version: "1.0",
            attributes: &[
                "CPUModel",
                "CPUCores",
                "CPUs",
                "CPUSpeedMHz",
                "CPUSockets",
                "CPUHyperThreadEnabled",
                "OSServicePack",
                "KernelVersion",
            ],
            numeric: &["CPUCores", "CPUs", "CPUSpeedMHz", "CPUSockets"],
        },
    ];

    let router = router();
    for row in rows {
        let case = row.type_name;
        let reported = schemas(&router, case, &json!({ "TypeName": case })).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case}: a read naming one type reports that one schema, got {reported:?}"
        );
        assert_eq!(
            reported[0]["TypeName"].as_str(),
            Some(row.type_name),
            "case {case}: expected the type the read names, got {}",
            reported[0]
        );
        assert_eq!(
            reported[0]["Version"].as_str(),
            Some(row.version),
            "case {case}: expected the published schema version {}, got {}",
            row.version,
            reported[0]
        );
        assert_eq!(
            attributes(&reported[0]),
            row.expected(),
            "case {case}: expected that type's published attributes, got {}",
            reported[0]
        );
    }

    // The rows carry both data types, so none of the above is satisfied by one type the
    // service reports for every attribute of every schema.
    assert!(
        rows.iter().any(|row| !row.numeric.is_empty())
            && rows.iter().any(|row| row.numeric.is_empty()),
        "the rows must span an attribute that carries a number and one that carries text"
    );

    // The unfiltered read reports the same schema for each of those types, so what a client
    // discovers by listing the catalog is what a narrowed read serves.
    let case = "the whole default catalog";
    let whole = schemas(&router, case, &json!({})).await;
    for row in rows {
        let found = whole
            .iter()
            .find(|schema| schema["TypeName"] == json!(row.type_name))
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the catalog must report {}, got {whole:?}",
                    row.type_name
                )
            });
        assert_eq!(
            (found["Version"].as_str(), attributes(found)),
            (Some(row.version), row.expected()),
            "case {case}: the catalog must report {} as the narrowed read does, got {found}",
            row.type_name
        );
    }

    // Every entry of the catalog satisfies what the model declares of an
    // `InventoryItemSchema`: a `AWS:`/`Custom:` type name and 1..=50 attributes.
    for schema in &whole {
        let type_name = schema["TypeName"].as_str().unwrap_or_default();
        assert!(
            type_name.starts_with("AWS:") || type_name.starts_with("Custom:"),
            "case {case}: every reported type name is of a published namespace, got {schema}"
        );
        let count = attributes(schema).len();
        assert!(
            (1..=50).contains(&count),
            "case {case}: {type_name} must report 1..=50 attributes, got {count}"
        );
    }
}

/// RULE: a document Systems Manager publishes several revisions of lists one
/// `DocumentVersionInfo` per revision — the newest first and the only default one — and a
/// name the catalogue holds resolves without any create, whichever family it belongs to.
#[tokio::test]
async fn a_published_support_runbook_lists_the_revisions_the_catalogue_publishes() {
    let router = router();

    // (case, the document, how many revisions the catalogue publishes of it)
    let rows: &[(&str, &str, usize)] = &[
        ("a support runbook with several revisions", "AWSSupport-TroubleshootSSH", 5),
        ("a support runbook published once", "AWSSupport-TroubleshootRDP", 1),
        ("a remediation runbook of another family", "AWSConfigRemediation-DeleteIAMRole", 1),
    ];

    for (case, name, revisions) in rows {
        let reported = versions(&router, case, name).await;
        assert_eq!(
            reported.len(),
            *revisions,
            "case {case}: expected {revisions} revision(s) of {name}, got {reported:?}"
        );
        for (index, version) in reported.iter().enumerate() {
            assert_eq!(
                version["Name"].as_str(),
                Some(*name),
                "case {case}: every entry names the document, got {version}"
            );
            assert_eq!(
                version["Status"].as_str(),
                Some("Active"),
                "case {case}: a published revision is Active, got {version}"
            );
            assert_eq!(
                version["IsDefaultVersion"],
                json!(index == 0),
                "case {case}: only the newest revision is the default one, got {version}"
            );
        }
        // The newest revision is the one a read naming no version resolves to.
        let document = described(&router, case, name).await;
        assert_eq!(
            document["DocumentVersion"].as_str(),
            reported[0]["DocumentVersion"].as_str(),
            "case {case}: a read naming no version resolves the newest revision, got {document}"
        );
    }
}
