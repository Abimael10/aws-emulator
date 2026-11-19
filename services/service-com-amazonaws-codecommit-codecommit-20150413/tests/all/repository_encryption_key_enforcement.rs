// SPDX-License-Identifier: CC0-1.0

//! What a repository's own KMS key GATES, with KMS mounted beside this service on
//! one shared engine so the key's state is moved by KMS's own operations — driven
//! as awsJson1_1 requests through the router that serves the real emulator.
//!
//! The rule under test: a repository's contents are encrypted under the KMS key it
//! records ("the ID of the Key Management Service encryption key used to encrypt and
//! decrypt the repository",
//! `smithy_query(query="doc",shape="RepositoryMetadata",member="kmsKeyId")`), so
//! while that key is disabled in KMS every operation that reads or writes those
//! contents — its objects, its references, its history — refuses with HTTP 400
//! `EncryptionKeyDisabledException` ("The encryption key is disabled",
//! `smithy_query(query="shape",shape="EncryptionKeyDisabledException")`), carrying
//! the key the repository holds, and serves no content and lands no commit; the gate
//! is the key's CURRENT state, so enabling it again serves the very same calls.
//!
//! The same key state gates the calls that SET which key a repository depends on:
//! creating the repository under a key (`CreateRepository`) or rotating it onto one
//! (`UpdateRepositoryEncryptionKey`) takes that key into use, so a `kmsKeyId`
//! resolving to a key KMS reports `Disabled` is refused in the same code and the
//! same words and changes nothing — the repository stays on the key it already had,
//! and a refused create mints no repository.
//!
//! The table spans what the rule generalises over rather than one recorded call:
//! every operation of this service that reads or writes a repository's git data —
//! reads and writes, single-object and whole-tree, the merge surface included — and,
//! beside it, the dimensions the gate must NOT key on: which selector form the
//! repository was created with or is pointed at (every form KMS documents for
//! `KeyId`), whether another repository under another key is touched at all, whether
//! the key named is one KMS holds at all, and how many times the key is disabled and
//! enabled again.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::error_with_message;
use crate::repository_lifecycle::{base64, call, from_base64, ok, string_at, REGION};

/// The code every refusal in this suite carries: the shape CodeCommit declares for
/// a call on a repository whose encryption key is disabled.
const DISABLED: &str = "EncryptionKeyDisabledException";

/// The access key id the KMS setup and key-state calls are signed with — a key no
/// store holds, which speaks for the account's own principal, so this suite is
/// about the gate rather than about KMS authorization.
const KMS_CALLER: &str = "AKIDCMKGATE";

/// This service beside KMS on one shared engine, so a repository's key is the very
/// key KMS's own `DisableKey` and `EnableKey` move.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codecommit_codecommit_20150413::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    crate::wire::hold_long_term_key(&engine, crate::wire::DEFAULT_ACCESS_KEY_ID);
    router
}

/// One KMS call, the way an SDK sends it: awsJson1_1 under KMS's own target prefix
/// and signing name, with the status asserted.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={KMS_CALLER}/20260101/{REGION}/kms/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "KMS {operation}: expected 200, got {status} with {payload}"
    );
    payload
}

/// A KMS key of the account, answered as its key id and its ARN.
async fn given_key(router: &MultiServiceRouter, description: &str) -> (String, String) {
    let created = kms(
        router,
        "CreateKey",
        &json!({ "Description": description }),
    )
    .await;
    (
        string_at(&created, "/KeyMetadata/KeyId"),
        string_at(&created, "/KeyMetadata/Arn"),
    )
}

/// Move `key_id` into the key state `DisableKey` or `EnableKey` leaves it in, and
/// assert KMS itself reports it — the state the gate is read from, never asserted
/// by waiting on a clock.
async fn key_state_moved_to(router: &MultiServiceRouter, key_id: &str, enabled: bool) {
    let operation = if enabled { "EnableKey" } else { "DisableKey" };
    kms(router, operation, &json!({ "KeyId": key_id })).await;
    let described = kms(router, "DescribeKey", &json!({ "KeyId": key_id })).await;
    let expected = if enabled { "Enabled" } else { "Disabled" };
    assert_eq!(
        described
            .pointer("/KeyMetadata/KeyState")
            .and_then(Value::as_str),
        Some(expected),
        "case {operation}: KMS must report KeyState {expected:?} for {key_id}, got {described}"
    );
}

/// The message CodeCommit refuses a call on a repository whose encryption key is
/// disabled with: fixed words around the key the REPOSITORY holds, which is the
/// sentence's only variable part.
fn disabled_message(key_arn: &str) -> String {
    format!("KMS key {key_arn} is disabled")
}

/// A repository encrypted under `selector`, seeded with one file on `main` and a
/// `feature` branch that diverged from it — each branch adding its own version of
/// one more path, so the two tips really conflict and the merge surface has
/// something to report. Answered as the first file's `blobId`, the tip of `main`
/// and the tip of `feature`.
struct Seeded {
    blob_id: String,
    main_tip: String,
    feature_tip: String,
}

/// The bytes the seeded file holds: asserted byte-identical again after the key is
/// enabled, so the suite checks the content survived the refusals.
const SECRET: &[u8] = b"payload\n";

/// The path both seeded branches add their own version of, so the merge surface has
/// a conflict to report for it.
const CONFLICTED: &str = "shared.txt";

async fn seed(router: &MultiServiceRouter, repository: &str, selector: Option<&str>) -> Seeded {
    let mut body = json!({ "repositoryName": repository });
    if let Some(selector) = selector {
        body["kmsKeyId"] = json!(selector);
    }
    ok(router, "CreateRepository", &body).await;

    let first = ok(
        router,
        "PutFile",
        &json!({
            "repositoryName": repository,
            "branchName": "main",
            "filePath": "secret.txt",
            "fileContent": base64(SECRET),
        }),
    )
    .await;
    let main_tip = string_at(&first, "/commitId");
    let blob_id = string_at(&first, "/blobId");

    ok(
        router,
        "CreateBranch",
        &json!({
            "repositoryName": repository,
            "branchName": "feature",
            "commitId": main_tip,
        }),
    )
    .await;
    // Each branch adds its OWN version of the same path, which is what makes the
    // two tips conflict.
    let on_feature = ok(
        router,
        "PutFile",
        &json!({
            "repositoryName": repository,
            "branchName": "feature",
            "filePath": CONFLICTED,
            "fileContent": base64(b"from the feature branch\n"),
            "parentCommitId": main_tip,
        }),
    )
    .await;
    let on_main = ok(
        router,
        "PutFile",
        &json!({
            "repositoryName": repository,
            "branchName": "main",
            "filePath": CONFLICTED,
            "fileContent": base64(b"from main\n"),
            "parentCommitId": main_tip,
        }),
    )
    .await;

    Seeded {
        blob_id,
        main_tip: string_at(&on_main, "/commitId"),
        feature_tip: string_at(&on_feature, "/commitId"),
    }
}

/// One row of the table: why it is there, the operation, and the request body.
struct Path {
    why: &'static str,
    operation: &'static str,
    body: Value,
}

/// Every operation of this service that reads a repository's git data, with a body
/// the seeded repository serves. The list is this service's read surface over
/// repository contents as `OPERATIONS.md` lays it out, not the operations the issue
/// named: `GetBlob`, `GetFile` and `GetBranch` are three of twelve rows.
fn read_paths(repository: &str, seeded: &Seeded) -> Vec<Path> {
    vec![
        Path {
            why: "one object by its git object name",
            operation: "GetBlob",
            body: json!({ "repositoryName": repository, "blobId": seeded.blob_id }),
        },
        Path {
            why: "one file at a commit specifier",
            operation: "GetFile",
            body: json!({
                "repositoryName": repository,
                "filePath": "secret.txt",
                "commitSpecifier": "main",
            }),
        },
        Path {
            why: "a whole folder of the tree",
            operation: "GetFolder",
            body: json!({ "repositoryName": repository, "folderPath": "/" }),
        },
        Path {
            why: "one reference",
            operation: "GetBranch",
            body: json!({ "repositoryName": repository, "branchName": "main" }),
        },
        Path {
            why: "every reference",
            operation: "ListBranches",
            body: json!({ "repositoryName": repository }),
        },
        Path {
            why: "one commit of the history",
            operation: "GetCommit",
            body: json!({ "repositoryName": repository, "commitId": seeded.main_tip }),
        },
        Path {
            why: "the difference between two commits",
            operation: "GetDifferences",
            body: json!({
                "repositoryName": repository,
                "afterCommitSpecifier": seeded.feature_tip,
                "beforeCommitSpecifier": seeded.main_tip,
            }),
        },
        Path {
            why: "the history of one path",
            operation: "ListFileCommitHistory",
            body: json!({ "repositoryName": repository, "filePath": "secret.txt" }),
        },
        Path {
            why: "which merges two commits admit",
            operation: "GetMergeOptions",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
            }),
        },
        Path {
            why: "the conflicts of a merge",
            operation: "GetMergeConflicts",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
                "mergeOption": "THREE_WAY_MERGE",
            }),
        },
        Path {
            why: "the conflicts of a merge, described",
            operation: "DescribeMergeConflicts",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
                "mergeOption": "THREE_WAY_MERGE",
                "filePath": CONFLICTED,
            }),
        },
        Path {
            why: "the conflicts of a merge, described in a batch",
            operation: "BatchDescribeMergeConflicts",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
                "mergeOption": "THREE_WAY_MERGE",
            }),
        },
    ]
}

/// Every operation of this service that writes a repository's git data, with a body
/// the seeded repository would serve while the key is usable.
fn write_paths(repository: &str, seeded: &Seeded) -> Vec<Path> {
    vec![
        Path {
            why: "one file written to a branch",
            operation: "PutFile",
            body: json!({
                "repositoryName": repository,
                "branchName": "main",
                "filePath": "added.txt",
                "fileContent": base64(b"added while the key was disabled\n"),
                "parentCommitId": seeded.main_tip,
            }),
        },
        Path {
            why: "one file deleted from a branch",
            operation: "DeleteFile",
            body: json!({
                "repositoryName": repository,
                "branchName": "main",
                "filePath": "secret.txt",
                "parentCommitId": seeded.main_tip,
            }),
        },
        Path {
            why: "a commit of several entries",
            operation: "CreateCommit",
            body: json!({
                "repositoryName": repository,
                "branchName": "main",
                "parentCommitId": seeded.main_tip,
                "commitMessage": "a commit while the key was disabled",
                "putFiles": [{ "filePath": "committed.txt", "fileContent": base64(b"committed\n") }],
            }),
        },
        Path {
            why: "a reference created",
            operation: "CreateBranch",
            body: json!({
                "repositoryName": repository,
                "branchName": "another",
                "commitId": seeded.main_tip,
            }),
        },
        Path {
            why: "a reference deleted",
            operation: "DeleteBranch",
            body: json!({ "repositoryName": repository, "branchName": "feature" }),
        },
        Path {
            why: "the default reference moved",
            operation: "UpdateDefaultBranch",
            body: json!({ "repositoryName": repository, "defaultBranchName": "feature" }),
        },
        Path {
            why: "a fast-forward merge",
            operation: "MergeBranchesByFastForward",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
                "targetBranch": "main",
            }),
        },
        Path {
            why: "a squash merge",
            operation: "MergeBranchesBySquash",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
                "targetBranch": "main",
            }),
        },
        Path {
            why: "a three-way merge",
            operation: "MergeBranchesByThreeWay",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
                "targetBranch": "main",
            }),
        },
        Path {
            why: "a merge commit no reference points at",
            operation: "CreateUnreferencedMergeCommit",
            body: json!({
                "repositoryName": repository,
                "sourceCommitSpecifier": "feature",
                "destinationCommitSpecifier": "main",
                "mergeOption": "THREE_WAY_MERGE",
            }),
        },
        Path {
            why: "a pull request over two references",
            operation: "CreatePullRequest",
            body: json!({
                "title": "a pull request while the key was disabled",
                "targets": [{
                    "repositoryName": repository,
                    "sourceReference": "feature",
                    "destinationReference": "main",
                }],
                "clientRequestToken": crate::wire::fresh_client_request_token(),
            }),
        },
    ]
}

/// Assert `path` is refused for the disabled key `key_arn`, and that the refusal
/// carried no part of the repository's contents.
async fn refuses(router: &MultiServiceRouter, path: &Path, key_arn: &str) {
    let (code, message) =
        error_with_message(router, path.operation, &path.body, 400).await;
    assert_eq!(
        code, DISABLED,
        "case {:?} ({}): expected {DISABLED}, got {code:?} saying {message:?}",
        path.why, path.operation
    );
    assert_eq!(
        message,
        disabled_message(key_arn),
        "case {:?} ({}): expected the disabled-key sentence, got {message:?}",
        path.why,
        path.operation
    );
}

#[tokio::test]
async fn a_disabled_repository_key_refuses_every_read_and_write_of_its_contents() {
    let router = emulator();
    let repository = "cmk-gated";
    let (key_id, key_arn) = given_key(&router, "the key this repository is encrypted under").await;
    let seeded = seed(&router, repository, Some(&key_arn)).await;

    // The positive barrier: with the key enabled, every read serves.
    for path in read_paths(repository, &seeded) {
        let (status, payload) = call(&router, path.operation, &path.body).await;
        assert_eq!(
            status, 200,
            "case {:?} ({}): must serve while the key is enabled, got {status} with {payload}",
            path.why, path.operation
        );
    }

    key_state_moved_to(&router, &key_id, false).await;

    // Every read and every write of the repository's contents is refused, in the
    // same code and the same words, while the key is disabled.
    for path in read_paths(repository, &seeded)
        .into_iter()
        .chain(write_paths(repository, &seeded))
    {
        refuses(&router, &path, &key_arn).await;
    }

    // Nothing the refused writes asked for landed: the references and the tip are
    // exactly what the seed left, read back once the key is usable again.
    key_state_moved_to(&router, &key_id, true).await;
    let branches = ok(
        &router,
        "ListBranches",
        &json!({ "repositoryName": repository }),
    )
    .await;
    assert_eq!(
        branches.get("branches"),
        Some(&json!(["feature", "main"])),
        "the refused writes must have created and deleted no reference, got {branches}"
    );
    let main = ok(
        &router,
        "GetBranch",
        &json!({ "repositoryName": repository, "branchName": "main" }),
    )
    .await;
    assert_eq!(
        string_at(&main, "/branch/commitId"),
        seeded.main_tip,
        "the refused writes must have landed no commit on main, got {main}"
    );

    // The same calls serve again, byte for byte: the gate is the key's current
    // state, and the contents were never touched.
    let blob = ok(
        &router,
        "GetBlob",
        &json!({ "repositoryName": repository, "blobId": seeded.blob_id }),
    )
    .await;
    assert_eq!(
        from_base64(&string_at(&blob, "/content")),
        SECRET,
        "the original bytes must come back once the key is enabled, got {blob}"
    );
    let file = ok(
        &router,
        "GetFile",
        &json!({
            "repositoryName": repository,
            "filePath": "secret.txt",
            "commitSpecifier": "main",
        }),
    )
    .await;
    assert_eq!(
        string_at(&file, "/blobId"),
        seeded.blob_id,
        "GetFile must resolve the original blobId once the key is enabled, got {file}"
    );

    // And a write lands again.
    let written = ok(
        &router,
        "PutFile",
        &json!({
            "repositoryName": repository,
            "branchName": "main",
            "filePath": "after.txt",
            "fileContent": base64(b"after the key came back\n"),
            "parentCommitId": seeded.main_tip,
        }),
    )
    .await;
    assert_ne!(
        string_at(&written, "/commitId"),
        seeded.main_tip,
        "a write must land a new commit once the key is enabled, got {written}"
    );
}

#[tokio::test]
async fn the_gate_follows_the_key_the_repository_holds_whichever_form_named_it() {
    let router = emulator();
    let (key_id, key_arn) = given_key(&router, "one key, named three ways").await;
    kms(
        &router,
        "CreateAlias",
        &json!({ "AliasName": "alias/codecommit-gate", "TargetKeyId": key_id }),
    )
    .await;
    let (other_id, other_arn) = given_key(&router, "another key, left enabled").await;

    // Rows: the selector form the repository was created with. All three name ONE
    // key, which is what the repository records, so all three are gated together —
    // and the fourth repository, under another key, is gated by nothing here.
    let gated = [
        ("a bare key id", "gate-by-id", key_id.clone()),
        ("a key ARN", "gate-by-arn", key_arn.clone()),
        ("an alias name", "gate-by-alias", "alias/codecommit-gate".to_string()),
    ];
    let mut seeded = Vec::new();
    for (why, repository, selector) in &gated {
        let fixture = seed(&router, repository, Some(selector)).await;
        seeded.push((*why, *repository, fixture));
    }
    let untouched = seed(&router, "gate-other-key", Some(&other_arn)).await;

    key_state_moved_to(&router, &key_id, false).await;

    for (why, repository, fixture) in &seeded {
        let path = Path {
            why,
            operation: "GetBlob",
            body: json!({ "repositoryName": repository, "blobId": fixture.blob_id }),
        };
        refuses(&router, &path, &key_arn).await;
    }

    // A repository under ANOTHER key is untouched: the gate is per repository key,
    // not a switch over the service.
    let blob = ok(
        &router,
        "GetBlob",
        &json!({ "repositoryName": "gate-other-key", "blobId": untouched.blob_id }),
    )
    .await;
    assert_eq!(
        from_base64(&string_at(&blob, "/content")),
        SECRET,
        "case {other_id:?}: a repository under another key must keep serving, got {blob}"
    );

    // Disabling and enabling again is not one-shot: the gate is read from the key
    // every call, so the same read is refused, served, and refused again.
    let repository = gated[0].1;
    let first = seeded[0].2.blob_id.clone();
    key_state_moved_to(&router, &key_id, true).await;
    let served = ok(
        &router,
        "GetBlob",
        &json!({ "repositoryName": repository, "blobId": first }),
    )
    .await;
    assert_eq!(
        from_base64(&string_at(&served, "/content")),
        SECRET,
        "case re-enabled: the read must serve again, got {served}"
    );
    key_state_moved_to(&router, &key_id, false).await;
    refuses(
        &router,
        &Path {
            why: "disabled a second time",
            operation: "GetBlob",
            body: json!({ "repositoryName": repository, "blobId": first }),
        },
        &key_arn,
    )
    .await;
}

/// The four forms KMS documents for `KeyId`, each naming ONE key: its bare id, its
/// ARN, an alias of it and that alias's ARN
/// (`smithy_query(query="doc",shape="UpdateRepositoryEncryptionKeyInput",member="kmsKeyId")`
/// sends a caller to that list). Answered as (why the row is there, the selector).
async fn selector_forms(
    router: &MultiServiceRouter,
    alias_name: &str,
    key_id: &str,
    key_arn: &str,
) -> Vec<(&'static str, String)> {
    kms(
        router,
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
                engine::emulator_account_id()
            ),
        ),
    ]
}

/// The key `GetRepository` reports the repository is encrypted under.
async fn reported_key(router: &MultiServiceRouter, repository: &str) -> String {
    let got = ok(router, "GetRepository", &json!({ "repositoryName": repository })).await;
    string_at(&got, "/repositoryMetadata/kmsKeyId")
}

/// Assert `operation` with `body` is refused for the disabled key `key_arn`, in the
/// code and the words this service words that refusal with.
async fn refuses_for_the_key(
    router: &MultiServiceRouter,
    why: &str,
    operation: &str,
    body: &Value,
    key_arn: &str,
) {
    let (code, message) = error_with_message(router, operation, body, 400).await;
    assert_eq!(
        code, DISABLED,
        "case {why:?} ({operation}): expected {DISABLED}, got {code:?} saying {message:?}"
    );
    assert_eq!(
        message,
        disabled_message(key_arn),
        "case {why:?} ({operation}): expected the disabled-key sentence, got {message:?}"
    );
}

/// Rotating a repository onto a key consults THAT key's state: the rotation takes the
/// key into use, so a selector resolving to a key KMS reports `Disabled` is refused
/// and the repository goes on holding the key it had. The rows are the four selector
/// forms, each naming the same disabled key, and the gate is read per call — the same
/// rotations serve once the key is enabled again.
#[tokio::test]
async fn a_rotation_onto_a_disabled_key_is_refused_and_the_repository_keeps_its_key() {
    let router = emulator();
    let (target_id, target_arn) = given_key(&router, "the key a rotation aims at").await;
    let (_, in_effect_arn) = given_key(&router, "the key the repositories hold").await;
    let rows = selector_forms(
        &router,
        "alias/codecommit-rotation-target",
        &target_id,
        &target_arn,
    )
    .await;

    key_state_moved_to(&router, &target_id, false).await;

    // One repository per row, each created under a key that is NOT the disabled one:
    // those creates serve, so the gate follows the key a call names rather than
    // switching the service off.
    let repositories: Vec<String> = (0..rows.len())
        .map(|row| format!("rotate-onto-disabled-{row}"))
        .collect();
    for ((why, _), repository) in rows.iter().zip(&repositories) {
        ok(
            &router,
            "CreateRepository",
            &json!({ "repositoryName": repository, "kmsKeyId": in_effect_arn }),
        )
        .await;
        assert_eq!(
            reported_key(&router, repository).await,
            in_effect_arn,
            "case {why:?}: the repository must be created under the enabled key"
        );
    }

    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let body = json!({ "repositoryName": repository, "kmsKeyId": selector });
        refuses_for_the_key(
            &router,
            why,
            "UpdateRepositoryEncryptionKey",
            &body,
            &target_arn,
        )
        .await;
        assert_eq!(
            reported_key(&router, repository).await,
            in_effect_arn,
            "case {why:?} ({selector}): a refused rotation must leave the repository on the \
             key it had"
        );
    }

    // The gate is the key's CURRENT state: every one of those rotations serves once
    // KMS reports the key enabled, and each reports the key itself.
    key_state_moved_to(&router, &target_id, true).await;
    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let rotated = ok(
            &router,
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
            reported_key(&router, repository).await,
            target_arn,
            "case {why:?} ({selector}): the rotated repository must hold the new key"
        );
    }

    // Disabled a second time, the same call is refused again — a rotation onto the key
    // the repository already holds included, which still takes that key into use.
    key_state_moved_to(&router, &target_id, false).await;
    let body = json!({ "repositoryName": repositories[0], "kmsKeyId": target_arn });
    refuses_for_the_key(
        &router,
        "disabled a second time",
        "UpdateRepositoryEncryptionKey",
        &body,
        &target_arn,
    )
    .await;
    assert_eq!(
        reported_key(&router, &repositories[0]).await,
        target_arn,
        "case {:?}: the refused rotation must leave the repository exactly as it was",
        "disabled a second time"
    );
}

/// Creating a repository under a key consults that key's state for the same reason:
/// the create encrypts the repository under the key, so a selector resolving to a
/// disabled key is refused and the account holds no repository under that name.
#[tokio::test]
async fn a_create_under_a_disabled_key_mints_no_repository() {
    let router = emulator();
    let (key_id, key_arn) = given_key(&router, "the key a create asks for").await;
    let rows = selector_forms(
        &router,
        "alias/codecommit-create-target",
        &key_id,
        &key_arn,
    )
    .await;

    key_state_moved_to(&router, &key_id, false).await;

    let repositories: Vec<String> = (0..rows.len())
        .map(|row| format!("create-under-disabled-{row}"))
        .collect();
    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let body = json!({ "repositoryName": repository, "kmsKeyId": selector });
        refuses_for_the_key(&router, why, "CreateRepository", &body, &key_arn).await;
        let code = crate::repository_lifecycle::error(
            &router,
            "GetRepository",
            &json!({ "repositoryName": repository }),
            400,
        )
        .await;
        assert_eq!(
            code, "RepositoryDoesNotExistException",
            "case {why:?} ({selector}): a refused create must mint no repository"
        );
    }

    // And the same creates serve once the key is usable, each reporting the key the
    // repository is encrypted under.
    key_state_moved_to(&router, &key_id, true).await;
    for ((why, selector), repository) in rows.iter().zip(&repositories) {
        let created = ok(
            &router,
            "CreateRepository",
            &json!({ "repositoryName": repository, "kmsKeyId": selector }),
        )
        .await;
        assert_eq!(
            created
                .pointer("/repositoryMetadata/kmsKeyId")
                .and_then(Value::as_str),
            Some(key_arn.as_str()),
            "case {why:?} ({selector}): the created repository must report its key, got \
             {created}"
        );
        assert_eq!(
            reported_key(&router, repository).await,
            key_arn,
            "case {why:?} ({selector}): the stored repository must hold that key"
        );
    }
}

/// A selector KMS holds no key for disables nothing: there is no key state to read,
/// so a call AWS serves is not refused for want of one.
#[tokio::test]
async fn a_key_kms_holds_no_record_of_refuses_nothing() {
    let router = emulator();
    let unheld_arn = crate::harness::kms_key_arn_of("1234abcd-12ab-34cd-56ef-1234567890ab");
    let unheld_id = "fedcba98-7654-3210-fedc-ba9876543210";

    let created = ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "unheld-key", "kmsKeyId": unheld_arn }),
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

    let rotated = ok(
        &router,
        "UpdateRepositoryEncryptionKey",
        &json!({ "repositoryName": "unheld-key", "kmsKeyId": unheld_id }),
    )
    .await;
    assert_eq!(
        rotated.get("kmsKeyId").and_then(Value::as_str),
        Some(crate::harness::kms_key_arn_of(unheld_id).as_str()),
        "case {:?}: the rotation must serve and report that key, got {rotated}",
        "a key id KMS holds no key for"
    );
}

#[tokio::test]
async fn a_repository_under_the_managed_key_is_served() {
    let router = emulator();

    // A repository that names no key of its own is encrypted under this service's
    // Amazon Web Services managed key, which nothing disables — so no call of the
    // gated surface is refused for it.
    let seeded = seed(&router, "managed-key-repo", None).await;
    for path in read_paths("managed-key-repo", &seeded) {
        let (status, payload) = call(&router, path.operation, &path.body).await;
        assert_eq!(
            status, 200,
            "case {:?} ({}): a repository under the managed key must serve, got {status} with \
             {payload}",
            path.why, path.operation
        );
    }
}
