// SPDX-License-Identifier: CC0-1.0

//! How `ListDistributionsByOwnedResource` WORDS the two refusals it decides from the
//! `ResourceArn` alone, end to end over HTTP.
//!
//! The rule under test: the sentence an operation puts on the wire is its OWN, even where the
//! CONDITION it refuses is one the service judges for several operations — so this listing
//! refuses a `ResourceArn` that cannot be read as an ARN with its syntax sentence, whose fixed
//! text is followed by the value the request carried, and a well-formed ARN naming a resource
//! type the listing does not cover with its fixed resource-type sentence, which carries nothing
//! of the request or of the store. Both are the modelled `InvalidArgument` (HTTP 400), and both
//! are decided before the store is consulted, so the type refusal answers the same for an ARN
//! of an uncovered type whose resource EXISTS and for the same ARN naming nothing — which is
//! what keeps `EntityNotFound` for an ARN of the type the listing does cover.
//!
//! The rows span what the rule generalises over: for the syntax refusal, the FORM of the value
//! that is no ARN (a bare resource id, a word, an ARN-shaped value with the prefix stripped, one
//! whose prefix differs only in case, one opening with another scheme, and one carrying the
//! prefix later in the value), so the echoed part differs on every row; for the type refusal,
//! WHICH type the ARN names — every type this service mints an ARN for other than the shareable
//! one, an ARN of another service, and an ARN whose resource part names a type but no id — and,
//! for one type, whether the resource it names exists.
//!
//! The wire harness is the one [`crate::distribution_lifecycle`] builds, the listing call the
//! one [`crate::distributions_by_owned_resource`] makes, and the VPC origin create the
//! resource-policy suite's own.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    NO_SUCH_RESOURCE_MESSAGE, RESOURCE_POLICY_RESOURCE_TYPE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{create_distribution, emulator, Answer, ACCOUNT_ID};
use crate::distributions_by_owned_resource::list_by_owned_resource;
use crate::resource_policy_lifecycle::{created_vpc_origin_arn, distribution_config};

/// The fixed text this operation refuses a `ResourceArn` that is no ARN at all with, written
/// out as a literal so a change to the service's own wording fails this suite. The value the
/// request carried follows it verbatim.
const INVALID_SYNTAX_PREFIX: &str = "Resource has invalid syntax: ARNs must start with 'arn:': ";

/// The whole sentence this operation refuses a well-formed `ResourceArn` naming a resource type
/// it does not cover with — fixed text in full, written out as a literal for the same reason.
const INVALID_RESOURCE_TYPE_SENTENCE: &str = "Invalid parameter received. Resource Type";

/// The modelled error code both refusals carry (`InvalidArgument`, HTTP 400).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// Every resource type this service mints an ARN for OTHER than the one shareable type, so an
/// ARN naming any of them is a type this listing does not cover.
///
/// Read off the model: each is a resource created or addressed by a route of its own —
/// `smithy_query(query="list_operations")` names the creates
/// (`CreateAnycastIpList`, `CreateConnectionFunction`, `CreateConnectionGroup`,
/// `CreateDistribution`, `CreateDistributionTenant`, `CreateFunction`, `CreateKeyValueStore`,
/// `CreateStreamingDistribution`, `CreateTrustStore`, `CreateCachePolicy`,
/// `CreateContinuousDeploymentPolicy`, `CreateOriginAccessControl`,
/// `CreateOriginRequestPolicy`, `CreatePublicKey`, `CreateResponseHeadersPolicy`) — and each is
/// spelled here the way that resource's ARN spells its type. Sorted, so the list reads as the
/// SET it is.
const UNCOVERED_RESOURCE_TYPES: [&str; 15] = [
    "anycast-ip-list",
    "cache-policy",
    "connection-function",
    "connection-group",
    "continuous-deployment-policy",
    "distribution",
    "distribution-tenant",
    "function",
    "key-value-store",
    "origin-access-control",
    "origin-request-policy",
    "public-key",
    "response-headers-policy",
    "streaming-distribution",
    "trust-store",
];

/// The error message an answer states, or `""` when it carries none.
fn message(answer: &Answer) -> &str {
    answer.root().find_text("Message").unwrap_or_default()
}

/// Assert `answer` is the modelled `InvalidArgument` (HTTP 400) stating `sentence`.
fn assert_invalid_argument(case: &str, answer: &Answer, sentence: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        INVALID_ARGUMENT,
        "case {case}: expected the modelled {INVALID_ARGUMENT}, got {}",
        answer.raw
    );
    assert_eq!(
        message(answer),
        sentence,
        "case {case}: expected the sentence {sentence:?}, got {:?}",
        message(answer)
    );
}

/// A `ResourceArn` that cannot be READ as an ARN is refused with this operation's syntax
/// sentence, which carries the value the request submitted.
#[tokio::test]
async fn a_resource_arn_that_is_no_arn_is_refused_with_the_syntax_sentence() {
    let router = emulator();
    let held = created_vpc_origin_arn(&router, "syntax-held").await;
    let held_id = held
        .rsplit('/')
        .next()
        .expect("the VPC origin ARN's id")
        .to_string();

    // (case, the ResourceArn submitted) — the form of a value that is no ARN at all.
    let rows: Vec<(&str, String)> = vec![
        ("the shareable resource's bare id", held_id),
        ("a word", "not-an-arn".to_string()),
        (
            "an ARN with its prefix stripped",
            format!("aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo_ABCDEF"),
        ),
        (
            "a prefix differing only in case",
            format!("ARN:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo_ABCDEF"),
        ),
        (
            "another scheme",
            format!("urn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo_ABCDEF"),
        ),
        (
            "the prefix later in the value",
            format!("x-arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo_X"),
        ),
        ("a bare separator run", "-".to_string()),
    ];

    for (case, resource_arn) in rows {
        let answer = list_by_owned_resource(&router, &resource_arn, "").await;
        assert_invalid_argument(
            case,
            &answer,
            &format!("{INVALID_SYNTAX_PREFIX}{resource_arn}"),
        );
        assert!(
            message(&answer).ends_with(&resource_arn),
            "case {case}: expected the submitted {resource_arn:?} to be echoed, got {:?}",
            message(&answer)
        );
    }
}

/// A well-formed ARN naming a resource type this listing does not cover is refused on the TYPE
/// it names, with the fixed resource-type sentence, whether or not the resource exists.
#[tokio::test]
async fn an_arn_of_an_uncovered_resource_type_is_refused_on_the_type() {
    let router = emulator();

    for kind in UNCOVERED_RESOURCE_TYPES {
        let answer = list_by_owned_resource(
            &router,
            &format!("arn:aws:cloudfront::{ACCOUNT_ID}:{kind}/ABCDEF1234567"),
            "",
        )
        .await;
        assert_invalid_argument(
            &format!("an ARN naming a {kind}"),
            &answer,
            INVALID_RESOURCE_TYPE_SENTENCE,
        );
    }

    for (case, resource_arn) in [
        (
            "an ARN of another service",
            format!("arn:aws:s3:::bucket-{ACCOUNT_ID}"),
        ),
        (
            "an ARN naming a type and no id",
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:distribution"),
        ),
        (
            "an ARN naming nothing after its account",
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:/"),
        ),
    ] {
        let answer = list_by_owned_resource(&router, &resource_arn, "").await;
        assert_invalid_argument(case, &answer, INVALID_RESOURCE_TYPE_SENTENCE);
    }
}

/// The TYPE is judged before existence: the ARN of a resource of an uncovered type that really
/// exists and the same ARN with an id this service holds nothing under are refused identically.
#[tokio::test]
async fn an_uncovered_type_is_refused_before_the_store_is_asked() {
    let router = emulator();
    let created = create_distribution(&router, &distribution_config("owned-resource-type")).await;
    assert_eq!(
        created.status, 201,
        "expected a created distribution to name an existing uncovered resource, got {} with {}",
        created.status, created.raw
    );
    let held_id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();
    assert!(
        !held_id.is_empty(),
        "expected the created distribution's id, got {}",
        created.raw
    );

    for (case, id) in [
        ("a distribution that exists", held_id.as_str()),
        ("a distribution id nothing is held under", "E0ABSENT12345"),
    ] {
        let answer = list_by_owned_resource(
            &router,
            &format!("arn:aws:cloudfront::{ACCOUNT_ID}:distribution/{id}"),
            "",
        )
        .await;
        assert_invalid_argument(case, &answer, INVALID_RESOURCE_TYPE_SENTENCE);
    }
}

/// The control the two sentences must not swallow: under the type this listing DOES cover, an
/// ARN naming no resource this store holds is `EntityNotFound` (HTTP 404), and a created
/// resource's own ARN is served a page — before and after the refusals, which disturb no state.
#[tokio::test]
async fn the_covered_resource_type_keeps_its_own_answers() {
    let router = emulator();
    let held = created_vpc_origin_arn(&router, "wording-control").await;

    assert_page(&router, &held, "before the refusals").await;

    let absent = format!("arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo_X1");
    let answer = list_by_owned_resource(&router, &absent, "").await;
    assert_eq!(
        answer.status, 404,
        "case the covered type naming nothing: expected HTTP 404, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "EntityNotFound",
        "case the covered type naming nothing: expected the modelled EntityNotFound, got {}",
        answer.raw
    );
    assert_eq!(
        message(&answer),
        NO_SUCH_RESOURCE_MESSAGE,
        "case the covered type naming nothing: expected the absent sentence, got {:?}",
        message(&answer)
    );

    let refused = list_by_owned_resource(&router, "not-an-arn", "").await;
    assert_invalid_argument(
        "a value that is no ARN",
        &refused,
        &format!("{INVALID_SYNTAX_PREFIX}not-an-arn"),
    );

    assert_page(&router, &held, "after the refusals").await;
}

/// The listing of a held shareable resource answers an empty page of the modelled list.
async fn assert_page(router: &MultiServiceRouter, resource_arn: &str, case: &str) {
    let answer = list_by_owned_resource(router, resource_arn, "").await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the held resource to be listed, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.root().find_text("Quantity").unwrap_or_default(),
        "0",
        "case {case}: expected a page of no distributions, got {}",
        answer.raw
    );
}
