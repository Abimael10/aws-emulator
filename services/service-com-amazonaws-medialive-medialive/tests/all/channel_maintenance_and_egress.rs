// SPDX-License-Identifier: CC0-1.0

//! The two things the service SELECTS for every channel — its maintenance
//! window and the public address each of its pipelines egresses from — end to
//! end over the restJson1 wire.
//!
//! Every test drives the service the way a client does — through
//! `MultiServiceRouter::dispatch` with real HTTP requests — so what is asserted
//! is the emulator's wire behaviour, not an internal function's return value.
//!
//! The suites are tables: a channel created with a maintenance block, and a
//! two-pipeline channel, are ONE row of each.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_lifecycle::{describe, described_once_in, wait_for_the_clock};
use crate::wire::{call, medialive};

/// Every value `MaintenanceDay` declares, in the model's own order
/// (`smithy_query(query="shape", shape="MaintenanceDay")`: MONDAY | TUESDAY |
/// WEDNESDAY | THURSDAY | FRIDAY | SATURDAY | SUNDAY), which is the set a
/// service-selected maintenance day must come from.
const MAINTENANCE_DAYS: [&str; 7] = [
    "MONDAY",
    "TUESDAY",
    "WEDNESDAY",
    "THURSDAY",
    "FRIDAY",
    "SATURDAY",
    "SUNDAY",
];

async fn create(router: &MultiServiceRouter, body: Value) -> Value {
    let body = crate::support::resolved_channel_body(router, body).await;
    let (status, response) = call(router, "POST", "/prod/channels", Some(body)).await;
    assert_eq!(
        status, 201,
        "CreateChannel: expected 201, got {status}: {response}"
    );
    response["channel"].clone()
}

/// The `ChannelSummary` `ListChannels` reports for `id`.
async fn summary(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, listed) = call(router, "GET", "/prod/channels", None).await;
    assert_eq!(
        status, 200,
        "ListChannels: expected 200, got {status}: {listed}"
    );
    listed["channels"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|channel| channel["id"] == json!(id))
        .unwrap_or(Value::Null)
}

/// The `maintenance` member of one channel-shaped body, as a client reads it.
fn maintenance(body: &Value) -> Value {
    body["maintenance"].clone()
}

/// The `sourceIp` of every `egressEndpoints` entry of one channel-shaped body,
/// in the order the body reports them.
fn source_ips(body: &Value) -> Vec<String> {
    body["egressEndpoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|endpoint| {
            endpoint["sourceIp"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// A complete, well-formed maintenance window: a day the enum declares and a
/// whole hour of the day ("Choose the hour that maintenance will start"), with
/// no deadline or scheduled date, since no maintenance has been scheduled for
/// or required of the channel.
fn assert_window_is_well_formed(case: &str, read: &str, window: &Value) {
    let day = window["maintenanceDay"].as_str().unwrap_or_default();
    assert!(
        MAINTENANCE_DAYS.contains(&day),
        "case {case:?}: {read}: expected a maintenanceDay from {MAINTENANCE_DAYS:?}, got {:?}",
        window["maintenanceDay"]
    );
    let start = window["maintenanceStartTime"].as_str().unwrap_or_default();
    let hour = start.strip_suffix(":00").unwrap_or_default();
    assert!(
        hour.len() == 2
            && hour.chars().all(|digit| digit.is_ascii_digit())
            && hour.parse::<u32>().is_ok_and(|hour| hour < 24),
        "case {case:?}: {read}: expected a maintenanceStartTime naming a whole hour (HH:00), got {:?}",
        window["maintenanceStartTime"]
    );
    assert!(
        window["maintenanceDeadline"].is_null() && window["maintenanceScheduledDate"].is_null(),
        "case {case:?}: {read}: expected no maintenance to be scheduled or required yet, got {window:?}"
    );
}

/// One row of the maintenance-window table: the case, the `maintenance` the
/// create request names, and the day and the start time that request chose
/// (`None` where it left the choice to the service).
type WindowRow = (&'static str, Option<Value>, Option<&'static str>, Option<&'static str>);

/// Rule: the service selects a maintenance window for EVERY channel, so every
/// channel-shaped response reports a `maintenance` status carrying the currently
/// selected day and start time — the request's `MaintenanceCreateSettings` only
/// chooses them member by member ("Choose one day of the week for maintenance",
/// "Choose the hour that maintenance will start"), and the window is the same on
/// every later read.
///
/// The rows differ along which members the create request chooses: neither (the
/// case the issue reports), the day alone, the start time alone, both, and an
/// empty settings object. The chosen days and hours are entries of the day enum
/// and of the hours of the day that the issue did not cite, including both
/// published bounds of the hour (`00:00` and `23:00`).
#[tokio::test]
async fn every_channel_reports_the_maintenance_window_the_service_selected() {
    // (case, the `maintenance` the request names, the day it chose, the start
    //  time it chose)
    let table: [WindowRow; 6] = [
        ("no maintenance member", None, None, None),
        ("an empty maintenance object", Some(json!({})), None, None),
        (
            "the day alone",
            Some(json!({ "maintenanceDay": "SUNDAY" })),
            Some("SUNDAY"),
            None,
        ),
        (
            "the start time alone, at the first hour of the day",
            Some(json!({ "maintenanceStartTime": "00:00" })),
            None,
            Some("00:00"),
        ),
        (
            "both, at the last hour of the day",
            Some(json!({ "maintenanceDay": "SATURDAY", "maintenanceStartTime": "23:00" })),
            Some("SATURDAY"),
            Some("23:00"),
        ),
        (
            "both, mid-week and mid-afternoon",
            Some(json!({ "maintenanceDay": "WEDNESDAY", "maintenanceStartTime": "15:00" })),
            Some("WEDNESDAY"),
            Some("15:00"),
        ),
    ];

    for (case, requested, chosen_day, chosen_start) in table {
        let router = medialive();
        let mut body = json!({ "name": format!("channel-{case}") });
        if let Some(requested) = requested {
            body["maintenance"] = requested;
        }

        let created = create(&router, body).await;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        let window = maintenance(&created);
        assert_window_is_well_formed(case, "CreateChannel", &window);

        // What the request chose is what the channel reports; what it left to
        // the service is still selected.
        if let Some(day) = chosen_day {
            assert_eq!(
                window["maintenanceDay"],
                json!(day),
                "case {case:?}: expected the chosen maintenanceDay {day}, got {:?}",
                window["maintenanceDay"]
            );
        }
        if let Some(start) = chosen_start {
            assert_eq!(
                window["maintenanceStartTime"],
                json!(start),
                "case {case:?}: expected the chosen maintenanceStartTime {start}, got {:?}",
                window["maintenanceStartTime"]
            );
        }

        // The selected window is the channel's, not one answer's: it reads back
        // the same through the describe and through the list summary.
        wait_for_the_clock();
        let (status, described) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel: got {status}: {described}"
        );
        assert_eq!(
            maintenance(&described),
            window,
            "case {case:?}: expected DescribeChannel to report the selected window {window:?}, got {:?}",
            maintenance(&described)
        );
        let listed = summary(&router, &id).await;
        assert_eq!(
            maintenance(&listed),
            window,
            "case {case:?}: expected the ListChannels summary to report the selected window {window:?}, got {:?}",
            maintenance(&listed)
        );
    }
}

/// Rule: the selected window is movable — `UpdateChannel`'s
/// `MaintenanceUpdateSettings` names the members it changes, and the ones it
/// does not name keep the values the channel already had, so the channel always
/// has a complete window.
///
/// The rows differ along which members the update names, over a channel whose
/// window the service selected on its own (the case the issue reports).
#[tokio::test]
async fn an_update_moves_the_selected_window_member_by_member() {
    // (case, the `maintenance` patch the update names)
    let table: [(&str, Value); 4] = [
        ("the day alone", json!({ "maintenanceDay": "FRIDAY" })),
        ("the start time alone", json!({ "maintenanceStartTime": "04:00" })),
        (
            "both",
            json!({ "maintenanceDay": "TUESDAY", "maintenanceStartTime": "21:00" }),
        ),
        ("neither", json!({})),
    ];

    for (case, patch) in table {
        let router = medialive();
        let created = create(&router, json!({ "name": format!("channel-{case}") })).await;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        let selected = maintenance(&created);
        assert_window_is_well_formed(case, "CreateChannel", &selected);

        wait_for_the_clock();
        let (status, updated) = call(
            &router,
            "PUT",
            &format!("/prod/channels/{id}"),
            Some(json!({ "maintenance": patch.clone() })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateChannel: expected 200, got {status}: {updated}"
        );

        // Each member is the patched value where the patch names it and the
        // service's selection where it does not.
        let mut expected = selected.clone();
        for member in ["maintenanceDay", "maintenanceStartTime"] {
            if let Some(value) = patch.get(member) {
                expected[member] = value.clone();
            }
        }
        let window = maintenance(&updated["channel"]);
        assert_window_is_well_formed(case, "UpdateChannel", &window);
        assert_eq!(
            window, expected,
            "case {case:?}: expected the moved window {expected:?}, got {window:?}"
        );

        wait_for_the_clock();
        let (status, described) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel after the update: got {status}: {described}"
        );
        assert_eq!(
            maintenance(&described),
            expected,
            "case {case:?}: expected the moved window to read back, got {:?}",
            maintenance(&described)
        );
    }
}

/// Rule: a channel egresses from one public address per pipeline of its class,
/// so every READ of the channel reports one `ChannelEgressEndpoint` per pipeline
/// (`SourceIp`: "Public IP of where a channel's output comes from"), whatever
/// state the channel is in and for the channel's whole lifetime, up to the
/// terminal `DELETED`.
///
/// The addresses are spent by the provisioning the accepted create starts, so
/// the create's own acknowledgement — the channel as it was created — reports
/// none of them yet: `EgressEndpoints` is "The endpoints where outgoing
/// connections initiate from", and a channel that has just been accepted
/// initiates connections from nowhere so far.
///
/// The rows span the `ChannelClass` enum (`smithy_query(query="shape",
/// shape="ChannelClass")`: STANDARD | SINGLE_PIPELINE) plus the absent-class
/// row, which the service defaults to the two-pipeline STANDARD class, so the
/// endpoint count varies along exactly what the pipeline count varies along.
#[tokio::test]
async fn a_channel_reports_one_egress_endpoint_per_pipeline_in_every_state() {
    // (case, the requested ChannelClass, the endpoints the channel reports)
    let table: [(&str, Option<&str>, usize); 3] = [
        ("single pipeline", Some("SINGLE_PIPELINE"), 1),
        ("standard", Some("STANDARD"), 2),
        ("class omitted", None, 2),
    ];

    for (case, requested, endpoints) in table {
        let router = medialive();
        let mut body = json!({ "name": format!("channel-{case}") });
        if let Some(class) = requested {
            body["channelClass"] = json!(class);
        }

        let created = create(&router, body).await;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        assert!(
            source_ips(&created).is_empty(),
            "case {case:?}: CreateChannel: expected the acknowledgement to report no egress endpoint yet, got {:?}",
            created["egressEndpoints"]
        );

        // The addresses the provisioning spent, as the first read of the
        // channel reports them.
        wait_for_the_clock();
        let (status, idle) = describe(&router, &id).await;
        assert_eq!(status, 200, "case {case:?}: DescribeChannel: got {status}: {idle}");
        let ips = source_ips(&idle);
        assert_eq!(
            ips.len(),
            endpoints,
            "case {case:?}: DescribeChannel: expected {endpoints} egress endpoint(s), got {:?}",
            idle["egressEndpoints"]
        );
        for ip in &ips {
            assert!(
                ip.parse::<Ipv4Addr>().is_ok(),
                "case {case:?}: expected every sourceIp to be an IPv4 address, got {ip:?}"
            );
        }
        assert_eq!(
            ips.iter().collect::<BTreeSet<_>>().len(),
            ips.len(),
            "case {case:?}: expected each pipeline its own egress address, got {ips:?}"
        );

        // The same endpoints through every state the channel passes through,
        // and through every channel-shaped response: the describe, the list
        // summary, the start, the stop and the delete.
        let reads: [(&str, Vec<String>); 2] = [
            ("DescribeChannel while IDLE", ips.clone()),
            ("the ListChannels summary", source_ips(&summary(&router, &id).await)),
        ];
        for (read, reported) in reads {
            assert_eq!(
                reported, ips,
                "case {case:?}: expected {read} to report the channel's egress endpoints {ips:?}, got {reported:?}"
            );
        }

        for (read, method, path) in [
            ("StartChannel", "POST", format!("/prod/channels/{id}/start")),
            ("StopChannel", "POST", format!("/prod/channels/{id}/stop")),
            ("DeleteChannel", "DELETE", format!("/prod/channels/{id}")),
        ] {
            wait_for_the_clock();
            let (status, body) = call(&router, method, &path, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: {read}: expected 200, got {status}: {body}"
            );
            let reported = source_ips(&body);
            assert_eq!(
                reported, ips,
                "case {case:?}: expected {read} to report the channel's egress endpoints {ips:?}, got {reported:?}"
            );
        }

        wait_for_the_clock();
        let (status, deleted) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel after the delete: got {status}: {deleted}"
        );
        assert_eq!(
            source_ips(&deleted), ips,
            "case {case:?}: expected a DELETED channel to still report its egress endpoints {ips:?}, got {:?}",
            source_ips(&deleted)
        );
    }
}

/// Rule: an egress address belongs to one pipeline of one channel, so no two
/// pipelines in an account — in the same channel or in another — report the same
/// `SourceIp`.
///
/// The rows differ along the classes the account's channels are created with, so
/// the pipeline count per channel varies while the uniqueness rule does not. The
/// addresses are read through the describe, which is where a channel reports
/// the addresses its provisioning spent.
#[tokio::test]
async fn no_two_pipelines_in_an_account_share_an_egress_address() {
    let table: [(&str, &[&str]); 3] = [
        ("every channel standard", &["STANDARD", "STANDARD", "STANDARD"]),
        (
            "every channel single pipeline",
            &["SINGLE_PIPELINE", "SINGLE_PIPELINE"],
        ),
        (
            "classes mixed",
            &["SINGLE_PIPELINE", "STANDARD", "SINGLE_PIPELINE", "STANDARD"],
        ),
    ];

    for (case, classes) in table {
        let router = medialive();
        let mut all = Vec::new();
        for (index, class) in classes.iter().enumerate() {
            let created = create(
                &router,
                json!({ "name": format!("channel-{index}"), "channelClass": class }),
            )
            .await;
            let id = created["id"].as_str().unwrap_or_default().to_string();
            let (status, described) = describe(&router, &id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeChannel: got {status}: {described}"
            );
            all.extend(source_ips(&described));
        }
        assert_eq!(
            all.iter().collect::<BTreeSet<_>>().len(),
            all.len(),
            "case {case:?}: expected every pipeline in the account its own egress address, got {all:?}"
        );
    }
}

/// What a channel with no pipeline-VM restart scheduled reports as its maintenance status:
/// the member is on the wire, and holds no deadline. The emulator schedules no such event
/// for any channel, which is why this is what every channel reports — the same reason
/// `RestartChannelPipelines` gives for the value its own modelled member answers.
pub(crate) const NO_DEADLINE: &str = "";

/// The flat `maintenanceStatus` member one channel-shaped body reports beside the modelled
/// `maintenance` sub-structure: present, and holding no deadline.
fn assert_maintenance_status(case: &str, read: &str, body: &Value) {
    assert_eq!(
        body["maintenanceStatus"],
        json!(NO_DEADLINE),
        "case {case:?}: {read}: expected maintenanceStatus on the wire holding no deadline, \
         got {:?}",
        body["maintenanceStatus"]
    );
}

/// Rule: a channel body reports its maintenance surface as the modelled `maintenance`
/// sub-structure AND, beside it, the flat `maintenanceStatus` member a recorded body carries
/// — present, holding no deadline, because this emulator schedules no pipeline-VM restart
/// for any channel. "No deadline" is a value a client reads, not a missing key, so every
/// channel body a recording covers carries it.
///
/// The rows differ along which maintenance members the create request chooses — none at all
/// (the case the issue reports), the day alone, and both at two different days and hours,
/// including the last hour of the day — so the member is reported whatever window the
/// channel carries, and the window itself keeps being reported where the model declares it.
/// Each row reads both surfaces through the create, through a describe while the channel is
/// still being created, through a describe once it has settled, and through the delete.
#[tokio::test]
async fn every_recorded_channel_body_reports_the_maintenance_status() {
    // (case, the `maintenance` the create request names)
    let table: [(&str, Option<Value>); 4] = [
        ("the service's own selection", None),
        ("the day alone", Some(json!({ "maintenanceDay": "SUNDAY" }))),
        (
            "both, at the last hour of the day",
            Some(json!({ "maintenanceDay": "SATURDAY", "maintenanceStartTime": "23:00" })),
        ),
        (
            "both, mid-week and mid-afternoon",
            Some(json!({ "maintenanceDay": "WEDNESDAY", "maintenanceStartTime": "15:00" })),
        ),
    ];

    for (case, requested) in table {
        let router = medialive();
        let mut body = json!({ "name": format!("channel-{case}") });
        if let Some(requested) = requested {
            body["maintenance"] = requested;
        }

        let created = create(&router, body).await;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        assert_maintenance_status(case, "CreateChannel", &created);
        assert_window_is_well_formed(case, "CreateChannel", &maintenance(&created));

        let (status, creating) = describe(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel: got {status}: {creating}"
        );
        assert_maintenance_status(
            case,
            "DescribeChannel while the channel is being created",
            &creating,
        );

        let idle = described_once_in(&router, case, &id, "IDLE").await;
        assert_maintenance_status(case, "DescribeChannel once the channel has settled", &idle);
        assert_eq!(
            maintenance(&idle),
            maintenance(&created),
            "case {case:?}: expected the modelled window to be reported beside the status, got \
             {:?}",
            maintenance(&idle)
        );

        let (status, deleted) =
            call(&router, "DELETE", &format!("/prod/channels/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteChannel: expected 200, got {status}: {deleted}"
        );
        assert_maintenance_status(case, "DeleteChannel", &deleted);
        assert_eq!(
            maintenance(&deleted),
            maintenance(&created),
            "case {case:?}: expected the delete to report the channel's window too, got {:?}",
            maintenance(&deleted)
        );
    }
}

/// Rule: the maintenance a caller SCHEDULES is reported where the model declares it — the
/// `maintenance` sub-structure's own `maintenanceScheduledDate` — and the flat member beside
/// it still holds no deadline, because scheduling maintenance on a channel schedules no
/// pipeline-VM restart, which is what that member reports the time of.
///
/// One channel, read after the update that moves its window and schedules maintenance: the
/// update's own answer and the describe that follows it.
#[tokio::test]
async fn maintenance_a_caller_schedules_is_reported_where_the_model_declares_it() {
    /// The date the update below schedules maintenance for.
    const SCHEDULED: &str = "2031-02-03";

    let case = "a channel whose window is moved and whose maintenance is scheduled";
    let router = medialive();

    let created = create(
        &router,
        json!({
            "name": "channel-scheduled-maintenance",
            "maintenance": { "maintenanceDay": "MONDAY", "maintenanceStartTime": "02:00" },
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap_or_default().to_string();
    assert_maintenance_status(case, "CreateChannel", &created);
    let idle = described_once_in(&router, case, &id, "IDLE").await;
    assert_maintenance_status(case, "DescribeChannel while IDLE", &idle);

    let (status, updated) = call(
        &router,
        "PUT",
        &format!("/prod/channels/{id}"),
        Some(json!({
            "maintenance": {
                "maintenanceDay": "FRIDAY",
                "maintenanceStartTime": "04:00",
                "maintenanceScheduledDate": SCHEDULED,
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateChannel: expected 200, got {status}: {updated}"
    );
    let window = maintenance(&updated["channel"]);
    assert_eq!(
        window,
        json!({
            "maintenanceDay": "FRIDAY",
            "maintenanceStartTime": "04:00",
            "maintenanceScheduledDate": SCHEDULED,
        }),
        "case {case:?}: expected the update to move the window and schedule the maintenance, \
         got {window:?}"
    );

    let (status, described) = describe(&router, &id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeChannel after the update: got {status}: {described}"
    );
    assert_eq!(
        maintenance(&described),
        window,
        "case {case:?}: expected the scheduled maintenance to read back in the modelled \
         sub-structure, got {:?}",
        maintenance(&described)
    );
    assert_maintenance_status(case, "DescribeChannel after the update", &described);
}
