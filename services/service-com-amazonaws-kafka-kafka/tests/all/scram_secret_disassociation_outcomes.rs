// SPDX-License-Identifier: CC0-1.0

//! The PER-ITEM outcome of `BatchDisassociateScramSecret`, driven end to end over
//! the same router the emulator serves from (the shared `kafka_harness`).
//!
//! RULE under test: the model gives this operation one success code (200) and an
//! `UnprocessedScramSecrets` list of `UnprocessedScramSecret{SecretArn,
//! ErrorCode, ErrorMessage}` — "List of errors when disassociating secrets to
//! cluster" — so the list is where it reports what it could not do. Each entry of
//! the batch is therefore decided ON ITS OWN against the cluster's associated
//! set: an entry the set holds is taken off, and an entry that names no
//! association of this cluster comes back in `UnprocessedScramSecrets` under its
//! own `SecretArn` with a non-empty `ErrorCode` and an `ErrorMessage`, while the
//! call still answers 200 and still removes the rest of the batch. The outcomes
//! are isolated per item, and the stored set a later `ListScramSecrets` reports
//! is exactly the set minus the entries it HELD.
//!
//! The rows differ along what the rule generalises over: WHY an entry names no
//! association (never associated, already taken off by a call before, a secret of
//! another account or partition, a value that is no secret ARN at all, the empty
//! string), how many entries a batch carries, WHERE in the batch the unheld ones
//! sit, how many of them there are, and whether the batch repeats one.

use serde_json::{json, Value};

use crate::kafka_harness::{
    pct, sleep_past_activation, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// The resource URI all three SCRAM-secret operations are bound to, with the
/// cluster ARN percent-encoded into the single `@httpLabel` path segment.
fn secrets_uri(arn: &str) -> String {
    format!("/v1/clusters/{}/scram-secrets", pct(arn))
}

/// One Secrets Manager secret ARN of this suite's own Region and account, in the
/// form `SecretArnList` carries.
fn secret_arn(name: &str) -> String {
    format!("arn:aws:secretsmanager:{REGION}:{ACCOUNT}:secret:{name}")
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

/// `BatchDisassociateScramSecret`, which the model binds to the same URI with
/// `PATCH`.
async fn disassociate(emulator: &Emulator, cluster: &str, secrets: &[String]) -> Reply {
    emulator
        .call(
            "PATCH",
            &secrets_uri(cluster),
            Some(json!({ "secretArnList": secrets })),
        )
        .await
}

/// The secrets `ListScramSecrets` reports for `cluster`, sorted, so a row asserts
/// the stored SET rather than one order of it.
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

/// The `unprocessedScramSecrets` entries of a reply, as the `SecretArn`s they
/// name in the order the response reports them — asserting on the way that each
/// carries a non-empty `ErrorCode` and `ErrorMessage`, which is what makes the
/// entry a failure a client can act on.
fn unprocessed(reply: &Reply, case: &str) -> Vec<String> {
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
            let secret_arn = member("secretArn");
            assert!(
                !member("errorCode").is_empty(),
                "case {case:?}: expected the entry for {secret_arn:?} to carry an errorCode, got \
                 {entry}"
            );
            assert!(
                !member("errorMessage").is_empty(),
                "case {case:?}: expected the entry for {secret_arn:?} to carry an errorMessage, \
                 got {entry}"
            );
            secret_arn
        })
        .collect()
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

/// The three secrets every row's cluster starts with associated.
fn associated_secrets() -> Vec<String> {
    vec![
        secret_arn("AmazonMSK_held_alpha-1a2b3c"),
        secret_arn("AmazonMSK_held_bravo-4d5e6f"),
        secret_arn("AmazonMSK_held_charlie-7g8h9i"),
    ]
}

/// One row: the batch a disassociation submits, the entries it must report
/// unprocessed (in that order) and the secrets the cluster must still hold
/// afterwards.
struct Row {
    case: &'static str,
    submitted: Vec<String>,
    unprocessed: Vec<String>,
    still_held: Vec<String>,
}

/// Every form of batch the rule generalises over.
fn rows() -> Vec<Row> {
    let held = associated_secrets();
    // A well-formed secret ARN of this Region that this cluster never held.
    let never = secret_arn("AmazonMSK_never_held-0z9y8x");
    // The same, in another account and in another partition: still a secret ARN,
    // still no association of this cluster.
    let other_account =
        "arn:aws:secretsmanager:us-west-2:111122223333:secret:AmazonMSK_other_account-2b3c4d"
            .to_string();
    let other_partition =
        "arn:aws-us-gov:secretsmanager:us-gov-west-1:444455556666:secret:AmazonMSK_gov-5e6f7g"
            .to_string();
    // Values that are no secret ARN at all: they name no association of this
    // cluster for the same reason the ones above do.
    let not_an_arn = "AmazonMSK_not-an-arn".to_string();
    let empty = String::new();

    vec![
        Row {
            case: "one held secret, nothing unprocessed",
            submitted: vec![held[0].clone()],
            unprocessed: Vec::new(),
            still_held: vec![held[1].clone(), held[2].clone()],
        },
        Row {
            case: "one never-associated secret alone",
            submitted: vec![never.clone()],
            unprocessed: vec![never.clone()],
            still_held: held.clone(),
        },
        Row {
            case: "the unheld entry behind a held one",
            submitted: vec![held[0].clone(), never.clone()],
            unprocessed: vec![never.clone()],
            still_held: vec![held[1].clone(), held[2].clone()],
        },
        Row {
            case: "the unheld entry ahead of a held one",
            submitted: vec![never.clone(), held[2].clone()],
            unprocessed: vec![never.clone()],
            still_held: vec![held[0].clone(), held[1].clone()],
        },
        Row {
            case: "three unheld entries among two held ones",
            submitted: vec![
                other_account.clone(),
                held[1].clone(),
                never.clone(),
                held[2].clone(),
                other_partition.clone(),
            ],
            unprocessed: vec![other_account, never.clone(), other_partition],
            still_held: vec![held[0].clone()],
        },
        Row {
            case: "a value that is no secret ARN at all, and the empty string",
            submitted: vec![not_an_arn.clone(), empty.clone(), held[0].clone()],
            unprocessed: vec![not_an_arn, empty],
            still_held: vec![held[1].clone(), held[2].clone()],
        },
        Row {
            // One secret is ONE outcome however many times the batch names it.
            case: "the same unheld entry three times, and a held one twice",
            submitted: vec![
                never.clone(),
                held[0].clone(),
                never.clone(),
                never.clone(),
                held[0].clone(),
            ],
            unprocessed: vec![never.clone()],
            still_held: vec![held[1].clone(), held[2].clone()],
        },
        Row {
            case: "every held secret at once",
            submitted: held.clone(),
            unprocessed: Vec::new(),
            still_held: Vec::new(),
        },
        Row {
            case: "every held secret and two that name no association",
            submitted: vec![
                held[0].clone(),
                never.clone(),
                held[1].clone(),
                held[2].clone(),
                not_an_arn_value(),
            ],
            unprocessed: vec![never, not_an_arn_value()],
            still_held: Vec::new(),
        },
    ]
}

/// A second value that is no secret ARN, used by the last row: a cluster ARN,
/// which is an ARN of this very service and still no association of the cluster.
fn not_an_arn_value() -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/other/00000000-0000-0000-0000-000000000000-1")
}

/// RULE: every entry of the batch is decided on its own — the ones the cluster
/// holds are removed, the ones that name no association come back unprocessed
/// with a code and a message, and the call answers 200 naming the cluster.
#[tokio::test]
async fn each_entry_of_a_disassociation_is_decided_on_its_own() {
    let emulator = Emulator::new();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let cluster = scram_cluster(&emulator, &format!("scram-disassoc-outcome-{index}")).await;
        assert_eq!(
            associate(&emulator, &cluster, &associated_secrets())
                .await
                .status,
            200,
            "case {case:?}: the association that seeds the stored set must succeed first"
        );

        let reply = disassociate(&emulator, &cluster, &row.submitted).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected BatchDisassociateScramSecret to answer 200, got {} {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.at("/clusterArn").as_str(),
            Some(cluster.as_str()),
            "case {case:?}: expected the answer to name the cluster, got {}",
            reply.body
        );
        let reported = unprocessed(&reply, case);
        assert_eq!(
            reported, row.unprocessed,
            "case {case:?}: expected exactly the entries naming no association to come back, got \
             {reported:?}"
        );

        let held = stored_secrets(&emulator, &cluster, case).await;
        let mut expected = row.still_held.clone();
        expected.sort();
        assert_eq!(
            held, expected,
            "case {case:?}: expected the listing to report the set minus the entries the cluster \
             held, got {held:?}"
        );
    }
}

/// RULE (the same rule read as an invariant): a disassociation of a secret the
/// cluster holds reports nothing unprocessed, and the SAME call repeated reports
/// that secret — the outcome follows the stored set at the time of the call, not
/// the request, so re-associating it makes the next removal clean again.
#[tokio::test]
async fn the_outcome_follows_the_stored_set_not_the_request() {
    let emulator = Emulator::new();
    let cluster = scram_cluster(&emulator, "scram-disassoc-repeat").await;
    let secret = secret_arn("AmazonMSK_repeat-3c4d5e");

    for round in 0..2 {
        assert_eq!(
            associate(&emulator, &cluster, std::slice::from_ref(&secret))
                .await
                .status,
            200,
            "round {round}: the association must succeed first"
        );

        let first = disassociate(&emulator, &cluster, std::slice::from_ref(&secret)).await;
        let case = format!("round {round} / the secret is held");
        assert_eq!(
            first.status, 200,
            "case {case:?}: expected 200, got {} {}",
            first.status, first.body
        );
        assert!(
            unprocessed(&first, &case).is_empty(),
            "case {case:?}: expected a held secret to be removed with nothing unprocessed, got {}",
            first.body
        );

        let again = disassociate(&emulator, &cluster, std::slice::from_ref(&secret)).await;
        let case = format!("round {round} / the secret is no longer held");
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected 200, got {} {}",
            again.status, again.body
        );
        assert_eq!(
            unprocessed(&again, &case),
            vec![secret.clone()],
            "case {case:?}: expected the secret the cluster no longer holds to come back \
             unprocessed, got {}",
            again.body
        );

        let held = stored_secrets(&emulator, &cluster, &case).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected the cluster to hold no secrets, got {held:?}"
        );
    }
}

/// RULE: the per-item list is reported by the ACCEPTED call only — a request this
/// endpoint refuses whole (a cluster it holds no record of) is still answered with
/// its modeled exception, not with a 200 carrying every entry as unprocessed.
#[tokio::test]
async fn a_refused_request_is_not_answered_as_a_per_item_failure() {
    let emulator = Emulator::new();
    let absent = format!(
        "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/never-issued/\
         00000000-0000-0000-0000-000000000000-1"
    );
    let reply = disassociate(
        &emulator,
        &absent,
        &[secret_arn("AmazonMSK_against-nothing-6f7g8h")],
    )
    .await;
    assert_eq!(
        (reply.status, reply.error_code()),
        (404, "NotFoundException"),
        "case \"a cluster this endpoint does not hold\": expected the not-found refusal, got {} {}",
        reply.status,
        reply.body
    );
}
