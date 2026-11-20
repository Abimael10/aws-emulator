// SPDX-License-Identifier: CC0-1.0

//! `GetBlob` over the wire, driven as awsJson1_1 requests through the router that
//! serves the real emulator.
//!
//! The rule under test: `GetBlob` answers with the bytes the named repository holds
//! under the requested git object name, byte for byte and unchanged by being read,
//! and refuses every other request with the error the model declares for it — and a
//! refusal for a well-formed object name the repository holds no object for NAMES
//! that object name, so a caller can tell which id was missing. The tables below
//! span the class the rule covers — content kind and size, object-name case, which
//! repository holds the object, and each way a request can be at fault — rather than
//! one recorded request.
//!
//! The harness (`emulator`, `ok`, `error`, `string_at`, `base64`, `from_base64` and
//! the independent `git_blob_id` object-name oracle) is the shared one in
//! [`crate::repository_lifecycle`].

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::error_with_message;
use crate::repository_lifecycle::{base64, emulator, error, from_base64, git_blob_id, ok, string_at};

/// The status `BlobIdDoesNotExistException` models (`errorKind: client`,
/// `httpError: 400`, `smithy_query(query="operation",operation="GetBlob")`).
const CLIENT_ERROR_STATUS: u16 = 400;

/// What `GetBlob` says when the repository holds no object under the requested
/// name: fixed text either side of ONE slot, which is the object name the request
/// carried, spelled as it carried it. Written here as a literal rather than read
/// from the crate, so a change to the crate's wording cannot change what this
/// suite expects.
fn blob_id_does_not_exist(blob_id: &str) -> String {
    format!("AWS CodeCommit could not find the blob {blob_id} in the repository.")
}

/// One row of the content table: a file written into the repository, and the bytes
/// `GetBlob` must answer with for the object name that write reported.
#[derive(Debug, Clone)]
struct Blob {
    label: &'static str,
    path: &'static str,
    content: Vec<u8>,
}

/// The table: rows differ along what the rule generalises over — the content's
/// kind (ascii text, every byte value including NUL and 0xFF, UTF-8, text that is
/// itself base64) and its size (one byte, a few bytes, 4 KiB, 64 KiB). The sizes
/// stay well inside `FileContent`'s modeled `length` 0..6291456
/// (`smithy_query(query="operation",operation="PutFile")`).
fn blob_cases() -> Vec<Blob> {
    vec![
        Blob {
            label: "one byte",
            path: "one.txt",
            content: b"x".to_vec(),
        },
        Blob {
            label: "ascii text with a trailing newline",
            path: "README.md",
            content: b"# demo\nline two\n".to_vec(),
        },
        Blob {
            label: "ascii text without a trailing newline",
            path: "src/main.rs",
            content: b"fn main() {}".to_vec(),
        },
        Blob {
            label: "every byte value, including NUL and 0xFF",
            path: "bin/all-bytes.dat",
            content: (0..=255u8).collect(),
        },
        Blob {
            label: "utf-8 text outside ascii",
            path: "docs/guía.md",
            content: "⚙ configuración — ünïcode\n".as_bytes().to_vec(),
        },
        Blob {
            label: "content that is itself base64 text",
            path: "encoded.txt",
            content: base64(b"nested payload").into_bytes(),
        },
        Blob {
            label: "4 KiB of repeating bytes",
            path: "data/4k.bin",
            content: (0..=255u8).cycle().take(4 * 1024).collect(),
        },
        Blob {
            label: "64 KiB of repeating bytes",
            path: "data/64k.bin",
            content: (0..=255u8).cycle().take(64 * 1024).collect(),
        },
    ]
}

/// Write every row of the table into one repository, chaining each commit onto the
/// branch tip, and answer the object name each write reported.
async fn write_cases(
    router: &MultiServiceRouter,
    repository: &str,
    branch: &str,
    cases: &[Blob],
) -> BTreeMap<&'static str, String> {
    ok(
        router,
        "CreateRepository",
        &json!({ "repositoryName": repository }),
    )
    .await;

    let mut tip: Option<String> = None;
    let mut blob_ids: BTreeMap<&'static str, String> = BTreeMap::new();
    for case in cases {
        let mut input = json!({
            "repositoryName": repository,
            "branchName": branch,
            "filePath": case.path,
            "fileContent": base64(&case.content),
            "commitMessage": format!("add {}", case.path),
        });
        if let Some(parent) = &tip {
            input["parentCommitId"] = json!(parent);
        }
        let put = ok(router, "PutFile", &input).await;
        let blob_id = string_at(&put, "/blobId");
        assert_eq!(
            blob_id,
            git_blob_id(&case.content),
            "case {:?}: blobId must be the object name of the content written",
            case.label
        );
        tip = Some(string_at(&put, "/commitId"));
        blob_ids.insert(case.label, blob_id);
    }
    blob_ids
}

#[tokio::test]
async fn a_written_blob_is_read_back_byte_identical() {
    let router = emulator();
    let cases = blob_cases();
    let blob_ids = write_cases(&router, "blobs", "main", &cases).await;

    for case in &cases {
        let label = case.label;
        let blob_id = blob_ids
            .get(label)
            .unwrap_or_else(|| panic!("case {label:?}: no blobId was recorded"));

        // GetBlob answers with exactly the bytes PutFile stored.
        let input = json!({ "repositoryName": "blobs", "blobId": blob_id });
        let got = ok(&router, "GetBlob", &input).await;
        let content = from_base64(&string_at(&got, "/content"));
        assert_eq!(
            content, case.content,
            "case {label:?}: GetBlob must answer the bytes that were written; got {} bytes, expected {}",
            content.len(),
            case.content.len()
        );

        // An object name is a hexadecimal number: its case does not change which
        // object it names.
        let upper = json!({
            "repositoryName": "blobs",
            "blobId": blob_id.to_ascii_uppercase(),
        });
        let from_upper = ok(&router, "GetBlob", &upper).await;
        assert_eq!(
            from_base64(&string_at(&from_upper, "/content")),
            case.content,
            "case {label:?}: an upper-case object name names the same object, got {from_upper}"
        );

        // A read does not change what the next caller sees.
        let again = ok(&router, "GetBlob", &input).await;
        assert_eq!(
            again, got,
            "case {label:?}: GetBlob must be repeatable, got {again} then {got}"
        );

        // The same bytes are what the file reader reports for that object.
        let file = ok(
            &router,
            "GetFile",
            &json!({ "repositoryName": "blobs", "filePath": case.path }),
        )
        .await;
        assert_eq!(
            string_at(&file, "/blobId"),
            *blob_id,
            "case {label:?}: GetFile must report the object name GetBlob was read by"
        );
        assert_eq!(
            from_base64(&string_at(&file, "/fileContent")),
            case.content,
            "case {label:?}: GetFile and GetBlob must agree on the content"
        );
    }

    // Distinct contents are distinct objects, and every one of them is still
    // addressable after all the reads above.
    let distinct: std::collections::BTreeSet<&String> = blob_ids.values().collect();
    assert_eq!(
        distinct.len(),
        cases.len(),
        "distinct contents must have distinct object names, got {blob_ids:?}"
    );
}

#[tokio::test]
async fn a_blob_is_only_readable_in_a_repository_that_holds_it() {
    let router = emulator();
    let shared = b"shared content\n".to_vec();
    let only_in_left = b"left only\n".to_vec();

    for repository in ["left", "right"] {
        ok(
            &router,
            "CreateRepository",
            &json!({ "repositoryName": repository }),
        )
        .await;
        let put = ok(
            &router,
            "PutFile",
            &json!({
                "repositoryName": repository,
                "branchName": "main",
                "filePath": "shared.txt",
                "fileContent": base64(&shared),
            }),
        )
        .await;
        // Identical content is the same object name in every repository that
        // holds it, because the name is a digest of the content.
        assert_eq!(
            string_at(&put, "/blobId"),
            git_blob_id(&shared),
            "repository {repository:?}: identical content must have the same object name"
        );
        let got = ok(
            &router,
            "GetBlob",
            &json!({ "repositoryName": repository, "blobId": git_blob_id(&shared) }),
        )
        .await;
        assert_eq!(
            from_base64(&string_at(&got, "/content")),
            shared,
            "repository {repository:?}: the shared object must read back from each repository"
        );
    }

    // An object only one repository holds does not exist in the other.
    let tip = string_at(
        &ok(
            &router,
            "GetBranch",
            &json!({ "repositoryName": "left", "branchName": "main" }),
        )
        .await,
        "/branch/commitId",
    );
    ok(
        &router,
        "PutFile",
        &json!({
            "repositoryName": "left",
            "branchName": "main",
            "filePath": "left.txt",
            "fileContent": base64(&only_in_left),
            "parentCommitId": tip,
        }),
    )
    .await;
    let left_only_id = git_blob_id(&only_in_left);
    let got = ok(
        &router,
        "GetBlob",
        &json!({ "repositoryName": "left", "blobId": left_only_id }),
    )
    .await;
    assert_eq!(
        from_base64(&string_at(&got, "/content")),
        only_in_left,
        "the object written to left must read back from left"
    );
    let code = error(
        &router,
        "GetBlob",
        &json!({ "repositoryName": "right", "blobId": left_only_id }),
        400,
    )
    .await;
    assert_eq!(
        code, "BlobIdDoesNotExistException",
        "an object another repository holds must not be readable here, got {code}"
    );
}

#[tokio::test]
async fn get_blob_is_refused_with_the_modeled_error() {
    let router = emulator();
    let content = b"guarded content\n".to_vec();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "guarded-blobs" }),
    )
    .await;
    let put = ok(
        &router,
        "PutFile",
        &json!({
            "repositoryName": "guarded-blobs",
            "branchName": "main",
            "filePath": "README.md",
            "fileContent": base64(&content),
        }),
    )
    .await;
    let stored = string_at(&put, "/blobId");

    // Each row is a `GetBlob` request that cannot be served, and the error the
    // model names for it. They differ along which part of the request is at
    // fault: the repository, the presence of the object name, its form, and
    // whether the repository holds that object.
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a repository that does not exist",
            json!({ "repositoryName": "no-such-repo", "blobId": stored.clone() }),
            "RepositoryDoesNotExistException",
        ),
        (
            "no repository name at all",
            json!({ "blobId": stored.clone() }),
            "RepositoryNameRequiredException",
        ),
        (
            // `RepositoryName`'s modeled pattern is ^[\w\.-]+$.
            "a repository name outside the modeled pattern",
            json!({ "repositoryName": "not a name!", "blobId": stored.clone() }),
            "InvalidRepositoryNameException",
        ),
        (
            "no blob id at all",
            json!({ "repositoryName": "guarded-blobs" }),
            "BlobIdRequiredException",
        ),
        (
            "an empty blob id",
            json!({ "repositoryName": "guarded-blobs", "blobId": "" }),
            "BlobIdRequiredException",
        ),
        (
            "a blank blob id",
            json!({ "repositoryName": "guarded-blobs", "blobId": "   " }),
            "BlobIdRequiredException",
        ),
        (
            "a blob id one character short of an object name",
            json!({ "repositoryName": "guarded-blobs", "blobId": "a".repeat(39) }),
            "InvalidBlobIdException",
        ),
        (
            "a blob id one character longer than an object name",
            json!({ "repositoryName": "guarded-blobs", "blobId": "a".repeat(41) }),
            "InvalidBlobIdException",
        ),
        (
            "a blob id of the right width that is not hexadecimal",
            json!({ "repositoryName": "guarded-blobs", "blobId": "z".repeat(40) }),
            "InvalidBlobIdException",
        ),
        (
            "a blob id that is not an object name at all",
            json!({ "repositoryName": "guarded-blobs", "blobId": "not-an-object-name" }),
            "InvalidBlobIdException",
        ),
        (
            "a well-formed object name this repository does not hold",
            json!({ "repositoryName": "guarded-blobs", "blobId": "0".repeat(40) }),
            "BlobIdDoesNotExistException",
        ),
        (
            "the object name of content nobody wrote",
            json!({
                "repositoryName": "guarded-blobs",
                "blobId": git_blob_id(b"never written"),
            }),
            "BlobIdDoesNotExistException",
        ),
    ];

    for (label, input, expected) in cases {
        let code = error(&router, "GetBlob", &input, 400).await;
        assert_eq!(
            code, expected,
            "case {label:?} ({input}): expected {expected}, got {code}"
        );
    }

    // None of the refusals changed what the repository holds.
    let got = ok(
        &router,
        "GetBlob",
        &json!({ "repositoryName": "guarded-blobs", "blobId": stored }),
    )
    .await;
    assert_eq!(
        from_base64(&string_at(&got, "/content")),
        content,
        "a refused read must leave the stored object readable"
    );
}

/// The refusal of a well-formed object name the repository holds no object for
/// IDENTIFIES that object name: the message is a template whose single slot is
/// the `blobId` the request carried, verbatim, so a caller resolving several
/// object names in one pass can tell which one was missing.
///
/// The rows span what the rule generalises over — how the absent name was arrived
/// at (the content-addressed name of content never written, of empty content, of
/// 64 KiB of content, the name another repository holds), each end of the
/// object-name range, and the case it is spelled in (an object name is
/// hexadecimal, so its case does not change which object it names, and the
/// message still reports the spelling the request used) — and each row is asked
/// of a repository that holds objects AND of one that holds none, so a template
/// that named the repository would differ between them.
#[tokio::test]
async fn an_absent_object_name_is_refused_by_the_name_that_was_asked_for() {
    let router = emulator();

    // One repository with content in it and one with none. Neither name is a
    // substring of the message's fixed text, so the assertions below can check
    // that the repository reaches no part of it.
    let held = b"held content\n".to_vec();
    let holder = "holder-repo";
    let empty = "empty-repo";
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": holder }),
    )
    .await;
    let put = ok(
        &router,
        "PutFile",
        &json!({
            "repositoryName": holder,
            "branchName": "main",
            "filePath": "held.txt",
            "fileContent": base64(&held),
        }),
    )
    .await;
    let held_id = string_at(&put, "/blobId");
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": empty }),
    )
    .await;

    // The object the holder DOES hold reads back, so every row below is refused
    // on the lookup rather than on the repository or on the object name's form.
    let read = ok(
        &router,
        "GetBlob",
        &json!({ "repositoryName": holder, "blobId": held_id }),
    )
    .await;
    assert_eq!(
        from_base64(&string_at(&read, "/content")),
        held,
        "the object the repository holds must read back, got {read}"
    );

    // Content written to a THIRD repository, so its object name is well formed
    // and stored somewhere, but not in either repository asked below.
    let elsewhere = b"written elsewhere\n".to_vec();
    ok(
        &router,
        "CreateRepository",
        &json!({ "repositoryName": "other-repo" }),
    )
    .await;
    ok(
        &router,
        "PutFile",
        &json!({
            "repositoryName": "other-repo",
            "branchName": "main",
            "filePath": "elsewhere.txt",
            "fileContent": base64(&elsewhere),
        }),
    )
    .await;

    let absent: Vec<(&str, String)> = vec![
        ("the name of content never written", git_blob_id(b"never written\n")),
        ("the name of empty content", git_blob_id(b"")),
        (
            "the name of 64 KiB never written",
            git_blob_id(&(0..=255u8).cycle().take(64 * 1024).collect::<Vec<u8>>()),
        ),
        ("the lowest object name", "0".repeat(40)),
        ("the highest object name", "f".repeat(40)),
        (
            "an object name in mixed case",
            "AbCdEf0123456789aBcDeF0123456789AbCdEf01".to_string(),
        ),
        (
            "the name of content only another repository holds",
            git_blob_id(&elsewhere),
        ),
    ];

    let mut messages: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (label, blob_id) in &absent {
        for repository in [holder, empty] {
            let (code, message) = error_with_message(
                &router,
                "GetBlob",
                &json!({ "repositoryName": repository, "blobId": blob_id }),
                CLIENT_ERROR_STATUS,
            )
            .await;
            assert_eq!(
                code, "BlobIdDoesNotExistException",
                "case ({label}, {repository}): expected code BlobIdDoesNotExistException, \
                 got {code:?}"
            );
            let expected = blob_id_does_not_exist(blob_id);
            assert_eq!(
                message, expected,
                "case ({label}, {repository}): expected message {expected:?}, got {message:?}"
            );
            // The slot carries the id the REQUEST spelled, so the caller can match
            // the refusal to the id it asked for.
            assert!(
                message.contains(blob_id.as_str()),
                "case ({label}, {repository}): the message must name the requested object \
                 name {blob_id:?}, got {message:?}"
            );
            // The repository is not part of the sentence.
            assert!(
                !message.contains(repository),
                "case ({label}, {repository}): the message must not name the repository, \
                 got {message:?}"
            );
            // Asking again says exactly the same thing: the refusal reports state,
            // it does not change it.
            let (again_code, again_message) = error_with_message(
                &router,
                "GetBlob",
                &json!({ "repositoryName": repository, "blobId": blob_id }),
                CLIENT_ERROR_STATUS,
            )
            .await;
            assert_eq!(
                (again_code.as_str(), again_message.as_str()),
                (code.as_str(), message.as_str()),
                "case ({label}, {repository}): the refusal must be repeatable, got \
                 {again_code:?} {again_message:?}"
            );
            messages.insert(message);
        }
    }

    // One sentence per absent object name, not one sentence for all of them: the
    // rows above are distinct ids, so they produce distinct messages.
    let distinct_ids: std::collections::BTreeSet<&String> =
        absent.iter().map(|(_, blob_id)| blob_id).collect();
    assert_eq!(
        messages.len(),
        distinct_ids.len(),
        "each absent object name must get its own sentence, got {messages:?}"
    );

    // Every refusal above left the holder's object exactly as it was.
    let after = ok(
        &router,
        "GetBlob",
        &json!({ "repositoryName": holder, "blobId": held_id }),
    )
    .await;
    assert_eq!(
        after, read,
        "the refusals must leave the stored object readable, got {after} then {read}"
    );
}
