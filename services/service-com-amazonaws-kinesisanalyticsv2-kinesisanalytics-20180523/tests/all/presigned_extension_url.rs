// SPDX-License-Identifier: CC0-1.0

//! What `CreateApplicationPresignedUrl` answers for an application's extension.
//!
//! The rule under test: an extension runs inside the running application, so the service
//! issues a URL for one only while the application is RUNNING — a URL signed against this
//! emulator, valid for the session duration the request asked for and for twelve hours when
//! it asked for none — and from every other status it refuses the call with
//! `ResourceInUseException` (HTTP 400) carrying
//! `<Operation> operation cannot be performed for Application in '<ApplicationStatus>' state`,
//! while a name it holds no application for is `ResourceNotFoundException`.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: which
//! extension is asked for (both `UrlType` members), which runtime family the application
//! runs (a Flink streaming application and a Zeppelin Studio notebook), which status the
//! application is caught in (the settled `READY` — both as a create leaves it and as a
//! completed stop leaves it — and `RUNNING`, the transient `STARTING` and `STOPPING`), what
//! session duration the request names (none, and each published bound of
//! `SessionExpirationDurationInSeconds`) and how the application is named.

use serde_json::{json, Value};
use smithy_server_runtime::{MultiServiceRouter, UNSERVED_URL_BASE};

use super::harness::{
    await_status, call, create, create_startable, describe, error_code, error_message,
    reported_status, router,
};

/// The runtime a streaming application in this suite runs
/// (`smithy_query(query="shape",shape="RuntimeEnvironment")`).
const FLINK_RUNTIME: &str = "FLINK-1_20";

/// The runtime a Studio notebook runs: the `ZEPPELIN-FLINK-` family of the same enum.
const NOTEBOOK_RUNTIME: &str = "ZEPPELIN-FLINK-3_0";

/// The `UrlType` members, as the model's enum spells them
/// (`smithy_query(query="shape",shape="UrlType")`).
const FLINK_DASHBOARD_URL: &str = "FLINK_DASHBOARD_URL";
const ZEPPELIN_UI_URL: &str = "ZEPPELIN_UI_URL";

/// The published bounds of `SessionExpirationDurationInSeconds` — `@range` 1800..43200
/// (`smithy_query(query="shape",shape="SessionExpirationDurationInSeconds")`) — and the
/// validity the operation documents for a request that names none: "If you do not provide
/// this parameter, the returned URL is valid for twelve hours."
const MIN_SESSION_SECONDS: i64 = 1_800;
const MAX_SESSION_SECONDS: i64 = 43_200;
const DEFAULT_SESSION_SECONDS: i64 = 12 * 60 * 60;

/// The longest an `AuthorizedUrl` may be: `@length` 1..2048 on that shape
/// (`smithy_query(query="shape",shape="AuthorizedUrl")`).
const MAX_AUTHORIZED_URL_LENGTH: usize = 2_048;

/// The whole `Message` a request refused on the application's status carries, written out
/// here so this suite pins the wording rather than reading the service's own helper.
fn not_performable_message(operation: &str, status: &str) -> String {
    format!("{operation} operation cannot be performed for Application in '{status}' state")
}

/// Ask for one extension's URL, with a session duration when `seconds` names one.
async fn presigned_url(
    router: &MultiServiceRouter,
    application: &str,
    url_type: &str,
    seconds: Option<i64>,
) -> (u16, Value) {
    let mut body = json!({"ApplicationName": application, "UrlType": url_type});
    if let Some(seconds) = seconds {
        body["SessionExpirationDurationInSeconds"] = json!(seconds);
    }
    call(router, "CreateApplicationPresignedUrl", &body).await
}

/// The status an application driven to `target` is in: a `STOPPED` row drives it through
/// RUNNING and back, which leaves it in the same settled READY a create leaves it in, so
/// the same status is reached by two different routes.
fn caught_status(target: &str) -> &str {
    match target {
        "STOPPED" => "READY",
        reached => reached,
    }
}

/// Create an application of `runtime` and drive it to `target`, asserting it is really in
/// the status that leaves it in, so each row below is answered from the status it claims.
async fn application_in(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    runtime: &str,
    target: &str,
) {
    // A Studio notebook carries its code in the notebook; every other runtime needs code
    // content stored before a start is accepted.
    if runtime.starts_with("ZEPPELIN-") {
        create(router, name, runtime).await;
    } else {
        create_startable(router, name, runtime).await;
    }
    match target {
        // A create leaves the application READY.
        "READY" => {}
        "STARTING" | "RUNNING" | "STOPPING" | "STOPPED" => {
            let (status, body) =
                call(router, "StartApplication", &json!({"ApplicationName": name})).await;
            assert_eq!(
                status, 200,
                "case {case}: StartApplication must be accepted from READY, got {status} {body}"
            );
            if target != "STARTING" {
                await_status(router, case, name, "RUNNING").await;
            }
            if target == "STOPPING" || target == "STOPPED" {
                let (status, body) =
                    call(router, "StopApplication", &json!({"ApplicationName": name})).await;
                assert_eq!(
                    status, 200,
                    "case {case}: StopApplication must be accepted from RUNNING, got {status} \
                     {body}"
                );
            }
            if target == "STOPPED" {
                await_status(router, case, name, "READY").await;
            }
        }
        other => panic!("case {case}: this suite drives no application into {other:?}"),
    }
    let seen = reported_status(router, case, name).await;
    let expected = caught_status(target);
    assert_eq!(
        seen, expected,
        "case {case}: expected the application to be caught in {expected}, got {seen:?}"
    );
}

/// The `AuthorizedUrl` of a successful answer.
fn authorized_url(case: &str, body: &Value) -> String {
    body["AuthorizedUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected an AuthorizedUrl, got {body}"))
        .to_string()
}

/// One row of the table below: an application to drive to a status, the extension asked
/// for, the session duration the request names, and what the answer must be.
struct Case {
    /// What the row is called in every assertion it makes.
    case: &'static str,
    /// The `ApplicationName` the row creates and asks about.
    application: &'static str,
    /// The `RuntimeEnvironment` that application runs.
    runtime: &'static str,
    /// The status the application is driven to before the call ([`caught_status`]).
    driven_to: &'static str,
    /// The `UrlType` the request names.
    url_type: &'static str,
    /// The `SessionExpirationDurationInSeconds` the request names, if any.
    asked_for: Option<i64>,
    /// How many seconds the issued URL must stay valid for; `None` on a row that is
    /// refused and so is answered no URL at all.
    signed_for: Option<i64>,
}

/// A RUNNING application's extension is answered with a URL that leads back to this
/// emulator, addresses that application's extension, and is signed for exactly as long as
/// the request asked the session to stay valid.
#[tokio::test]
async fn a_running_application_is_answered_with_a_signed_url_for_the_extension() {
    let cases: [Case; 5] = [
        Case {
            case: "flink-dashboard-default-validity",
            application: "flink.dashboard-0",
            runtime: FLINK_RUNTIME,
            driven_to: "RUNNING",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: None,
            signed_for: Some(DEFAULT_SESSION_SECONDS),
        },
        Case {
            case: "flink-dashboard-shortest-session",
            application: "flink_dashboard_min",
            runtime: FLINK_RUNTIME,
            driven_to: "RUNNING",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: Some(MIN_SESSION_SECONDS),
            signed_for: Some(MIN_SESSION_SECONDS),
        },
        Case {
            case: "flink-dashboard-longest-session",
            application: "a_streaming.application-with-a-long-name-0123456789",
            runtime: FLINK_RUNTIME,
            driven_to: "RUNNING",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: Some(MAX_SESSION_SECONDS),
            signed_for: Some(MAX_SESSION_SECONDS),
        },
        Case {
            // The recorded instance's call, once the notebook is RUNNING.
            case: "notebook-zeppelin-ui",
            application: "studio.notebook-0",
            runtime: NOTEBOOK_RUNTIME,
            driven_to: "RUNNING",
            url_type: ZEPPELIN_UI_URL,
            asked_for: Some(7_200),
            signed_for: Some(7_200),
        },
        Case {
            // A notebook runs a Flink job of its own, so its dashboard is an extension of
            // the same application: the URL is issued for whichever extension the request
            // names. The shortest name `ApplicationName` admits.
            case: "notebook-flink-dashboard",
            application: "x",
            runtime: NOTEBOOK_RUNTIME,
            driven_to: "RUNNING",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: None,
            signed_for: Some(DEFAULT_SESSION_SECONDS),
        },
    ];

    let router = router();
    for row in cases {
        let Case {
            case,
            application: name,
            runtime,
            driven_to,
            url_type,
            asked_for,
            signed_for,
        } = row;
        let signed_for = signed_for.expect("every row of this table is answered a URL");
        application_in(&router, case, name, runtime, driven_to).await;

        let (status, body) = presigned_url(&router, name, url_type, asked_for).await;
        assert_eq!(
            status, 200,
            "case {case}: a RUNNING application's extension must be answered, got {status} \
             {body}"
        );
        let url = authorized_url(case, &body);
        assert!(
            !url.is_empty() && url.len() <= MAX_AUTHORIZED_URL_LENGTH,
            "case {case}: expected an AuthorizedUrl of 1..{MAX_AUTHORIZED_URL_LENGTH} \
             characters, got {} in {url:?}",
            url.len()
        );
        // The URL has to lead back to THIS emulator, never to the real service.
        assert!(
            url.starts_with(UNSERVED_URL_BASE),
            "case {case}: expected a URL on this emulator ({UNSERVED_URL_BASE}), got {url:?}"
        );
        assert!(
            !url.contains("amazonaws.com"),
            "case {case}: the URL must not address the real service, got {url:?}"
        );
        // It addresses THAT application's THAT extension…
        let path = format!("/applications/{name}/extensions/{url_type}");
        assert!(
            url.contains(&path),
            "case {case}: expected the URL to address {path}, got {url:?}"
        );
        // …it is query-signed as a request to this service…
        for field in [
            "X-Amz-Algorithm=AWS4-HMAC-SHA256",
            "X-Amz-Signature=",
            "X-Amz-Credential=",
        ] {
            assert!(
                url.contains(field),
                "case {case}: expected the URL to carry {field}, got {url:?}"
            );
        }
        assert!(
            url.contains("kinesisanalytics"),
            "case {case}: expected the URL to be signed for this service, got {url:?}"
        );
        // …and it stays valid for exactly the session the request asked for.
        let expires = format!("X-Amz-Expires={signed_for}");
        assert!(
            url.contains(&expires),
            "case {case}: expected the URL to carry {expires}, got {url:?}"
        );
    }
}

/// Every status but RUNNING is refused with the fixed sentence for that class: the operation
/// and the status the application is in, quoted, and no application name.
#[tokio::test]
async fn an_application_that_is_not_running_is_refused_on_its_status() {
    let cases: [Case; 6] = [
        // The recorded instance: a Studio notebook that was never started is READY, and a
        // READY notebook has no Zeppelin UI to connect to.
        Case {
            case: "notebook-zeppelin-ui-from-ready",
            application: "studio.notebook-0",
            runtime: NOTEBOOK_RUNTIME,
            driven_to: "READY",
            url_type: ZEPPELIN_UI_URL,
            asked_for: None,
            signed_for: None,
        },
        Case {
            case: "notebook-zeppelin-ui-from-starting",
            application: "studio-notebook-while-starting",
            runtime: NOTEBOOK_RUNTIME,
            driven_to: "STARTING",
            url_type: ZEPPELIN_UI_URL,
            asked_for: Some(MAX_SESSION_SECONDS),
            signed_for: None,
        },
        Case {
            case: "flink-dashboard-from-ready",
            application: "flink-dashboard-while-ready",
            runtime: FLINK_RUNTIME,
            driven_to: "READY",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: Some(MIN_SESSION_SECONDS),
            signed_for: None,
        },
        Case {
            case: "flink-dashboard-from-starting",
            application: "flink-dashboard-while-starting",
            runtime: FLINK_RUNTIME,
            driven_to: "STARTING",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: None,
            signed_for: None,
        },
        Case {
            case: "flink-dashboard-from-stopping",
            application: "flink-dashboard-while-stopping",
            runtime: FLINK_RUNTIME,
            driven_to: "STOPPING",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: Some(3_600),
            signed_for: None,
        },
        Case {
            // The same settled READY, reached by a completed stop rather than by a create:
            // the rule is about the status, not about how the application got there.
            case: "flink-dashboard-from-ready-after-a-stop",
            application: "flink.dashboard_after-stop",
            runtime: FLINK_RUNTIME,
            driven_to: "STOPPED",
            url_type: FLINK_DASHBOARD_URL,
            asked_for: Some(MAX_SESSION_SECONDS),
            signed_for: None,
        },
    ];

    let router = router();
    for row in cases {
        let Case {
            case,
            application: name,
            runtime,
            driven_to,
            url_type,
            asked_for,
            ..
        } = row;
        application_in(&router, case, name, runtime, driven_to).await;
        let caught_in = caught_status(driven_to);

        let (status, body) = presigned_url(&router, name, url_type, asked_for).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400 from {caught_in}, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceInUseException",
            "case {case}: expected ResourceInUseException, got {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message,
            not_performable_message("CreateApplicationPresignedUrl", caught_in),
            "case {case}: expected the fixed sentence for a call refused in {caught_in}, got \
             {message:?}"
        );
        assert!(
            !message.contains(name),
            "case {case}: the refusal must not name the application, got {message:?}"
        );
        assert!(
            body.get("AuthorizedUrl").is_none(),
            "case {case}: a refused call must hand out no URL, got {body}"
        );
    }
}

/// A name the service holds no application under is a lookup miss, not a status refusal.
#[tokio::test]
async fn a_name_no_application_is_stored_under_is_not_found() {
    // (case, the name asked for, the extension asked for)
    let cases: [(&str, &str, &str); 3] = [
        ("never-created", "no-such-application", ZEPPELIN_UI_URL),
        ("shortest-name", "q", FLINK_DASHBOARD_URL),
        (
            "every-admitted-character",
            "A_name.with-every_kind.of-character-0123456789",
            FLINK_DASHBOARD_URL,
        ),
    ];

    let router = router();
    // One stored application, so the miss is about the name asked for and not about an
    // empty store.
    application_in(&router, "neighbour", "a-stored-application", FLINK_RUNTIME, "RUNNING").await;

    for (case, name, url_type) in cases {
        let (status, body) = presigned_url(&router, name, url_type, None).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400 for an absent application, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Application '{name}' does not exist."),
            "case {case}: got {body}"
        );
    }
}

/// Two extensions of one application, and one extension of two applications, are answered
/// with URLs of their own — the URL addresses the extension it was asked for.
#[tokio::test]
async fn each_application_and_extension_has_its_own_url() {
    let router = router();
    let first = "first.application-0";
    let second = "second_application_1";
    application_in(&router, "first", first, NOTEBOOK_RUNTIME, "RUNNING").await;
    application_in(&router, "second", second, NOTEBOOK_RUNTIME, "RUNNING").await;

    // (case, application, extension)
    let asked: [(&str, &str, &str); 4] = [
        ("first-zeppelin", first, ZEPPELIN_UI_URL),
        ("first-dashboard", first, FLINK_DASHBOARD_URL),
        ("second-zeppelin", second, ZEPPELIN_UI_URL),
        ("second-dashboard", second, FLINK_DASHBOARD_URL),
    ];
    let mut issued: Vec<(&str, String)> = Vec::new();
    for (case, application, url_type) in asked {
        let (status, body) = presigned_url(&router, application, url_type, None).await;
        assert_eq!(status, 200, "case {case}: got {status} {body}");
        issued.push((case, authorized_url(case, &body)));
    }
    for (case, url) in &issued {
        for (other_case, other) in &issued {
            if case == other_case {
                continue;
            }
            assert_ne!(
                url, other,
                "case {case}: expected a URL of its own, got the one {other_case} was answered \
                 with: {url:?}"
            );
        }
    }
}

/// Issuing a URL reports on the application; it changes nothing a later caller sees — not
/// its status, not its version, and not the operations it records.
#[tokio::test]
async fn issuing_a_url_changes_nothing_a_later_caller_sees() {
    let router = router();
    let name = "reporting.only-0";
    application_in(&router, "reporting-only", name, FLINK_RUNTIME, "RUNNING").await;

    let before = describe(&router, name).await;

    for round in 0..3 {
        let (status, body) = presigned_url(&router, name, FLINK_DASHBOARD_URL, None).await;
        assert_eq!(status, 200, "case round-{round}: got {status} {body}");
        let url = authorized_url("round", &body);
        assert!(
            !url.is_empty(),
            "case round-{round}: expected a URL, got {body}"
        );
    }

    // The whole detail, so nothing the application reports — its status, its version, its
    // timestamps, its configuration — moved because a URL was issued.
    let after = describe(&router, name).await;
    assert_eq!(
        after, before,
        "case detail: issuing a URL must leave the application as it was, got {after} after \
         {before}"
    );
}
