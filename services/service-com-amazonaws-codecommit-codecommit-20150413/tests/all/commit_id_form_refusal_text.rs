// SPDX-License-Identifier: CC0-1.0

//! What CodeCommit SAYS when a value offered as a git commit id is not one, and
//! when a commit is asked for with nothing to change, over the wire.
//!
//! Three rules, each about the text (and for one of them the CODE) of a refusal
//! rather than about the status, which is 400 for every error here:
//!
//! 1. A member the model types as a raw git object id (`CommitId`/`ObjectId`,
//!    both unconstrained — `smithy_query(query="shape",shape="ObjectId")`) is a
//!    git object NAME, so a present value that is not one is refused as
//!    `InvalidCommitIdException` with the rule it broke — it is not a valid Git
//!    object ID — and the rejected value itself, whichever operation and
//!    whichever such member carried it. A member supplied with no characters at
//!    all is the other class, `CommitIdRequiredException`, whose sentence names
//!    nothing.
//! 2. A `parentCommitId` is a git object name too, and its FORM is checked
//!    before anything is looked up: a value that is not one is refused as
//!    `InvalidParentCommitIdException` with the FIXED sentence for the git rule
//!    it broke (the width of an object name, or its hexadecimal alphabet),
//!    naming neither the value nor the repository — whichever write path took it
//!    and whether or not the repository holds the branch or any commit. Only a
//!    WELL-FORMED id no commit of the repository carries reaches the existence
//!    class, `ParentCommitDoesNotExistException`.
//! 3. A commit asked for with no file entry at all is refused as
//!    `FileEntryRequiredException` with a fixed sentence pair — the condition,
//!    then the rule — carrying nothing from the request, however the caller
//!    spelled the empty change and whatever the branch's state is.
//!
//! Each table varies what its rule generalises over rather than one request: the
//! way the rejected value fails the object-name rule (too short, too long, the
//! right width but not hexadecimal, a reference name, a fully qualified
//! reference, whitespace, empty), the member and the operation that carried it,
//! the shape of the repository (no branch at all, a branch with a tip), and the
//! way a commit can name no change (each entry list absent, or present and
//! empty).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::error_with_message;
use crate::repository_lifecycle::{base64, emulator, ok, string_at};

/// The HTTP status each error asserted here models (`errorKind: client`,
/// `httpError: 400` on every one of these error shapes).
const CLIENT_ERROR_STATUS: u16 = 400;

/// A syntactically valid git object id (40 hexadecimal characters) that no
/// repository here holds, so a request carrying it gets past every form check and
/// reaches the lookup under test.
const ABSENT_OBJECT_ID: &str = "0123456789abcdef0123456789abcdef01234567";

/// `CommitIdRequiredException`'s whole text, and the two `InvalidParentCommitId`
/// and the `FileEntryRequired` sentences. Each is asserted as a literal rather
/// than read from the crate, so a change to the crate's constant cannot change
/// what this suite expects.
const COMMIT_ID_REQUIRED: &str = "A commit ID is required, but no commit ID was specified.";
const PARENT_COMMIT_ID_WRONG_LENGTH: &str = "The parent commit ID is not valid. The commit ID is \
                                             not a valid length for commit IDs in Git.";
const PARENT_COMMIT_ID_NOT_HEXADECIMAL: &str =
    "The parent commit ID is not valid. The commit ID is not a valid Git object ID.";
const PARENT_COMMIT_DOES_NOT_EXIST: &str =
    "The parent commit ID is not valid. The specified parent commit ID does not exist.";
const FILE_ENTRY_REQUIRED: &str = "A commit cannot be created because there are no changed files \
                                   included in the commit. A commit requires at least one change.";

/// `InvalidCommitIdException`'s text for a value that does not name a git object:
/// the rule broken, then the rejected value.
fn invalid_commit_id(commit_id: &str) -> String {
    format!("The specified commit ID is not a valid Git object ID: {commit_id}")
}

/// A repository with one commit on `branch` holding `path`, answering that
/// commit's id. The first write of an empty repository is what creates the
/// branch, so it carries no `parentCommitId`.
async fn seeded(
    router: &MultiServiceRouter,
    repository: &str,
    branch: &str,
    path: &str,
    content: &[u8],
) -> String {
    ok(
        router,
        "CreateRepository",
        &json!({ "repositoryName": repository }),
    )
    .await;
    let written = ok(
        router,
        "PutFile",
        &json!({
            "repositoryName": repository,
            "branchName": branch,
            "filePath": path,
            "fileContent": base64(content),
        }),
    )
    .await;
    string_at(&written, "/commitId")
}

/// The ways a value can fail the git object-name rule, which is a WIDTH (40) and
/// an ALPHABET (hexadecimal): each row is a value no git object could be named
/// by, paired with whether it fails on the width or on the alphabet.
///
/// `tip` is a real object name, so the rows built from it differ from a name that
/// resolves by exactly one character.
fn malformed_object_names(tip: &str) -> Vec<(&'static str, String, bool)> {
    vec![
        ("a reference name", "HEAD".to_string(), true),
        (
            "a fully qualified reference",
            "refs/heads/main".to_string(),
            true,
        ),
        ("a branch name", "main".to_string(), true),
        ("no characters at all", String::new(), true),
        ("whitespace", "   ".to_string(), true),
        ("a shortened object name", tip[..7].to_string(), true),
        (
            "one character short of an object name",
            tip[..39].to_string(),
            true,
        ),
        ("one character past an object name", format!("{tip}0"), true),
        ("the right width but not hexadecimal", "z".repeat(40), false),
        (
            "the right width with one non-hexadecimal character",
            format!("{}-", &tip[..39]),
            false,
        ),
    ]
}

/// Rule 1 — every member the model types as a raw git object id naming a commit
/// is refused with the object-ID rule and the value it rejected, whichever
/// operation carried it.
#[tokio::test]
async fn invalid_commit_id_message_names_the_rejected_id() {
    let router = emulator();
    let repository = "commit-id-wording";
    let tip = seeded(&router, repository, "main", "README.md", b"# demo\n").await;

    for (label, rejected, _) in malformed_object_names(&tip) {
        // An id carrying no character names no commit and is the REQUIRED class,
        // not the malformed one: it is asserted by its own test below.
        if rejected.is_empty() {
            continue;
        }
        // Each row is asked of every operation that takes a commit id by a member
        // of its own: the commit READ, the branch write, and both the read and the
        // write of a comparison's comments.
        let requests: Vec<(&str, Value)> = vec![
            (
                "GetCommit",
                json!({ "repositoryName": repository, "commitId": rejected }),
            ),
            (
                "CreateBranch",
                json!({
                    "repositoryName": repository,
                    "branchName": "from-a-malformed-id",
                    "commitId": rejected,
                }),
            ),
            (
                "GetCommentsForComparedCommit",
                json!({ "repositoryName": repository, "afterCommitId": rejected }),
            ),
            (
                "GetCommentsForComparedCommit",
                json!({
                    "repositoryName": repository,
                    "beforeCommitId": rejected,
                    "afterCommitId": tip,
                }),
            ),
            (
                "PostCommentForComparedCommit",
                json!({
                    "repositoryName": repository,
                    "afterCommitId": rejected,
                    "content": "a comment",
                }),
            ),
        ];
        for (operation, body) in requests {
            let (code, message) =
                error_with_message(&router, operation, &body, CLIENT_ERROR_STATUS).await;
            assert_eq!(
                code, "InvalidCommitIdException",
                "case ({operation}, {label}): expected code InvalidCommitIdException for \
                 {rejected:?}, got {code:?}"
            );
            let expected = invalid_commit_id(&rejected);
            assert_eq!(
                message, expected,
                "case ({operation}, {label}): expected message {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains(repository),
                "case ({operation}, {label}): the message must not name the repository, got \
                 {message:?}"
            );
        }
    }
}

/// Rule 1's other half — a commit id supplied with no character at all is the
/// REQUIRED class, whose sentence names nothing, so a caller can tell a malformed
/// id from one that was never given.
#[tokio::test]
async fn an_empty_commit_id_is_the_required_class() {
    let router = emulator();
    let repository = "commit-id-absent";
    let tip = seeded(&router, repository, "main", "README.md", b"# demo\n").await;

    let requests: Vec<(&str, Value)> = vec![
        (
            "GetCommit",
            json!({ "repositoryName": repository, "commitId": "" }),
        ),
        (
            "CreateBranch",
            json!({
                "repositoryName": repository,
                "branchName": "from-no-id",
                "commitId": "",
            }),
        ),
        (
            "GetCommentsForComparedCommit",
            json!({ "repositoryName": repository, "afterCommitId": "" }),
        ),
        (
            "PostCommentForComparedCommit",
            json!({
                "repositoryName": repository,
                "afterCommitId": "",
                "content": "a comment",
            }),
        ),
        // The member left out altogether, which the request boundary refuses.
        (
            "GetCommit",
            json!({ "repositoryName": repository }),
        ),
        (
            "GetCommentsForComparedCommit",
            json!({ "repositoryName": repository, "beforeCommitId": tip }),
        ),
    ];
    for (operation, body) in requests {
        let (code, message) =
            error_with_message(&router, operation, &body, CLIENT_ERROR_STATUS).await;
        assert_eq!(
            code, "CommitIdRequiredException",
            "case ({operation}, {body}): expected code CommitIdRequiredException, got {code:?}"
        );
        assert_eq!(
            message, COMMIT_ID_REQUIRED,
            "case ({operation}, {body}): expected message {COMMIT_ID_REQUIRED:?}, got \
             {message:?}"
        );
    }
}

/// Rule 2 — a malformed `parentCommitId` is refused on FORM, with the fixed
/// sentence for the git rule it broke and nothing of the request, from every
/// write path and whether or not the repository holds the branch it names.
#[tokio::test]
async fn a_malformed_parent_commit_id_is_refused_on_form() {
    let router = emulator();

    // A repository with a tip, and an EMPTY one that holds no branch at all: the
    // form check precedes every lookup, so both answer the same refusal.
    let seeded_repository = "parent-form-seeded";
    let branch = "main";
    let tip = seeded(
        &router,
        seeded_repository,
        branch,
        "README.md",
        b"# demo\n",
    )
    .await;
    let empty_repository = "parent-form-empty";
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": empty_repository }),
    )
    .await;

    for (label, parent, wrong_length) in malformed_object_names(&tip) {
        let expected = if wrong_length {
            PARENT_COMMIT_ID_WRONG_LENGTH
        } else {
            PARENT_COMMIT_ID_NOT_HEXADECIMAL
        };
        let requests: Vec<(&str, &str, Value)> = vec![
            (
                "PutFile",
                "a branch with a tip",
                json!({
                    "repositoryName": seeded_repository,
                    "branchName": branch,
                    "filePath": "added.txt",
                    "fileContent": base64(b"added"),
                    "parentCommitId": parent,
                }),
            ),
            (
                "CreateCommit",
                "a branch with a tip",
                json!({
                    "repositoryName": seeded_repository,
                    "branchName": branch,
                    "parentCommitId": parent,
                    "putFiles": [
                        { "filePath": "created.txt", "fileContent": base64(b"created") },
                    ],
                }),
            ),
            (
                "DeleteFile",
                "a branch with a tip",
                json!({
                    "repositoryName": seeded_repository,
                    "branchName": branch,
                    "filePath": "README.md",
                    "parentCommitId": parent,
                }),
            ),
            (
                "PutFile",
                "a repository holding no branch",
                json!({
                    "repositoryName": empty_repository,
                    "branchName": branch,
                    "filePath": "added.txt",
                    "fileContent": base64(b"added"),
                    "parentCommitId": parent,
                }),
            ),
            (
                "CreateCommit",
                "a repository holding no branch",
                json!({
                    "repositoryName": empty_repository,
                    "branchName": branch,
                    "parentCommitId": parent,
                    "putFiles": [
                        { "filePath": "created.txt", "fileContent": base64(b"created") },
                    ],
                }),
            ),
        ];
        for (operation, shape, body) in requests {
            let (code, message) =
                error_with_message(&router, operation, &body, CLIENT_ERROR_STATUS).await;
            assert_eq!(
                code, "InvalidParentCommitIdException",
                "case ({operation}, {shape}, {label}): expected code \
                 InvalidParentCommitIdException for {parent:?}, got {code:?}"
            );
            assert_eq!(
                message, expected,
                "case ({operation}, {shape}, {label}): expected message {expected:?}, got \
                 {message:?}"
            );
            if !parent.trim().is_empty() {
                assert!(
                    !message.contains(parent.as_str()),
                    "case ({operation}, {shape}, {label}): the message must not name the parent \
                     commit id, got {message:?}"
                );
            }
            for named in [seeded_repository, empty_repository, branch] {
                assert!(
                    !message.contains(named),
                    "case ({operation}, {shape}, {label}): the message must not name \
                     {named:?}, got {message:?}"
                );
            }
        }
    }
}

/// Rule 2's other half — a WELL-FORMED parent commit id the repository does not
/// hold is the EXISTENCE class, so the form rejection above cannot swallow it.
#[tokio::test]
async fn a_well_formed_parent_commit_id_no_commit_carries_is_the_existence_class() {
    let router = emulator();
    let seeded_repository = "parent-existence-seeded";
    let branch = "main";
    seeded(
        &router,
        seeded_repository,
        branch,
        "README.md",
        b"# demo\n",
    )
    .await;
    let empty_repository = "parent-existence-empty";
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": empty_repository }),
    )
    .await;

    // Object names whose FORM is a git object name's, that no commit here carries.
    let absent: Vec<(&str, &str)> = vec![
        ("an all-zero object name", "0000000000000000000000000000000000000000"),
        ("a mixed-digit object name", ABSENT_OBJECT_ID),
        ("an all-f object name", "ffffffffffffffffffffffffffffffffffffffff"),
        ("an upper-case object name", "0123456789ABCDEF0123456789ABCDEF01234567"),
    ];
    for (label, parent) in absent {
        let requests: Vec<(&str, &str, Value)> = vec![
            (
                "PutFile",
                "a branch with a tip",
                json!({
                    "repositoryName": seeded_repository,
                    "branchName": branch,
                    "filePath": "added.txt",
                    "fileContent": base64(b"added"),
                    "parentCommitId": parent,
                }),
            ),
            (
                "CreateCommit",
                "a branch with a tip",
                json!({
                    "repositoryName": seeded_repository,
                    "branchName": branch,
                    "parentCommitId": parent,
                    "putFiles": [
                        { "filePath": "created.txt", "fileContent": base64(b"created") },
                    ],
                }),
            ),
            (
                "DeleteFile",
                "a branch with a tip",
                json!({
                    "repositoryName": seeded_repository,
                    "branchName": branch,
                    "filePath": "README.md",
                    "parentCommitId": parent,
                }),
            ),
            (
                "PutFile",
                "a repository holding no branch",
                json!({
                    "repositoryName": empty_repository,
                    "branchName": branch,
                    "filePath": "added.txt",
                    "fileContent": base64(b"added"),
                    "parentCommitId": parent,
                }),
            ),
        ];
        for (operation, shape, body) in requests {
            let (code, message) =
                error_with_message(&router, operation, &body, CLIENT_ERROR_STATUS).await;
            assert_eq!(
                code, "ParentCommitDoesNotExistException",
                "case ({operation}, {shape}, {label}): expected code \
                 ParentCommitDoesNotExistException for {parent:?}, got {code:?}"
            );
            assert_eq!(
                message, PARENT_COMMIT_DOES_NOT_EXIST,
                "case ({operation}, {shape}, {label}): expected message \
                 {PARENT_COMMIT_DOES_NOT_EXIST:?}, got {message:?}"
            );
        }
    }
}

/// Rule 3 — a commit that names no change at all is refused with the fixed
/// sentence pair, however the caller spelled the empty change and whatever the
/// branch's state is, and the text carries nothing from the request.
#[tokio::test]
async fn a_commit_with_no_file_entry_is_refused_with_the_fixed_sentence() {
    let router = emulator();
    let seeded_repository = "no-entries-seeded";
    let branch = "main";
    let tip = seeded(
        &router,
        seeded_repository,
        branch,
        "README.md",
        b"# demo\n",
    )
    .await;
    let empty_repository = "no-entries-empty";
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": empty_repository }),
    )
    .await;

    // The ways one request can name no change: each entry list left out, or
    // carried and empty.
    let empties: Vec<(&str, Value)> = vec![
        ("no entry member at all", json!({})),
        ("an empty putFiles list", json!({ "putFiles": [] })),
        ("an empty deleteFiles list", json!({ "deleteFiles": [] })),
        ("an empty setFileModes list", json!({ "setFileModes": [] })),
        (
            "all three lists carried and empty",
            json!({ "putFiles": [], "deleteFiles": [], "setFileModes": [] }),
        ),
    ];
    // The branch states a commit can be asked for in: the tip named, the tip left
    // unnamed on a branch that has one, and a repository holding no branch.
    let targets: Vec<(&str, Value)> = vec![
        (
            "the branch tip named",
            json!({
                "repositoryName": seeded_repository,
                "branchName": branch,
                "parentCommitId": tip,
            }),
        ),
        (
            "a branch with a tip and no parent named",
            json!({ "repositoryName": seeded_repository, "branchName": branch }),
        ),
        (
            "a repository holding no branch",
            json!({ "repositoryName": empty_repository, "branchName": branch }),
        ),
    ];

    for (entries_label, entries) in &empties {
        for (target_label, target) in &targets {
            let mut body = target.clone();
            let object = body.as_object_mut().expect("the row is a JSON object");
            for (member, value) in entries.as_object().expect("the row is a JSON object") {
                object.insert(member.clone(), value.clone());
            }
            let (code, message) =
                error_with_message(&router, "CreateCommit", &body, CLIENT_ERROR_STATUS).await;
            assert_eq!(
                code, "FileEntryRequiredException",
                "case ({entries_label}, {target_label}): expected code \
                 FileEntryRequiredException, got {code:?}"
            );
            assert_eq!(
                message, FILE_ENTRY_REQUIRED,
                "case ({entries_label}, {target_label}): expected message \
                 {FILE_ENTRY_REQUIRED:?}, got {message:?}"
            );
            for named in [seeded_repository, empty_repository, branch, tip.as_str()] {
                assert!(
                    !message.contains(named),
                    "case ({entries_label}, {target_label}): the message must not name \
                     {named:?}, got {message:?}"
                );
            }
        }
    }

    // Nothing was committed by any of the refusals: the branch is still at the
    // commit the seeding write created.
    let read = ok(
        &router,
        "GetBranch",
        &json!({ "repositoryName": seeded_repository, "branchName": branch }),
    )
    .await;
    assert_eq!(
        string_at(&read, "/branch/commitId"),
        tip,
        "a refused commit must leave the branch tip where it was, got {read}"
    );
}
