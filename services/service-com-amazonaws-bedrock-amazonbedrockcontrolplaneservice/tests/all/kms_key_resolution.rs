// SPDX-License-Identifier: CC0-1.0

//! The KMS key a request names through a `KmsKeyId` member: WHICH key it is, and whether
//! AWS KMS holds one under that identifier at all.
//!
//! RULE 1 — the key a member REPORTS. Every member this service takes a customer managed key
//! through is a `KmsKeyId`, whose `@pattern` admits four spellings of one key — a key ARN, a
//! bare 36-character key id, `alias/<name>` and an alias ARN
//! (`smithy_query(query="shape",shape="KmsKeyId")`) — while every member a read reports that
//! key through is a `KmsKeyArn`, whose `@pattern` admits
//! `arn:aws…:kms:<region>:<account>:key/<id>` and nothing else
//! (`smithy_query(query="shape",shape="KmsKeyArn")`). So a read reports the ARN of the key
//! the request named, and an ALIAS is resolved to the key it targets — the member has no
//! form that could carry the alias itself.
//!
//! RULE 2 — the key a member NAMES has to exist. `@length` and `@pattern` constrain only the
//! SHAPE of the identifier, so whether it names a key at all is a pre-condition on a resource
//! AWS KMS owns rather than a boundary check: a write naming a key AWS KMS holds none of is
//! refused with the modelled `ValidationException` (`@httpError(400)`,
//! `smithy_query(query="operation",operation="CreateGuardrail,UpdateGuardrail,CreateAutomatedReasoningPolicy")`)
//! stating `The kmsKeyId provided is invalid.`, and the refusal is EFFECT-FREE: the create
//! mints nothing and the update leaves the stored key, the name and `updatedAt` as they
//! stood. A client configuring customer-managed-key encryption has that one call to correct a
//! mistyped or deleted key, so the refusal belongs there rather than on every later read.
//!
//! Which keys and aliases exist is a fact AWS KMS holds, so AWS KMS is mounted BESIDE this
//! control plane on one engine, exactly as the assembled router mounts them, and every key
//! and alias a row names was minted through KMS's OWN API (`CreateKey`, `CreateAlias`). The
//! closing test pins the GROUNDING half of rule 2: a refusal rests on what AWS KMS ANSWERED,
//! so an emulator assembled with no KMS to ask admits the very write the table refuses.
//!
//! The rows differ along what the rules generalise over — which spelling names the key, which
//! of two distinct keys it names, and why nothing answers to it (an alias nothing was pointed
//! at, a key never created, the right key id in a Region that holds no key) — and the one
//! table is driven through every path that writes such a member: `CreateGuardrail`,
//! `UpdateGuardrail`, `CreateAutomatedReasoningPolicy` (the other operation whose member is
//! `kmsKeyId`) and `CreateModelImportJob`, whose own member is `importedModelKmsKeyId` and
//! which therefore records what it was given.

use serde_json::{json, Value};

use crate::support::{emulator, emulator_with_kms, label, Emulator, Reply, ACCOUNT, REGION};

/// The signing name AWS KMS claims its requests under, and the `x-amz-target` prefix its
/// awsJson1.1 operations are addressed by (`TrentService`, this model's service shape).
const KMS_SIGNING_NAME: &str = "kms";

/// The sentence Amazon Bedrock refuses a `kmsKeyId` naming no key with: the FIXED text of its
/// error class, naming the member and interpolating nothing — not the identifier submitted,
/// not the resource being written and not the Region.
const INVALID_KMS_KEY_ID: &str = "The kmsKeyId provided is invalid.";

/// A key id of the form AWS KMS issues — the 36 characters `KmsKeyId`'s pattern spells as
/// `[a-zA-Z0-9-]{36}` — that no `CreateKey` here ever minted.
const NEVER_CREATED_KEY_ID: &str = "2b1c0f3a-55d4-4e8a-9f0b-7c6d5e4a3b21";

/// An alias name inside `KmsKeyId`'s own `alias/[a-zA-Z0-9-_/]+` alternative that no
/// `CreateAlias` here ever pointed at a key.
const NEVER_CREATED_ALIAS: &str = "alias/bedrock-no-such-key";

/// A Region this suite mints no key in, so a key it DOES hold is named by no identifier
/// scoped to that Region.
const OTHER_REGION: &str = "us-east-1";

/// One row: the case, the `kmsKeyId` the request carries, the `KmsKeyArn` a member that
/// RECORDS what it was given reports for it, and whether AWS KMS answers for a key under it.
struct Row {
    case: &'static str,
    kms_key_id: Option<String>,
    /// Rule 1: the key ARN a read reports for this identifier.
    expected: Value,
    /// Rule 2: whether AWS KMS holds a key this identifier names. The paths that take the
    /// `kmsKeyId` member REFUSE a row this is false for; the ones taking a differently named
    /// member record what they were given, which is [`Row::expected`].
    resolves: bool,
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

/// The key ARN of `key_id` at `region`'s endpoint, the one form `KmsKeyArn` admits.
fn key_arn(region: &str, key_id: &str) -> String {
    format!("arn:aws:kms:{region}:{ACCOUNT}:key/{key_id}")
}

/// The table: one key named every way `KmsKeyId` admits, a SECOND key so an alias is shown
/// to resolve to the key it targets rather than to any key the account holds, every reason
/// AWS KMS answers for no key under an identifier, and no key at all.
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
            resolves: true,
        },
        Row {
            case: "the first key's ARN",
            kms_key_id: Some(first.arn.clone()),
            expected: json!(first.arn.clone()),
            resolves: true,
        },
        Row {
            case: "the second key's bare key id",
            kms_key_id: Some(second.id.clone()),
            expected: json!(second.arn.clone()),
            resolves: true,
        },
        Row {
            case: "an alias name naming the first key",
            kms_key_id: Some(first_alias.to_string()),
            expected: json!(first.arn.clone()),
            resolves: true,
        },
        Row {
            case: "an alias ARN naming the first key",
            kms_key_id: Some(alias_arn(first_alias)),
            expected: json!(first.arn.clone()),
            resolves: true,
        },
        Row {
            case: "a nested alias name naming the second key",
            kms_key_id: Some(second_alias.to_string()),
            expected: json!(second.arn.clone()),
            resolves: true,
        },
        Row {
            case: "an alias ARN naming the second key",
            kms_key_id: Some(alias_arn(second_alias)),
            expected: json!(second.arn.clone()),
            resolves: true,
        },
        Row {
            case: "an alias AWS KMS holds no key for",
            kms_key_id: Some(NEVER_CREATED_ALIAS.to_string()),
            expected: Value::Null,
            resolves: false,
        },
        Row {
            case: "an alias ARN AWS KMS holds no key for",
            kms_key_id: Some(alias_arn(NEVER_CREATED_ALIAS)),
            expected: Value::Null,
            resolves: false,
        },
        Row {
            case: "a well-formed key ARN of a key never created",
            kms_key_id: Some(key_arn(REGION, NEVER_CREATED_KEY_ID)),
            expected: json!(key_arn(REGION, NEVER_CREATED_KEY_ID)),
            resolves: false,
        },
        Row {
            case: "a bare key id of a key never created",
            kms_key_id: Some(NEVER_CREATED_KEY_ID.to_string()),
            expected: json!(key_arn(REGION, NEVER_CREATED_KEY_ID)),
            resolves: false,
        },
        Row {
            case: "the first key's id in a Region that holds no key",
            kms_key_id: Some(key_arn(OTHER_REGION, &first.id)),
            expected: json!(key_arn(OTHER_REGION, &first.id)),
            resolves: false,
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

/// Assert `reply` is the refusal rule 2 calls for: HTTP 400, the modelled
/// `ValidationException`, and the one sentence that error class states.
fn assert_refused(case: &str, operation: &str, reply: &Reply) {
    let message = reply
        .get("message")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        (reply.status, reply.error.as_deref(), message.as_str()),
        (400, Some("ValidationException"), INVALID_KMS_KEY_ID),
        "case {case:?}: {operation} must refuse a key AWS KMS holds none of, got {} {:?} {:?}",
        reply.status,
        reply.error,
        reply.body
    );
}

/// The `name` of every guardrail `ListGuardrails` reports, sorted, so what a refused create
/// left behind is comparable whatever order the listing walks.
async fn listed_guardrail_names(emu: &Emulator) -> Vec<String> {
    let listed = emu.list("").await;
    let summaries = listed.get("guardrails");
    let mut names: Vec<String> = summaries
        .as_array()
        .unwrap_or_else(|| {
            panic!("ListGuardrails reports a list of guardrails, got {summaries:?}")
        })
        .iter()
        .map(|summary| {
            summary
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("every guardrail summary carries a name, got {summary:?}")
                })
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// The `name` of every Automated Reasoning policy the listing reports, sorted.
async fn listed_policy_names(emu: &Emulator) -> Vec<String> {
    let listed = emu.call("GET", "/automated-reasoning-policies", None).await;
    let summaries = listed.get("automatedReasoningPolicySummaries");
    let mut names: Vec<String> = summaries
        .as_array()
        .unwrap_or_else(|| {
            panic!("ListAutomatedReasoningPolicies reports a list of summaries, got {summaries:?}")
        })
        .iter()
        .map(|summary| {
            summary
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("every policy summary carries a name, got {summary:?}"))
                .to_string()
        })
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------------------
// 1. The key a CREATE names is the key the read reports, and a key AWS KMS holds none of
//    is refused.
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
        if !row.resolves {
            // Rule 2: AWS KMS answers for no key under this identifier, so there is no
            // guardrail to read — what the refusal left behind is asserted below in
            // `a_refused_guardrail_create_mints_nothing`.
            assert_refused(row.case, "CreateGuardrail", &created);
            continue;
        }
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
// 2. The key an UPDATE names is the key the read reports, and a key AWS KMS holds none of
//    is refused.
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
        if !row.resolves {
            // Rule 2 on the update path: the identifier is refused and the guardrail keeps
            // the key it had, which here is none.
            assert_refused(row.case, "UpdateGuardrail", &updated);
            assert_eq!(
                emu.get(&id, None).await.get("kmsKeyArn"),
                Value::Null,
                "case {:?}: a refused update must leave the guardrail without a key",
                row.case
            );
            continue;
        }
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

// ---------------------------------------------------------------------------------------
// 4. A refused CREATE mints nothing.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_guardrail_create_mints_nothing() {
    let emu = emulator_with_kms();
    let mut admitted: Vec<String> = Vec::new();

    for (index, row) in rows(&emu).await.into_iter().enumerate() {
        let name = format!("minted-{index}");
        let created = emu
            .create(guardrail_body(&name, row.kms_key_id.as_deref()))
            .await;
        if row.resolves {
            assert_eq!(
                created.status, 202,
                "case {:?}: CreateGuardrail must answer 202, got {} {:?}",
                row.case, created.status, created.body
            );
            admitted.push(name);
            continue;
        }
        assert_refused(row.case, "CreateGuardrail", &created);
        assert_eq!(
            created.get("guardrailId"),
            Value::Null,
            "case {:?}: a refused create mints no guardrail id, got {:?}",
            row.case,
            created.body
        );
    }

    admitted.sort();
    assert_eq!(
        listed_guardrail_names(&emu).await,
        admitted,
        "a refused create must leave no guardrail behind, so the listing holds exactly the \
         admitted rows"
    );
}

// ---------------------------------------------------------------------------------------
// 5. A refused UPDATE moves nothing the update would have written.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_guardrail_update_moves_neither_the_stored_key_nor_updated_at() {
    let emu = emulator_with_kms();
    let rows = rows(&emu).await;
    // The guardrail under test is created with a key AWS KMS DOES hold, so what a refused
    // update must preserve is a stored key rather than the absence of one.
    let held = rows
        .iter()
        .find(|row| row.resolves && row.kms_key_id.is_some())
        .expect("the table names at least one key AWS KMS holds");
    let name = "held-key-guardrail";

    let created = emu
        .create(guardrail_body(name, held.kms_key_id.as_deref()))
        .await;
    assert_eq!(
        created.status, 202,
        "case {:?}: CreateGuardrail must answer 202, got {} {:?}",
        held.case, created.status, created.body
    );
    let id = created.text("guardrailId");
    let before = emu.get(&id, None).await;
    assert_eq!(
        before.get("kmsKeyArn"),
        held.expected,
        "case {:?}: the guardrail must report the key it was created with, got {:?}",
        held.case,
        before.get("kmsKeyArn")
    );
    let stored_updated_at = before.get("updatedAt");

    for (index, row) in rows.iter().filter(|row| !row.resolves).enumerate() {
        // The refused update also asks for a new name and a description, so what the refusal
        // leaves untouched covers every member the update writes rather than the key alone.
        let mut body = guardrail_body(&format!("renamed-{index}"), row.kms_key_id.as_deref());
        body["description"] = json!("a description a refused update must not store");
        let refused = emu.update(&id, body).await;
        assert_refused(row.case, "UpdateGuardrail", &refused);

        let after = emu.get(&id, None).await;
        assert_eq!(
            (
                after.get("kmsKeyArn"),
                after.get("updatedAt"),
                after.get("name"),
                after.get("description"),
            ),
            (
                held.expected.clone(),
                stored_updated_at.clone(),
                json!(name),
                Value::Null,
            ),
            "case {:?}: a refused update must leave the guardrail as it stood, got {:?}",
            row.case,
            after.body
        );
    }

    // The positive control, on the same guardrail after every refusal: an identifier AWS KMS
    // does answer for is accepted and the read reports the key it names.
    let accepted = emu
        .update(&id, guardrail_body(name, held.kms_key_id.as_deref()))
        .await;
    assert_eq!(
        accepted.status, 202,
        "case {:?}: UpdateGuardrail must answer 202 after the refusals, got {} {:?}",
        held.case, accepted.status, accepted.body
    );
    assert_eq!(
        emu.get(&id, None).await.get("kmsKeyArn"),
        held.expected,
        "case {:?}: the accepted update must report the key it named",
        held.case
    );
}

// ---------------------------------------------------------------------------------------
// 6. The other operation taking the `kmsKeyId` member answers the same way.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_automated_reasoning_policy_create_resolves_its_kms_key_id() {
    let emu = emulator_with_kms();
    let mut admitted: Vec<String> = Vec::new();

    for (index, row) in rows(&emu).await.into_iter().enumerate() {
        let name = format!("policy-{index}");
        let mut body = json!({ "name": name.clone() });
        if let Some(kms_key_id) = row.kms_key_id.as_deref() {
            body["kmsKeyId"] = json!(kms_key_id);
        }
        let created = emu
            .call("POST", "/automated-reasoning-policies", Some(body))
            .await;
        if !row.resolves {
            assert_refused(row.case, "CreateAutomatedReasoningPolicy", &created);
            assert_eq!(
                created.get("policyArn"),
                Value::Null,
                "case {:?}: a refused create mints no policy ARN, got {:?}",
                row.case,
                created.body
            );
            continue;
        }
        assert_eq!(
            created.status, 200,
            "case {:?}: CreateAutomatedReasoningPolicy must answer 200, got {} {:?}",
            row.case, created.status, created.body
        );
        let read = emu
            .call(
                "GET",
                &format!(
                    "/automated-reasoning-policies/{}",
                    label(&created.text("policyArn"))
                ),
                None,
            )
            .await;
        assert_eq!(
            read.get("kmsKeyArn"),
            row.expected,
            "case {:?}: expected kmsKeyArn {:?}, got {:?}",
            row.case,
            row.expected,
            read.get("kmsKeyArn")
        );
        admitted.push(name);
    }

    admitted.sort();
    assert_eq!(
        listed_policy_names(&emu).await,
        admitted,
        "a refused create must leave no policy behind, so the listing holds exactly the \
         admitted rows"
    );
}

// ---------------------------------------------------------------------------------------
// 7. The grounding half of rule 2: a refusal rests on what AWS KMS answered.
// ---------------------------------------------------------------------------------------

/// An emulator assembled with no AWS KMS to ask establishes nothing about a key, so it admits
/// the very identifier the table refuses — the bar every cross-service resolution of this
/// service is held to.
#[tokio::test]
async fn a_key_no_kms_can_be_asked_about_is_admitted() {
    let emu = emulator();
    let unresolvable = key_arn(REGION, NEVER_CREATED_KEY_ID);
    let created = emu
        .create(guardrail_body("no-kms-to-ask", Some(&unresolvable)))
        .await;
    assert_eq!(
        (created.status, created.error.as_deref()),
        (202, None),
        "case \"no KMS to ask\": an ungrounded refusal must not be made, got {} {:?} {:?}",
        created.status,
        created.error,
        created.body
    );
    let read = emu.get(&created.text("guardrailId"), None).await;
    assert_eq!(
        read.get("kmsKeyArn"),
        json!(unresolvable),
        "case \"no KMS to ask\": the guardrail must report the key it was given, got {:?}",
        read.get("kmsKeyArn")
    );
}
