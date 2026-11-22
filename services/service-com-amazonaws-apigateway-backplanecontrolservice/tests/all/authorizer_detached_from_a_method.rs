// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteAuthorizer` once the method that NAMES the authorizer no
//! longer uses it.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness, the `CreateAuthorizer`/`GetAuthorizer`
//! callers, the `PutMethod` caller, the `UpdateMethod` patch builder and the
//! `DeleteAuthorizer` caller are this crate's existing ones, shared with the
//! `authorizer_lifecycle`, `execute_api_data_plane`, `patch_updates` and `authorizer_in_use`
//! suites.
//!
//! The RULE under test, one level above the recorded case:
//!
//! > `Method$authorizerId` is "the identifier of an Authorizer to use on this method. The
//! > `authorizationType` must be `CUSTOM`", so a stored method DEPENDS on an authorizer only
//! > while its own `authorizationType` is one an `Authorizer` authorizes (`CUSTOM`,
//! > `COGNITO_USER_POOLS`). A method whose type is decided without asking any authorizer
//! > (`NONE`, `AWS_IAM`) uses none, whether it was written that way or moved there by
//! > `UpdateMethod`, so nothing of the API depends on the authorizer and the delete is
//! > accepted — while a method still under an authorizer-backed type keeps it refused with
//! > the modelled `ConflictException`.
//!
//! The rows differ along what the rule generalises over: the authorizer's own type, the
//! `authorizationType` the method is written with, the type it is MOVED to (each documented
//! value, including the two that keep the dependency standing), whether the method sits on
//! the API's root resource or a child of it, its HTTP verb, and how many methods used the
//! authorizer.

use serde_json::{json, Value};

use crate::authorizer_in_use::delete_authorizer;
use crate::authorizer_lifecycle::{created_authorizer, get_authorizer, lambda_uri, user_pool_arn};
use crate::execute_api_data_plane::put_method;
use crate::patch_updates::{patch, replace};
use crate::rest_api_lifecycle::{
    assert_failure, create_api, resource_under, string_member, Gateway,
};

/// The modelled error code a delete the API's own records block carries.
const CONFLICT: &str = "ConflictException";

/// Whether a method under `authorization_type` is authorized BY an `Authorizer` of its API.
///
/// The catalog is the member's own documented value list: "Valid values are `NONE` for open
/// access, `AWS_IAM` for using AWS IAM permissions, `CUSTOM` for using a custom authorizer,
/// or `COGNITO_USER_POOLS` for using a Cognito user pool"
/// (`smithy_query(query="doc", shape="Method", member="authorizationType")`) — the last two
/// ask an authorizer, the first two are decided without one.
fn uses_an_authorizer(authorization_type: &str) -> bool {
    matches!(authorization_type, "CUSTOM" | "COGNITO_USER_POOLS")
}

/// A `TOKEN` authorizer request, named `name`.
fn token_authorizer(name: &'static str) -> Value {
    json!({
        "name": name,
        "type": "TOKEN",
        "identitySource": "method.request.header.Auth",
        "authorizerUri": lambda_uri("authorize"),
    })
}

/// A `REQUEST` authorizer request, named `name`.
fn request_authorizer(name: &'static str) -> Value {
    json!({
        "name": name,
        "type": "REQUEST",
        "identitySource": "method.request.header.Auth",
        "authorizerUri": lambda_uri("authorize"),
    })
}

/// A `COGNITO_USER_POOLS` authorizer request, named `name`.
fn user_pool_authorizer(name: &'static str) -> Value {
    json!({
        "name": name,
        "type": "COGNITO_USER_POOLS",
        "identitySource": "method.request.header.Authorization",
        "providerARNs": [user_pool_arn("us-east-1_pool")],
    })
}

/// One method that NAMES an authorizer, and where its `authorizationType` ends up.
struct Row {
    case: &'static str,
    /// The `CreateAuthorizer` request for the authorizer the method names.
    authorizer: Value,
    /// The `authorizationType` the method is written with, naming the authorizer.
    put_as: &'static str,
    /// The `authorizationType` the method is MOVED to by `UpdateMethod`, or `None` for a
    /// method left exactly as it was written.
    moved_to: Option<&'static str>,
    /// Whether the method sits on the API's root resource or on a child of it.
    on_root: bool,
    /// The verb the method is put on, from the set API Gateway admits for a `Method`.
    verb: &'static str,
}

impl Row {
    /// The `authorizationType` the method carries once the row's move (if any) was made.
    fn final_type(&self) -> &'static str {
        self.moved_to.unwrap_or(self.put_as)
    }
}

/// The table every case runs over.
fn table() -> Vec<Row> {
    vec![
        Row {
            case: "a TOKEN authorizer whose CUSTOM GET on the root resource moves to NONE",
            authorizer: token_authorizer("movedtonone"),
            put_as: "CUSTOM",
            moved_to: Some("NONE"),
            on_root: true,
            verb: "GET",
        },
        Row {
            case: "a TOKEN authorizer whose CUSTOM POST moves to AWS_IAM",
            authorizer: token_authorizer("movedtoiam"),
            put_as: "CUSTOM",
            moved_to: Some("AWS_IAM"),
            on_root: false,
            verb: "POST",
        },
        Row {
            case: "a REQUEST authorizer whose CUSTOM DELETE moves to NONE",
            authorizer: request_authorizer("requestmoved"),
            put_as: "CUSTOM",
            moved_to: Some("NONE"),
            on_root: false,
            verb: "DELETE",
        },
        Row {
            case: "a user-pool authorizer whose COGNITO_USER_POOLS HEAD moves to AWS_IAM",
            authorizer: user_pool_authorizer("cognitomoved"),
            put_as: "COGNITO_USER_POOLS",
            moved_to: Some("AWS_IAM"),
            on_root: false,
            verb: "HEAD",
        },
        Row {
            case: "a TOKEN authorizer whose method moves between two authorizer-backed types",
            authorizer: token_authorizer("stillcognito"),
            put_as: "CUSTOM",
            moved_to: Some("COGNITO_USER_POOLS"),
            on_root: true,
            verb: "PUT",
        },
        Row {
            case: "a user-pool authorizer whose method moves to CUSTOM",
            authorizer: user_pool_authorizer("stillcustom"),
            put_as: "COGNITO_USER_POOLS",
            moved_to: Some("CUSTOM"),
            on_root: false,
            verb: "PATCH",
        },
        Row {
            case: "an open method that names the authorizer it never asks",
            authorizer: token_authorizer("openmethod"),
            put_as: "NONE",
            moved_to: None,
            on_root: false,
            verb: "OPTIONS",
        },
        Row {
            case: "an IAM-authorized method that names the authorizer it never asks",
            authorizer: request_authorizer("iammethod"),
            put_as: "AWS_IAM",
            moved_to: None,
            on_root: true,
            verb: "ANY",
        },
        Row {
            case: "a CUSTOM method left exactly as it was written",
            authorizer: token_authorizer("leftalone"),
            put_as: "CUSTOM",
            moved_to: None,
            on_root: false,
            verb: "GET",
        },
    ]
}

#[tokio::test]
async fn an_authorizer_no_method_uses_can_be_deleted() {
    for row in table() {
        let gateway = Gateway::new();
        let (api, root) = create_api(&gateway, "authorizer-detached").await;
        let resource = if row.on_root {
            root.clone()
        } else {
            resource_under(&gateway, &api, &root, "secure").await
        };

        let authorizer = created_authorizer(&gateway, &api, row.case, row.authorizer.clone()).await;
        let id = string_member(&authorizer, "id", row.case);
        put_method(
            &gateway,
            &api,
            &resource,
            row.verb,
            json!({ "authorizationType": row.put_as, "authorizerId": id.clone() }),
        )
        .await;
        let uri = format!(
            "/restapis/{api}/resources/{resource}/methods/{verb}",
            verb = row.verb
        );

        // A method under a type an `Authorizer` authorizes USES the one it names, so the
        // delete is refused while it stands — and the refusal takes nothing away.
        if uses_an_authorizer(row.put_as) {
            assert_failure(
                &delete_authorizer(&gateway, &api, &id).await,
                409,
                CONFLICT,
                &format!("{} as written", row.case),
            );
            let read = get_authorizer(&gateway, &api, &id).await;
            assert_eq!(
                read.body, authorizer,
                "case {}: expected the authorizer untouched by the refusal, got {:?}",
                row.case, read.body
            );
        }

        if let Some(moved_to) = row.moved_to {
            let moved = patch(&gateway, &uri, vec![replace("/authorizationType", moved_to)]).await;
            assert_eq!(
                moved.status, 200,
                "case {}: expected UpdateMethod to move the authorization type to \
                 {moved_to}, got {} with {:?}",
                row.case, moved.status, moved.body
            );
            assert_eq!(
                moved.body["authorizationType"].as_str(),
                Some(moved_to),
                "case {}: expected UpdateMethod to report the moved type, got {:?}",
                row.case,
                moved.body["authorizationType"]
            );
        }

        let answer = delete_authorizer(&gateway, &api, &id).await;
        if uses_an_authorizer(row.final_type()) {
            assert_failure(
                &answer,
                409,
                CONFLICT,
                &format!("{} under {}", row.case, row.final_type()),
            );
            let read = get_authorizer(&gateway, &api, &id).await;
            assert_eq!(
                read.body, authorizer,
                "case {}: expected the authorizer to survive a delete its own API blocks, \
                 got {:?}",
                row.case, read.body
            );
        } else {
            assert_eq!(
                answer.status, 202,
                "case {}: expected 202 once no method of the API uses the authorizer, got \
                 {} with {:?}",
                row.case, answer.status, answer.body
            );
            assert_failure(
                &get_authorizer(&gateway, &api, &id).await,
                404,
                "NotFoundException",
                &format!("GetAuthorizer after deleting {}", row.case),
            );
        }

        // The method is the one the row left behind either way: a delete of the authorizer
        // takes nothing from the method that named it.
        let read = gateway.call("GET", &uri, None).await;
        assert_eq!(
            read.status, 200,
            "case {}: expected the method to be readable, got {} with {:?}",
            row.case, read.status, read.body
        );
        assert_eq!(
            read.body["authorizationType"].as_str(),
            Some(row.final_type()),
            "case {}: GetMethod must report the method's own authorization type, got {:?}",
            row.case,
            read.body["authorizationType"]
        );
    }
}

#[tokio::test]
async fn every_method_that_uses_the_authorizer_has_to_be_moved_off_it() {
    // The dependency is one METHOD's, so an authorizer several methods use is freed only
    // once the last of them is authorized without it: the rows above vary one method, this
    // varies how MANY hold the authorizer.
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "authorizer-detached-many").await;
    let resource = resource_under(&gateway, &api, &root, "secure").await;
    let authorizer = created_authorizer(
        &gateway,
        &api,
        "an authorizer three methods use",
        token_authorizer("usedbythree"),
    )
    .await;
    let id = string_member(&authorizer, "id", "an authorizer three methods use");

    // Three methods of the same API, each authorized BY the authorizer: one on the root
    // resource and two on the child, so the walk that finds a user covers both.
    let holders = [(root.clone(), "GET"), (resource.clone(), "GET"), (resource.clone(), "POST")];
    for (on, verb) in &holders {
        put_method(
            &gateway,
            &api,
            on,
            verb,
            json!({ "authorizationType": "CUSTOM", "authorizerId": id.clone() }),
        )
        .await;
    }

    for (index, (on, verb)) in holders.iter().enumerate() {
        let left = holders.len() - index;
        assert_failure(
            &delete_authorizer(&gateway, &api, &id).await,
            409,
            CONFLICT,
            &format!("{left} method(s) still using the authorizer"),
        );
        let moved = patch(
            &gateway,
            &format!("/restapis/{api}/resources/{on}/methods/{verb}"),
            vec![replace("/authorizationType", "NONE")],
        )
        .await;
        assert_eq!(
            moved.status, 200,
            "case moving {verb} off the authorizer: expected 200, got {} with {:?}",
            moved.status, moved.body
        );
    }

    let answer = delete_authorizer(&gateway, &api, &id).await;
    assert_eq!(
        answer.status, 202,
        "case the last method moved off the authorizer: expected 202, got {} with {:?}",
        answer.status, answer.body
    );
    assert_failure(
        &get_authorizer(&gateway, &api, &id).await,
        404,
        "NotFoundException",
        "GetAuthorizer once every method was moved off the authorizer",
    );
}
