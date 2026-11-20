// SPDX-License-Identifier: CC0-1.0

//! What a call of `UpdateMemberDetectors` that configures no member account leaves
//! behind, end to end over the wire.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rule under test, the one the `refused_mutation_clock`, `refused_create_clock`,
//! `no_op_mutation_clock`, `no_op_batch_clock` and `member_batch_no_op_clock` suites
//! assert for the refused calls, the no-op single-record writes and the member
//! batches, here over the per-account CONFIGURATION write and every reason one of its
//! calls writes nothing: a mutating operation takes an instant off the service's
//! monotonic "this resource changed now" clock only once a write has really landed,
//! so a call that leaves every named member exactly as it found it consumes no
//! instant — and the `CreatedAt`/`UpdatedAt` a LATER accepted call stamps therefore
//! still reports the wall-clock instant of that change rather than one pushed into
//! the future by the calls before it.
//!
//! What this suite adds to those: the configuration write is the one member batch
//! whose stored switches are per FEATURE rather than per relationship, it carries two
//! views of the same switches (the `Features` list and the deprecated `DataSources`
//! one), and a switch its administrator's organization auto-enables is not one a
//! per-account write moves at all — so this operation has reasons to write nothing
//! that no other member batch has. Observable per request, three ways:
//!
//! * the clock a mutation reads is what the detector's own `UpdatedAt` records when a
//!   sub-resource is added to it (`smithy_query(operation="GetDetector")`), so a burst
//!   of calls that configure no member between two accepted mutations shows whether
//!   the burst consumed instants: the clock may advance across it only by the
//!   wall-clock time that really passed, whereas a handler reading the clock before it
//!   knows it is going to write spends an instant per call on top of that;
//! * the instant a member's own switch carries once a call that really does move it
//!   follows the burst
//!   (`smithy_query(shape="MemberFeaturesConfigurationResult",member="UpdatedAt")`),
//!   which must be the wall-clock instant of that move;
//! * and what the call ANSWERS before and after the burst, plus what `GetMembers` and
//!   `GetMemberDetectors` report either side of it — so a call that consumed no
//!   instant is also shown to have reported what it owed and stored nothing.
//!
//! The rows differ along what the rule generalises over: the reason the call
//! configures nobody (the request names no configuration, it re-states the switch the
//! account already carries, the switch belongs to the organization, the account has no
//! member record, the administrator has only recorded it, a disassociation already
//! ended its association), the view the configuration is carried in, how many accounts
//! one call names, whether a named id is repeated, and whether the call is accepted at
//! all — `AccountIds` is `len 1..50` on this input
//! (`smithy_query(operation="UpdateMemberDetectors")`), so a request naming no account
//! is refused rather than accepted, and a refusal writes nothing either.

use serde_json::{json, Value};

use crate::detector_lifecycle::{
    enabled_detector, error_code, error_shape, router_with_engine, Router,
};
use crate::member_association::{
    create_members, disassociate_members, get_members, invite_members, unprocessed_ids,
    unprocessed_result, DEFAULT_REGION, MEMBERS, NON_MEMBER_ACCOUNT_IDS, NOT_AN_ASSOCIATED_MEMBER,
};
use crate::member_detector_configuration::update_member_detectors;
use crate::member_organization_auto_enable::{
    feature_instant, feature_status, member_configuration, organization_body, record_member,
};
use crate::member_relationship_scope::monitor_member;
// The measurement — the burst, the detector that must not move across it and the
// accepted create that probes the clock afterwards — lives in the suite that owns the
// rule for the per-record writes, so every suite asserting it measures it one way.
use crate::no_op_batch_clock::a_burst_of_it_consumes_no_instant;
use crate::organization_configuration::update_ok_in;
use crate::refused_create_clock::READING_SLACK_MILLIS;
use service_com_amazonaws_guardduty_guarddutyapiservice::state;

/// Where this operation is bound under `/detector/{DetectorId}`, from its own `http`
/// binding (`smithy_query(operation="UpdateMemberDetectors")`).
const SEGMENT: &str = "member/detector/update";

/// The only client error this service's model binds to the operation, carried in both
/// `x-amzn-errortype` and the body's code
/// (`smithy_query(operation="UpdateMemberDetectors")`).
const MODELED_CLIENT_ERROR: &str = "BadRequestException";

/// The member account each relationship a named account can be in is held by, drawn
/// from the shared member table so this suite cannot drift on what a member is.
const MONITORED: &str = MEMBERS[0].0;
const RECORDED: &str = MEMBERS[1].0;
const DISASSOCIATED: &str = MEMBERS[2].0;

/// Well-formed account ids no `CreateMembers` here records.
const ABSENT: &str = NON_MEMBER_ACCOUNT_IDS[0].0;
const OTHER_ABSENT: &str = NON_MEMBER_ACCOUNT_IDS[1].0;

/// The switch the control write flips: a feature managed per account in every fixture
/// below, so flipping it always moves something.
const CONTROL_FEATURE: &str = "LAMBDA_NETWORK_LOGS";

/// What every call of a row must answer.
enum Answer {
    /// HTTP 200, reporting exactly these accounts in `unprocessedAccounts`, in
    /// request order, each with the fixed reason an account that is not an associated
    /// member of the current account carries.
    Reported(Vec<&'static str>),
    /// HTTP 400 under the modelled client error, carrying `message`.
    Refused(&'static str),
}

/// One row: the writes that put the members into the state the burst then asks for,
/// the accounts the burst names, the configuration it carries and what it answers.
struct Row {
    case: &'static str,
    /// The calls that run ONCE before the measurement, each the accounts it names and
    /// the configuration it carries, so the burst re-states what the store already
    /// holds. These DO write, which is what makes the burst a repeat.
    prepare: Vec<(Vec<&'static str>, Value)>,
    accounts: Vec<&'static str>,
    /// The configuration members the request carries beside `accountIds` — none, one
    /// view, or both.
    configuration: Value,
    answer: Answer,
}

/// The `Features` body naming one switch, the current view of the configuration.
fn features(name: &str, status: &str) -> Value {
    json!({"features": [{"name": name, "status": status}]})
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an account with no member record, the Features view naming a switch",
            prepare: Vec::new(),
            accounts: vec![ABSENT],
            configuration: features("S3_DATA_EVENTS", "ENABLED"),
            answer: Answer::Reported(vec![ABSENT]),
        },
        Row {
            case: "two accounts with no member record, the deprecated dataSources view",
            prepare: Vec::new(),
            accounts: vec![ABSENT, OTHER_ABSENT],
            configuration: json!({"dataSources": {"s3Logs": {"enable": false}}}),
            answer: Answer::Reported(vec![ABSENT, OTHER_ABSENT]),
        },
        Row {
            case: "the same account with no member record named twice in one call",
            prepare: Vec::new(),
            accounts: vec![ABSENT, ABSENT],
            configuration: features("EKS_AUDIT_LOGS", "DISABLED"),
            // `AccountIds` names a SET, so a repeated id is reported once.
            answer: Answer::Reported(vec![ABSENT]),
        },
        Row {
            case: "an account the administrator has only recorded",
            prepare: Vec::new(),
            accounts: vec![RECORDED],
            configuration: features("EKS_AUDIT_LOGS", "DISABLED"),
            answer: Answer::Reported(vec![RECORDED]),
        },
        Row {
            case: "an account whose association a disassociation already ended",
            prepare: Vec::new(),
            accounts: vec![DISASSOCIATED],
            configuration: features("RDS_LOGIN_EVENTS", "DISABLED"),
            answer: Answer::Reported(vec![DISASSOCIATED]),
        },
        Row {
            // Both configuration members are OPTIONAL
            // (`smithy_query(operation="UpdateMemberDetectors")`), so a request that
            // carries neither names no switch to write.
            case: "a monitored member beside an account with no record, carrying no configuration",
            prepare: Vec::new(),
            accounts: vec![MONITORED, ABSENT],
            configuration: json!({}),
            answer: Answer::Reported(vec![ABSENT]),
        },
        Row {
            case: "a monitored member, re-stating the switch it already carries",
            prepare: vec![(vec![MONITORED], features("EKS_AUDIT_LOGS", "DISABLED"))],
            accounts: vec![MONITORED],
            configuration: features("EKS_AUDIT_LOGS", "DISABLED"),
            answer: Answer::Reported(Vec::new()),
        },
        Row {
            case: "a monitored member, re-stating a switch it carries through the dataSources view",
            prepare: vec![(
                vec![MONITORED],
                json!({"dataSources": {"kubernetes": {"auditLogs": {"enable": false}}}}),
            )],
            accounts: vec![MONITORED],
            configuration: json!({"dataSources": {"kubernetes": {"auditLogs": {"enable": false}}}}),
            answer: Answer::Reported(Vec::new()),
        },
        Row {
            case: "every relationship in one call, re-stating both views at once",
            prepare: vec![(
                vec![MONITORED, RECORDED, DISASSOCIATED, ABSENT],
                json!({
                    "features": [{"name": "S3_DATA_EVENTS", "status": "DISABLED"}],
                    "dataSources": {"kubernetes": {"auditLogs": {"enable": false}}},
                }),
            )],
            accounts: vec![MONITORED, RECORDED, DISASSOCIATED, ABSENT],
            configuration: json!({
                "features": [{"name": "S3_DATA_EVENTS", "status": "DISABLED"}],
                "dataSources": {"kubernetes": {"auditLogs": {"enable": false}}},
            }),
            answer: Answer::Reported(vec![RECORDED, DISASSOCIATED, ABSENT]),
        },
        Row {
            // `AccountIds` is `len 1..50` on this input, so naming no account is a
            // request outside a constraint the model declares rather than a legal
            // batch of nothing — refused, and a refusal writes nothing either.
            case: "a request naming no account at all",
            prepare: Vec::new(),
            accounts: Vec::new(),
            configuration: features("S3_DATA_EVENTS", "DISABLED"),
            answer: Answer::Refused(state::INVALID_INPUT_PARAMETERS_MESSAGE),
        },
    ]
}

/// The body one call sends: the accounts it names plus the configuration members it
/// carries, built the way [`update_member_detectors`] builds it so the burst and the
/// single calls either side of it send exactly the same request.
fn body_of(accounts: &[&str], configuration: &Value) -> Value {
    let mut body = json!({"accountIds": accounts});
    for (member, value) in configuration.as_object().into_iter().flatten() {
        body[member.as_str()] = value.clone();
    }
    body
}

/// An enabled detector holding one member account in each relationship a named
/// account can be in: MONITORED under the administrator, recorded and not invited
/// yet, and disassociated.
///
/// The monitored relationship is reached when the member ACCOUNT answers the
/// invitation, which is that account's own call to make: it goes through the one
/// state seam the `member_relationship_scope` suite owns, and everything else here
/// goes over the wire.
async fn detector_with_each_relationship(case: &str) -> (Router, String) {
    let (router, engine) = router_with_engine();
    let detector_id = enabled_detector(&router).await;
    let (status, recorded) = create_members(&router, &detector_id, &MEMBERS).await;
    assert_eq!(
        status, 200,
        "case {case:?}: create-members expected 200, got {status} {recorded:?}"
    );
    let (status, invited) = invite_members(&router, &detector_id, &[MONITORED, DISASSOCIATED]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: invite-members expected 200, got {status} {invited:?}"
    );
    monitor_member(&engine, &detector_id, MONITORED);
    let (status, ended) = disassociate_members(&router, &detector_id, &[DISASSOCIATED]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: disassociate-members expected 200, got {status} {ended:?}"
    );
    assert!(
        unprocessed_ids(&ended).is_empty(),
        "case {case:?}: expected the disassociation to process the account, got {ended:?}"
    );
    (router, detector_id)
}

/// Send one call of a row and assert what the rule says it answers — `what` names
/// which call of the row it is, so a failure says whether the answer differed before
/// or after the burst.
async fn assert_answer(
    router: &Router,
    detector_id: &str,
    row: &Row,
    what: &str,
) {
    let case = row.case;
    let (status, answered) = update_member_detectors(
        router,
        detector_id,
        &row.accounts,
        row.configuration.clone(),
    )
    .await;
    match &row.answer {
        Answer::Reported(reported) => {
            assert_eq!(
                status, 200,
                "case {case:?}: {what} expected 200, got {status} {answered:?}"
            );
            let expected: Vec<String> = reported
                .iter()
                .map(|account_id| (*account_id).to_string())
                .collect();
            assert_eq!(
                unprocessed_ids(&answered),
                expected,
                "case {case:?}: {what} must report each account it could not process once, in \
                 request order, expected {expected:?}, got {answered:?}"
            );
            for account_id in reported {
                let result = unprocessed_result(&answered, account_id);
                assert_eq!(
                    result.as_deref(),
                    Some(NOT_AN_ASSOCIATED_MEMBER),
                    "case {case:?}: {what} must report {account_id} with the fixed reason an \
                     account that is not an associated member carries, got {result:?}"
                );
            }
        }
        Answer::Refused(message) => {
            assert_eq!(
                status, 400,
                "case {case:?}: {what} expected 400, got {status} {answered:?}"
            );
            let shape = error_shape(&answered);
            assert_eq!(
                shape, MODELED_CLIENT_ERROR,
                "case {case:?}: {what} expected the modelled error shape, got {shape:?} in \
                 {answered:?}"
            );
            let code = error_code(&answered);
            assert_eq!(
                code, MODELED_CLIENT_ERROR,
                "case {case:?}: {what} expected the modelled error code, got {code:?} in \
                 {answered:?}"
            );
            let got = answered["message"].as_str().unwrap_or_default();
            assert_eq!(
                got, *message,
                "case {case:?}: {what} expected the fixed text of this refusal class, got \
                 {got:?}"
            );
        }
    }
}

/// What a later caller sees of the members and of the monitored member's stored
/// configuration: two of these, either side of a burst, is what makes "the burst
/// wrote nothing" an assertion about the store rather than only about a timestamp.
async fn stored(router: &Router, detector_id: &str, case: &str) -> (Value, Value) {
    let (status, members) = get_members(
        router,
        detector_id,
        &[MONITORED, RECORDED, DISASSOCIATED],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: get-members expected 200, got {status} {members:?}"
    );
    let configuration = member_configuration(router, detector_id, MONITORED, case).await;
    (members, configuration)
}

/// The epoch milliseconds a reported switch instant names
/// (`MemberFeaturesConfigurationResult.UpdatedAt` is a `Timestamp`, so the wire
/// carries epoch seconds), so it can be compared with a wall-clock reading.
fn instant_millis(configuration: &Value, feature: &str, case: &str) -> i64 {
    (feature_instant(configuration, feature, case) * 1000.0).round() as i64
}

/// The other value of the `FeatureStatus` enum (`smithy_query(shape="FeatureStatus")`).
fn opposite(status: &str) -> &'static str {
    if status == "ENABLED" {
        "DISABLED"
    } else {
        "ENABLED"
    }
}

/// The control: a call of this same operation that DOES move the monitored member
/// flips one of its switches, and the instant that switch then carries is the
/// WALL-CLOCK instant of the move rather than one the burst spent ahead of it.
async fn a_write_that_lands_stamps_a_wall_clock_instant(
    router: &Router,
    detector_id: &str,
    feature: &str,
    case: &str,
) {
    let before = member_configuration(router, detector_id, MONITORED, case).await;
    let was = feature_status(&before, feature, case);
    let was_at = instant_millis(&before, feature, case);
    let wanted = opposite(&was);

    let (status, moved) = update_member_detectors(
        router,
        detector_id,
        &[MONITORED],
        features(feature, wanted),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the moving call expected 200, got {status} {moved:?}"
    );
    assert!(
        unprocessed_ids(&moved).is_empty(),
        "case {case:?}: the moving call must process the monitored member, got {moved:?}"
    );
    let wall_at_move = state::now_millis();

    let after = member_configuration(router, detector_id, MONITORED, case).await;
    let got = feature_status(&after, feature, case);
    assert_eq!(
        got, wanted,
        "case {case:?}: the moving call must leave {feature} {wanted}, got {got} in {after:?}"
    );
    let stamp = instant_millis(&after, feature, case);
    assert!(
        stamp > was_at,
        "case {case:?}: {feature} moved {was} -> {got}, so its instant must move past \
         {was_at} ms, got {stamp} ms"
    );
    assert!(
        stamp <= wall_at_move + READING_SLACK_MILLIS,
        "case {case:?}: the moving call must stamp {feature} with the wall-clock instant of \
         the move, so {stamp} ms may not be more than {READING_SLACK_MILLIS} ms past the \
         {wall_at_move} ms the wall clock read just after it"
    );
}

// ---------------------------------------------------------------------------
// The rule: a configuration call that moves no member consumes no instant, so the
// next accepted mutation still stamps a wall-clock instant.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn member_configuration_calls_that_move_no_member_leave_the_clock_where_the_wall_clock_is() {
    for row in rows() {
        let case = row.case;
        // A router and a detector per row, so a row cannot pass or fail on another
        // row's writes: there is one detector per account per Region.
        let (router, detector_id) = detector_with_each_relationship(case).await;
        for (accounts, configuration) in &row.prepare {
            let (status, prepared) =
                update_member_detectors(&router, &detector_id, accounts, configuration.clone())
                    .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the preparing call expected 200, got {status} {prepared:?}"
            );
        }

        // What the call answers, and what a later caller sees, before the burst.
        assert_answer(&router, &detector_id, &row, "the call before the burst").await;
        let before = stored(&router, &detector_id, case).await;

        let path = format!("/detector/{detector_id}/{SEGMENT}");
        let body = body_of(&row.accounts, &row.configuration);
        let expected_status = match &row.answer {
            Answer::Reported(_) => 200,
            Answer::Refused(_) => 400,
        };
        a_burst_of_it_consumes_no_instant(
            &router,
            &detector_id,
            case,
            &path,
            &body,
            expected_status,
        )
        .await;

        // A call that moves no member leaves every member record, and every stored
        // switch of the member whose detector the administrator maintains, exactly as
        // it was.
        let after = stored(&router, &detector_id, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: a burst of calls that move no member must leave every member and \
             every stored switch exactly as it was, expected {before:?}, got {after:?}"
        );
        // And it still answers exactly what it owed.
        assert_answer(&router, &detector_id, &row, "the call after the burst").await;

        a_write_that_lands_stamps_a_wall_clock_instant(
            &router,
            &detector_id,
            CONTROL_FEATURE,
            case,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// The same rule for the one reason to write nothing only this operation has: the
// switch the request names belongs to the ORGANIZATION.
// ---------------------------------------------------------------------------

/// The organization settings an organization row configures, as
/// `(feature, autoEnable)`: a switch auto-enabled for any account at all is the
/// organization's, whichever `OrgFeatureStatus` says so
/// (`smithy_query(shape="OrgFeatureStatus")` declares NEW, NONE and ALL), so the rows
/// span both statuses that auto-enable one and three features of the `OrgFeature`
/// catalog.
const ORGANIZATION_OWNED: [(&str, &str); 4] = [
    ("S3_DATA_EVENTS", "ALL"),
    ("S3_DATA_EVENTS", "NEW"),
    ("EKS_AUDIT_LOGS", "ALL"),
    ("EBS_MALWARE_PROTECTION", "NEW"),
];

#[tokio::test]
async fn a_member_write_of_a_switch_the_organization_owns_consumes_no_instant() {
    for (feature, auto_enable) in ORGANIZATION_OWNED {
        // One organization, one detector and one member account per row.
        let (router, engine) = router_with_engine();
        let detector_id = enabled_detector(&router).await;
        let case = format!("{feature} auto-enabled {auto_enable}, member write DISABLED");
        update_ok_in(
            &router,
            DEFAULT_REGION,
            &detector_id,
            organization_body(&[(feature, auto_enable)]),
        )
        .await;
        record_member(
            &engine,
            &router,
            &detector_id,
            MONITORED,
            MEMBERS[0].1,
            &case,
        )
        .await;

        // The first write of the organization's switch: it brings the account into
        // agreement with the organization if anything was still owed, so the burst
        // that follows asks for exactly what the store already holds.
        let configuration = features(feature, "DISABLED");
        let (status, prepared) = update_member_detectors(
            &router,
            &detector_id,
            &[MONITORED],
            configuration.clone(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the preparing call expected 200, got {status} {prepared:?}"
        );

        let before = member_configuration(&router, &detector_id, MONITORED, &case).await;
        // The discriminating fact: the request asks for the switch OFF and the
        // organization auto-enables it, so the account carries it ENABLED and the
        // request moves nothing.
        let was = feature_status(&before, feature, &case);
        assert_eq!(
            was, "ENABLED",
            "case {case:?}: the organization auto-enables {feature}, so the account must carry \
             it ENABLED whatever the write asked for, got {was} in {before:?}"
        );

        let path = format!("/detector/{detector_id}/{SEGMENT}");
        let body = body_of(&[MONITORED], &configuration);
        a_burst_of_it_consumes_no_instant(&router, &detector_id, &case, &path, &body, 200).await;

        let after = member_configuration(&router, &detector_id, MONITORED, &case).await;
        assert_eq!(
            after, before,
            "case {case:?}: a burst of writes of a switch the organization owns must leave \
             every stored switch exactly as it was, expected {before:?}, got {after:?}"
        );

        a_write_that_lands_stamps_a_wall_clock_instant(
            &router,
            &detector_id,
            CONTROL_FEATURE,
            &case,
        )
        .await;
    }
}
