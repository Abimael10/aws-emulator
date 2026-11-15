// SPDX-License-Identifier: CC0-1.0

//! How many TIMES a refusal of this service carries the `restJson1` error-type header,
//! read off the raw response — driven over the real wire path through the router's
//! `dispatch`, so every call is deserialized, constraint-checked, handled and serialized
//! exactly as a client's would be.
//!
//! The rule under test: the FAIL-CLOSED refusal this service answers — HTTP 401 with the
//! `UnauthorizedException` code, for a request whose caller's ownership of the resource it
//! named cannot be established — carries `x-amzn-errortype` REPEATED, twice, both
//! occurrences naming that code, which is what this service's wire does and what a client
//! joining repeated header values reads. Every other answer keeps the single occurrence the
//! protocol's error serializer writes (none at all on a served request), so the
//! multiplicity belongs to that refusal and not to every response.
//!
//! Asserted over a TABLE spanning the class rather than one request. The rows differ along
//! what the rule generalises over — WHICH operation was asked (every gated operation of the
//! user plane, the identity-provider plane and the license-server plane), WHICH surface
//! decided the refusal (the gate hoisted ahead of dispatch, which decides it for an
//! operation naming an EC2 instance, and a handler's own error channel, which decides it for
//! the rest), and WHICH way the request named what it acts on (an instance id, an ARN of one
//! of this service's own resources, a (provider, product) pair). The contrast rows are the
//! other classes: a served request, and refusals carrying another code.
//!
//! The harness (router + engine + the account's directories and network) is the one in
//! [`super::identity_provider_subscriptions`]; only the rule is new here.

use serde_json::{json, Value};

// ONE wire harness per service: the dispatch plumbing and the fixtures come from the
// sibling suites rather than a second copy.
use super::identity_provider_subscriptions::{
    arn_prefix, as_str, dispatch, encoded_label, harness, provider_union, register, Harness,
    PRODUCTS,
};
use super::resource_resolution_refusals::held_instance;

/// The header a REST-JSON client resolves a failure's error shape from. Spelled out here
/// rather than read from the production constant, so a change to it fails a test instead of
/// silently changing what this suite accepts.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The code the fail-closed refusal carries, and the codes the contrast rows carry.
const UNAUTHORIZED: &str = "UnauthorizedException";
const BAD_REQUEST: &str = "BadRequestException";
const VALIDATION: &str = "ValidationException";

/// The fixed sentence that travels with the fail-closed refusal.
const UNAUTHORIZED_MESSAGE: &str =
    "Unauthorized. Please check the request and access policy and try again.";

/// How many occurrences of [`ERROR_TYPE_HEADER`] the fail-closed refusal is carried with,
/// and how many every other refusal class is.
const REPEATED: usize = 2;
const ONCE: usize = 1;

/// A directory of the account's fleet this suite registers a provider for.
const REGISTERED_DIRECTORY: &str = "d-0123456789";
/// A directory of the account's fleet this suite registers nothing for.
const UNREGISTERED_DIRECTORY: &str = "d-00000000ff";
/// A subnet of the account's own Amazon EC2 store, which a registration's `Settings` name.
const SUBNET: &str = "subnet-0123456789abcdef0";
/// A security group of that store, likewise.
const SECURITY_GROUP: &str = "sg-0123abcd";

/// Which surface decided the refusal a row drives — recorded on the row so the table is
/// read as covering both, and so a failure says which one answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecidedBy {
    /// The gate hoisted ahead of dispatch, for an operation that names an EC2 instance.
    Gate,
    /// The operation's own error channel, for an operation that resolves what it was asked
    /// about in its handler.
    Handler,
}

/// One row: a request, and what its answer must carry.
struct Row {
    case: String,
    decided_by: DecidedBy,
    status: u16,
    /// The code both the header and the body must name, or `None` for a served request.
    code: Option<&'static str>,
    /// How many occurrences of [`ERROR_TYPE_HEADER`] the response must carry.
    occurrences: usize,
    response: http::Response<bytes::Bytes>,
}

/// Every value `response` carries for [`ERROR_TYPE_HEADER`], in order — the SET a client
/// reading the raw headers observes, rather than the first occurrence alone.
fn error_type_header_set(response: &http::Response<bytes::Bytes>) -> Vec<String> {
    response
        .headers()
        .get_all(ERROR_TYPE_HEADER)
        .iter()
        .map(|value| {
            value
                .to_str()
                .expect("an error-type header value is textual")
                .to_string()
        })
        .collect()
}

/// The response body as JSON, or `Null` when there is none.
fn body_of(response: &http::Response<bytes::Bytes>) -> Value {
    if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    }
}

/// Assert `row` was answered with its status, with its code carried the expected number of
/// times in the error-type header, and with the body naming that same code — so a client
/// that joins the repeated header values, one that reads only the first, and one that reads
/// the body all resolve the same error shape.
fn assert_row(row: &Row) {
    let Row {
        case,
        decided_by,
        status,
        code,
        occurrences,
        response,
    } = row;
    let body = body_of(response);
    assert_eq!(
        response.status().as_u16(),
        *status,
        "case {case:?} (decided by {decided_by:?}): expected HTTP {status}, got {} with body \
         {body:?}",
        response.status().as_u16()
    );
    let carried = error_type_header_set(response);
    let expected: Vec<String> = code
        .map(|code| vec![code.to_string(); *occurrences])
        .unwrap_or_default();
    assert_eq!(
        carried, expected,
        "case {case:?} (decided by {decided_by:?}): expected the error-type header carried \
         {occurrences} time(s) as {code:?}, got {carried:?} with body {body:?}"
    );
    let named = body.get("__type").and_then(Value::as_str);
    assert_eq!(
        named, *code,
        "case {case:?} (decided by {decided_by:?}): the body and the header must name the \
         same shape, got body {named:?} against header set {carried:?}"
    );
}

/// Register `directory_id` for `product` and answer the `IdentityProviderArn` minted.
async fn registered(h: &Harness, directory_id: &str, product: &str, case: &str) -> String {
    let reply = register(
        h,
        &json!({
            "IdentityProvider": provider_union(directory_id),
            "Product": product,
            "Settings": { "Subnets": [SUBNET], "SecurityGroupId": SECURITY_GROUP },
        }),
    )
    .await;
    as_str(
        &reply.ok(case)["IdentityProviderSummary"],
        "IdentityProviderArn",
        case,
    )
    .to_string()
}

/// One `POST` of `body` to `path`, as the raw response it was answered with.
async fn post(h: &Harness, path: &str, body: &Value) -> http::Response<bytes::Bytes> {
    dispatch(h, "POST", path, Some(body)).await
}

/// A fail-closed row: the request is answered HTTP 401 and carries the code repeated in the
/// error-type header and once in the body.
fn fails_closed(case: &str, decided_by: DecidedBy, response: http::Response<bytes::Bytes>) -> Row {
    Row {
        case: case.to_string(),
        decided_by,
        status: 401,
        code: Some(UNAUTHORIZED),
        occurrences: REPEATED,
        response,
    }
}

/// RULE: the fail-closed refusal of this service carries the error-type header REPEATED,
/// naming its code in every occurrence — on every surface that can fail closed, whichever
/// way the request named what it acts on, and whichever layer decided it — while every other
/// answer carries that header at most once.
#[tokio::test]
async fn the_fail_closed_refusal_carries_its_error_type_header_repeated() {
    let h = harness();
    // One real registration, so the refusals below are about what each request names and
    // not about an empty store.
    let provider_arn = registered(&h, REGISTERED_DIRECTORY, PRODUCTS[0], "the registration").await;
    let held = held_instance(0);

    // An instance id no instance of the account has, and ARNs of this service's own
    // resource kinds that state holds nothing for.
    let unheld_instance = "i-0123456789abcdef0";
    let unknown_association = format!("{}iu-000000009999", arn_prefix("instance-user"));
    let unknown_endpoint = format!("{}lse-000000009999", arn_prefix("license-server-endpoint"));
    let unknown_product_user = format!("{}pu-000000009999", arn_prefix("user"));
    // An ARN of this service's shape that resolves to no registration. ONE slash in the
    // resource part, which is what the member's `Arn` @pattern admits, so the refusal is
    // reached rather than a constraint violation.
    let unknown_provider = format!("{}idp-000000009999", arn_prefix("identity-provider"));

    let rows: Vec<Row> = vec![
        // THE GATE'S HALF: the operations whose input names an EC2 instance, refused ahead
        // of dispatch for an instance the account does not hold.
        fails_closed(
            "AssociateUser on an instance the account does not hold",
            DecidedBy::Gate,
            post(
                &h,
                "/user/AssociateUser",
                &json!({
                    "Username": "someone",
                    "InstanceId": unheld_instance,
                    "IdentityProvider": provider_union(REGISTERED_DIRECTORY),
                }),
            )
            .await,
        ),
        fails_closed(
            "DisassociateUser on an instance the account does not hold",
            DecidedBy::Gate,
            post(
                &h,
                "/user/DisassociateUser",
                &json!({
                    "Username": "someone",
                    "InstanceId": unheld_instance,
                    "IdentityProvider": provider_union(REGISTERED_DIRECTORY),
                }),
            )
            .await,
        ),
        fails_closed(
            "DisassociateUser by an InstanceUserArn state holds nothing for",
            DecidedBy::Gate,
            post(
                &h,
                "/user/DisassociateUser",
                &json!({ "InstanceUserArn": unknown_association }),
            )
            .await,
        ),
        fails_closed(
            "ListUserAssociations on an instance the account does not hold",
            DecidedBy::Gate,
            post(
                &h,
                "/user/ListUserAssociations",
                &json!({
                    "InstanceId": unheld_instance,
                    "IdentityProvider": provider_union(REGISTERED_DIRECTORY),
                }),
            )
            .await,
        ),
        // THE HANDLERS' HALF: the operations that resolve what they were asked about in
        // their own handler, refused through its error channel.
        fails_closed(
            "ListProductSubscriptions through an unregistered identity provider",
            DecidedBy::Handler,
            post(
                &h,
                "/user/ListProductSubscriptions",
                &json!({
                    "IdentityProvider": provider_union(UNREGISTERED_DIRECTORY),
                    "Product": PRODUCTS[0],
                }),
            )
            .await,
        ),
        fails_closed(
            "StartProductSubscription through an unregistered identity provider",
            DecidedBy::Handler,
            post(
                &h,
                "/user/StartProductSubscription",
                &json!({
                    "Username": "someone",
                    "IdentityProvider": provider_union(UNREGISTERED_DIRECTORY),
                    "Product": PRODUCTS[1],
                }),
            )
            .await,
        ),
        fails_closed(
            "StopProductSubscription through an unregistered identity provider",
            DecidedBy::Handler,
            post(
                &h,
                "/user/StopProductSubscription",
                &json!({
                    "Username": "someone",
                    "IdentityProvider": provider_union(UNREGISTERED_DIRECTORY),
                    "Product": PRODUCTS[2],
                }),
            )
            .await,
        ),
        fails_closed(
            "StopProductSubscription by a ProductUserArn state holds nothing for",
            DecidedBy::Handler,
            post(
                &h,
                "/user/StopProductSubscription",
                &json!({ "ProductUserArn": unknown_product_user }),
            )
            .await,
        ),
        fails_closed(
            "DeregisterIdentityProvider by an unregistered (provider, product) pair",
            DecidedBy::Handler,
            post(
                &h,
                "/identity-provider/DeregisterIdentityProvider",
                &json!({
                    "IdentityProvider": provider_union(UNREGISTERED_DIRECTORY),
                    "Product": PRODUCTS[0],
                }),
            )
            .await,
        ),
        fails_closed(
            "UpdateIdentityProviderSettings by an unregistered (provider, product) pair",
            DecidedBy::Handler,
            post(
                &h,
                "/identity-provider/UpdateIdentityProviderSettings",
                &json!({
                    "IdentityProvider": provider_union(UNREGISTERED_DIRECTORY),
                    "Product": PRODUCTS[0],
                    "UpdateSettings": { "AddSubnets": [SUBNET], "RemoveSubnets": [] },
                }),
            )
            .await,
        ),
        fails_closed(
            "DeleteLicenseServerEndpoint by an ARN state holds nothing for",
            DecidedBy::Handler,
            post(
                &h,
                "/license-server/DeleteLicenseServerEndpoint",
                &json!({
                    "LicenseServerEndpointArn": unknown_endpoint,
                    "ServerType": "RDS_SAL",
                }),
            )
            .await,
        ),
        // THE CONTRAST: a served request names no shape at all, and a refusal of another
        // class names its own code exactly ONCE — the repeated header belongs to the
        // fail-closed refusal, not to every response of this service.
        Row {
            case: "ListUserAssociations on an instance the account holds, served".to_string(),
            decided_by: DecidedBy::Gate,
            status: 200,
            code: None,
            occurrences: 0,
            response: post(
                &h,
                "/user/ListUserAssociations",
                &json!({
                    "InstanceId": held,
                    "IdentityProvider": provider_union(REGISTERED_DIRECTORY),
                }),
            )
            .await,
        },
        Row {
            case: "UpdateIdentityProviderSettings on a held registration, served".to_string(),
            decided_by: DecidedBy::Handler,
            status: 200,
            code: None,
            occurrences: 0,
            response: post(
                &h,
                "/identity-provider/UpdateIdentityProviderSettings",
                &json!({
                    "IdentityProviderArn": provider_arn,
                    "UpdateSettings": { "AddSubnets": [], "RemoveSubnets": [] },
                }),
            )
            .await,
        },
        Row {
            case: "ListTagsForResource by an ARN naming no resource".to_string(),
            decided_by: DecidedBy::Handler,
            status: 400,
            code: Some(BAD_REQUEST),
            occurrences: ONCE,
            response: dispatch(
                &h,
                "GET",
                &format!("/tags/{}", encoded_label(&unknown_association)),
                None,
            )
            .await,
        },
        Row {
            case: "DeregisterIdentityProvider by an IdentityProviderArn naming no registration"
                .to_string(),
            decided_by: DecidedBy::Handler,
            status: 400,
            code: Some(VALIDATION),
            occurrences: ONCE,
            response: post(
                &h,
                "/identity-provider/DeregisterIdentityProvider",
                &json!({ "IdentityProviderArn": unknown_provider }),
            )
            .await,
        },
    ];

    // Both halves of the rule are exercised: the table would otherwise pass while covering
    // only the surface whose refusals happen to be rendered in one place.
    for surface in [DecidedBy::Gate, DecidedBy::Handler] {
        let covered = rows
            .iter()
            .filter(|row| row.decided_by == surface && row.status == 401)
            .count();
        assert!(
            covered > 0,
            "the table must fail a request closed on the {surface:?} surface, it covers \
             {covered} such rows"
        );
    }

    for row in &rows {
        assert_row(row);
        // The fail-closed class also carries one fixed sentence, whatever it was asked
        // about: the envelope a client branches on is the status, the shape and that text
        // together, and the repeated header changes none of the rest of it.
        if row.status == 401 {
            let body = body_of(&row.response);
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(UNAUTHORIZED_MESSAGE),
                "case {:?}: expected the fixed fail-closed sentence, got {body:?}",
                row.case
            );
        }
    }
}
