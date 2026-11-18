// SPDX-License-Identifier: CC0-1.0

//! What an enum-constraint refusal states about the value it REFUSED: nothing.
//!
//! The rule under test: a clause refusing a value outside an enumerated type states the
//! member and the set that value had to come from and WITHHOLDS the value itself — `Value
//! at '<member path>' failed to satisfy constraint: Member must satisfy enum value set:
//! [ … ]` — where a `@length` clause of the very same message still quotes the value it
//! refused. The rule is keyed on the constraint CLASS, so it holds for every member
//! targeting any enumerated type of this model, at whatever depth and in whatever binding
//! the value arrived; and because the clause says nothing of the request, two requests
//! refusing one member for two different inadmissible values state the SAME message.
//!
//! The rows differ along what the rule generalises over: which enumerated type carried the
//! refused value (seven of this model's, of two, three, five, eight and thirty-seven
//! values), which operation and member carried it (nine members over seven operations, one
//! of them nested inside a structure member), the BINDING the refused value arrived in (a
//! top-level member, a member nested in a structure member, and an element of a list of an
//! enumerated type, at the list's first, middle and last admissible position), how the
//! refused value was spelled (a name no value carries, an admitted value miscased, padded,
//! the empty string, a long value, a value carrying a quote, a value carrying the set's own
//! text, an admitted value of another type), and how many constraint violations one request
//! carries (one; an enum together with another class; and two enum violations, of one type
//! and of two). The value SET's order is not pinned here — it is `enum_value_set_order.rs`'s
//! rule; this suite reads the set out of the message and holds it to being the modelled set
//! re-ordered.

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

/// Everything ONE clause states before the value set it lists: the member path and the
/// requirement — and no value.
fn clause_before_the_set(member_path: &str) -> String {
    format!(
        "Value at '{member_path}' failed to satisfy constraint: Member must satisfy enum \
         value set: ["
    )
}

/// Everything a message stating ONE clause says before the value set: the envelope's
/// count, then the clause.
fn clause_opening(member_path: &str) -> String {
    format!(
        "1 validation error detected: {}",
        clause_before_the_set(member_path)
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
fn segments_refused_at(position: usize) -> Vec<&'static str> {
    (1..=INDUSTRY_SEGMENTS_MAX)
        .map(|index| {
            if index == position {
                "NOT_A_MODELLED_VALUE"
            } else {
                ADMITTED_SEGMENT
            }
        })
        .collect()
}

/// Every enum-constrained input member of this service, each refused a value outside its
/// type: one validation error, naming the member and the set it had to come from, quoting
/// no value at all, and the set it lists is the modelled set re-ordered.
#[tokio::test]
async fn an_enum_refusal_withholds_the_value_it_refused() {
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
        assert!(
            !message.contains(outside),
            "case {case:?}: an enum clause states no value, got {message:?}"
        );
        let listed = listed_set(&message, member_path, case);
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
/// the wording the rule states — so a clause that quotes the value it refused, at the member
/// path or anywhere before it, fails here.
fn listed_set(message: &str, member_path: &str, case: &str) -> Vec<String> {
    let opening = clause_opening(member_path);
    let listed = message
        .strip_prefix(opening.as_str())
        .and_then(|tail| tail.strip_suffix(']'))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {opening:?} then the set, got {message:?}")
        });
    listed.split(", ").map(str::to_string).collect()
}

/// The clause does NOT follow what the request sent: one member refused every spelling of
/// an inadmissible value a caller can send states one and the same message, so nothing of
/// the submitted value can be read back out of it.
#[tokio::test]
async fn the_clause_does_not_follow_what_the_request_sent() {
    let emulator = Emulator::new();

    // A long value, under the ceiling the framework's own walk elides a clause's echo at:
    // past it the clause quotes a prefix and the value's byte count, which is the
    // framework's rendering of a long value rather than this rule.
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
        listed_set(&message, "status", case);
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

/// The value half is decided per CLASS: a request that breaks an enum and a `@length`
/// maximum together quotes the over-long value it sent in the length clause and states no
/// value in the enum clause.
#[tokio::test]
async fn each_class_in_the_same_request_states_its_own_value_half() {
    let emulator = Emulator::new();
    let over_long_token = "t".repeat(CLIENT_TOKEN_MAX + 1);
    let refused = "NOT_A_SOLUTION_TYPE";
    let case = "an over-long client token and a solution type outside its set";

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
    let enum_clause = "Value at 'primarySolutionType' failed to satisfy constraint: Member must \
                       satisfy enum value set: [";
    assert!(
        message.contains(enum_clause),
        "case {case:?}: expected the enum clause {enum_clause:?}, got {message:?}"
    );
    assert!(
        !message.contains(refused),
        "case {case:?}: the enum clause states no value, got {message:?}"
    );
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
/// listed and no value stated — whichever position of the list carried it.
#[tokio::test]
async fn an_element_of_a_list_of_the_type_states_no_value_either() {
    let emulator = Emulator::new();
    let outside = "NOT_A_MODELLED_VALUE";

    // One row per position the list admits: first, middle and last.
    for position in 1..=INDUSTRY_SEGMENTS_MAX {
        let case = format!("the industry segment at position {position} of {INDUSTRY_SEGMENTS_MAX}");
        let case = case.as_str();
        let (status, body) = emulator
            .call(
                "StartProfileUpdateTask",
                &profile_update_task("SOFTWARE_PRODUCTS", &segments_refused_at(position)),
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
        assert!(
            !message.contains(outside),
            "case {case:?}: an enum clause states no value, got {message:?}"
        );
        let member_path = format!("taskDetails.industrySegments.{position}.member");
        let listed = listed_set(&message, &member_path, case);
        assert_modelled_set_re_ordered(&listed, INDUSTRY_SEGMENT_VALUES, case);
    }
}

/// One row of the two-violation table: the case, the operation, the request, and the two
/// member paths each with the value set its clause must list.
type TwoEnumViolations = (
    &'static str,
    &'static str,
    Value,
    [(String, &'static [&'static str]); 2],
);

/// TWO members outside their enumerated types in one request: the envelope counts both,
/// each clause names its own member path and set, and neither states a value — whether the
/// two members reach two enumerated types or one type at two positions of a list.
#[tokio::test]
async fn two_enum_violations_each_state_their_own_member_and_no_value() {
    let emulator = Emulator::new();
    let outside = "NOT_A_MODELLED_VALUE";

    let rows: Vec<TwoEnumViolations> = vec![
        (
            "two filters of two different enumerated types",
            "ListConnectionInvitations",
            json!({ "Catalog": "AWS", "Status": outside, "ParticipantType": outside }),
            [
                ("status".to_string(), INVITATION_STATUS_VALUES),
                ("participantType".to_string(), PARTICIPANT_TYPE_VALUES),
            ],
        ),
        (
            "two elements of one list, so one type at two positions",
            "StartProfileUpdateTask",
            profile_update_task("SOFTWARE_PRODUCTS", &[ADMITTED_SEGMENT, outside, outside]),
            [
                (
                    "taskDetails.industrySegments.2.member".to_string(),
                    INDUSTRY_SEGMENT_VALUES,
                ),
                (
                    "taskDetails.industrySegments.3.member".to_string(),
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
        assert!(
            !message.contains(outside),
            "case {case:?}: an enum clause states no value, got {message:?}"
        );
        for (member_path, modelled) in expected {
            // The clause is read out of a message holding two of them, so only this
            // clause's own wording — path, requirement and the absent value half — is
            // held here.
            let clause = clause_before_the_set(&member_path);
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

/// The value half is decided per CLASS wherever the member sits: a nested member outside
/// its `@pattern` echoes the value it refused in the same message whose enum clause states
/// none.
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
    let enum_clause = clause_before_the_set("taskDetails.primarySolutionType");
    assert!(
        message.contains(enum_clause.as_str()),
        "case {case:?}: expected the enum clause {enum_clause:?}, got {message:?}"
    );
    assert!(
        !message.contains(outside),
        "case {case:?}: the enum clause states no value, got {message:?}"
    );
}

/// What a clause states belongs to the CLASS and the TYPE, not to the binding the value
/// arrived in: one enumerated type refused through a top-level member and through a member
/// nested in a structure states the same set the same way, the member path the only
/// difference.
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
        let listed = listed_set(&message, member_path, case);
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
