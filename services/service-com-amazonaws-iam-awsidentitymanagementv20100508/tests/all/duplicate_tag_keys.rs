// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the refusal a tag list naming one key twice answers with, end to end over
//! the awsQuery wire.
//!
//! RULE: the sentence IAM states for that class is fixed text and belongs to the OPERATION
//! the list was supplied to — "Duplicate tag keys found. Please note that Tag keys are case
//! insensitive." for a principal's tags (a user's or a role's), "Duplicate tag keys found."
//! for any other IAM resource's — and it quotes nothing of the request back.
//! `tests/all/duplicate_tag_key_wording_by_operation.rs` tables that rule over both halves;
//! the rows here pin the sentence each of these four operations answers with.
//!
//! WHY a suite of its own: `tests/all/supplied_tag_lists.rs` states the RULE (which lists are
//! refused, with which code, across every operation that takes one) and compares the message
//! against the crate's own constant, which cannot tell a wrong sentence from a right one.
//! Here the expectation is the LITERAL text AWS answers with, written out, so a change to
//! that constant has to be deliberate. The rows differ in which operation carries the list
//! and where in it the repetition sits, so no single request is the specification.
//!
//! The awsQuery harness (`call`, `element`, `error_code`, `param`, `borrowed`) is the one
//! `tests/all/user_lifecycle.rs` and `tests/all/role_lifecycle.rs` own and every suite in
//! this crate shares; the tag-parameter builder is `tests/all/role_tags.rs`'s.

use crate::role_lifecycle::{borrowed, param};
use crate::role_tags::{tags, with_tag_params, TRUST_POLICY};
use crate::user_lifecycle::{call, element, error_code, router};

/// The sentence AWS answers a tag list naming one key twice with when the list was supplied
/// for a PRINCIPAL's tags, written out rather than read from the service's own constant.
const PRINCIPAL_DUPLICATE_TAG_KEYS_SENTENCE: &str =
    "Duplicate tag keys found. Please note that Tag keys are case insensitive.";

/// The sentence it answers the same list with when the list was supplied for any other IAM
/// resource's tags.
const RESOURCE_DUPLICATE_TAG_KEYS_SENTENCE: &str = "Duplicate tag keys found.";

/// A managed policy document, for the row that creates a policy.
const POLICY_DOCUMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"sts:GetCallerIdentity","Resource":"*"}]}"#;

/// One row: the operation that carries the tag list, the parameters addressing its
/// resource, and the list whose repeated key the refusal is read from.
struct WordingCase {
    action: &'static str,
    addressing: Vec<(String, String)>,
    list: Vec<(String, String)>,
    /// The sentence this operation words the class with.
    expected: &'static str,
}

/// RULE: every operation taking a `Tags` list answers the repeated key with the sentence it
/// words that class with, under `InvalidInput` (HTTP 400), and writes nothing.
#[tokio::test]
async fn the_repeated_key_refusal_carries_iam_s_sentence_verbatim() {
    let router = router();

    let rows = vec![
        WordingCase {
            action: "TagUser",
            addressing: vec![param("UserName", "wording-user")],
            list: tags(&[("Team", "a"), ("Team", "b")]),
            expected: PRINCIPAL_DUPLICATE_TAG_KEYS_SENTENCE,
        },
        WordingCase {
            action: "TagRole",
            addressing: vec![param("RoleName", "wording-role")],
            list: tags(&[("env", "prod"), ("owner", "iam"), ("env", "dev")]),
            expected: PRINCIPAL_DUPLICATE_TAG_KEYS_SENTENCE,
        },
        WordingCase {
            action: "CreatePolicy",
            addressing: vec![
                param("PolicyName", "wording-policy"),
                param("PolicyDocument", POLICY_DOCUMENT),
            ],
            list: tags(&[("a", "1"), ("b", "2"), ("b", "3"), ("c", "4")]),
            expected: RESOURCE_DUPLICATE_TAG_KEYS_SENTENCE,
        },
        WordingCase {
            action: "CreateUser",
            addressing: vec![param("UserName", "wording-created-user")],
            list: tags(&[("thrice", "1"), ("thrice", "2"), ("thrice", "3")]),
            expected: PRINCIPAL_DUPLICATE_TAG_KEYS_SENTENCE,
        },
    ];

    // The two `Tag*` rows address resources the account has to hold first.
    let (status, body) = call(&router, "CreateUser", &[("UserName", "wording-user")]).await;
    assert_eq!(status, 200, "the fixture user must be created, got {body}");
    let (status, body) = call(
        &router,
        "CreateRole",
        &[
            ("RoleName", "wording-role"),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(status, 200, "the fixture role must be created, got {body}");

    for WordingCase {
        action,
        addressing,
        list,
        expected,
    } in &rows
    {
        let params = with_tag_params(addressing.clone(), list);
        let (status, body) = call(&router, action, &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {action:?}: a list naming one key twice must answer 400, got {status}: \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {action:?}: expected the modelled awsQuery code, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            *expected,
            "case {action:?}: expected IAM's sentence {expected:?}, got {:?}",
            element(&body, "Message")
        );
    }

    // The resource the refused create named was not created, and the held user kept no tag
    // from the refused tagging write.
    let (status, body) = call(&router, "GetUser", &[("UserName", "wording-created-user")]).await;
    assert_eq!(
        status, 404,
        "a refused create must create nothing, got {status}: {body}"
    );
    let (status, body) = call(&router, "ListUserTags", &[("UserName", "wording-user")]).await;
    assert_eq!(status, 200, "ListUserTags must be served, got {status}: {body}");
    assert!(
        !body.contains("<Key>"),
        "a refused tagging write must store nothing, got {body}"
    );
}
