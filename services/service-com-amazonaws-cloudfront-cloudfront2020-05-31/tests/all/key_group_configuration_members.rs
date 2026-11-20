// SPDX-License-Identifier: CC0-1.0

//! What a key group CONFIGURATION's own members have to be, end to end over HTTP.
//!
//! RULE: a key group is stored only when every member of the submitted `KeyGroupConfig` is
//! inside the domain CloudFront admits for that member, and a configuration outside it is
//! refused `InvalidArgument` (HTTP 400) naming the MEMBER the caller has to fix, in CloudFront's
//! own fixed wording — nothing of the submitted value, of the bound or of the length is echoed,
//! so every value outside one member's domain reads alike:
//!
//! * `Name` identifies the group, so it is bounded in length and built from the characters
//!   CloudFront admits in a name (ASCII letters, digits, `-` and `_`); a longer one is "too
//!   big" and one carrying anything else "contains illegal characters";
//! * `Comment` is bounded at the documented 128 characters, past which it is "too big";
//! * `Items` is the list of public key ids the group is made of, so a configuration naming
//!   none is an invalid `publicKeyIdsList`.
//!
//! Each rule is about the CONFIGURATION rather than about the call carrying one, so the create
//! and the update — the two calls that submit a whole configuration — answer the same sentence,
//! and a refused call stores nothing: the group the update addressed still reads back as it
//! was, and the name a refused create named is still free.
//!
//! The rows span what the rules generalise over: each bound one character inside it, exactly on
//! it and far past it; the character rule over one illegal character of each kind (a space,
//! punctuation, a path separator, a symbol and a non-ASCII letter) and over a legal name using
//! every admitted character class; and the empty key list. The sentences are LITERALS here
//! rather than the crate's constants, so a wrong constant cannot make this suite pass.
//!
//! The harness (the router, the request helpers, the key group document and the minted public
//! keys) is the key group lifecycle and distribution lifecycle suites', reused rather than
//! copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MAX_KEY_GROUP_COMMENT, MAX_KEY_GROUP_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, is_version_token, Answer, API_VERSION};
use crate::key_group_lifecycle::{create_group, get_group, mint_public_keys, Group};

// ---------------------------------------------------------------------------
// Calling the surface under test
// ---------------------------------------------------------------------------

/// `PUT /2020-05-31/key-group/{Id}` with the `KeyGroupConfig` document `body` under the
/// version token `if_match`.
async fn update_group(router: &MultiServiceRouter, id: &str, body: &str, if_match: &str) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/key-group/{id}"),
        Some(body),
        Some(if_match),
    )
    .await
}

// ---------------------------------------------------------------------------
// The configurations under test
// ---------------------------------------------------------------------------

/// What the service answers a row with: the configuration is stored, or it is refused with this
/// sentence under `InvalidArgument`.
enum Expect {
    /// Stored: the call is served and the record reads back as submitted.
    Stored,
    /// Refused HTTP 400 `InvalidArgument` with this exact sentence.
    Refused(&'static str),
}

/// One row: the member it varies, the `Name` and `Comment` it submits, whether it names any
/// public key, and what the service answers.
struct Row {
    case: &'static str,
    /// The `Name` the configuration submits, built from `index` so no two accepted rows collide.
    name: fn(usize) -> String,
    /// The `Comment` the configuration submits.
    comment: fn() -> Option<String>,
    /// Whether the configuration names the account's public key (`false` submits no `Items`).
    names_a_key: bool,
    expect: Expect,
}

/// The sentence CloudFront answers a name longer than it admits with.
const NAME_TOO_BIG: &str = "The parameter KeyGroupName is too big.";

/// The sentence CloudFront answers a name carrying a character it does not admit with.
const NAME_ILLEGAL_CHARACTERS: &str = "The parameter Name contains illegal characters.";

/// The sentence CloudFront answers a comment longer than the member admits with.
const COMMENT_TOO_BIG: &str = "The parameter Comment is too big.";

/// The sentence CloudFront answers a key list that names no public key with.
const PUBLIC_KEY_ID_LIST_INVALID: &str = "The parameter publicKeyIdsList is invalid.";

/// A name of exactly `length` characters, unique for `index`: a legal stem followed by padding.
fn name_of_length(index: usize, length: usize) -> String {
    let stem = format!("kg-param-{index}-");
    let mut name = stem.clone();
    if length > stem.chars().count() {
        name.push_str(&"n".repeat(length - stem.chars().count()));
    } else {
        name = stem.chars().take(length).collect();
    }
    name
}

/// The rows every assertion below runs over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a name one character inside the bound",
            name: |index| name_of_length(index, MAX_KEY_GROUP_NAME - 1),
            comment: || None,
            names_a_key: true,
            expect: Expect::Stored,
        },
        Row {
            case: "a name exactly at the bound",
            name: |index| name_of_length(index, MAX_KEY_GROUP_NAME),
            comment: || None,
            names_a_key: true,
            expect: Expect::Stored,
        },
        Row {
            case: "a name one character past the bound",
            name: |index| name_of_length(index, MAX_KEY_GROUP_NAME + 1),
            comment: || None,
            names_a_key: true,
            expect: Expect::Refused(NAME_TOO_BIG),
        },
        Row {
            case: "a name far past the bound",
            name: |index| name_of_length(index, MAX_KEY_GROUP_NAME * 4),
            comment: || None,
            names_a_key: true,
            expect: Expect::Refused(NAME_TOO_BIG),
        },
        Row {
            case: "a name using every admitted character class",
            name: |index| format!("kg_param-{index}-Ab09_-Z"),
            comment: || None,
            names_a_key: true,
            expect: Expect::Stored,
        },
        Row {
            case: "a name carrying a space",
            name: |index| format!("kg param {index}"),
            comment: || None,
            names_a_key: true,
            expect: Expect::Refused(NAME_ILLEGAL_CHARACTERS),
        },
        Row {
            case: "a name carrying punctuation",
            name: |index| format!("kg.param.{index}"),
            comment: || None,
            names_a_key: true,
            expect: Expect::Refused(NAME_ILLEGAL_CHARACTERS),
        },
        Row {
            case: "a name carrying a path separator",
            name: |index| format!("kg/param/{index}"),
            comment: || None,
            names_a_key: true,
            expect: Expect::Refused(NAME_ILLEGAL_CHARACTERS),
        },
        Row {
            case: "a name carrying a symbol",
            name: |index| format!("kg+param!{index}"),
            comment: || None,
            names_a_key: true,
            expect: Expect::Refused(NAME_ILLEGAL_CHARACTERS),
        },
        Row {
            case: "a name carrying a non-ASCII letter",
            name: |index| format!("kg-param-é-{index}"),
            comment: || None,
            names_a_key: true,
            expect: Expect::Refused(NAME_ILLEGAL_CHARACTERS),
        },
        Row {
            case: "a comment exactly at the documented bound",
            name: |index| format!("kg-param-comment-bound-{index}"),
            comment: || Some("c".repeat(MAX_KEY_GROUP_COMMENT)),
            names_a_key: true,
            expect: Expect::Stored,
        },
        Row {
            case: "a comment one character past the documented bound",
            name: |index| format!("kg-param-comment-past-{index}"),
            comment: || Some("c".repeat(MAX_KEY_GROUP_COMMENT + 1)),
            names_a_key: true,
            expect: Expect::Refused(COMMENT_TOO_BIG),
        },
        Row {
            case: "a comment far past the documented bound",
            name: |index| format!("kg-param-comment-far-{index}"),
            comment: || Some("c".repeat(MAX_KEY_GROUP_COMMENT * 4)),
            names_a_key: true,
            expect: Expect::Refused(COMMENT_TOO_BIG),
        },
        Row {
            case: "a key list naming no public key",
            name: |index| format!("kg-param-no-keys-{index}"),
            comment: || None,
            names_a_key: false,
            expect: Expect::Refused(PUBLIC_KEY_ID_LIST_INVALID),
        },
        Row {
            case: "a key list naming no public key beside a comment",
            name: |index| format!("kg-param-no-keys-commented-{index}"),
            comment: || Some("a group that names nothing".to_string()),
            names_a_key: false,
            expect: Expect::Refused(PUBLIC_KEY_ID_LIST_INVALID),
        },
    ]
}

/// The configuration a row submits, over the account's public `key`.
fn configuration(row: &Row, index: usize, key: &str) -> Group {
    Group {
        case: row.case.to_string(),
        name: (row.name)(index),
        items: if row.names_a_key {
            vec![key.to_string()]
        } else {
            Vec::new()
        },
        comment: (row.comment)(),
    }
}

/// Assert that `answer` is the HTTP 400 `InvalidArgument` carrying `sentence`.
fn assert_refused(answer: &Answer, sentence: &str, case: &str, context: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case} ({context}): expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidArgument",
        "case {case} ({context}): expected InvalidArgument, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, sentence,
        "case {case} ({context}): expected the sentence {sentence:?}, got {message:?} in {}",
        answer.raw
    );
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// The create stores a configuration every member's domain admits and refuses one outside any
/// of them, naming the member; a refused create stores nothing, so the name it named is still
/// free for a configuration that is admitted.
#[tokio::test]
async fn a_create_admits_a_configuration_only_inside_every_members_domain() {
    let router = emulator();
    let keys = mint_public_keys(&router, "kg-param-create", 1).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let submitted = configuration(row, index, &keys[0]);
        let answer = create_group(&router, &submitted.xml()).await;
        match row.expect {
            Expect::Stored => {
                assert_eq!(
                    answer.status, 201,
                    "case {case}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
                    answer.status, answer.raw
                );
                let id = answer.root().text_at("Id").unwrap_or_default().to_string();
                let etag = answer.header("etag").to_string();
                assert!(
                    is_version_token(&etag),
                    "case {case}: expected a minted version token, got {etag:?}"
                );
                let read = get_group(&router, &id).await;
                assert_eq!(
                    read.status, 200,
                    "case {case}: expected the stored group to be readable, got {} with {}",
                    read.status, read.raw
                );
                let config = read.root().at("KeyGroupConfig").unwrap_or_else(|| {
                    panic!("case {case}: no KeyGroupConfig in {}", read.raw)
                });
                submitted.assert_matches(config, "GetKeyGroup after the create", &read.raw);
            }
            Expect::Refused(sentence) => {
                assert_refused(&answer, sentence, case, "the create");
                // The refusal stored nothing: a configuration every domain admits, named for
                // this row, is created instead.
                let repaired = Group {
                    case: format!("{case} repaired"),
                    name: format!("kg-param-repaired-{index}"),
                    items: keys[..1].to_vec(),
                    comment: None,
                };
                let accepted = create_group(&router, &repaired.xml()).await;
                assert_eq!(
                    accepted.status, 201,
                    "case {case}: expected the repaired configuration to be created, got {} \
                     with {}",
                    accepted.status, accepted.raw
                );
            }
        }
    }
}

/// The update makes the same judgement, member for member and sentence for sentence, and a
/// refused update leaves the group it addressed exactly as it was — the configuration it holds
/// and the version token an `If-Match` against it is satisfied by.
#[tokio::test]
async fn an_update_admits_a_configuration_only_inside_every_members_domain() {
    let router = emulator();
    let keys = mint_public_keys(&router, "kg-param-update", 1).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        // Each row updates a group of its own, created from a configuration every domain
        // admits, so what the row drives is the UPDATE's payload.
        let held = Group {
            case: format!("{case} (before)"),
            name: format!("kg-param-held-{index}"),
            items: keys[..1].to_vec(),
            comment: Some("the configuration the create stored".to_string()),
        };
        let created = create_group(&router, &held.xml()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let etag = created.header("etag").to_string();

        let submitted = configuration(row, 1_000 + index, &keys[0]);
        let answer = update_group(&router, &id, &submitted.xml(), &etag).await;
        match row.expect {
            Expect::Stored => {
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected HTTP 200 from UpdateKeyGroup, got {} with {}",
                    answer.status, answer.raw
                );
                let read = get_group(&router, &id).await;
                let config = read.root().at("KeyGroupConfig").unwrap_or_else(|| {
                    panic!("case {case}: no KeyGroupConfig in {}", read.raw)
                });
                submitted.assert_matches(config, "GetKeyGroup after the update", &read.raw);
            }
            Expect::Refused(sentence) => {
                assert_refused(&answer, sentence, case, "the update");
                // Nothing was written: the group still holds what the create stored, at the
                // version the create handed back.
                let read = get_group(&router, &id).await;
                assert_eq!(
                    read.header("etag"),
                    etag,
                    "case {case}: expected the refused update to leave the version {etag:?}, \
                     got {:?}",
                    read.header("etag")
                );
                let config = read.root().at("KeyGroupConfig").unwrap_or_else(|| {
                    panic!("case {case}: no KeyGroupConfig in {}", read.raw)
                });
                held.assert_matches(config, "GetKeyGroup after a refused update", &read.raw);
            }
        }
    }
}
