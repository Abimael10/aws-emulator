// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the applications this service serves that the `app`
//! member's DOCUMENTED snapshot of the set does not list, driven over the wire
//! (restJson1) through the same router the emulator boots.
//!
//! RULE: the set of applications an AppFabric operation serves is the SERVICE's
//! own (`state::SUPPORTED_APPLICATIONS`) — the list the `app` member's
//! documentation carries is a snapshot of it, not its definition, so an
//! application AppFabric serves runs the whole chain (authorize → connect →
//! ingest, and the user-access fan-out over the authorizations stored by it)
//! exactly like a listed one, and nothing on any of those paths is refused on
//! its name. A request for such an application is therefore refused only for the
//! precondition it really misses: `CreateIngestion` before the pair's
//! authorization is connected reports the connect precondition, and
//! `StartUserAccessTasks` reports one item per application tenant the bundle
//! holds, whether or not the application is one the snapshot lists.
//!
//! The tables span the application name (served names from inside the documented
//! snapshot and from outside it), the tenant identifier, and the number and mix
//! of application tenants one bundle holds. `supported_application_names` owns
//! the refusing half of the rule (a name the service does not serve) and the
//! whole served set's round trip.
//!
//! The harness (the router, `Answer`, `encode`, `arn_of`) is the one
//! `app_bundle_ingestion_lifecycle` builds; the set-up helpers and the
//! user-access wording are `user_access_tasks`'.

use serde_json::{json, Value};

use crate::app_bundle_ingestion_lifecycle::{encode, Answer, Emulator};
use crate::user_access_tasks::{
    app_bundle, authorization, display_name, member, start_tasks, task_items,
    NO_AUTHENTICATION_TOKEN_CODE, NO_AUTHENTICATION_TOKEN_MESSAGE,
};

/// Applications the service serves whose names the member's documented snapshot
/// ("Valid values are: SLACK ASANA JIRA M365 M365AUDITLOGS ZOOM ZENDESK OKTA
/// GOOGLE DROPBOX SMARTSHEET CISCO" —
/// `smithy_query(query="doc",shape="CreateAppAuthorizationRequest",member="app")`)
/// does NOT list, so each row proves the set is the service's rather than the
/// snapshot's.
const SERVED_BEYOND_THE_SNAPSHOT: &[&str] = &["PAGERDUTY"];

/// Applications the snapshot DOES list, so every row below is run for a name of
/// each provenance and no single value stands for the rule. These are names the
/// issue did not cite.
const SERVED_IN_THE_SNAPSHOT: &[&str] = &["ZENDESK", "SMARTSHEET"];

/// The sentence `CreateIngestion` refuses an (application, tenant) pair whose
/// app authorization has not completed the connect handshake with — fixed text,
/// naming neither the application nor the tenant (see
/// `ingestion_authorization_refusal_wording`, where the wording is pinned).
const AUTHORIZATION_NOT_CONNECTED_MESSAGE: &str =
    "The app authorization for the specified application and tenant was not connected. \
     Connect the app authorization for the application and tenant before creating an ingestion.";

/// The `app` names every row runs: the served names outside the documented
/// snapshot first, then served names inside it.
fn served_apps() -> Vec<(&'static str, &'static str)> {
    let mut rows: Vec<(&'static str, &'static str)> = SERVED_BEYOND_THE_SNAPSHOT
        .iter()
        .map(|app| ("outside the documented snapshot", *app))
        .collect();
    rows.extend(
        SERVED_IN_THE_SNAPSHOT
            .iter()
            .map(|app| ("inside the documented snapshot", *app)),
    );
    rows
}

/// One member of the `appAuthorization` a success body carries.
fn authorization_member(answer: &Answer, name: &str) -> Value {
    answer
        .body
        .get("appAuthorization")
        .and_then(|authorization| authorization.get(name))
        .cloned()
        .unwrap_or_else(|| panic!("expected appAuthorization.{name} in {}", answer.body))
}

/// `POST /appbundles/{appBundleIdentifier}/ingestions`, with NO set-up of its
/// own: what the store holds for the pair is what each row arranged.
async fn create_ingestion(emulator: &Emulator, bundle: &str, app: &str, tenant: &str) -> Answer {
    emulator
        .call(
            "POST",
            &format!("/appbundles/{}/ingestions", encode(bundle)),
            Some(json!({"app": app, "tenantId": tenant, "ingestionType": "auditLog"})),
        )
        .await
}

/// `POST /appbundles/{appBundleIdentifier}/appauthorizations/{id}/connect`.
async fn connect(emulator: &Emulator, bundle: &str, authorization_arn: &str) -> Answer {
    emulator
        .call(
            "POST",
            &format!(
                "/appbundles/{}/appauthorizations/{}/connect",
                encode(bundle),
                encode(authorization_arn)
            ),
            Some(json!({})),
        )
        .await
}

/// The ARNs `ListIngestions` reports for `bundle`.
async fn held_ingestions(emulator: &Emulator, bundle: &str) -> Vec<String> {
    let listed = emulator.list_ingestions(bundle, "").await;
    assert_eq!(
        listed.status, 200,
        "ListIngestions must answer HTTP 200, got {} with body {}",
        listed.status, listed.body
    );
    listed
        .body
        .get("ingestions")
        .and_then(Value::as_array)
        .map(|summaries| {
            summaries
                .iter()
                .filter_map(|summary| summary.get("arn"))
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: an application the service serves runs the whole chain whichever side
/// of the documented snapshot its name falls on — `CreateAppAuthorization`
/// answers HTTP 201 with the authorization it stored (the request's `app`,
/// `authType` and tenant, the `PendingConnect` status and an ARN nested under
/// the bundle's), a read reports the same record back, and once the pair is
/// connected `CreateIngestion` of that pair succeeds.
#[tokio::test]
async fn a_served_application_runs_the_whole_chain() {
    for (provenance, app) in served_apps() {
        // A fresh store per row: a Region holds only one app bundle.
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator).await;
        let tenant = format!("{}.tenant.example", app.to_ascii_lowercase());

        let created = emulator
            .call(
                "POST",
                &format!("/appbundles/{}/appauthorizations", encode(&bundle)),
                Some(json!({
                    "app": app,
                    "authType": "apiKey",
                    "credential": {"apiKeyCredential": {"apiKey": "placeholder-api-key"}},
                    "tenant": {
                        "tenantIdentifier": &tenant,
                        "tenantDisplayName": display_name(&tenant),
                    },
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {app:?} ({provenance}): expected HTTP 201 from CreateAppAuthorization, got {} \
             with body {}",
            created.status, created.body
        );
        for (name, expected) in [
            ("app", json!(app)),
            ("authType", json!("apiKey")),
            ("status", json!("PendingConnect")),
            (
                "tenant",
                json!({
                    "tenantIdentifier": &tenant,
                    "tenantDisplayName": display_name(&tenant),
                }),
            ),
        ] {
            assert_eq!(
                authorization_member(&created, name),
                expected,
                "case {app:?} ({provenance}): expected appAuthorization.{name} == {expected}, got \
                 {}",
                created.body
            );
        }
        let arn = authorization_member(&created, "appAuthorizationArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let bundle_arn = authorization_member(&created, "appBundleArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            bundle_arn, bundle,
            "case {app:?} ({provenance}): expected the authorization to name its bundle, got {}",
            created.body
        );
        assert!(
            arn.starts_with(&format!("{bundle_arn}/appauthorization/")),
            "case {app:?} ({provenance}): expected the authorization ARN nested under the \
             bundle's, got {arn:?}"
        );

        // ROUND TRIP: the reader answers out of the same stored record.
        let read = emulator
            .call(
                "GET",
                &format!(
                    "/appbundles/{}/appauthorizations/{}",
                    encode(&bundle),
                    encode(&arn)
                ),
                None,
            )
            .await;
        assert_eq!(
            read.status, 200,
            "case {app:?} ({provenance}): expected the authorization to be readable, got {} with \
             body {}",
            read.status, read.body
        );
        assert_eq!(
            (
                authorization_member(&read, "app"),
                authorization_member(&read, "appAuthorizationArn"),
            ),
            (json!(app), json!(&arn)),
            "case {app:?} ({provenance}): expected the stored authorization on the read, got {}",
            read.body
        );

        let connected = connect(&emulator, &bundle, &arn).await;
        assert_eq!(
            connected.status, 200,
            "case {app:?} ({provenance}): expected HTTP 200 from ConnectAppAuthorization, got {} \
             with body {}",
            connected.status, connected.body
        );

        let ingestion = create_ingestion(&emulator, &bundle, app, &tenant).await;
        assert_eq!(
            ingestion.status, 201,
            "case {app:?} ({provenance}): expected HTTP 201 from CreateIngestion, got {} with \
             body {}",
            ingestion.status, ingestion.body
        );
        assert_eq!(
            ingestion
                .body
                .get("ingestion")
                .and_then(|record| record.get("app")),
            Some(&json!(app)),
            "case {app:?} ({provenance}): expected the ingestion to report the application name, \
             got {}",
            ingestion.body
        );
        assert_eq!(
            held_ingestions(&emulator, &bundle).await.len(),
            1,
            "case {app:?} ({provenance}): expected the bundle to hold exactly the created \
             ingestion"
        );
    }
}

/// RULE: for an application the service serves, `CreateIngestion` is refused for
/// the precondition the request really misses rather than on the application
/// name — HTTP 400 `ValidationException` carrying the connect-precondition
/// sentence (fixed text, carrying neither the application nor the tenant) while
/// the pair's authorization is still `PendingConnect` — and the refusal writes
/// no ingestion, so the same request succeeds once the connect call has run.
#[tokio::test]
async fn an_unconnected_pair_is_refused_for_the_connect_precondition() {
    for (provenance, app) in served_apps() {
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator).await;
        let tenant = format!("{}.unconnected.example", app.to_ascii_lowercase());
        let arn = authorization(&emulator, &bundle, app, &tenant, false).await;

        let refused = create_ingestion(&emulator, &bundle, app, &tenant).await;
        assert_eq!(
            refused.status, 400,
            "case {app:?} ({provenance}): expected HTTP 400, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {app:?} ({provenance}): expected ValidationException, got {} with body {}",
            refused.error_code(),
            refused.body
        );
        let message = member(&refused.body, "message");
        assert_eq!(
            message, AUTHORIZATION_NOT_CONNECTED_MESSAGE,
            "case {app:?} ({provenance}): expected the connect-precondition sentence, got \
             {message:?}"
        );
        for value in [app, tenant.as_str()] {
            assert!(
                !message.contains(value),
                "case {app:?} ({provenance}): the sentence is fixed, so it must not carry \
                 {value:?}, got {message:?}"
            );
        }
        assert_eq!(
            held_ingestions(&emulator, &bundle).await,
            Vec::<String>::new(),
            "case {app:?} ({provenance}): a refused create must write no ingestion"
        );

        // The refusal was about the connect step the caller still owed, so
        // running it makes the very same request succeed.
        let connected = connect(&emulator, &bundle, &arn).await;
        assert_eq!(
            connected.status, 200,
            "case {app:?} ({provenance}): expected HTTP 200 from ConnectAppAuthorization, got {} \
             with body {}",
            connected.status, connected.body
        );
        let accepted = create_ingestion(&emulator, &bundle, app, &tenant).await;
        assert_eq!(
            accepted.status, 201,
            "case {app:?} ({provenance}): expected the same request to succeed once connected, \
             got {} with body {}",
            accepted.status, accepted.body
        );
    }
}

/// RULE: `StartUserAccessTasks` fans out over the app authorizations the bundle
/// holds — one item per application tenant, carrying that authorization's `app`
/// and tenant id — whichever side of the documented snapshot each application
/// name falls on, and a tenant it holds no authentication token for carries the
/// service's fixed no-token error and no `taskId`.
///
/// The table spans how many application tenants the bundle holds and how the two
/// provenances are mixed and ordered among them.
#[tokio::test]
async fn the_user_access_fan_out_reports_every_application_tenant() {
    // (case, the bundle's authorizations as (app, tenant))
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "one tenant, an application outside the snapshot",
            vec![("PAGERDUTY", "acme.pagerduty.example")],
        ),
        (
            "a snapshot application before one outside it",
            vec![
                ("ZENDESK", "tenant-zendesk"),
                ("PAGERDUTY", "acme.pagerduty.example"),
            ],
        ),
        (
            "an application outside the snapshot before a snapshot one",
            vec![
                ("PAGERDUTY", "acme.pagerduty.example"),
                ("SMARTSHEET", "tenant-smartsheet"),
            ],
        ),
        (
            "two tenants of the application outside the snapshot, and one snapshot application",
            vec![
                ("PAGERDUTY", "first.pagerduty.example"),
                ("SLACK", "tenant-slack"),
                ("PAGERDUTY", "second.pagerduty.example"),
            ],
        ),
    ];

    for (case, tenants) in cases {
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator).await;
        for (app, tenant) in &tenants {
            authorization(&emulator, &bundle, app, tenant, false).await;
        }

        let started = start_tasks(&emulator, &bundle, "ada.lovelace@example.com").await;
        assert_eq!(
            started.status, 201,
            "case {case:?}: StartUserAccessTasks must answer HTTP 201, got {} with body {}",
            started.status, started.body
        );
        let items = task_items(&started);
        assert_eq!(
            items.len(),
            tenants.len(),
            "case {case:?}: expected one item per application tenant ({}), got {items:?}",
            tenants.len()
        );
        for (index, (app, tenant)) in tenants.iter().enumerate() {
            let item = &items[index];
            assert_eq!(
                (member(item, "app"), member(item, "tenantId")),
                ((*app).to_string(), (*tenant).to_string()),
                "case {case:?}: item {index} must report the application tenant it is about, got \
                 {item}"
            );
            assert!(
                item.get("taskId").is_none(),
                "case {case:?}: item {index} started no task, so it must carry no taskId, got \
                 {item}"
            );
            let error = item.get("error").unwrap_or_else(|| {
                panic!("case {case:?}: expected an error in item {index}: {item}")
            });
            assert_eq!(
                (member(error, "errorCode"), member(error, "errorMessage")),
                (
                    NO_AUTHENTICATION_TOKEN_CODE.to_string(),
                    NO_AUTHENTICATION_TOKEN_MESSAGE.to_string()
                ),
                "case {case:?}: item {index} must carry the service's fixed no-token error, got \
                 {item}"
            );
        }
    }
}
