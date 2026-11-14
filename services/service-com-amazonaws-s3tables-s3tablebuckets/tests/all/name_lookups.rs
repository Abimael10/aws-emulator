// SPDX-License-Identifier: CC0-1.0

//! How S3 Tables answers a namespace or table name that is outside the
//! character set and length the model declares for it.
//!
//! The rule under test: a name member a request uses to ADDRESS an existing
//! resource is a lookup key — the service resolves it against what the account
//! holds and answers `NotFoundException` (404) with the one fixed sentence for
//! that resource class when nothing is stored under it, whatever characters the
//! name is spelled with — while the modelled name format
//! (`NamespaceName`/`TableName`: `@length(1, 255)`, `@pattern(^[0-9a-z_]*$)`,
//! `smithy_query(query="shape", shape="NamespaceName,TableName")`) gates only
//! the members that NAME a resource being created or renamed, which are refused
//! with `BadRequestException` (400) and that class's fixed sentence.
//!
//! The rows span what the rule generalises over: which operation carries the
//! member (a read, a list, a delete, a create's namespace, a rename's current
//! coordinates, a sub-resource read, a write), which name member it addresses
//! (a namespace, a table), and HOW the name leaves the modelled format (an
//! uppercase letter, a hyphen, a dot, past the maximum length). The accepted
//! rows keep it honest: a conforming name that IS resident still resolves, and a
//! conforming name that is not is the same 404 as a non-conforming one.

use serde_json::{json, Value};

use crate::harness::{encode, query_string, Emulator, Reply};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The fixed sentences, spelled out here rather than read from the crate, so
/// this suite pins the wording itself.
const NAMESPACE_NOT_FOUND: &str = "The specified namespace does not exist.";
const TABLE_NOT_FOUND: &str = "The specified table does not exist.";
const INVALID_NAMESPACE_NAME: &str =
    "The specified namespace name isn’t valid. Specify a different namespace name, and then try \
     again.";
const INVALID_TABLE_NAME: &str = "The specified table name is not valid.";

/// The namespace and the table one bucket of this suite holds, under names the
/// model's format admits.
const RESIDENT_NAMESPACE: &str = "analytics";
const RESIDENT_TABLE: &str = "user_events";

/// Names outside the modelled format, one per way of leaving it. None of them
/// can be created, so none of them is ever stored.
fn unstorable_names() -> Vec<String> {
    vec![
        "Analytics".to_string(),
        "has-hyphen".to_string(),
        "with.dot".to_string(),
        "n".repeat(256),
    ]
}

/// A bucket holding one namespace and one table, and its ARN.
async fn resident_bucket(emulator: &Emulator, bucket: &str) -> String {
    let created = emulator
        .create_table_bucket(REGION, &json!({ "name": bucket }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating the bucket {bucket:?} must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let arn = created.text("arn");
    let namespace = emulator
        .create_namespace(REGION, &arn, &json!({ "namespace": [RESIDENT_NAMESPACE] }))
        .await;
    assert_eq!(
        namespace.status, 200,
        "creating the namespace must succeed, got {status} {body}",
        status = namespace.status,
        body = namespace.body
    );
    let table = emulator
        .create_table(
            REGION,
            &arn,
            RESIDENT_NAMESPACE,
            &json!({ "name": RESIDENT_TABLE, "format": "ICEBERG" }),
        )
        .await;
    assert_eq!(
        table.status, 200,
        "creating the table must succeed, got {status} {body}",
        status = table.status,
        body = table.body
    );
    arn
}

/// Assert `reply` is the refusal `(status, code, message)` names.
fn assert_refusal(reply: &Reply, status: u16, code: &str, message: &str, case: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected status {status}, got {actual} {body}",
        actual = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(code),
        "case {case:?}: expected error type {code}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected the fixed sentence, got {actual:?}",
        actual = reply.message()
    );
}

/// Every operation that ADDRESSES a namespace by name, as `(label, method,
/// path, body)` for the namespace `namespace`.
fn namespace_lookups(arn: &str, namespace: &str) -> Vec<(String, &'static str, String, Option<Value>)> {
    vec![
        (
            format!("GetNamespace on {namespace:?}"),
            "GET",
            format!("/namespaces/{}/{}", encode(arn), encode(namespace)),
            None,
        ),
        (
            format!("DeleteNamespace on {namespace:?}"),
            "DELETE",
            format!("/namespaces/{}/{}", encode(arn), encode(namespace)),
            None,
        ),
        (
            format!("ListTables in {namespace:?}"),
            "GET",
            format!(
                "/tables/{}{}",
                encode(arn),
                query_string(&[("namespace", namespace.to_string())])
            ),
            None,
        ),
        (
            format!("CreateTable in {namespace:?}"),
            "PUT",
            format!("/tables/{}/{}", encode(arn), encode(namespace)),
            Some(json!({ "name": "landing", "format": "ICEBERG" })),
        ),
    ]
}

/// Every operation that ADDRESSES a table by name, as `(label, method, path,
/// body)` for the table `name` in the resident namespace.
fn table_lookups(arn: &str, name: &str) -> Vec<(String, &'static str, String, Option<Value>)> {
    let table_path = |suffix: &str| {
        format!(
            "/tables/{}/{}/{}{suffix}",
            encode(arn),
            encode(RESIDENT_NAMESPACE),
            encode(name)
        )
    };
    vec![
        (
            format!("GetTable on {name:?}"),
            "GET",
            format!(
                "/get-table{}",
                query_string(&[
                    ("tableBucketARN", arn.to_string()),
                    ("namespace", RESIDENT_NAMESPACE.to_string()),
                    ("name", name.to_string()),
                ])
            ),
            None,
        ),
        (
            format!("DeleteTable on {name:?}"),
            "DELETE",
            table_path(""),
            None,
        ),
        (
            format!("GetTableMetadataLocation on {name:?}"),
            "GET",
            table_path("/metadata-location"),
            None,
        ),
        (
            format!("GetTableEncryption on {name:?}"),
            "GET",
            table_path("/encryption"),
            None,
        ),
        (
            format!("GetTableStorageClass on {name:?}"),
            "GET",
            table_path("/storage-class"),
            None,
        ),
        (
            format!("GetTableMaintenanceConfiguration on {name:?}"),
            "GET",
            table_path("/maintenance"),
            None,
        ),
        (
            format!("RenameTable from {name:?}"),
            "PUT",
            table_path("/rename"),
            Some(json!({ "newName": "renamed_table" })),
        ),
        (
            format!("UpdateTableMetadataLocation on {name:?}"),
            "PUT",
            table_path("/metadata-location"),
            Some(json!({
                "metadataLocation": "s3://whatever/metadata/0.metadata.json",
                "versionToken": "a-token-of-the-callers-view",
            })),
        ),
    ]
}

#[tokio::test]
async fn a_namespace_a_lookup_addresses_is_resolved_and_not_format_checked() {
    let emulator = Emulator::new();
    let arn = resident_bucket(&emulator, "namespace-lookup-keys").await;

    // A name the format rules out is a name nothing was ever created under, so
    // every operation that ADDRESSES a namespace by it reports the namespace as
    // absent — and a conforming name that was never created is the same answer,
    // which is what makes the first a lookup rather than a format check.
    let mut addressed = unstorable_names();
    addressed.push("never_created".to_string());
    for namespace in &addressed {
        for (label, method, path, body) in namespace_lookups(&arn, namespace) {
            let reply = emulator.call(method, &path, REGION, body.as_ref()).await;
            assert_refusal(
                &reply,
                404,
                "NotFoundException",
                NAMESPACE_NOT_FOUND,
                &label,
            );
        }
    }

    // The resident namespace is still resolved by every one of them, so the
    // layer in front of the lookups did not take the ordinary path away. The
    // read is the row that must answer 200; the others change state, so only
    // their refusal class is of interest above.
    let resident = emulator
        .get_namespace(REGION, &arn, RESIDENT_NAMESPACE)
        .await;
    assert_eq!(
        resident.status, 200,
        "case \"the resident namespace\": a stored namespace must still resolve, got {status} \
         {body}",
        status = resident.status,
        body = resident.body
    );
    let listed = emulator
        .list_tables(
            REGION,
            &arn,
            &[("namespace", RESIDENT_NAMESPACE.to_string())],
        )
        .await;
    assert_eq!(
        listed.status, 200,
        "case \"the resident namespace\": its tables must still list, got {status} {body}",
        status = listed.status,
        body = listed.body
    );
}

#[tokio::test]
async fn a_table_a_lookup_addresses_is_resolved_and_not_format_checked() {
    let emulator = Emulator::new();
    let arn = resident_bucket(&emulator, "table-lookup-keys").await;

    let mut addressed = unstorable_names();
    addressed.push("never_created".to_string());
    for name in &addressed {
        for (label, method, path, body) in table_lookups(&arn, name) {
            let reply = emulator.call(method, &path, REGION, body.as_ref()).await;
            assert_refusal(&reply, 404, "NotFoundException", TABLE_NOT_FOUND, &label);
        }
    }

    // The resident table is still read back, under both addressing modes.
    let resident = emulator
        .get_table(REGION, &arn, RESIDENT_NAMESPACE, RESIDENT_TABLE)
        .await;
    assert_eq!(
        resident.status, 200,
        "case \"the resident table\": a stored table must still resolve, got {status} {body}",
        status = resident.status,
        body = resident.body
    );
}

#[tokio::test]
async fn the_name_format_still_gates_the_creates_and_the_rename() {
    let emulator = Emulator::new();
    let arn = resident_bucket(&emulator, "create-paths-still-gated").await;

    for name in unstorable_names() {
        // The namespace a create NAMES is refused for its format, not looked up.
        let namespace = emulator
            .create_namespace(REGION, &arn, &json!({ "namespace": [name.clone()] }))
            .await;
        assert_refusal(
            &namespace,
            400,
            "BadRequestException",
            INVALID_NAMESPACE_NAME,
            &format!("CreateNamespace naming {name:?}"),
        );

        // So is the table name a create names, even though the namespace it is
        // created in is resolved.
        let table = emulator
            .create_table(
                REGION,
                &arn,
                RESIDENT_NAMESPACE,
                &json!({ "name": name.clone(), "format": "ICEBERG" }),
            )
            .await;
        assert_refusal(
            &table,
            400,
            "BadRequestException",
            INVALID_TABLE_NAME,
            &format!("CreateTable naming {name:?}"),
        );

        // And so are both coordinates a rename MOVES a table to, although the
        // ones it moves the table FROM are looked up.
        let renamed = emulator
            .rename_table(
                REGION,
                &arn,
                RESIDENT_NAMESPACE,
                RESIDENT_TABLE,
                &json!({ "newName": name.clone() }),
            )
            .await;
        assert_refusal(
            &renamed,
            400,
            "BadRequestException",
            INVALID_TABLE_NAME,
            &format!("RenameTable to the name {name:?}"),
        );
        let moved = emulator
            .rename_table(
                REGION,
                &arn,
                RESIDENT_NAMESPACE,
                RESIDENT_TABLE,
                &json!({ "newNamespaceName": name.clone() }),
            )
            .await;
        assert_refusal(
            &moved,
            400,
            "BadRequestException",
            INVALID_NAMESPACE_NAME,
            &format!("RenameTable into the namespace {name:?}"),
        );
    }

    // A create whose namespace AND table name are both outside the format is
    // still a client-input refusal of a name it was asked to create: WHICH of
    // the two the refusal names is not a rule this suite pins, only that the
    // request is refused rather than resolved.
    let both = emulator
        .call(
            "PUT",
            &format!("/tables/{}/{}", encode(&arn), encode("Analytics")),
            REGION,
            Some(&json!({ "name": "Landing-Zone", "format": "ICEBERG" })),
        )
        .await;
    let case = "CreateTable naming an unstorable table in an unstorable namespace";
    assert_eq!(
        both.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = both.status,
        body = both.body
    );
    assert_eq!(
        both.error_code.as_deref(),
        Some("BadRequestException"),
        "case {case:?}: expected BadRequestException, got {actual:?}",
        actual = both.error_code
    );
    assert!(
        [INVALID_TABLE_NAME, INVALID_NAMESPACE_NAME].contains(&both.message().as_str()),
        "case {case:?}: expected one of the two name sentences, got {actual:?}",
        actual = both.message()
    );
}
