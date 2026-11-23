// SPDX-License-Identifier: CC0-1.0

//! How Secrets Manager answers an operation whose KMS call on the secret's customer
//! managed key the caller's authority does not reach.
//!
//! The rule this suite asserts: a secret under a customer managed key is read by
//! decrypting it through that key and written by generating a data key from it, each on
//! the CALLER's behalf — so when the authority in force on that key stops reaching the
//! cryptographic operation, the call is refused `AccessDeniedException` (HTTP 400) and
//! the refusal reads exactly "Access to KMS is not allowed": Secrets Manager's own
//! sentence for the class, naming neither the principal, nor the action, nor the key,
//! and carrying nothing of the answer KMS gave the service. The refusal follows the
//! authority, so taking it away refuses the call and giving it back serves it again,
//! and a refused write stores nothing.
//!
//! The rows span what the rule generalises over rather than one recorded request:
//!
//! * HOW the authority goes away — the key policy statement replaced without the
//!   cryptographic actions, and the grant that carried them revoked, which are the two
//!   ways a caller's use of a key is withdrawn;
//! * WHICH operation needed the access — `GetSecretValue` addressing its version three
//!   ways, `BatchGetSecretValue`, whose refusal belongs to the one entry, and the three
//!   writes that have to seal a value (`PutSecretValue`, `UpdateSecret`, `CreateSecret`);
//! * the member the value travels in, since the envelope is the same for both.
//!
//! The other half of the same rule, asserted by the last test here: only the secret's
//! VALUE is sealed under that key — "Retrieves the details of a secret. It does not
//! include the encrypted secret value" (`DescribeSecret`, this service's Smithy model),
//! whose required permission is `secretsmanager:DescribeSecret` alone — so every
//! operation that reports or changes METADATA keeps answering, and keeps enumerating the
//! secret, while the authority on the key no longer reaches the cryptographic operation a
//! value needs. Its rows add the third way that authority is withdrawn: a key policy
//! clause that admits a cryptographic use only when it arrives through one service's path
//! to the key (`kms:ViaService`), which confines the key rather than revoking it — so the
//! envelope this service makes on the caller's behalf is served through the clause that
//! names THIS service while the caller's own direct use of the key is refused, and
//! refused through the clause that names another.
//!
//! KMS is mounted beside Secrets Manager on ONE engine, as `core/router/src/lib.rs`
//! mounts them, so every key, policy and grant below is minted through KMS's own
//! operations. The harness is [`crate::secret_kms_key_state`]'s, which is the suite that
//! owns the rest of the envelope's behaviour.

use serde_json::{json, Map, Value};

use crate::secret_kms_key_state::{create_body, key_policy, version_count, Emulator, REGION};
use crate::secret_lifecycle::ValueCase;

/// The whole `Message` of the `AccessDeniedException` Secrets Manager refuses an
/// operation with when the KMS call it makes for the secret's key is not allowed for the
/// caller: "Access to KMS is not allowed".
///
/// The service states only that the access it needed was not allowed. The cryptographic
/// call is the SERVICE's, so the answer KMS gave it — which principal, which action,
/// which key — is not passed on to the caller, and the sentence has no variable part to
/// fill from the request or the store.
pub(crate) const KMS_ACCESS_DENIED_MESSAGE: &str = "Access to KMS is not allowed";

/// The error code every refusal in this suite carries, which is the AWS common one.
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";

/// The cryptographic action on the secret's key an operation needs, as the key policy
/// and the grants below name it.
///
/// Read from what each path performs on the key: a read unwraps the data key the version
/// was sealed under (`kms:Decrypt`), and a write asks the key for a fresh data key
/// (`kms:GenerateDataKey`) — "you also need kms:Decrypt permissions for that key"
/// (`GetSecretValue`, this service's Smithy model) and "you need … kms:GenerateDataKey"
/// (`CreateSecret`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    Decrypt,
    GenerateDataKey,
}

impl KeyAction {
    /// Both of them, which is what a secret under a customer managed key needs over its
    /// whole life: one to write a value and one to read it back.
    const BOTH: [KeyAction; 2] = [Self::Decrypt, Self::GenerateDataKey];
    /// The IAM action this is, as a policy statement and a grant operation name it.
    fn iam(self) -> &'static str {
        match self {
            Self::Decrypt => "kms:Decrypt",
            Self::GenerateDataKey => "kms:GenerateDataKey",
        }
    }

    /// The KMS operation a grant lists to carry it (`GrantOperation`, KMS's own model).
    fn grant_operation(self) -> &'static str {
        match self {
            Self::Decrypt => "Decrypt",
            Self::GenerateDataKey => "GenerateDataKey",
        }
    }

    /// Every cryptographic action BUT this one, as a key policy statement names them: the
    /// policy that keeps a secret's other path working and takes away only this one.
    fn others(self) -> Vec<&'static str> {
        Self::BOTH
            .into_iter()
            .filter(|action| *action != self)
            .map(Self::iam)
            .collect()
    }
}

/// How a refusal reaches the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    /// The whole call is refused, so the refusal is the response's own error.
    WholeCall,
    /// The call answers 200 and the refusal belongs to the ONE entry it covers: "If
    /// Secrets Manager encounters errors … while attempting to retrieve any of the
    /// secrets, you can see the errors in Errors in the response."
    /// (`BatchGetSecretValue`, this service's Smithy model.)
    OneEntry,
}

/// Everything a row's request can be built from: the secret under the key, the version
/// its value lives in, the key itself, and a name no secret holds yet.
struct Target<'a> {
    name: &'a str,
    version_id: &'a str,
    key_arn: &'a str,
    fresh: &'a str,
}

/// One operation that cannot be served without the key, and how it is refused.
struct OperationRow {
    case: &'static str,
    operation: &'static str,
    /// The action on the key this operation performs, which is the one the row's removal
    /// takes away.
    action: KeyAction,
    refused: Refused,
    /// Whether the call would have STORED a version, so a refused one must be shown to
    /// have stored none.
    writes: bool,
    body: fn(&Target<'_>) -> Value,
}

/// The replacement value a write row stores, which the refusal must leave unstored.
const REPLACEMENT: &str = "the value a refused write must not store";

/// The operations under test: every one that has to reach the secret's key, addressing
/// the version by nothing, by id and by staging label, and both the single and the
/// batched read beside the three writes.
const OPERATION_ROWS: &[OperationRow] = &[
    OperationRow {
        case: "GetSecretValue naming no version",
        operation: "GetSecretValue",
        action: KeyAction::Decrypt,
        refused: Refused::WholeCall,
        writes: false,
        body: |target| json!({"SecretId": target.name}),
    },
    OperationRow {
        case: "GetSecretValue naming the version id",
        operation: "GetSecretValue",
        action: KeyAction::Decrypt,
        refused: Refused::WholeCall,
        writes: false,
        body: |target| json!({"SecretId": target.name, "VersionId": target.version_id}),
    },
    OperationRow {
        case: "GetSecretValue naming the staging label",
        operation: "GetSecretValue",
        action: KeyAction::Decrypt,
        refused: Refused::WholeCall,
        writes: false,
        body: |target| json!({"SecretId": target.name, "VersionStage": "AWSCURRENT"}),
    },
    OperationRow {
        case: "BatchGetSecretValue listing the secret",
        operation: "BatchGetSecretValue",
        action: KeyAction::Decrypt,
        refused: Refused::OneEntry,
        writes: false,
        body: |target| json!({"SecretIdList": [target.name]}),
    },
    OperationRow {
        case: "PutSecretValue storing another value",
        operation: "PutSecretValue",
        action: KeyAction::GenerateDataKey,
        refused: Refused::WholeCall,
        writes: true,
        body: |target| json!({"SecretId": target.name, "SecretString": REPLACEMENT}),
    },
    OperationRow {
        case: "UpdateSecret storing another value",
        operation: "UpdateSecret",
        action: KeyAction::GenerateDataKey,
        refused: Refused::WholeCall,
        writes: true,
        body: |target| json!({"SecretId": target.name, "SecretString": REPLACEMENT}),
    },
    OperationRow {
        case: "CreateSecret naming the same key",
        operation: "CreateSecret",
        action: KeyAction::GenerateDataKey,
        refused: Refused::WholeCall,
        writes: true,
        body: |target| {
            json!({
                "Name": target.fresh,
                "KmsKeyId": target.key_arn,
                "SecretString": REPLACEMENT,
            })
        },
    },
];

/// How the caller's authority on the key is taken away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removal {
    /// The key policy is replaced with one whose statement no longer names the
    /// cryptographic action this operation needs, while still naming the other one — the
    /// break-glass change that cuts a secret's reads off without touching the secret.
    KeyPolicyStatement,
    /// The key policy never named either cryptographic action and a GRANT carried them,
    /// and that grant is revoked: "Grants are an alternative to … the key policy".
    Grant,
}

impl Removal {
    fn label(self) -> &'static str {
        match self {
            Self::KeyPolicyStatement => "the key policy statement is replaced without the action",
            Self::Grant => "the grant that carried the actions is revoked",
        }
    }
}

/// The actions the key policy keeps whatever else it states, so the suite can keep
/// describing the key, replacing its policy and issuing and revoking grants on it: the
/// service asks the key about itself through `kms:DescribeKey`, and these rows change the
/// key's own authority between calls.
const POLICY_ADMINISTRATION: [&str; 3] = ["kms:CreateGrant", "kms:RevokeGrant", "kms:ListGrants"];

/// The value cases a row runs over: the two members a secret value travels in, since the
/// envelope — and so the KMS call behind it — is the same for both.
fn value_cases() -> Vec<ValueCase> {
    vec![
        ValueCase::string("a string value", "s3cr3t-under-a-key-the-caller-loses"),
        ValueCase::binary("a binary value", (0u8..=64).collect()),
    ]
}

/// RULE: when the authority in force on a secret's customer managed key stops reaching
/// the cryptographic operation an answer needs, the call is refused
/// `AccessDeniedException` (HTTP 400) whose whole message is Secrets Manager's own
/// sentence for that refusal, [`KMS_ACCESS_DENIED_MESSAGE`] — whichever operation needed
/// the access, whichever way the authority was withdrawn, and with nothing of the KMS
/// answer in it — and a refused write stores nothing.
#[tokio::test]
async fn an_operation_the_secrets_key_does_not_permit_is_refused_the_services_own_sentence() {
    for removal in [Removal::KeyPolicyStatement, Removal::Grant] {
        for (index, row) in OPERATION_ROWS.iter().enumerate() {
            for (value_index, value) in value_cases().into_iter().enumerate() {
                let case = format!("{}, {}, {}", removal.label(), row.case, value.label);
                let emulator = Emulator::new();
                let (key_arn, _) = emulator.create_key(&case).await;
                let name = format!("kms-denied-{index}-{value_index}");
                let fresh = format!("{name}-fresh");

                // The secret is created while the key still permits the caller
                // everything, so what is asserted below is the removal and nothing else.
                emulator
                    .sm_ok(
                        "CreateSecret",
                        &create_body(&name, Some(&key_arn), &value),
                        &case,
                    )
                    .await;

                // The authority the row then takes away: for a grant row the key policy
                // names no cryptographic action at all first, so the grant it issues is
                // the only authority that reaches them.
                let grant_id = match removal {
                    Removal::KeyPolicyStatement => None,
                    Removal::Grant => {
                        put_policy(&emulator, &key_arn, &[], &case).await;
                        Some(grant(&emulator, &key_arn, &KeyAction::BOTH, &case).await)
                    }
                };

                // The POSITIVE CONTROL: with that authority in force the value is served,
                // so the refusal below is the removal's doing.
                let read = emulator
                    .sm_ok("GetSecretValue", &json!({"SecretId": &name}), &case)
                    .await;
                value.assert_round_trip(&read, &case);
                let version_id = read["VersionId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: the read reported no VersionId"))
                    .to_string();

                match (removal, grant_id) {
                    (Removal::KeyPolicyStatement, _) => {
                        // A statement naming the OTHER cryptographic action and not this
                        // one: the key stays describable, its policy replaceable and the
                        // secret's other path open, and nothing reaches this one.
                        put_policy(&emulator, &key_arn, &row.action.others(), &case).await;
                    }
                    (Removal::Grant, Some(grant_id)) => {
                        emulator
                            .kms_ok(
                                "RevokeGrant",
                                &json!({"KeyId": &key_arn, "GrantId": grant_id}),
                                &case,
                            )
                            .await;
                    }
                    (Removal::Grant, None) => {
                        panic!("case {case:?}: a grant row minted no grant")
                    }
                }

                let before = version_count(&emulator, &name, &case).await;
                let target = Target {
                    name: &name,
                    version_id: &version_id,
                    key_arn: &key_arn,
                    fresh: &fresh,
                };
                let body = (row.body)(&target);
                match row.refused {
                    Refused::WholeCall => {
                        let (code, message) =
                            emulator.sm_err(row.operation, &body, &case).await;
                        assert_eq!(
                            code, ACCESS_DENIED_CODE,
                            "case {case:?}: expected {ACCESS_DENIED_CODE}, got {code} with \
                             {message:?}"
                        );
                        assert_eq!(
                            message, KMS_ACCESS_DENIED_MESSAGE,
                            "case {case:?}: expected the message {KMS_ACCESS_DENIED_MESSAGE:?}, \
                             got {message:?}"
                        );
                    }
                    Refused::OneEntry => {
                        let answered = emulator.sm_ok(row.operation, &body, &case).await;
                        let values = answered["SecretValues"].as_array();
                        assert!(
                            values.is_none_or(|values| values.is_empty()),
                            "case {case:?}: expected no value for a secret the key does not \
                             permit, got {answered}"
                        );
                        let errors = answered["Errors"]
                            .as_array()
                            .unwrap_or_else(|| panic!("case {case:?}: no Errors in {answered}"));
                        assert_eq!(
                            errors.len(),
                            1,
                            "case {case:?}: expected the one selected secret to report one \
                             error, got {answered}"
                        );
                        let error = &errors[0];
                        assert_eq!(
                            error["SecretId"].as_str(),
                            Some(name.as_str()),
                            "case {case:?}: expected the error to name the secret, got {error}"
                        );
                        assert_eq!(
                            error["ErrorCode"].as_str(),
                            Some(ACCESS_DENIED_CODE),
                            "case {case:?}: expected {ACCESS_DENIED_CODE}, got {error}"
                        );
                        assert_eq!(
                            error["Message"].as_str(),
                            Some(KMS_ACCESS_DENIED_MESSAGE),
                            "case {case:?}: expected the message \
                             {KMS_ACCESS_DENIED_MESSAGE:?}, got {error}"
                        );
                    }
                }

                // A refused write stored nothing: the secret holds the versions it held,
                // and a refused create left no secret behind.
                assert_eq!(
                    version_count(&emulator, &name, &case).await,
                    before,
                    "case {case:?}: a refused call changed the versions the secret holds"
                );
                if row.writes {
                    let (status, parsed) = emulator
                        .sm("DescribeSecret", &json!({"SecretId": &fresh}))
                        .await;
                    assert_eq!(
                        status, 400,
                        "case {case:?}: expected no secret {fresh:?} to exist, got {status} \
                         with {parsed}"
                    );
                }
            }
        }
    }
}

/// RULE (the same one, read the other way): the refusal follows the authority in force at
/// the time of the call, so a read refused once the caller's `kms:Decrypt` on the key is
/// gone is served again as soon as that authority is back — through a replaced key policy
/// or through a fresh grant — and refused again when it goes away a second time.
#[tokio::test]
async fn the_refusal_follows_the_authority_in_force_at_each_call() {
    for removal in [Removal::KeyPolicyStatement, Removal::Grant] {
        let case = format!("{}, restored", removal.label());
        let emulator = Emulator::new();
        let (key_arn, _) = emulator.create_key(&case).await;
        let name = "kms-denied-restored";
        let value = ValueCase::string("a string value", "s3cr3t-restored");
        emulator
            .sm_ok(
                "CreateSecret",
                &create_body(name, Some(&key_arn), &value),
                &case,
            )
            .await;
        let read_body = json!({"SecretId": name});

        // Two full rounds, so a refusal that latched on the first removal would be caught
        // by the second, and so would a service that stopped consulting the key once it
        // had served the value.
        let mut in_force: Option<String> = None;
        for round in 0..2 {
            let case = format!("{case}, round {round}");
            // TAKEN AWAY: the key policy stops naming the cryptographic action, and for a
            // grant row the grant that carried it is revoked as well.
            put_policy(&emulator, &key_arn, &[], &case).await;
            if let Some(grant_id) = in_force.take() {
                emulator
                    .kms_ok(
                        "RevokeGrant",
                        &json!({"KeyId": &key_arn, "GrantId": grant_id}),
                        &case,
                    )
                    .await;
            }
            let (code, message) = emulator.sm_err("GetSecretValue", &read_body, &case).await;
            assert_eq!(
                code, ACCESS_DENIED_CODE,
                "case {case:?}: expected {ACCESS_DENIED_CODE}, got {code} with {message:?}"
            );
            assert_eq!(
                message, KMS_ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected the message {KMS_ACCESS_DENIED_MESSAGE:?}, got \
                 {message:?}"
            );

            // GIVEN BACK, the same way it was taken.
            match removal {
                Removal::KeyPolicyStatement => {
                    put_policy(&emulator, &key_arn, &[KeyAction::Decrypt.iam()], &case).await;
                }
                Removal::Grant => {
                    in_force =
                        Some(grant(&emulator, &key_arn, &[KeyAction::Decrypt], &case).await);
                }
            }
            let served = emulator.sm_ok("GetSecretValue", &read_body, &case).await;
            value.assert_round_trip(&served, &case);
        }
    }
}

/// Replace the key's policy with one granting the caller `granted` beside the actions
/// this suite needs to keep ([`POLICY_ADMINISTRATION`], and the administration
/// [`crate::secret_kms_key_state::key_policy`] keeps).
async fn put_policy(emulator: &Emulator, key_arn: &str, granted: &[&str], case: &str) {
    let mut actions: Vec<&str> = POLICY_ADMINISTRATION.to_vec();
    actions.extend_from_slice(granted);
    emulator
        .kms_ok(
            "PutKeyPolicy",
            &json!({
                "KeyId": key_arn,
                "Policy": key_policy(&actions),
                // The document is the one statement under test, so it leaves no path to a
                // later PutKeyPolicy the lockout safety check would accept.
                "BypassPolicyLockoutSafetyCheck": true,
            }),
            case,
        )
        .await;
}

/// Issue a grant on `key_arn` carrying `actions` to the principal this emulator
/// authenticates every request as, and answer its `GrantId`.
async fn grant(
    emulator: &Emulator,
    key_arn: &str,
    actions: &[KeyAction],
    case: &str,
) -> String {
    let operations: Vec<&str> = actions
        .iter()
        .copied()
        .map(KeyAction::grant_operation)
        .collect();
    let mut body = Map::new();
    body.insert("KeyId".to_string(), json!(key_arn));
    body.insert(
        "GranteePrincipal".to_string(),
        json!(service_com_amazonaws_kms_trentservice::state::caller_identity_arn()),
    );
    body.insert("Operations".to_string(), json!(operations));
    let created = emulator
        .kms_ok("CreateGrant", &Value::Object(body), case)
        .await;
    created["GrantId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateGrant reported no GrantId in {created}"))
        .to_string()
}

/// The regional endpoint name of an AWS service, which is the value `kms:ViaService` carries
/// for a cryptographic request that service makes on a caller's behalf and the value a key
/// policy confining a key to one service's path to it compares against.
fn endpoint(vendor: &str) -> String {
    format!("{vendor}.{REGION}.amazonaws.com")
}

/// The endpoint prefix of THIS service, which is what a key policy confining a key to the
/// secret store's use of it names.
const THIS_SERVICE: &str = "secretsmanager";

/// Another service's endpoint prefix, which names a path to the key that no call below
/// arrives through.
const ANOTHER_SERVICE: &str = "s3";

/// The actions a statement confining a key to one service's path to it is written over: the
/// cryptographic uses of a key, two of them as action PATTERNS rather than names, which is
/// how such a statement is written.
fn cryptographic_actions() -> Value {
    json!([
        "kms:Encrypt",
        "kms:Decrypt",
        "kms:GenerateDataKey",
        "kms:GenerateDataKey*",
        "kms:ReEncrypt*",
    ])
}

/// How the use of the secret's key is taken out of the caller's hands for a row of the
/// metadata table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawal {
    /// The key policy is replaced with one naming no cryptographic action at all, which is
    /// [`Removal::KeyPolicyStatement`] read at its limit.
    KeyPolicyWithoutTheActions,
    /// The cryptographic actions reached the caller through a GRANT, and that grant is
    /// revoked ([`Removal::Grant`]).
    GrantRevoked,
    /// The key is CONFINED rather than revoked: every cryptographic use that does not arrive
    /// through the named service's path to the key is denied. The envelope arrives through
    /// this service, so the clause naming this service admits it and the clause naming
    /// another refuses it — and the caller's own direct use, which presents no value for the
    /// condition key, is refused either way.
    ConfinedTo(&'static str),
}

impl Withdrawal {
    fn label(self) -> String {
        match self {
            Self::KeyPolicyWithoutTheActions => {
                "the key policy names no cryptographic action".to_string()
            }
            Self::GrantRevoked => "the grant that carried the actions is revoked".to_string(),
            Self::ConfinedTo(vendor) => {
                format!("the key is confined to {}'s path to it", endpoint(vendor))
            }
        }
    }

    /// Whether the secret's VALUE is still served while this withdrawal is in force, which
    /// only the clause naming THIS service's path leaves open.
    fn value_served(self) -> bool {
        self == Self::ConfinedTo(THIS_SERVICE)
    }
}

/// The key policy that confines the key to `vendor`'s path to it: this caller may administer
/// and use the key, and NO principal may perform a cryptographic operation on it except
/// through that service.
fn confining_policy(vendor: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Id": "key-confined-to-one-service",
        "Statement": [
            {
                "Sid": "AdministerAndUseTheKey",
                "Effect": "Allow",
                "Principal": {
                    "AWS": service_com_amazonaws_kms_trentservice::state::caller_identity_arn(),
                },
                "Action": "kms:*",
                "Resource": "*",
            },
            {
                "Sid": "OnlyThroughTheNamedService",
                "Effect": "Deny",
                "Principal": {"AWS": "*"},
                "Action": cryptographic_actions(),
                "Resource": "*",
                "Condition": {"StringNotEquals": {"kms:ViaService": [endpoint(vendor)]}},
            },
        ],
    })
    .to_string()
}

/// What a metadata answer has to say about the secret it reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reports {
    /// The secret's own details: the key it is under and the version it holds.
    Details,
    /// An enumeration the secret appears in, with the key it is under.
    Enumerated,
    /// The secret's versions, which hold the one its value lives in.
    Versions,
    /// The secret's resource-policy slot, which names the secret whether or not a document
    /// is attached.
    PolicySlot,
}

/// One operation that reports metadata, and what its answer must say.
struct MetadataCall {
    case: &'static str,
    operation: &'static str,
    body: fn(&Target<'_>) -> Value,
    reports: Reports,
}

/// The metadata-reporting operations under test: the secret's own details, both the
/// unfiltered and the filtered enumeration it must appear in, its versions, and its
/// resource-policy slot. None of them "include the encrypted secret value"
/// (`DescribeSecret`), and each states `secretsmanager:<operation>` as its only required
/// permission (this service's Smithy model).
const METADATA_CALLS: &[MetadataCall] = &[
    MetadataCall {
        case: "DescribeSecret",
        operation: "DescribeSecret",
        body: |target| json!({"SecretId": target.name}),
        reports: Reports::Details,
    },
    MetadataCall {
        case: "ListSecrets with no filter",
        operation: "ListSecrets",
        body: |_| json!({}),
        reports: Reports::Enumerated,
    },
    MetadataCall {
        case: "ListSecrets filtered by the secret's name",
        operation: "ListSecrets",
        body: |target| json!({"Filters": [{"Key": "name", "Values": [target.name]}]}),
        reports: Reports::Enumerated,
    },
    MetadataCall {
        case: "ListSecretVersionIds",
        operation: "ListSecretVersionIds",
        body: |target| json!({"SecretId": target.name, "IncludeDeprecated": true}),
        reports: Reports::Versions,
    },
    MetadataCall {
        case: "GetResourcePolicy",
        operation: "GetResourcePolicy",
        body: |target| json!({"SecretId": target.name}),
        reports: Reports::PolicySlot,
    },
];

/// The tag a metadata WRITE attaches while the key withholds the value: "Tags are part of
/// the secret's metadata" (`TagResource`, this service's Smithy model).
const TAG_KEY: &str = "environment";
const TAG_VALUE: &str = "attached-while-the-key-withheld-the-value";

/// Assert `answered` says about the secret what this kind of metadata answer reports.
fn assert_reports(answered: &Value, reports: Reports, target: &Target<'_>, case: &str) {
    match reports {
        Reports::Details => {
            assert_eq!(
                answered["Name"].as_str(),
                Some(target.name),
                "case {case:?}: expected the details to name the secret, got {answered}"
            );
            assert_eq!(
                answered["KmsKeyId"].as_str(),
                Some(target.key_arn),
                "case {case:?}: expected the details to report the key the secret is under, \
                 got {answered}"
            );
            assert!(
                answered["VersionIdsToStages"]
                    .get(target.version_id)
                    .is_some(),
                "case {case:?}: expected the details to report the version {:?} the secret \
                 holds, got {answered}",
                target.version_id
            );
        }
        Reports::Enumerated => {
            let listed = answered["SecretList"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no SecretList in {answered}"));
            let entry = listed
                .iter()
                .find(|entry| entry["Name"].as_str() == Some(target.name))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the enumeration to hold the secret {:?}, got \
                         {answered}",
                        target.name
                    )
                });
            assert_eq!(
                entry["KmsKeyId"].as_str(),
                Some(target.key_arn),
                "case {case:?}: expected the entry to report the key the secret is under, got \
                 {entry}"
            );
        }
        Reports::Versions => {
            let versions = answered["Versions"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no Versions in {answered}"));
            assert!(
                versions
                    .iter()
                    .any(|version| version["VersionId"].as_str() == Some(target.version_id)),
                "case {case:?}: expected the versions to hold {:?}, got {answered}",
                target.version_id
            );
        }
        Reports::PolicySlot => {
            assert_eq!(
                answered["Name"].as_str(),
                Some(target.name),
                "case {case:?}: expected the resource-policy slot to name the secret, got \
                 {answered}"
            );
            assert!(
                answered["ARN"]
                    .as_str()
                    .is_some_and(|arn| arn.contains(target.name)),
                "case {case:?}: expected the resource-policy slot to report the secret's ARN, \
                 got {answered}"
            );
        }
    }
}

/// Assert the caller's OWN direct cryptographic call on the key is refused: such a request
/// presents no value for `kms:ViaService`, so a clause comparing that key fires on it
/// whichever service's path the clause admits.
async fn assert_direct_use_refused(emulator: &Emulator, key_arn: &str, case: &str) {
    let (status, answered) = emulator
        .kms(
            "GenerateDataKey",
            &json!({"KeyId": key_arn, "KeySpec": "AES_256"}),
        )
        .await;
    let raw = answered
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        (status, raw.split('#').next_back().unwrap_or(raw)),
        (400, ACCESS_DENIED_CODE),
        "case {case:?}: expected the caller's own direct kms:GenerateDataKey to be refused \
         HTTP 400 {ACCESS_DENIED_CODE}, got {status} with {answered}"
    );
}

/// RULE: only a secret's VALUE is sealed under its customer managed key — the metadata is
/// not — so the operations that report or change metadata keep answering, and keep
/// enumerating the secret, however the authority in force on that key stops reaching the
/// cryptographic operation a value needs; and a clause that admits a cryptographic use only
/// through THIS service's path to the key serves the envelope this service makes on the
/// caller's behalf while refusing the caller's own direct use of the key.
#[tokio::test]
async fn the_metadata_of_a_secret_answers_while_its_key_withholds_the_value() {
    for (index, withdrawal) in [
        Withdrawal::KeyPolicyWithoutTheActions,
        Withdrawal::GrantRevoked,
        Withdrawal::ConfinedTo(ANOTHER_SERVICE),
        Withdrawal::ConfinedTo(THIS_SERVICE),
    ]
    .into_iter()
    .enumerate()
    {
        let case = withdrawal.label();
        let emulator = Emulator::new();
        let (key_arn, _) = emulator.create_key(&case).await;
        let name = format!("kms-denied-metadata-{index}");
        let value = ValueCase::string("a string value", "s3cr3t-behind-a-withheld-key");

        // The secret is created while the key still permits the caller everything, so what
        // the rows assert below is the withdrawal and nothing else.
        let created = emulator
            .sm_ok(
                "CreateSecret",
                &create_body(&name, Some(&key_arn), &value),
                &case,
            )
            .await;
        let version_id = created["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateSecret reported no VersionId"))
            .to_string();

        // For the grant row the key policy names no cryptographic action first, so the grant
        // it issues is the only authority that reaches them.
        let grant_id = match withdrawal {
            Withdrawal::GrantRevoked => {
                put_policy(&emulator, &key_arn, &[], &case).await;
                Some(grant(&emulator, &key_arn, &KeyAction::BOTH, &case).await)
            }
            _ => None,
        };

        // The POSITIVE CONTROL: the value is served while the authority is in force.
        let read = emulator
            .sm_ok("GetSecretValue", &json!({"SecretId": &name}), &case)
            .await;
        value.assert_round_trip(&read, &case);

        // The withdrawal itself.
        match (withdrawal, grant_id) {
            (Withdrawal::KeyPolicyWithoutTheActions, _) => {
                put_policy(&emulator, &key_arn, &[], &case).await;
            }
            (Withdrawal::GrantRevoked, Some(grant_id)) => {
                emulator
                    .kms_ok(
                        "RevokeGrant",
                        &json!({"KeyId": &key_arn, "GrantId": grant_id}),
                        &case,
                    )
                    .await;
            }
            (Withdrawal::GrantRevoked, None) => {
                panic!("case {case:?}: the grant row minted no grant")
            }
            (Withdrawal::ConfinedTo(vendor), _) => {
                emulator
                    .kms_ok(
                        "PutKeyPolicy",
                        &json!({
                            "KeyId": &key_arn,
                            "Policy": confining_policy(vendor),
                            // The document is the one statement under test, so it leaves no
                            // path to a later PutKeyPolicy the lockout check would accept.
                            "BypassPolicyLockoutSafetyCheck": true,
                        }),
                        &case,
                    )
                    .await;
            }
        }

        // The VALUE plane, which is what the key decides: refused with this service's own
        // sentence wherever the withdrawal reaches the envelope's own call, and served
        // through the clause that admits this service's path to the key — where a write
        // through the envelope stores a new version and both versions read back.
        let before = version_count(&emulator, &name, &case).await;
        if withdrawal.value_served() {
            let served = emulator
                .sm_ok("GetSecretValue", &json!({"SecretId": &name}), &case)
                .await;
            value.assert_round_trip(&served, &case);
            let replacement = ValueCase::string("a replacement", "written-through-this-service");
            let mut write = Map::new();
            write.insert("SecretId".to_string(), json!(&name));
            if let Value::Object(members) = replacement.wire_members() {
                write.extend(members);
            }
            let written = emulator
                .sm_ok("PutSecretValue", &Value::Object(write), &case)
                .await;
            assert_ne!(
                written["VersionId"].as_str(),
                Some(version_id.as_str()),
                "case {case:?}: expected the write through this service's path to store a new \
                 version, got {written}"
            );
            assert_eq!(
                version_count(&emulator, &name, &case).await,
                before + 1,
                "case {case:?}: expected the served write to store one version"
            );
            let baseline = emulator
                .sm_ok(
                    "GetSecretValue",
                    &json!({"SecretId": &name, "VersionId": &version_id}),
                    &case,
                )
                .await;
            value.assert_round_trip(&baseline, &case);
        } else {
            let (code, message) = emulator
                .sm_err("GetSecretValue", &json!({"SecretId": &name}), &case)
                .await;
            assert_eq!(
                code, ACCESS_DENIED_CODE,
                "case {case:?}: expected {ACCESS_DENIED_CODE}, got {code} with {message:?}"
            );
            assert_eq!(
                message, KMS_ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected the message {KMS_ACCESS_DENIED_MESSAGE:?}, got \
                 {message:?}"
            );
        }

        // A confining clause compares HOW the request arrived, so the caller's own direct use
        // of the key is refused whichever service's path that clause admits — which is what
        // shows the clause is in force for the row the envelope is served on.
        if matches!(withdrawal, Withdrawal::ConfinedTo(_)) {
            assert_direct_use_refused(&emulator, &key_arn, &case).await;
        }

        // The METADATA plane, in the same window: every reporting operation answers and says
        // what it says about the secret, the key it is under included.
        let target = Target {
            name: &name,
            version_id: &version_id,
            key_arn: &key_arn,
            fresh: &name,
        };
        for call in METADATA_CALLS {
            let case = format!("{case}, {}", call.case);
            let answered = emulator
                .sm_ok(call.operation, &(call.body)(&target), &case)
                .await;
            assert_reports(&answered, call.reports, &target, &case);
        }

        // And a metadata WRITE is applied, because no value is sealed to apply it: the tag
        // attaches and the details report it.
        let tag_case = format!("{case}, TagResource");
        emulator
            .sm_ok(
                "TagResource",
                &json!({"SecretId": &name, "Tags": [{"Key": TAG_KEY, "Value": TAG_VALUE}]}),
                &tag_case,
            )
            .await;
        let described = emulator
            .sm_ok("DescribeSecret", &json!({"SecretId": &name}), &tag_case)
            .await;
        let tags = described["Tags"]
            .as_array()
            .unwrap_or_else(|| panic!("case {tag_case:?}: no Tags in {described}"));
        assert!(
            tags.iter().any(|tag| {
                tag["Key"].as_str() == Some(TAG_KEY) && tag["Value"].as_str() == Some(TAG_VALUE)
            }),
            "case {tag_case:?}: expected the tag attached while the key withheld the value, \
             got {described}"
        );
    }
}
