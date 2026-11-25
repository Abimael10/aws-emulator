// SPDX-License-Identifier: CC0-1.0

//! What `ListDistributionsByOwnedResource` can READ of a `ResourceArn` before it judges WHICH
//! resource that ARN names, end to end over HTTP.
//!
//! The rule under test: a member taking the ARN of a resource of this service is READ before it
//! is judged, and a value that cannot be read is refused on its SYNTAX — `InvalidArgument`
//! (HTTP 400) with the sentence naming the part that could not be read and echoing the value the
//! request carried. TWO readings can fail: the value does not open with `arn:` and is no ARN at
//! all, and the value is an ARN of this service whose resource part spells a resource type
//! CloudFront writes no resource under, which names nothing there is to resolve. Only an ARN
//! that CAN be read is judged on the TYPE it names: a type CloudFront does write that this
//! listing does not cover is the fixed resource-type sentence, and only an ARN of the one
//! shareable type reaches the store, where an id nothing is held under is `EntityNotFound`
//! (HTTP 404).
//!
//! So the type a value spells decides WHICH refusal it gets, and nothing about the store does:
//! an unreadable type is refused the same whether or not this account holds any resource, and
//! the resource that really exists keeps its page before and after every refusal.
//!
//! The rows span what the rule generalises over: for the unreadable reading, HOW the type word
//! fails to be one of this service's (the covered type spelled as its ROUTE spells it, in upper
//! case, pluralised, under the other separator, a prefix of a type, a word of no type at all,
//! and a type of another service's namespace) and the partition the ARN is written in; for the
//! readable reading, EVERY resource type CloudFront's ARN namespace writes, so the two sides are
//! asserted over the whole catalogue rather than over the types one recording happened to carry.
//!
//! The CONDITION is shared with the resource-policy surface and the SENTENCE is each
//! operation's own, so the same unreadable value is also put to the resource-policy read, which
//! answers it in that surface's words.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the listing call
//! [`crate::distributions_by_owned_resource`]'s, and the VPC origin create and the
//! resource-policy read the resource-policy suite's own.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    CLOUDFRONT_ARN_RESOURCE_TYPES, INVALID_RESOURCE_POLICY_ARN_MESSAGE, NO_SUCH_RESOURCE_MESSAGE,
    RESOURCE_POLICY_RESOURCE_TYPE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, Answer, ACCOUNT_ID};
use crate::distributions_by_owned_resource::list_by_owned_resource;
use crate::resource_policy_lifecycle::{created_vpc_origin_arn, get_resource_policy};

/// The fixed text this listing refuses an ARN whose resource TYPE it cannot read with, written
/// out as a literal so a change to the service's own wording fails this suite. The value the
/// request carried follows it verbatim.
const INVALID_TYPE_SYNTAX_PREFIX: &str = "Resource has invalid syntax: invalid resource type: ";

/// The fixed text this listing refuses a value that is no ARN AT ALL with — the same sentence
/// family under the other reading that can fail.
const INVALID_ARN_SYNTAX_PREFIX: &str = "Resource has invalid syntax: ARNs must start with 'arn:': ";

/// The whole sentence this listing refuses a READABLE ARN naming a resource type it does not
/// cover with — fixed text in full, written out as a literal for the same reason.
const INVALID_RESOURCE_TYPE_SENTENCE: &str = "Invalid parameter received. Resource Type";

/// The modelled error code every argument refusal here carries (HTTP 400).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// An id written the way this service writes a VPC origin's, for the ARNs no resource is held
/// under: what each row varies is the TYPE word, so the body is held still.
const RESOURCE_BODY: &str = "vo_ABCDEF1234567";

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

/// The ARNs whose resource TYPE this service cannot read: `(case, the ResourceArn submitted)`.
///
/// Each is an ARN of THIS service — it opens with `arn:`, names a partition and carries this
/// service's namespace — whose resource part spells a word no resource type of CloudFront's ARN
/// namespace is spelled with ([`CLOUDFRONT_ARN_RESOURCE_TYPES`]), so there is no resource in it
/// to resolve. The rows differ along HOW the word fails to be one of them, and the last varies
/// the partition the value is written in.
fn unreadable_types() -> Vec<(&'static str, String)> {
    let rows: Vec<(&'static str, String)> = vec![
        (
            "the shareable type spelled as its route spells it",
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:vpc-origin/{RESOURCE_BODY}"),
        ),
        (
            "the shareable type in upper case",
            format!(
                "arn:aws:cloudfront::{ACCOUNT_ID}:{}/{RESOURCE_BODY}",
                RESOURCE_POLICY_RESOURCE_TYPE.to_uppercase()
            ),
        ),
        (
            "the shareable type pluralised",
            format!(
                "arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}s/{RESOURCE_BODY}"
            ),
        ),
        (
            "the shareable type under the other separator",
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:vpc_origin/{RESOURCE_BODY}"),
        ),
        (
            "a prefix of a type this service writes",
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:distributio/E0ABCDEF12345"),
        ),
        (
            "a word no resource type is spelled with",
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:not-a-resource-type/{RESOURCE_BODY}"),
        ),
        (
            "a resource type of another service's namespace",
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:bucket/some-shared-bucket"),
        ),
        (
            "an unreadable type in another partition",
            format!("arn:aws-cn:cloudfront::{ACCOUNT_ID}:vpc-origin/{RESOURCE_BODY}"),
        ),
    ];
    rows
}

/// An ARN of this service whose resource part spells no type CloudFront writes a resource under
/// is refused on the SYNTAX it could not be read as, with the value echoed.
#[tokio::test]
async fn an_arn_naming_no_cloudfront_resource_type_is_refused_on_its_syntax() {
    let router = emulator();

    for (case, resource_arn) in unreadable_types() {
        let answer = list_by_owned_resource(&router, &resource_arn, "").await;
        assert_invalid_argument(
            case,
            &answer,
            &format!("{INVALID_TYPE_SYNTAX_PREFIX}{resource_arn}"),
        );
        assert!(
            message(&answer).ends_with(&resource_arn),
            "case {case}: expected the submitted {resource_arn:?} to be echoed, got {:?}",
            message(&answer)
        );
    }
}

/// Every resource type CloudFront's ARN namespace DOES write is read, and then judged on the
/// type it names: the one shareable type reaches the store, and each of the others is refused
/// with the fixed resource-type sentence rather than with the syntax one.
#[tokio::test]
async fn every_type_cloudfront_writes_is_read_and_judged_on_the_type() {
    let router = emulator();

    for kind in CLOUDFRONT_ARN_RESOURCE_TYPES {
        let resource_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:{kind}/{RESOURCE_BODY}");
        let answer = list_by_owned_resource(&router, &resource_arn, "").await;
        let case = format!("an ARN naming a {kind}");
        if kind == RESOURCE_POLICY_RESOURCE_TYPE {
            // The ARN was READ and its type IS the one this listing covers, so the store is
            // what answers: nothing is held under that body.
            assert_eq!(
                answer.status, 404,
                "case {case}: expected HTTP 404 for the covered type, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "EntityNotFound",
                "case {case}: expected the modelled EntityNotFound, got {}",
                answer.raw
            );
            assert_eq!(
                message(&answer),
                NO_SUCH_RESOURCE_MESSAGE,
                "case {case}: expected the absent sentence, got {:?}",
                message(&answer)
            );
            continue;
        }
        assert_invalid_argument(&case, &answer, INVALID_RESOURCE_TYPE_SENTENCE);
    }
}

/// The two readings that can fail are told apart, and neither swallows the answers the listing
/// must still serve: the resource that exists is listed before and after every refusal.
#[tokio::test]
async fn the_readings_that_fail_are_told_apart_and_disturb_nothing() {
    let router = emulator();
    let held = created_vpc_origin_arn(&router, "arn-readability-held").await;

    assert_page(&router, &held, "before the refusals").await;

    // No ARN at all: the value does not open with `arn:`, so the sentence states THAT reading.
    for value in ["not-an-arn", "cloudfront::vpc-origin/vo_1"] {
        let answer = list_by_owned_resource(&router, value, "").await;
        assert_invalid_argument(
            &format!("a value that is no ARN: {value:?}"),
            &answer,
            &format!("{INVALID_ARN_SYNTAX_PREFIX}{value}"),
        );
    }

    // An ARN of this service whose type cannot be read: the same family, the other reading.
    let unreadable = format!("arn:aws:cloudfront::{ACCOUNT_ID}:vpc-origin/{RESOURCE_BODY}");
    let answer = list_by_owned_resource(&router, &unreadable, "").await;
    assert_invalid_argument(
        "an ARN whose resource type cannot be read",
        &answer,
        &format!("{INVALID_TYPE_SYNTAX_PREFIX}{unreadable}"),
    );

    // An ARN of ANOTHER service names a resource of that service's namespace, which this one
    // has no spelling to read it against, so it is carried through to the type judgement.
    let foreign = format!("arn:aws:s3:::bucket-{ACCOUNT_ID}");
    let answer = list_by_owned_resource(&router, &foreign, "").await;
    assert_invalid_argument(
        "an ARN of another service",
        &answer,
        INVALID_RESOURCE_TYPE_SENTENCE,
    );

    assert_page(&router, &held, "after the refusals").await;
}

/// The CONDITION is the sharing surface's and the SENTENCE is each operation's own: the
/// resource-policy read answers the same unreadable value in its own words.
#[tokio::test]
async fn the_resource_policy_read_words_the_same_condition_its_own_way() {
    let router = emulator();

    let unreadable = format!("arn:aws:cloudfront::{ACCOUNT_ID}:vpc-origin/{RESOURCE_BODY}");
    let answer = get_resource_policy(&router, &unreadable).await;
    assert_invalid_argument(
        "the resource-policy read of an unreadable resource type",
        &answer,
        INVALID_RESOURCE_POLICY_ARN_MESSAGE,
    );
}

/// The listing of a held shareable resource answers a page of the modelled list.
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
