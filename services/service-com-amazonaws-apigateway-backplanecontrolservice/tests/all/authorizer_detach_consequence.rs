// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DEPENDENCY an `UpdateMethod` patch leaves — or does not leave —
//! between a `Method` and the `Authorizer` it names, and for what that authorizer's id
//! resolves to once the delete the patch freed has taken the record away.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client sees,
//! not what a handler returns. The harness, the `CreateAuthorizer`/`GetAuthorizer` callers,
//! the `PutMethod` caller, the patch builders and the `DeleteAuthorizer` caller are this
//! crate's existing ones, shared with the `authorizer_lifecycle`, `authorizer_in_use`,
//! `execute_api_data_plane` and `patch_updates` suites.
//!
//! THE RULE UNDER TEST, one level above any single recorded sequence:
//!
//! > `Method$authorizerId` is "the identifier of an Authorizer to use on this method. The
//! > `authorizationType` must be `CUSTOM`", so a method DEPENDS on an authorizer only while
//! > the method's CURRENTLY STORED state names it under an authorization type an `Authorizer`
//! > decides. The reference guard therefore reads the live method record: a patch list the
//! > service APPLIED decides the dependency by what it left behind, and a patch list the
//! > service REFUSED decides nothing at all, because a refused list leaves the stored method
//! > exactly as it was. `DeleteAuthorizer` is refused with the modelled `ConflictException`
//! > (409) exactly while such a dependency stands and accepted (202) as soon as none does,
//! > after which the id names no record for ANY reader of it.
//!
//! The rows differ along what the rule generalises over: HOW the patch list moves the method
//! off — or fails to move it off — the authorizer (the type alone, the type and the id in one
//! list, the two ops in either order, a list that moves it away and back, a list whose later
//! op is refused for an unknown member, one refused for an unresolvable model reference, and
//! one that would leave an authorizer-backed method naming no authorizer at all) — and, in the
//! second test, WHICH caller resolves the deleted id (a read, a patch, a second delete, the
//! collection, a method that would name it, and a patch that would re-point a method at it).
//!
//! Why it matters: an authorizer is replaced by detaching it from its methods and deleting it,
//! so a guard that keeps a dependency a patch already cleared — or drops one a refused patch
//! never cleared — either wedges every teardown on a permanent 409 or deletes a record a live
//! method still asks for.
//!
//! What this suite OWNS, beside the sibling `authorizer_detached_from_a_method` suite (which
//! spans the authorizer types, the verbs, the resources and the reference count for a patch
//! the service APPLIES): the lists the service REFUSES, which have to leave the dependency
//! standing, and what the deleted id resolves to for each of its callers. The applied rows are
//! kept as the CONTROL those refused rows are read against — "a refused list frees nothing" says
//! nothing on its own unless an applied list in the same table frees the delete — and the first
//! of them is the detach-then-delete sequence this suite was written for, as one row of the set.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::ops::put_method::MISSING_METHOD_AUTHORIZER;
use service_com_amazonaws_apigateway_backplanecontrolservice::state::NO_SUCH_AUTHORIZER;

use crate::authorizer_in_use::delete_authorizer;
use crate::authorizer_lifecycle::{created_authorizer, get_authorizer, lambda_uri};
use crate::execute_api_data_plane::put_method;
use crate::patch_updates::{op, patch, replace};
use crate::rest_api_lifecycle::{
    assert_failure, create_api, page_items, resource_under, string_member, Gateway,
};

/// The modelled error code a delete the API's own records block carries.
const CONFLICT: &str = "ConflictException";

/// The modelled error code a request the service cannot apply carries.
const BAD_REQUEST: &str = "BadRequestException";

/// The modelled error code an identifier that names no record carries.
const NOT_FOUND: &str = "NotFoundException";

/// The verb and the path part every method below sits on: the method's identity is not what
/// these rules generalise over (the sibling `authorizer_detached_from_a_method` suite spans
/// the verbs and the resources), so it is fixed here and the patch list is what varies.
const VERB: &str = "GET";
const PATH_PART: &str = "secure";

/// A `remove` entry, which carries no `value` — "`value`: … applicable for the add or replace
/// operation" (`smithy_query(query="doc", shape="PatchOperation", member="value")`).
///
/// Local, as the sibling suites that need the same entry keep it local
/// (`stage_logging_role_current_setting`, `stage_canary_and_patch_grammar`): every apigateway
/// fix in flight edits the shared `patch_updates` helpers, and widening them for one entry costs
/// this fix a serialized squash merge. The ops this suite shares with them — `op`, `replace` and
/// the PATCH caller — are imported from there rather than copied.
fn remove(path: &str) -> Value {
    op("remove", path, None)
}

/// A `TOKEN` authorizer request, named `name`.
fn token_authorizer(name: &str) -> Value {
    json!({
        "name": name,
        "type": "TOKEN",
        "identitySource": "method.request.header.Auth",
        "authorizerUri": lambda_uri("authorize"),
    })
}

/// One patch list sent to a `CUSTOM` method that names the authorizer, and what the rule owes
/// afterwards.
struct Row {
    case: &'static str,
    /// The `patchOperations` the row sends to `UpdateMethod`.
    operations: Vec<Value>,
    /// The status `UpdateMethod` answers: the modelled 200 for a list it applies, 400 for one
    /// it refuses.
    patch_status: u16,
    /// The message a refused list states, for a row whose refusal has a sentence of its own.
    refusal: Option<&'static str>,
    /// The `authorizationType` the stored method carries once the list was answered.
    authorization_type: &'static str,
    /// Whether the stored method still reports an `authorizerId` afterwards.
    keeps_the_id: bool,
    /// Whether the authorizer is still DEPENDED on, which is what decides the delete.
    still_used: bool,
}

/// The table both the patch answer and the delete that follows it are asserted over.
fn table() -> Vec<Row> {
    vec![
        Row {
            case: "the type moved to NONE on its own, leaving the id behind",
            operations: vec![replace("/authorizationType", "NONE")],
            patch_status: 200,
            refusal: None,
            authorization_type: "NONE",
            keeps_the_id: true,
            still_used: false,
        },
        Row {
            case: "the type moved to NONE and the id removed in one list",
            operations: vec![
                replace("/authorizationType", "NONE"),
                remove("/authorizerId"),
            ],
            patch_status: 200,
            refusal: None,
            authorization_type: "NONE",
            keeps_the_id: false,
            still_used: false,
        },
        Row {
            case: "the id removed before the type moved, in the same list",
            operations: vec![
                remove("/authorizerId"),
                replace("/authorizationType", "NONE"),
            ],
            patch_status: 200,
            refusal: None,
            authorization_type: "NONE",
            keeps_the_id: false,
            still_used: false,
        },
        Row {
            case: "the id removed while the type stays AWS_IAM, which asks no authorizer",
            operations: vec![
                replace("/authorizationType", "AWS_IAM"),
                remove("/authorizerId"),
            ],
            patch_status: 200,
            refusal: None,
            authorization_type: "AWS_IAM",
            keeps_the_id: false,
            still_used: false,
        },
        Row {
            case: "a list that moves the type away and back again",
            operations: vec![
                replace("/authorizationType", "NONE"),
                replace("/authorizationType", "CUSTOM"),
            ],
            patch_status: 200,
            refusal: None,
            authorization_type: "CUSTOM",
            keeps_the_id: true,
            still_used: true,
        },
        Row {
            case: "a list whose later op names no member of a Method",
            operations: vec![
                replace("/authorizationType", "NONE"),
                replace("/notAMemberOfMethod", "whatever"),
            ],
            patch_status: 400,
            refusal: None,
            authorization_type: "CUSTOM",
            keeps_the_id: true,
            still_used: true,
        },
        Row {
            case: "a list whose later op names a model the API does not hold",
            operations: vec![
                replace("/authorizationType", "NONE"),
                replace("/requestModels/application~1json", "NoSuchModel"),
            ],
            patch_status: 400,
            refusal: None,
            authorization_type: "CUSTOM",
            keeps_the_id: true,
            still_used: true,
        },
        Row {
            case: "a list that would leave a CUSTOM method naming no authorizer at all",
            operations: vec![remove("/authorizerId")],
            patch_status: 400,
            refusal: Some(MISSING_METHOD_AUTHORIZER),
            authorization_type: "CUSTOM",
            keeps_the_id: true,
            still_used: true,
        },
        Row {
            case: "a list that would set the id to the empty string, which names none",
            operations: vec![replace("/authorizerId", "")],
            patch_status: 400,
            refusal: Some(MISSING_METHOD_AUTHORIZER),
            authorization_type: "CUSTOM",
            keeps_the_id: true,
            still_used: true,
        },
    ]
}

/// `GetMethod` for the one method each row stands up, asserting the modelled 200.
async fn get_method(gateway: &Gateway, api: &str, resource: &str, case: &str) -> Value {
    let answer = gateway
        .call(
            "GET",
            &format!("/restapis/{api}/resources/{resource}/methods/{VERB}"),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected GetMethod to answer 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

#[tokio::test]
async fn the_delete_guard_reads_the_method_the_patch_left_behind() {
    for row in table() {
        let gateway = Gateway::new();
        let (api, root) = create_api(&gateway, "authorizer-detach-consequence").await;
        let resource = resource_under(&gateway, &api, &root, PATH_PART).await;
        let created = created_authorizer(&gateway, &api, row.case, token_authorizer("detach")).await;
        let id = string_member(&created, "id", row.case);
        put_method(
            &gateway,
            &api,
            &resource,
            VERB,
            json!({ "authorizationType": "CUSTOM", "authorizerId": id.clone() }),
        )
        .await;

        // As written, the method is authorized BY the authorizer, so the delete is blocked.
        assert_failure(
            &delete_authorizer(&gateway, &api, &id).await,
            409,
            CONFLICT,
            &format!("{} before the patch", row.case),
        );

        let answered = patch(
            &gateway,
            &format!("/restapis/{api}/resources/{resource}/methods/{VERB}"),
            row.operations.clone(),
        )
        .await;
        assert_eq!(
            answered.status, row.patch_status,
            "case {}: expected UpdateMethod to answer {}, got {} with {:?}",
            row.case, row.patch_status, answered.status, answered.body
        );
        if row.patch_status != 200 {
            assert_eq!(
                answered.error.as_deref(),
                Some(BAD_REQUEST),
                "case {}: expected {BAD_REQUEST}, got {:?}",
                row.case,
                answered.error
            );
        }
        if let Some(refusal) = row.refusal {
            assert_eq!(
                answered.body["message"].as_str(),
                Some(refusal),
                "case {}: expected the refusal {refusal:?}, got {:?}",
                row.case,
                answered.body["message"]
            );
        }

        // What the method now IS — the record the guard has to read — whether the list was
        // applied or refused.
        let stored = get_method(&gateway, &api, &resource, row.case).await;
        assert_eq!(
            stored["authorizationType"].as_str(),
            Some(row.authorization_type),
            "case {}: expected the stored authorizationType {:?}, got {:?}",
            row.case,
            row.authorization_type,
            stored["authorizationType"]
        );
        let expected_id = if row.keeps_the_id {
            json!(id.clone())
        } else {
            Value::Null
        };
        assert_eq!(
            stored["authorizerId"], expected_id,
            "case {}: expected the stored authorizerId {expected_id:?}, got {:?}",
            row.case, stored["authorizerId"]
        );

        let answer = delete_authorizer(&gateway, &api, &id).await;
        if row.still_used {
            assert_failure(
                &answer,
                409,
                CONFLICT,
                &format!("{} after the patch", row.case),
            );
            let read = get_authorizer(&gateway, &api, &id).await;
            assert_eq!(
                read.body, created,
                "case {}: expected the authorizer untouched by the refusal, got {:?}",
                row.case, read.body
            );
        } else {
            assert_eq!(
                answer.status, 202,
                "case {}: expected 202 once no method of the API uses the authorizer, got {} \
                 with {:?}",
                row.case, answer.status, answer.body
            );
            let read = get_authorizer(&gateway, &api, &id).await;
            assert_failure(&read, 404, NOT_FOUND, &format!("{} deleted", row.case));
            assert_eq!(
                read.body["message"].as_str(),
                Some(NO_SUCH_AUTHORIZER),
                "case {}: expected the sentence {NO_SUCH_AUTHORIZER:?}, got {:?}",
                row.case,
                read.body["message"]
            );
        }

        // The method itself is the one the row left behind either way: neither the delete nor
        // the refusal rewrites it.
        let after = get_method(&gateway, &api, &resource, row.case).await;
        assert_eq!(
            after["authorizationType"].as_str(),
            Some(row.authorization_type),
            "case {}: expected the method's own authorizationType after the delete, got {:?}",
            row.case,
            after["authorizationType"]
        );
    }
}

/// One caller that resolves an `authorizerId`: the request it sends and the refusal the
/// deleted id earns from it.
struct Reader {
    case: &'static str,
    method: &'static str,
    /// The request body, or `None` for a caller that sends none.
    body: Option<Value>,
    status: u16,
    code: &'static str,
}

#[tokio::test]
async fn the_deleted_id_names_no_record_for_any_caller_that_resolves_it() {
    // The consequence of the accepted delete, read through every caller of the id rather than
    // through one: the record is GONE, so nothing resolves it and no method can be pointed at
    // it again — while the API's OTHER authorizer, which nothing deleted, still resolves.
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "authorizer-detach-readers").await;
    let resource = resource_under(&gateway, &api, &root, PATH_PART).await;
    let deleted = created_authorizer(
        &gateway,
        &api,
        "the detached authorizer",
        token_authorizer("detached"),
    )
    .await;
    let deleted_id = string_member(&deleted, "id", "the detached authorizer");
    let kept = created_authorizer(
        &gateway,
        &api,
        "the authorizer nothing deletes",
        token_authorizer("kept"),
    )
    .await;
    let kept_id = string_member(&kept, "id", "the authorizer nothing deletes");

    let method_uri = format!("/restapis/{api}/resources/{resource}/methods/{VERB}");
    put_method(
        &gateway,
        &api,
        &resource,
        VERB,
        json!({ "authorizationType": "CUSTOM", "authorizerId": deleted_id.clone() }),
    )
    .await;
    let detached = patch(
        &gateway,
        &method_uri,
        vec![
            replace("/authorizationType", "NONE"),
            remove("/authorizerId"),
        ],
    )
    .await;
    assert_eq!(
        detached.status, 200,
        "case detaching the method: expected 200, got {} with {:?}",
        detached.status, detached.body
    );
    let answer = delete_authorizer(&gateway, &api, &deleted_id).await;
    assert_eq!(
        answer.status, 202,
        "case deleting the detached authorizer: expected 202, got {} with {:?}",
        answer.status, answer.body
    );

    let authorizer_uri = format!("/restapis/{api}/authorizers/{deleted_id}");
    let readers = [
        Reader {
            case: "GetAuthorizer",
            method: "GET",
            body: None,
            status: 404,
            code: NOT_FOUND,
        },
        Reader {
            case: "UpdateAuthorizer",
            method: "PATCH",
            body: Some(json!({
                "patchOperations": [replace("/name", "renamed")],
            })),
            status: 404,
            code: NOT_FOUND,
        },
        Reader {
            case: "a second DeleteAuthorizer",
            method: "DELETE",
            body: None,
            status: 404,
            code: NOT_FOUND,
        },
    ];
    for reader in &readers {
        let answer = gateway
            .call(reader.method, &authorizer_uri, reader.body.clone())
            .await;
        assert_failure(&answer, reader.status, reader.code, reader.case);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(NO_SUCH_AUTHORIZER),
            "case {}: expected the sentence {NO_SUCH_AUTHORIZER:?}, got {:?}",
            reader.case,
            answer.body["message"]
        );
    }

    // A reference to the deleted id cannot be STORED either, through a create or a patch: it
    // would authorize a method with a record the API no longer holds.
    let put = gateway
        .call(
            "PUT",
            &format!("/restapis/{api}/resources/{root}/methods/{VERB}"),
            Some(json!({
                "authorizationType": "CUSTOM",
                "authorizerId": deleted_id.clone(),
            })),
        )
        .await;
    assert_failure(&put, 400, BAD_REQUEST, "PutMethod naming the deleted id");
    assert_eq!(
        put.body["message"].as_str(),
        Some(NO_SUCH_AUTHORIZER),
        "case PutMethod naming the deleted id: expected the sentence \
         {NO_SUCH_AUTHORIZER:?}, got {:?}",
        put.body["message"]
    );
    let repointed = patch(
        &gateway,
        &method_uri,
        vec![
            replace("/authorizationType", "CUSTOM"),
            replace("/authorizerId", &deleted_id),
        ],
    )
    .await;
    assert_failure(
        &repointed,
        400,
        BAD_REQUEST,
        "UpdateMethod re-pointing at the deleted id",
    );
    assert_eq!(
        repointed.body["message"].as_str(),
        Some(NO_SUCH_AUTHORIZER),
        "case UpdateMethod re-pointing at the deleted id: expected the sentence \
         {NO_SUCH_AUTHORIZER:?}, got {:?}",
        repointed.body["message"]
    );

    // The collection reports exactly the authorizers the API still holds, and the one nothing
    // deleted still resolves through its own reader.
    let listed = gateway
        .call("GET", &format!("/restapis/{api}/authorizers"), None)
        .await;
    assert_eq!(
        listed.status, 200,
        "case GetAuthorizers: expected 200, got {} with {:?}",
        listed.status, listed.body
    );
    let ids: Vec<String> = page_items(&listed.body, "GetAuthorizers")
        .iter()
        .map(|item| string_member(item, "id", "GetAuthorizers"))
        .collect();
    assert_eq!(
        ids,
        vec![kept_id.clone()],
        "case GetAuthorizers: expected only the authorizer nothing deleted, got {ids:?}"
    );
    let read = get_authorizer(&gateway, &api, &kept_id).await;
    assert_eq!(
        read.body, kept,
        "case GetAuthorizer on the kept authorizer: expected the record as created, got {:?}",
        read.body
    );
    // And the method the detach left behind is still the API's own open method.
    let stored = get_method(&gateway, &api, &resource, "the detached method").await;
    assert_eq!(
        stored["authorizationType"].as_str(),
        Some("NONE"),
        "case the detached method: expected the type the patch set, got {:?}",
        stored["authorizationType"]
    );
}
