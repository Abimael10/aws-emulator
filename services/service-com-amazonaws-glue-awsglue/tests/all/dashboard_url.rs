// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `GetDashboardUrl`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: the operation dispatches on the KIND of resource the request names.
//! A `ResourceType` it serves no dashboard for is refused with
//! `OperationNotSupportedException` naming that type, before anything is looked up — so
//! the answer is the same whether or not the account holds such a resource. The kind it
//! does resolve, `SESSION`, is looked up, and an id no session of the account has is
//! refused with `EntityNotFoundException` carrying this operation's fixed, generic miss
//! text — which discloses neither the id nor the kind — rather than handed a URL.
//!
//! The rows below differ along what the rule generalises over: the resource type asked for
//! (every value of the modelled enum), whether a resource of that name exists, the form of
//! the `ResourceId`, and whether the call carries a `RequestOrigin`.

use std::sync::Arc;

use serde_json::json;
use service_com_amazonaws_glue_awsglue::state::messages::{
    dashboard_url_not_supported, DASHBOARD_URL_RESOURCE_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{already_past, job_body};
use crate::statements::{create_session, set_session_due};
use crate::wire::{call, emulator, emulator_on, error_code, error_message, ok};

/// Every value of the modelled `GlueResourceType` enum, which `ResourceType` is one of.
/// Source: `smithy_query shape=GlueResourceType`.
const RESOURCE_TYPES: [&str; 2] = ["JOB", "SESSION"];

/// The miss of this operation as Glue words it, written out here as a literal rather than
/// read from the service's own constant, so this suite pins the WORDING instead of
/// restating it: the whole sentence is fixed, with no trailing full stop.
#[test]
fn the_resource_miss_is_worded_as_glue_words_it() {
    assert_eq!(
        DASHBOARD_URL_RESOURCE_NOT_FOUND, "Entity not found",
        "unexpected GetDashboardUrl entity-miss wording"
    );
}

/// The one resource type this operation resolves rather than refusing outright.
const SESSION: &str = "SESSION";

/// A `GetDashboardUrl` call, answering the HTTP status and body.
async fn dashboard_url(
    router: &MultiServiceRouter,
    resource_id: &str,
    resource_type: &str,
    request_origin: Option<&str>,
) -> (u16, serde_json::Value) {
    let mut body = json!({ "ResourceId": resource_id, "ResourceType": resource_type });
    if let Some(origin) = request_origin {
        body["RequestOrigin"] = json!(origin);
    }
    call(router, "GetDashboardUrl", &body).await
}

/// A resource type this operation serves no dashboard for is refused as unsupported,
/// whether or not the account holds a resource of that name: the refusal is about the KIND.
///
/// The rows differ along whether the named job exists, the form of the `ResourceId` and
/// whether the call names a `RequestOrigin`.
#[tokio::test]
async fn a_resource_type_with_no_dashboard_is_refused_as_unsupported() {
    let router = emulator();
    ok(&router, "CreateJob", &job_body("etl_job", json!({}))).await;

    // (case, the resource id, the request origin)
    let cases: [(&str, &str, Option<&str>); 4] = [
        ("a job the account holds", "etl_job", None),
        ("a job name nothing created", "no_such_job", None),
        ("a job with a request origin", "etl_job", Some("console")),
        (
            "an id in another form",
            "job-with-dashes.and.dots_1",
            Some("sdk"),
        ),
    ];

    for (case, resource_id, origin) in cases {
        let (status, body) = dashboard_url(&router, resource_id, "JOB", origin).await;
        assert_eq!(
            status, 400,
            "case {case}: an unsupported resource type must be refused, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).ends_with("OperationNotSupportedException"),
            "case {case}: expected OperationNotSupportedException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            dashboard_url_not_supported("JOB"),
            "case {case}: the refusal must name the resource type it does not serve, got \
             {body}"
        );
        assert!(
            body["Url"].is_null(),
            "case {case}: a refused call hands out no URL, got {body}"
        );
    }

    // The job the refusals named is untouched: the refusal reads nothing and writes
    // nothing.
    let job = ok(&router, "GetJob", &json!({ "JobName": "etl_job" })).await;
    assert_eq!(
        job["Job"]["Name"].as_str(),
        Some("etl_job"),
        "the job must still be readable after the refusals, got {job}"
    );
}

/// A `SESSION` id no session of the account has is refused with `EntityNotFoundException`
/// carrying the operation's fixed miss text — never answered with a URL for a session
/// nothing created, and never echoing the id back.
///
/// The rows differ along the form of the id and whether a `RequestOrigin` is named.
#[tokio::test]
async fn an_unknown_session_is_refused_as_not_found() {
    let router = emulator();

    let cases: [(&str, &str, Option<&str>); 4] = [
        ("a plain id", "session-1", None),
        ("a uuid-shaped id", "11111111-2222-3333-4444-555555555555", None),
        ("an id with a request origin", "session-2", Some("console")),
        ("an id carrying dots", "team.session.42", Some("sdk")),
    ];

    for (case, resource_id, origin) in cases {
        let (status, body) = dashboard_url(&router, resource_id, SESSION, origin).await;
        assert_eq!(
            status, 400,
            "case {case}: an unknown session must be refused, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            DASHBOARD_URL_RESOURCE_NOT_FOUND,
            "case {case}: the miss must be the operation's fixed sentence, got {body}"
        );
        assert!(
            !error_message(&body).contains(resource_id),
            "case {case}: the refusal must not echo the id the request addressed, got {body}"
        );
        assert!(
            body["Url"].is_null(),
            "case {case}: a refused call hands out no URL, got {body}"
        );
    }
}

/// A session this endpoint no longer holds — one a delete removed — is the same miss as an
/// id that never existed: this operation's own fixed sentence, which is NOT the session
/// read's templated one for the same id. The two refusals carry the same code for the same
/// missing session and must still differ in wording, so they cannot share one message.
///
/// The rows differ along the form of the id the deleted session was addressed by.
#[tokio::test]
async fn a_deleted_session_is_refused_with_this_operations_own_miss_text() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    let ids = [
        "deleted-session",
        "team.session.7",
        "11111111-2222-3333-4444-555555555555",
    ];

    for id in ids {
        // A session only reaches a status a delete is allowed in once its provisioning has
        // settled, so the record is brought there before it is removed.
        create_session(&router, id, json!({})).await;
        set_session_due(&engine, id, already_past());
        let (status, body) = dashboard_url(&router, id, SESSION, None).await;
        assert_eq!(
            status, 200,
            "case {id}: a session this endpoint holds must be answered with its dashboard URL, \
             got {status} with {body}"
        );
        ok(&router, "DeleteSession", &json!({ "Id": id })).await;

        let (status, body) = dashboard_url(&router, id, SESSION, None).await;
        assert_eq!(
            status, 400,
            "case {id}: a deleted session must be refused, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {id}: expected EntityNotFoundException, got {} in {body}",
            error_code(&body)
        );
        let dashboard_miss = error_message(&body);
        assert_eq!(
            dashboard_miss, "Entity not found",
            "case {id}: the miss must be this operation's fixed sentence, got {body}"
        );
        assert!(
            !dashboard_miss.contains(id),
            "case {id}: the refusal must not echo the id the request addressed, got {body}"
        );
        assert!(
            body["Url"].is_null(),
            "case {id}: a refused call hands out no URL, got {body}"
        );

        // The session READ's own miss for the SAME id: the same error class, worded as that
        // operation words it — which is what the two messages must not share.
        let (status, read) = call(&router, "GetSession", &json!({ "Id": id })).await;
        assert_eq!(
            status, 400,
            "case {id}: the deleted session must be a miss for the session read too, got \
             {status} with {read}"
        );
        assert!(
            error_code(&read).ends_with("EntityNotFoundException"),
            "case {id}: expected EntityNotFoundException, got {} in {read}",
            error_code(&read)
        );
        let session_miss = error_message(&read);
        assert_eq!(
            session_miss,
            format!("Session {id} not found."),
            "case {id}: the session read's miss names the id it could not resolve, got {read}"
        );
        assert_ne!(
            dashboard_miss, session_miss,
            "case {id}: the dashboard miss and the session read's miss must differ"
        );
    }
}

/// Every value of the modelled resource-type enum is answered with ONE of the operation's
/// two refusal classes, and none of them with a URL: the dispatch is exhaustive over the
/// enum.
#[tokio::test]
async fn every_modelled_resource_type_is_dispatched() {
    let router = emulator();

    for resource_type in RESOURCE_TYPES {
        let (status, body) = dashboard_url(&router, "some-resource", resource_type, None).await;
        assert_eq!(
            status, 400,
            "case {resource_type}: no resource of that name exists, so the call must be \
             refused, got {status} with {body}"
        );
        let code = error_code(&body);
        let expected = if resource_type == SESSION {
            "EntityNotFoundException"
        } else {
            "OperationNotSupportedException"
        };
        assert!(
            code.ends_with(expected),
            "case {resource_type}: expected {expected}, got {code} in {body}"
        );
    }
}
