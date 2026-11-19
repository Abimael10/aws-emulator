// SPDX-License-Identifier: CC0-1.0

//! Pointing a CodeCommit repository at a KMS key consults that KEY's state, driven over
//! the assembled `MultiServiceRouter` with CodeCommit and KMS mounted on one engine, so
//! the key the repository is handed is the very key KMS's own `DisableKey` / `EnableKey`
//! move.
//!
//! The RULE under test: **an operation that points a repository at a caller-named
//! encryption key — creating the repository under it (`CreateRepository`) or rotating the
//! repository onto it (`UpdateRepositoryEncryptionKey`) — takes that key into use, so a
//! `kmsKeyId` resolving to a key KMS reports in key state `Disabled` is refused HTTP 400
//! with the modelled `EncryptionKeyDisabledException` ("The encryption key is disabled",
//! client, 400, `smithy_query(query="operation",operation="UpdateRepositoryEncryptionKey")`)
//! and changes nothing: the repository stays on the key it already had, and a refused
//! create mints no repository. The gate is the key's state at the moment of the call —
//! read from KMS, which owns it — so the same call serves once the key is enabled again
//! and is refused again once it is disabled again.**
//!
//! The rows differ along what the rule generalises over: which FORM of the four KMS
//! documents for `KeyId` names the key (a bare key id, a key ARN, an alias name, an alias
//! ARN — `smithy_query(query="doc",shape="UpdateRepositoryEncryptionKeyInput",member="kmsKeyId")`
//! sends a caller to that list), which operation points the repository at it, whether the
//! key is disabled, enabled again or disabled a second time, and whether the key named is
//! one KMS holds at all. Beside them are the dimensions the gate must NOT key on: a
//! repository created under another key while this one is disabled is served, and a key
//! KMS holds no record of (a key in an account or a key store this emulator does not
//! keep) refuses nothing — there is no key state to read.
//!
//! Two crates have to agree for the rule to hold — KMS owns the key and its state,
//! CodeCommit owns the repository that depends on it — so the suite lives here, over the
//! WHOLE assembled service set the binary mounts, routed by signing name exactly as a
//! client reaches it. The CodeCommit crate's own
//! `tests/all/repository_encryption_key_enforcement.rs` holds the same rule's rows over a
//! two-service router (and the other half of the key-state rule: every call on a
//! repository's CONTENTS while the key it records is disabled); what is added here is the
//! assembled emulator, where CodeCommit and KMS are two of every service mounted on one
//! engine.
//!
//! Every key state is applied through KMS's own call and read back through KMS's own
//! `DescribeKey` before the call that state governs, so no outcome here depends on how
//! fast the host runs.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};

/// The Region every call here is signed for, and so the Region the keys and the
/// repositories belong to.
const REGION: &str = "us-west-2";

/// CodeCommit's `X-Amz-Target` prefix and its signing name.
const CODECOMMIT_SIGNING_NAME: &str = "codecommit";
const CODECOMMIT_TARGET_PREFIX: &str = "CodeCommit_20150413";

/// KMS's `X-Amz-Target` prefix and its signing name.
const KMS_SIGNING_NAME: &str = "kms";
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The error code CodeCommit refuses a call that would take a disabled key into use with,
/// and the one it refuses a read of a repository the account does not hold with.
const DISABLED: &str = "EncryptionKeyDisabledException";
const REPOSITORY_ABSENT: &str = "RepositoryDoesNotExistException";

/// The key state KMS reports for a key `DisableKey` left behind, and the one `EnableKey`
/// leaves behind.
const KEY_STATE_DISABLED: &str = "Disabled";
const KEY_STATE_ENABLED: &str = "Enabled";

/// One `X-Amz-Target` RPC call (both services speak `awsJson1_1`), with whatever status it
/// answered.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name, REGION))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CodeCommit call with whatever status it answered.
async fn codecommit_answered(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc(
        router,
        CODECOMMIT_SIGNING_NAME,
        CODECOMMIT_TARGET_PREFIX,
        operation,
        body,
    )
    .await
}

/// One CodeCommit call that must answer 200.
async fn codecommit(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = codecommit_answered(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CodeCommit {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One KMS call that must answer 200.
async fn kms(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, KMS_SIGNING_NAME, KMS_TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from KMS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The error code an `awsJson1_1` refusal carries, as the body's `__type` names it (which
/// may be namespace-qualified).
fn error_code(body: &Value) -> String {
    let rendered = body.get("__type").and_then(Value::as_str).unwrap_or("");
    rendered.rsplit('#').next().unwrap_or(rendered).to_string()
}

/// The sentence a refusal carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The one string a `Value` member must hold.
fn string_at(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no {pointer} in {body}"))
        .to_string()
}

/// A KMS key of the account, answered as its key id and its ARN.
async fn given_key(router: &MultiServiceRouter, case: &str, description: &str) -> (String, String) {
    let created = kms(
        router,
        case,
        "CreateKey",
        &json!({ "Description": description }),
    )
    .await;
    (
        string_at(&created, "/KeyMetadata/KeyId"),
        string_at(&created, "/KeyMetadata/Arn"),
    )
}

/// Move `key_id` into the state `DisableKey` or `EnableKey` leaves it in, and read that
/// state back through KMS's own `DescribeKey` — the service that owns the state is what
/// reports it, so nothing here waits on a clock.
async fn key_state_moved_to(router: &MultiServiceRouter, case: &str, key_id: &str, enabled: bool) {
    let operation = if enabled { "EnableKey" } else { "DisableKey" };
    kms(router, case, operation, &json!({ "KeyId": key_id })).await;
    let described = kms(router, case, "DescribeKey", &json!({ "KeyId": key_id })).await;
    let expected = if enabled {
        KEY_STATE_ENABLED
    } else {
        KEY_STATE_DISABLED
    };
    assert_eq!(
        string_at(&described, "/KeyMetadata/KeyState"),
        expected,
        "case {case:?}: KMS must report key state {expected:?} for {key_id:?}, got {described}"
    );
}

/// The key `GetRepository` reports the repository is encrypted under.
async fn repository_key(router: &MultiServiceRouter, case: &str, repository: &str) -> String {
    let got = codecommit(
        router,
        case,
        "GetRepository",
        &json!({ "repositoryName": repository }),
    )
    .await;
    string_at(&got, "/repositoryMetadata/kmsKeyId")
}

/// The sentence CodeCommit refuses a call that would take a disabled key into use with:
/// fixed words around the key, which is the sentence's only variable part.
fn disabled_message(key: &str) -> String {
    format!("KMS key {key} is disabled")
}

/// The account a KMS ARN names, read out of the ARN KMS itself minted rather than written
/// here, so an alias ARN this suite builds names the same account as the key.
fn account_of(key_arn: &str) -> String {
    key_arn
        .split(':')
        .nth(4)
        .unwrap_or_else(|| panic!("no account in the KMS ARN {key_arn:?}"))
        .to_string()
}

/// The four forms KMS documents for `KeyId`, each naming ONE key: its bare id, its ARN, an
/// alias of it and that alias's ARN. Answered as (why the row is there, the selector).
async fn selector_forms(
    router: &MultiServiceRouter,
    case: &str,
    alias_name: &str,
    key_id: &str,
    key_arn: &str,
) -> Vec<(&'static str, String)> {
    kms(
        router,
        case,
        "CreateAlias",
        &json!({ "AliasName": alias_name, "TargetKeyId": key_id }),
    )
    .await;
    vec![
        ("a bare key id", key_id.to_string()),
        ("a key ARN", key_arn.to_string()),
        ("an alias name", alias_name.to_string()),
        (
            "an alias ARN",
            format!(
                "arn:aws:kms:{REGION}:{}:{alias_name}",
                account_of(key_arn)
            ),
        ),
    ]
}

#[tokio::test]
async fn a_rotation_onto_a_disabled_key_is_refused_and_the_repository_keeps_the_key_it_had() {
    let router = router();
    let (target_id, target_arn) = given_key(&router, "setup", "the key a rotation aims at").await;
    let (_, in_effect_arn) = given_key(&router, "setup", "the key the repositories hold").await;
    let rows = selector_forms(
        &router,
        "setup",
        "alias/codecommit-rotation-target",
        &target_id,
        &target_arn,
    )
    .await;

    key_state_moved_to(&router, "the rotation target disabled", &target_id, false).await;

    // One repository per selector form, each created under a key that is NOT the disabled
    // one: the create serves, so the gate is the key a call names rather than a switch
    // over the service.
    let repositories: Vec<String> = (0..rows.len())
        .map(|row| format!("rotate-onto-disabled-{row}"))
        .collect();
    for ((why, _), repository) in rows.iter().zip(&repositories) {
        codecommit(
            &router,
            why,
            "CreateRepository",
            &json!({ "repositoryName": repository, "kmsKeyId": in_effect_arn }),
        )
        .await;
        assert_eq!(
            repository_key(&router, why, repository).await,
            in_effect_arn,
            "case {why:?}: the repository must be created under the enabled key"
        );
    }

    // Each form of the disabled key's name is refused, in the same code and the same
    // words, and leaves the repository on the key it had.
    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let (status, answered) = codecommit_answered(
            &router,
            "UpdateRepositoryEncryptionKey",
            &json!({ "repositoryName": repository, "kmsKeyId": selector }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {why:?} ({selector}): a rotation onto a disabled key must be refused 400, \
             got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            DISABLED,
            "case {why:?} ({selector}): expected {DISABLED}, got {answered}"
        );
        assert_eq!(
            error_message(&answered),
            disabled_message(&target_arn),
            "case {why:?} ({selector}): expected the disabled-key sentence naming the key, \
             got {answered}"
        );
        assert_eq!(
            repository_key(&router, why, repository).await,
            in_effect_arn,
            "case {why:?} ({selector}): a refused rotation must leave the repository on the \
             key it had"
        );
    }

    // The gate is the key's CURRENT state: the very same rotations serve once KMS reports
    // the key enabled, and each reports the key itself whichever form named it.
    key_state_moved_to(&router, "the rotation target enabled", &target_id, true).await;
    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let rotated = codecommit(
            &router,
            why,
            "UpdateRepositoryEncryptionKey",
            &json!({ "repositoryName": repository, "kmsKeyId": selector }),
        )
        .await;
        assert_eq!(
            rotated.get("kmsKeyId").and_then(Value::as_str),
            Some(target_arn.as_str()),
            "case {why:?} ({selector}): the accepted rotation must report the key now in \
             effect, got {rotated}"
        );
        assert_eq!(
            rotated.get("originalKmsKeyId").and_then(Value::as_str),
            Some(in_effect_arn.as_str()),
            "case {why:?} ({selector}): the accepted rotation must report the key left \
             behind, got {rotated}"
        );
        assert_eq!(
            repository_key(&router, why, repository).await,
            target_arn,
            "case {why:?} ({selector}): the rotated repository must hold the new key"
        );
    }

    // And disabling it again refuses the same call again — including a rotation onto the
    // key the repository already holds, which still has to take that key into use.
    key_state_moved_to(&router, "disabled a second time", &target_id, false).await;
    let (status, answered) = codecommit_answered(
        &router,
        "UpdateRepositoryEncryptionKey",
        &json!({ "repositoryName": repositories[0], "kmsKeyId": target_arn }),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered)),
        (400, DISABLED.to_string()),
        "case {:?}: the gate is read per call, got {status} with {answered}",
        "disabled a second time"
    );
    assert_eq!(
        repository_key(&router, "disabled a second time", &repositories[0]).await,
        target_arn,
        "case {:?}: the refused rotation must leave the repository exactly as it was",
        "disabled a second time"
    );
}

#[tokio::test]
async fn a_create_under_a_disabled_key_mints_no_repository() {
    let router = router();
    let (key_id, key_arn) = given_key(&router, "setup", "the key a create asks for").await;
    let rows = selector_forms(
        &router,
        "setup",
        "alias/codecommit-create-target",
        &key_id,
        &key_arn,
    )
    .await;

    key_state_moved_to(&router, "the create target disabled", &key_id, false).await;

    let repositories: Vec<String> = (0..rows.len())
        .map(|row| format!("create-under-disabled-{row}"))
        .collect();
    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let (status, answered) = codecommit_answered(
            &router,
            "CreateRepository",
            &json!({ "repositoryName": repository, "kmsKeyId": selector }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {why:?} ({selector}): a create under a disabled key must be refused 400, \
             got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            DISABLED,
            "case {why:?} ({selector}): expected {DISABLED}, got {answered}"
        );
        assert_eq!(
            error_message(&answered),
            disabled_message(&key_arn),
            "case {why:?} ({selector}): expected the disabled-key sentence naming the key, \
             got {answered}"
        );

        // Nothing was minted: the account does not hold the repository the refused create
        // named.
        let (status, read) = codecommit_answered(
            &router,
            "GetRepository",
            &json!({ "repositoryName": repository }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read)),
            (400, REPOSITORY_ABSENT.to_string()),
            "case {why:?} ({selector}): a refused create must mint no repository, got \
             {status} with {read}"
        );
    }

    // The same creates serve once the key is usable, each reporting the key itself.
    key_state_moved_to(&router, "the create target enabled", &key_id, true).await;
    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let created = codecommit(
            &router,
            why,
            "CreateRepository",
            &json!({ "repositoryName": repository, "kmsKeyId": selector }),
        )
        .await;
        assert_eq!(
            created
                .pointer("/repositoryMetadata/kmsKeyId")
                .and_then(Value::as_str),
            Some(key_arn.as_str()),
            "case {why:?} ({selector}): the created repository must report the key it is \
             encrypted under, got {created}"
        );
        assert_eq!(
            repository_key(&router, why, repository).await,
            key_arn,
            "case {why:?} ({selector}): the stored repository must hold that key"
        );
    }
}

#[tokio::test]
async fn a_key_kms_holds_no_record_of_refuses_nothing() {
    let router = router();

    // A key id and a key ARN of the forms KMS documents, which this KMS holds no key for:
    // the key is the caller's own, in an account or a key store this emulator does not
    // keep, so there is no key state to read and a call AWS serves is not refused.
    let unheld_id = "1234abcd-12ab-34cd-56ef-1234567890ab";
    let (_, minted_arn) = given_key(&router, "setup", "a key only for its account id").await;
    let unheld_arn = format!(
        "arn:aws:kms:{REGION}:{}:key/fedcba98-7654-3210-fedc-ba9876543210",
        account_of(&minted_arn)
    );

    let created = codecommit(
        &router,
        "a key ARN KMS holds no key for",
        "CreateRepository",
        &json!({ "repositoryName": "unheld-key-create", "kmsKeyId": unheld_arn }),
    )
    .await;
    assert_eq!(
        created
            .pointer("/repositoryMetadata/kmsKeyId")
            .and_then(Value::as_str),
        Some(unheld_arn.as_str()),
        "case {:?}: the create must serve and hold the key as named, got {created}",
        "a key ARN KMS holds no key for"
    );

    let rotated = codecommit(
        &router,
        "a key id KMS holds no key for",
        "UpdateRepositoryEncryptionKey",
        &json!({ "repositoryName": "unheld-key-create", "kmsKeyId": unheld_id }),
    )
    .await;
    let now_in_effect = string_at(&rotated, "/kmsKeyId");
    assert!(
        now_in_effect.ends_with(&format!("key/{unheld_id}")),
        "case {:?}: the rotation must serve and report that key, got {now_in_effect:?}",
        "a key id KMS holds no key for"
    );
}
