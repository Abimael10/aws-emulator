// SPDX-License-Identifier: CC0-1.0

//! An image-not-found refusal as a SET: two different references never read the same, and a
//! request carrying no digest is told about no digest. End to end over the wire.
//!
//! The rule under test is the one the suites in `refusal_wording` and
//! `image_lookup_refusal_is_request_scoped` pin per call — a lookup resolving to no stored
//! image is refused with `ImageNotFoundException` (HTTP 400) stating the `ImageIdentifier`
//! the REQUEST sent, both members spelled out and a member the request omitted rendered as
//! the bare literal `null`, beside the repository the request named and the registry holding
//! it. What these two suites add is the property that follows from it ACROSS calls, which no
//! single-call assertion can state:
//!
//! 1. the sentences a repository holding no image answers a table of references with are
//!    pairwise DISTINCT, and a reference carrying no digest is answered with no digest at
//!    all — so no value the caller never sent can stand in for an unset member, and the
//!    refusal a pipeline reads tells it WHICH of its own references was rejected (whether
//!    the tag is simply unpushed, so build and retry, or the digest it pinned is gone, so
//!    fail fast);
//! 2. which of the two not-found refusals an EMPTY repository earns: the repository itself
//!    resolves — a read naming no identifier succeeds and reports no image — so a lookup in
//!    it is refused about the image, while the same lookup against a repository this
//!    registry does not hold is refused about the repository and names no identifier.
//!
//! The first suite's rows differ along what the rule generalises over: which members the
//! reference carries, the length of the tag (the shortest and the longest `ImageTag`'s
//! `@length(1..300)` admits), the characters in it, and the digest's own value. The reported
//! instance — a `latest` lookup on a repository nothing was ever pushed to — is the first
//! row. Every case goes through `crate::wire`'s router, so each exercises routing, the
//! boundary validator, the handler, the persisted state and serialization.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{at, call, emulator, ok, refused, REGISTRY_ID};

/// How a member of a rendered `ImageIdentifier` reads when the request carried none.
const UNSET_MEMBER: &str = "null";

/// A well-formed `sha256` digest nothing was ever pushed under, so a lookup by it resolves
/// to nothing.
const UNSTORED_DIGEST: &str =
    "sha256:4f8b2d6e0a9c13577b4d2e6a8c0f1359de7b2c4a6e80135779f4d2c6b8a0e135";

/// A second such digest, so the digest a refusal states cannot be one value these suites
/// happen to use everywhere.
const OTHER_UNSTORED_DIGEST: &str =
    "sha256:b1c2d3e4f5061728394a5b6c7d8e9f00112233445566778899aabbccddeeff00";

/// The error code an image-not-found refusal carries (`smithy_query(query="operation",
/// operation="DescribeImages")`: `ImageNotFoundException`, a client error at HTTP 400).
const IMAGE_NOT_FOUND: &str = "ImageNotFoundException";

/// The error code a refusal about the repository itself carries.
const REPOSITORY_NOT_FOUND: &str = "RepositoryNotFoundException";

/// One row: the reference a call sends, and the two slots the refusal must state.
struct Reference {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The `imageIds` entry the request carries.
    id: Value,
    /// The value the refusal's `imageDigest` slot must hold.
    digest: String,
    /// The value the refusal's `imageTag` slot must hold.
    tag: String,
}

/// The sentence an image held inside a repository is reported missing in, written out
/// literally: the service builds exactly this, so computing it from the implementation would
/// assert nothing.
fn image_not_found_sentence(digest: &str, tag: &str, repository: &str) -> String {
    format!(
        "The image with imageId {{imageDigest:'{digest}', imageTag:'{tag}'}} does not exist \
         within the repository with name '{repository}' in the registry with id '{REGISTRY_ID}'"
    )
}

/// The sentence a repository that is not in this registry is reported missing in.
fn repository_not_found_sentence(repository: &str) -> String {
    format!(
        "The repository with name '{repository}' does not exist in the registry with id \
         '{REGISTRY_ID}'"
    )
}

/// A repository, created through the operation that creates one, and left holding nothing.
async fn given_empty_repository(router: &MultiServiceRouter, case: &str, name: &str) {
    ok(
        router,
        case,
        "CreateRepository",
        &json!({"repositoryName": name}),
    )
    .await;
}

/// One row per reference class the rule generalises over, each with the slots its refusal
/// must state.
fn references(longest_tag: &str) -> Vec<Reference> {
    let by_tag = |case: &'static str, tag: &str| Reference {
        case,
        id: json!({"imageTag": tag}),
        digest: UNSET_MEMBER.to_string(),
        tag: tag.to_string(),
    };
    let by_digest = |case: &'static str, digest: &str| Reference {
        case,
        id: json!({"imageDigest": digest}),
        digest: digest.to_string(),
        tag: UNSET_MEMBER.to_string(),
    };
    vec![
        // The reported instance: a pipeline pulling `latest` from a repository nothing was
        // ever pushed to.
        by_tag("the tag a pipeline pulls", "latest"),
        by_tag("a version tag", "1.0"),
        by_tag("the shortest tag the model admits", "a"),
        by_tag("the longest tag the model admits", longest_tag),
        by_tag("a tag carrying separators", "release-1.2_3"),
        // A tag spelled like the absent-member rendering is a tag like any other: what each
        // slot holds is decided by the request, so the spelling of an absent member is not a
        // value the handler reads back out of one.
        by_tag("a tag spelled like the absent-member rendering", UNSET_MEMBER),
        by_digest("a digest nothing was pushed under", UNSTORED_DIGEST),
        by_digest("another digest nothing was pushed under", OTHER_UNSTORED_DIGEST),
        Reference {
            case: "a digest and a tag together",
            id: json!({"imageDigest": UNSTORED_DIGEST, "imageTag": "latest"}),
            digest: UNSTORED_DIGEST.to_string(),
            tag: "latest".to_string(),
        },
    ]
}

/// A repository holding no image answers two different references with two different
/// sentences, each stating the reference its own call sent — and a call carrying no digest is
/// answered with no digest at all, so nothing can stand in for the member it left unset.
#[tokio::test]
async fn two_references_never_read_the_same_refusal() {
    // `ImageTag` is `@length(1..300)` (`smithy_query(query="shape", shape="ImageTag")`), so
    // a one-character tag and a 300-character one are both well formed and reach the lookup.
    let longest_tag = "t".repeat(300);
    let repository = "empty-reference-set";

    let router = emulator();
    given_empty_repository(&router, "the repository holding nothing", repository).await;

    let mut seen: Vec<(&str, String)> = Vec::new();
    for row in references(&longest_tag) {
        let expected = image_not_found_sentence(&row.digest, &row.tag, repository);
        let body = refused(
            &router,
            row.case,
            "DescribeImages",
            &json!({"repositoryName": repository, "imageIds": [row.id.clone()]}),
            IMAGE_NOT_FOUND,
        )
        .await;
        let message = at(&body, "/message")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            message, expected,
            "case {:?}: expected the refusal to read {expected:?}, got {message:?}",
            row.case
        );
        // A request carrying no digest gets no digest back: the sentence mentions the
        // algorithm prefix only where the caller's own entry carried one.
        if row.id.get("imageDigest").is_none() {
            assert!(
                !message.contains("sha256:"),
                "case {:?}: a lookup carrying no digest must state none, got {message:?}",
                row.case
            );
        }
        seen.push((row.case, message));
    }

    for (index, (case, message)) in seen.iter().enumerate() {
        for (other_case, other) in seen.iter().skip(index + 1) {
            assert_ne!(
                message, other,
                "cases {case:?} and {other_case:?}: two different references must not read \
                 the same refusal, both read {message:?}"
            );
        }
    }
}

/// WHICH refusal an empty repository earns. The repository resolves — a read naming no
/// identifier succeeds and reports no image — so a lookup in it is refused about the image;
/// the same lookup against a repository this registry does not hold is refused about the
/// repository, and that sentence names no identifier at all.
#[tokio::test]
async fn an_empty_repository_resolves_so_the_refusal_is_about_the_image() {
    let router = emulator();
    let held = "empty-but-present";
    let absent = "never-created";
    given_empty_repository(&router, "the repository holding nothing", held).await;

    // The positive control: the repository is there, and it holds nothing.
    let control = "a read naming no identifier";
    let listed = ok(
        &router,
        control,
        "DescribeImages",
        &json!({"repositoryName": held}),
    )
    .await;
    assert_eq!(
        at(&listed, "/imageDetails"),
        json!([]),
        "case {control:?}: expected no image details, got {listed:?}"
    );

    // (case, the repository named, the entry sent, the code, the sentence)
    let rows: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            "a tag lookup in the repository that is there",
            held,
            json!({"imageTag": "latest"}),
            IMAGE_NOT_FOUND,
            image_not_found_sentence(UNSET_MEMBER, "latest", held),
        ),
        (
            "a digest lookup in the repository that is there",
            held,
            json!({"imageDigest": UNSTORED_DIGEST}),
            IMAGE_NOT_FOUND,
            image_not_found_sentence(UNSTORED_DIGEST, UNSET_MEMBER, held),
        ),
        (
            "a tag lookup in a repository that is not",
            absent,
            json!({"imageTag": "latest"}),
            REPOSITORY_NOT_FOUND,
            repository_not_found_sentence(absent),
        ),
        (
            "a digest lookup in a repository that is not",
            absent,
            json!({"imageDigest": UNSTORED_DIGEST}),
            REPOSITORY_NOT_FOUND,
            repository_not_found_sentence(absent),
        ),
    ];

    for (case, repository, id, code, expected) in rows {
        let answer = call(
            &router,
            "DescribeImages",
            &json!({"repositoryName": repository, "imageIds": [id]}),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            code,
            "case {case:?}: expected {code}, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: expected the refusal to read {expected:?}, got {:?}",
            answer.message()
        );
        if code == REPOSITORY_NOT_FOUND {
            assert!(
                !answer.message().contains("imageId"),
                "case {case:?}: a refusal about the repository names no identifier, got {:?}",
                answer.message()
            );
        }
    }
}
