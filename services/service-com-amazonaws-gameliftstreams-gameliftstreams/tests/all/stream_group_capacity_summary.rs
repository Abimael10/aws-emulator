// SPDX-License-Identifier: CC0-1.0

//! The capacity a stream group reports for ITSELF, beside its per-location
//! `LocationStates`.
//!
//! The rule under test: a stream group reports its own capacity as the roll-up of the
//! locations it holds — `DesiredCapacity` is the capacity those locations have most
//! recently requested, `ActiveCapacity` the capacity the service has provisioned
//! across them, and `IdleCapacity` how much of that provisioned capacity is not
//! streaming — and it reports the same three members through every operation that
//! answers with a stream group (`CreateStreamGroup`, `UpdateStreamGroup`,
//! `GetStreamGroup`). The three members are recorded behaviour: this service's
//! on-disk model shipped without them, so they are declared on the stream-group
//! outputs of `smithy-model/com-amazonaws-gameliftstreams-gameliftstreams.json` (see
//! the module docs of `src/ops/get_stream_group.rs`).
//!
//! The table spans the class: the rows differ in how many locations one group holds,
//! in which capacity setting each location is configured through (the always-on
//! minimum, the target-idle level, the maximum bound and the deprecated on-demand
//! bound), and therefore in whether what a location requests is provisioned at all.
//! A further test drives the summary while a stream session is streaming — the one
//! thing that moves `IdleCapacity` without any capacity being reconfigured — and
//! through an `UpdateStreamGroup` that scales a location.
//!
//! Every assertion reads a RESPONSE BODY, and every mutation is read back through
//! `GetStreamGroup`, so a handler that answered 2xx without persisting anything fails
//! here.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::application_read_projection::{poll_until, stream_group_fully_active};
use super::stream_group_capacity::{
    group_with_locations, PLACED_SESSION_STATUSES, TERMINATED_SESSION_STATUSES,
};
use super::stream_workflow::{call, emulator, ready_application, text, REGION};

/// The three members a stream group reports for itself.
const SUMMARY_MEMBERS: [&str; 3] = ["DesiredCapacity", "ActiveCapacity", "IdleCapacity"];

/// The summary a body is expected to carry: requested, provisioned, idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Summary {
    desired: i64,
    active: i64,
    idle: i64,
}

/// Assert `body` reports exactly `expected` as its own capacity, every member of it a
/// number.
fn assert_summary(body: &Value, expected: Summary, case: &str) {
    for member in SUMMARY_MEMBERS {
        assert!(
            body[member].is_number(),
            "case {case:?}: {member} must be reported as a number, got {body}"
        );
    }
    let got = Summary {
        desired: body["DesiredCapacity"].as_i64().unwrap_or(-1),
        active: body["ActiveCapacity"].as_i64().unwrap_or(-1),
        idle: body["IdleCapacity"].as_i64().unwrap_or(-1),
    };
    assert_eq!(
        got, expected,
        "case {case:?}: expected the group to report {expected:?}, got {got:?} in {body}"
    );
}

/// Read a group back and answer its body.
async fn read(router: &MultiServiceRouter, group_id: &str, case: &str) -> Value {
    let (status, body) = call(router, "GET", &format!("/streamgroups/{group_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetStreamGroup 200, got {status} {body}"
    );
    body
}

/// One row: the case, the locations the group is created with, and the summary it
/// reports before and after the clock has provisioned those locations.
struct SummaryCase<'a> {
    case: &'a str,
    configurations: Vec<Value>,
    activating: Summary,
    active: Summary,
}

#[tokio::test]
async fn a_stream_group_reports_the_capacity_of_the_locations_it_holds() {
    let router = emulator();
    let (_, app_arn) = ready_application(&router, "summary-app").await;

    let cases: [SummaryCase; 6] = [
        SummaryCase {
            case: "one location with always-on capacity",
            configurations: vec![json!({"LocationName": REGION, "AlwaysOnCapacity": 2})],
            activating: Summary {
                desired: 2,
                active: 0,
                idle: 0,
            },
            active: Summary {
                desired: 2,
                active: 2,
                idle: 2,
            },
        },
        SummaryCase {
            // A bound alone: the group asks the service to be able to allocate that
            // much, and nothing stands provisioned until a stream asks for it.
            case: "one location with a maximum bound alone",
            configurations: vec![json!({"LocationName": REGION, "MaximumCapacity": 2})],
            activating: Summary {
                desired: 2,
                active: 0,
                idle: 0,
            },
            active: Summary {
                desired: 2,
                active: 0,
                idle: 0,
            },
        },
        SummaryCase {
            case: "one location with the deprecated on-demand bound",
            configurations: vec![json!({"LocationName": REGION, "OnDemandCapacity": 4})],
            activating: Summary {
                desired: 4,
                active: 0,
                idle: 0,
            },
            active: Summary {
                desired: 4,
                active: 0,
                idle: 0,
            },
        },
        SummaryCase {
            case: "one location holding a target idle level under a maximum",
            configurations: vec![json!({
                "LocationName": REGION,
                "MaximumCapacity": 6,
                "TargetIdleCapacity": 2,
            })],
            activating: Summary {
                desired: 6,
                active: 0,
                idle: 0,
            },
            active: Summary {
                desired: 6,
                active: 2,
                idle: 2,
            },
        },
        SummaryCase {
            case: "three locations, each configured differently",
            configurations: vec![
                json!({"LocationName": REGION, "AlwaysOnCapacity": 1}),
                json!({"LocationName": "us-east-1", "MaximumCapacity": 5,
                       "TargetIdleCapacity": 3}),
                json!({"LocationName": "eu-west-1", "MaximumCapacity": 2}),
            ],
            activating: Summary {
                desired: 8,
                active: 0,
                idle: 0,
            },
            active: Summary {
                desired: 8,
                active: 4,
                idle: 4,
            },
        },
        SummaryCase {
            case: "a location that asks for nothing",
            configurations: vec![json!({"LocationName": REGION})],
            activating: Summary {
                desired: 0,
                active: 0,
                idle: 0,
            },
            active: Summary {
                desired: 0,
                active: 0,
                idle: 0,
            },
        },
    ];

    for (index, row) in cases.into_iter().enumerate() {
        let SummaryCase {
            case,
            configurations,
            activating,
            active,
        } = row;
        // The create answers the same projection a read does, over locations that are
        // still being prepared and hold nothing yet. Asserted on the create's own
        // response: a later read can only be made once the clock has provisioned them.
        let (group_id, _, created) = group_with_locations(
            &router,
            &format!("summary-group-{index}"),
            &app_arn,
            &configurations,
        )
        .await;
        assert_summary(&created, activating, case);

        // Once the clock has provisioned the locations — polled for, not waited on —
        // the group reports what they now hold.
        stream_group_fully_active(&router, &group_id, case).await;
        let group = read(&router, &group_id, case).await;
        assert_summary(&group, active, case);

        // Reading a group changes nothing about the capacity a later read sees.
        let again = read(&router, &group_id, case).await;
        assert_summary(&again, active, case);
    }
}

#[tokio::test]
async fn the_summary_follows_a_streaming_session_and_a_capacity_update() {
    let router = emulator();
    let (app_id, app_arn) = ready_application(&router, "summary-session-app").await;
    let (group_id, _, _) = group_with_locations(
        &router,
        "summary-session-group",
        &app_arn,
        &[json!({"LocationName": REGION, "AlwaysOnCapacity": 2})],
    )
    .await;
    // Wait for the CONDITION the summary depends on — the location provisioned —
    // rather than for a fixed time.
    stream_group_fully_active(&router, &group_id, "summary-session-group").await;

    let case = "two always-on streams, none of them streaming";
    assert_summary(
        &read(&router, &group_id, case).await,
        Summary {
            desired: 2,
            active: 2,
            idle: 2,
        },
        case,
    );

    // A session streaming occupies one of the two provisioned streams: the group
    // still has the same capacity requested and provisioned, and half of it idle.
    // "NO_CLIENT_CONNECTION ... will create a session without needing any browser
    // request or Web SDK integration", so the stream is placed and stays placed.
    let (status, session) = call(
        &router,
        "POST",
        &format!("/streamgroups/{group_id}/streamsessions"),
        Some(json!({
            "Protocol": "WebRTC",
            "SignalRequest": "NO_CLIENT_CONNECTION",
            "ApplicationIdentifier": app_id,
            "Locations": [REGION],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"one session streaming\": expected StartStreamSession 201, got {status} \
         {session}"
    );
    let case = "one session streaming";
    let session_id = text(&session, "/Arn", case)
        .rsplit('/')
        .next()
        .expect("a stream session ARN ends in the session id")
        .to_string();
    let session_path = format!("/streamgroups/{group_id}/streamsessions/{session_id}");
    poll_until(&router, &session_path, case, |body| {
        PLACED_SESSION_STATUSES
            .iter()
            .any(|status| body["Status"] == json!(status))
    })
    .await;
    assert_summary(
        &read(&router, &group_id, case).await,
        Summary {
            desired: 2,
            active: 2,
            idle: 1,
        },
        case,
    );

    // Scaling the location through `UpdateStreamGroup`: the update answers the new
    // summary and a read agrees with it.
    let case = "the location scaled to four always-on streams";
    let (status, updated) = call(
        &router,
        "PATCH",
        &format!("/streamgroups/{group_id}"),
        Some(json!({
            "LocationConfigurations": [{"LocationName": REGION, "AlwaysOnCapacity": 4}],
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateStreamGroup 200, got {status} {updated}"
    );
    assert_eq!(
        text(&updated, "/Id", case),
        group_id,
        "case {case:?}: the update answers the group it addressed, got {updated}"
    );
    // The session terminated gives its stream back, so the whole allocation is idle.
    let (status, terminated) = call(&router, "DELETE", &session_path, None).await;
    assert_eq!(
        status, 204,
        "case {case:?}: expected TerminateStreamSession 204, got {status} {terminated}"
    );
    poll_until(&router, &session_path, case, |body| {
        TERMINATED_SESSION_STATUSES
            .iter()
            .any(|status| body["Status"] == json!(status))
    })
    .await;
    let expected = Summary {
        desired: 4,
        active: 4,
        idle: 4,
    };
    assert_summary(&read(&router, &group_id, case).await, expected, case);
}
