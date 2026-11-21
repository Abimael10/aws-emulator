// SPDX-License-Identifier: CC0-1.0

//! Detaching a resolver, end to end over HTTP: what `DeleteResolver` removes, what it
//! leaves, and how it refuses a pair that addresses no resolver.
//!
//! The rule asserted here is stated over the CLASS of addressed resolvers rather than
//! over one request: a delete removes exactly the resolver attached to the field of the
//! type it names under the API it names, so afterwards every reader of that field reports
//! no resolver, the listing of its type no longer carries it, the field may carry a new
//! resolver again and what the detached resolver used is no longer in use; and a pair
//! that carries no resolver — never attached, already detached, attached under another
//! type or under another API — is a 404 `NotFoundException` with AppSync's fixed
//! no-resolver text that detaches nothing. The API the pair sits under is resolved in
//! either form AppSync publishes an API's identity in, so a reference that is neither is
//! a 400 `BadRequestException` naming it and one of an API this account does not hold a
//! 404.
//!
//! The rows differ along what the rule generalises over — the DEFINITION the deleted
//! resolver was written with — because a delete addresses a resolver by its type and
//! field whatever the resolver is defined by. They are the listing suite's own rows
//! (`crate::resolvers`), the one resolver-definition table this test crate keeps: both
//! `ResolverKind` values and a request that names none, VTL mapping templates and `code`
//! with a `runtime`, cached and versioned resolvers, both published `maxBatchSize` bounds,
//! both metrics configurations, and templates carrying non-ASCII text.
//!
//! A delete is asserted here to address a resolver by the type/field PAIR it is attached
//! to and nothing else: a type that carries no resolver takes the same no-resolver 404 as
//! a field that carries none, because this operation reads what is attached rather than
//! the API's declared type system (whether a `typeName` the schema does not declare is
//! instead refused as a missing TYPE is the subject of the open `ListResolvers` report,
//! and is to be revisited here together with it).
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the boundary validator, the handlers
//! and the shared engine together. The harness (`router`, `call`, the API with its data
//! source and function, the resolver create/read helpers, the path-label encoder and the
//! empty-200 and no-resolver assertions) is the sibling suites' own, reused rather than
//! copied. What a detached resolver RELEASES — the function a deleted pipeline resolver
//! invoked, which its own delete then goes through for — is asserted by
//! `crate::plane_dependency_refusals`, so it is not restated here.

use serde_json::Value;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::api_identifier_resolution::label;
use super::api_key_deletes_unmoved_by_other_calls::api_arn;
use super::error_contract::{call, router, Answer};
use super::plane_dependency_refusals::assert_empty_ok;
use super::resolver_caching_config::{assert_no_resolver, NO_RESOLVER};
use super::resolvers::{
    api_with_dependencies, create_resolver_body, get_resolver, listed_fields as fields_of,
    request_body, rows, Row, TYPE,
};

/// AppSync's fixed text for an `apiId` of the right shape that names no API this account
/// holds. Written out here rather than read off the service's own constant, so a change
/// of wording is caught rather than followed.
const API_NOT_FOUND: &str = "API not found.";

/// A second type of the same API, so a delete addressed to one type is not a delete of
/// the same field name under another.
const OTHER_TYPE: &str = "Mutation";

/// More entries than any listing these cases walk holds, which bounds that walk.
const MOST_ENTRIES_LISTED: usize = 64;

/// Attach `row` to `field` of `type_name`, asserting the create was served.
async fn attach(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
    field: &str,
    row: &Row,
    function_id: &str,
) {
    let created = create_resolver_body(
        router,
        api_id,
        type_name,
        request_body(row, field, function_id),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case attach {} to {type_name}/{field}: expected CreateResolver 200, got {created:?}",
        row.label
    );
}

/// Send one `DeleteResolver` for the resolver `type_name`/`field` addresses under `api`.
///
/// The API reference travels in the single non-greedy `{apiId}` path segment, so it is
/// encoded the way an AWS SDK sends one (`label`, the API-identifier suite's own encoder):
/// an ARN carries `/`, which belongs to the reference rather than to the path.
pub(crate) async fn delete_resolver(
    router: &MultiServiceRouter,
    api: &str,
    type_name: &str,
    field: &str,
) -> Answer {
    call(
        router,
        "DELETE",
        &format!(
            "/v1/apis/{}/types/{type_name}/resolvers/{field}",
            label(api)
        ),
        None,
    )
    .await
}

/// The field names `ListResolvers` reports for `type_name` of `api_id`, walked to the
/// last page so the answer is the whole listing rather than its first page.
///
/// The sibling listing suite's walk (`crate::resolvers`) is stated over an EXPECTED total,
/// which is what it asserts the paging against; these cases know only that a field is or is
/// not among the entries, so the walk here takes no total and is bounded by the entries the
/// cases attach.
async fn listed_fields(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    type_name: &str,
) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // A page carries at least one entry while entries remain, so a walk cannot take more
    // pages than the listing holds entries: a bound above the entries these cases attach
    // (two per row of the table, plus the one each re-attaches) is reached only by a cursor
    // that does not advance.
    for _ in 0..=MOST_ENTRIES_LISTED {
        let path = match &token {
            Some(next) => format!("/v1/apis/{api_id}/types/{type_name}/resolvers?nextToken={next}"),
            None => format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
        };
        let answer = call(router, "GET", &path, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected ListResolvers 200, got {answer:?}"
        );
        let page: Vec<Value> = answer.body["resolvers"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected a page of resolvers, got {:?}",
                    answer.body
                )
            })
            .clone();
        // The entries are projected through the listing suite's own reading of a page.
        fields.extend(fields_of(&page));
        match answer.body["nextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return fields,
        }
    }
    panic!("case {case}: the listing of {type_name} never reached a last page");
}

/// A delete detaches the resolver its type and field address and nothing else, whatever
/// definition that resolver was written with, and the field it freed may carry one again.
#[tokio::test]
async fn a_delete_detaches_only_the_resolver_it_addresses_whatever_its_definition() {
    let router = router();
    let (api_id, function_id) = api_with_dependencies(&router, "Api_delete_resolver_table").await;

    for (index, row) in rows().iter().enumerate() {
        let label = row.label;
        let target = format!("target_{index}");
        let sibling = format!("sibling_{index}");

        // Three resolvers created from the same row: the one the delete addresses, one on
        // another field of the same type, and one on the SAME field name under another
        // type of the same API.
        attach(&router, &api_id, TYPE, &target, row, &function_id).await;
        attach(&router, &api_id, TYPE, &sibling, row, &function_id).await;
        attach(&router, &api_id, OTHER_TYPE, &target, row, &function_id).await;
        let before = get_resolver(&router, &api_id, TYPE, &target).await;
        assert_eq!(
            before.status, 200,
            "case {label}: expected the attached resolver to be served first, got {before:?}"
        );

        let deleted = delete_resolver(&router, &api_id, TYPE, &target).await;
        assert_empty_ok(label, &deleted);

        // The field carries no resolver any more, so every reader of it answers without
        // one.
        let read = get_resolver(&router, &api_id, TYPE, &target).await;
        assert_no_resolver(&format!("{label} after the delete"), &read);
        let listed = listed_fields(&router, label, &api_id, TYPE).await;
        assert!(
            !listed.contains(&target),
            "case {label}: expected {target:?} gone from the listing of {TYPE}, got {listed:?}"
        );
        assert!(
            listed.contains(&sibling),
            "case {label}: expected {sibling:?} still listed under {TYPE}, got {listed:?}"
        );

        // The same field name of another type addresses a different resolver, which the
        // delete left where it was.
        let elsewhere = get_resolver(&router, &api_id, OTHER_TYPE, &target).await;
        assert_eq!(
            elsewhere.status, 200,
            "case {label}: expected {OTHER_TYPE}/{target} untouched by the delete, got \
             {elsewhere:?}"
        );

        // The field is free again, so a create may attach a new resolver to it and the
        // reads serve that one.
        attach(&router, &api_id, TYPE, &target, row, &function_id).await;
        let reattached = get_resolver(&router, &api_id, TYPE, &target).await;
        assert_eq!(
            reattached.status, 200,
            "case {label}: expected the re-attached resolver to be served, got {reattached:?}"
        );
    }
}

/// A pair that carries no resolver names no resource to delete, so the call is AppSync's
/// fixed no-resolver 404 and nothing is detached by it.
#[tokio::test]
async fn a_pair_that_carries_no_resolver_is_a_not_found_that_detaches_nothing() {
    let router = router();
    let (api_id, function_id) = api_with_dependencies(&router, "Api_delete_resolver_absent").await;
    let (other_api, other_function) =
        api_with_dependencies(&router, "Api_delete_resolver_absent_other").await;
    let row = rows()[0].clone();

    // The resolvers that DO exist: one under this API's type, one on the same type and
    // field of ANOTHER API, one on another type of this API, and one this test detaches
    // before the table runs.
    attach(&router, &api_id, TYPE, "attached", &row, &function_id).await;
    attach(
        &router,
        &other_api,
        TYPE,
        "elsewhere",
        &row,
        &other_function,
    )
    .await;
    attach(
        &router,
        &api_id,
        OTHER_TYPE,
        "other_type_only",
        &row,
        &function_id,
    )
    .await;
    attach(&router, &api_id, TYPE, "detached", &row, &function_id).await;
    let detached = delete_resolver(&router, &api_id, TYPE, "detached").await;
    assert_empty_ok("the resolver detached before the table", &detached);

    let cases: &[(&str, &str, &str)] = &[
        (
            "a field no create ever attached a resolver to",
            TYPE,
            "never_attached",
        ),
        (
            "a field an earlier delete already detached",
            TYPE,
            "detached",
        ),
        (
            "a field carrying a resolver under another type of the same API",
            TYPE,
            "other_type_only",
        ),
        (
            "a field carrying a resolver under another API",
            TYPE,
            "elsewhere",
        ),
        (
            "a type of the API that carries no resolver at all",
            "Subscription",
            "attached",
        ),
    ];
    for (case, type_name, field) in cases {
        let refused = delete_resolver(&router, &api_id, type_name, field).await;
        assert_eq!(
            (
                refused.status,
                refused.error_type.as_str(),
                refused.message.as_str()
            ),
            (404, "NotFoundException", NO_RESOLVER),
            "case {case}: expected 404 NotFoundException {NO_RESOLVER:?}, got {refused:?}"
        );
        assert!(
            !refused.message.contains(field)
                && !refused.message.contains(type_name)
                && !refused.message.contains(api_id.as_str()),
            "case {case}: expected nothing of the request quoted, got {:?}",
            refused.message
        );
    }

    // Every resolver that existed before the table is still served after it, so no
    // refusal detached anything.
    for (case, api, type_name, field) in [
        ("the resolver under the API addressed", api_id.as_str(), TYPE, "attached"),
        (
            "the resolver under the other type",
            api_id.as_str(),
            OTHER_TYPE,
            "other_type_only",
        ),
        (
            "the resolver under the other API",
            other_api.as_str(),
            TYPE,
            "elsewhere",
        ),
    ] {
        let read = get_resolver(&router, api, type_name, field).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected it still served after the refusals, got {read:?}"
        );
    }
}

/// The API a delete names is resolved in either form AppSync publishes an API's identity
/// in, and a reference that names no API this account holds detaches nothing.
#[tokio::test]
async fn a_delete_resolves_its_api_in_either_published_form_and_refuses_any_other() {
    let router = router();
    let (api_id, function_id) =
        api_with_dependencies(&router, "Api_delete_resolver_reference").await;
    let arn = api_arn(&router, &api_id).await;
    let row = rows()[0].clone();
    attach(&router, &api_id, TYPE, "by_arn", &row, &function_id).await;
    attach(&router, &api_id, TYPE, "kept", &row, &function_id).await;

    // An `apiId` longer than an AppSync API id holds more characters than one has, so it
    // names no API at all and the call is malformed; one of the right length this account
    // simply does not hold names nothing.
    let unnameable = "z".repeat(state::API_ID_LEN + 1);
    let absent = "z".repeat(state::API_ID_LEN);
    for (case, api, status, error, message) in [
        (
            "an apiId longer than an AppSync API id",
            unnameable.as_str(),
            400,
            "BadRequestException",
            format!("Invalid API ID {unnameable}"),
        ),
        (
            "an apiId of the right length this account does not hold",
            absent.as_str(),
            404,
            "NotFoundException",
            API_NOT_FOUND.to_string(),
        ),
    ] {
        let refused = delete_resolver(&router, api, TYPE, "kept").await;
        assert_eq!(
            (
                refused.status,
                refused.error_type.as_str(),
                refused.message.as_str()
            ),
            (status, error, message.as_str()),
            "case {case}: expected {status} {error} {message:?}, got {refused:?}"
        );
    }
    for field in ["kept", "by_arn"] {
        let read = get_resolver(&router, &api_id, TYPE, field).await;
        assert_eq!(
            read.status, 200,
            "case {field:?} after the refused deletes: expected it still served, got {read:?}"
        );
    }

    // The ARN AppSync published for the API names the same API, so a delete addressed by
    // it detaches the same resolver the id form addresses.
    let deleted = delete_resolver(&router, &arn, TYPE, "by_arn").await;
    assert_empty_ok("a delete addressed by the API's ARN", &deleted);
    let read = get_resolver(&router, &api_id, TYPE, "by_arn").await;
    assert_no_resolver("after a delete addressed by the API's ARN", &read);
    let kept = get_resolver(&router, &label(&arn), TYPE, "kept").await;
    assert_eq!(
        kept.status, 200,
        "case the resolver the ARN-addressed delete did not name: expected it still served, got \
         {kept:?}"
    );
}
