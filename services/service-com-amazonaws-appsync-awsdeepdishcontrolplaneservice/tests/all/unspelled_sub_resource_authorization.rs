// SPDX-License-Identifier: CC0-1.0

//! WHICH resource IAM judges a call against when the call addresses a member of a child
//! space this service publishes no ARN spelling for, over the restJson1 wire.
//!
//! The rule under test: a request that addresses a member of such a space — a function of
//! an API (`/v{n}/apis/{apiId}/functions/{functionId}`), a resolver on a field of a type
//! (`/v{n}/apis/{apiId}/types/{typeName}/resolvers/{fieldName}`) — is judged against the
//! request's OWN URI path, its version prefix and its leading `/` included, hung off this
//! service's ARN prefix. Two things follow, and both are asserted here:
//!
//! 1. that path is the resource the implicit-deny `AccessDeniedException` NAMES, so a
//!    caller reading the refusal is told the resource its policy has to state — never the
//!    ARN this service REPORTS the addressed object under (`functionArn`, `resolverArn`),
//!    which spells the object rather than the call;
//! 2. that path is the resource a policy has to state to LIFT the denial: a grant written
//!    about it, or about a wildcard over that URI space, serves the call, and a grant
//!    written about the object's own ARN, about the parent API's ARN, or about a wildcard
//!    over the ARN space of that API serves nothing — so a least-privilege tool that
//!    authors a statement from the refusal's resource authors one that works.
//!
//! A call on a space this service DOES spell keeps that spelling: a call on a child
//! COLLECTION is judged against the resource it collects under — the API for its functions,
//! the type for its resolvers — which the control rows of the same table state.
//!
//! The table's rows differ along what the rule generalises over: the operation and its
//! action, the HTTP method (a read, a write and a delete of the same member), the space the
//! member belongs to, the member identifier (a function id this service minted, one nothing
//! created, and resolvers on two different types), and whether the call addresses a member
//! or the collection above it. The decision is the real one: the principals are made with
//! IAM's own operations over the same engine, which is the harness the sibling
//! authorization suite builds.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::authorization::ACTION_PREFIX;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::identity_authorization::{
    allow, arn, default_deny_message, Emulator, Reply, ACCESS_DENIED, ACCESS_DENIED_STATUS,
    ACCOUNT_KEY,
};

/// The data source the seeded function and resolvers resolve against.
const DATA_SOURCE: &str = "ds_unspelled";

/// The seeded function's name, and the version of the mapping templates it carries
/// (`FunctionConfiguration$functionVersion`, whose only supported value is this one).
const FUNCTION_NAME: &str = "fn_unspelled";
const FUNCTION_VERSION: &str = "2018-05-29";

/// The mapping templates every seeded definition carries.
const REQUEST_TEMPLATE: &str = "{\"version\": \"2018-05-29\"}";
const RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result)";

/// The two types a seeded resolver is attached to, and the field on each: so the rule is
/// stated over more than one type name and more than one field name.
const TYPE_NAME: &str = "Query";
const FIELD_NAME: &str = "echo";
const OTHER_TYPE_NAME: &str = "Mutation";
const OTHER_FIELD_NAME: &str = "publish";

/// A function id of the modelled `ResourceName` form that nothing created: the decision is
/// taken on the request, so a member that does not exist is judged exactly like one that
/// does.
const ABSENT_FUNCTION_ID: &str = "fn_nothing_created";

/// The resources the table is written about, created by the account's own credential.
struct Fixture {
    api_id: String,
    function_id: String,
}

/// One request a decision is taken about.
struct Judged {
    /// What the row exercises, named in every assertion of it.
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    /// The operation's own name, which the action is formed from.
    operation: &'static str,
    /// The resource IAM judges the call against, which the refusal names.
    resource: String,
    /// The ARN this service REPORTS the addressed object under, for a row that addresses
    /// one object — the spelling a policy written about the object would use, which is NOT
    /// the resource of the call.
    object_arn: Option<String>,
    /// The answer an AUTHORIZED caller gets, or `None` for a row this suite does not drive
    /// to success because the call would destroy or duplicate the fixture.
    served_status: Option<u16>,
}

/// Create the API, data source, function and resolvers the table addresses.
async fn fixture(emulator: &Emulator) -> Fixture {
    let created = call_ok(
        emulator,
        "fixture: the API",
        "POST",
        "/v1/apis",
        Some(json!({"name": "Unspelled_api", "authenticationType": "API_KEY"})),
    )
    .await;
    let api_id = created.body["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    call_ok(
        emulator,
        "fixture: the data source",
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    let function = call_ok(
        emulator,
        "fixture: the function",
        "POST",
        &format!("/v1/apis/{api_id}/functions"),
        Some(function_body(FUNCTION_NAME)),
    )
    .await;
    let function_id = function.body["functionConfiguration"]["functionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    for (type_name, field_name) in [
        (TYPE_NAME, FIELD_NAME),
        (OTHER_TYPE_NAME, OTHER_FIELD_NAME),
    ] {
        call_ok(
            emulator,
            "fixture: the resolver",
            "POST",
            &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
            Some(resolver_body(Some(field_name))),
        )
        .await;
    }
    Fixture {
        api_id,
        function_id,
    }
}

/// One setup call, which must be served.
async fn call_ok(
    emulator: &Emulator,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let reply = emulator
        .call(ACCOUNT_KEY, method, path, body.as_ref())
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the setup call must be served, got {} {:?} with {:?}",
        reply.status, reply.error_code, reply.body
    );
    reply
}

/// A `CreateFunction` / `UpdateFunction` body naming `name`: both operations take the same
/// member set.
fn function_body(name: &str) -> Value {
    json!({
        "name": name,
        "dataSourceName": DATA_SOURCE,
        "functionVersion": FUNCTION_VERSION,
        "requestMappingTemplate": REQUEST_TEMPLATE,
        "responseMappingTemplate": RESPONSE_TEMPLATE,
    })
}

/// A `CreateResolver` body for `field_name` (which `UpdateResolver` carries without, the
/// field being its own path label).
fn resolver_body(field_name: Option<&str>) -> Value {
    let mut body = json!({
        "dataSourceName": DATA_SOURCE,
        "requestMappingTemplate": REQUEST_TEMPLATE,
        "responseMappingTemplate": RESPONSE_TEMPLATE,
    });
    if let Some(field_name) = field_name {
        body.as_object_mut()
            .expect("the body is an object")
            .insert("fieldName".to_string(), json!(field_name));
    }
    body
}

/// The table: every call on a member of an unspelled space, and the control rows that
/// address a collection this service DOES spell.
fn judged_requests(fixture: &Fixture) -> Vec<Judged> {
    let api = &fixture.api_id;
    let function_id = &fixture.function_id;
    let function_path = format!("/v1/apis/{api}/functions/{function_id}");
    let resolver_path = format!("/v1/apis/{api}/types/{TYPE_NAME}/resolvers/{FIELD_NAME}");
    vec![
        Judged {
            case: "a read of one function",
            method: "GET",
            path: function_path.clone(),
            body: None,
            operation: "GetFunction",
            resource: arn(&function_path),
            object_arn: Some(arn(&state::function_resource(api, function_id))),
            served_status: Some(200),
        },
        Judged {
            case: "an update of one function",
            method: "POST",
            path: function_path.clone(),
            body: Some(function_body(FUNCTION_NAME)),
            operation: "UpdateFunction",
            resource: arn(&function_path),
            object_arn: Some(arn(&state::function_resource(api, function_id))),
            served_status: Some(200),
        },
        Judged {
            case: "a delete of one function",
            method: "DELETE",
            path: function_path,
            body: None,
            operation: "DeleteFunction",
            resource: arn(&format!("/v1/apis/{api}/functions/{function_id}")),
            object_arn: Some(arn(&state::function_resource(api, function_id))),
            // Driven refused only: a served delete would take the member the rest of the
            // table addresses.
            served_status: None,
        },
        Judged {
            case: "a read of one function nothing created",
            method: "GET",
            path: format!("/v1/apis/{api}/functions/{ABSENT_FUNCTION_ID}"),
            body: None,
            operation: "GetFunction",
            resource: arn(&format!("/v1/apis/{api}/functions/{ABSENT_FUNCTION_ID}")),
            object_arn: Some(arn(&state::function_resource(api, ABSENT_FUNCTION_ID))),
            // The gate fires before the lookup, so only an authorized caller learns the
            // function is absent.
            served_status: Some(404),
        },
        Judged {
            case: "a read of one resolver",
            method: "GET",
            path: resolver_path.clone(),
            body: None,
            operation: "GetResolver",
            resource: arn(&resolver_path),
            object_arn: Some(arn(&state::resolver_resource(api, TYPE_NAME, FIELD_NAME))),
            served_status: Some(200),
        },
        Judged {
            case: "an update of one resolver",
            method: "POST",
            path: resolver_path.clone(),
            body: Some(resolver_body(None)),
            operation: "UpdateResolver",
            resource: arn(&resolver_path),
            object_arn: Some(arn(&state::resolver_resource(api, TYPE_NAME, FIELD_NAME))),
            served_status: Some(200),
        },
        Judged {
            case: "a delete of one resolver",
            method: "DELETE",
            path: resolver_path,
            body: None,
            operation: "DeleteResolver",
            resource: arn(&format!(
                "/v1/apis/{api}/types/{TYPE_NAME}/resolvers/{FIELD_NAME}"
            )),
            object_arn: Some(arn(&state::resolver_resource(api, TYPE_NAME, FIELD_NAME))),
            served_status: None,
        },
        Judged {
            case: "a read of one resolver on another type",
            method: "GET",
            path: format!("/v1/apis/{api}/types/{OTHER_TYPE_NAME}/resolvers/{OTHER_FIELD_NAME}"),
            body: None,
            operation: "GetResolver",
            resource: arn(&format!(
                "/v1/apis/{api}/types/{OTHER_TYPE_NAME}/resolvers/{OTHER_FIELD_NAME}"
            )),
            object_arn: Some(arn(&state::resolver_resource(
                api,
                OTHER_TYPE_NAME,
                OTHER_FIELD_NAME,
            ))),
            served_status: Some(200),
        },
        // The control rows: a call on a child COLLECTION names the resource it collects
        // under, which this service does spell.
        Judged {
            case: "a read of the function collection",
            method: "GET",
            path: format!("/v1/apis/{api}/functions"),
            body: None,
            operation: "ListFunctions",
            resource: arn(&state::api_resource(api)),
            object_arn: None,
            served_status: Some(200),
        },
        Judged {
            case: "a create in the function collection",
            method: "POST",
            path: format!("/v1/apis/{api}/functions"),
            body: Some(function_body("fn_second")),
            operation: "CreateFunction",
            resource: arn(&state::api_resource(api)),
            object_arn: None,
            // Driven refused only: a served create cannot be repeated under the same name.
            served_status: None,
        },
        Judged {
            case: "a read of the resolver collection of one type",
            method: "GET",
            path: format!("/v1/apis/{api}/types/{TYPE_NAME}/resolvers"),
            body: None,
            operation: "ListResolvers",
            resource: arn(&state::type_resource(api, TYPE_NAME)),
            object_arn: None,
            served_status: Some(200),
        },
        Judged {
            case: "a create in the resolver collection of one type",
            method: "POST",
            path: format!("/v1/apis/{api}/types/{TYPE_NAME}/resolvers"),
            body: Some(resolver_body(Some("second"))),
            operation: "CreateResolver",
            resource: arn(&state::type_resource(api, TYPE_NAME)),
            object_arn: None,
            served_status: None,
        },
    ]
}

/// RULE: the implicit-deny refusal names, as the resource, the request's own URI path hung
/// off this service's ARN prefix for a call on a member of a space this service publishes no
/// ARN spelling for — and the spelled ARN of the resource it collects under for a call on a
/// collection.
#[tokio::test]
async fn the_refusal_names_the_calls_own_uri_path_for_an_unspelled_member() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;
    let principal = emulator.user("unspelled_zero", None).await;

    for request in judged_requests(&fixture) {
        let reply = emulator
            .call(
                &principal.access_key_id,
                request.method,
                &request.path,
                request.body.as_ref(),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {:?}",
            request.case,
            reply.status,
            reply.error_code,
            reply.body
        );
        let expected = default_deny_message(&principal.arn, request.operation, &request.resource);
        assert_eq!(
            reply.message(),
            expected,
            "case {:?}: expected the refusal to name {:?} as the resource, got {:?}",
            request.case,
            request.resource,
            reply.message()
        );
        // The ARN the object is REPORTED under spells the object, not the call, so it is
        // not what the refusal names.
        if let Some(object_arn) = &request.object_arn {
            assert!(
                !reply.message().contains(object_arn),
                "case {:?}: expected the refusal not to name the object's own ARN {object_arn:?}, \
                 got {:?}",
                request.case,
                reply.message()
            );
        }
    }
}

/// RULE: the resource the refusal names is the resource a policy has to state to lift it —
/// a grant about that URI path (or a wildcard over that URI space) serves the call, and a
/// grant about the object's own ARN, the parent API's ARN or a wildcard over that API's ARN
/// space does not.
#[tokio::test]
async fn only_a_grant_about_the_judged_path_lifts_the_denial() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;
    let api = &fixture.api_id;

    for (index, request) in judged_requests(&fixture)
        .into_iter()
        .filter(|request| request.object_arn.is_some() && request.served_status.is_some())
        .enumerate()
    {
        let action = format!("{ACTION_PREFIX}:{}", request.operation);
        let object_arn = request
            .object_arn
            .clone()
            .expect("the rows are filtered to those addressing one object");
        let served_status = request
            .served_status
            .expect("the rows are filtered to those driven to success");
        // (what the policy's resource states, whether it covers the call)
        let policies: &[(&str, String, bool)] = &[
            (
                "the resource the refusal names",
                request.resource.clone(),
                true,
            ),
            (
                "a wildcard over the URI space of that API",
                arn(&format!("/v1/apis/{api}/*")),
                true,
            ),
            (
                "the ARN the object itself is reported under",
                object_arn.clone(),
                false,
            ),
            (
                "the ARN of the parent API",
                arn(&state::api_resource(api)),
                false,
            ),
            (
                "a wildcard over the ARN space of that API",
                arn(&format!("apis/{api}/*")),
                false,
            ),
        ];
        for (policy_index, (policy, resource, covers)) in policies.iter().enumerate() {
            let principal = emulator
                .user(
                    &format!("unspelled_lift_{index}_{policy_index}"),
                    Some(&allow(&action, resource)),
                )
                .await;
            let reply = emulator
                .call(
                    &principal.access_key_id,
                    request.method,
                    &request.path,
                    request.body.as_ref(),
                )
                .await;
            if *covers {
                assert_eq!(
                    reply.status, served_status,
                    "case {:?} / {policy:?}: expected the operation's own {served_status} \
                     answer, got {} {:?} with {:?}",
                    request.case, reply.status, reply.error_code, reply.body
                );
            } else {
                assert_eq!(
                    (reply.status, reply.error_code.as_str()),
                    (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                    "case {:?} / {policy:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, \
                     got {} {:?} with {:?}",
                    request.case,
                    reply.status,
                    reply.error_code,
                    reply.body
                );
                assert_eq!(
                    reply.message(),
                    default_deny_message(
                        &principal.arn,
                        request.operation,
                        &request.resource
                    ),
                    "case {:?} / {policy:?}: expected the refusal to name {:?} as the \
                     resource, got {:?}",
                    request.case,
                    request.resource,
                    reply.message()
                );
            }
        }
    }
}
