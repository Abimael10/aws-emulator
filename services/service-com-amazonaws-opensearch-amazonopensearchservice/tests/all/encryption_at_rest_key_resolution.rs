// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the KMS key an enabled `EncryptionAtRestOptions` names — the
//! container `CreateDomain` and `UpdateDomainConfig` both take.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees. The keys the rows name are created, disabled, re-enabled, aliased and scheduled
//! for deletion through the AWS KMS service's OWN operations, mounted on the same shared
//! engine ([`Emulator::with_kms`]), so the keys under test are the ones that service
//! stores and a key's state is the state its own `DisableKey` wrote.
//!
//! The rule under test, asserted over a table rather than one call: a configuration that
//! ENABLES encryption at rest is accepted only when the key it names is a key KMS answers
//! for at the domain's OWN Region and answers for in the `Enabled` key state; anything else
//! is refused with `ValidationException` (HTTP 400) and neither creates a domain nor changes
//! the encryption of one. A configuration that enables no encryption, or names no key of its
//! own, uses no caller-named key and so has none to access — and one that enables encryption
//! without naming a key is STORED naming the key it is encrypted under, the Amazon Web
//! Services managed key of this service, whose ARN is read from KMS itself
//! ([`aws_managed_key_arn`]) rather than written here.
//!
//! Both refusals are stated under the one sentence the service states a failed key access
//! in, and the detail after it is the half that differs: a FAILED KMS call relays KMS's own
//! message with the exception trailer an SDK appends (service, status, error code and the id
//! of that call), while a key KMS answered for in another key state is the service's own
//! statement about the key, carrying no trailer. The id of a KMS call is that call's own, so
//! it is asserted as a shape rather than as a value.
//!
//! The rows differ along what the rule generalises over: which of the four spellings of a
//! key the caller writes (key id, key ARN, alias name, alias ARN), whether the key exists
//! at the domain's Region (one created there, one created in another Region, one nobody
//! created, an alias nobody created), which FIELD of a key ARN misses this endpoint's keys
//! (another Region, another account), which key STATE the key is in (enabled, disabled,
//! re-enabled, awaiting deletion), whose Amazon Web Services managed key a predefined alias
//! names (this service's own, another service's), whether the value names a KMS key at all,
//! and whether the configuration enables encryption or names a key in the first place —
//! each row driven through BOTH operations that take the container.

use std::time::Duration;

use serde_json::{json, Value};

use super::domain_configuration_rules::{assert_no_domain, assert_refusal};
use super::domain_lifecycle::{baseline_create, create_baseline, Emulator, REGION};
use super::service_software_update::message;

/// The error every refusal below carries: the operation's modelled 400 for a
/// configuration the service will not provision.
const REFUSAL: &str = "ValidationException";

/// How many times a row looks for a change the clock owes a domain, and how long it waits
/// between looks. Nothing in the emulator sleeps: the span is materialised by whichever
/// later read reads the record, so the loop looks again rather than waiting a span out.
const POLL_LIMIT: u32 = 240;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// A Region other than the one the requests are signed for, where a case puts a key the
/// domain's endpoint therefore cannot reach.
const OTHER_REGION: &str = "us-west-2";

/// An account other than this emulator's, for the row that moves a created key's ARN into
/// one — twelve digits, the form an account id takes.
const OTHER_ACCOUNT: &str = "210987654321";

/// The alias a case points at a created key through KMS's own `CreateAlias`.
const KEY_ALIAS: &str = "alias/opensearch-domains";

/// The PREDEFINED Amazon Web Services alias of this service: `aws/<service-name>` is the
/// form of such an alias and `es` is this service's own name, so it is the alias of the
/// Amazon Web Services managed key a domain encrypts with when the caller names no key of
/// its own. Amazon Web Services holds it in every account and Region, and no caller moves
/// the key state behind it, so naming it is not a configuration this rule can refuse.
const AWS_MANAGED_ALIAS: &str = "alias/aws/es";

/// The predefined Amazon Web Services alias of ANOTHER service, which names that service's
/// managed key rather than one a domain of this service is encrypted with — so it is
/// accessed like any other reference instead of being taken for this service's own.
const FOREIGN_AWS_MANAGED_ALIAS: &str = "alias/aws/s3";

/// A key id nobody created, in the form KMS issues one
/// (`smithy_query(query="doc",shape="EncryptionAtRestOptions",member="KmsKeyId")`: "takes
/// the form 1a2a3a4-1a2a-3a4a-5a6a-1a2a3a4a5a6a").
const UNCREATED_KEY_ID: &str = "8d1d1f2c-3b0a-4a6e-9c4d-5e6f70819a2b";

/// The sentence every refusal below is stated under: accessing the key the configuration
/// named failed, and what follows the colon is the detail.
const ENVELOPE: &str = "Error in Accessing KmsKeyID with details:";

/// How the rule refuses a row, and the message it states that refusal in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// The KMS call FAILED: the detail is KMS's own message, closed by the exception
    /// trailer an SDK appends — the service, the status, the error code, the id of that
    /// KMS call and the proxy. The id is that call's own, so it is asserted as a shape
    /// rather than as a value.
    KmsError { code: &'static str, detail: String },
    /// The key EXISTS and is not in the key state a domain's encryption at rest requires:
    /// this service's own statement about the key it accessed, naming it by its ARN and
    /// naming the state it is not in — no KMS exception, so no trailer.
    NotEnabled(String),
}

impl Refused {
    /// The whole message, for a refusal whose text carries nothing of that one call.
    fn whole(&self) -> Option<String> {
        match self {
            Self::NotEnabled(arn) => {
                Some(format!("{ENVELOPE}KmsKeyID:{arn} is not in Enabled State"))
            }
            Self::KmsError { .. } => None,
        }
    }

    /// The text a relayed KMS exception begins with, up to the id of the call it reports.
    fn prefix(&self) -> Option<String> {
        match self {
            Self::KmsError { code, detail } => Some(format!(
                "{ENVELOPE}{detail} (Service: AWSKMS; Status Code: 400; Error Code: {code}; \
                 Request ID: "
            )),
            Self::NotEnabled(_) => None,
        }
    }

    /// Assert that `message` is how this refusal reads.
    fn stated_in(&self, message: &str, case: &str) {
        if let Some(whole) = self.whole() {
            assert_eq!(
                message, whole,
                "case {case}: expected {whole:?}, got {message:?}"
            );
            return;
        }
        let prefix = self
            .prefix()
            .unwrap_or_else(|| panic!("case {case}: a refusal reads one way or the other"));
        let id = message
            .strip_prefix(prefix.as_str())
            .and_then(|rest| rest.strip_suffix("; Proxy: null)"))
            .unwrap_or_else(|| {
                panic!("case {case}: expected {prefix:?} … \"; Proxy: null)\", got {message:?}")
            });
        // The id names THAT attempt to access the key, so what is asserted is its form.
        let groups: Vec<usize> = id.split('-').map(str::len).collect();
        assert_eq!(
            groups,
            vec![8, 4, 4, 4, 12],
            "case {case}: expected the KMS call's id, got {id:?}"
        );
    }
}

/// The ARN of the alias `alias_name` in this account and Region, and of the key `key_id`
/// — how KMS names a resource it answered for, and so how a relayed KMS message names the
/// thing it could not find.
fn kms_arn(region: &str, resource: &str) -> String {
    format!(
        "arn:aws:kms:{region}:{}:{resource}",
        ::engine::emulator_account_id()
    )
}

/// The detail KMS answers a well-formed key id it holds no key for: it names the key by
/// the key ARN that id would have at this endpoint.
fn no_such_key(key_id: &str) -> String {
    format!(
        "Key '{}' does not exist",
        kms_arn(REGION, &format!("key/{key_id}"))
    )
}

/// One row of the table: the `EncryptionAtRestOptions` container to send, the case it
/// stands for, and how the rule refuses it — `None` where the rule accepts it.
struct Row {
    case: String,
    options: Value,
    refused: Option<Refused>,
}

/// The keys the rows resolve against, each put into its state through KMS's own
/// operations.
struct Keys {
    /// A created, enabled key at the domain's own Region: its ARN and its bare key id.
    enabled_arn: String,
    enabled_id: String,
    /// Its alias ARN, for the alias [`KEY_ALIAS`] created against it.
    alias_arn: String,
    /// A key that was created and then disabled.
    disabled_arn: String,
    /// A key that was disabled and then enabled again.
    reenabled_arn: String,
    /// A key whose deletion has been scheduled.
    pending_deletion_arn: String,
    /// A key created at [`OTHER_REGION`]: its ARN and its bare key id.
    other_region_arn: String,
    other_region_id: String,
}

/// The key id of a key ARN — everything after `key/`.
fn key_id_of(arn: &str) -> String {
    arn.rsplit_once("key/")
        .map(|(_, id)| id.to_string())
        .unwrap_or_else(|| panic!("expected a key ARN, got {arn:?}"))
}

/// Put every key the rows name into its state, through KMS's own operations.
async fn keys(emulator: &Emulator) -> Keys {
    let enabled_arn = emulator.created_key(REGION).await;
    emulator
        .kms_key_operation(
            REGION,
            "CreateAlias",
            json!({ "AliasName": KEY_ALIAS, "TargetKeyId": enabled_arn }),
        )
        .await;

    let disabled_arn = emulator.created_key(REGION).await;
    emulator
        .kms_key_operation(REGION, "DisableKey", json!({ "KeyId": disabled_arn }))
        .await;

    let reenabled_arn = emulator.created_key(REGION).await;
    emulator
        .kms_key_operation(REGION, "DisableKey", json!({ "KeyId": reenabled_arn }))
        .await;
    emulator
        .kms_key_operation(REGION, "EnableKey", json!({ "KeyId": reenabled_arn }))
        .await;

    let pending_deletion_arn = emulator.created_key(REGION).await;
    emulator
        .kms_key_operation(
            REGION,
            "ScheduleKeyDeletion",
            json!({ "KeyId": pending_deletion_arn, "PendingWindowInDays": 7 }),
        )
        .await;

    let other_region_arn = emulator.created_key(OTHER_REGION).await;

    Keys {
        enabled_id: key_id_of(&enabled_arn),
        alias_arn: format!(
            "arn:aws:kms:{REGION}:{}:{KEY_ALIAS}",
            ::engine::emulator_account_id()
        ),
        enabled_arn,
        disabled_arn,
        reenabled_arn,
        pending_deletion_arn,
        other_region_id: key_id_of(&other_region_arn),
        other_region_arn,
    }
}

/// An `EncryptionAtRestOptions` enabling encryption under `key`.
fn enabled(key: &str) -> Value {
    json!({ "Enabled": true, "KmsKeyId": key })
}

/// The table: every row is the whole container a request carries.
fn rows(keys: &Keys) -> Vec<Row> {
    let mut rows = vec![
        Row {
            case: "a created, enabled key by its key ARN".to_string(),
            options: enabled(&keys.enabled_arn),
            refused: None,
        },
        Row {
            case: "the same key by its bare key id".to_string(),
            options: enabled(&keys.enabled_id),
            refused: None,
        },
        Row {
            case: "the same key by an alias pointing at it".to_string(),
            options: enabled(KEY_ALIAS),
            refused: None,
        },
        Row {
            case: "the same key by that alias's ARN".to_string(),
            options: enabled(&keys.alias_arn),
            refused: None,
        },
        Row {
            case: "the predefined Amazon Web Services alias of this service".to_string(),
            options: enabled(AWS_MANAGED_ALIAS),
            refused: None,
        },
        Row {
            case: "a key disabled and then enabled again".to_string(),
            options: enabled(&keys.reenabled_arn),
            refused: None,
        },
        Row {
            // No key is named, so the domain encrypts under the Amazon Web Services
            // managed key: there is no caller-named reference to resolve.
            case: "encryption enabled naming no key at all".to_string(),
            options: json!({ "Enabled": true }),
            refused: None,
        },
        Row {
            // Encryption is switched off, so the key is used for nothing and the value
            // is stored as written.
            case: "encryption disabled against a disabled key".to_string(),
            options: json!({ "Enabled": false, "KmsKeyId": keys.disabled_arn }),
            refused: None,
        },
        Row {
            case: "a disabled key".to_string(),
            options: enabled(&keys.disabled_arn),
            refused: Some(Refused::NotEnabled(keys.disabled_arn.clone())),
        },
        Row {
            case: "a key whose deletion is scheduled".to_string(),
            options: enabled(&keys.pending_deletion_arn),
            refused: Some(Refused::NotEnabled(keys.pending_deletion_arn.clone())),
        },
        Row {
            // A key ARN pins the reference to one Region's endpoint, and KMS answers for
            // no key at another's — naming the ARN's own Region as the invalid part.
            case: "a key of another Region, by its key ARN".to_string(),
            options: enabled(&keys.other_region_arn),
            refused: Some(Refused::KmsError {
                code: "NotFoundException",
                detail: format!("Invalid arn {OTHER_REGION}"),
            }),
        },
        Row {
            // The same key named by its bare key id, which the domain's own Region holds
            // no key for: KMS names the key by the ARN that id would have here.
            case: "a key of another Region, by its bare key id".to_string(),
            options: enabled(&keys.other_region_id),
            refused: Some(Refused::KmsError {
                code: "NotFoundException",
                detail: no_such_key(&keys.other_region_id),
            }),
        },
        Row {
            case: "a created key's ARN moved into another account".to_string(),
            options: enabled(
                &keys
                    .enabled_arn
                    .replace(::engine::emulator_account_id(), OTHER_ACCOUNT),
            ),
            refused: Some(Refused::KmsError {
                code: "NotFoundException",
                detail: format!(
                    "Key '{}' does not exist",
                    keys.enabled_arn
                        .replace(::engine::emulator_account_id(), OTHER_ACCOUNT)
                ),
            }),
        },
        Row {
            case: "a key id nobody created".to_string(),
            options: enabled(UNCREATED_KEY_ID),
            refused: Some(Refused::KmsError {
                code: "NotFoundException",
                detail: no_such_key(UNCREATED_KEY_ID),
            }),
        },
        Row {
            case: "an alias nobody created".to_string(),
            options: enabled("alias/never-created"),
            refused: Some(Refused::KmsError {
                code: "NotFoundException",
                detail: format!(
                    "Alias {} is not found.",
                    kms_arn(REGION, "alias/never-created")
                ),
            }),
        },
        Row {
            // Another service's own Amazon Web Services managed key is not a key a domain
            // of this service is encrypted with, so the alias naming it is accessed like
            // any other reference rather than taken for this service's own.
            case: "the predefined Amazon Web Services alias of another service".to_string(),
            options: enabled(FOREIGN_AWS_MANAGED_ALIAS),
            refused: Some(Refused::KmsError {
                code: "NotFoundException",
                detail: format!(
                    "Alias {} is not found.",
                    kms_arn(REGION, FOREIGN_AWS_MANAGED_ALIAS)
                ),
            }),
        },
        Row {
            // A value that is no key id KMS could have issued names no key at all, and
            // KMS reports it as the invalid key id it is.
            case: "a value that is no key identifier at all".to_string(),
            options: enabled("not-a-kms-key"),
            refused: Some(Refused::KmsError {
                code: "NotFoundException",
                detail: "Invalid keyId 'not-a-kms-key'".to_string(),
            }),
        },
        Row {
            // An ARN that is not a KMS key or alias ARN is a malformed identifier rather
            // than a missing key, which KMS answers on its own error channel.
            case: "an ARN of another service".to_string(),
            options: enabled("arn:aws:s3:::a-bucket"),
            refused: Some(Refused::KmsError {
                code: "InvalidArnException",
                detail: "The specified ARN is not valid: arn:aws:s3:::a-bucket.".to_string(),
            }),
        },
    ];
    // The same created key written with its Region segment moved: the reference is a key
    // ARN of an endpoint that holds no such key, whichever Region the domain itself is in.
    let moved = keys.enabled_arn.replace(REGION, OTHER_REGION);
    rows.push(Row {
        case: "a created key's ARN moved into another Region".to_string(),
        options: enabled(&moved),
        refused: Some(Refused::KmsError {
            code: "NotFoundException",
            detail: format!("Invalid arn {OTHER_REGION}"),
        }),
    });
    rows
}

/// What `DescribeDomain` reports `name` encrypting at rest with — absent when the domain
/// holds no container at all.
async fn encryption(emulator: &Emulator, name: &str) -> Option<Value> {
    let answer = emulator.describe(name).await;
    assert_eq!(
        answer.status, 200,
        "case DescribeDomain({name:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["DomainStatus"]
        .get("EncryptionAtRestOptions")
        .cloned()
}

/// The ARN of the Amazon Web Services managed key of THIS service at [`REGION`], as KMS
/// itself answers for it.
///
/// A predefined Amazon Web Services alias carries no key until one is asked for, and
/// `DescribeKey` on it is such a call, so this is the key a domain that enables encryption
/// without naming one is encrypted under — read from the service that HOLDS it rather than
/// written here.
async fn aws_managed_key_arn(emulator: &Emulator) -> String {
    let answer = emulator
        .kms_call(REGION, "DescribeKey", json!({ "KeyId": AWS_MANAGED_ALIAS }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case DescribeKey({AWS_MANAGED_ALIAS:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case DescribeKey({AWS_MANAGED_ALIAS:?}): expected a key ARN, got {:?}",
                answer.body
            )
        })
        .to_string()
}

/// The container a read of the domain reports for the container a request CARRIED: the same
/// one, except that a configuration which ENABLES encryption without naming a key is stored
/// naming the key its data is encrypted under — this service's managed key, `managed_key`.
fn reported_for(options: &Value, managed_key: &str) -> Value {
    let mut expected = options.clone();
    if options["Enabled"] == json!(true) && options.get("KmsKeyId").is_none() {
        expected["KmsKeyId"] = json!(managed_key);
    }
    expected
}

/// Look at `name` through `DescribeDomain` until it reports no work in flight — the
/// condition a further change waits for, since the service applies one configuration
/// change of a domain at a time. The wait is for the CONDITION: nothing here asserts on how
/// long it took, and the budget below ([`POLL_LIMIT`] × [`POLL_INTERVAL`]) is many times the
/// span the clock owes a change, so a loaded host still gets there.
async fn settled(emulator: &Emulator, name: &str) {
    for _ in 0..POLL_LIMIT {
        let answer = emulator.describe(name).await;
        assert_eq!(
            answer.status, 200,
            "case DescribeDomain({name:?}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        if answer.body["DomainStatus"]["Processing"].as_bool() == Some(false) {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {name:?}: expected the domain's change to land within the poll budget");
}

#[tokio::test]
async fn an_enabled_encryption_at_rest_option_must_name_a_usable_key_of_the_domains_region() {
    let emulator = Emulator::with_kms();
    let keys = keys(&emulator).await;
    let managed_key = aws_managed_key_arn(&emulator).await;

    for (index, row) in rows(&keys).into_iter().enumerate() {
        let Row {
            case,
            options,
            refused,
        } = row;

        let created = format!("cmk-create-{index}");
        let mut body = baseline_create(&created);
        body["EncryptionAtRestOptions"] = options.clone();
        let answer = emulator.create(body).await;
        match &refused {
            Some(refused) => {
                assert_refusal(&answer, REFUSAL, &case);
                refused.stated_in(&message(&answer, &case), &format!("CreateDomain({case})"));
                // A refused create leaves no domain behind, so the name it asked for is
                // still free.
                assert_no_domain(&emulator, &created).await;
            }
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case CreateDomain({case}): expected 200, got {} with {:?}",
                    answer.status, answer.body
                );
                let reported = encryption(&emulator, &created).await;
                let expected = reported_for(&options, &managed_key);
                assert_eq!(
                    reported.as_ref(),
                    Some(&expected),
                    "case DescribeDomain({case}): expected the domain to report {expected:?}, \
                     got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_same_key_rule_governs_a_change_to_a_domains_encryption() {
    let emulator = Emulator::with_kms();
    let keys = keys(&emulator).await;
    let managed_key = aws_managed_key_arn(&emulator).await;

    // One settled domain for the rows that CHANGE a domain's encryption: a domain with
    // work in flight refuses a further change, so the create's own change lands first.
    let changed = "cmk-update";
    create_baseline(&emulator, changed).await;
    settled(&emulator, changed).await;
    let mut held = encryption(&emulator, changed).await;

    for row in rows(&keys) {
        let Row {
            case,
            options,
            refused,
        } = row;

        let answer = emulator
            .update_config(changed, json!({ "EncryptionAtRestOptions": options.clone() }))
            .await;
        match &refused {
            Some(refused) => {
                assert_refusal(&answer, REFUSAL, &case);
                refused.stated_in(
                    &message(&answer, &case),
                    &format!("UpdateDomainConfig({case})"),
                );
                // A refused change stores nothing: the domain keeps the encryption it had.
                let reported = encryption(&emulator, changed).await;
                assert_eq!(
                    reported, held,
                    "case DescribeDomain({case}): a refused change must leave the domain \
                     encrypting with {held:?}, got {reported:?}"
                );
            }
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case UpdateDomainConfig({case}): expected 200, got {} with {:?}",
                    answer.status, answer.body
                );
                // An accepted change lands before the next row starts another.
                settled(&emulator, changed).await;
                held = Some(reported_for(&options, &managed_key));
                let reported = encryption(&emulator, changed).await;
                assert_eq!(
                    reported, held,
                    "case DescribeDomain({case}): expected the changed domain to encrypt \
                     with {held:?}, got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_decision_follows_the_key_state_kms_holds_at_the_time_of_the_call() {
    let emulator = Emulator::with_kms();
    let key = emulator.created_key(REGION).await;

    // The identical request, answered from the key's state as KMS holds it when the call
    // arrives: enabled, then disabled, then enabled again. Nothing about the request
    // changes between the rows — only what KMS says about the key.
    for (index, (case, operation, accepted)) in [
        ("the key as created", None, true),
        ("after DisableKey", Some("DisableKey"), false),
        ("after EnableKey", Some("EnableKey"), true),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(operation) = operation {
            emulator
                .kms_key_operation(REGION, operation, json!({ "KeyId": key }))
                .await;
        }
        let name = format!("cmk-state-{index}");
        let mut body = baseline_create(&name);
        body["EncryptionAtRestOptions"] = enabled(&key);
        let answer = emulator.create(body).await;
        if accepted {
            assert_eq!(
                answer.status, 200,
                "case CreateDomain({case}): expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            let reported = encryption(&emulator, &name).await;
            assert_eq!(
                reported,
                Some(enabled(&key)),
                "case DescribeDomain({case}): expected the domain to encrypt with the key, \
                 got {reported:?}"
            );
        } else {
            assert_refusal(&answer, REFUSAL, case);
            Refused::NotEnabled(key.clone())
                .stated_in(&message(&answer, case), &format!("CreateDomain({case})"));
            assert_no_domain(&emulator, &name).await;
        }
    }
}

/// One row of the default-key table: the container a request carries, and the key the stored
/// configuration must then NAME.
struct KeyRow {
    case: &'static str,
    options: Value,
    /// The key the configuration must report, or `None` where it reports none at all.
    reported_key: Option<String>,
}

/// The default-key table: whether encryption is enabled, and whether the request names a key
/// — and, where it does, in which of the spellings KMS takes one. `managed` is this service's
/// Amazon Web Services managed key and `created` a customer managed key of this account, both
/// read out of KMS.
fn key_rows(managed: &str, created: &str, alias_arn: &str) -> Vec<KeyRow> {
    vec![
        KeyRow {
            case: "encryption enabled naming no key at all",
            options: json!({ "Enabled": true }),
            reported_key: Some(managed.to_string()),
        },
        KeyRow {
            case: "encryption enabled naming a created key by its ARN",
            options: enabled(created),
            reported_key: Some(created.to_string()),
        },
        KeyRow {
            case: "encryption enabled naming that key by its bare key id",
            options: enabled(&key_id_of(created)),
            reported_key: Some(key_id_of(created)),
        },
        KeyRow {
            case: "encryption enabled naming that key by an alias",
            options: enabled(KEY_ALIAS),
            reported_key: Some(KEY_ALIAS.to_string()),
        },
        KeyRow {
            case: "encryption enabled naming that alias by its ARN",
            options: enabled(alias_arn),
            reported_key: Some(alias_arn.to_string()),
        },
        KeyRow {
            case: "encryption enabled naming the managed alias itself",
            options: enabled(AWS_MANAGED_ALIAS),
            reported_key: Some(AWS_MANAGED_ALIAS.to_string()),
        },
        KeyRow {
            case: "encryption disabled",
            options: json!({ "Enabled": false }),
            reported_key: None,
        },
    ]
}

/// The container `DescribeDomainConfig` reports for `name`, inside its `OptionStatus`.
async fn configured_encryption(emulator: &Emulator, name: &str) -> Value {
    let answer = emulator.describe_config(name).await;
    assert_eq!(
        answer.status, 200,
        "case DescribeDomainConfig({name:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["DomainConfig"]["EncryptionAtRestOptions"]["Options"].clone()
}

/// Assert both reads of `name` report `expected` as the key the domain is encrypted under,
/// and that the encryption they report is the one the request asked for.
async fn assert_reports_key(
    emulator: &Emulator,
    name: &str,
    sent: &Value,
    expected: Option<&String>,
    case: &str,
) {
    let described = encryption(emulator, name)
        .await
        .unwrap_or_else(|| panic!("case {case}: expected a reported container"));
    for (source, reported) in [
        ("DescribeDomain", described),
        ("DescribeDomainConfig", configured_encryption(emulator, name).await),
    ] {
        assert_eq!(
            reported["Enabled"], sent["Enabled"],
            "case {case}: {source} must report the encryption the request asked for, got \
             {reported:?}"
        );
        assert_eq!(
            reported.get("KmsKeyId").and_then(Value::as_str),
            expected.map(String::as_str),
            "case {case}: {source} must report {expected:?} as the key the domain is \
             encrypted under, got {reported:?}"
        );
        // A domain encrypted under the service's own key reports a RESOLVED key: a key ARN,
        // not the alias the service asked KMS for it by.
        if sent.get("KmsKeyId").is_none() && sent["Enabled"] == json!(true) {
            let named = reported["KmsKeyId"].as_str().unwrap_or_default();
            assert!(
                named.starts_with("arn:aws:kms:"),
                "case {case}: {source} must report the resolved key ARN, got {named:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_created_domain_reports_the_key_its_data_is_encrypted_under() {
    let emulator = Emulator::with_kms();
    let managed = aws_managed_key_arn(&emulator).await;
    let created = emulator.created_key(REGION).await;
    emulator
        .kms_key_operation(
            REGION,
            "CreateAlias",
            json!({ "AliasName": KEY_ALIAS, "TargetKeyId": created.clone() }),
        )
        .await;
    let alias_arn = kms_arn(REGION, KEY_ALIAS);

    for (index, row) in key_rows(&managed, &created, &alias_arn)
        .into_iter()
        .enumerate()
    {
        let KeyRow {
            case,
            options,
            reported_key,
        } = row;
        let name = format!("ear-key-create-{index}");
        let mut body = baseline_create(&name);
        body["EncryptionAtRestOptions"] = options.clone();
        let answer = emulator.create(body).await;
        assert_eq!(
            answer.status, 200,
            "case CreateDomain({case}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        // The create's own answer carries the stored configuration too, so the key is
        // reported from the first call rather than only on a later read.
        assert_eq!(
            answer.body["DomainStatus"]["EncryptionAtRestOptions"]
                .get("KmsKeyId")
                .and_then(Value::as_str),
            reported_key.as_deref(),
            "case CreateDomain({case}): the create must answer with the key the domain is \
             encrypted under, got {:?}",
            answer.body["DomainStatus"]["EncryptionAtRestOptions"]
        );
        assert_reports_key(
            &emulator,
            &name,
            &options,
            reported_key.as_ref(),
            &format!("CreateDomain({case})"),
        )
        .await;
    }
}

#[tokio::test]
async fn a_changed_domain_reports_the_key_its_data_is_encrypted_under() {
    let emulator = Emulator::with_kms();
    let managed = aws_managed_key_arn(&emulator).await;
    let created = emulator.created_key(REGION).await;
    emulator
        .kms_key_operation(
            REGION,
            "CreateAlias",
            json!({ "AliasName": KEY_ALIAS, "TargetKeyId": created.clone() }),
        )
        .await;
    let alias_arn = kms_arn(REGION, KEY_ALIAS);

    // One domain the rows change in turn: the service applies one configuration change of a
    // domain at a time, so each row waits for the CONDITION that nothing is in flight.
    let name = "ear-key-change";
    let mut body = baseline_create(name);
    body["EncryptionAtRestOptions"] = json!({ "Enabled": false });
    let created_answer = emulator.create(body).await;
    assert_eq!(
        created_answer.status, 200,
        "setup: CreateDomain({name:?}) must be served, got {} with {:?}",
        created_answer.status, created_answer.body
    );

    for row in key_rows(&managed, &created, &alias_arn) {
        let KeyRow {
            case,
            options,
            reported_key,
        } = row;
        settled(&emulator, name).await;
        let changed = emulator
            .update_config(name, json!({ "EncryptionAtRestOptions": options.clone() }))
            .await;
        assert_eq!(
            changed.status, 200,
            "case UpdateDomainConfig({case}): expected 200, got {} with {:?}",
            changed.status, changed.body
        );
        assert_reports_key(
            &emulator,
            name,
            &options,
            reported_key.as_ref(),
            &format!("UpdateDomainConfig({case})"),
        )
        .await;
    }
}
