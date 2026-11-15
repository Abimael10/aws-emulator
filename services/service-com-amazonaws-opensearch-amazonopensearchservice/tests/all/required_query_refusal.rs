// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the request this API answers when its router can name no
//! operation for it: one whose method and path address an operation of this service but
//! which omits a member that operation requires in the QUERY STRING.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the resources the served legs address from [`super::data_source_attachments`] and
//! [`super::resource_tags`]; this suite adds no second copy of either.
//!
//! The RULE under test, asserted over a SET of inputs rather than over one call: a request
//! whose method and path name an operation this API serves IS that operation's request, so
//! a member the operation requires in the query string and the request does not carry is
//! refused with the modelled `ValidationException` (HTTP 400) naming that member, and the
//! same request carrying it is SERVED by that operation. A request that genuinely names no
//! operation keeps the unknown-operation answer.
//!
//! The rows span every operation of this model whose input binds a `@required`
//! `@httpQuery` member — `ListMigrations` (`applicationId`), `GetDomainMaintenanceStatus`
//! (`maintenanceId`) and `ListTags` (`arn`), from
//! `smithy_query(query="operation",operation="<Op>")` — and, within one of them, the forms
//! a request reaches that route in: the bare collection path, the path carrying the
//! operation's OTHER members and not the required one, and the path with the empty last
//! segment a read of ONE resource leaves behind when its required `@httpLabel` renders
//! empty.

use super::data_source_attachments::application;
use super::domain_lifecycle::{error_code, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS};
use super::resource_tags::{domain_arn, list_tags};

/// The sentence AWS refuses an absent required member with, over the name the request
/// carries that member under. The fixed text is the envelope's; the only variable part is
/// the member.
fn missing_member_message(member: &str) -> String {
    format!(
        "1 validation error detected: Value null at '{member}' failed to satisfy constraint: \
         Member must not be null"
    )
}

/// A `maintenanceId` nobody minted, inside `RequestId`'s `@length(min: 1, max: 100)`.
const UNMINTED_MAINTENANCE_ID: &str = "0123456789abcdef0123456789abcdef";

/// RULE: a request addressing an operation's route without the member that operation
/// requires in the query string is refused as a validation error naming that member.
#[tokio::test]
async fn a_request_omitting_a_required_query_member_is_refused_as_a_validation_error() {
    let emulator = Emulator::new();

    // (case, the request target, the member the route requires and the request omits)
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "ListMigrations with no query string at all",
            format!("{PREFIX}/opensearch/app-migrations"),
            "applicationId",
        ),
        (
            "ListMigrations carrying its page size and not the application",
            format!("{PREFIX}/opensearch/app-migrations?maxResults=5"),
            "applicationId",
        ),
        (
            "ListMigrations carrying its filter and its token and not the application",
            format!("{PREFIX}/opensearch/app-migrations?status=PENDING&nextToken=abc"),
            "applicationId",
        ),
        (
            "GetMigration with an empty migrationId, which addresses the collection",
            format!("{PREFIX}/opensearch/app-migrations/"),
            "applicationId",
        ),
        (
            "the same empty migrationId with a query string on it",
            format!("{PREFIX}/opensearch/app-migrations/?maxResults=5"),
            "applicationId",
        ),
        (
            "GetDomainMaintenanceStatus with no maintenanceId",
            format!("{PREFIX}/opensearch/domain/some-domain/domainMaintenance"),
            "maintenanceId",
        ),
        (
            "GetDomainMaintenanceStatus for a domain nobody created, with no maintenanceId",
            format!("{PREFIX}/opensearch/domain/absent-domain/domainMaintenance"),
            "maintenanceId",
        ),
        (
            "ListTags with no arn",
            format!("{PREFIX}/tags"),
            "arn",
        ),
    ];

    for (case, uri, member) in rows {
        let answer = emulator.call("GET", &uri, None).await;
        let expected = missing_member_message(member);
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, "ValidationException"),
            "case {case}: expected the modelled validation error for {uri}, got {} with {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.body["message"].as_str(),
            Some(expected.as_str()),
            "case {case}: expected {expected:?}, got {:?}",
            answer.body
        );
    }
}

/// RULE: the same request carrying the member the route requires is SERVED by that
/// operation — the refusal above is about the request, not about the operation existing.
#[tokio::test]
async fn the_same_request_carrying_the_required_member_is_served() {
    let emulator = Emulator::new();
    let id = application(&emulator, "servedquery").await;

    // The collection path and the same path with the empty last segment a read of one
    // migration leaves behind: both are the listing, and both report it.
    for (case, uri) in [
        (
            "ListMigrations on the collection path",
            format!("{PREFIX}/opensearch/app-migrations?applicationId={id}"),
        ),
        (
            "ListMigrations on the path an empty migrationId leaves",
            format!("{PREFIX}/opensearch/app-migrations/?applicationId={id}"),
        ),
    ] {
        let answer = emulator.call("GET", &uri, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["migrations"].as_array().map(Vec::len),
            Some(0),
            "case {case}: expected the empty listing of an application holding no job, got {:?}",
            answer.body
        );
    }

    // `ListTags` with the member it requires reports the resource's tags.
    let arn = domain_arn(&emulator, "served-tags").await;
    let tags = list_tags(&emulator, &arn).await;
    assert_eq!(
        tags.status, 200,
        "case ListTags({arn}): expected 200, got {} with {:?}",
        tags.status, tags.body
    );
    assert!(
        tags.body["TagList"].is_array(),
        "case ListTags({arn}): expected a TagList, got {:?}",
        tags.body
    );

    // `GetDomainMaintenanceStatus` with the member it requires is served by the
    // operation, which then answers about the ACTION the id names: an id nobody minted
    // names no action, which is the modelled missing-resource refusal rather than a
    // complaint about the request.
    let status = emulator
        .call(
            "GET",
            &format!(
                "{PREFIX}/opensearch/domain/served-tags/domainMaintenance\
                 ?maintenanceId={UNMINTED_MAINTENANCE_ID}"
            ),
            None,
        )
        .await;
    assert_eq!(
        (status.status, error_code(&status).as_str()),
        (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
        "case GetDomainMaintenanceStatus(an id nobody minted): expected the modelled \
         missing-resource refusal, got {} with {:?}",
        status.status,
        status.body
    );
}

/// RULE: a request that addresses no route of this model at all keeps the
/// unknown-operation answer — the refusal above is earned by addressing an operation's own
/// route, not by sharing a prefix with one.
#[tokio::test]
async fn a_request_addressing_no_route_keeps_the_unknown_operation_answer() {
    let emulator = Emulator::new();

    // (case, method, the request target)
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a path one segment deeper than any route",
            "GET",
            format!("{PREFIX}/opensearch/app-migrations/one/two"),
        ),
        (
            "a collection this model binds nothing under",
            "GET",
            format!("{PREFIX}/opensearch/no-such-collection"),
        ),
        (
            "the migrations collection under a method it binds nothing for",
            "DELETE",
            format!("{PREFIX}/opensearch/app-migrations"),
        ),
        (
            "the tags family under a method it binds nothing for",
            "DELETE",
            format!("{PREFIX}/tags"),
        ),
    ];

    for (case, method, uri) in rows {
        let answer = emulator.call(method, &uri, None).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (501, "NotImplemented"),
            "case {case}: expected the unknown-operation answer for {method} {uri}, got {} \
             with {:?}",
            answer.status,
            answer.body
        );
    }
}
