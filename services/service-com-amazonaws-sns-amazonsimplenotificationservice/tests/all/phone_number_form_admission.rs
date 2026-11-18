// SPDX-License-Identifier: CC0-1.0

//! Which values this service ADMITS as a phone number, surface by surface, end to end over
//! the wire.
//!
//! THE RULE: a phone-number member the model leaves an unconstrained string is held to the
//! form the service PUBLISHES for a phone number — `PhoneNumberString` is
//! `length 0..20, pattern ^(\+[0-9]{8,}|[0-9]{0,9})$`
//! (`smithy_query(query="shape",shape="PhoneNumber,PhoneNumberString")`, which reports
//! `PhoneNumber` itself as a plain sensitive string) — and how strictly depends on what the
//! operation DOES with the number:
//!
//! * an operation that only REPORTS on this account's record of a number asks only that the
//!   value be a phone number AT ALL — digits, optionally introduced by a `+` and grouped with
//!   the usual separators, and no longer than a phone number is written:
//!   `CheckIfPhoneNumberIsOptedOut` states a property of the record, and its member asks
//!   nothing more of the value ("the phone number for which you want to check the opt out
//!   status"), so every value that could be a number is answered — one the account holds no
//!   record for by the output's documented default, "false – The phone number is opted in" —
//!   while a value that is no phone number is refused with `InvalidParameter` and the fixed
//!   badly-formatted-number sentence;
//! * an operation that ACTS on the record holds the value to the published form:
//!   `OptInPhoneNumber` serves every value that form admits — a short code, a national number
//!   at the pattern's own digit bound, a country code E.164 assigns to no country — and
//!   refuses every other with `InvalidParameter` and the fixed badly-formatted-number
//!   sentence;
//! * an operation whose member names a DESTINATION a message has to reach is narrower still:
//!   the SMS sandbox trio documents "the destination phone number … that you can send SMS
//!   messages to", so it takes only a number in E.164 form — a `+`, a country code E.164
//!   assigns, and no more digits than E.164 caps a number at — and refuses the rest with that
//!   same sentence, before the sandbox record is looked up. Those members target the
//!   CONSTRAINED shape, so a value outside the published pattern is refused by the boundary
//!   validator, which states the pattern itself.
//!
//! The rows span what the rule generalises over: both branches of the published pattern, each
//! of their bounds and one past each (the international floor, the national bound, the
//! published length), a country code E.164 assigns to no country, forms carrying separators or
//! text, and the empty value. Each row is driven through one operation of each of the three
//! surfaces, so what is asserted is the same value answered three ways — and the three rules
//! NEST: a value one surface acts on is never refused by a wider one.
//!
//! Every call goes over the wire through the router the emulator serves in production, and no
//! refusal echoes the (sensitive) value back.

use service_com_amazonaws_sns_amazonsimplenotificationservice::state::invalid_phone_number_message;

use super::topics_subscriptions_publish::Sns;

/// The member the opt-out pair spells its number with on the wire, and the one the sandbox
/// trio spells its own with: `smithy_query(query="operation",…)` reports `phoneNumber` on the
/// opt-out pair and `PhoneNumber` on the sandbox operations, and an awsQuery member's wire
/// name IS its member name.
const OPT_OUT_PARAM: &str = "phoneNumber";
const SANDBOX_PARAM: &str = "PhoneNumber";

/// What each surface does with one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    /// What the row is, named in every assertion.
    case: &'static str,
    /// The value submitted where a phone number belongs.
    value: &'static str,
    /// Whether it is written as a phone number at all — what the operation that only REPORTS
    /// on the record is held to.
    reported: bool,
    /// Whether the published phone-number pattern admits it — what the operation that ACTS on
    /// the record is held to.
    admitted: bool,
    /// Whether it is a DESTINATION number an SMS could be carried to — what the sandbox trio
    /// is held to.
    destination: bool,
}

/// The table: both branches of the published pattern, their bounds, one past each, and the
/// values neither branch takes.
const ROWS: &[Row] = &[
    Row {
        case: "an E.164 number with a one-digit country code",
        value: "+12065550100",
        reported: true,
        admitted: true,
        destination: true,
    },
    Row {
        case: "an E.164 number at the pattern's international floor",
        value: "+99999999",
        reported: true,
        admitted: true,
        destination: true,
    },
    Row {
        case: "an E.164 number at the fifteen digits E.164 caps one at",
        value: "+111111111111111",
        reported: true,
        admitted: true,
        destination: true,
    },
    Row {
        case: "one digit past the fifteen E.164 caps a number at",
        value: "+1111111111111111",
        reported: true,
        admitted: true,
        destination: false,
    },
    Row {
        case: "a country code E.164 assigns to no country",
        value: "+0123456789",
        reported: true,
        admitted: true,
        destination: false,
    },
    Row {
        case: "a bare short code",
        value: "12345",
        reported: true,
        admitted: true,
        destination: false,
    },
    Row {
        case: "bare digits at the pattern's national bound",
        value: "123456789",
        reported: true,
        admitted: true,
        destination: false,
    },
    Row {
        case: "one digit past the pattern's national bound",
        value: "1234567890",
        reported: true,
        admitted: false,
        destination: false,
    },
    Row {
        case: "one digit below the pattern's international floor",
        value: "+1234567",
        reported: true,
        admitted: false,
        destination: false,
    },
    Row {
        case: "an international form past the published length",
        value: "+12345678901234567890",
        reported: false,
        admitted: false,
        destination: false,
    },
    Row {
        case: "a national form written with separators",
        value: "+1-206-555-0100",
        reported: true,
        admitted: false,
        destination: false,
    },
    Row {
        case: "a value that is no number at all",
        value: "not-a-phone-number",
        reported: false,
        admitted: false,
        destination: false,
    },
    Row {
        case: "the empty value",
        value: "",
        reported: false,
        admitted: false,
        destination: false,
    },
];

impl Row {
    /// Whether the PUBLISHED pattern matches the value, which is what a member targeting the
    /// constrained shape is refused by the boundary for.
    ///
    /// Every form this service admits matches it, and so does the empty value: the pattern's
    /// digits-only branch takes zero digits (`[0-9]{0,9}`). A `@required` member carrying no
    /// digits names no number, so the operations themselves refuse it (`admitted` is `false`
    /// for it) — past the boundary rather than at it.
    fn pattern_matches(&self) -> bool {
        self.admitted || self.value.is_empty()
    }
}

/// RULE, overall: the three judgements NEST. Every value the DESTINATION rule accepts is one
/// the published pattern admits, and every value the pattern admits is one the reporting
/// surface answers — so no operation of this service refuses a value the operation beside it
/// acts on.
#[test]
fn the_three_phone_number_judgements_nest() {
    for row in ROWS {
        let case = row.case;
        assert!(
            !row.destination || row.admitted,
            "case {case:?}: a destination number must be a form the pattern admits, got \
             {row:?}"
        );
        assert!(
            !row.admitted || row.reported,
            "case {case:?}: a form the pattern admits must be reported on, got {row:?}"
        );
    }
}

/// RULE, first clause: the opt-out CHECK answers every value that is written as a phone
/// number, reports no opt-out for one this account holds no record for, and refuses a value
/// that is no phone number at all.
#[tokio::test]
async fn the_opt_out_check_answers_every_value_written_as_a_phone_number() {
    let sns = Sns::new();
    for row in ROWS {
        let case = row.case;
        let answer = sns
            .call(
                "CheckIfPhoneNumberIsOptedOut",
                &[(OPT_OUT_PARAM, row.value)],
            )
            .await;
        if !row.reported {
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected the refusal, got {} / {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.code(),
                "InvalidParameter",
                "case {case:?}: expected the InvalidParameter code, got {}",
                answer.body
            );
            assert_eq!(
                answer.element("Message"),
                Some(invalid_phone_number_message()),
                "case {case:?}: expected the fixed badly-formatted-number sentence, got {}",
                answer.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the report to be served, got {} / {}",
            answer.status, answer.body
        );
        let reported = answer
            .element("isOptedOut")
            .or_else(|| answer.element("IsOptedOut"));
        assert_eq!(
            reported.as_deref(),
            Some("false"),
            "case {case:?}: a value with no record must be reported opted in, got {}",
            answer.body
        );
    }

    // The reports wrote nothing: a read of a record is not a write of one.
    assert!(
        sns.state().lock().phone_numbers.is_empty(),
        "the reports must write no record, got {:?}",
        sns.state().lock().phone_numbers
    );
}

/// RULE, second clause: the OPT-IN takes every value the published pattern admits and refuses
/// every other one with the fixed sentence, before the record is touched.
#[tokio::test]
async fn the_opt_in_takes_exactly_the_forms_the_published_pattern_admits() {
    let sns = Sns::new();
    for row in ROWS {
        let case = row.case;
        let answer = sns
            .call("OptInPhoneNumber", &[(OPT_OUT_PARAM, row.value)])
            .await;
        if row.admitted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the admitted form to be served, got {} / {}",
                answer.status, answer.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected the refusal, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            "InvalidParameter",
            "case {case:?}: expected the InvalidParameter code, got {}",
            answer.body
        );
        assert_eq!(
            answer.element("Message"),
            Some(invalid_phone_number_message()),
            "case {case:?}: expected the fixed badly-formatted-number sentence, got {}",
            answer.body
        );
        if !row.value.is_empty() {
            assert!(
                !answer.body.contains(row.value),
                "case {case:?}: the refusal must not echo the sensitive value, got {}",
                answer.body
            );
        }
    }
}

/// RULE, third clause: a member that names a DESTINATION takes only an E.164 number — and a
/// value the published pattern does not admit never reaches the handler at all, because that
/// member targets the CONSTRAINED shape and the boundary validator states the pattern itself.
/// Either way the form is judged before the sandbox record is looked up: a destination number
/// this account never registered is answered for the record it does not hold.
#[tokio::test]
async fn a_destination_member_takes_only_an_e164_number() {
    let sns = Sns::new();
    for row in ROWS {
        let case = row.case;
        let answer = sns
            .call("DeleteSMSSandboxPhoneNumber", &[(SANDBOX_PARAM, row.value)])
            .await;
        if row.destination {
            assert_eq!(
                answer.code(),
                "ResourceNotFound",
                "case {case:?}: a destination number the sandbox does not hold must be \
                 answered for the record, got {} / {}",
                answer.status,
                answer.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected the refusal, got {} / {}",
            answer.status, answer.body
        );
        if row.pattern_matches() {
            // Past the boundary, so it is the operation that refuses it: the number is of a
            // form the service admits but is no destination a message could be carried to.
            assert_eq!(
                answer.element("Message"),
                Some(invalid_phone_number_message()),
                "case {case:?}: expected the fixed badly-formatted-number sentence, got {}",
                answer.body
            );
        } else {
            // Outside the pattern the member's own shape publishes, so the boundary refuses
            // it and states that constraint.
            assert_eq!(
                answer.code(),
                "ValidationError",
                "case {case:?}: expected the boundary's constraint refusal, got {}",
                answer.body
            );
            let stated = answer.element("Message").unwrap_or_default();
            assert!(
                stated.contains("failed to satisfy constraint")
                    && stated.contains(OPT_OUT_PARAM),
                "case {case:?}: expected the constraint clause naming the member, got \
                 {stated:?}"
            );
        }
    }
}
