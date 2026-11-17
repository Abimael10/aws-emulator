// SPDX-License-Identifier: CC0-1.0

//! How a cluster's `kmsEncryptionKey` resolves into its `encryptionDetails`, end to
//! end over the wire.
//!
//! RULE: `AWS_OWNED_KMS_KEY` is a RESERVED value of `kmsEncryptionKey`, not a key
//! identifier — it is the `EncryptionType` member standing for the key owned by
//! Amazon Web Services — so a request carrying exactly that value puts the cluster
//! on the Amazon-owned key (`encryptionType = AWS_OWNED_KMS_KEY`, no `kmsKeyArn`),
//! while every OTHER value names a customer-managed key and is expanded into a KMS
//! key ARN in the request's region.
//!
//! The table below spans the class of values the member admits: a bare key id, an
//! alias, a full ARN, the reserved value itself, and three values that merely
//! resemble it (a longer one, a differently cased one, and one behind an `alias/`
//! prefix) — which are ordinary customer keys, because the reserved value is
//! reserved only as itself.
//!
//! SECOND RULE, asserted by the last suite here: a value that names a customer key
//! has to name one that EXISTS. The member admits an ARN, an id or an alias and none
//! of those says whether a key is there, so the value is resolved against AWS KMS —
//! the service that owns keys — and a value KMS holds no key for is refused HTTP 400
//! before any `encryptionDetails` is recorded, on `CreateCluster` and on
//! `UpdateCluster` alike. A refused call changes nothing: the cluster keeps the
//! encryption it had and takes the next mutation straight away.
//!
//! Every call goes through the `Dsql` harness in `tests/all/harness.rs`, this
//! crate's one test client.

use serde_json::{json, Value};

use crate::harness::{account_of, wait_for_the_clock, Answer, Dsql};

/// The reserved value that switches a cluster to the key owned by Amazon Web
/// Services, and the two `EncryptionType` values a cluster can report.
///
/// Source: `smithy_query(query="doc", shape="UpdateClusterInput",
/// member="kmsEncryptionKey")` — "To switch to the key owned by Amazon Web
/// Services, specify the reserved value `AWS_OWNED_KMS_KEY`" — and
/// `smithy_query(query="shape", shape="EncryptionType")`, whose values are
/// `AWS_OWNED_KMS_KEY | CUSTOMER_MANAGED_KMS_KEY`.
const RESERVED_AWS_OWNED: &str = "AWS_OWNED_KMS_KEY";
/// The `EncryptionType` a cluster on the Amazon-owned key reports. Spelled the
/// same as [`RESERVED_AWS_OWNED`] and named separately on purpose: one is an input
/// value of `kmsEncryptionKey`, the other an `EncryptionType` member of the output.
const AWS_OWNED: &str = "AWS_OWNED_KMS_KEY";
/// The `EncryptionType` a cluster on a key of its own reports. See [`AWS_OWNED`].
const CUSTOMER_MANAGED: &str = "CUSTOMER_MANAGED_KMS_KEY";

/// The regions the table is driven in, so the KMS ARN a key id expands into is
/// asserted against more than one request region.
const REGIONS: &[&str] = &["us-east-1", "eu-west-1"];

/// One `kmsEncryptionKey` value and the `encryptionDetails` a cluster carrying it
/// must report.
struct KeyCase {
    name: &'static str,
    key: String,
    want_type: &'static str,
    /// The `kmsKeyArn` the cluster must report, or `None` when the cluster is on
    /// the Amazon-owned key.
    want_key_arn: Option<String>,
}

fn key_cases(region: &str) -> Vec<KeyCase> {
    let account = engine::EMULATOR_ACCOUNT_ID;
    vec![
        KeyCase {
            name: "a bare key id",
            key: "1234abcd-12ab-34cd-56ef".to_string(),
            want_type: CUSTOMER_MANAGED,
            want_key_arn: Some(format!(
                "arn:aws:kms:{region}:{account}:key/1234abcd-12ab-34cd-56ef"
            )),
        },
        KeyCase {
            name: "an alias",
            key: "alias/finance".to_string(),
            want_type: CUSTOMER_MANAGED,
            want_key_arn: Some(format!("arn:aws:kms:{region}:{account}:alias/finance")),
        },
        KeyCase {
            name: "a full key ARN in another region, which is kept verbatim",
            key: format!("arn:aws:kms:ap-south-1:{account}:key/9f0abc"),
            want_type: CUSTOMER_MANAGED,
            want_key_arn: Some(format!("arn:aws:kms:ap-south-1:{account}:key/9f0abc")),
        },
        KeyCase {
            name: "the reserved value: back to the key owned by Amazon Web Services",
            key: RESERVED_AWS_OWNED.to_string(),
            want_type: AWS_OWNED,
            want_key_arn: None,
        },
        KeyCase {
            name: "a key id that only starts with the reserved value",
            key: format!("{RESERVED_AWS_OWNED}_2"),
            want_type: CUSTOMER_MANAGED,
            want_key_arn: Some(format!(
                "arn:aws:kms:{region}:{account}:key/{RESERVED_AWS_OWNED}_2"
            )),
        },
        KeyCase {
            name: "the reserved value in another case is an ordinary key id",
            key: RESERVED_AWS_OWNED.to_lowercase(),
            want_type: CUSTOMER_MANAGED,
            want_key_arn: Some(format!(
                "arn:aws:kms:{region}:{account}:key/{}",
                RESERVED_AWS_OWNED.to_lowercase()
            )),
        },
        KeyCase {
            name: "the reserved value behind an alias prefix is an ordinary alias",
            key: format!("alias/{RESERVED_AWS_OWNED}"),
            want_type: CUSTOMER_MANAGED,
            want_key_arn: Some(format!(
                "arn:aws:kms:{region}:{account}:alias/{RESERVED_AWS_OWNED}"
            )),
        },
    ]
}

/// The `encryptionType` and `kmsKeyArn` a response reports, as a pair.
fn encryption(answer: &Answer) -> (String, Option<String>) {
    let details = answer.get("encryptionDetails");
    (
        details
            .get("encryptionType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        details
            .get("kmsKeyArn")
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// RULE applied to `UpdateCluster`, from a cluster already on a customer-managed
/// key: the reserved value moves it back to the Amazon-owned key, every other value
/// names a customer-managed key.
#[tokio::test]
async fn update_resolves_the_reserved_value_to_the_amazon_owned_key() {
    for region in REGIONS {
        for case in key_cases(region) {
            let dsql = Dsql::in_region(region);
            // The starting point is a cluster already encrypted under a
            // customer-managed key, so a case expecting AWS_OWNED is a real change.
            let created = dsql
                .create(json!({
                    "deletionProtectionEnabled": false,
                    "kmsEncryptionKey": "alias/initial"
                }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {:?} in {region}: CreateCluster must answer 200, got {} {:?}",
                case.name, created.status, created.body
            );
            let identifier = created.str("identifier");
            wait_for_the_clock().await;
            assert_eq!(
                encryption(&dsql.get(&identifier).await).0,
                CUSTOMER_MANAGED,
                "case {:?} in {region}: the cluster must start on a customer-managed key",
                case.name
            );

            let updated = dsql
                .update(&identifier, json!({"kmsEncryptionKey": case.key}))
                .await;
            assert_eq!(
                updated.status, 200,
                "case {:?} in {region}: UpdateCluster must answer 200, got {} {:?}",
                case.name, updated.status, updated.body
            );

            wait_for_the_clock().await;
            let after = dsql.get(&identifier).await;
            let (encryption_type, key_arn) = encryption(&after);
            assert_eq!(
                encryption_type, case.want_type,
                "case {:?} in {region}: encryptionType after the update, got {:?}",
                case.name,
                after.get("encryptionDetails")
            );
            assert_eq!(
                key_arn, case.want_key_arn,
                "case {:?} in {region}: kmsKeyArn after the update, got {:?}",
                case.name,
                after.get("encryptionDetails")
            );

            // The answer comes off persisted state, so a second read reports it
            // unchanged.
            let again = dsql.get(&identifier).await;
            assert_eq!(
                encryption(&again),
                (encryption_type, key_arn),
                "case {:?} in {region}: encryptionDetails must be stable across reads, got {:?}",
                case.name,
                again.get("encryptionDetails")
            );
        }
    }
}

/// RULE applied to `CreateCluster`, which resolves `kmsEncryptionKey` the same way:
/// the reserved value never becomes a key ARN.
#[tokio::test]
async fn create_resolves_the_reserved_value_to_the_amazon_owned_key() {
    for region in REGIONS {
        for case in key_cases(region) {
            let dsql = Dsql::in_region(region);
            let created = dsql
                .create(json!({
                    "deletionProtectionEnabled": false,
                    "kmsEncryptionKey": case.key
                }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {:?} in {region}: CreateCluster must answer 200, got {} {:?}",
                case.name, created.status, created.body
            );
            assert_eq!(
                encryption(&created),
                (case.want_type.to_string(), case.want_key_arn.clone()),
                "case {:?} in {region}: encryptionDetails CreateCluster reports, got {:?}",
                case.name,
                created.get("encryptionDetails")
            );

            let identifier = created.str("identifier");
            wait_for_the_clock().await;
            let fetched = dsql.get(&identifier).await;
            assert_eq!(
                encryption(&fetched),
                (case.want_type.to_string(), case.want_key_arn),
                "case {:?} in {region}: GetCluster must report the persisted encryptionDetails, got {:?}",
                case.name,
                fetched.get("encryptionDetails")
            );
        }
    }
}

/// RULE as an invariant: the choice of key round-trips through any sequence of
/// updates — each call's value alone decides the cluster's encryption, and
/// repeating a value changes nothing.
#[tokio::test]
async fn the_key_choice_round_trips_in_both_directions() {
    let account = engine::EMULATOR_ACCOUNT_ID;
    // Each step: the value the update carries, and what the cluster must then
    // report. The sequence crosses between the two encryption types in both
    // directions and repeats each value, so idempotence is asserted too.
    let steps: Vec<(&str, &str, Option<String>)> = vec![
        (
            "alias/one",
            CUSTOMER_MANAGED,
            Some(format!("arn:aws:kms:us-east-1:{account}:alias/one")),
        ),
        (RESERVED_AWS_OWNED, AWS_OWNED, None),
        (RESERVED_AWS_OWNED, AWS_OWNED, None),
        (
            "two",
            CUSTOMER_MANAGED,
            Some(format!("arn:aws:kms:us-east-1:{account}:key/two")),
        ),
        (
            "two",
            CUSTOMER_MANAGED,
            Some(format!("arn:aws:kms:us-east-1:{account}:key/two")),
        ),
        (RESERVED_AWS_OWNED, AWS_OWNED, None),
    ];

    let dsql = Dsql::new();
    let identifier = dsql.create_settled().await;
    assert_eq!(
        encryption(&dsql.get(&identifier).await),
        (AWS_OWNED.to_string(), None),
        "a cluster created without a kmsEncryptionKey starts on the Amazon-owned key"
    );

    for (step, (key, want_type, want_key_arn)) in steps.into_iter().enumerate() {
        let updated = dsql
            .update(&identifier, json!({"kmsEncryptionKey": key}))
            .await;
        assert_eq!(
            updated.status, 200,
            "step {step} ({key:?}): UpdateCluster must answer 200, got {} {:?}",
            updated.status, updated.body
        );
        wait_for_the_clock().await;
        let after = dsql.get(&identifier).await;
        assert_eq!(
            encryption(&after),
            (want_type.to_string(), want_key_arn),
            "step {step} ({key:?}): encryptionDetails after the update, got {:?}",
            after.get("encryptionDetails")
        );

        // An update that does not carry the member leaves the choice alone.
        let untouched = dsql.update(&identifier, json!({})).await;
        assert_eq!(
            untouched.status, 200,
            "step {step} ({key:?}): an empty UpdateCluster must answer 200, got {} {:?}",
            untouched.status, untouched.body
        );
        wait_for_the_clock().await;
        assert_eq!(
            encryption(&dsql.get(&identifier).await),
            encryption(&after),
            "step {step} ({key:?}): an update without kmsEncryptionKey must not change the key"
        );
    }
}

/// The Region the key-resolution table below is driven in.
const KEY_REGION: &str = "us-east-1";

/// The alias name the suite creates for the key KMS holds. `AliasNameType` is
/// `^[a-zA-Z0-9:/_-]+$` and an alias name begins `alias/`
/// (`smithy_query(query="operation", operation="CreateAlias")` of the KMS model).
const HELD_ALIAS: &str = "alias/dsql-held";

/// A Region that is NOT [`KEY_REGION`], for the row whose key ARN names another
/// Region: that Region's endpoint holds no such key.
const ANOTHER_REGION: &str = "eu-central-1";

/// The key AWS KMS holds for the rows that name a held key, in every form the
/// `kmsEncryptionKey` member admits — each one read off what KMS itself answered
/// rather than written as a literal.
struct HeldKey {
    key_id: String,
    key_arn: String,
    alias_arn: String,
}

/// One KMS key and one alias of it, created through that service's own API.
async fn given_a_kms_key(dsql: &Dsql) -> HeldKey {
    let created = dsql
        .rpc("kms", "TrentService", "CreateKey", json!({}))
        .await;
    assert_eq!(
        created.status, 200,
        "the key must be created in KMS first, got {} {:?}",
        created.status, created.body
    );
    let metadata = created.get("KeyMetadata");
    let key_id = metadata
        .get("KeyId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateKey must answer a KeyId, got {metadata:?}"))
        .to_string();
    let key_arn = metadata
        .get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateKey must answer the key's ARN, got {metadata:?}"))
        .to_string();

    let aliased = dsql
        .rpc(
            "kms",
            "TrentService",
            "CreateAlias",
            json!({"AliasName": HELD_ALIAS, "TargetKeyId": key_id}),
        )
        .await;
    assert_eq!(
        aliased.status, 200,
        "the alias must be created in KMS first, got {} {:?}",
        aliased.status, aliased.body
    );
    // An alias ARN names the alias at one endpoint, so it is assembled from the
    // account the key ARN KMS minted carries.
    let alias_arn = format!(
        "arn:aws:kms:{KEY_REGION}:{}:{HELD_ALIAS}",
        account_of(&key_arn)
    );
    HeldKey {
        key_id,
        key_arn,
        alias_arn,
    }
}

/// One `kmsEncryptionKey` value and whether the key it names is one the account
/// holds — so a row says what the call must answer without restating it.
struct ResolutionCase {
    name: &'static str,
    key: String,
    /// Whether the value names a key the write may record: a key KMS answered for,
    /// or the reserved value, which names no customer key at all.
    recordable: bool,
}

/// The table: every form the member admits, for a key KMS HOLDS and for one it holds
/// nothing under, plus the reserved value — which is never resolved because it names
/// no customer key.
fn resolution_cases(held: &HeldKey) -> Vec<ResolutionCase> {
    let account = account_of(&held.key_arn);
    vec![
        ResolutionCase {
            name: "the key id of a key KMS holds",
            key: held.key_id.clone(),
            recordable: true,
        },
        ResolutionCase {
            name: "the ARN of that same key",
            key: held.key_arn.clone(),
            recordable: true,
        },
        ResolutionCase {
            name: "an alias of that same key",
            key: HELD_ALIAS.to_string(),
            recordable: true,
        },
        ResolutionCase {
            name: "the ARN of that same alias",
            key: held.alias_arn.clone(),
            recordable: true,
        },
        ResolutionCase {
            name: "the reserved value, which names no customer key",
            key: RESERVED_AWS_OWNED.to_string(),
            recordable: true,
        },
        ResolutionCase {
            name: "a key id KMS holds nothing under",
            key: "12345678-1234-1234-1234-123456789012".to_string(),
            recordable: false,
        },
        ResolutionCase {
            name: "an alias nothing ever created",
            key: "alias/dsql-never-created".to_string(),
            recordable: false,
        },
        ResolutionCase {
            name: "a well-formed key ARN of this Region naming no key",
            key: format!("arn:aws:kms:{KEY_REGION}:{account}:key/absent-key"),
            recordable: false,
        },
        ResolutionCase {
            name: "the held key's id written as an ARN of another Region",
            key: format!(
                "arn:aws:kms:{ANOTHER_REGION}:{account}:key/{}",
                held.key_id
            ),
            recordable: false,
        },
    ]
}

/// The sentence a value naming no key is refused with: fixed text around the one
/// variable part, the identifier exactly as the request supplied it.
fn key_not_found(identifier: &str) -> String {
    format!(
        "AWS KMS key {identifier} not found. Verify the key ID or ARN and ensure the key \
         exists in your AWS account and Region."
    )
}

/// Assert one refusal of a key that does not exist.
fn assert_key_not_found(case: &str, answer: &Answer, key: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: a key KMS holds nothing under is refused 400, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error.as_deref(),
        Some("ValidationException"),
        "case {case:?}: the wire code must be ValidationException, got {:?}",
        answer.error
    );
    assert_eq!(
        answer.message(),
        key_not_found(key),
        "case {case:?}: expected {:?}, got {:?}",
        key_not_found(key),
        answer.message()
    );
}

/// RULE: a `kmsEncryptionKey` that names a customer key is resolved against KMS
/// before anything is recorded — a value KMS answered for is accepted, and one it
/// holds no key under is refused HTTP 400 with the sentence that class carries,
/// leaving the cluster's encryption exactly as it was and leaving the cluster ready
/// to take the next mutation.
#[tokio::test]
async fn a_kms_encryption_key_is_resolved_before_it_is_recorded() {
    let dsql = Dsql::with_key_service(KEY_REGION);
    let held = given_a_kms_key(&dsql).await;
    let cases = resolution_cases(&held);

    // One cluster per row, all created before the single wait, each on the
    // Amazon-owned key — so a refused update is visible as "still Amazon-owned".
    let clusters = dsql.create_settled_many(cases.len()).await;
    let before = dsql.list("?max-results=100").await;
    let clusters_before = before
        .get("clusters")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();

    for (case, cluster) in cases.iter().zip(&clusters) {
        // CreateCluster: the key is resolved before the cluster is minted.
        let created = dsql
            .create(json!({
                "deletionProtectionEnabled": false,
                "kmsEncryptionKey": case.key,
            }))
            .await;
        if case.recordable {
            assert_eq!(
                created.status, 200,
                "case {:?}: CreateCluster must accept a key KMS answered for, got {} {:?}",
                case.name, created.status, created.body
            );
        } else {
            let name = format!("CreateCluster with {}", case.name);
            assert_key_not_found(&name, &created, &case.key);
            assert!(
                created.str("identifier").is_empty(),
                "case {name:?}: a refused create must hand back no cluster, got {:?}",
                created.body
            );
        }

        // UpdateCluster: the same resolution, ahead of the stored encryptionDetails.
        let updated = dsql
            .update(cluster, json!({"kmsEncryptionKey": case.key}))
            .await;
        if case.recordable {
            assert_eq!(
                updated.status, 200,
                "case {:?}: UpdateCluster must accept a key KMS answered for, got {} {:?}",
                case.name, updated.status, updated.body
            );
            continue;
        }

        let name = format!("UpdateCluster with {}", case.name);
        assert_key_not_found(&name, &updated, &case.key);

        // The cluster is untouched: it reports the encryption it had, so no read
        // claims protection by a key that is not there.
        let read = dsql.get(cluster).await;
        assert_eq!(
            encryption(&read),
            (AWS_OWNED.to_string(), None),
            "case {name:?}: a refused key must leave the cluster on the Amazon-owned key, got {:?}",
            read.get("encryptionDetails")
        );

        // And nothing is in flight: the next configuration change is served rather
        // than conflicting with an update the service never applied.
        let next = dsql
            .update(cluster, json!({"deletionProtectionEnabled": true}))
            .await;
        assert_eq!(
            next.status, 200,
            "case {name:?}: the next UpdateCluster must be served, got {} {:?}",
            next.status, next.body
        );
    }

    // Every refused create minted nothing, so the store grew only by the accepted
    // rows.
    let accepted = cases.iter().filter(|case| case.recordable).count();
    let after = dsql.list("?max-results=100").await;
    let clusters_after = after
        .get("clusters")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        clusters_after,
        clusters_before + accepted,
        "the store must grow by the {accepted} accepted creates alone, got {:?}",
        after.get("clusters")
    );
}
