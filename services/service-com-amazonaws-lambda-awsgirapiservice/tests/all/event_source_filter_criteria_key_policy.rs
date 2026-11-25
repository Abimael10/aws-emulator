// SPDX-License-Identifier: CC0-1.0

//! What `CreateEventSourceMapping` and `UpdateEventSourceMapping` do with a `KMSKeyArn` whose
//! KEY POLICY decides whether Lambda itself may use the key.
//!
//! RULE: the `KMSKeyArn` of a mapping is "the ... customer managed key that Lambda uses to
//! encrypt your function's filter criteria", and Lambda opens those criteria again under that
//! key as its OWN service principal — a principal that is no identity of the account and holds
//! no identity-based policies, so the key's own policy is the whole authority over the use. A
//! configuration whose key policy withholds that decrypt from this service is therefore refused
//! 400 `InvalidParameterValueException` with nothing created or changed, and one whose policy
//! admits it — by either published spelling of the principal, through a wildcard over the KMS
//! namespace, or under a condition this use satisfies — is accepted; a key that seals nothing,
//! because the configuration carries no filter to encrypt or names no customer managed key at
//! all, is never asked about.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with the key
//! service mounted beside Lambda on ONE engine: each key and each key policy is written through
//! KMS's own `CreateKey`, so the document the decision is taken over is the one that service
//! holds, and every accepted mapping is read back through `GetEventSourceMapping` — so a
//! configuration that was answered but never persisted fails here, as does one accepted under a
//! key that cannot open it.
//!
//! The rows differ along what the rule generalises over: WHICH document names the principal
//! (the default policy, a statement naming the Region-less principal, the regional one), HOW it
//! names the action (the decrypt itself, a namespace wildcard, another action only), whether an
//! explicit deny stands beside the allow, which CONDITION scopes the allow (the service the use
//! arrives through, the account the mapping belongs to — each satisfied and each not), whether
//! there is anything to seal at all, and whether the pair is stated by the create or by an
//! update over what the mapping already holds.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_completion::{kms_call, router_with_key_service};
use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, create_request, get_mapping,
    list_mappings, refusal_message, Answer, MAPPINGS_URI, STATE_ENABLED,
};

/// The Region every request below is signed for, and the account this emulator answers for.
const REGION: &str = "us-east-1";
/// See [`REGION`] (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// Another account's id, for the condition rows that scope an allow to an account this mapping
/// does not belong to.
const ANOTHER_ACCOUNT: &str = "210987654321";

/// The sentence AWS refuses a mapping whose filter-criteria key withholds the decrypt from the
/// Lambda service principal with — fixed text with no variable part (recorded behaviour, trace
/// `kms__lambda__sqs.esm-filter-criteria-kms-key-without-the-lambda-service-grant-is-rejected`).
const DENIED_MESSAGE: &str = "Lambda service principal was denied access to decrypt your filter \
                              criteria. Please check your KMS key policy";

/// The modelled error such a refusal is answered under: "one of the parameters in the request is
/// not valid" (`smithy_query(query="operation", operation="CreateEventSourceMapping")`, where
/// `InvalidParameterValueException` is `@httpError(400)`).
const REFUSAL_CODE: &str = "InvalidParameterValueException";
/// See [`REFUSAL_CODE`].
const REFUSAL_STATUS: u16 = 400;

/// The KMS action a key policy has to allow this service for an encrypted mapping's criteria to
/// be read back: the criteria are reported in plaintext "if you have kms:Decrypt permissions for
/// the correct KMS key" (`EventSourceMappingConfiguration$FilterCriteria`).
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// The criteria every row below seals under its key: one filter carrying one pattern, so the
/// mapping really has something to encrypt.
fn criteria() -> Value {
    json!({ "Filters": [{ "Pattern": "{\"body\": {\"kind\": [\"wanted\"]}}" }] })
}

/// The statement the default key policy carries, which every document below keeps beside its own
/// statements: "the default key policy ... gives the Amazon Web Services account that owns the
/// KMS key permission to use it in all cryptographic operations", which is also what keeps the
/// document past the policy lockout safety check.
fn account_root_statement() -> Value {
    json!({
        "Sid": "Enable IAM User Permissions",
        "Effect": "Allow",
        "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// A key policy document carrying `statements` beside [`account_root_statement`].
fn key_policy(statements: Vec<Value>) -> Value {
    let mut all = vec![account_root_statement()];
    all.extend(statements);
    json!({ "Version": "2012-10-17", "Statement": all })
}

/// One statement over a SERVICE principal: `effect` for `actions` by `principal`, under
/// `condition` when one is given.
fn service_statement(
    sid: &str,
    effect: &str,
    principal: &str,
    actions: &[&str],
    condition: Option<Value>,
) -> Value {
    let mut statement = json!({
        "Sid": sid,
        "Effect": effect,
        "Principal": { "Service": principal },
        "Action": actions,
        "Resource": "*",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    statement
}

/// The two spellings AWS publishes this service's principal in, both of which name it.
fn regionless_principal() -> String {
    "lambda.amazonaws.com".to_string()
}
/// See [`regionless_principal`].
fn regional_principal() -> String {
    format!("lambda.{REGION}.amazonaws.com")
}

/// A key this account holds, created through KMS's OWN `CreateKey` under the policy `policy`
/// states — `None` for the policy KMS writes for a key created without one, which names the
/// account root alone — answered by the ARN that service minted for it.
async fn given_key(router: &MultiServiceRouter, case: &str, policy: Option<Value>) -> String {
    let mut body = json!({ "Description": format!("filter criteria key for {case}") });
    if let Some(policy) = policy {
        body["Policy"] = Value::String(policy.to_string());
    }
    let created = kms_call(router, "CreateKey", &body).await;
    let answered = String::from_utf8_lossy(&created.body).to_string();
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateKey must answer 200, got {} {answered}",
        created.status
    );
    created
        .json()
        .pointer("/KeyMetadata/Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey must answer a key ARN, got {answered}"))
        .to_string()
}

/// Assert `answer` is the refusal this rule states, whichever call produced it.
fn assert_denied(case: &str, label: &str, answer: &Answer) {
    assert_eq!(
        answer.status,
        REFUSAL_STATUS,
        "case {case:?}: {label} must be refused {REFUSAL_STATUS}, got {} {}",
        answer.status,
        answer.text()
    );
    assert!(
        answer.error_code().contains(REFUSAL_CODE),
        "case {case:?}: {label} must be refused as {REFUSAL_CODE}, got {:?}",
        answer.error_code()
    );
    let message = refusal_message(answer).unwrap_or_default();
    assert_eq!(
        message, DENIED_MESSAGE,
        "case {case:?}: {label} must state {DENIED_MESSAGE:?}, got {message:?}"
    );
}

/// `UpdateEventSourceMapping` on the mapping `uuid` names.
async fn update_mapping(router: &MultiServiceRouter, uuid: &str, body: &Value) -> Answer {
    call(router, "PUT", &format!("{MAPPINGS_URI}/{uuid}"), body).await
}

/// One row of [`a_create_is_held_to_what_the_filter_criteria_key_policy_admits`]: the key policy
/// the mapping's key carries, and whether this service may decrypt under it.
struct KeyPolicyCase {
    name: &'static str,
    /// The document the key is created with, `None` for the default policy KMS writes.
    policy: Option<Value>,
    /// Whether the mapping must be created.
    accepted: bool,
}

fn key_policy_cases() -> Vec<KeyPolicyCase> {
    vec![
        // The policy KMS writes for a key created without one names the account root alone, so
        // it names this service nothing: the create is refused.
        KeyPolicyCase {
            name: "the-default-policy-naming-only-the-account-root",
            policy: None,
            accepted: false,
        },
        KeyPolicyCase {
            name: "the-service-principal-named-without-a-region",
            policy: Some(key_policy(vec![service_statement(
                "LetLambdaDecrypt",
                "Allow",
                &regionless_principal(),
                &[DECRYPT_ACTION, "kms:GenerateDataKey"],
                None,
            )])),
            accepted: true,
        },
        KeyPolicyCase {
            name: "the-service-principal-named-with-the-region",
            policy: Some(key_policy(vec![service_statement(
                "LetRegionalLambdaDecrypt",
                "Allow",
                &regional_principal(),
                &[DECRYPT_ACTION],
                None,
            )])),
            accepted: true,
        },
        KeyPolicyCase {
            name: "every-action-of-the-namespace-for-the-service-principal",
            policy: Some(key_policy(vec![service_statement(
                "LetLambdaUseTheKey",
                "Allow",
                &regionless_principal(),
                &["kms:*"],
                None,
            )])),
            accepted: true,
        },
        // A statement naming the principal for ANOTHER action allows nothing of the decrypt the
        // criteria are read back under.
        KeyPolicyCase {
            name: "only-another-action-for-the-service-principal",
            policy: Some(key_policy(vec![service_statement(
                "LetLambdaEncryptOnly",
                "Allow",
                &regionless_principal(),
                &["kms:Encrypt"],
                None,
            )])),
            accepted: false,
        },
        // An explicit deny wins over every allow, whichever order they are written in.
        KeyPolicyCase {
            name: "an-explicit-deny-beside-the-allow",
            policy: Some(key_policy(vec![
                service_statement(
                    "LetLambdaDecrypt",
                    "Allow",
                    &regionless_principal(),
                    &["kms:*"],
                    None,
                ),
                service_statement(
                    "KeepLambdaOut",
                    "Deny",
                    &regionless_principal(),
                    &[DECRYPT_ACTION],
                    None,
                ),
            ])),
            accepted: false,
        },
        // `kms:ViaService` "limits use of a KMS key to requests from particular Amazon Web
        // Services services", and this use arrives through THIS service in the Region the call
        // was signed for.
        KeyPolicyCase {
            name: "the-allow-scoped-to-this-services-via-service",
            policy: Some(key_policy(vec![service_statement(
                "LetLambdaDecryptThroughLambda",
                "Allow",
                &regionless_principal(),
                &[DECRYPT_ACTION],
                Some(json!({
                    "StringEquals": { "kms:ViaService": regional_principal() },
                })),
            )])),
            accepted: true,
        },
        KeyPolicyCase {
            name: "the-allow-scoped-to-another-services-via-service",
            policy: Some(key_policy(vec![service_statement(
                "LetLambdaDecryptThroughQueues",
                "Allow",
                &regionless_principal(),
                &[DECRYPT_ACTION],
                Some(json!({
                    "StringEquals": { "kms:ViaService": format!("sqs.{REGION}.amazonaws.com") },
                })),
            )])),
            accepted: false,
        },
        // `aws:SourceAccount` names "the account the resource making a service-to-service
        // request belongs to", which for this use is the account holding the mapping.
        KeyPolicyCase {
            name: "the-allow-scoped-to-the-mappings-own-account",
            policy: Some(key_policy(vec![service_statement(
                "LetLambdaDecryptForThisAccount",
                "Allow",
                &regionless_principal(),
                &[DECRYPT_ACTION],
                Some(json!({ "StringEquals": { "aws:SourceAccount": ACCOUNT } })),
            )])),
            accepted: true,
        },
        KeyPolicyCase {
            name: "the-allow-scoped-to-another-account",
            policy: Some(key_policy(vec![service_statement(
                "LetLambdaDecryptForAnotherAccount",
                "Allow",
                &regionless_principal(),
                &[DECRYPT_ACTION],
                Some(json!({ "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT } })),
            )])),
            accepted: false,
        },
    ]
}

#[tokio::test]
async fn a_create_is_held_to_what_the_filter_criteria_key_policy_admits() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);
    let function = "esm-criteria-key-fn";
    create_function(&router, function).await;

    for (index, case) in key_policy_cases().into_iter().enumerate() {
        let KeyPolicyCase {
            name,
            policy,
            accepted,
        } = case;
        let key_arn = given_key(&router, name, policy).await;
        // One source per row, so an accepted row cannot collide with another mapping of the
        // same function and source.
        let source = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:criteria-key-{index}");
        let request = create_request(
            function,
            Some(&source),
            &json!({ "FilterCriteria": criteria(), "KMSKeyArn": key_arn }),
        );
        let created = create_mapping(&router, &request).await;

        if !accepted {
            assert_denied(name, "the create", &created);
            // Nothing is created: the refusal leaves this account holding no mapping of that
            // source at all.
            let listed = list_mappings(&router, &format!("EventSourceArn={source}")).await;
            assert!(
                listed.mappings().is_empty(),
                "case {name:?}: the refused create must leave no mapping of {source:?}, got {}",
                listed.text()
            );
            continue;
        }

        assert_eq!(
            created.status,
            202,
            "case {name:?}: the create must answer 202, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {name:?}: no UUID in {}", created.text()));
        // The mapping is read back through the implemented reader: an accepted configuration is
        // one a later caller really sees, key and criteria together.
        let fetched = get_mapping(&router, &uuid).await;
        assert_eq!(
            (
                fetched.string("KMSKeyArn"),
                fetched.json().get("FilterCriteria").cloned()
            ),
            (Some(key_arn.clone()), Some(criteria())),
            "case {name:?}: the get must report the key and the criteria it was created with, \
             got {}",
            fetched.text()
        );
    }
}

/// One row of [`a_key_that_seals_nothing_is_never_asked_about`]: what the create states beside
/// the denying key, and whether the mapping must be created.
struct SealedCase {
    name: &'static str,
    /// What the request states, with `{key}` standing for the denying key's ARN.
    extras: Value,
    accepted: bool,
}

#[tokio::test]
async fn a_key_that_seals_nothing_is_never_asked_about() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);
    let function = "esm-unsealed-fn";
    create_function(&router, function).await;
    // ONE key for every row, carrying the policy that admits this service nothing, so what each
    // row shows is whether the key was asked about at all.
    let key_arn = given_key(&router, "denying", None).await;

    let cases = vec![
        // A mapping with no criteria seals nothing under the key.
        SealedCase {
            name: "no-filter-criteria-at-all",
            extras: json!({ "KMSKeyArn": key_arn }),
            accepted: true,
        },
        // "To delete the filter criteria, specify an empty FilterCriteria object": criteria
        // carrying no filter are no criteria, so there is still nothing to seal.
        SealedCase {
            name: "filter-criteria-carrying-no-filter",
            extras: json!({ "FilterCriteria": {}, "KMSKeyArn": key_arn }),
            accepted: true,
        },
        SealedCase {
            name: "filter-criteria-whose-filters-carry-no-pattern",
            extras: json!({ "FilterCriteria": { "Filters": [{}] }, "KMSKeyArn": key_arn }),
            accepted: true,
        },
        // "By default, Lambda does not encrypt your filter criteria object": criteria under no
        // customer managed key are sealed under nothing this service needs permission for.
        SealedCase {
            name: "criteria-under-no-customer-managed-key",
            extras: json!({ "FilterCriteria": criteria() }),
            accepted: true,
        },
        // `KMSKeyArn`'s own pattern admits the empty string, which names no key.
        SealedCase {
            name: "criteria-under-the-empty-key-arn",
            extras: json!({ "FilterCriteria": criteria(), "KMSKeyArn": "" }),
            accepted: true,
        },
        // The control: the same key, with criteria that ARE sealed under it.
        SealedCase {
            name: "criteria-under-the-denying-key",
            extras: json!({ "FilterCriteria": criteria(), "KMSKeyArn": key_arn }),
            accepted: false,
        },
    ];

    for (index, case) in cases.into_iter().enumerate() {
        let SealedCase {
            name,
            extras,
            accepted,
        } = case;
        let source = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:unsealed-{index}");
        let created =
            create_mapping(&router, &create_request(function, Some(&source), &extras)).await;
        if accepted {
            assert_eq!(
                created.status,
                202,
                "case {name:?}: the create must answer 202, got {} {}",
                created.status,
                created.text()
            );
            let uuid = created
                .string("UUID")
                .unwrap_or_else(|| panic!("case {name:?}: no UUID in {}", created.text()));
            let fetched = get_mapping(&router, &uuid).await;
            assert_eq!(
                fetched.string("UUID"),
                Some(uuid),
                "case {name:?}: the accepted mapping must be readable back, got {}",
                fetched.text()
            );
        } else {
            assert_denied(name, "the create", &created);
        }
    }
}

#[tokio::test]
async fn an_update_is_held_to_the_key_and_criteria_in_force_after_it() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);
    let function = "esm-criteria-key-update-fn";
    create_function(&router, function).await;

    let granting = given_key(
        &router,
        "granting",
        Some(key_policy(vec![service_statement(
            "LetLambdaDecrypt",
            "Allow",
            &regionless_principal(),
            &[DECRYPT_ACTION, "kms:GenerateDataKey"],
            None,
        )])),
    )
    .await;
    let denying = given_key(&router, "denying", None).await;

    // Row 1: a mapping already sealing its criteria under a key that admits this service. An
    // update that would move them onto a key that does not is refused, and the mapping keeps the
    // key it had; the same update under the key that does is accepted.
    let encrypted_source = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:criteria-key-update-0");
    let encrypted = create_mapping(
        &router,
        &create_request(
            function,
            Some(&encrypted_source),
            &json!({ "FilterCriteria": criteria(), "KMSKeyArn": granting }),
        ),
    )
    .await;
    assert_eq!(
        encrypted.status,
        202,
        "case \"re-keyed\": the mapping under test must be created, got {} {}",
        encrypted.status,
        encrypted.text()
    );
    let encrypted_uuid = encrypted
        .string("UUID")
        .expect("the create answers a UUID");
    // The settle out of `Creating` is owed by the CLOCK, so this waits for the mapping to REPORT
    // it rather than for a span of its own.
    await_mapping_state(&router, "re-keyed", &encrypted_uuid, STATE_ENABLED).await;

    let refused = update_mapping(
        &router,
        &encrypted_uuid,
        &json!({ "KMSKeyArn": denying.clone() }),
    )
    .await;
    assert_denied("re-keyed", "the update onto the denying key", &refused);
    let fetched = get_mapping(&router, &encrypted_uuid).await;
    assert_eq!(
        fetched.string("KMSKeyArn"),
        Some(granting.clone()),
        "case \"re-keyed\": the refused update must leave the mapping on the key it had, got {}",
        fetched.text()
    );

    let accepted = update_mapping(
        &router,
        &encrypted_uuid,
        &json!({ "FilterCriteria": { "Filters": [{ "Pattern": "{\"body\": {\"kind\": [\"other\"]}}" }] } }),
    )
    .await;
    assert_eq!(
        accepted.status,
        202,
        "case \"re-keyed\": criteria restated under the key that admits this service must be \
         accepted, got {} {}",
        accepted.status,
        accepted.text()
    );

    // Row 2: a mapping created with a key that admits this service nothing and NO criteria —
    // accepted, because it sealed nothing. An update that gives it criteria seals them under
    // that stored key, so it is refused; one that states the criteria together with a key that
    // admits the use is accepted.
    let plain_source = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:criteria-key-update-1");
    let plain = create_mapping(
        &router,
        &create_request(
            function,
            Some(&plain_source),
            &json!({ "KMSKeyArn": denying.clone() }),
        ),
    )
    .await;
    assert_eq!(
        plain.status,
        202,
        "case \"newly-sealed\": a mapping sealing nothing must be created, got {} {}",
        plain.status,
        plain.text()
    );
    let plain_uuid = plain.string("UUID").expect("the create answers a UUID");
    await_mapping_state(&router, "newly-sealed", &plain_uuid, STATE_ENABLED).await;

    let refused = update_mapping(&router, &plain_uuid, &json!({ "FilterCriteria": criteria() })).await;
    assert_denied("newly-sealed", "the update that seals criteria", &refused);
    let fetched = get_mapping(&router, &plain_uuid).await;
    assert_eq!(
        fetched.json().get("FilterCriteria"),
        None,
        "case \"newly-sealed\": the refused update must leave the mapping without criteria, got \
         {}",
        fetched.text()
    );

    let accepted = update_mapping(
        &router,
        &plain_uuid,
        &json!({ "FilterCriteria": criteria(), "KMSKeyArn": granting.clone() }),
    )
    .await;
    assert_eq!(
        accepted.status,
        202,
        "case \"newly-sealed\": criteria stated with a key that admits the use must be accepted, \
         got {} {}",
        accepted.status,
        accepted.text()
    );
    let fetched = get_mapping(&router, &plain_uuid).await;
    assert_eq!(
        (
            fetched.string("KMSKeyArn"),
            fetched.json().get("FilterCriteria").cloned()
        ),
        (Some(granting), Some(criteria())),
        "case \"newly-sealed\": the accepted update must be what a later caller reads, got {}",
        fetched.text()
    );
}
