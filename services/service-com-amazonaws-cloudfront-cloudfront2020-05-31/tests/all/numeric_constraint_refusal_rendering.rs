// SPDX-License-Identifier: CC0-1.0

//! How a refusal of a value outside a NUMERIC constraint writes the two numbers it states,
//! end to end over HTTP.
//!
//! The rule under test: a constraint refusal reports the value submitted and the bound the
//! model declares, and both belong to the MEMBER — so each is written as a number of that
//! member's own type. A floating-point member's value and bound carry a decimal fraction
//! (`150.0`, `100.0`), whatever text the request wrote the value in and whether or not the
//! number happens to be integral; a member of an integral type, and a bound that counts
//! something rather than measuring it, carry none.
//!
//! `ResponseHeadersPolicyServerTimingHeadersConfig$SamplingRate` is the one
//! `@range`-constrained floating-point member of this service
//! (`smithy_query(query="shape",shape="ResponseHeadersPolicyServerTimingHeadersConfig")` — a
//! `double` with `@range` min 0.0 max 100.0), so the rows generalise over everything the rule
//! leaves free:
//!
//! * which BOUND the value broke — past the maximum and below the minimum, each published
//!   bound itself being accepted;
//! * the TEXT the request wrote the value in: an integer, the same number with a fraction, and
//!   the same number with trailing zeros all state one value, so one sentence answers all
//!   three;
//! * whether the submitted number is integral or fractional, and how far outside the range it
//!   sits (one step past a bound, and far past it);
//! * the OPERATION the document travelled in, over both writes that submit a
//!   `ResponseHeadersPolicyConfig`.
//!
//! The control rows are the other side of the rule: a `@length` bound, which counts characters
//! of a string member, keeps its integral form, and a value inside the range is stored rather
//! than refused — so the rendering is read off the member's type and is not a rewrite of every
//! number a refusal states.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, and the create helpers are the
//! response-headers-policy lifecycle suite's.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, config, create_distribution, emulator, Answer, ACCOUNT_ID, API_VERSION,
};
use crate::response_headers_policy_lifecycle::{create_policy, get_policy};

/// The member path every `SamplingRate` refusal below is about, as a refusal renders a path:
/// the payload member, the group it carries and the member inside it, each in lowerCamel.
const SAMPLING_RATE_PATH: &str =
    "responseHeadersPolicyConfig.serverTimingHeadersConfig.samplingRate";

/// `PUT /2020-05-31/response-headers-policy/{Id}` — the route, the payload member and the
/// version header the model binds `UpdateResponseHeadersPolicy` to.
async fn update_policy(router: &MultiServiceRouter, id: &str, body: &str, if_match: &str) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/response-headers-policy/{id}"),
        Some(body),
        Some(if_match),
    )
    .await
}

/// A `ResponseHeadersPolicyConfig` whose `Server-Timing` group states `rate` VERBATIM, so a
/// row controls the text the request writes the number in rather than only its value.
fn config_with_rate(name: &str, rate: &str) -> String {
    format!(
        "<ResponseHeadersPolicyConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\"><Name>{name}</Name>\
         <ServerTimingHeadersConfig><Enabled>true</Enabled>\
         <SamplingRate>{rate}</SamplingRate></ServerTimingHeadersConfig>\
         </ResponseHeadersPolicyConfig>"
    )
}

/// The sentence a value outside the modelled range is refused with: the frame the request
/// boundary states a constraint in, around the member, the value and the bound.
fn out_of_range_message(value: &str, requirement: &str, bound: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{SAMPLING_RATE_PATH}' failed to \
         satisfy constraint: Member must have value {requirement} than or equal to {bound}"
    )
}

/// One row: the text the request submits, and the sentence the refusal states for it.
struct Row {
    case: &'static str,
    submitted: &'static str,
    message: String,
}

/// The rows: every way a `SamplingRate` can sit outside the published range, and every text one
/// value can be written in.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an integral value past the maximum",
            submitted: "150",
            message: out_of_range_message("150.0", "less", "100.0"),
        },
        Row {
            case: "the same value written with a fraction",
            submitted: "150.0",
            message: out_of_range_message("150.0", "less", "100.0"),
        },
        Row {
            case: "the same value written with trailing zeros",
            submitted: "150.000",
            message: out_of_range_message("150.0", "less", "100.0"),
        },
        Row {
            case: "one step past the maximum",
            submitted: "101",
            message: out_of_range_message("101.0", "less", "100.0"),
        },
        Row {
            case: "a fraction past the maximum",
            submitted: "100.5",
            message: out_of_range_message("100.5", "less", "100.0"),
        },
        Row {
            case: "far past the maximum",
            submitted: "100000",
            message: out_of_range_message("100000.0", "less", "100.0"),
        },
        Row {
            case: "an integral value below the minimum",
            submitted: "-1",
            message: out_of_range_message("-1.0", "greater", "0.0"),
        },
        Row {
            case: "a fraction below the minimum",
            submitted: "-0.5",
            message: out_of_range_message("-0.5", "greater", "0.0"),
        },
        Row {
            case: "far below the minimum",
            submitted: "-250",
            message: out_of_range_message("-250.0", "greater", "0.0"),
        },
    ]
}

/// A `SamplingRate` outside the modelled range is refused with the value submitted and the
/// bound declared written as the member's own `double` writes a number — on the create that
/// submits the document.
#[tokio::test]
async fn a_rate_outside_the_range_states_both_numbers_as_doubles_on_the_create() {
    let router = emulator();

    for row in rows() {
        let name = format!("sampling-{}", row.submitted.replace(['.', '-'], "_"));
        let answer = create_policy(&router, &config_with_rate(&name, row.submitted)).await;
        assert_eq!(
            answer.status, 400,
            "case {}: expected HTTP 400 for {:?}, got {} with {}",
            row.case, row.submitted, answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "MalformedXML",
            "case {}: expected MalformedXML for {:?}, got {:?} in {}",
            row.case,
            row.submitted,
            answer.error_code(),
            answer.raw
        );
        let message = answer.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            message, row.message,
            "case {}: expected the sentence {:?} for {:?}, got {message:?}",
            row.case, row.message, row.submitted
        );
    }
}

/// The same rule on the UPDATE that submits the document: the rendering is a property of the
/// member, so it does not depend on which operation carried it — and the refused update leaves
/// the stored policy exactly as it was.
#[tokio::test]
async fn a_rate_outside_the_range_states_both_numbers_as_doubles_on_the_update() {
    let router = emulator();
    let created = create_policy(&router, &config_with_rate("sampling-update", "25.5")).await;
    assert_eq!(
        created.status, 201,
        "creating the policy under test: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();
    let etag = created.header("etag").to_string();

    for row in rows() {
        let answer = update_policy(
            &router,
            &id,
            &config_with_rate("sampling-update", row.submitted),
            &etag,
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {}: expected HTTP 400 for {:?}, got {} with {}",
            row.case, row.submitted, answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "MalformedXML",
            "case {}: expected MalformedXML for {:?}, got {:?} in {}",
            row.case,
            row.submitted,
            answer.error_code(),
            answer.raw
        );
        let message = answer.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            message, row.message,
            "case {}: expected the sentence {:?} for {:?}, got {message:?}",
            row.case, row.message, row.submitted
        );
    }

    // Every refusal above left the record as the create stored it.
    let fetched = get_policy(&router, &id).await;
    assert_eq!(
        fetched.status, 200,
        "case the refused updates: expected the policy to be readable, got {} with {}",
        fetched.status, fetched.raw
    );
    let stored = fetched
        .root()
        .find_text("SamplingRate")
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        stored, "25.5",
        "case the refused updates: expected the stored rate to be unchanged, got {stored:?} in {}",
        fetched.raw
    );
}

/// Each PUBLISHED bound is inside the range, so it is stored rather than refused: the rows
/// above are about values outside the range and not about the member being read at all.
#[tokio::test]
async fn each_published_bound_is_accepted_and_stored() {
    let router = emulator();

    // The two ends of the modelled range, and one value between them.
    for (case, submitted, stored) in [
        ("the lower bound", "0", "0"),
        ("the upper bound", "100", "100"),
        ("a value between the bounds", "0.0001", "0.0001"),
    ] {
        let name = format!("bound-{}", submitted.replace(['.', '-'], "_"));
        let created = create_policy(&router, &config_with_rate(&name, submitted)).await;
        assert_eq!(
            created.status, 201,
            "case {case} ({submitted}): expected HTTP 201, got {} with {}",
            created.status, created.raw
        );
        let id = created
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string();
        let fetched = get_policy(&router, &id).await;
        let reported = fetched.root().find_text("SamplingRate").unwrap_or_default();
        assert_eq!(
            reported, stored,
            "case {case} ({submitted}): expected the stored rate {stored:?}, got {reported:?} in \
             {}",
            fetched.raw
        );
    }
}

/// The control: a `@length` bound counts the characters of a string member, so the number it
/// reports is not a measurement of a floating-point member and keeps its integral form — the
/// rendering above is read off the member's type rather than applied to every number a refusal
/// states.
#[tokio::test]
async fn a_length_bound_keeps_its_integral_form() {
    let router = emulator();
    let created = create_distribution(&router, &config("length-bound", "length-bound").xml()).await;
    assert_eq!(
        created.status, 201,
        "creating the distribution under test: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();

    // `TagKey` is "between 1 and 128 characters"
    // (`smithy_query(query="shape",shape="TagKey")`), so a key one character past it breaks
    // that bound.
    let key = "k".repeat(129);
    let answer = call(
        &router,
        "POST",
        &format!(
            "/{API_VERSION}/tagging?Operation=Tag&Resource=arn:aws:cloudfront::{ACCOUNT_ID}:distribution/{id}"
        ),
        Some(&format!(
            "<Tags xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\"><Items>\
             <Tag><Key>{key}</Key><Value>v</Value></Tag></Items></Tags>"
        )),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "case a tag key past its length: expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    let expected = format!(
        "1 validation error detected: Value '{key}' at 'tags.items.1.member.key' failed to \
         satisfy constraint: Member must have length less than or equal to 128"
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, expected,
        "case a tag key past its length: expected the length bound stated as a count, got \
         {message:?}"
    );
}
