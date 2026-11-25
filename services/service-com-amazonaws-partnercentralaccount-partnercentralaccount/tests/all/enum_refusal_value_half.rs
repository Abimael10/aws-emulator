// SPDX-License-Identifier: CC0-1.0

//! What an enum-constraint refusal states about the value it REFUSED.
//!
//! The rule under test: a clause refusing a value outside an enumerated type echoes that
//! value — `Value '<supplied value>' at '<member path>' failed to satisfy constraint: Member
//! must satisfy enum value set: [ … ]`, the same value half a `@length`, a `@range` and a
//! `@pattern` clause of the very same message state — except for the enumerated types whose
//! refusals state no value at all, whose clause names the member and the admissible set
//! alone (`Value at '<member path>' …`). Which it is belongs to the enumerated TYPE rather
//! than to a member, an operation or a request, so one type is stated the same way at
//! whatever depth and in whatever binding its value arrived; and where a clause echoes, it
//! echoes what the request actually sent, so two requests refusing one member for two
//! different inadmissible values state two different messages.
//!
//! The rows differ along what the rule generalises over: which enumerated type carried the
//! refused value (seven of this model's, of two, three, five, eight and thirty-seven
//! values, including the one type that states no value), which operation and member carried
//! it (nine members over seven operations, one of them nested inside a structure member),
//! the BINDING the refused value arrived in (a top-level member, a member nested in a
//! structure member, and an element of a list of an enumerated type, at the list's first,
//! middle and last admissible position), how the refused value was spelled (a name no value
//! carries, an admitted value miscased, padded, the empty string, a value carrying a quote,
//! a value carrying the set's own text, an admitted value of another type), and how many
//! constraint violations one request carries (one; an enum together with another class —
//! including a class on a `@sensitive` member, whose value stays withheld either way; and
//! two enum violations, of one type and of two — including two types whose value halves
//! differ inside one message). The value SET's order is not pinned here — it is
//! `enum_value_set_order.rs`'s rule; this suite reads the set out of the message and holds
//! it to being the modelled set re-ordered.

use serde_json::{json, Value};

use service_com_amazonaws_partnercentralaccount_partnercentralaccount::ops::types::{
    ACCESS_TYPE_VALUES, CONNECTION_TYPE_VALUES, INDUSTRY_SEGMENT_VALUES, INVITATION_STATUS_VALUES,
    PARTICIPANT_TYPE_VALUES, PRIMARY_SOLUTION_TYPE_VALUES, PROFILE_VISIBILITY_VALUES,
};

use crate::connection_lifecycle::{error_code, string, Emulator, RECEIVER_PROFILE};
use crate::enum_value_set_order::create_partner;

/// `ClientToken`'s modelled maximum (`@length(1, 64)`, source
/// `smithy_query(query="operation",operation="CreatePartner")`).
const CLIENT_TOKEN_MAX: usize = 64;

/// An `IndustrySegment` the model admits, so a row refusing one element of a list of that
/// type does not refuse its other elements too (source:
/// `smithy_query(query="shape",shape="IndustrySegment")`).
const ADMITTED_SEGMENT: &str = "RETAIL";

/// `IndustrySegmentList`'s modelled maximum element count (`@length(1, 3)`, source
/// `smithy_query(query="shape",shape="IndustrySegmentList")`), so a row refusing the list's
/// last position refuses an element the list may actually carry.
const INDUSTRY_SEGMENTS_MAX: usize = 3;

/// Whether a refusal of a value outside the type whose admissible values are `modelled`
/// states that value: the invitation status a connection invitation is filtered by
/// (`InvitationStatus`) is the one enumerated type of this model whose refusals state none,
/// and every other type echoes what the request sent.
fn withholds_the_value(modelled: &[&str]) -> bool {
    modelled == INVITATION_STATUS_VALUES
}

/// Everything ONE clause states before the value set it lists: the value half the refused
/// value's type gives it, the member path and the requirement.
fn clause_before_the_set(member_path: &str, refused: &str, modelled: &[&str]) -> String {
    let value_half = if withholds_the_value(modelled) {
        "Value".to_string()
    } else {
        format!("Value '{refused}'")
    };
    format!(
        "{value_half} at '{member_path}' failed to satisfy constraint: Member must satisfy enum \
         value set: ["
    )
}

/// Everything a message stating ONE clause says before the value set: the envelope's
/// count, then the clause.
fn clause_opening(member_path: &str, refused: &str, modelled: &[&str]) -> String {
    format!(
        "1 validation error detected: {}",
        clause_before_the_set(member_path, refused, modelled)
    )
}

/// A `StartProfileUpdateTask` request valid but for `solution_type`, which sits on the
/// nested `TaskDetails` member (source:
/// `smithy_query(query="shape",shape="TaskDetails")`).
fn start_profile_update_task(solution_type: &str) -> Value {
    profile_update_task(solution_type, &[ADMITTED_SEGMENT])
}

/// A `StartProfileUpdateTask` request valid but for the enum members given: the solution
/// type on the nested `TaskDetails` member, and the elements of the `IndustrySegments` list
/// it carries (source: `smithy_query(query="shape",shape="TaskDetails")`).
fn profile_update_task(solution_type: &str, segments: &[&str]) -> Value {
    json!({
        "Catalog": "AWS",
        "Identifier": "partner-ABCDEFGHIJKLM",
        "ClientToken": "tok-profile-task",
        "TaskDetails": {
            "DisplayName": "Example Partner",
            "Description": "An example partner profile.",
            "WebsiteUrl": "https://example.com",
            "LogoUrl": "https://example.com/logo.png",
            "PrimarySolutionType": solution_type,
            "IndustrySegments": segments,
            "TranslationSourceLocale": "en-US",
        },
    })
}

/// The elements of an `IndustrySegments` list filled to its modelled maximum whose element
/// at `position` (1-based, as a member path counts one) is outside the type and whose every
/// other element is admitted.
fn segments_refused_at(position: usize, refused: &'static str) -> Vec<&'static str> {
    (1..=INDUSTRY_SEGMENTS_MAX)
        .map(|index| {
            if index == position {
                refused
            } else {
                ADMITTED_SEGMENT
            }
        })
        .collect()
}

/// Every enum-constrained input member of this service, each refused a value outside its
/// type: one validation error, naming the member, the value half its type states and the
/// set it had to come from — and the set it lists is the modelled set re-ordered.
#[tokio::test]
async fn an_enum_refusal_states_the_value_half_its_type_states() {
    let emulator = Emulator::new();
    let outside = "NOT_A_MODELLED_VALUE";

    // (case, operation, request, member path, the type's modelled value set)
    let rows: Vec<(&str, &str, Value, &str, &[&str])> = vec![
        (
            "ListConnectionInvitations' status filter",
            "ListConnectionInvitations",
            json!({ "Catalog": "AWS", "Status": outside }),
            "status",
            INVITATION_STATUS_VALUES,
        ),
        (
            "ListConnectionInvitations' participant-type filter",
            "ListConnectionInvitations",
            json!({ "Catalog": "AWS", "ParticipantType": outside }),
            "participantType",
            PARTICIPANT_TYPE_VALUES,
        ),
        (
            "ListConnectionInvitations' connection-type filter",
            "ListConnectionInvitations",
            json!({ "Catalog": "AWS", "ConnectionType": outside }),
            "connectionType",
            CONNECTION_TYPE_VALUES,
        ),
        (
            "CreateConnectionInvitation's connection type",
            "CreateConnectionInvitation",
            json!({
                "Catalog": "AWS",
                "ClientToken": "tok-create-invitation",
                "ConnectionType": outside,
                "Email": "inviter@example.com",
                "Message": "please connect",
                "Name": "Inviter",
                "ReceiverIdentifier": RECEIVER_PROFILE,
            }),
            "connectionType",
            CONNECTION_TYPE_VALUES,
        ),
        (
            "CancelConnection's connection type",
            "CancelConnection",
            json!({
                "Catalog": "AWS",
                "Identifier": "pac-ABCDEFGHIJKLM",
                "ConnectionType": outside,
                "Reason": "no longer needed",
                "ClientToken": "tok-cancel",
            }),
            "connectionType",
            CONNECTION_TYPE_VALUES,
        ),
        (
            "PutProfileVisibility's visibility",
            "PutProfileVisibility",
            json!({
                "Catalog": "AWS",
                "Identifier": "partner-ABCDEFGHIJKLM",
                "Visibility": outside,
            }),
            "visibility",
            PROFILE_VISIBILITY_VALUES,
        ),
        (
            "UpdateConnectionPreferences' access type",
            "UpdateConnectionPreferences",
            json!({ "Catalog": "AWS", "Revision": 1, "AccessType": outside }),
            "accessType",
            ACCESS_TYPE_VALUES,
        ),
        (
            "CreatePartner's primary solution type",
            "CreatePartner",
            create_partner(outside, "tok-value-half"),
            "primarySolutionType",
            PRIMARY_SOLUTION_TYPE_VALUES,
        ),
        (
            "StartProfileUpdateTask's solution type, nested in TaskDetails",
            "StartProfileUpdateTask",
            start_profile_update_task(outside),
            "taskDetails.primarySolutionType",
            PRIMARY_SOLUTION_TYPE_VALUES,
        ),
    ];

    for (case, operation, request, member_path, modelled) in rows {
        let (status, body) = emulator.call(operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the modelled set is refused, got {status}: {body}"
        );
        let code = error_code(&body);
        assert!(
            code.contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {code:?}: {body}"
        );
        let message = string(&body, "Message", case);
        assert_eq!(
            message.contains(outside),
            !withholds_the_value(modelled),
            "case {case:?}: the clause states the value half its type states, got {message:?}"
        );
        let listed = listed_set(&message, member_path, outside, modelled, case);
        assert_modelled_set_re_ordered(&listed, modelled, case);
    }
}

/// `listed` is the modelled set re-ordered: every admissible value stated exactly once,
/// nothing added and nothing dropped.
fn assert_modelled_set_re_ordered(listed: &[String], modelled: &[&str], case: &str) {
    let mut sorted_listed = listed.to_vec();
    sorted_listed.sort_unstable();
    let mut sorted_modelled: Vec<String> =
        modelled.iter().map(|value| (*value).to_string()).collect();
    sorted_modelled.sort_unstable();
    assert_eq!(
        sorted_listed, sorted_modelled,
        "case {case:?}: the listed set is the modelled set re-ordered, got {listed:?}"
    );
}

/// The values a refusal lists for `member_path`, with the whole clause around them held to
/// the wording the rule states — so a clause stating the wrong value half, at the member
/// path or anywhere before it, fails here.
fn listed_set(
    message: &str,
    member_path: &str,
    refused: &str,
    modelled: &[&str],
    case: &str,
) -> Vec<String> {
    let opening = clause_opening(member_path, refused, modelled);
    let listed = message
        .strip_prefix(opening.as_str())
        .and_then(|tail| tail.strip_suffix(']'))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {opening:?} then the set, got {message:?}")
        });
    listed.split(", ").map(str::to_string).collect()
}

/// A clause that ECHOES follows what the request sent: one member refused every spelling of
/// an inadmissible value a caller can send quotes that spelling back, so no two of them
/// state one message.
#[tokio::test]
async fn an_echoing_clause_quotes_back_what_the_request_sent() {
    let emulator = Emulator::new();

    // A long value, under the ceiling the framework's own walk elides a clause's echo at:
    // past it the clause quotes a prefix and the value's byte count, which is the
    // framework's rendering of a long value rather than this rule.
    let long = "V".repeat(200);
    let spellings = [
        ("a name no value carries", "NOT_A_SOLUTION_TYPE"),
        ("an admitted value miscased", "software_products"),
        ("an admitted value padded", " SOFTWARE_PRODUCTS"),
        ("the empty string", ""),
        ("a value carrying a quote", "O'Brien"),
        (
            "a value carrying the set's own text",
            "TRAINING_SERVICES, MANAGED_SERVICES",
        ),
        ("an admitted value of ANOTHER type", "PENDING"),
        ("a long value", long.as_str()),
    ];

    let mut stated: Vec<(&str, String)> = Vec::new();
    for (case, refused) in spellings {
        let (status, body) = emulator
            .call("CreatePartner", &create_partner(refused, "tok-spelling"))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the set is refused, got {status}: {body}"
        );
        let message = string(&body, "Message", case);
        // The clause states the member, the value it refused and the set: `listed_set`
        // holds the whole opening of the message.
        listed_set(
            &message,
            "primarySolutionType",
            refused,
            PRIMARY_SOLUTION_TYPE_VALUES,
            case,
        );
        for (earlier_case, earlier) in &stated {
            assert_ne!(
                &message, earlier,
                "case {case:?}: a clause quoting what was sent differs from {earlier_case:?}'s, \
                 got {message:?}"
            );
        }
        stated.push((case, message));
    }
}

/// A clause of the one type that WITHHOLDS says nothing of the request: that member refused
/// every spelling of an inadmissible value states one and the same message, so nothing of
/// the submitted value can be read back out of it.
#[tokio::test]
async fn a_withholding_clause_does_not_follow_what_the_request_sent() {
    let emulator = Emulator::new();

    let long = "V".repeat(200);
    let spellings = [
        ("a name no value carries", "NOT_A_MODELLED_VALUE"),
        ("an admitted value miscased", "pending"),
        ("an admitted value padded", " PENDING"),
        ("the empty string", ""),
        ("a value carrying a quote", "O'Brien"),
        ("a value carrying the set's own text", "EXPIRED, ACCEPTED"),
        ("an admitted value of ANOTHER type", "SUBSIDIARY"),
        ("a long value", long.as_str()),
    ];

    let mut first: Option<(&str, String)> = None;
    for (case, refused) in spellings {
        let (status, body) = emulator
            .call(
                "ListConnectionInvitations",
                &json!({ "Catalog": "AWS", "Status": refused }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the set is refused, got {status}: {body}"
        );
        let message = string(&body, "Message", case);
        // The clause states the member and the set and nothing of the value: `listed_set`
        // holds the whole opening of the message.
        listed_set(&message, "status", refused, INVITATION_STATUS_VALUES, case);
        match &first {
            None => first = Some((case, message)),
            Some((earlier_case, earlier)) => assert_eq!(
                &message, earlier,
                "case {case:?}: a clause that states no value reads the same as \
                 {earlier_case:?}'s, got {message:?}"
            ),
        }
    }
}

/// Two classes in one message each state their own value half: a request that breaks an
/// enum and a `@length` maximum together quotes the over-long value it sent in the length
/// clause and the inadmissible value it sent in the enum clause — whatever either value
/// was.
#[tokio::test]
async fn each_class_in_the_same_request_states_its_own_value_half() {
    let emulator = Emulator::new();

    // (case, the refused solution type, the over-long client token)
    let rows = [
        (
            "a name no value carries, with a token one character over the maximum",
            "NOT_A_SOLUTION_TYPE",
            "t".repeat(CLIENT_TOKEN_MAX + 1),
        ),
        (
            "an admitted value miscased, with a far longer token",
            "managed_services",
            format!("tok-{}", "a".repeat(CLIENT_TOKEN_MAX)),
        ),
    ];

    for (case, refused, over_long_token) in rows {
        let (status, body) = emulator
            .call("CreatePartner", &create_partner(refused, &over_long_token))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: two constraint violations are HTTP 400, got {status}: {body}"
        );
        let message = string(&body, "Message", case);
        assert!(
            message.starts_with("2 validation errors detected: "),
            "case {case:?}: the envelope counts the violations, got {message:?}"
        );
        let token_clause = format!(
            "Value '{over_long_token}' at 'clientToken' failed to satisfy constraint: Member must \
             have length less than or equal to {CLIENT_TOKEN_MAX}"
        );
        assert!(
            message.contains(&token_clause),
            "case {case:?}: expected the length clause {token_clause:?}, got {message:?}"
        );
        let enum_clause = clause_before_the_set(
            "primarySolutionType",
            refused,
            PRIMARY_SOLUTION_TYPE_VALUES,
        );
        assert!(
            message.contains(enum_clause.as_str()),
            "case {case:?}: expected the enum clause {enum_clause:?}, got {message:?}"
        );
    }
}

/// What the model marks `@sensitive` stays withheld whatever the enum clause states: a
/// request breaking a sensitive member's `@length` and an enumerated member's set together
/// states the sensitive member with no value and the enumerated one with the value half its
/// type gives it.
#[tokio::test]
async fn a_sensitive_member_keeps_its_value_withheld_beside_an_enum_clause() {
    let emulator = Emulator::new();

    // `EmailVerificationCode` is modelled `@sensitive` `@length(6, 6)` `@pattern(^[0-9]+$)`
    // (source: `smithy_query(query="operation",operation="CreatePartner")`).
    let rows = [
        ("a code one digit over the length", "1234567"),
        ("a far longer code", "9876543210"),
    ];
    let refused = "NOT_A_SOLUTION_TYPE";

    for (case, code) in rows {
        let mut request = create_partner(refused, "tok-sensitive");
        request["EmailVerificationCode"] = json!(code);
        let (status, body) = emulator.call("CreatePartner", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: two constraint violations are HTTP 400, got {status}: {body}"
        );
        let message = string(&body, "Message", case);
        assert!(
            !message.contains(code),
            "case {case:?}: a sensitive member's value is never stated, got {message:?}"
        );
        let sensitive_clause = "Value at 'emailVerificationCode' failed to satisfy constraint: \
                                Member must have length less than or equal to 6";
        assert!(
            message.contains(sensitive_clause),
            "case {case:?}: expected the sensitive member's clause {sensitive_clause:?}, got \
             {message:?}"
        );
        let enum_clause = clause_before_the_set(
            "primarySolutionType",
            refused,
            PRIMARY_SOLUTION_TYPE_VALUES,
        );
        assert!(
            message.contains(enum_clause.as_str()),
            "case {case:?}: expected the enum clause {enum_clause:?}, got {message:?}"
        );
    }
}

/// The refusal is owed to a value OUTSIDE the set and to nothing else: every value the
/// type admits is served.
#[tokio::test]
async fn every_admitted_value_is_served() {
    let emulator = Emulator::new();

    for status_filter in INVITATION_STATUS_VALUES {
        let answered = emulator
            .ok(
                "ListConnectionInvitations",
                &json!({ "Catalog": "AWS", "Status": status_filter }),
            )
            .await;
        assert!(
            answered
                .get("ConnectionInvitationSummaries")
                .and_then(Value::as_array)
                .is_some(),
            "case {status_filter:?}: an admitted filter value is served a list, got {answered}"
        );
    }
}

/// The rule holds in the LIST binding too: an element of a list of an enumerated type
/// outside that type is refused by the element's own member path, with the admissible set
/// listed and the element's own value echoed — whichever position of the list carried it.
#[tokio::test]
async fn an_element_of_a_list_of_the_type_states_its_own_value() {
    let emulator = Emulator::new();
    let outside = "NOT_A_MODELLED_VALUE";

    // One row per position the list admits: first, middle and last.
    for position in 1..=INDUSTRY_SEGMENTS_MAX {
        let case = format!("the industry segment at position {position} of {INDUSTRY_SEGMENTS_MAX}");
        let case = case.as_str();
        let (status, body) = emulator
            .call(
                "StartProfileUpdateTask",
                &profile_update_task(
                    "SOFTWARE_PRODUCTS",
                    &segments_refused_at(position, outside),
                ),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the modelled set is refused, got {status}: {body}"
        );
        let code = error_code(&body);
        assert!(
            code.contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {code:?}: {body}"
        );
        let message = string(&body, "Message", case);
        let member_path = format!("taskDetails.industrySegments.{position}.member");
        let listed = listed_set(
            &message,
            &member_path,
            outside,
            INDUSTRY_SEGMENT_VALUES,
            case,
        );
        assert_modelled_set_re_ordered(&listed, INDUSTRY_SEGMENT_VALUES, case);
    }
}

/// One row of the two-violation table: the case, the operation, the request, and the two
/// member paths each with the value it refused and the value set its clause must list.
type TwoEnumViolations = (
    &'static str,
    &'static str,
    Value,
    [(String, &'static str, &'static [&'static str]); 2],
);

/// TWO members outside their enumerated types in one request: the envelope counts both and
/// each clause names its own member path, its own set and the value half its own type
/// states — whether the two members reach two enumerated types whose value halves DIFFER,
/// or one type at two positions of a list.
#[tokio::test]
async fn two_enum_violations_each_state_their_own_member_and_value_half() {
    let emulator = Emulator::new();
    let outside = "NOT_A_MODELLED_VALUE";

    let rows: Vec<TwoEnumViolations> = vec![
        (
            "two filters of two different enumerated types, one stating its value and one not",
            "ListConnectionInvitations",
            json!({ "Catalog": "AWS", "Status": outside, "ParticipantType": outside }),
            [
                ("status".to_string(), outside, INVITATION_STATUS_VALUES),
                (
                    "participantType".to_string(),
                    outside,
                    PARTICIPANT_TYPE_VALUES,
                ),
            ],
        ),
        (
            "two elements of one list, so one type at two positions",
            "StartProfileUpdateTask",
            profile_update_task("SOFTWARE_PRODUCTS", &[ADMITTED_SEGMENT, outside, outside]),
            [
                (
                    "taskDetails.industrySegments.2.member".to_string(),
                    outside,
                    INDUSTRY_SEGMENT_VALUES,
                ),
                (
                    "taskDetails.industrySegments.3.member".to_string(),
                    outside,
                    INDUSTRY_SEGMENT_VALUES,
                ),
            ],
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, body) = emulator.call(operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: two values outside their sets are refused, got {status}: {body}"
        );
        let message = string(&body, "Message", case);
        assert!(
            message.starts_with("2 validation errors detected: "),
            "case {case:?}: the envelope counts both violations, got {message:?}"
        );
        for (member_path, refused, modelled) in expected {
            // The clause is read out of a message holding two of them, so only this
            // clause's own wording — path, requirement and its value half — is held here.
            let clause = clause_before_the_set(&member_path, refused, modelled);
            let listed = message
                .split_once(clause.as_str())
                .and_then(|(_, tail)| tail.split_once(']'))
                .map(|(listed, _)| listed.split(", ").map(str::to_string).collect::<Vec<_>>())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the clause {clause:?}, got {message:?}")
                });
            assert_modelled_set_re_ordered(&listed, modelled, case);
        }
    }
}

/// The value half is decided per TYPE wherever the member sits: a nested member outside its
/// `@pattern` echoes the value it refused in the same message whose enum clause echoes its
/// own.
#[tokio::test]
async fn a_pattern_clause_beside_an_enum_clause_keeps_its_own_value_half() {
    let emulator = Emulator::new();
    let outside = "NOT_A_SOLUTION_TYPE";
    // `TaskDetails$TranslationSourceLocale`'s modelled `@pattern` `^[a-z]{2}-[A-Z]{2}$`
    // (source: `smithy_query(query="shape",shape="TaskDetails")`), as a clause quotes it:
    // without the whole-value anchors the model brackets it with.
    let locale_pattern = "[a-z]{2}-[A-Z]{2}";
    let off_pattern = "english";
    let case = "a nested locale outside its pattern and a nested solution type outside its set";

    let mut request = profile_update_task(outside, &[ADMITTED_SEGMENT]);
    request["TaskDetails"]["TranslationSourceLocale"] = json!(off_pattern);
    let (status, body) = emulator.call("StartProfileUpdateTask", &request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: two constraint violations are HTTP 400, got {status}: {body}"
    );
    let message = string(&body, "Message", case);
    assert!(
        message.starts_with("2 validation errors detected: "),
        "case {case:?}: the envelope counts the violations, got {message:?}"
    );
    let pattern_clause = format!(
        "Value '{off_pattern}' at 'taskDetails.translationSourceLocale' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {locale_pattern}"
    );
    assert!(
        message.contains(&pattern_clause),
        "case {case:?}: expected the pattern clause {pattern_clause:?}, got {message:?}"
    );
    let enum_clause = clause_before_the_set(
        "taskDetails.primarySolutionType",
        outside,
        PRIMARY_SOLUTION_TYPE_VALUES,
    );
    assert!(
        message.contains(enum_clause.as_str()),
        "case {case:?}: expected the enum clause {enum_clause:?}, got {message:?}"
    );
}

/// What a clause states belongs to the TYPE, not to the binding the value arrived in: one
/// enumerated type refused through a top-level member and through a member nested in a
/// structure states the same set the same way, with the same value half, the member path
/// the only difference.
#[tokio::test]
async fn one_type_is_stated_alike_through_two_bindings() {
    let emulator = Emulator::new();
    let outside = "NOT_A_MODELLED_VALUE";

    // Both members target `PrimarySolutionType`.
    let bindings: [(&str, &str, Value, &str); 2] = [
        (
            "a top-level member",
            "CreatePartner",
            create_partner(outside, "tok-two-bindings"),
            "primarySolutionType",
        ),
        (
            "a member nested in a structure member",
            "StartProfileUpdateTask",
            start_profile_update_task(outside),
            "taskDetails.primarySolutionType",
        ),
    ];

    let mut stated: Option<(&str, Vec<String>)> = None;
    for (case, operation, request, member_path) in bindings {
        let (status, body) = emulator.call(operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the modelled set is refused, got {status}: {body}"
        );
        let message = string(&body, "Message", case);
        let listed = listed_set(
            &message,
            member_path,
            outside,
            PRIMARY_SOLUTION_TYPE_VALUES,
            case,
        );
        assert_modelled_set_re_ordered(&listed, PRIMARY_SOLUTION_TYPE_VALUES, case);
        match &stated {
            None => stated = Some((case, listed)),
            Some((earlier_case, earlier)) => assert_eq!(
                &listed, earlier,
                "case {case:?}: one type is listed as {earlier_case:?} lists it, {earlier:?}, \
                 got {listed:?}"
            ),
        }
    }
}
