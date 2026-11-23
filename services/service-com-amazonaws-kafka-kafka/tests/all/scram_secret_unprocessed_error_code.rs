// SPDX-License-Identifier: CC0-1.0

//! HOW a SCRAM-secret batch CLASSIFIES a secret it could not act on: the
//! `UnprocessedScramSecret$ErrorCode` beside the `SecretArn`, driven end to end over the
//! same router the emulator serves from (the shared `kafka_harness`), with AWS Secrets
//! Manager mounted beside this service so every secret a row names is minted through
//! that service's own `CreateSecret`.
//!
//! RULE under test: `ErrorCode` is the only machine-readable discriminator of a
//! per-secret failure the batch surface reports inside its 200, so it carries Amazon
//! MSK's code for the CLASS the failure belongs to — `InvalidSecretArn` for a secret ARN
//! the service cannot act on, which a secret left on the Amazon Web Services managed
//! Secrets Manager key rather than on a customer managed KMS key belongs to — and WHICH
//! condition of that class the secret met is said in the entry's `ErrorMessage`, not in
//! a code of this emulator's own. The code is therefore the same for every condition,
//! every form the condition takes, every position and multiplicity of the secret in the
//! batch, and both batch operations; the sentence is the same for every form of ONE
//! condition and different between two conditions.
//!
//! The rows differ along what the rule generalises over: HOW the secret names the managed
//! key (no `KmsKeyId` at all, its alias, the alias without the `alias/` prefix, the empty
//! string), WHERE the rejected secret sits in the batch (alone, behind an admissible
//! secret — the shape a provisioning flow submits — ahead of one, between two), how often
//! the batch repeats it, WHICH condition of the class the secret met (on the managed key,
//! or no secret of this endpoint at all), and WHICH of the two batch operations reports
//! the entry.
//!
//! `ErrorCode` is an unconstrained `__string` in the model
//! (`smithy_query(query="shape",shape="com.amazonaws.kafka#UnprocessedScramSecret")`:
//! "Error code for associate/disassociate failure"), so the value is a behavioral
//! requirement rather than a modelled enumeration — which is why it is written out here
//! instead of read from the code under test.

use serde_json::{json, Value};

use crate::kafka_harness::{pct, sleep_past_activation, ClusterRequest, Emulator, Reply, REGION};

/// Amazon MSK's `ErrorCode` for the whole class of secret ARNs a SCRAM batch could not
/// act on — the value a client branches on to tell an actionable rejection (re-key the
/// secret, name another one) from an unknown failure.
const SECRET_ARN_CLASS_CODE: &str = "InvalidSecretArn";

/// A customer managed KMS key, named by its full ARN and by an alias of its own: two of
/// the three forms Secrets Manager's `CreateSecretRequest$KmsKeyId` takes ("The ARN, key
/// ID, or alias of the KMS key").
fn customer_key_arn() -> String {
    format!("arn:aws:kms:{REGION}:000000000000:key/3f0b9a71-6d24-4c58-9e71-0c5a4d8b7e22")
}
const CUSTOMER_KEY_ALIAS: &str = "alias/msk-scram-credentials-pinned";

/// Every form in which a secret ends up on the Amazon Web Services managed Secrets
/// Manager key: the member omitted, its alias, that alias without the `alias/` prefix,
/// and the empty string, which names no key either.
const MANAGED_KEY_FORMS: [(&str, Option<&str>); 4] = [
    ("no KmsKeyId at all", None),
    ("the managed key's alias", Some("alias/aws/secretsmanager")),
    (
        "the managed key without the alias prefix",
        Some("aws/secretsmanager"),
    ),
    ("the empty string", Some("")),
];

/// The resource URI all three SCRAM-secret operations are bound to, with the cluster ARN
/// percent-encoded into the single `@httpLabel` path segment the model declares.
fn secrets_uri(arn: &str) -> String {
    format!("/v1/clusters/{}/scram-secrets", pct(arn))
}

async fn associate(emulator: &Emulator, cluster: &str, secrets: &[String]) -> Reply {
    emulator
        .call(
            "POST",
            &secrets_uri(cluster),
            Some(json!({ "secretArnList": secrets })),
        )
        .await
}

/// `BatchDisassociateScramSecret`, which the model binds to the same URI with `PATCH`.
async fn disassociate(emulator: &Emulator, cluster: &str, secrets: &[String]) -> Reply {
    emulator
        .call(
            "PATCH",
            &secrets_uri(cluster),
            Some(json!({ "secretArnList": secrets })),
        )
        .await
}

/// The secrets `ListScramSecrets` reports for `cluster`, sorted, so a case asserts the
/// stored SET rather than one order of it.
async fn stored_secrets(emulator: &Emulator, cluster: &str, case: &str) -> Vec<String> {
    let reply = emulator.call("GET", &secrets_uri(cluster), None).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected ListScramSecrets to answer 200, got {} {}",
        reply.status, reply.body
    );
    let mut listed: Vec<String> = reply
        .at("/secretArnList")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_string())
        .collect();
    listed.sort();
    listed
}

/// One `unprocessedScramSecrets` entry, as the three members the model gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    secret_arn: String,
    error_code: String,
    error_message: String,
}

/// The entries a batch answer reports, in the order it reports them.
fn unprocessed(reply: &Reply) -> Vec<Entry> {
    reply
        .at("/unprocessedScramSecrets")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            let member = |name: &str| {
                entry
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            Entry {
                secret_arn: member("secretArn"),
                error_code: member("errorCode"),
                error_message: member("errorMessage"),
            }
        })
        .collect()
}

/// Mint a secret through Secrets Manager's own `CreateSecret` and answer the ARN that
/// service reports for it. `kms_key_id` is the member as the case names it: `None` for a
/// request that omits it, and `token` numbers the request so no two secrets of one case
/// submit the same idempotency token.
async fn create_secret(
    emulator: &Emulator,
    token: usize,
    name: &str,
    kms_key_id: Option<&str>,
) -> String {
    let mut body = json!({
        "Name": name,
        // A raw request generates its own idempotency token, as an SDK would
        // (`CreateSecretRequest$ClientRequestToken`).
        "ClientRequestToken": format!("00000000-0000-4000-8000-{token:012}"),
        "SecretString": "{\"username\":\"scram-user\",\"password\":\"scram-Secret-77\"}",
    });
    if let Some(key) = kms_key_id {
        body["KmsKeyId"] = json!(key);
    }
    let reply = emulator.secrets_manager(REGION, "CreateSecret", body).await;
    assert_eq!(
        reply.status, 200,
        "setup {name:?}: Secrets Manager must create the secret, got {} {}",
        reply.status, reply.body
    );
    reply
        .at("/ARN")
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "setup {name:?}: Secrets Manager reported no ARN: {}",
                reply.body
            )
        })
        .to_string()
}

/// A cluster that authenticates SASL/SCRAM clients, settled past the emulator's
/// `CREATING` -> `ACTIVE` span so its secret set can be acted on.
async fn scram_cluster(emulator: &Emulator, name: &str) -> String {
    let arn = emulator
        .create_ok(&ClusterRequest {
            sasl_scram: Some(true),
            ..ClusterRequest::named(name)
        })
        .await;
    sleep_past_activation();
    arn
}

/// Assert `entry` classifies a rejection of `secret_arn` under the class code, and hand
/// back its sentence so a caller can compare the sentences of two conditions.
fn classified(entry: &Entry, secret_arn: &str, case: &str) -> String {
    assert_eq!(
        entry.secret_arn, secret_arn,
        "case {case:?}: expected the entry to name the offending secret, got {entry:?}"
    );
    assert_eq!(
        entry.error_code, SECRET_ARN_CLASS_CODE,
        "case {case:?}: expected ErrorCode {SECRET_ARN_CLASS_CODE:?}, got {:?}",
        entry.error_code
    );
    assert!(
        !entry.error_message.is_empty(),
        "case {case:?}: expected the entry to carry an ErrorMessage, got {entry:?}"
    );
    // The sentence is the condition's FIXED text: the offending ARN is reported in the
    // entry's own `SecretArn`, so it is not interpolated into the sentence as well.
    assert!(
        !entry.error_message.contains(secret_arn),
        "case {case:?}: expected the sentence to carry no request value, got {:?}",
        entry.error_message
    );
    entry.error_message.clone()
}

/// One batch shape: what the association submits, and which of its secrets must come
/// back unprocessed, in that order.
struct Shape {
    case: &'static str,
    /// Indices into the batch's own secret list, so a shape is written once and reused
    /// for every form of the condition.
    submitted: &'static [usize],
    rejected: &'static [usize],
}

/// Index 0 is the secret on the managed key; 1 and 2 are secrets on a key of their own.
const MANAGED: usize = 0;

/// Every position and multiplicity the rule generalises over.
const SHAPES: [Shape; 5] = [
    Shape {
        case: "the rejected secret alone",
        submitted: &[MANAGED],
        rejected: &[MANAGED],
    },
    Shape {
        case: "the rejected secret behind one on a key of its own",
        submitted: &[1, MANAGED],
        rejected: &[MANAGED],
    },
    Shape {
        case: "the rejected secret ahead of one on a key of its own",
        submitted: &[MANAGED, 1],
        rejected: &[MANAGED],
    },
    Shape {
        case: "the rejected secret between two on keys of their own",
        submitted: &[1, MANAGED, 2],
        rejected: &[MANAGED],
    },
    Shape {
        case: "the rejected secret three times among two on keys of their own",
        submitted: &[MANAGED, 1, MANAGED, 2, MANAGED],
        rejected: &[MANAGED],
    },
];

/// RULE: a secret left on the Amazon Web Services managed Secrets Manager key is
/// rejected per item under Amazon MSK's code for the secret-ARN class, whatever form the
/// request used to leave it there and wherever it sits in the batch — and the sentence
/// beside the code is the one that names the restriction, so it does not vary with the
/// form either. The admissible secrets of the same batch are still associated, and the
/// rejected one is never stored: `ListScramSecrets` reads the set back.
#[tokio::test]
async fn a_secret_on_the_managed_key_is_rejected_under_the_secret_arn_class_code() {
    let emulator = Emulator::with_secrets_manager();
    let cluster = scram_cluster(&emulator, "scram-code-managed-key").await;

    let own_key_first = create_secret(
        &emulator,
        1,
        "AmazonMSK_code_own_key_1",
        Some(&customer_key_arn()),
    )
    .await;
    let own_key_second = create_secret(
        &emulator,
        2,
        "AmazonMSK_code_own_key_2",
        Some(CUSTOMER_KEY_ALIAS),
    )
    .await;

    // The sentence every form of this ONE condition must come back with: read from the
    // first answer and then required of every later one, so the suite pins that the
    // condition is worded once without writing that wording out.
    let mut condition_sentence: Option<String> = None;

    for (form_index, (form, kms_key_id)) in MANAGED_KEY_FORMS.iter().enumerate() {
        let managed = create_secret(
            &emulator,
            10 + form_index,
            &format!("AmazonMSK_code_managed_{form_index}"),
            *kms_key_id,
        )
        .await;
        let secrets = [
            managed.clone(),
            own_key_first.clone(),
            own_key_second.clone(),
        ];

        for shape in &SHAPES {
            let case = format!("{form} / {}", shape.case);
            let submitted: Vec<String> = shape
                .submitted
                .iter()
                .map(|index| secrets[*index].clone())
                .collect();

            let reply = associate(&emulator, &cluster, &submitted).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected BatchAssociateScramSecret to answer 200, got {} {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.at("/clusterArn").as_str(),
                Some(cluster.as_str()),
                "case {case:?}: expected the answer to name the cluster, got {}",
                reply.body
            );

            let reported = unprocessed(&reply);
            let expected: Vec<&String> = shape
                .rejected
                .iter()
                .map(|index| &secrets[*index])
                .collect();
            assert_eq!(
                reported.len(),
                expected.len(),
                "case {case:?}: expected {} unprocessed entrie(s), got {reported:?}",
                expected.len()
            );
            for (entry, secret_arn) in reported.iter().zip(expected) {
                let sentence = classified(entry, secret_arn, &case);
                match &condition_sentence {
                    None => condition_sentence = Some(sentence),
                    Some(first) => assert_eq!(
                        &sentence, first,
                        "case {case:?}: expected one sentence for the managed-key condition, got \
                         {sentence:?} after {first:?}"
                    ),
                }
            }
        }
    }

    // The restriction the sentence has to name: a secret on the account's default key
    // cannot serve, because Amazon MSK reads the credential under a key the caller can
    // grant it on — a customer managed KMS key.
    let sentence = condition_sentence.expect("every form of the condition reported an entry");
    let lowered = sentence.to_lowercase();
    for required in ["default", "customer managed", "kms"] {
        assert!(
            lowered.contains(required),
            "case \"the sentence names the restriction\": expected it to mention {required:?}, \
             got {sentence:?}"
        );
    }

    // Read the set back: every batch above carried at least one secret on a key of its
    // own, and none of the rejected ones was ever stored.
    let stored = stored_secrets(&emulator, &cluster, "the set after every batch").await;
    let mut expected = vec![own_key_first, own_key_second];
    expected.sort();
    assert_eq!(
        stored, expected,
        "case \"the set after every batch\": expected only the secrets on keys of their own, got \
         {stored:?}"
    );
}

/// RULE: the code names the CLASS, so two different conditions of it — a secret on the
/// managed key and an ARN this endpoint's Secrets Manager holds no secret for — come back
/// under the SAME code in one answer, and are told apart by their sentences.
#[tokio::test]
async fn two_conditions_of_the_class_share_the_code_and_differ_in_the_sentence() {
    let emulator = Emulator::with_secrets_manager();
    let cluster = scram_cluster(&emulator, "scram-code-conditions").await;

    let managed = create_secret(&emulator, 1, "AmazonMSK_code_conditions_managed", None).await;
    // A well-formed secret ARN of this Region that Secrets Manager was never asked to
    // create, so it names no secret of this endpoint.
    let absent =
        format!("arn:aws:secretsmanager:{REGION}:000000000000:secret:AmazonMSK_code_absent-aBcDeF");
    let own_key = create_secret(
        &emulator,
        2,
        "AmazonMSK_code_conditions_own_key",
        Some(&customer_key_arn()),
    )
    .await;

    let case = "the managed-key secret and one this endpoint holds none of";
    let reply = associate(
        &emulator,
        &cluster,
        &[managed.clone(), own_key.clone(), absent.clone()],
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected BatchAssociateScramSecret to answer 200, got {} {}",
        reply.status, reply.body
    );

    let reported = unprocessed(&reply);
    assert_eq!(
        reported.len(),
        2,
        "case {case:?}: expected both inadmissible secrets to come back, got {reported:?}"
    );
    let on_managed_key = classified(&reported[0], &managed, case);
    let held_by_nothing = classified(&reported[1], &absent, case);
    assert_ne!(
        on_managed_key, held_by_nothing,
        "case {case:?}: expected the two conditions to be told apart by their sentences, got \
         {on_managed_key:?} twice"
    );

    let stored = stored_secrets(&emulator, &cluster, case).await;
    assert_eq!(
        stored,
        vec![own_key],
        "case {case:?}: expected only the admissible secret to be stored, got {stored:?}"
    );
}

/// RULE: the class code belongs to the SURFACE, not to one operation — the
/// disassociation reports an entry naming no association of the cluster under the same
/// code, so a client branching on it does not have to know which batch call it made.
#[tokio::test]
async fn the_disassociation_reports_its_entries_under_the_same_code() {
    let emulator = Emulator::new();
    let cluster = scram_cluster(&emulator, "scram-code-disassociation").await;

    let held =
        format!("arn:aws:secretsmanager:{REGION}:000000000000:secret:AmazonMSK_code_held-aBcDeF");
    let never_held = format!(
        "arn:aws:secretsmanager:{REGION}:000000000000:secret:AmazonMSK_code_never_held-aBcDeF"
    );
    assert_eq!(
        associate(&emulator, &cluster, std::slice::from_ref(&held))
            .await
            .status,
        200,
        "setup: the association that seeds the stored set must succeed first"
    );

    let case = "an entry the cluster holds no association for, behind one it holds";
    let reply = disassociate(&emulator, &cluster, &[held, never_held.clone()]).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected BatchDisassociateScramSecret to answer 200, got {} {}",
        reply.status, reply.body
    );
    let reported = unprocessed(&reply);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly the entry naming no association to come back, got \
         {reported:?}"
    );
    classified(&reported[0], &never_held, case);

    let stored = stored_secrets(&emulator, &cluster, case).await;
    assert!(
        stored.is_empty(),
        "case {case:?}: expected the held secret to have been removed, got {stored:?}"
    );
}
