// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the IAM decision this service's CONTROL plane takes: what a modelled
//! operation answers a request whose caller's policies do not allow the action it named.
//!
//! THE RULE UNDER TEST: a control-plane request is served only where the identity-based
//! policies in force on the principal its credential speaks for allow that call — which API
//! Gateway publishes as the action `apigateway:<HTTP METHOD>` on the ARN of the API Gateway
//! resource the request addressed (`arn:aws:apigateway:{region}::{request path}`). Every other
//! request is refused `403` with `x-amzn-errortype: AccessDeniedException` and the sentence
//! IAM words an authorization failure in, and the operation behind it never runs: it reads
//! nothing, writes nothing, and its refusal says nothing about whether the record the request
//! named exists. The account's own published credential holds no IAM policies — the position
//! AWS account root credentials are in — and a grant this emulator cannot read grounds no
//! refusal.
//!
//! The tables span what that rule generalises over: the document in force on the caller (none
//! at all, every action of the service, the exact verb on the exact resource, the verb on
//! another resource, another service's actions, an explicit deny beside a grant of
//! everything), the VERB the request used — which is what the action is named after, so a
//! grant of one verb must not cover another — the PRINCIPAL the credential speaks for (an
//! assumed-role session, the account's own published credential), and the OPERATION, from a
//! read of one record to a write of a collection, so the decision is the plane's rather than
//! one handler's.
//!
//! The role, its policies and the session live in the IAM and STS stores of the SAME shared
//! engine — the stores those services' own handlers write — so the documents under test are
//! the ones `iam:PutRolePolicy` stores and the principal a refusal blames is the one
//! `sts:GetCallerIdentity` answers for the key the request signed with. The api, its resource
//! and its method response are written through this service's own control plane under the
//! account's published credential, and every case drives the real wire path, so what is
//! asserted is what a client holding those credentials sees.
//!
//! The gate under test is `src/service/control_plane_authorization.rs`, which asks the one
//! evaluation order this crate's data plane is judged by
//! (`src/service/invoke/execute_api_authorization.rs`). The sibling
//! `control_plane_fidelity_rules` suite carries ONE row over that gate — a session denied the
//! call it makes — among the ten rules it pins; what is added here is the TABLE the rule
//! generalises over: every reading of a document, every verb, and the records a refused write
//! did not leave behind.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

// The signing helper, the document builder, the ARN builder and the seeded session are the
// siblings' own: one spelling of a control-plane request's credential scope, one of an
// allow, one of this service's ARN form, and one seeded role, so no two suites can drive
// this gate with two different ones.
use crate::control_plane_fidelity_rules::call_as;
use crate::rest_api_lifecycle::{create_api, resource_under, string_member, Gateway};
use crate::stage_invoke_identity_policy::{
    allow, given_policy, given_session, session_arn, Entity, SESSION_KEY_ID,
};
use crate::tag_reads_by_arn::apigateway_arn;

/// The wire error code an authorization refusal carries, which is in no operation's modelled
/// error set: authorization is decided ahead of the service.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// An access key id neither store holds a record for: the position the account's own
/// published credential is in, and what the harness's own calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The status code the method response under test is put on.
const STATUS_CODE: &str = "200";

/// A policy allowing every action on everything and DENYING `action` on `resource`: the
/// explicit deny no grant lifts, which the single-statement [`allow`] cannot express.
fn allow_everything_but(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "*", "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": resource },
        ]
    })
}

/// The sentence IAM refuses an unauthorized call with, as every authorizing service in this
/// tree words it: who was refused, which action, on which resource, and which of IAM's rules
/// refused it.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// The NAME of the role the seeded session is a session of, read out of the session's own
/// principal ARN (`arn:aws:sts::{account}:assumed-role/{role}/{session}`) rather than restated
/// here, so this suite and the suite that seeds the session cannot name two different roles.
fn seeded_role_name() -> String {
    let arn = session_arn();
    let (_, tail) = arn
        .split_once("assumed-role/")
        .unwrap_or_else(|| panic!("the seeded session's ARN names a role: {arn:?}"));
    tail.split('/')
        .next()
        .unwrap_or_else(|| panic!("the seeded session's ARN names a role: {arn:?}"))
        .to_string()
}

/// One api carrying `GET /orders` with a method response on [`STATUS_CODE`], written through
/// this service's own control plane: the api id, the resource id, and the path of the method
/// response under test.
struct Method {
    api: String,
    resource: String,
}

impl Method {
    /// Stand the api, its child resource, its method and its method response up.
    async fn stand_up(gateway: &Gateway, name: &str) -> Self {
        let (api, root) = create_api(gateway, name).await;
        let resource = resource_under(gateway, &api, &root, "orders").await;
        let put = gateway
            .call(
                "PUT",
                &format!("/restapis/{api}/resources/{resource}/methods/GET"),
                Some(json!({ "authorizationType": "NONE" })),
            )
            .await;
        assert_eq!(
            put.status, 201,
            "case {name:?}: expected PutMethod to answer 201, got {} with {:?}",
            put.status, put.body
        );
        let response = gateway
            .call(
                "PUT",
                &format!(
                    "/restapis/{api}/resources/{resource}/methods/GET/responses/{STATUS_CODE}"
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            response.status, 201,
            "case {name:?}: expected PutMethodResponse to answer 201, got {} with {:?}",
            response.status, response.body
        );
        Self { api, resource }
    }

    /// The path of the method response under test, which is also the resource portion of the
    /// ARN the decision about a call on it is taken over.
    fn method_response_path(&self) -> String {
        format!(
            "/restapis/{api}/resources/{resource}/methods/GET/responses/{STATUS_CODE}",
            api = self.api,
            resource = self.resource,
        )
    }
}

#[tokio::test]
async fn the_callers_own_policies_decide_a_control_plane_read() {
    let gateway = Gateway::new();
    let method = Method::stand_up(&gateway, "control-plane-authorization-rows").await;
    given_session(&gateway);
    let path = method.method_response_path();
    let resource = apigateway_arn(&path);
    let api_resources = apigateway_arn(&format!("/restapis/{}/*", method.api));

    // Rows differ along the DOCUMENT in force on the caller; the call is one and the same
    // `GetMethodResponse` of a method response that EXISTS, so a row's answer is the
    // decision's and nothing else.
    let rows: Vec<(&str, Option<Value>, bool)> = vec![
        ("no document at all", None, false),
        (
            "every action of this service on everything",
            Some(allow("apigateway:*", "*")),
            true,
        ),
        (
            "the verb on the exact resource",
            Some(allow("apigateway:GET", &resource)),
            true,
        ),
        (
            "the verb on every resource of the api",
            Some(allow("apigateway:GET", &api_resources)),
            true,
        ),
        (
            "the verb in another case",
            Some(allow("apigateway:get", &resource)),
            true,
        ),
        (
            "another verb on the exact resource",
            Some(allow("apigateway:DELETE", &resource)),
            false,
        ),
        (
            "the verb on another api's resources",
            Some(allow("apigateway:GET", &apigateway_arn("/restapis/other0000/*"))),
            false,
        ),
        (
            "another service's actions",
            Some(allow("execute-api:Invoke", "*")),
            false,
        ),
        (
            "everything but this very call",
            Some(allow_everything_but("apigateway:GET", &resource)),
            false,
        ),
    ];

    for (case, document, served) in &rows {
        given_policy(&gateway, Entity::Role, document.as_ref());
        let answered = call_as(&gateway, SESSION_KEY_ID, "GET", &path, None).await;
        let (status, error, body) = (answered.status, answered.error.clone(), answered.body);
        if *served {
            assert_eq!(
                (status, error.as_deref()),
                (200, None),
                "case {case:?}: expected the method response to be served, got {status} {error:?} \
                 with {body:?}"
            );
            assert_eq!(
                body["statusCode"].as_str(),
                Some(STATUS_CODE),
                "case {case:?}: expected the stored method response, got {body:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error.as_deref()),
            (403, Some(ACCESS_DENIED)),
            "case {case:?}: expected 403 {ACCESS_DENIED}, got {status} {error:?} with {body:?}"
        );
        let message = string_member(&body, "message", case);
        let expected = if *case == "everything but this very call" {
            not_authorized(
                &session_arn(),
                "apigateway:GET",
                &resource,
                "with an explicit deny in an identity-based policy",
            )
        } else {
            not_authorized(
                &session_arn(),
                "apigateway:GET",
                &resource,
                "because no identity-based policy allows the apigateway:GET action",
            )
        };
        assert_eq!(
            message, expected,
            "case {case:?}: expected IAM's authorization sentence, got {message:?}"
        );
    }

    // The account's own published credential holds no IAM policies — the position AWS account
    // root credentials are in — so the same call under it is served whatever the role carries.
    given_policy(&gateway, Entity::Role, None);
    let answered = call_as(&gateway, ACCOUNT_KEY_ID, "GET", &path, None).await;
    let (status, error, body) = (answered.status, answered.error.clone(), answered.body);
    assert_eq!(
        (status, error.as_deref()),
        (200, None),
        "case \"the account's own published credential\": expected the method response to be \
         served, got {status} {error:?} with {body:?}"
    );
}

#[tokio::test]
async fn the_verb_and_the_path_of_the_request_name_the_action_and_the_resource() {
    let gateway = Gateway::new();
    let method = Method::stand_up(&gateway, "control-plane-authorization-verbs").await;
    given_session(&gateway);

    // One grant of the READ verb over everything: it covers every GET of the plane and
    // nothing else, so each row below is decided by the verb its own call uses.
    given_policy(
        &gateway,
        Entity::Role,
        Some(&allow("apigateway:GET", "*")),
    );

    // Rows differ along the OPERATION — its verb, its path and whether it addresses one
    // record or a collection — so the decision is the plane's rather than one handler's.
    let reads: Vec<(&str, String)> = vec![
        ("GetMethodResponse", method.method_response_path()),
        (
            "GetMethod",
            format!(
                "/restapis/{}/resources/{}/methods/GET",
                method.api, method.resource
            ),
        ),
        ("GetRestApi", format!("/restapis/{}", method.api)),
        ("GetRestApis", "/restapis".to_string()),
        ("GetDomainNames", "/domainnames".to_string()),
    ];
    for (case, path) in &reads {
        let answered = call_as(&gateway, SESSION_KEY_ID, "GET", path, None).await;
        let (status, error, body) = (answered.status, answered.error.clone(), answered.body);
        assert_eq!(
            (status, error.as_deref()),
            (200, None),
            "case {case:?}: expected the read the grant covers to be served, got {status} \
             {error:?} with {body:?}"
        );
    }

    // The WRITES the same grant does not cover: each is refused for the action ITS OWN verb
    // names, on the ARN of the path it addressed, and the record it would have made is not
    // there afterwards.
    let writes: Vec<(&str, &str, String, Option<Value>)> = vec![
        (
            "CreateRestApi",
            "POST",
            "/restapis".to_string(),
            Some(json!({ "name": "refused" })),
        ),
        (
            "PutMethodResponse",
            "PUT",
            format!(
                "/restapis/{}/resources/{}/methods/GET/responses/404",
                method.api, method.resource
            ),
            Some(json!({})),
        ),
        (
            "UpdateRestApi",
            "PATCH",
            format!("/restapis/{}", method.api),
            Some(json!({
                "patchOperations": [
                    { "op": "replace", "path": "/description", "value": "refused" }
                ]
            })),
        ),
        (
            "DeleteRestApi",
            "DELETE",
            format!("/restapis/{}", method.api),
            None,
        ),
    ];
    for (case, verb, path, body) in &writes {
        let refused = call_as(&gateway, SESSION_KEY_ID, verb, path, body.clone()).await;
        let (status, error, answered) = (refused.status, refused.error.clone(), refused.body);
        assert_eq!(
            (status, error.as_deref()),
            (403, Some(ACCESS_DENIED)),
            "case {case:?}: expected 403 {ACCESS_DENIED}, got {status} {error:?} with \
             {answered:?}"
        );
        let action = format!("apigateway:{verb}");
        let message = string_member(&answered, "message", case);
        assert_eq!(
            message,
            not_authorized(
                &session_arn(),
                &action,
                &apigateway_arn(path),
                &format!("because no identity-based policy allows the {action} action"),
            ),
            "case {case:?}: expected the refusal to name the verb and the path of the request, \
             got {message:?}"
        );
    }

    // Nothing the refusals asked for happened: the api is still there, with the one method
    // response the setup put on it.
    let read = gateway
        .call("GET", &format!("/restapis/{}", method.api), None)
        .await;
    assert_eq!(
        read.status, 200,
        "case \"after every refusal\": expected the api to be untouched, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.body["description"].as_str(),
        None,
        "case \"after every refusal\": expected the refused patch to have changed nothing, got \
         {:?}",
        read.body
    );
    let refused_response = gateway
        .call(
            "GET",
            &format!(
                "/restapis/{}/resources/{}/methods/GET/responses/404",
                method.api, method.resource
            ),
            None,
        )
        .await;
    assert_eq!(
        refused_response.status, 404,
        "case \"after every refusal\": expected no method response to have been written, got \
         {} with {:?}",
        refused_response.status, refused_response.body
    );
}

#[tokio::test]
async fn a_grant_this_emulator_cannot_read_grounds_no_refusal() {
    let gateway = Gateway::new();
    let method = Method::stand_up(&gateway, "control-plane-authorization-principals").await;
    let path = method.method_response_path();
    let resource = apigateway_arn(&path);

    // A session of a role the IAM store holds no record for carries no grant at all, which is
    // IAM's default deny: a role with no identity-based policy has no permissions, so the call
    // is refused rather than served. The session itself is recorded the way a mint records
    // one, so the principal resolves and the refusal names it.
    let session_key = "ASIAEXAMPLEUNHELDROL";
    let session = format!(
        "arn:aws:sts::{}:assumed-role/not-held/unheld",
        ::engine::emulator_account_id()
    );
    let role_arn = format!(
        "arn:aws:iam::{}:role/not-held",
        ::engine::emulator_account_id()
    );
    let identity = sts::Identity {
        user_id: format!("{}:unheld", sts::role_id(&role_arn)),
        account: ::engine::emulator_account_id().to_string(),
        arn: session.clone(),
    };
    let vend = sts::NewSession {
        role_arn: Some(role_arn),
        ..sts::NewSession::new(identity, sts::SessionKind::AssumedRole, 3_600)
    };
    let sessions = sts::service_state(gateway.engine()).expect("the STS store");
    sessions
        .lock()
        .insert(vend.into_session(session_key, SystemTime::now()))
        .expect("the vended session is recorded");

    let case = "a session of a role the account holds no policies for";
    let answered = call_as(&gateway, session_key, "GET", &path, None).await;
    let (status, error, body) = (answered.status, answered.error.clone(), answered.body);
    assert_eq!(
        (status, error.as_deref()),
        (403, Some(ACCESS_DENIED)),
        "case {case:?}: expected 403 {ACCESS_DENIED}, got {status} {error:?} with {body:?}"
    );
    let message = string_member(&body, "message", case);
    assert_eq!(
        message,
        not_authorized(
            &session,
            "apigateway:GET",
            &resource,
            "because no identity-based policy allows the apigateway:GET action",
        ),
        "case {case:?}: expected IAM's default-deny sentence naming the session, got {message:?}"
    );

    // A principal whose grant this emulator CANNOT READ is the other case: an AWS-managed
    // policy's document belongs to AWS rather than to this account, so the store holds none
    // for it, and refusing on a grant it cannot see would deny access AWS permits.
    given_session(&gateway);
    given_policy(&gateway, Entity::Role, None);
    let role_name = seeded_role_name();
    let store = iam::service_state(gateway.engine()).expect("the IAM store");
    store.lock().role_attached_policies.insert(
        iam::name_key(&role_name),
        std::collections::BTreeSet::from([
            "arn:aws:iam::aws:policy/NotCarriedByThisEmulator".to_string()
        ]),
    );
    let served = call_as(&gateway, SESSION_KEY_ID, "GET", &path, None).await;
    let (status, error, body) = (served.status, served.error.clone(), served.body);
    assert_eq!(
        (status, error.as_deref()),
        (200, None),
        "case \"a grant this emulator cannot read\": expected the call to be served, got \
         {status} {error:?} with {body:?}"
    );
}
