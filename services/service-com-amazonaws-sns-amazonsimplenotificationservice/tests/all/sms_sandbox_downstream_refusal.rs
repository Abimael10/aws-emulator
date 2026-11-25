// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the refusal a DESTINATION number the SMS service will not carry
//! a message to is answered with.
//!
//! The rule under test: an SNS operation that asks for a message to be SENT to a phone
//! number hands the number on to the SMS service that would carry it, as that service's
//! own `destinationPhoneNumber` argument, and surfaces ITS refusal verbatim inside SNS's
//! `Invalid parameter:` frame — so the sentence names the inner service, the inner
//! argument and the inner call's own request id, never SNS's `PhoneNumber` member and
//! never the (`@sensitive`) number the caller sent. The inner service states two
//! sentences, and which one it is is the only thing distinguishing the two classes under
//! one error code (`InvalidParameterException`, awsQuery `InvalidParameter`, HTTP 400):
//! a value that is not written as a number at all fails its argument validation, while a
//! well-formed number whose country code E.164 assigns to no country is an argument it
//! cannot use.
//!
//! `CreateSMSSandboxPhoneNumber` is the implemented door: it "sends a one-time password
//! (OTP) to that phone number"
//! (`smithy_query(query="operation",operation="CreateSMSSandboxPhoneNumber")`). The
//! sandbox operations that only read or clear a record this account's sandbox already
//! holds send nothing onward and keep SNS's own member sentence, which
//! `sms_sandbox_destination_numbers.rs` holds.
//!
//! The rows differ along what the rule generalises over: the class of fault, the way the
//! value fails inside its class (no country code at all, at the bare branch's digit
//! bound, empty, one digit past the E.164 maximum, an unassigned country code at three
//! lengths) and the call repeated, which is what the per-call request id is read from.
//! The accepted forms are driven through the same call as the control.

use super::sms_sandbox_phone_numbers::{
    accepted_number_forms, given_an_origination_number, reported_numbers,
};
use super::topics_subscriptions_publish::{Answer, Sns};

/// The sentence the SMS service states for a `destinationPhoneNumber` argument that is
/// not written as a phone number at all, as SNS frames it. Wholly fixed text apart from
/// the tail: AWS's recorded `CreateSMSSandboxPhoneNumber` refusal of bare national
/// digits.
const NOT_A_NUMBER_SENTENCE: &str = "Invalid parameter: Validation Error Occurred - \
                                     Reason=\"INVALID_PARAMETER\" \
                                     Fields=\"destinationPhoneNumber\"";

/// The sentence the SMS service states for a well-formed number it cannot use as a
/// `destinationPhoneNumber`, as SNS frames it — AWS's recorded refusal of a number whose
/// country code is unassigned.
const UNUSABLE_NUMBER_SENTENCE: &str = "Invalid parameter: The following parameter is \
                                        null, empty or invalid: \
                                        ['destinationPhoneNumber']";

/// The tail every such sentence ends with, up to the inner call's request id.
const TAIL_OPEN: &str = " (Service: PinpointSmsVoiceV2, Status Code: 400, Request ID: ";

/// Whether `id` is written in the 8-4-4-4-12 lowercase-hexadecimal form of a UUID, which
/// is the form an AWS request id takes.
fn is_uuid_shaped(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .map(|group| group.len())
            .eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && id == id.to_ascii_lowercase()
}

/// The inner call's request id carried by `answer`'s message, having asserted that
/// everything before it is `fixed` followed by the fixed tail.
fn inner_request_id(case: &str, answer: &Answer, fixed: &str) -> String {
    let message = answer
        .element("Message")
        .unwrap_or_else(|| panic!("case {case:?}: no <Message> in {}", answer.body));
    let tail = message.strip_prefix(fixed).map(str::to_string);
    let tail = tail.unwrap_or_else(|| {
        panic!("case {case:?}: expected the message to open with {fixed:?}, got {message:?}")
    });
    let id = tail
        .strip_prefix(TAIL_OPEN)
        .and_then(|rest| rest.strip_suffix(')'))
        .map(str::to_string);
    let id = id.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the tail {:?} after the sentence, got {tail:?}",
            format!("{TAIL_OPEN}<request id>)")
        )
    });
    assert!(
        is_uuid_shaped(&id),
        "case {case:?}: expected the tail to name a request id, got {id:?}"
    );
    id
}

/// The refusal `answer` states, asserted to be one of the SMS service's two whole
/// sentences — the class's fixed text, the fixed tail and a request id and nothing else —
/// and reported back so a caller can say which class it was.
///
/// `pub(crate)` so the sibling destination-number suite pins the whole sentence through
/// this one reading of it rather than asserting something weaker of its own.
pub(crate) fn stated_sms_service_refusal(case: &str, answer: &Answer) -> &'static str {
    let message = answer
        .element("Message")
        .unwrap_or_else(|| panic!("case {case:?}: no <Message> in {}", answer.body));
    for fixed in [NOT_A_NUMBER_SENTENCE, UNUSABLE_NUMBER_SENTENCE] {
        if message.starts_with(fixed) {
            inner_request_id(case, answer, fixed);
            return fixed;
        }
    }
    panic!(
        "case {case:?}: expected one of the SMS service's two refusals, got {message:?}"
    )
}

/// The destination numbers the SMS service refuses, and the class each one falls in.
///
/// Every value here satisfies the modelled constraint of the member's shape
/// (`smithy_query(query="shape",shape="PhoneNumberString")`: `length {min: 0, max: 20}`,
/// pattern `^(\+[0-9]{8,}|[0-9]{0,9})$`), so the request passes the boundary validator
/// and the handler is what judges the number.
fn refused_destinations() -> Vec<(&'static str, String, &'static str)> {
    vec![
        ("a bare short code", "12345".to_string(), NOT_A_NUMBER_SENTENCE),
        (
            "bare digits at the nine digit bound of the modelled pattern",
            "123456789".to_string(),
            NOT_A_NUMBER_SENTENCE,
        ),
        ("no digits at all", String::new(), NOT_A_NUMBER_SENTENCE),
        (
            "one digit past the fifteen digits E.164 caps a number at",
            format!("+{}", "1".repeat(16)),
            NOT_A_NUMBER_SENTENCE,
        ),
        (
            "an unassigned country code, nine digits",
            "+012345678".to_string(),
            UNUSABLE_NUMBER_SENTENCE,
        ),
        (
            "an unassigned country code, ten digits",
            "+0123456789".to_string(),
            UNUSABLE_NUMBER_SENTENCE,
        ),
        (
            "an unassigned country code at the fifteen digit maximum",
            format!("+0{}", "9".repeat(14)),
            UNUSABLE_NUMBER_SENTENCE,
        ),
    ]
}

/// Each class of refused destination number states the inner service's own sentence, and
/// the refusal registers nothing.
#[tokio::test]
async fn a_destination_the_sms_service_refuses_states_that_services_sentence() {
    let sns = Sns::new();

    for (case, number, fixed) in refused_destinations() {
        let answer = sns
            .call(
                "CreateSMSSandboxPhoneNumber",
                &[("PhoneNumber", number.as_str())],
            )
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            "InvalidParameter",
            "case {case:?}: expected the InvalidParameter code, got {}",
            answer.body
        );
        // The whole message: the class's fixed sentence, the fixed tail, and the inner
        // call's request id — which is all that varies.
        inner_request_id(case, &answer, fixed);
        let message = answer.element("Message").unwrap_or_default();
        assert!(
            !message.contains("Invalid parameter: PhoneNumber"),
            "case {case:?}: the sentence names the inner service's argument, not SNS's \
             own member, got {message:?}"
        );
        assert!(
            number.is_empty() || !answer.body.contains(number.as_str()),
            "case {case:?}: the number is a sensitive member, so the refusal must not \
             echo it, got {}",
            answer.body
        );
    }

    let listed = sns.call("ListSMSSandboxPhoneNumbers", &[]).await;
    assert!(
        reported_numbers(&listed).is_empty(),
        "a refused registration must add no number, got {:?}",
        reported_numbers(&listed)
    );
}

/// The tail's request id is the INNER call's own: it is minted per call, so two identical
/// requests state the same sentence with two different ids, and it is not the id the SNS
/// response reports as its own.
#[tokio::test]
async fn the_tail_names_a_fresh_request_id_for_each_call() {
    let sns = Sns::new();

    for (case, number, fixed) in refused_destinations() {
        let mut seen: Vec<String> = Vec::new();
        for attempt in 0..2 {
            let case = format!("{case} (attempt {attempt})");
            let answer = sns
                .call(
                    "CreateSMSSandboxPhoneNumber",
                    &[("PhoneNumber", number.as_str())],
                )
                .await;
            let inner = inner_request_id(&case, &answer, fixed);
            let own = answer.element("RequestId").unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the error document must carry a <RequestId>, got {}",
                    answer.body
                )
            });
            assert_ne!(
                inner, own,
                "case {case:?}: the tail names the inner call's id, not the SNS \
                 response's own {own:?}"
            );
            seen.push(inner);
        }
        assert_ne!(
            seen[0], seen[1],
            "case {case:?}: each call carries its own inner request id, got {seen:?}"
        );
    }
}

/// The control: a destination number the SMS service WOULD carry a message to is still
/// registered, and reads back from the sandbox listing.
#[tokio::test]
async fn a_destination_the_sms_service_accepts_is_still_registered() {
    let sns = Sns::new();
    // An account that holds an entity the one-time password can be sent from, so the
    // destination number itself is plainly what the call is judged on; what the inventory
    // decides is held in `sms_sandbox_origination_entities.rs`.
    given_an_origination_number(&sns);

    for (case, number) in accepted_number_forms() {
        let answer = sns
            .call("CreateSMSSandboxPhoneNumber", &[("PhoneNumber", &number)])
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} / {}",
            answer.status, answer.body
        );
        let listed = sns.call("ListSMSSandboxPhoneNumbers", &[]).await;
        assert!(
            reported_numbers(&listed)
                .iter()
                .any(|(listed, _)| *listed == number),
            "case {case:?}: expected the number in the sandbox listing, got {:?}",
            reported_numbers(&listed)
        );
    }
}
