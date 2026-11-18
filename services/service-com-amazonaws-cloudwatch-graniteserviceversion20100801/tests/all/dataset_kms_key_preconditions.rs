// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the KMS preconditions `AssociateDatasetKmsKey` takes before it writes an
//! association, driven over the wire with CloudWatch and AWS KMS on one engine — so what decides
//! each row is the key AWS KMS really holds and the policy `PutKeyPolicy` really left on it, not
//! anything this suite states.
//!
//! "Before completing the association, Amazon CloudWatch validates the key by performing a series
//! of dry-run KMS operations. Service-principal checks run first to verify that the key policy
//! grants the required access to Amazon CloudWatch." (`AssociateDatasetKmsKey`) — so an
//! association takes effect only for a key this service can really use, and the rows below span
//! what makes a key usable or not: the grant its key policy states for this service's principal
//! (none, the documented operations, every operation, an explicit deny, a grant scoped to one
//! service's endpoint), the STATE the key is in, the KIND of key it is, and whether KMS holds a
//! key of that ARN at all.
//!
//! Every refused row asserts the same two things the recorded behaviour asserts: the refusal a
//! client branches on — one wire error code for EVERY failure of the dry run, in the request's
//! own status class, rather than one of the three modelled 400 faults whose sentences describe
//! those conditions — and that the association did NOT take effect, so `GetDataset` still
//! reports what it reported before the call. The dry run does not report WHICH of its checks
//! failed, so every outcome but the key's own state carries one sentence naming the key
//! (`crate::state::dataset_kms_key::unable_to_validate_message`) and a key KMS holds in a state
//! that admits no cryptographic operation carries the other one.
//!
//! The emulator and the KMS wire calls come from the suite that drives this service's OTHER
//! decision over a customer managed key (`sns_action_publish_grant`), so KMS is mounted and asked
//! one way across both.

use serde_json::{json, Value};

use crate::harness::{call, error_code, error_message, Emulator, DEFAULT_REGION};
use crate::sns_action_publish_grant::{
    account_root_statement, emulator_with_kms, key_policy, key_statement, kms, set_key_policy,
};
use crate::sns_action_topic_policy::CLOUDWATCH_PRINCIPAL;

/// The permissions AWS documents on the key of an encrypted dataset: "Its key policy must grant
/// the CloudWatch service principal (cloudwatch.amazonaws.com) these permissions: kms:DescribeKey,
/// kms:GenerateDataKey, kms:Encrypt, kms:Decrypt, and kms:ReEncrypt*."
/// (`AssociateDatasetKmsKey`) — the set a key's owner writes, which is what the permitted rows
/// grant.
const DOCUMENTED_PERMISSIONS: [&str; 5] = [
    "kms:DescribeKey",
    "kms:GenerateDataKey",
    "kms:Encrypt",
    "kms:Decrypt",
    "kms:ReEncrypt*",
];

/// The wire error code AWS answers a key this service cannot use with — the one code EVERY
/// usability failure surfaces as, rather than one per failure mode.
const DENIED_CODE: &str = "AccessDenied";

/// The permissions a caller of this operation holds on the key beside this service's own: "The
/// calling principal must have kms:Decrypt permission on the key." (`AssociateDatasetKmsKey`) —
/// what the rows that DENY the caller take away.
const CALLER_PERMISSION: &str = "kms:Decrypt";

/// An ARN of the form `AssociateDatasetKmsKeyInput$KmsKeyArn` requires, for the rows whose key
/// AWS KMS holds nothing for.
const UNHELD_KEY: &str = "arn:aws:kms:us-east-1:000000000000:key/\
                          11111111-2222-4333-8444-555555555555";

/// A customer managed key as `CreateKey` mints it, carrying `policy` and the key spec and usage
/// `spec` names: its ARN as KMS reports it.
async fn given_key_with(
    emulator: &Emulator,
    case: &str,
    policy: &str,
    spec: Option<(&str, &str)>,
) -> String {
    let mut request = json!({
        "Policy": policy,
        "Description": "the key a dataset's data is sealed under",
    });
    if let Some((key_spec, key_usage)) = spec {
        request["KeySpec"] = json!(key_spec);
        request["KeyUsage"] = json!(key_usage);
    }
    let created = kms(emulator, case, "CreateKey", &request).await;
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reported no Arn: {created}"))
        .to_string()
}

/// One key-policy statement about this service's principal.
fn service_statement(effect: &str, actions: Value, extra: Value) -> Value {
    key_statement(effect, json!({"Service": CLOUDWATCH_PRINCIPAL}), actions, extra)
}

/// The key policy AWS documents for a dataset's key: the documented permissions, granted to this
/// service's principal.
fn documented_policy() -> String {
    key_policy(vec![service_statement(
        "Allow",
        json!(DOCUMENTED_PERMISSIONS),
        json!({}),
    )])
}

/// Associate `key_arn` with this Region's default dataset, answering the status and body.
async fn associate(emulator: &Emulator, key_arn: &str) -> (u16, Value) {
    call(
        &emulator.router,
        "AssociateDatasetKmsKey",
        &json!({"DatasetIdentifier": "default", "KmsKeyArn": key_arn}),
    )
    .await
}

/// The `KmsKeyArn` `GetDataset` reports for the default dataset, or `None` when it omits it.
async fn reported_key(emulator: &Emulator, case: &str) -> Option<String> {
    let (status, body) = call(
        &emulator.router,
        "GetDataset",
        &json!({"DatasetIdentifier": "default"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the dataset back must answer HTTP 200, got {status} with {body}"
    );
    body.get("KmsKeyArn")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// What a row expects of the association.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The key is usable, so the association takes effect and the dataset reports it.
    Associated,
    /// The dry run did not pass on the KEY: the one refusal every such failure carries, in the
    /// request's own status class, and which sentence of the two that refusal states.
    Denied(Sentence),
    /// The dry run did not pass on the CALLER's own authority over the key: the same wire code,
    /// in the status an authorization failure carries.
    DeniedToTheCaller,
}

/// Which sentence a refusal states — the ones the dry run tells apart. Each is the text the
/// behavioural recordings of this operation compare exactly, written out here rather than read
/// off the implementation's own constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sentence {
    /// The key could not be validated for the association, whichever check found that.
    NotValidated,
    /// KMS holds the key in a state that admits no cryptographic operation.
    Disabled,
    /// The authorization refusal of the dry run run with the CALLER's own credentials, over the
    /// calling principal and the key.
    TheCallersOwn,
}

impl Sentence {
    /// The whole message this sentence states about `key_arn`, as the service words it.
    fn message(self, key_arn: &str) -> String {
        match self {
            Self::NotValidated => format!(
                "Unable to validate KMS key {key_arn}. Verify the key exists and the key policy \
                 grants kms:DescribeKey to the CloudWatch service principal."
            ),
            Self::Disabled => format!("KMS key {key_arn} is disabled."),
            Self::TheCallersOwn => format!(
                "User: {} is not authorized to perform: kms:Decrypt on resource: {key_arn} \
                 with an explicit deny in a resource-based policy",
                service_common::caller_principal::ambient_user_arn()
            ),
        }
    }
}

/// How a row sets its key up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// A symmetric encryption key carrying the policy the row names.
    WithPolicy(Policy),
    /// A key carrying the documented policy, disabled after it was created.
    Disabled,
    /// An ASYMMETRIC encryption key carrying the documented policy.
    Asymmetric,
    /// An ARN of the right form that KMS holds no key for.
    Unheld,
}

/// The grant a row's key policy states for this service's principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// Nothing beside the statement a new key carries for the account root.
    AccountRootOnly,
    /// The permissions AWS documents for a dataset's key.
    Documented,
    /// Every KMS action, by wildcard.
    EveryAction,
    /// The documented permissions, and an explicit deny of one of them.
    DocumentedWithExplicitDeny,
    /// The documented permissions, granted only when the key is reached through THIS service's
    /// endpoint — the condition such a grant is scoped by.
    ViaThisService,
    /// The same grant scoped to ANOTHER service's endpoint, which this use is not made through.
    ViaAnotherService,
    /// The documented permissions, with the CALLER's own `kms:Decrypt` denied outright.
    DocumentedWithTheCallerDenied,
}

impl Policy {
    /// The policy document this grant is written as.
    fn document(self) -> String {
        match self {
            Self::AccountRootOnly => {
                json!({"Version": "2012-10-17", "Statement": [account_root_statement()]})
                    .to_string()
            }
            Self::Documented => documented_policy(),
            Self::EveryAction => {
                key_policy(vec![service_statement("Allow", json!("kms:*"), json!({}))])
            }
            Self::DocumentedWithExplicitDeny => key_policy(vec![
                service_statement("Allow", json!(DOCUMENTED_PERMISSIONS), json!({})),
                service_statement("Deny", json!("kms:Encrypt"), json!({})),
            ]),
            Self::ViaThisService => key_policy(vec![service_statement(
                "Allow",
                json!(DOCUMENTED_PERMISSIONS),
                json!({"Condition": {"StringEquals": {
                    "kms:ViaService": format!("cloudwatch.{DEFAULT_REGION}.amazonaws.com"),
                }}}),
            )]),
            Self::ViaAnotherService => key_policy(vec![service_statement(
                "Allow",
                json!(DOCUMENTED_PERMISSIONS),
                json!({"Condition": {"StringEquals": {
                    "kms:ViaService": format!("sns.{DEFAULT_REGION}.amazonaws.com"),
                }}}),
            )]),
            // The caller of this emulator is the principal its own published credential speaks
            // for, which is what `sts:GetCallerIdentity` reports for such a request.
            Self::DocumentedWithTheCallerDenied => key_policy(vec![
                service_statement("Allow", json!(DOCUMENTED_PERMISSIONS), json!({})),
                key_statement(
                    "Deny",
                    json!({"AWS": service_common::caller_principal::ambient_user_arn()}),
                    json!(CALLER_PERMISSION),
                    json!({}),
                ),
            ]),
        }
    }
}

/// Set `key` up in KMS and answer the ARN the association names.
async fn given(emulator: &Emulator, case: &str, key: Key) -> String {
    match key {
        Key::WithPolicy(policy) => given_key_with(emulator, case, &policy.document(), None).await,
        Key::Disabled => {
            let arn = given_key_with(emulator, case, &documented_policy(), None).await;
            kms(emulator, case, "DisableKey", &json!({"KeyId": arn.clone()})).await;
            arn
        }
        // "Asymmetric keys … are not supported." — an RSA encryption key is still a key whose
        // policy grants everything the association asks for, so only its KIND can refuse it.
        Key::Asymmetric => {
            given_key_with(
                emulator,
                case,
                &documented_policy(),
                Some(("RSA_2048", "ENCRYPT_DECRYPT")),
            )
            .await
        }
        Key::Unheld => UNHELD_KEY.to_string(),
    }
}

/// An association takes effect exactly when AWS KMS says the key it names is one this service can
/// use, and a refused one leaves the dataset with no key at all.
#[tokio::test]
async fn an_association_takes_effect_only_for_a_key_this_service_can_use() {
    // (the case, how the key is set up, what the association must answer)
    let rows: [(&str, Key, Expected); 10] = [
        (
            "a key whose policy names only the account root",
            Key::WithPolicy(Policy::AccountRootOnly),
            Expected::Denied(Sentence::NotValidated),
        ),
        (
            "a key whose policy grants the documented permissions",
            Key::WithPolicy(Policy::Documented),
            Expected::Associated,
        ),
        (
            "a key whose policy grants every KMS action",
            Key::WithPolicy(Policy::EveryAction),
            Expected::Associated,
        ),
        (
            "a key whose policy denies one documented permission outright",
            Key::WithPolicy(Policy::DocumentedWithExplicitDeny),
            Expected::Denied(Sentence::NotValidated),
        ),
        (
            "a key granted through this service's own endpoint",
            Key::WithPolicy(Policy::ViaThisService),
            Expected::Associated,
        ),
        (
            "a key granted only through another service's endpoint",
            Key::WithPolicy(Policy::ViaAnotherService),
            Expected::Denied(Sentence::NotValidated),
        ),
        (
            "a key whose policy denies the CALLER the one operation it needs",
            Key::WithPolicy(Policy::DocumentedWithTheCallerDenied),
            Expected::DeniedToTheCaller,
        ),
        (
            "a permissioned key that has been disabled",
            Key::Disabled,
            Expected::Denied(Sentence::Disabled),
        ),
        (
            "a permissioned asymmetric key",
            Key::Asymmetric,
            Expected::Denied(Sentence::NotValidated),
        ),
        (
            "an ARN KMS holds no key for",
            Key::Unheld,
            Expected::Denied(Sentence::NotValidated),
        ),
    ];

    for (case, key, expected) in rows {
        // One emulator per row, so each row's dataset starts with no association and what the
        // row asserts about the dataset is its own call's doing.
        let emulator = emulator_with_kms();
        let key_arn = given(&emulator, case, key).await;
        let (status, answered) = associate(&emulator, &key_arn).await;
        match expected {
            Expected::Associated => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the association to be accepted, got {status} with \
                     {answered}"
                );
                assert_eq!(
                    reported_key(&emulator, case).await,
                    Some(key_arn.clone()),
                    "case {case:?}: the dataset must report the key it was associated with"
                );
            }
            Expected::Denied(sentence) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with {answered}"
                );
                assert_eq!(
                    error_code(&answered),
                    DENIED_CODE,
                    "case {case:?}: expected the {DENIED_CODE} code, got {answered}"
                );
                let message = error_message(&answered);
                assert_eq!(
                    message,
                    sentence.message(&key_arn),
                    "case {case:?}: expected the {sentence:?} sentence naming the key, got \
                     {message:?}"
                );
                assert_eq!(
                    reported_key(&emulator, case).await,
                    None,
                    "case {case:?}: a refused association must leave the dataset with no key"
                );
            }
            Expected::DeniedToTheCaller => {
                assert_eq!(
                    status, 403,
                    "case {case:?}: expected HTTP 403, got {status} with {answered}"
                );
                assert_eq!(
                    error_code(&answered),
                    DENIED_CODE,
                    "case {case:?}: expected the {DENIED_CODE} code, got {answered}"
                );
                // The authorization refusal over the calling principal and the key, which is
                // what the dry run run with the caller's credentials answers.
                let message = error_message(&answered);
                assert_eq!(
                    message,
                    Sentence::TheCallersOwn.message(&key_arn),
                    "case {case:?}: expected the refusal of the caller's own dry run, got \
                     {message:?}"
                );
                assert_eq!(
                    reported_key(&emulator, case).await,
                    None,
                    "case {case:?}: a refused association must leave the dataset with no key"
                );
            }
        }
    }
}

/// How a row repairs the precondition its key broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repair {
    /// The key's owner writes the documented grant into its key policy.
    GrantTheServicePrincipal,
    /// The key's owner re-enables the key.
    EnableTheKey,
}

/// Once the precondition is repaired, the IDENTICAL call succeeds and the dataset reports the key:
/// what the refusal withheld is the key's own condition, not anything this service remembered
/// about the request.
#[tokio::test]
async fn the_same_call_succeeds_once_the_key_is_repaired() {
    // (the case, how the key starts out, how its owner repairs it, the sentence the first
    //  refusal states, the status it carries)
    let rows: [(&str, Key, Repair, Sentence, u16); 3] = [
        (
            "a key policy that grants this service nothing",
            Key::WithPolicy(Policy::AccountRootOnly),
            Repair::GrantTheServicePrincipal,
            Sentence::NotValidated,
            400,
        ),
        (
            // The caller's own dry run is the one refused in the authorization class, with the
            // authorization refusal over the calling principal and the key.
            "a key policy that denies the caller the operation it needs",
            Key::WithPolicy(Policy::DocumentedWithTheCallerDenied),
            Repair::GrantTheServicePrincipal,
            Sentence::TheCallersOwn,
            403,
        ),
        (
            "a permissioned key that has been disabled",
            Key::Disabled,
            Repair::EnableTheKey,
            Sentence::Disabled,
            400,
        ),
    ];

    for (case, key, repair, sentence, refused_status) in rows {
        let emulator = emulator_with_kms();
        let key_arn = given(&emulator, case, key).await;
        let (status, answered) = associate(&emulator, &key_arn).await;
        assert_eq!(
            status, refused_status,
            "case {case:?}: expected the unrepaired key to be refused, got {status} with \
             {answered}"
        );
        assert_eq!(
            error_code(&answered),
            DENIED_CODE,
            "case {case:?}: expected the {DENIED_CODE} code, got {answered}"
        );
        let message = error_message(&answered);
        assert_eq!(
            message,
            sentence.message(&key_arn),
            "case {case:?}: expected the {sentence:?} sentence naming the key, got {message:?}"
        );

        match repair {
            Repair::GrantTheServicePrincipal => {
                set_key_policy(&emulator, case, &key_arn, &documented_policy()).await;
            }
            Repair::EnableTheKey => {
                kms(
                    &emulator,
                    case,
                    "EnableKey",
                    &json!({"KeyId": key_arn.clone()}),
                )
                .await;
            }
        }

        let (status, answered) = associate(&emulator, &key_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the repaired key to be accepted, got {status} with \
             {answered}"
        );
        assert_eq!(
            reported_key(&emulator, case).await,
            Some(key_arn.clone()),
            "case {case:?}: the repaired association must be the one the dataset reports"
        );
    }
}

/// "If any of these checks on the new key fails, the operation fails and the existing key
/// association (if any) remains unchanged." — a refused REPLACEMENT leaves the key the dataset
/// already carries in place.
#[tokio::test]
async fn a_refused_replacement_leaves_the_existing_association_in_place() {
    let emulator = emulator_with_kms();
    let case = "replacing a usable key with one this service cannot use";
    let held = given(&emulator, case, Key::WithPolicy(Policy::Documented)).await;
    let (status, answered) = associate(&emulator, &held).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first association must be accepted, got {status} with {answered}"
    );

    for replacement in [
        Key::WithPolicy(Policy::AccountRootOnly),
        Key::Disabled,
        Key::Unheld,
    ] {
        let case = format!("{case} ({replacement:?})");
        let unusable = given(&emulator, &case, replacement).await;
        let (status, answered) = associate(&emulator, &unusable).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the replacement must be refused, got {status} with {answered}"
        );
        assert_eq!(
            reported_key(&emulator, &case).await,
            Some(held.clone()),
            "case {case:?}: the dataset must still report the key it already carried"
        );
    }
}

/// With no KMS to ask, the key's own preconditions establish NOTHING, so the association is
/// written exactly as it was before this service asked anything of KMS.
///
/// The deliberate fallback: this service refuses a key on what KMS SAID about it, never on a
/// question that went unanswered — so a deployment that mounts CloudWatch alone still binds a
/// well-formed key ARN, and the same ARN against a mounted KMS that holds no such key is refused
/// (the table above). Pinned here so a later change cannot turn an unanswered question into a
/// refusal.
#[tokio::test]
async fn an_association_stands_where_no_kms_answers_for_the_key() {
    let (router, _engine) = crate::harness::build_router_with_engine();
    let case = "a well-formed key ARN with no KMS mounted";
    let (status, answered) = call(
        &router,
        "AssociateDatasetKmsKey",
        &json!({"DatasetIdentifier": "default", "KmsKeyArn": UNHELD_KEY}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the association to be accepted, got {status} with {answered}"
    );
    let (status, body) = call(
        &router,
        "GetDataset",
        &json!({"DatasetIdentifier": "default"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the dataset back must answer HTTP 200, got {status} with {body}"
    );
    assert_eq!(
        body.get("KmsKeyArn").and_then(Value::as_str),
        Some(UNHELD_KEY),
        "case {case:?}: the dataset must report the key it was associated with, got {body}"
    );
}
