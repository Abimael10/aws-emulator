// SPDX-License-Identifier: CC0-1.0

//! The KMS key a request names is reported by the ARN of the KEY it names, whichever of
//! the spellings `KmsKeyId` admits named it.
//!
//! THE RULE: every member this service takes a customer managed key through is a
//! `KmsKeyId`, whose `@pattern` admits four spellings of one key — a key ARN, a bare
//! 36-character key id, `alias/<name>` and an alias ARN
//! (`smithy_query(query="shape",shape="KmsKeyId")`) — while every member a read reports
//! that key through is a `KmsKeyArn`, whose `@pattern` admits
//! `arn:aws…:kms:<region>:<account>:key/<id>` and nothing else
//! (`smithy_query(query="shape",shape="KmsKeyArn")`). So a read reports the ARN of the key
//! the request named, and an ALIAS is resolved to the key it targets — the member has no
//! form that could carry the alias itself.
//!
//! Which key an alias targets is a fact AWS KMS holds, so AWS KMS is mounted BESIDE this
//! control plane on one engine, exactly as the assembled router mounts them, and every key
//! and alias a row names was minted through KMS's OWN API
//! (`CreateKey`, `CreateAlias`). An alias KMS holds no key for resolves to nothing, so the
//! optional member is omitted rather than carrying an invented ARN.
//!
//! The rows differ along what the rule generalises over — which spelling names the key,
//! which of two distinct keys it names, and whether anything answers to it — and the same
//! table is driven through the two paths that write the member (`CreateGuardrail` and
//! `UpdateGuardrail`) plus one operation of another family (`CreateModelImportJob`), since
//! the rule is about the member and not about one operation.

use serde_json::{json, Value};

use crate::support::{emulator_with_kms, label, Emulator, Reply, ACCOUNT, REGION};

/// The signing name AWS KMS claims its requests under, and the `x-amz-target` prefix its
/// awsJson1.1 operations are addressed by (`TrentService`, this model's service shape).
const KMS_SIGNING_NAME: &str = "kms";

/// One row: the case, the `kmsKeyId` the request carries, and the `kmsKeyArn` a read must
/// report for it.
struct Row {
    case: &'static str,
    kms_key_id: Option<String>,
    expected: Value,
}

/// A key AWS KMS minted: the key id it issued and the key ARN it reports for it.
struct Key {
    id: String,
    arn: String,
}

/// Mint a key through AWS KMS's own `CreateKey`, answering the key id and key ARN KMS
/// reports for it.
async fn create_key(emu: &Emulator, case: &str) -> Key {
    let reply = emu
        .call_awsjson(KMS_SIGNING_NAME, "TrentService.CreateKey", json!({}))
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: CreateKey must answer 200, got {} {:?}",
        reply.status, reply.body
    );
    let metadata = reply.get("KeyMetadata");
    let id = metadata
        .get("KeyId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reported no KeyId: {metadata:?}"))
        .to_string();
    let arn = metadata
        .get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reported no Arn: {metadata:?}"))
        .to_string();
    Key { id, arn }
}

/// Point the alias `alias_name` at the key `target` through AWS KMS's own `CreateAlias`.
async fn create_alias(emu: &Emulator, case: &str, alias_name: &str, target: &str) {
    let reply = emu
        .call_awsjson(
            KMS_SIGNING_NAME,
            "TrentService.CreateAlias",
            json!({ "AliasName": alias_name, "TargetKeyId": target }),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: CreateAlias must answer 200, got {} {:?}",
        reply.status, reply.body
    );
}

/// The alias ARN of `alias_name` at this Region's endpoint, which is the other spelling
/// `KmsKeyId` admits for an alias.
fn alias_arn(alias_name: &str) -> String {
    format!("arn:aws:kms:{REGION}:{ACCOUNT}:{alias_name}")
}

/// The table: one key named every way `KmsKeyId` admits, a SECOND key so an alias is shown
/// to resolve to the key it targets rather than to any key the account holds, an alias
/// nothing answers to, and no key at all.
async fn rows(emu: &Emulator) -> Vec<Row> {
    let first = create_key(emu, "first key").await;
    let second = create_key(emu, "second key").await;
    // Alias names inside `KmsKeyId`'s own `alias/[a-zA-Z0-9-_/]+` alternative: a plain one
    // and one carrying the separators that alternative admits.
    let first_alias = "alias/bedrock-first-key";
    let second_alias = "alias/bedrock/second_key";
    create_alias(emu, "first alias", first_alias, &first.id).await;
    create_alias(emu, "second alias", second_alias, &second.id).await;

    vec![
        Row {
            case: "no key at all",
            kms_key_id: None,
            expected: Value::Null,
        },
        Row {
            case: "the first key's ARN",
            kms_key_id: Some(first.arn.clone()),
            expected: json!(first.arn.clone()),
        },
        Row {
            case: "the second key's bare key id",
            kms_key_id: Some(second.id.clone()),
            expected: json!(second.arn.clone()),
        },
        Row {
            case: "an alias name naming the first key",
            kms_key_id: Some(first_alias.to_string()),
            expected: json!(first.arn.clone()),
        },
        Row {
            case: "an alias ARN naming the first key",
            kms_key_id: Some(alias_arn(first_alias)),
            expected: json!(first.arn.clone()),
        },
        Row {
            case: "a nested alias name naming the second key",
            kms_key_id: Some(second_alias.to_string()),
            expected: json!(second.arn.clone()),
        },
        Row {
            case: "an alias ARN naming the second key",
            kms_key_id: Some(alias_arn(second_alias)),
            expected: json!(second.arn.clone()),
        },
        Row {
            case: "an alias AWS KMS holds no key for",
            kms_key_id: Some("alias/bedrock-no-such-key".to_string()),
            expected: Value::Null,
        },
        Row {
            case: "an alias ARN AWS KMS holds no key for",
            kms_key_id: Some(alias_arn("alias/bedrock-no-such-key")),
            expected: Value::Null,
        },
    ]
}

/// A `CreateGuardrail` / `UpdateGuardrail` body carrying the required members, plus the
/// `kmsKeyId` of a row when it names one.
fn guardrail_body(name: &str, kms_key_id: Option<&str>) -> Value {
    let mut body = json!({
        "name": name,
        "blockedInputMessaging": "input blocked",
        "blockedOutputsMessaging": "output blocked",
    });
    if let Some(kms_key_id) = kms_key_id {
        body["kmsKeyId"] = json!(kms_key_id);
    }
    body
}

// ---------------------------------------------------------------------------------------
// 1. The key a CREATE names is the key the read reports.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_created_guardrail_reports_the_key_its_kms_key_id_names() {
    let emu = emulator_with_kms();
    for (index, row) in rows(&emu).await.into_iter().enumerate() {
        let created = emu
            .create(guardrail_body(
                &format!("created-{index}"),
                row.kms_key_id.as_deref(),
            ))
            .await;
        assert_eq!(
            created.status, 202,
            "case {:?}: CreateGuardrail must answer 202, got {} {:?}",
            row.case, created.status, created.body
        );
        let read = emu.get(&created.text("guardrailId"), None).await;
        assert_eq!(
            read.get("kmsKeyArn"),
            row.expected,
            "case {:?}: expected kmsKeyArn {:?}, got {:?}",
            row.case,
            row.expected,
            read.get("kmsKeyArn")
        );
    }
}

// ---------------------------------------------------------------------------------------
// 2. The key an UPDATE names is the key the read reports.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_updated_guardrail_reports_the_key_its_kms_key_id_names() {
    let emu = emulator_with_kms();
    for (index, row) in rows(&emu).await.into_iter().enumerate() {
        let name = format!("updated-{index}");
        // Created with NO key, so what the read reports afterwards can only have come from
        // the update.
        let created = emu.create(guardrail_body(&name, None)).await;
        assert_eq!(
            created.status, 202,
            "case {:?}: CreateGuardrail must answer 202, got {} {:?}",
            row.case, created.status, created.body
        );
        let id = created.text("guardrailId");
        let before = emu.get(&id, None).await;
        assert_eq!(
            before.get("kmsKeyArn"),
            Value::Null,
            "case {:?}: a guardrail created with no key must report none, got {:?}",
            row.case,
            before.get("kmsKeyArn")
        );

        let updated = emu
            .update(&id, guardrail_body(&name, row.kms_key_id.as_deref()))
            .await;
        assert_eq!(
            updated.status, 202,
            "case {:?}: UpdateGuardrail must answer 202, got {} {:?}",
            row.case, updated.status, updated.body
        );
        let read = emu.get(&id, None).await;
        assert_eq!(
            read.get("kmsKeyArn"),
            row.expected,
            "case {:?}: expected kmsKeyArn {:?} after the update, got {:?}",
            row.case,
            row.expected,
            read.get("kmsKeyArn")
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3. The same resolution serves another resource family: a model import job's
//    `importedModelKmsKeyId` is reported as `importedModelKmsKeyArn`.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_import_job_reports_the_key_its_kms_key_id_names() {
    let emu = emulator_with_kms();
    for (index, row) in rows(&emu).await.into_iter().enumerate() {
        let mut body = json!({
            "jobName": format!("import-{index}"),
            "importedModelName": format!("model-{index}"),
            "roleArn": "arn:aws:iam::000000000000:role/bedrock-model-import",
            "modelDataSource": { "s3DataSource": { "s3Uri": "s3://imported-models/weights" } },
        });
        if let Some(kms_key_id) = row.kms_key_id.as_deref() {
            body["importedModelKmsKeyId"] = json!(kms_key_id);
        }
        let created: Reply = emu.call("POST", "/model-import-jobs", Some(body)).await;
        assert_eq!(
            created.status, 201,
            "case {:?}: CreateModelImportJob must answer 201, got {} {:?}",
            row.case, created.status, created.body
        );
        let job_arn = created.text("jobArn");
        let read = emu
            .call(
                "GET",
                &format!("/model-import-jobs/{}", label(&job_arn)),
                None,
            )
            .await;
        assert_eq!(
            read.get("importedModelKmsKeyArn"),
            row.expected,
            "case {:?}: expected importedModelKmsKeyArn {:?}, got {:?}",
            row.case,
            row.expected,
            read.get("importedModelKmsKeyArn")
        );
    }
}
