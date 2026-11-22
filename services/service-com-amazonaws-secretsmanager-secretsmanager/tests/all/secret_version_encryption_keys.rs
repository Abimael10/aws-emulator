// SPDX-License-Identifier: CC0-1.0

//! Which KMS key a secret version reports, end to end over the wire.
//!
//! Every case drives the real HTTP path — an `awsJson1_1` POST through the router's
//! `dispatch` — so these are the same bytes an SDK client sends. Nothing reaches into the
//! service's state struct: the key a version is held under is only ever read back through
//! `ListSecretVersionIds`, beside the key `DescribeSecret` reports for the secret itself.
//!
//! THE RULE: `SecretVersionsListEntry$KmsKeyIds` names the key THAT version's value is
//! held under, as the request that put it there named the key — in whichever of the forms
//! `KmsKeyId` accepts (key ARN, key id, alias) it was given — and a version moves to
//! another key only where `UpdateSecret` moves it, which is the versions carrying the
//! staging labels the service itself moves. So the entry for a version the secret's
//! current key never covered goes on naming the key holding it, and only a version stored
//! while no key was named at all reports the default key's own token, which is the one case
//! the service has no identifier of the caller's to report. For a secret holding ONE
//! version, that version is always the current one, so what it reports is the key the
//! secret's configuration names — which is what `DescribeSecret` answers.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::{
    DEFAULT_ENCRYPTION_KEY_ID, DEFAULT_KMS_KEY_ALIAS,
};
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::secret_kms_key_state::{create_body, value_body, version_id_of, Emulator};
use crate::secret_lifecycle::ValueCase;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// A fresh emulator holding only this service, over its own engine.
fn emulator() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(engine));
    router
}

/// One call that must be served, answering its payload.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/\
                 secretsmanager/aws4_request, SignedHeaders=host;x-amz-date, Signature=deadbeef"
            ),
        )
        .body(::bytes::Bytes::from(
            ::serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        ::serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// The keys every version of `secret_id` reports, as one set of lists in listing order.
async fn reported_keys(router: &MultiServiceRouter, case: &str, secret_id: &str) -> Vec<Vec<String>> {
    ok(
        router,
        case,
        "ListSecretVersionIds",
        json!({ "SecretId": secret_id, "IncludeDeprecated": true }),
    )
    .await["Versions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            entry["KmsKeyIds"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|key| key.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect()
}

/// The key `DescribeSecret` reports for `secret_id`, or `None` for a secret that names none.
async fn configured_key(router: &MultiServiceRouter, case: &str, secret_id: &str) -> Option<String> {
    ok(
        router,
        case,
        "DescribeSecret",
        json!({ "SecretId": secret_id }),
    )
    .await["KmsKeyId"]
        .as_str()
        .map(str::to_string)
}

/// One row of the rekey table: the case, the `KmsKeyId` its `UpdateSecret` names (or none at
/// all), and the keys the secret's versions must then report.
type RekeyRow = (&'static str, Option<String>, Vec<String>);

/// A key ARN of the form `KmsKeyId` documents, for a key of this account.
fn key_arn(key_id: &str) -> String {
    format!(
        "arn:aws:kms:{REGION}:{}:key/{key_id}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The id the `number`-th version this suite stakes is created under.
///
/// A write that stores a value has to NAME the version it creates, and
/// `ClientRequestToken` is `len 32..64` — so the token takes the 36-character form a UUID
/// has, which is the form the service's own ids take.
fn version_token(number: usize) -> String {
    format!("00000000-0000-0000-0000-{number:012}")
}

/// THE RULE over the re-keying an `UpdateSecret` performs.
///
/// The rows span what the rule generalises over: a secret re-keyed onto the Amazon Web
/// Services managed key by its alias, onto that key by the empty string the model documents
/// for it, onto another customer managed key by bare id and by ARN, and one whose update
/// names no key at all (so it stays on the key it was created with). Every row starts from a
/// secret created under a customer managed key, which is the state a key change is made from.
#[tokio::test]
async fn a_version_reports_the_key_the_secret_is_configured_with_after_a_rekey() {
    let created_under = "1234abcd-12ab-34cd-56ef-1234567890ab";
    let another_key = "9876fedc-98fe-76dc-54ba-9876543210fe";
    let another_key_arn = key_arn(another_key);
    let rows: Vec<RekeyRow> = vec![
        (
            "re-keyed onto the managed key by its alias",
            Some(DEFAULT_KMS_KEY_ALIAS.to_string()),
            vec![DEFAULT_KMS_KEY_ALIAS.to_string()],
        ),
        (
            "re-keyed onto the managed key by the empty string",
            Some(String::new()),
            vec![DEFAULT_KMS_KEY_ALIAS.to_string()],
        ),
        (
            "re-keyed onto another key by its id",
            Some(another_key.to_string()),
            vec![another_key.to_string()],
        ),
        (
            "re-keyed onto another key by its ARN",
            Some(another_key_arn.clone()),
            vec![another_key_arn.clone()],
        ),
        (
            "an update that names no key",
            None,
            vec![created_under.to_string()],
        ),
    ];

    let router = emulator();
    for (number, (case, rekey_to, expected)) in rows.iter().enumerate() {
        let name = format!("rekey/{}", case.replace(' ', "-"));
        ok(
            &router,
            case,
            "CreateSecret",
            json!({
                "Name": name.clone(),
                "SecretString": "value-0",
                "KmsKeyId": created_under,
                "ClientRequestToken": version_token(number),
            }),
        )
        .await;
        assert_eq!(
            reported_keys(&router, case, &name).await,
            vec![vec![created_under.to_string()]],
            "case {case:?}: a fresh secret's version reports the key it was created under"
        );

        let mut update = json!({
            "SecretId": name.clone(),
            "Description": "re-keyed in place",
        });
        if let Some(key) = rekey_to {
            update["KmsKeyId"] = json!(key);
        }
        ok(&router, case, "UpdateSecret", update).await;

        let reported = reported_keys(&router, case, &name).await;
        assert_eq!(
            reported,
            vec![expected.clone()],
            "case {case:?}: expected the version to report {expected:?}"
        );
        // The version's key and the secret's own configuration are ONE fact, so the two
        // reads cannot disagree about it.
        assert_eq!(
            configured_key(&router, case, &name).await.as_deref(),
            expected.first().map(String::as_str),
            "case {case:?}: DescribeSecret must report the same key the version does"
        );
    }
}

/// THE RULE's one exception: a secret whose configuration names NO key reports the default
/// key's own token, and goes on reporting it over every version it holds — until an update
/// gives it a key of its own, after which it reports that key.
#[tokio::test]
async fn only_a_secret_that_names_no_key_reports_the_default_key_token() {
    let router = emulator();
    let case = "a secret created with no key";
    let name = "rekey/no-key-at-first";
    ok(
        &router,
        case,
        "CreateSecret",
        json!({
            "Name": name,
            "SecretString": "value-0",
            "ClientRequestToken": version_token(0),
        }),
    )
    .await;
    ok(
        &router,
        case,
        "PutSecretValue",
        json!({
            "SecretId": name,
            "SecretString": "value-1",
            "ClientRequestToken": version_token(1),
        }),
    )
    .await;
    assert_eq!(
        reported_keys(&router, case, name).await,
        vec![
            vec![DEFAULT_ENCRYPTION_KEY_ID.to_string()],
            vec![DEFAULT_ENCRYPTION_KEY_ID.to_string()],
        ],
        "case {case:?}: every version of a secret that names no key reports the default token"
    );
    assert_eq!(
        configured_key(&router, case, name).await,
        None,
        "case {case:?}: such a secret reports no KmsKeyId of its own"
    );

    let case = "the same secret given a key of its own";
    let given = "alias/my-secrets";
    ok(
        &router,
        case,
        "UpdateSecret",
        json!({ "SecretId": name, "KmsKeyId": given }),
    )
    .await;
    assert_eq!(
        reported_keys(&router, case, name).await,
        vec![vec![given.to_string()], vec![given.to_string()]],
        "case {case:?}: the versions report the key the secret now names"
    );
}

// --- the key a version of its OWN -------------------------------------------------
//
// The cases below mount KMS beside Secrets Manager on one engine (the harness
// `crate::secret_kms_key_state` builds), so every key named is a real key minted through
// KMS's own operations and a version that moves to another key is really re-encrypted
// under it.

/// The staging label a rotation stakes, one of the three the service itself moves and so
/// one a key change covers (`UpdateSecretRequest$KmsKeyId`, this service's Smithy model).
const AWSPENDING: &str = "AWSPENDING";

/// A staging label a CALLER attaches, which is none of those three: a version carrying
/// only this one is outside what a key change covers.
const CALLER_LABEL: &str = "ANALYTICS";

/// Every version of `secret_id` with the keys it reports, deprecated versions included,
/// read back through `ListSecretVersionIds` over the wire.
async fn keys_by_version(
    emulator: &Emulator,
    secret_id: &str,
    case: &str,
) -> BTreeMap<String, Vec<String>> {
    let listed = emulator
        .sm_ok(
            "ListSecretVersionIds",
            &json!({"SecretId": secret_id, "IncludeDeprecated": true}),
            case,
        )
        .await;
    let versions = listed["Versions"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no Versions in {listed}"));
    versions
        .iter()
        .map(|entry| {
            let version_id = entry["VersionId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {entry}"))
                .to_string();
            let keys = entry["KmsKeyIds"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no KmsKeyIds in {entry}"))
                .iter()
                .map(|key| {
                    key.as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: {key} is not a key reference"))
                        .to_string()
                })
                .collect();
            (version_id, keys)
        })
        .collect()
}

/// One write of a value, answering the id of the version it created.
async fn write(
    emulator: &Emulator,
    name: &str,
    case: &str,
    value: &'static str,
    stages: &[&str],
) -> String {
    version_id_of(
        &emulator
            .sm_ok(
                "PutSecretValue",
                &value_body(name, &ValueCase::string("a stored value", value), stages),
                case,
            )
            .await,
        case,
    )
}

#[tokio::test]
async fn each_version_reports_the_key_that_holds_it_not_the_secrets_current_key() {
    let emulator = Emulator::new();
    let setup = "two customer managed keys";
    let (first_key, _) = emulator.create_key(setup).await;
    let (second_key, second_key_id) = emulator.create_key(setup).await;

    // The rows span what the rule generalises over rather than one recorded request: the
    // FORM the update names the new key in (key ARN, bare key id, the managed key's
    // alias, and the empty string the model documents for that key — "if you set this to
    // an empty string, Secrets Manager uses the Amazon Web Services managed key
    // aws/secretsmanager", `UpdateSecretRequest$KmsKeyId`), the DIRECTION the change goes
    // in (from a customer managed key, and from the managed key a secret naming no key is
    // on), and an update that names no key at all, which moves nothing.
    let rows: Vec<(&str, Option<String>, Option<String>, String)> = vec![
        (
            "onto another key by its ARN",
            Some(first_key.clone()),
            Some(second_key.clone()),
            second_key.clone(),
        ),
        (
            "onto another key by its bare id",
            Some(first_key.clone()),
            Some(second_key_id.clone()),
            second_key_id.clone(),
        ),
        (
            "onto the managed key by its alias",
            Some(first_key.clone()),
            Some(DEFAULT_KMS_KEY_ALIAS.to_string()),
            DEFAULT_KMS_KEY_ALIAS.to_string(),
        ),
        (
            "onto the managed key by the empty string",
            Some(first_key.clone()),
            Some(String::new()),
            DEFAULT_KMS_KEY_ALIAS.to_string(),
        ),
        (
            "from the managed key onto a customer managed key",
            None,
            Some(second_key.clone()),
            second_key.clone(),
        ),
        (
            "an update that names no key at all",
            Some(first_key.clone()),
            None,
            first_key.clone(),
        ),
        (
            "a secret that never named a key",
            None,
            None,
            DEFAULT_ENCRYPTION_KEY_ID.to_string(),
        ),
    ];

    for (row_index, (case, created_under, rekey_to, expected_after)) in rows.iter().enumerate() {
        let name = format!("per-version-{row_index}");
        // What every version reports before the update: the key the create named, or the
        // default key's own token for a secret that named none.
        let expected_before = created_under
            .clone()
            .unwrap_or_else(|| DEFAULT_ENCRYPTION_KEY_ID.to_string());

        // Five writes, leaving one version in each position the rule distinguishes:
        // deprecated, AWSPREVIOUS, AWSCURRENT, a caller's own label, and AWSPENDING.
        let deprecated = version_id_of(
            &emulator
                .sm_ok(
                    "CreateSecret",
                    &create_body(
                        &name,
                        created_under.as_deref(),
                        &ValueCase::string("the created value", "value-one"),
                    ),
                    case,
                )
                .await,
            case,
        );
        let previous = write(&emulator, &name, case, "value-two", &[]).await;
        let current = write(&emulator, &name, case, "value-three", &[]).await;
        let caller_labelled = write(&emulator, &name, case, "value-four", &[CALLER_LABEL]).await;
        let pending = write(&emulator, &name, case, "value-five", &[AWSPENDING]).await;

        let before = keys_by_version(&emulator, &name, case).await;
        for version in [&deprecated, &previous, &current, &caller_labelled, &pending] {
            assert_eq!(
                before.get(version),
                Some(&vec![expected_before.clone()]),
                "case {case:?}: expected the version {version} to report the key it was \
                 stored under, {expected_before:?}, got {before:?}"
            );
        }

        let mut update = json!({"SecretId": name.clone()});
        match rekey_to {
            Some(key) => update["KmsKeyId"] = json!(key),
            // A row that names no key still updates SOMETHING, so it changes a detail
            // that has nothing to do with the key.
            None => update["Description"] = json!("updated without naming a key"),
        }
        emulator.sm_ok("UpdateSecret", &update, case).await;

        let after = keys_by_version(&emulator, &name, case).await;
        // The versions the labels the service moves reach are on the key the update
        // named, in the form it named it...
        for version in [&previous, &current, &pending] {
            assert_eq!(
                after.get(version),
                Some(&vec![expected_after.clone()]),
                "case {case:?}: expected the staged version {version} to report \
                 {expected_after:?}, got {after:?}"
            );
        }
        // ... and the two outside them keep the key that holds them, whatever the secret
        // itself now names.
        for version in [&deprecated, &caller_labelled] {
            assert_eq!(
                after.get(version),
                Some(&vec![expected_before.clone()]),
                "case {case:?}: expected the version {version} outside those labels to keep \
                 {expected_before:?}, got {after:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_versions_key_is_the_one_in_force_when_it_was_stored_or_last_moved() {
    // A version's key is its OWN HISTORY, not the secret's current configuration: two key
    // changes with writes between them leave the secret's versions on different keys, and
    // each entry reports the one holding it.
    let emulator = Emulator::new();
    let case = "two key changes over a version history";
    let (first_key, _) = emulator.create_key(case).await;
    let (second_key, _) = emulator.create_key(case).await;
    let name = "per-version-history";

    let left_on_second = version_id_of(
        &emulator
            .sm_ok(
                "CreateSecret",
                &create_body(
                    name,
                    Some(&first_key),
                    &ValueCase::string("the created value", "value-one"),
                ),
                case,
            )
            .await,
        case,
    );
    // The first change moves that version to the second key while it is still current.
    emulator
        .sm_ok(
            "UpdateSecret",
            &json!({"SecretId": name, "KmsKeyId": second_key.clone()}),
            case,
        )
        .await;
    // Two writes under the second key, the second of which deprecates the created
    // version — leaving it on the second key for good.
    let moved_back = write(&emulator, name, case, "value-two", &[]).await;
    let also_moved_back = write(&emulator, name, case, "value-three", &[]).await;
    // An update that stores a value AND names the first key again: it mints a version
    // under that key and moves the versions the labels reach back to it.
    let minted = version_id_of(
        &emulator
            .sm_ok(
                "UpdateSecret",
                &json!({
                    "SecretId": name,
                    "SecretString": "value-four",
                    "KmsKeyId": first_key.clone(),
                }),
                case,
            )
            .await,
        case,
    );

    let reported = keys_by_version(&emulator, name, case).await;
    for (version, expected) in [
        (&left_on_second, &second_key),
        (&moved_back, &first_key),
        (&also_moved_back, &first_key),
        (&minted, &first_key),
    ] {
        assert_eq!(
            reported.get(version),
            Some(&vec![expected.clone()]),
            "case {case:?}: expected the version {version} to report {expected:?}, got \
             {reported:?}"
        );
    }
    // The secret's own configuration and the version `AWSCURRENT` names cannot disagree
    // about the key in force now.
    let described = emulator
        .sm_ok("DescribeSecret", &json!({"SecretId": name}), case)
        .await;
    assert_eq!(
        described["KmsKeyId"].as_str(),
        Some(first_key.as_str()),
        "case {case:?}: expected DescribeSecret to name the key the current version is \
         held under, got {described}"
    );
}
