// SPDX-License-Identifier: CC0-1.0

//! What an operation ADDRESSED by a distribution identifier reads of the submitted value
//! BEYOND the alphabet an identifier is written in, end to end over HTTP: nothing at all.
//!
//! The rule under test: a distribution's path identifier carries no format constraint in the
//! model — `smithy_query(query="operation",operation="GetDistribution,GetDistributionConfig,\
//! DeleteDistribution,CreateInvalidation,ListInvalidations,GetInvalidation,\
//! GetMonitoringSubscription")` reports each one's `Id`/`DistributionId` as a required
//! `@httpLabel` on a plain `string`, with no `@pattern` and no `@length`, and
//! `NoSuchDistribution` (404) among its errors — so no route may refuse a value WRITTEN as an
//! identifier of CloudFront's mint on its width, on the namespace letter it opens with or on
//! which other kind of resource holds it: WHICH identifier was ever issued, and to what, is a
//! property of the STORE alone, and every such value this account holds no distribution under
//! is the distribution reported ABSENT, under this error class's absent-resource sentence "The
//! specified distribution does not exist." The identifier is never echoed back.
//!
//! The one form these routes DO read is the alphabet itself; a value written outside it is the
//! identifier a route reports it cannot read, which the suite
//! `distribution_identifier_reads_its_alphabet` owns — and which also carries the positive
//! control for every route driven here.
//!
//! The rows span what the rule generalises over: bodies narrower, wider and far wider than the
//! one this emulator mints, the live identifiers of two OTHER resource kinds (one of the form a
//! distribution shares with three other kinds, one of a namespace of its own), a namespace
//! letter no kind of this service mints, a body of digits alone, and the live distribution's own
//! identifier with its last character changed. Every row is driven through all seven operations
//! addressed by a distribution identifier.
//!
//! The wire harness and the create helpers are the crate's own.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::NO_SUCH_DISTRIBUTION_MESSAGE;

use crate::distribution_lifecycle::{
    batch_xml, call, config, create_distribution, emulator, Answer, API_VERSION,
};
use crate::origin_access_control_lifecycle::{create_control, table};
use crate::public_key_lifecycle::{create_key, pem, Key};

/// The code every route below answers an identifier it cannot resolve.
pub const NO_SUCH_DISTRIBUTION: &str = "NoSuchDistribution";

/// An invalidation id the rows that need one carry. Its value decides nothing: the
/// distribution the path names is resolved first, and when it is absent nothing reads this.
const SOME_INVALIDATION_ID: &str = "I2J0DX6BA7462Q";

/// The caller reference the invalidation batch every mutation row submits carries. The batch
/// is well formed, so what the call is answered with is decided by the identifier in its path
/// and nothing else.
const BATCH_REFERENCE: &str = "no-id-form-batch";

/// One operation addressed by a distribution identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    GetDistribution,
    GetDistributionConfig,
    DeleteDistribution,
    CreateInvalidation,
    ListInvalidations,
    GetInvalidation,
    GetMonitoringSubscription,
}

/// Every operation this suite drives: four of the distribution's own routes and three of the
/// routes addressed by the distribution a resource is held under.
pub const OPS: &[Op] = &[
    Op::GetDistribution,
    Op::GetDistributionConfig,
    Op::DeleteDistribution,
    Op::CreateInvalidation,
    Op::ListInvalidations,
    Op::GetInvalidation,
    Op::GetMonitoringSubscription,
];

impl Op {
    /// Address `id` through this operation. No `If-Match` is sent, so a conditional route is
    /// never refused for a precondition, and the invalidation batch is well formed, so the
    /// mutation is never refused for its payload: what each call is answered with is decided by
    /// the identifier in its path and nothing else.
    pub async fn send(self, router: &MultiServiceRouter, id: &str) -> Answer {
        match self {
            Self::GetDistribution => {
                call(router, "GET", &format!("/{API_VERSION}/distribution/{id}"), None, None).await
            }
            Self::GetDistributionConfig => {
                call(
                    router,
                    "GET",
                    &format!("/{API_VERSION}/distribution/{id}/config"),
                    None,
                    None,
                )
                .await
            }
            Self::DeleteDistribution => {
                call(
                    router,
                    "DELETE",
                    &format!("/{API_VERSION}/distribution/{id}"),
                    None,
                    None,
                )
                .await
            }
            Self::CreateInvalidation => {
                call(
                    router,
                    "POST",
                    &format!("/{API_VERSION}/distribution/{id}/invalidation"),
                    Some(&batch_xml(BATCH_REFERENCE, &["/*"])),
                    None,
                )
                .await
            }
            Self::ListInvalidations => {
                call(
                    router,
                    "GET",
                    &format!("/{API_VERSION}/distribution/{id}/invalidation"),
                    None,
                    None,
                )
                .await
            }
            Self::GetInvalidation => {
                call(
                    router,
                    "GET",
                    &format!("/{API_VERSION}/distribution/{id}/invalidation/{SOME_INVALIDATION_ID}"),
                    None,
                    None,
                )
                .await
            }
            Self::GetMonitoringSubscription => {
                call(
                    router,
                    "GET",
                    &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
                    None,
                    None,
                )
                .await
            }
        }
    }

    /// The model's name for the member THIS operation carries the distribution's identifier
    /// in: the distribution's own routes address it by `Id`
    /// (`GetDistribution$Id`), and the routes addressed by the distribution a resource is held
    /// under carry it in `DistributionId` (`CreateInvalidation$DistributionId`). It is the one
    /// variable part of the sentence an identifier that cannot be READ earns, which the
    /// sibling suite asserts.
    pub fn member(self) -> &'static str {
        match self {
            Self::GetDistribution | Self::GetDistributionConfig | Self::DeleteDistribution => "Id",
            Self::CreateInvalidation
            | Self::ListInvalidations
            | Self::GetInvalidation
            | Self::GetMonitoringSubscription => "DistributionId",
        }
    }
}

/// The live resources these rows address and name.
pub struct Live {
    pub distribution_id: String,
    pub control_id: String,
    pub public_key_id: String,
}

/// Stand up one distribution — what the positive control addresses — plus one origin access
/// control and one public key, whose really-minted identifiers two of the rows submit in a
/// distribution identifier's place.
pub async fn live(router: &MultiServiceRouter, reference: &str) -> Live {
    let created = create_distribution(
        router,
        &config("the distribution these rows address", reference).xml(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case creating the distribution: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );

    let control = table()[0].with_name("the control these rows name", &format!("{reference}-oac"));
    let control_created = create_control(router, &control.xml()).await;
    assert_eq!(
        control_created.status, 201,
        "case creating the control: expected HTTP 201, got {} with {}",
        control_created.status, control_created.raw
    );

    let key_created = create_key(
        router,
        &Key {
            case: "the public key these rows name".to_string(),
            caller_reference: format!("{reference}-key"),
            name: format!("{reference}-key"),
            encoded_key: pem(1, 'A'),
            comment: None,
        }
        .xml(),
    )
    .await;
    assert_eq!(
        key_created.status, 201,
        "case creating the public key: expected HTTP 201, got {} with {}",
        key_created.status, key_created.raw
    );

    Live {
        distribution_id: created.root().text_at("Id").unwrap_or_default().to_string(),
        control_id: control_created
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string(),
        public_key_id: key_created
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string(),
    }
}

/// Every value this suite submits where a distribution identifier belongs: each one WRITTEN as
/// an identifier of CloudFront's mint and held by no distribution of this account, paired with
/// the case that says how it came to name none.
fn unresolvable(live: &Live) -> Vec<(String, String)> {
    let held = &live.distribution_id;
    let body = &held[1..];
    let mut short = held.clone();
    short.pop();
    let mut changed: Vec<char> = held.chars().collect();
    if let Some(last) = changed.last_mut() {
        *last = if *last == 'A' { 'B' } else { 'A' };
    }

    vec![
        (
            "an identifier of the minted width that was never issued".to_string(),
            format!("E{}", "0".repeat(body.len())),
        ),
        (
            "the distribution's own identifier with its last character changed".to_string(),
            changed.into_iter().collect(),
        ),
        ("a body one character short".to_string(), short),
        (
            "a body one character wider than this emulator mints".to_string(),
            format!("{held}Z"),
        ),
        (
            "a far wider body than this emulator mints".to_string(),
            format!("E{body}{body}"),
        ),
        (
            "a live origin access control's own identifier".to_string(),
            live.control_id.clone(),
        ),
        (
            "a live public key's own identifier, from a namespace of its own".to_string(),
            live.public_key_id.clone(),
        ),
        (
            "a namespace letter no kind of this service mints".to_string(),
            format!("W{body}"),
        ),
        (
            "a body of digits alone".to_string(),
            "0123456789012".to_string(),
        ),
    ]
}

/// RULE: every operation addressed by a distribution identifier resolves a value written as an
/// identifier of CloudFront's mint against the store and reports the distribution ABSENT — one
/// status, one code and one fixed sentence — for every identifier it holds no distribution
/// under, whatever the value's width, its namespace letter or which other kind of resource this
/// account holds it as.
#[tokio::test]
async fn an_identifier_no_distribution_answers_to_is_the_absent_distribution() {
    let router = emulator();
    let live = live(&router, "no-id-form").await;

    for op in OPS {
        for (case, id) in unresolvable(&live) {
            let case = format!("{op:?} with {case} ({id:?})");
            let answer = op.send(&router, &id).await;
            assert_eq!(
                answer.status, 404,
                "case {case}: expected HTTP 404, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                NO_SUCH_DISTRIBUTION,
                "case {case}: expected code {NO_SUCH_DISTRIBUTION}, got {:?} with {}",
                answer.error_code(),
                answer.raw
            );
            let message = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                message, NO_SUCH_DISTRIBUTION_MESSAGE,
                "case {case}: expected the sentence {NO_SUCH_DISTRIBUTION_MESSAGE:?}, got \
                 {message:?}"
            );
            assert!(
                !answer.raw.contains(id.as_str()),
                "case {case}: the refusal echoed {id:?} back in {}",
                answer.raw
            );
        }
    }
}
