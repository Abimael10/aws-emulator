// SPDX-License-Identifier: CC0-1.0

//! What CodeCommit SAYS when a request does not name a resource it has to name,
//! over the wire.
//!
//! One rule: a refusal that says a required member was not supplied carries the
//! SERVICE's own fixed sentence for that error code — the same text whichever
//! operation raises it, and whether the model marks the member `@required` (so the
//! request boundary refuses it) or leaves it optional (so the handler does). The
//! text names nothing of the request: it states the requirement, and for a branch
//! name also the rule that a name carrying no character does not satisfy it.
//!
//! The tables vary what the rule generalises over rather than one request: which
//! member is missing, the operation (reads and writes, branch and file paths), how
//! the member comes to be unsupplied (left out of an operation that requires it,
//! left out of one that does not), and what else the request does carry.
//!
//! `GetBranch` is the operation whose `repositoryName` and `branchName` the model
//! leaves OPTIONAL (`smithy_query(query="operation",operation="GetBranch")`), so
//! its refusals are the handler's own and are what pin boundary and handler to one
//! wording.

use serde_json::{json, Value};

use crate::harness::error_with_message;
use crate::repository_lifecycle::{base64, emulator, ok};

/// The HTTP status each error asserted here models (`errorKind: client`,
/// `httpError: 400` on both error shapes).
const CLIENT_ERROR_STATUS: u16 = 400;

/// The two texts under test, asserted as literals rather than read from the crate,
/// so a change to the crate's constant cannot change what this suite expects.
const BRANCH_NAME_REQUIRED: &str = "You must provide a branch name. The branch name cannot be \
                                    empty.";
const REPOSITORY_NAME_REQUIRED: &str = "A repository name is required, but was not specified.";

/// A repository with one commit on `main` holding `README.md`, so the requests
/// below are refused for the member they omit rather than for naming no
/// repository.
async fn seeded(router: &smithy_server_runtime::MultiServiceRouter, repository: &str) {
    ok(
        router,
        "CreateRepository",
        &json!({ "repositoryName": repository }),
    )
    .await;
    ok(
        router,
        "PutFile",
        &json!({
            "repositoryName": repository,
            "branchName": "main",
            "filePath": "README.md",
            "fileContent": base64(b"# demo\n"),
        }),
    )
    .await;
}

/// A request naming no branch is refused with the one branch-name sentence,
/// whichever operation it was sent to.
#[tokio::test]
async fn a_request_naming_no_branch_carries_the_one_sentence() {
    let router = emulator();
    let repository = "branch-name-required";
    seeded(&router, repository).await;

    // Every operation that takes a branch name, each sent without one: the writes
    // and the branch control plane (whose member the model requires, so the request
    // boundary refuses it) and `GetBranch` (whose members it leaves optional, so
    // the handler does).
    let requests: Vec<(&str, Value)> = vec![
        (
            "PutFile",
            json!({
                "repositoryName": repository,
                "filePath": "added.txt",
                "fileContent": base64(b"added"),
            }),
        ),
        (
            "CreateCommit",
            json!({
                "repositoryName": repository,
                "putFiles": [{ "filePath": "created.txt", "fileContent": base64(b"created") }],
            }),
        ),
        (
            "DeleteFile",
            json!({
                "repositoryName": repository,
                "filePath": "README.md",
                "parentCommitId": "0123456789abcdef0123456789abcdef01234567",
            }),
        ),
        (
            "CreateBranch",
            json!({
                "repositoryName": repository,
                "commitId": "0123456789abcdef0123456789abcdef01234567",
            }),
        ),
        ("DeleteBranch", json!({ "repositoryName": repository })),
        ("UpdateDefaultBranch", json!({ "repositoryName": repository })),
        ("GetBranch", json!({ "repositoryName": repository })),
    ];
    for (operation, body) in requests {
        let (code, message) =
            error_with_message(&router, operation, &body, CLIENT_ERROR_STATUS).await;
        assert_eq!(
            code, "BranchNameRequiredException",
            "case ({operation}, {body}): expected code BranchNameRequiredException, got {code:?}"
        );
        assert_eq!(
            message, BRANCH_NAME_REQUIRED,
            "case ({operation}, {body}): expected message {BRANCH_NAME_REQUIRED:?}, got \
             {message:?}"
        );
        assert!(
            !message.contains(repository),
            "case ({operation}, {body}): the message must not name the repository, got \
             {message:?}"
        );
    }
}

/// A request naming no repository is refused with the one repository-name
/// sentence, from the boundary and from the handler alike.
#[tokio::test]
async fn a_request_naming_no_repository_carries_the_one_sentence() {
    let router = emulator();

    let requests: Vec<(&str, Value)> = vec![
        (
            "GetCommit",
            json!({ "commitId": "0123456789abcdef0123456789abcdef01234567" }),
        ),
        (
            "PutFile",
            json!({
                "branchName": "main",
                "filePath": "added.txt",
                "fileContent": base64(b"added"),
            }),
        ),
        ("ListBranches", json!({})),
        ("GetBranch", json!({ "branchName": "main" })),
        ("GetBranch", json!({})),
    ];
    for (operation, body) in requests {
        let (code, message) =
            error_with_message(&router, operation, &body, CLIENT_ERROR_STATUS).await;
        assert_eq!(
            code, "RepositoryNameRequiredException",
            "case ({operation}, {body}): expected code RepositoryNameRequiredException, got \
             {code:?}"
        );
        assert_eq!(
            message, REPOSITORY_NAME_REQUIRED,
            "case ({operation}, {body}): expected message {REPOSITORY_NAME_REQUIRED:?}, got \
             {message:?}"
        );
    }
}
