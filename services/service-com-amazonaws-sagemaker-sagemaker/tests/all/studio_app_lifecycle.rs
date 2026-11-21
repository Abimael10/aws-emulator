// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker Studio's app lifecycle: `CreateApp`,
//! `DescribeApp` and `DeleteApp`.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so
//! what these tests exercise is what a real SDK client gets. The harness is the one
//! `inference_deployment_lifecycle` builds.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: an app
//! is identified by all five of `DomainId`, the owning `UserProfileName` **or**
//! `SpaceName`, `AppType` and `AppName`, so the round trip, the not-found refusal and
//! the teardown are asserted for every app type a create accepts, for both owner kinds,
//! and for a request that differs from a stored app in each single component.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Map, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    self, ComAmazonawsSagemakerSagemakerState,
};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator, ACCOUNT};
use crate::studio_user_profiles::{seed_studio_domain, seed_studio_owner};

/// Every value `AppType` accepts, from the model
/// (`smithy_query(query="shape", shape="AppType", model_path=".../com-amazonaws-sagemaker-sagemaker.json")`,
/// mirrored by `ops::types::APP_TYPE_VALUES`).
const APP_TYPES: [&str; 9] = [
    "Canvas",
    "CodeEditor",
    "DetailedProfiler",
    "JupyterLab",
    "JupyterServer",
    "KernelGateway",
    "RSessionGateway",
    "RStudioServerPro",
    "TensorBoard",
];

/// The `AppType`s the service has WITHDRAWN, which no `CreateApp` launches an app of
/// (`state::DEPRECATED_APP_TYPES`). They are still members of the modelled enum, so an app
/// of one is still addressed, read and deleted.
const DEPRECATED_APP_TYPES: [&str; 1] = ["KernelGateway"];

/// How long a poll below gives the clock to launch an app: far longer than the span the
/// state gives `state::APP_IN_SERVICE_AFTER`, so a loaded host still settles well inside
/// it. Only the loop's condition decides when it stops.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often that poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The two members that can name an app's owner, per `CreateApp`'s input: exactly one of
/// them is set on any accepted request.
const OWNER_MEMBERS: [&str; 2] = ["UserProfileName", "SpaceName"];

/// One app, as the five members that identify it.
#[derive(Clone)]
struct AppRef {
    case: String,
    domain: String,
    owner_member: &'static str,
    owner_name: String,
    app_type: &'static str,
    app_name: String,
}

impl AppRef {
    fn new(
        case: &str,
        domain: &str,
        owner_member: &'static str,
        owner_name: &str,
        app_type: &'static str,
        app_name: &str,
    ) -> Self {
        Self {
            case: case.to_string(),
            domain: domain.to_string(),
            owner_member,
            owner_name: owner_name.to_string(),
            app_type,
            app_name: app_name.to_string(),
        }
    }

    /// The request body naming this app, i.e. the whole input of `DescribeApp` and
    /// `DeleteApp` and the identifying part of `CreateApp`'s.
    fn body(&self) -> Value {
        let mut members = Map::new();
        members.insert("DomainId".to_string(), json!(self.domain));
        members.insert(self.owner_member.to_string(), json!(self.owner_name));
        members.insert("AppType".to_string(), json!(self.app_type));
        members.insert("AppName".to_string(), json!(self.app_name));
        Value::Object(members)
    }

    /// The member that does NOT name this app's owner, which a reader must omit.
    fn other_owner_member(&self) -> &'static str {
        if self.owner_member == "UserProfileName" {
            "SpaceName"
        } else {
            "UserProfileName"
        }
    }

    /// `arn:aws:sagemaker:<region>:<account>:app/<DomainId>/<owner>/<AppType>/<AppName>`.
    fn arn_in(&self, region: &str) -> String {
        format!(
            "arn:aws:sagemaker:{region}:{ACCOUNT}:app/{}/{}/{}/{}",
            self.domain, self.owner_name, self.app_type, self.app_name
        )
    }

    fn arn(&self) -> String {
        self.arn_in("us-east-1")
    }
}

/// `base` with every member of `extra` added — how a `CreateApp` body is built from the
/// five identifying members plus the optional ones a row carries.
fn with_members(base: &Value, extra: &Value) -> Value {
    let mut merged = base
        .as_object()
        .expect("the identifying members form an object")
        .clone();
    for (member, value) in extra
        .as_object()
        .expect("the extra members form an object")
    {
        merged.insert(member.clone(), value.clone());
    }
    Value::Object(merged)
}

/// Put the PARENTS `app` hangs off into the store, so a create of it resolves them.
///
/// `CreateApp` only creates an app in a domain this service holds, and only for a user
/// profile that domain holds, and these rows address domains by `DomainId`s of their own
/// choosing — ids no `CreateDomain` can mint, since the id is the server's — so the domain
/// and, for a user-profile app, the profile are seeded directly into the store the handler
/// reads (`crate::studio_user_profiles`), the way a suite seeds any record whose writer it
/// is not exercising. A space app's owner is a space, which this service holds no records
/// of, so only its domain is seeded.
fn seed_owner(emulator: &Emulator, app: &AppRef, region: &str) {
    if app.owner_member == "UserProfileName" {
        seed_studio_owner(emulator, region, &app.domain, &app.owner_name);
    } else {
        seed_studio_domain(emulator, region, &app.domain);
    }
}

/// Put `app` into the store as one whose teardown was asked for at `requested_at`, so a
/// case can drive the states a deleted app reports without depending on how fast the host
/// runs: a `requested_at` in the future keeps it `Deleting`, one in the past settles it on
/// `Deleted` for the first reader.
///
/// Seeded directly into the store the handlers read, the way this suite seeds the domain
/// and the profile an app hangs off ([`seed_owner`]): what is under test is what the
/// handlers answer FOR each state, not how the record got into it.
fn seed_deleting_app(emulator: &Emulator, app: &AppRef, region: &str, requested_at: SystemTime) {
    seed_owner(emulator, app, region);
    let key = state::app_key(
        &app.domain,
        (app.owner_member == "UserProfileName").then_some(app.owner_name.as_str()),
        (app.owner_member == "SpaceName").then_some(app.owner_name.as_str()),
        app.app_type,
        &app.app_name,
    )
    .expect("the row names exactly one owner");
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let mut store = store.lock();
    let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
    store.apps.insert(
        key,
        state::App {
            arn: app.arn_in(region),
            status: state::APP_STATUS_DELETING.to_string(),
            resource_spec: None,
            recovery_mode: None,
            tags: Vec::new(),
            creation_time: created,
            in_service_at: created,
            deletion_requested_at: Some(requested_at),
        },
    );
}

/// The rows the round-trip and release rules are asserted over: every app type in the
/// catalog, alternating owner kind, domain ids and app names at their modelled extremes,
/// and the optional members present on some rows and absent on others.
///
/// Each row carries the members `DescribeApp` must echo back (`echoed`); `Tags` is
/// deliberately on a row whose `echoed` is empty, because `DescribeApp` models no tag
/// member.
fn round_trip_rows() -> Vec<(AppRef, Value, Value)> {
    // `AppName` is `len 0..63`; `DomainId` is `len 0..63, pattern ^d-(-*[a-z0-9]){1,61}$`
    // (smithy_query operation=CreateApp), so these are the longest accepted values.
    let longest_name = "a".repeat(63);
    let longest_domain = format!("d-{}", "x".repeat(61));

    vec![
        (
            AppRef::new(
                "canvas-user-profile",
                "d-abc123",
                "UserProfileName",
                "studio-user",
                "Canvas",
                "default",
            ),
            json!({}),
            json!({}),
        ),
        (
            AppRef::new(
                "code-editor-space-with-instance-type",
                "d-abc123",
                "SpaceName",
                "team-space",
                "CodeEditor",
                "editor-1",
            ),
            json!({ "ResourceSpec": { "InstanceType": "ml.t3.medium" } }),
            json!({ "ResourceSpec": { "InstanceType": "ml.t3.medium" } }),
        ),
        (
            AppRef::new(
                "detailed-profiler-shortest-domain",
                "d-0",
                "UserProfileName",
                "u",
                "DetailedProfiler",
                "profiler",
            ),
            json!({}),
            json!({}),
        ),
        (
            AppRef::new(
                "jupyter-lab-longest-domain-and-name",
                &longest_domain,
                "SpaceName",
                "space-with-hyphens",
                "JupyterLab",
                &longest_name,
            ),
            json!({}),
            json!({}),
        ),
        (
            AppRef::new(
                "jupyter-server-recovery-mode",
                "d-abc123",
                "UserProfileName",
                "studio-user",
                "JupyterServer",
                "default",
            ),
            json!({ "RecoveryMode": true }),
            json!({ "RecoveryMode": true }),
        ),
        (
            AppRef::new(
                "jupyter-lab-full-resource-spec",
                "d-abc123",
                "SpaceName",
                "team-space",
                "JupyterLab",
                "full-spec",
            ),
            json!({
                "ResourceSpec": {
                    "InstanceType": "ml.m5.large",
                    "SageMakerImageArn": format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:image/datascience"),
                },
                "RecoveryMode": false,
            }),
            json!({
                "ResourceSpec": {
                    "InstanceType": "ml.m5.large",
                    "SageMakerImageArn": format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:image/datascience"),
                },
                "RecoveryMode": false,
            }),
        ),
        (
            AppRef::new(
                "r-session-gateway-digits-and-hyphens",
                "d-9f8e7d6c",
                "UserProfileName",
                "user-42",
                "RSessionGateway",
                "r-session-1",
            ),
            json!({}),
            json!({}),
        ),
        (
            AppRef::new(
                "rstudio-server-pro-space",
                "d-abc123",
                "SpaceName",
                "rstudio-space",
                "RStudioServerPro",
                "rstudio",
            ),
            json!({}),
            json!({}),
        ),
        (
            AppRef::new(
                "tensorboard-shortest-name-with-tags",
                "d-abc123",
                "UserProfileName",
                "studio-user",
                "TensorBoard",
                "t",
            ),
            json!({ "Tags": [{ "Key": "team", "Value": "ml" }] }),
            json!({}),
        ),
    ]
}

/// RULE: `CreateApp` stores an app under all five members that identify it and mints
/// `arn:aws:sagemaker:<region>:<account>:app/<DomainId>/<owner>/<AppType>/<AppName>`;
/// `DescribeApp` naming those same five members replays what the create stored — the
/// ARN, the identifying members (with only the owner member that was set), the optional
/// members it carried, the stored creation time, and the launching `Pending` status.
#[tokio::test]
async fn describe_app_replays_exactly_what_create_app_stored() {
    let emulator = Emulator::new();

    for (app, extra, echoed) in round_trip_rows() {
        let case = &app.case;
        seed_owner(&emulator, &app, "us-east-1");
        let created = emulator
            .ok("CreateApp", &with_members(&app.body(), &extra))
            .await;
        assert_eq!(
            str_field(&created, "AppArn"),
            app.arn(),
            "case {case}: CreateApp AppArn, got {created:?}"
        );

        let described = emulator.ok("DescribeApp", &app.body()).await;
        assert_eq!(
            str_field(&described, "AppArn"),
            app.arn(),
            "case {case}: DescribeApp AppArn, got {described:?}"
        );
        assert_eq!(
            str_field(&described, "DomainId"),
            app.domain,
            "case {case}: DomainId, got {described:?}"
        );
        assert_eq!(
            str_field(&described, "AppType"),
            app.app_type,
            "case {case}: AppType, got {described:?}"
        );
        assert_eq!(
            str_field(&described, "AppName"),
            app.app_name,
            "case {case}: AppName, got {described:?}"
        );
        assert_eq!(
            str_field(&described, app.owner_member),
            app.owner_name,
            "case {case}: {}, got {described:?}",
            app.owner_member
        );
        assert!(
            described.get(app.other_owner_member()).is_none(),
            "case {case}: {} must be omitted for an app the other member owns, got {described:?}",
            app.other_owner_member()
        );
        assert_eq!(
            str_field(&described, "Status"),
            "Pending",
            "case {case}: a just-created app is still launching, got {described:?}"
        );
        assert!(
            described.get("CreationTime").is_some_and(Value::is_number),
            "case {case}: CreationTime must be a stored timestamp, got {described:?}"
        );
        for (member, value) in echoed.as_object().expect("echoed members form an object") {
            assert_eq!(
                described.get(member),
                Some(value),
                "case {case}: {member} must be replayed as stored, got {described:?}"
            );
        }
        // Nothing health-checked the app and no user touched it, so the emulator reports
        // no activity timestamps and no failure for an app that is launching.
        for member in ["LastHealthCheckTimestamp", "LastUserActivityTimestamp", "FailureReason"] {
            assert!(
                described.get(member).is_none(),
                "case {case}: {member} must be omitted when nothing produced one, got {described:?}"
            );
        }

        let again = emulator.ok("DescribeApp", &app.body()).await;
        assert_eq!(
            again, described,
            "case {case}: a second DescribeApp must answer identically"
        );
    }
}

/// RULE: an app's ARN carries the region the caller signed for, so a client reading an
/// app back never sees an ARN naming another region.
#[tokio::test]
async fn an_app_arn_names_the_region_the_caller_signed_for() {
    let emulator = Emulator::new();

    for region in ["us-east-1", "eu-west-2", "ap-southeast-4"] {
        // The app's key carries no Region, so each row names its own app: a key whose app
        // a previous row tore down is retained, not free.
        let app = AppRef::new(
            region,
            "d-abc123",
            "UserProfileName",
            "studio-user",
            "JupyterLab",
            &format!("regional-{region}"),
        );
        seed_owner(&emulator, &app, region);
        let (status, created) = emulator.call_in(region, "CreateApp", &app.body()).await;
        assert_eq!(
            status, 200,
            "case {region}: expected HTTP 200 from CreateApp, got {status} with {created:?}"
        );
        assert_eq!(
            str_field(&created, "AppArn"),
            app.arn_in(region),
            "case {region}: AppArn, got {created:?}"
        );

        let (status, deleted) = emulator.call_in(region, "DeleteApp", &app.body()).await;
        assert_eq!(
            status, 200,
            "case {region}: expected HTTP 200 from DeleteApp, got {status} with {deleted:?}"
        );
    }
}

/// RULE: a newly created app is reported `Pending`, and the `InService` transition the
/// clock owes becomes visible once its span has elapsed — without any call having to make
/// it happen, and stably thereafter.
#[tokio::test]
async fn an_app_advances_from_pending_to_in_service_once_the_clock_owes_it() {
    let emulator = Emulator::new();
    let app = AppRef::new(
        "clock-owed-launch",
        "d-abc123",
        "SpaceName",
        "team-space",
        "JupyterLab",
        "waiter",
    );
    seed_owner(&emulator, &app, "us-east-1");
    emulator.ok("CreateApp", &app.body()).await;

    let pending = emulator.ok("DescribeApp", &app.body()).await;
    assert_eq!(
        str_field(&pending, "Status"),
        "Pending",
        "case immediately-after-create: Status, got {pending:?}"
    );

    // Wait for the CONDITION, not for a fixed time: the launch is the clock's, and no call
    // has to make it happen.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let settled = loop {
        let described = emulator.ok("DescribeApp", &app.body()).await;
        if str_field(&described, "Status") == "InService" {
            break described;
        }
        assert!(
            Instant::now() < deadline,
            "case after-the-due-span: the app must reach InService on the clock, last read \
             {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    let again = emulator.ok("DescribeApp", &app.body()).await;
    assert_eq!(
        again, settled,
        "case repeated-read: a settled app must read back identically"
    );
}

/// RULE: `DeleteApp` is refused with HTTP 400 `ResourceNotFound` — never a 501 and never
/// an empty 200 — for every key no app is stored under: one nothing created, one that
/// differs from a stored app in any single identifying member, and one a previous delete
/// already released. The refusal's message is built from the request, naming the app's
/// ARN: `App [<arn>] does not exist.`
#[tokio::test]
async fn deleting_an_app_that_is_not_stored_is_refused_with_resource_not_found() {
    let emulator = Emulator::new();

    let stored = AppRef::new(
        "stored",
        "d-abc123",
        "UserProfileName",
        "studio-user",
        "JupyterServer",
        "default",
    );
    seed_owner(&emulator, &stored, "us-east-1");
    emulator.ok("CreateApp", &stored.body()).await;

    // Rows: the empty-store case, then one row per identifying member, each differing
    // from the stored app in that member alone, so no single component can be ignored.
    let mut cases = vec![
        AppRef::new(
            "nothing-created",
            "d-999999",
            "UserProfileName",
            "nobody",
            "Canvas",
            "absent",
        ),
        AppRef::new(
            "other-domain",
            "d-def456",
            "UserProfileName",
            "studio-user",
            "JupyterServer",
            "default",
        ),
        AppRef::new(
            "other-user-profile",
            "d-abc123",
            "UserProfileName",
            "other-user",
            "JupyterServer",
            "default",
        ),
        AppRef::new(
            "owner-is-a-space-not-a-user-profile",
            "d-abc123",
            "SpaceName",
            "studio-user",
            "JupyterServer",
            "default",
        ),
        AppRef::new(
            "other-app-name",
            "d-abc123",
            "UserProfileName",
            "studio-user",
            "JupyterServer",
            "other",
        ),
    ];
    // One row per app type the stored app is not, so the type is part of the key for the
    // whole modelled catalog rather than for one value.
    cases.extend(
        APP_TYPES
            .iter()
            .filter(|app_type| **app_type != stored.app_type)
            .map(|app_type| {
                AppRef::new(
                    &format!("other-app-type-{app_type}"),
                    "d-abc123",
                    "UserProfileName",
                    "studio-user",
                    app_type,
                    "default",
                )
            }),
    );

    for app in &cases {
        let case = &app.case;
        let (status, parsed) = emulator.call("DeleteApp", &app.body()).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400 from DeleteApp, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {case}: error code, got {parsed:?}"
        );
        assert_eq!(
            message(&parsed),
            format!("App [{}] does not exist.", app.arn()),
            "case {case}: the refusal must name the app it could not find, got {parsed:?}"
        );
    }

    // The stored app is still there: none of the refused deletes touched it.
    let described = emulator.ok("DescribeApp", &stored.body()).await;
    assert_eq!(
        str_field(&described, "AppArn"),
        stored.arn(),
        "case stored-app-survives: AppArn, got {described:?}"
    );

    // And a key whose app a delete has already torn down is NOT answered as not found:
    // the record is retained, so the second delete reports the teardown instead — which is
    // what lets idempotent teardown code tell "already deleted" from "never existed".
    let (status, parsed) = emulator.call("DeleteApp", &stored.body()).await;
    assert_eq!(
        status, 200,
        "case first-delete: expected HTTP 200, got {status} with {parsed:?}"
    );
    let (status, parsed) = emulator.call("DeleteApp", &stored.body()).await;
    assert_eq!(
        status, 400,
        "case second-delete: expected HTTP 400, got {status} with {parsed:?}"
    );
    assert_eq!(
        error_code(&parsed),
        "ValidationException",
        "case second-delete: error code, got {parsed:?}"
    );
    // Either sentence is correct here, because WHICH status the app is in is the clock's:
    // each one is asserted on its own, against a seeded app in that exact state, by
    // `deleting_or_recreating_an_app_being_torn_down_is_refused_over_its_status`.
    let sentences = [
        format!(
            "App [{}] is already being deleted. Wait until the app is deleted.",
            stored.app_name
        ),
        format!(
            "App [{}] has already been deleted. For tracking purposes, apps are available \
             in the ListApps API results for only 24 hours after deletion.",
            stored.app_name
        ),
    ];
    assert!(
        sentences.contains(&message(&parsed)),
        "case second-delete: the refusal must report the teardown, got {parsed:?}"
    );
}

/// RULE: a delete tears down exactly the app its five members name — that app reports the
/// non-terminal `Deleting` from the call and settles on `Deleted`, which its record keeps
/// answering for the whole retention window, while every app differing in any single member
/// is untouched; once it has settled, its key takes a fresh app again.
#[tokio::test]
async fn deleting_an_app_tears_down_only_that_app_and_keeps_reporting_it() {
    let emulator = Emulator::new();

    // Four apps that pairwise differ in exactly one identifying member, so a delete
    // keyed on the whole tuple can be told apart from one keyed on any part of it.
    let apps = [
        AppRef::new(
            "target",
            "d-abc123",
            "UserProfileName",
            "studio-user",
            "JupyterServer",
            "shared-name",
        ),
        AppRef::new(
            "same-name-other-type",
            "d-abc123",
            "UserProfileName",
            "studio-user",
            "Canvas",
            "shared-name",
        ),
        AppRef::new(
            "same-name-owned-by-a-space",
            "d-abc123",
            "SpaceName",
            "studio-user",
            "JupyterServer",
            "shared-name",
        ),
        AppRef::new(
            "same-name-other-domain",
            "d-def456",
            "UserProfileName",
            "studio-user",
            "JupyterServer",
            "shared-name",
        ),
    ];
    for app in &apps {
        seed_owner(&emulator, app, "us-east-1");
        emulator.ok("CreateApp", &app.body()).await;
    }

    emulator.ok("DeleteApp", &apps[0].body()).await;

    // The deleted app is still readable, and reports a status from the teardown rather
    // than the one it was launched in.
    let described = emulator.ok("DescribeApp", &apps[0].body()).await;
    assert_eq!(
        str_field(&described, "AppArn"),
        apps[0].arn(),
        "case deleted-app: a deleted app's record is retained, got {described:?}"
    );
    assert!(
        ["Deleting", "Deleted"].contains(&str_field(&described, "Status").as_str()),
        "case deleted-app: a deleted app reports its teardown, got {described:?}"
    );

    // Wait for the CONDITION: the `Deleted` status is the clock's, and no call has to make
    // it happen.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let settled = loop {
        let described = emulator.ok("DescribeApp", &apps[0].body()).await;
        if str_field(&described, "Status") == "Deleted" {
            break described;
        }
        assert!(
            Instant::now() < deadline,
            "case deleted-app: the app must reach Deleted on the clock, last read \
             {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    let again = emulator.ok("DescribeApp", &apps[0].body()).await;
    assert_eq!(
        again, settled,
        "case deleted-app-repeated-read: a settled app must read back identically"
    );

    for app in &apps[1..] {
        let described = emulator.ok("DescribeApp", &app.body()).await;
        assert_eq!(
            str_field(&described, "AppArn"),
            app.arn(),
            "case {}: a sibling app must be untouched by the delete, got {described:?}",
            app.case
        );
        assert!(
            ["Pending", "InService"].contains(&str_field(&described, "Status").as_str()),
            "case {}: a sibling app must still be running, got {described:?}",
            app.case
        );
    }

    // A settled app's key takes a fresh app again — the status AWS's own refusal names as
    // the one to wait for — and the new app is launching rather than deleted.
    let recreated = emulator.ok("CreateApp", &apps[0].body()).await;
    assert_eq!(
        str_field(&recreated, "AppArn"),
        apps[0].arn(),
        "case recreate-after-delete: AppArn, got {recreated:?}"
    );
    let described = emulator.ok("DescribeApp", &apps[0].body()).await;
    assert_eq!(
        str_field(&described, "Status"),
        "Pending",
        "case recreate-after-delete: the new app is launching, got {described:?}"
    );
}

/// RULE: every `AppType` the service has withdrawn is refused by `CreateApp` with HTTP 400
/// `ValidationException` — `Invalid AppType [<AppType>]. This AppType has been deprecated.`
/// — before any app is stored, while every other member of the enum is launched.
#[tokio::test]
async fn creating_an_app_of_a_withdrawn_app_type_is_refused() {
    let emulator = Emulator::new();

    // Rows: each withdrawn type under each owner kind, so the refusal is about the type
    // rather than about who owns the app.
    for app_type in DEPRECATED_APP_TYPES {
        for owner_member in OWNER_MEMBERS {
            let app = AppRef::new(
                &format!("{app_type}-{owner_member}"),
                "d-abc123",
                owner_member,
                "studio-user",
                app_type,
                "withdrawn",
            );
            let case = &app.case;
            seed_owner(&emulator, &app, "us-east-1");
            let (status, parsed) = emulator.call("CreateApp", &app.body()).await;
            assert_eq!(
                status, 400,
                "case {case}: expected HTTP 400 from CreateApp, got {status} with {parsed:?}"
            );
            assert_eq!(
                error_code(&parsed),
                "ValidationException",
                "case {case}: error code, got {parsed:?}"
            );
            assert_eq!(
                message(&parsed),
                format!("Invalid AppType [{app_type}]. This AppType has been deprecated."),
                "case {case}: the refusal must name the withdrawn app type, got {parsed:?}"
            );

            // Nothing was stored for the refused create.
            let (status, parsed) = emulator.call("DescribeApp", &app.body()).await;
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "ResourceNotFound"),
                "case {case}: the refused create stored nothing, got {status} with {parsed:?}"
            );
        }
    }

    // The positive control: every type the service still serves is launched.
    for app_type in APP_TYPES
        .iter()
        .filter(|app_type| !DEPRECATED_APP_TYPES.contains(app_type))
    {
        let app = AppRef::new(
            app_type,
            "d-abc123",
            "UserProfileName",
            "studio-user",
            app_type,
            "served",
        );
        seed_owner(&emulator, &app, "us-east-1");
        let (status, created) = emulator.call("CreateApp", &app.body()).await;
        assert_eq!(
            status, 200,
            "case {app_type}: a type the service serves is launched, got {status} with \
             {created:?}"
        );
    }
}

/// RULE: a second `DeleteApp` for an app the service is already tearing down, or has torn
/// down, is refused with HTTP 400 `ValidationException` and the sentence for the status the
/// app is in — never with the not-found refusal, which is reserved for a key nothing
/// created — and a `CreateApp` for that key is refused as in use until the app settles.
#[tokio::test]
async fn deleting_or_recreating_an_app_being_torn_down_is_refused_over_its_status() {
    let emulator = Emulator::new();

    // Rows: an app whose `Deleted` status the clock does NOT yet owe (the teardown is in
    // flight) and one whose record has already settled, each under an owner kind. WHICH
    // state the app is in is seeded, so neither case depends on how fast the host runs.
    let in_flight = AppRef::new(
        "still-deleting",
        "d-abc123",
        "UserProfileName",
        "studio-user",
        "JupyterServer",
        "tearing-down",
    );
    let settled = AppRef::new(
        "already-deleted",
        "d-abc123",
        "SpaceName",
        "team-space",
        "JupyterLab",
        "torn-down",
    );
    let now = SystemTime::now();
    seed_deleting_app(&emulator, &in_flight, "us-east-1", now + SETTLE_DEADLINE);
    // Past the span the clock owes the `Deleted` status, and far inside the retention
    // window, so the first reader settles this one and the record is still there.
    seed_deleting_app(
        &emulator,
        &settled,
        "us-east-1",
        now - (state::APP_DELETED_AFTER + Duration::from_secs(1)),
    );

    // The app still being torn down: the reader reports `Deleting`, the delete says the
    // teardown is in flight, and the create cannot take the key over.
    let described = emulator.ok("DescribeApp", &in_flight.body()).await;
    assert_eq!(
        str_field(&described, "Status"),
        "Deleting",
        "case still-deleting: Status, got {described:?}"
    );
    let (status, parsed) = emulator.call("DeleteApp", &in_flight.body()).await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ValidationException"),
        "case still-deleting: expected a 400 ValidationException, got {status} with {parsed:?}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "App [{}] is already being deleted. Wait until the app is deleted.",
            in_flight.app_name
        ),
        "case still-deleting: the refusal must report the teardown in flight, got {parsed:?}"
    );
    let (status, parsed) = emulator.call("CreateApp", &in_flight.body()).await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ResourceInUse"),
        "case still-deleting: expected a 400 ResourceInUse, got {status} with {parsed:?}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "App [{}] is currently in [Deleting] status. To restart the App, wait until it \
             is in Deleted or Failed status.",
            in_flight.arn()
        ),
        "case still-deleting: the refusal must report the status to wait for, got {parsed:?}"
    );

    // The app already torn down: the reader reports `Deleted` for as long as the record is
    // retained, the delete says so, and the create may take the key over.
    let described = emulator.ok("DescribeApp", &settled.body()).await;
    assert_eq!(
        str_field(&described, "Status"),
        "Deleted",
        "case already-deleted: Status, got {described:?}"
    );
    let (status, parsed) = emulator.call("DeleteApp", &settled.body()).await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ValidationException"),
        "case already-deleted: expected a 400 ValidationException, got {status} with {parsed:?}"
    );
    assert_eq!(
        message(&parsed),
        format!(
            "App [{}] has already been deleted. For tracking purposes, apps are available \
             in the ListApps API results for only 24 hours after deletion.",
            settled.app_name
        ),
        "case already-deleted: the refusal must report the completed teardown, got {parsed:?}"
    );
    let (status, created) = emulator.call("CreateApp", &settled.body()).await;
    assert_eq!(
        status, 200,
        "case already-deleted: a settled app's key takes a fresh app, got {status} with \
         {created:?}"
    );
}

/// RULE: a `CreateApp` whose five identifying members address an app the service has not
/// finished with is refused with the modelled `ResourceInUse`, so the running app is never
/// silently replaced, and the refusal reports the status the caller has to wait for:
/// `App [<arn>] is currently in [<AppStatus>] status. To restart the App, wait until it is
/// in Deleted or Failed status.`
#[tokio::test]
async fn creating_an_app_whose_key_is_in_use_is_refused_with_resource_in_use() {
    let emulator = Emulator::new();

    for (app, extra, _) in round_trip_rows() {
        let case = &app.case;
        seed_owner(&emulator, &app, "us-east-1");
        emulator
            .ok("CreateApp", &with_members(&app.body(), &extra))
            .await;

        // Wait for the CONDITION the clock owes the launch, so the status the refusal
        // reports is the settled one rather than whichever the host raced to.
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let described = emulator.ok("DescribeApp", &app.body()).await;
            if str_field(&described, "Status") == "InService" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: the app must reach InService on the clock, last read \
                 {described:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        // A second create of the same key, carrying different optional members, must
        // neither succeed nor overwrite what is stored.
        let (status, parsed) = emulator
            .call(
                "CreateApp",
                &with_members(
                    &app.body(),
                    &json!({ "ResourceSpec": { "InstanceType": "ml.g5.xlarge" } }),
                ),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400 from the duplicate CreateApp, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceInUse",
            "case {case}: error code, got {parsed:?}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "App [{}] is currently in [InService] status. To restart the App, wait \
                 until it is in Deleted or Failed status.",
                app.arn()
            ),
            "case {case}: the refusal must name the app and the status it is in, got \
             {parsed:?}"
        );

        let described = emulator.ok("DescribeApp", &app.body()).await;
        let expected_spec = extra.get("ResourceSpec");
        assert_eq!(
            described.get("ResourceSpec"),
            expected_spec,
            "case {case}: the stored app must keep the first create's ResourceSpec, got {described:?}"
        );
    }
}

/// RULE: an app operation naming neither `UserProfileName` nor `SpaceName`, or naming
/// both, is refused with HTTP 400 `ValidationException`: each member's model doc makes it
/// required when the other is absent, so the owner is exactly one of them.
#[tokio::test]
async fn an_app_request_must_name_exactly_one_owner() {
    let emulator = Emulator::new();

    // A stored app, so a refusal cannot be mistaken for "not found".
    let stored = AppRef::new(
        "stored",
        "d-abc123",
        "UserProfileName",
        "studio-user",
        "JupyterServer",
        "default",
    );
    seed_owner(&emulator, &stored, "us-east-1");
    emulator.ok("CreateApp", &stored.body()).await;

    let neither = json!({
        "DomainId": "d-abc123",
        "AppType": "JupyterServer",
        "AppName": "default",
    });
    let both = json!({
        "DomainId": "d-abc123",
        "UserProfileName": "studio-user",
        "SpaceName": "team-space",
        "AppType": "JupyterServer",
        "AppName": "default",
    });

    // Rows: every operation that resolves an app, with each malformed owner combination.
    for target in ["CreateApp", "DescribeApp", "DeleteApp"] {
        for (case, body) in [("neither-owner", &neither), ("both-owners", &both)] {
            let (status, parsed) = emulator.call(target, body).await;
            assert_eq!(
                status, 400,
                "case {target}/{case}: expected HTTP 400, got {status} with {parsed:?}"
            );
            assert_eq!(
                error_code(&parsed),
                "ValidationException",
                "case {target}/{case}: error code, got {parsed:?}"
            );
            for member in OWNER_MEMBERS {
                assert!(
                    message(&parsed).contains(member),
                    "case {target}/{case}: the refusal must name {member}, got {parsed:?}"
                );
            }
        }
    }

    // The stored app is unchanged by any of those refusals.
    let described = emulator.ok("DescribeApp", &stored.body()).await;
    assert_eq!(
        str_field(&described, "AppArn"),
        stored.arn(),
        "case stored-app-survives: AppArn, got {described:?}"
    );
}

/// RULE: an app request outside a member's modelled constraint is refused with HTTP 400
/// before any state is written.
#[tokio::test]
async fn app_requests_outside_the_modelled_constraints_are_refused() {
    let emulator = Emulator::new();

    // Rows: one past each published bound — `AppName` / `DomainId` are `len 0..63`,
    // `DomainId` matches `^d-(-*[a-z0-9]){1,61}$`, and `AppType` is the enum whose values
    // `APP_TYPES` lists (smithy_query operation=CreateApp).
    let too_long_name = "a".repeat(64);
    let too_long_domain = format!("d-{}", "x".repeat(62));
    let cases: Vec<(&str, Value)> = vec![
        (
            "app-name-one-past-63",
            json!({
                "DomainId": "d-abc123",
                "UserProfileName": "studio-user",
                "AppType": "JupyterServer",
                "AppName": too_long_name,
            }),
        ),
        (
            "domain-id-one-past-63",
            json!({
                "DomainId": too_long_domain,
                "UserProfileName": "studio-user",
                "AppType": "JupyterServer",
                "AppName": "default",
            }),
        ),
        (
            "domain-id-outside-the-pattern",
            json!({
                "DomainId": "not-a-domain",
                "UserProfileName": "studio-user",
                "AppType": "JupyterServer",
                "AppName": "default",
            }),
        ),
        (
            "app-type-outside-the-catalog",
            json!({
                "DomainId": "d-abc123",
                "UserProfileName": "studio-user",
                "AppType": "NotAnAppType",
                "AppName": "default",
            }),
        ),
    ];

    for target in ["CreateApp", "DescribeApp", "DeleteApp"] {
        for (case, body) in &cases {
            let (status, parsed) = emulator.call(target, body).await;
            assert_eq!(
                status, 400,
                "case {target}/{case}: expected HTTP 400, got {status} with {parsed:?}"
            );
            assert_eq!(
                error_code(&parsed),
                "ValidationException",
                "case {target}/{case}: error code, got {parsed:?}"
            );
        }
    }
}

/// RULE: the app store holds at most its quota of records; the create past it is refused
/// with the modelled `ResourceLimitExceeded` and stores nothing, so no caller can grow the
/// emulator's state without end by varying the app's name.
#[tokio::test]
async fn a_create_past_the_app_quota_is_refused_with_the_modelled_limit_failure() {
    // `state::APP_QUOTA`.
    const QUOTA: usize = 1_000;

    let emulator = Emulator::new();
    let app_at = |index: usize| {
        AppRef::new(
            "quota",
            "d-abc123",
            "UserProfileName",
            "studio-user",
            "JupyterServer",
            &format!("app{index}"),
        )
    };

    seed_owner(&emulator, &app_at(0), "us-east-1");
    for index in 0..QUOTA {
        emulator.ok("CreateApp", &app_at(index).body()).await;
    }

    let overflow = app_at(QUOTA);
    let (status, parsed) = emulator.call("CreateApp", &overflow.body()).await;
    assert_eq!(
        status, 400,
        "case one-past-the-quota: expected HTTP 400, got {status} with {parsed:?}"
    );
    assert_eq!(
        error_code(&parsed),
        "ResourceLimitExceeded",
        "case one-past-the-quota: error code, got {parsed:?}"
    );

    // Nothing was stored for the refused create.
    let (status, parsed) = emulator.call("DescribeApp", &overflow.body()).await;
    assert_eq!(
        status, 400,
        "case refused-create-stored-nothing: expected HTTP 400, got {status} with {parsed:?}"
    );
    assert_eq!(
        error_code(&parsed),
        "ResourceNotFound",
        "case refused-create-stored-nothing: error code, got {parsed:?}"
    );

    // Deleting one app frees exactly one slot.
    emulator.ok("DeleteApp", &app_at(0).body()).await;
    emulator.ok("CreateApp", &overflow.body()).await;
}
