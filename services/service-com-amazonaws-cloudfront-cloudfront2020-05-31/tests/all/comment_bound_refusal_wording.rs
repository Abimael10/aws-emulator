// SPDX-License-Identifier: CC0-1.0

//! The sentence a `Comment` longer than its member admits is refused with, across the surfaces
//! of this service that carry one, end to end over HTTP.
//!
//! RULE: every CloudFront configuration that carries a `Comment` bounds it at the documented 128
//! characters, and a longer one is refused `InvalidArgument` (HTTP 400) naming the PARAMETER the
//! value arrived in — "The parameter Comment is too big." — fixed text that echoes neither the
//! bound nor the length submitted, so one comment past the bound and another far past it read
//! alike and two surfaces cannot word the same condition differently. A comment AT the bound is
//! stored.
//!
//! The rows span what the rule generalises over: the SURFACE the comment arrived on (a public key
//! create, a public key update — the call whose "only value you can change is the comment" — an
//! origin access identity create, a field-level encryption profile create, and a key group
//! create), and the comment itself (exactly at the bound, one character past it, far past it, and
//! a multi-byte comment one character past it, since the bound counts characters rather than
//! bytes).
//!
//! The sentence is a LITERAL here rather than the crate's own helper, so a wrong constant cannot
//! make this suite pass; the per-surface suites that read it off the service's refusal keep
//! asserting that every surface serves the same one.
//!
//! The harness (the router, the request helpers and the configuration documents) belongs to the
//! lifecycle suites of each surface, reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MAX_KEY_GROUP_COMMENT;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::field_level_encryption_profile_lifecycle::{create_profile, upload_key};
use crate::key_group_lifecycle::{create_group, mint_public_keys, Group};
use crate::origin_access_identity_lifecycle::create_identity;
use crate::public_key_lifecycle::{create_key, pem};
use crate::response_headers_policy_lifecycle::escape;

/// The sentence CloudFront refuses an over-long `Comment` with, whatever surface carried it.
const COMMENT_TOO_BIG: &str = "The parameter Comment is too big.";

/// The surfaces a comment arrives on.
#[derive(Clone, Copy, Debug)]
enum Surface {
    /// `CreatePublicKey`.
    PublicKeyCreate,
    /// `UpdatePublicKey`, whose "only value you can change is the comment".
    PublicKeyUpdate,
    /// `CreateCloudFrontOriginAccessIdentity`.
    OriginAccessIdentity,
    /// `CreateFieldLevelEncryptionProfile`.
    EncryptionProfile,
    /// `CreateKeyGroup`.
    KeyGroup,
}

/// One comment under test.
struct Comment {
    case: &'static str,
    text: String,
    /// Whether the configuration carrying it is stored.
    stored: bool,
}

/// The comments every surface is driven over: the published bound, one character past it, far
/// past it, and a multi-byte comment one character past it.
fn comments() -> Vec<Comment> {
    vec![
        Comment {
            case: "a comment at the documented bound",
            text: "c".repeat(MAX_KEY_GROUP_COMMENT),
            stored: true,
        },
        Comment {
            case: "a comment one character past the documented bound",
            text: "c".repeat(MAX_KEY_GROUP_COMMENT + 1),
            stored: false,
        },
        Comment {
            case: "a comment far past the documented bound",
            text: "c".repeat(MAX_KEY_GROUP_COMMENT * 4),
            stored: false,
        },
        Comment {
            // The bound counts CHARACTERS, so a multi-byte comment one character past it is
            // past it too.
            case: "a multi-byte comment one character past the documented bound",
            text: "é".repeat(MAX_KEY_GROUP_COMMENT + 1),
            stored: false,
        },
    ]
}

/// `PUT /2020-05-31/public-key/{Id}/config` with `body` under the version token `if_match`.
async fn update_key(
    router: &MultiServiceRouter,
    id: &str,
    body: &str,
    if_match: &str,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/public-key/{id}/config"),
        Some(body),
        Some(if_match),
    )
    .await
}

/// The `PublicKeyConfig` document a public key write submits, carrying `comment`.
fn public_key_xml(reference: &str, comment: &str) -> String {
    format!(
        "<PublicKeyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Name>{reference}</Name>\
         <EncodedKey>{}</EncodedKey>\
         <Comment>{}</Comment></PublicKeyConfig>",
        escape(&pem(1, 'C')),
        escape(comment)
    )
}

/// Submit `comment` on `surface` and report what the service answered.
async fn submit(
    router: &MultiServiceRouter,
    surface: Surface,
    index: usize,
    comment: &str,
) -> (Answer, u16) {
    let slug = format!("comment-bound-{index}");
    match surface {
        Surface::PublicKeyCreate => (create_key(router, &public_key_xml(&slug, comment)).await, 201),
        Surface::PublicKeyUpdate => {
            // The key the update addresses is created with a comment every bound admits, so what
            // the row drives is the UPDATE's payload.
            let created = create_key(router, &public_key_xml(&slug, "within every bound")).await;
            assert_eq!(
                created.status, 201,
                "case {slug}: expected HTTP 201 from CreatePublicKey, got {} with {}",
                created.status, created.raw
            );
            let id = created.root().text_at("Id").unwrap_or_default().to_string();
            let etag = created.header("etag").to_string();
            (
                update_key(router, &id, &public_key_xml(&slug, comment), &etag).await,
                200,
            )
        }
        Surface::OriginAccessIdentity => (
            create_identity(
                router,
                &format!(
                    "<CloudFrontOriginAccessIdentityConfig \
                     xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                     <CallerReference>{slug}</CallerReference>\
                     <Comment>{}</Comment></CloudFrontOriginAccessIdentityConfig>",
                    escape(comment)
                ),
            )
            .await,
            201,
        ),
        Surface::EncryptionProfile => {
            let public_key_id = upload_key(router, 900 + index).await;
            (
                create_profile(
                    router,
                    &format!(
                        "<FieldLevelEncryptionProfileConfig \
                         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                         <Name>{slug}</Name>\
                         <CallerReference>{slug}</CallerReference>\
                         <Comment>{}</Comment>\
                         <EncryptionEntities><Quantity>1</Quantity><Items><EncryptionEntity>\
                         <PublicKeyId>{public_key_id}</PublicKeyId>\
                         <ProviderId>{slug}-provider</ProviderId>\
                         <FieldPatterns><Quantity>1</Quantity><Items><FieldPattern>secret-*\
                         </FieldPattern></Items></FieldPatterns>\
                         </EncryptionEntity></Items></EncryptionEntities>\
                         </FieldLevelEncryptionProfileConfig>",
                        escape(comment)
                    ),
                )
                .await,
                201,
            )
        }
        Surface::KeyGroup => {
            let keys = mint_public_keys(router, &slug, 1).await;
            let group = Group {
                case: slug.clone(),
                name: slug.clone(),
                items: keys,
                comment: Some(comment.to_string()),
            };
            (create_group(router, &group.xml()).await, 201)
        }
    }
}

/// One sentence for every surface: a comment past the documented bound is refused with it, and a
/// comment at the bound is stored.
#[tokio::test]
async fn an_over_long_comment_is_refused_with_one_sentence_on_every_surface() {
    let router = emulator();
    let surfaces = [
        ("CreatePublicKey", Surface::PublicKeyCreate),
        ("UpdatePublicKey", Surface::PublicKeyUpdate),
        (
            "CreateCloudFrontOriginAccessIdentity",
            Surface::OriginAccessIdentity,
        ),
        (
            "CreateFieldLevelEncryptionProfile",
            Surface::EncryptionProfile,
        ),
        ("CreateKeyGroup", Surface::KeyGroup),
    ];

    let mut index = 0;
    for (name, surface) in surfaces {
        for comment in comments() {
            index += 1;
            let case = format!("{name} with {}", comment.case);
            let (answer, served) = submit(&router, surface, index, &comment.text).await;
            if comment.stored {
                assert_eq!(
                    answer.status, served,
                    "case {case}: expected HTTP {served}, got {} with {}",
                    answer.status, answer.raw
                );
                continue;
            }
            assert_eq!(
                answer.status, 400,
                "case {case}: expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgument",
                "case {case}: expected InvalidArgument, got {:?} with {}",
                answer.error_code(),
                answer.raw
            );
            let message = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                message, COMMENT_TOO_BIG,
                "case {case}: expected the sentence {COMMENT_TOO_BIG:?}, got {message:?} in {}",
                answer.raw
            );
        }
    }
}
