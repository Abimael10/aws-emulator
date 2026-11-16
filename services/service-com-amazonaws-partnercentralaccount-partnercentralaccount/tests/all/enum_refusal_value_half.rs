// SPDX-License-Identifier: CC0-1.0

//! What an enum-constraint refusal states about the value it REFUSED: that value.
//!
//! The rule under test: a constraint-violation clause quotes the value the request carried
//! for the member it refuses — `Value '<value>' at '<member path>' failed to satisfy
//! constraint: <requirement>` — whatever constraint class the value broke, so the enum
//! class reads exactly like the `@length`, `@range` and `@pattern` classes of the same
//! message. The clause therefore FOLLOWS the request: two requests refusing the same member
//! for two different values state two different messages, and a client that reads the quoted
//! value out of a clause finds one for every clause of the message.
//!
//! The rows differ along what the rule generalises over: which enumerated type carried the
//! refused value (six of this model's, of two, three, five and eight values), which
//! operation and member carried it (nine members over seven operations, one of them nested
//! inside a structure member), how the refused value was spelled (a name no value carries,
//! an admitted value miscased, padded, the empty string, a long value, a value carrying a
//! quote, a value carrying the set's own text, an admitted value of another type), and
//! whether the request breaks the enum alone or an enum together with another class. The
//! value SET's order is not pinned here — it is `enum_value_set_order.rs`'s rule; this suite
//! reads the set out of the message and holds it to being the modelled set re-ordered.

use serde_json::{json, Value};

use service_com_amazonaws_partnercentralaccount_partnercentralaccount::ops::types::{
    ACCESS_TYPE_VALUES, CONNECTION_TYPE_VALUES, INVITATION_STATUS_VALUES, PARTICIPANT_TYPE_VALUES,
    PRIMARY_SOLUTION_TYPE_VALUES, PROFILE_VISIBILITY_VALUES,
};

use crate::connection_lifecycle::{error_code, string, Emulator, RECEIVER_PROFILE};
use crate::enum_value_set_order::create_partner;

/// `ClientToken`'s modelled maximum (`@length(1, 64)`, source
/// `smithy_query(query="operation",operation="CreatePartner")`).
const CLIENT_TOKEN_MAX: usize = 64;

/// Everything a clause states before the value set it lists: the envelope's count, the
/// value the request carried, the member path and the requirement.
fn clause_opening(refused: &str, member_path: &str) -> String {
    format!(
        "1 validation error detected: Value '{refused}' at '{member_path}' failed to satisfy \
         constraint: Member must satisfy enum value set: ["
    )
}

/// A `StartProfileUpdateTask` request valid but for `solution_type`, which sits on the
/// nested `TaskDetails` member (source:
/// `smithy_query(query="shape",shape="TaskDetails")`).
fn start_profile_update_task(solution_type: &str) -> Value {
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
            "IndustrySegments": ["RETAIL"],
            "TranslationSourceLocale": "en-US",
        },
    })
}

/// Every enum-constrained input member of this service, each refused a value outside its
/// type: one validation error, quoting the value the request carried, naming the member and
/// the set it had to come from, and the set it lists is the modelled set re-ordered.
#[tokio::test]
async fn an_enum_refusal_quotes_the_value_it_refused() {
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
        let listed = listed_set(&message, outside, member_path, case);

        let mut sorted_listed = listed.clone();
        sorted_listed.sort_unstable();
        let mut sorted_modelled: Vec<String> =
            modelled.iter().map(|value| (*value).to_string()).collect();
        sorted_modelled.sort_unstable();
        assert_eq!(
            sorted_listed, sorted_modelled,
            "case {case:?}: the listed set is the modelled set re-ordered, got {listed:?}"
        );
    }
}

/// The values a refusal lists for `member_path` after quoting `refused`, with the whole
/// clause around them held to the wording the rule states — so a clause that drops the
/// value half, or quotes another value, fails here.
fn listed_set(message: &str, refused: &str, member_path: &str, case: &str) -> Vec<String> {
    let opening = clause_opening(refused, member_path);
    let listed = message
        .strip_prefix(opening.as_str())
        .and_then(|tail| tail.strip_suffix(']'))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {opening:?} then the set, got {message:?}")
        });
    listed.split(", ").map(str::to_string).collect()
}

/// The quoted value is the one the REQUEST sent: one member refused every spelling of an
/// inadmissible value a caller can send quotes that spelling back, so no two of these
/// messages are the same.
#[tokio::test]
async fn the_quoted_value_follows_what_the_request_sent() {
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

    let mut seen: Vec<(&str, String)> = Vec::new();
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
        // The clause quotes this spelling and no other: `listed_set` holds the whole
        // opening of the message, the quoted value included.
        listed_set(&message, refused, "status", case);
        for (earlier_case, earlier) in &seen {
            assert_ne!(
                &message, earlier,
                "case {case:?}: a clause following the value cannot read the same as \
                 {earlier_case:?}'s, got {message:?}"
            );
        }
        seen.push((case, message));
    }
}

/// The value half belongs to the CLASS-independent half of a clause: a request that breaks
/// an enum and a `@length` maximum together quotes the over-long value it sent in one clause
/// and the inadmissible one in the other, and the message carries no clause without a value.
#[tokio::test]
async fn each_class_in_the_same_request_quotes_its_own_value() {
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
    let enum_clause = format!(
        "Value '{refused}' at 'primarySolutionType' failed to satisfy constraint: Member must \
         satisfy enum value set: ["
    );
    assert!(
        message.contains(&enum_clause),
        "case {case:?}: expected the enum clause {enum_clause:?}, got {message:?}"
    );
    assert!(
        !message.contains("Value at '"),
        "case {case:?}: every clause quotes the value it refused, got {message:?}"
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
