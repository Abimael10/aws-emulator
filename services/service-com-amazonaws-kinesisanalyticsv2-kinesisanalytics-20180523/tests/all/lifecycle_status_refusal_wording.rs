// SPDX-License-Identifier: CC0-1.0

//! The wording every lifecycle action of this service refuses an application with when the
//! status it is in is not one that action is accepted from.
//!
//! The rule under test: such a refusal carries `ResourceInUseException` (HTTP 400) with the
//! fixed sentence for its class, whose only variable part is the application's CURRENT
//! status, quoted: `Application cannot be <Transition> in '<ApplicationStatus>' state` for an
//! action the status does not admit, and `You can't force stop an application in
//! '<ApplicationStatus>' status.` for a stop the caller already asked to FORCE. The status in
//! the sentence is read off the stored application, so it is whichever status the caller
//! caught it in, and the application's name is in neither sentence.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: which
//! operation is refused (start, stop, a forced stop, delete, both updates), which status the
//! application is caught in (the settled `READY` and `RUNNING`, the transient `STARTING` and
//! `STOPPING`), which runtime family the application runs (a Flink streaming application and
//! an `INTERACTIVE` Zeppelin Studio notebook that was never started) and what the
//! application is NAMED — the names vary in length and in the characters
//! `ApplicationName`'s pattern admits, because the name must appear in no refusal.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    await_status, call, create_body, describe, error_code, error_message, reported_status, router,
};

/// The runtime a streaming application in this suite runs
/// (`smithy_query(query="shape",shape="RuntimeEnvironment")`).
const FLINK_RUNTIME: &str = "FLINK-1_20";

/// The runtime and mode of a Studio notebook: the `ZEPPELIN-FLINK-` family, run
/// `INTERACTIVE` (`smithy_query(query="shape",shape="RuntimeEnvironment")`,
/// `shape="ApplicationMode"`).
const NOTEBOOK_RUNTIME: &str = "ZEPPELIN-FLINK-3_0";

/// The whole `Message` an ineligible-status refusal carries, written out here so this suite
/// pins the wording rather than reading the service's own helper.
fn ineligible_status_message(transition: &str, status: &str) -> String {
    format!("Application cannot be {transition} in '{status}' state")
}

/// The whole `Message` a FORCED stop is refused with when the status does not admit a
/// force, pinned here the same way.
fn force_stop_refusal_message(status: &str) -> String {
    format!("You can't force stop an application in '{status}' status.")
}

/// Create an application of `runtime`, with the inline code content a streaming application
/// needs before it can be started, and return the `CreateTimestamp` it was stored with (the
/// value `DeleteApplication` has to carry).
async fn create_application(router: &MultiServiceRouter, name: &str, runtime: &str) -> i64 {
    let mut body = create_body(name, runtime);
    body["ApplicationConfiguration"] = json!({
        "ApplicationCodeConfiguration": {
            "CodeContentType": "PLAINTEXT",
            "CodeContent": {"TextContent": "-- the application's code"}
        }
    });
    if runtime.starts_with("ZEPPELIN-FLINK") {
        body["ApplicationMode"] = json!("INTERACTIVE");
        // A notebook carries its code in the notebook, not in the request.
        body["ApplicationConfiguration"] = json!({});
    }
    let (status, answered) = call(router, "CreateApplication", &body).await;
    assert_eq!(
        status, 200,
        "case {name:?}: CreateApplication must store the application, got {status} {answered}"
    );
    answered["ApplicationDetail"]["CreateTimestamp"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {name:?}: expected a CreateTimestamp, got {answered}"))
}

/// The status the clock owes an application in `status` next: a transient status settles on
/// its own, so a read taken after a refused call may legitimately see the successor instead
/// of the transient status the call was refused from. A settled status is its own successor.
fn settled_successor(status: &str) -> &str {
    match status {
        "STARTING" => "RUNNING",
        "STOPPING" => "READY",
        settled => settled,
    }
}

/// Drive a freshly created application into `target` and assert it is really in it, so each
/// row of the table below is refused from the status it claims.
async fn drive_to_status(router: &MultiServiceRouter, name: &str, target: &str) {
    match target {
        // A create leaves the application READY.
        "READY" => {}
        "STARTING" | "RUNNING" | "STOPPING" => {
            let (status, body) = call(router, "StartApplication", &json!({"ApplicationName": name})).await;
            assert_eq!(
                status, 200,
                "case {name:?}: StartApplication must be accepted from READY, got {status} {body}"
            );
            if target != "STARTING" {
                await_status(router, name, name, "RUNNING").await;
            }
            if target == "STOPPING" {
                let (status, body) =
                    call(router, "StopApplication", &json!({"ApplicationName": name})).await;
                assert_eq!(
                    status, 200,
                    "case {name:?}: StopApplication must be accepted from RUNNING, got {status} \
                     {body}"
                );
            }
        }
        other => panic!("case {name:?}: this suite drives no application into {other:?}"),
    }
    let seen = reported_status(router, name, name).await;
    assert_eq!(
        seen, target,
        "case {name:?}: expected the application to be caught in {target}, got {seen:?}"
    );
}

/// Every lifecycle action refused because of the status the application is in reports the
/// fixed sentence for that class: the current status, quoted, and no application name.
#[tokio::test]
async fn an_action_refused_on_the_applications_status_reports_the_fixed_sentence() {
    // (case, application name, runtime, the status the application is caught in,
    //  the operation refused, the members it carries besides ApplicationName,
    //  the whole Message the refusal must carry)
    let cases: [(&str, &str, &str, &str, &str, Value, String); 12] = [
        // The recorded instance: a Studio notebook that was never started is READY, which
        // is not a stoppable status — with and without Force.
        (
            "notebook-stop-from-ready",
            "studio.notebook-0",
            NOTEBOOK_RUNTIME,
            "READY",
            "StopApplication",
            json!({}),
            ineligible_status_message("Stopped", "READY"),
        ),
        (
            "notebook-force-stop-from-ready",
            // The shortest name `ApplicationName`'s length constraint admits, and a
            // character neither sentence ever contains, so the check below that the name
            // does not appear means something.
            "x",
            NOTEBOOK_RUNTIME,
            "READY",
            "StopApplication",
            json!({"Force": true}),
            force_stop_refusal_message("READY"),
        ),
        (
            "flink-stop-from-ready",
            "a_streaming.application-with-a-long-name-0123456789",
            FLINK_RUNTIME,
            "READY",
            "StopApplication",
            json!({}),
            ineligible_status_message("Stopped", "READY"),
        ),
        (
            "flink-force-stop-from-ready",
            "force-stop-from-ready",
            FLINK_RUNTIME,
            "READY",
            "StopApplication",
            json!({"Force": true}),
            force_stop_refusal_message("READY"),
        ),
        (
            "flink-stop-from-starting",
            "stop-while-starting",
            FLINK_RUNTIME,
            "STARTING",
            "StopApplication",
            json!({}),
            ineligible_status_message("Stopped", "STARTING"),
        ),
        (
            "flink-start-from-starting",
            "start-while-starting",
            FLINK_RUNTIME,
            "STARTING",
            "StartApplication",
            json!({}),
            ineligible_status_message("Started", "STARTING"),
        ),
        (
            "flink-start-from-running",
            "start-while-running",
            FLINK_RUNTIME,
            "RUNNING",
            "StartApplication",
            json!({}),
            ineligible_status_message("Started", "RUNNING"),
        ),
        (
            "flink-start-from-stopping",
            "start-while-stopping",
            FLINK_RUNTIME,
            "STOPPING",
            "StartApplication",
            json!({}),
            ineligible_status_message("Started", "STOPPING"),
        ),
        (
            "flink-delete-from-running",
            "delete-while-running",
            FLINK_RUNTIME,
            "RUNNING",
            "DeleteApplication",
            json!({}),
            ineligible_status_message("Deleted", "RUNNING"),
        ),
        (
            "flink-delete-from-starting",
            "delete-while-starting",
            FLINK_RUNTIME,
            "STARTING",
            "DeleteApplication",
            json!({}),
            ineligible_status_message("Deleted", "STARTING"),
        ),
        (
            "flink-update-from-stopping",
            "update-while-stopping",
            FLINK_RUNTIME,
            "STOPPING",
            "UpdateApplication",
            json!({
                // Filled in below from the version the application is on, so what the
                // refusal reports is the status and not a stale version.
                "CurrentApplicationVersionId": null,
                "ServiceExecutionRoleUpdate":
                    "arn:aws:iam::000000000000:role/kda-other-service-role",
            }),
            ineligible_status_message("Updated", "STOPPING"),
        ),
        (
            "flink-maintenance-update-from-starting",
            "maintenance-while-starting",
            FLINK_RUNTIME,
            "STARTING",
            "UpdateApplicationMaintenanceConfiguration",
            json!({
                "ApplicationMaintenanceConfigurationUpdate": {
                    "ApplicationMaintenanceWindowStartTimeUpdate": "08:00",
                },
            }),
            ineligible_status_message("Updated", "STARTING"),
        ),
    ];

    let router = router();
    for (case, name, runtime, caught_in, operation, members, expected) in cases {
        let create_timestamp = create_application(&router, name, runtime).await;
        drive_to_status(&router, name, caught_in).await;

        let mut body = members.clone();
        body["ApplicationName"] = json!(name);
        if operation == "DeleteApplication" {
            // The delete carries the create timestamp the caller last read, so what the
            // refusal reports is the status and not a mismatched timestamp.
            body["CreateTimestamp"] = json!(create_timestamp);
        }
        if body.get("CurrentApplicationVersionId").is_some() {
            let detail = describe(&router, name).await;
            body["CurrentApplicationVersionId"] = detail["ApplicationVersionId"].clone();
        }

        let (status, answered) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ResourceInUseException",
            "case {case}: expected ResourceInUseException, got {answered}"
        );
        let message = error_message(&answered);
        assert_eq!(
            message, expected,
            "case {case}: expected the fixed sentence for an action refused in {caught_in}, \
             got {message:?}"
        );
        assert!(
            !message.contains(name),
            "case {case}: the refusal must not name the application, got {message:?}"
        );

        // A refused action leaves the application to the clock: the status it was caught
        // in, or — where that status is a transient one — the status the clock owed it
        // next, never a status this refused call moved it to.
        let after = reported_status(&router, case, name).await;
        let owed = settled_successor(caught_in);
        assert!(
            after == caught_in || after == owed,
            "case {case}: a refused action must leave the status to the clock, expected \
             {caught_in} or {owed}, got {after:?}"
        );
    }
}

/// The sentence states the status the application is in at the moment of the call, not one
/// status for every refusal: the same operation refused from two different statuses reports
/// each of them.
#[tokio::test]
async fn the_sentence_states_whichever_status_the_application_is_in() {
    let router = router();
    let name = "status-is-read-per-call";
    create_application(&router, name, FLINK_RUNTIME).await;

    // READY: an unforced stop is refused and names READY, and a FORCED one is refused as a
    // force the status does not admit.
    let (_, from_ready) = call(&router, "StopApplication", &json!({"ApplicationName": name})).await;
    assert_eq!(
        error_message(&from_ready),
        ineligible_status_message("Stopped", "READY"),
        "case from-ready: got {from_ready}"
    );
    let (_, forced_from_ready) = call(
        &router,
        "StopApplication",
        &json!({"ApplicationName": name, "Force": true}),
    )
    .await;
    assert_eq!(
        error_message(&forced_from_ready),
        force_stop_refusal_message("READY"),
        "case forced-from-ready: got {forced_from_ready}"
    );

    // RUNNING: a start is refused and names RUNNING, so the status is read per call.
    drive_to_status(&router, name, "RUNNING").await;
    let (_, from_running) =
        call(&router, "StartApplication", &json!({"ApplicationName": name})).await;
    assert_eq!(
        error_message(&from_running),
        ineligible_status_message("Started", "RUNNING"),
        "case from-running: got {from_running}"
    );

    // …and the action each status DOES admit is still accepted, so the refusal is about the
    // status and not about the operation.
    let (status, stopped) = call(&router, "StopApplication", &json!({"ApplicationName": name})).await;
    assert_eq!(
        status, 200,
        "case stop-from-running: a stop of a RUNNING application must be accepted, got \
         {status} {stopped}"
    );
}
