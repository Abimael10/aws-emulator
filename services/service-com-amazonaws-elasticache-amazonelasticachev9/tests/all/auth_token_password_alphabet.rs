// SPDX-License-Identifier: CC0-1.0

//! Which passwords an `AuthToken` may be: the alphabet and the length bounds a cache's
//! password is accepted over, at every writer of one.
//!
//! `AuthToken` is "the password used to access a password protected server", and the API
//! states its constraints the same way at each operation that takes one: a password is
//! "only printable ASCII characters", "at least 16 characters and no more than 128
//! characters in length", and "[c]annot contain any of the following characters: '/', '"',
//! or '@', '%'" (`smithy_query(query="doc", shape="ModifyReplicationGroupMessage",
//! member="AuthToken")`; `CreateCacheClusterMessage$AuthToken` and
//! `ModifyCacheClusterMessage$AuthToken` state the same password constraints).
//!
//! So the alphabet is a DENY list over printable ASCII, not an allow list of a few
//! specials: a password generated the normal way — a random URL-safe secret over
//! `[A-Za-z0-9-_]` — is one the service takes, while a password carrying one of the four
//! characters the rule reserves is not.
//!
//! The rows span what the rule generalises over: the character class a password is drawn
//! from (alphanumeric, the URL-safe extras `-` and `_`, other printable punctuation, the
//! space), each published length bound and one character past it, every reserved character,
//! and the two ways a character can fail to be printable ASCII (a control character and a
//! character outside ASCII). Each accepted row is read back through
//! `DescribeReplicationGroups`, because a password the service claims to have taken has to
//! leave a group that reports it holds one.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::cache_cluster_lifecycle::{create_available_cluster, create_cluster, describe_cluster};
use crate::harness::{assert_refusal, params, poll_until, Emulator, Reply, REGION};

/// The `awsQuery` error code and status a refused password carries, from the model:
/// `InvalidParameterValueException` has `awsQueryError` code `InvalidParameterValue` with
/// HTTP 400 (`smithy_query(query="operation", operation="CreateReplicationGroup")`).
const REFUSAL_CODE: &str = "InvalidParameterValue";
const REFUSAL_STATUS: u16 = 400;

/// A password every writer below accepts, used where a row is about something else: a
/// random URL-safe secret, which is what a client's own token generator produces.
const URL_SAFE_SECRET: &str = "kR9_hB-3xQz0tV7wR2mNpLs4";

/// A password carrying one of the characters the rule reserves, used the same way.
const RESERVED_SECRET: &str = "kR9@hB-3xQz0tV7wR2mNpLs4";

/// Passwords the documented rule accepts, with the case name.
///
/// The length rows are built from the published bounds themselves
/// ([`state::AUTH_TOKEN_MIN_LEN`] / [`state::AUTH_TOKEN_MAX_LEN`]), so a row cannot drift
/// from the rule it tests.
fn accepted_passwords() -> Vec<(&'static str, String)> {
    vec![
        ("a random URL-safe secret", URL_SAFE_SECRET.to_string()),
        (
            "the underscore of the URL-safe alphabet alone",
            "pass_word_pass_word".to_string(),
        ),
        (
            "the printable specials the older wording listed",
            "Aa1!&#$^<>-bcdef".to_string(),
        ),
        (
            "other printable punctuation the rule does not reserve",
            "p+a*s(s)w=o~r,d.1;:[]{}".to_string(),
        ),
        (
            "a space, which is printable ASCII too",
            "correct horse battery".to_string(),
        ),
        (
            "exactly the minimum length",
            "a".repeat(state::AUTH_TOKEN_MIN_LEN),
        ),
        (
            "exactly the maximum length",
            "z".repeat(state::AUTH_TOKEN_MAX_LEN),
        ),
    ]
}

/// Passwords the documented rule refuses, with the case name: each length bound overrun by
/// one character, every reserved character, and the two ways a character is not printable
/// ASCII.
fn refused_passwords() -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "one character short of the minimum".to_string(),
            "a".repeat(state::AUTH_TOKEN_MIN_LEN - 1),
        ),
        (
            "one character past the maximum".to_string(),
            "z".repeat(state::AUTH_TOKEN_MAX_LEN + 1),
        ),
        ("no password at all".to_string(), String::new()),
        (
            "a tab, which is not printable".to_string(),
            "pass\tword_pass_word".to_string(),
        ),
        (
            "a character outside ASCII".to_string(),
            "pass\u{e9}word_pass_word".to_string(),
        ),
    ];
    for reserved in state::AUTH_TOKEN_RESERVED_CHARACTERS {
        rows.push((
            format!("the reserved character {reserved:?}"),
            format!("pass{reserved}word_pass_word"),
        ));
    }
    rows
}

/// One `CreateReplicationGroup` carrying `token` as the password of an encrypted group.
async fn create_encrypted_group(emulator: &Emulator, id: &str, token: &str) -> Reply {
    let mut request = params(&[
        ("ReplicationGroupId", id),
        ("ReplicationGroupDescription", "an auth-token group"),
        ("TransitEncryptionEnabled", "true"),
    ]);
    request.extend(params(&[("AuthToken", token)]));
    emulator.call("CreateReplicationGroup", &request).await
}

/// One `ModifyReplicationGroup` rotating the group's password to `token`.
async fn rotate_group_password(emulator: &Emulator, token: &str) -> Reply {
    emulator
        .call(
            "ModifyReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "pw-writers-rg"),
                ("AuthToken", token),
                ("AuthTokenUpdateStrategy", "ROTATE"),
                ("ApplyImmediately", "true"),
            ]),
        )
        .await
}

/// One `ModifyCacheCluster` rotating the cluster's password to `token`.
async fn rotate_cluster_password(emulator: &Emulator, token: &str) -> Reply {
    emulator
        .call(
            "ModifyCacheCluster",
            &params(&[
                ("CacheClusterId", "pw-writers-cluster"),
                ("AuthToken", token),
                ("AuthTokenUpdateStrategy", "ROTATE"),
                ("ApplyImmediately", "true"),
            ]),
        )
        .await
}

/// Read the group `id` back until the clock has carried it to `available`.
async fn settled_group(emulator: &Emulator, case: &str, id: &str) -> Reply {
    poll_until(
        emulator,
        REGION,
        case,
        "the group reporting available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", id)]),
        |reply| reply.text("ReplicationGroup/Status") == Some(state::STATUS_AVAILABLE),
    )
    .await
}

#[tokio::test]
async fn every_printable_password_inside_the_length_bounds_is_accepted() {
    let emulator = Emulator::new();
    for (index, (case, token)) in accepted_passwords().into_iter().enumerate() {
        let id = format!("pw-ok-{index}");
        let created = create_encrypted_group(&emulator, &id, &token).await;
        assert_eq!(
            created.status, 200,
            "case {case}: a password the rule accepts must be taken, got {} {}",
            created.status, created.body
        );
        // The password itself is a secret: nothing the API answers may carry it back.
        assert!(
            !created.body.contains(&token),
            "case {case}: the password must never be echoed, got {}",
            created.body
        );

        let described = settled_group(&emulator, case, &id).await;
        for (element, expected) in [
            ("ReplicationGroup/Status", state::STATUS_AVAILABLE),
            ("ReplicationGroup/AuthTokenEnabled", "true"),
            ("ReplicationGroup/TransitEncryptionEnabled", "true"),
        ] {
            assert_eq!(
                described.text(element),
                Some(expected),
                "case {case}: {element} must report {expected:?}, got {:?} ({})",
                described.text(element),
                described.body
            );
        }
        assert!(
            !described.body.contains(&token),
            "case {case}: the password must never be read back, got {}",
            described.body
        );
    }
}

#[tokio::test]
async fn a_password_outside_the_rule_is_refused_and_creates_nothing() {
    let emulator = Emulator::new();
    for (index, (case, token)) in refused_passwords().into_iter().enumerate() {
        let id = format!("pw-bad-{index}");
        let refused = create_encrypted_group(&emulator, &id, &token).await;
        assert_refusal(
            &case,
            &refused,
            REFUSAL_STATUS,
            REFUSAL_CODE,
            state::INVALID_AUTH_TOKEN_MESSAGE,
        );
        let described = emulator
            .call(
                "DescribeReplicationGroups",
                &params(&[("ReplicationGroupId", &id)]),
            )
            .await;
        assert_eq!(
            (described.status, described.error_code()),
            (404, "ReplicationGroupNotFoundFault"),
            "case {case}: a refused password must leave no group behind, got {} {}",
            described.status,
            described.body
        );
    }
}

#[tokio::test]
async fn every_writer_of_a_cache_password_judges_it_by_the_same_rule() {
    // The axis: the operation that takes the password. All four document the same password
    // constraints, so all four must take the same secret and refuse the same one.
    let emulator = Emulator::new();

    // `CreateReplicationGroup` / `ModifyReplicationGroup`.
    let created = create_encrypted_group(&emulator, "pw-writers-rg", URL_SAFE_SECRET).await;
    assert_eq!(
        created.status, 200,
        "case CreateReplicationGroup: the secret must be taken, got {} {}",
        created.status, created.body
    );
    settled_group(&emulator, "CreateReplicationGroup", "pw-writers-rg").await;
    let rotated = rotate_group_password(&emulator, URL_SAFE_SECRET).await;
    assert_eq!(
        rotated.status, 200,
        "case ModifyReplicationGroup: the secret must be taken, got {} {}",
        rotated.status, rotated.body
    );
    assert_refusal(
        "ModifyReplicationGroup with a reserved character",
        &rotate_group_password(&emulator, RESERVED_SECRET).await,
        REFUSAL_STATUS,
        REFUSAL_CODE,
        state::INVALID_AUTH_TOKEN_MESSAGE,
    );

    // `CreateCacheCluster` / `ModifyCacheCluster`: the same password, on a cluster of its
    // own rather than a replication group's member.
    assert_refusal(
        "CreateCacheCluster with a reserved character",
        &create_cluster(
            &emulator,
            "pw-writers-cluster-bad",
            &[
                ("TransitEncryptionEnabled", "true"),
                ("AuthToken", RESERVED_SECRET),
            ],
        )
        .await,
        REFUSAL_STATUS,
        REFUSAL_CODE,
        state::INVALID_AUTH_TOKEN_MESSAGE,
    );
    let absent = describe_cluster(&emulator, "pw-writers-cluster-bad").await;
    assert_eq!(
        (absent.status, absent.error_code()),
        (404, "CacheClusterNotFound"),
        "case CreateCacheCluster with a reserved character: a refused password must leave no \
         cluster behind, got {} {}",
        absent.status,
        absent.body
    );
    create_available_cluster(
        &emulator,
        "pw-writers-cluster",
        &[
            ("TransitEncryptionEnabled", "true"),
            ("AuthToken", URL_SAFE_SECRET),
        ],
    )
    .await;
    let described = describe_cluster(&emulator, "pw-writers-cluster").await;
    assert_eq!(
        described.text("CacheCluster/AuthTokenEnabled"),
        Some("true"),
        "case CreateCacheCluster: the cluster must report it holds a password, got {:?} ({})",
        described.text("CacheCluster/AuthTokenEnabled"),
        described.body
    );
    let modified = rotate_cluster_password(&emulator, URL_SAFE_SECRET).await;
    assert_eq!(
        modified.status, 200,
        "case ModifyCacheCluster: the secret must be taken, got {} {}",
        modified.status, modified.body
    );
    assert_refusal(
        "ModifyCacheCluster with a reserved character",
        &rotate_cluster_password(&emulator, RESERVED_SECRET).await,
        REFUSAL_STATUS,
        REFUSAL_CODE,
        state::INVALID_AUTH_TOKEN_MESSAGE,
    );
}
