// SPDX-License-Identifier: CC0-1.0

//! Which `Resource` element decides an API Gateway REST CONTROL-PLANE call, and WHEN a document
//! a client writes on a role starts deciding one.
//!
//! > **RULE: a control-plane request of this plane is decided against the identity-based
//! > documents in force on its principal AT THE INSTANT OF THE CALL — so a document a client
//! > puts on a role after a credential of that role was minted, and after an identical call
//! > under that credential was already refused, decides the very next call made with it. The
//! > question asked is `apigateway:<the request's own HTTP METHOD>` on
//! > `arn:aws:apigateway:{region}::{the request's own URI path}` — an ARN whose ACCOUNT field is
//! > EMPTY and whose resource portion is a slash path — and a statement's `Resource` element is
//! > matched against that whole ARN text with IAM's two wildcards. A call a matching `Allow`
//! > covers is served; every other call is refused HTTP 403 `AccessDeniedException` with the
//! > sentence IAM words an authorization failure in, having performed nothing.**
//!
//! The two wildcards are the ones IAM documents for a policy's `Resource` element: `*` stands
//! for any combination of characters, including none, and `?` for exactly one character. They
//! are matched over the ARN as one string rather than field by field, so a `*` a developer
//! writes may span the separator between two path segments — which is what makes
//! `arn:aws:apigateway:{region}::/restapis*` the grant that covers both the collection and every
//! path beneath it, and what makes `arn:aws:apigateway:{region}::/restapis/*` cover the paths
//! beneath it and not the collection itself.
//!
//! Three crates have to agree for the rule to hold, which is why this is a router suite: IAM
//! holds the role and the document a client embedded in it, the token service mints the session a
//! request signs with, and API Gateway takes the decision and words the refusal. Everything is
//! driven the way a client reaches it — `iam:CreateRole`, `iam:PutRolePolicy`, `sts:AssumeRole`
//! and then the signed management call — so the document under test is the one IAM stored and the
//! principal a refusal blames is the one the credential was minted for.
//!
//! The table spans what the matching rule generalises over: where the wildcard sits in the
//! `Resource` element (at the end of a path, after a path separator, in place of one character,
//! standing for a whole field, standing for everything), which FIELD of the ARN it varies (the
//! Region, the account that this plane's ARNs leave empty, the resource path), which PATH the
//! row's own call addresses (the collection and one api beneath it), and which ACTION the one
//! `Allow` names — so a grant is seen to cover exactly the calls its text covers. Every row is
//! called under ONE credential minted before any document existed, with the role's single inline
//! document REPLACED between rows: the only thing differing between two rows is the document in
//! force at the instant the call is made, so a decision taken against a snapshot of the role's
//! permissions — the one a credential was minted with, or the one a resource was created with —
//! fails here.
//!
//! What this suite deliberately does NOT re-write: the in-function half of the same gate — a
//! handler whose execution role is refused, granted and then narrowed between invocations —
//! belongs to `apigateway_control_plane_role_authorization`'s own in-function test, which drives
//! exactly those beats. A second copy of that probe was removed from this directory once already
//! (see the note in `lambda_in_function_apigateway_control_plane_authorization`), so the rows
//! that suite has are asked for there and only the dimension it does not span is added here. The
//! sibling `apigateway_invoke_grant_arn_matching` is the same `Resource` question for the
//! `execute-api` data plane's method ARNs.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region every ARN below names.
const REGION: &str = "us-east-1";

/// A Region nothing here is addressed in, which one row's grant names instead.
const OTHER_REGION: &str = "eu-west-1";

/// An access key id neither the token service nor IAM holds a record for: the position the
/// account's own published credential is in, which is what the setup calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The awsQuery API versions the setup services are called at.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// The error shape and status an authorization refusal carries: IAM's own, in no operation's
/// modelled error set, because the decision is taken ahead of the service.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The action a READ of this plane performs, and the action a deployment WRITE performs: this
/// plane publishes no action per operation — its actions are the HTTP methods of the REST API
/// that serves it.
const READ_ACTION: &str = "apigateway:GET";
const WRITE_ACTION: &str = "apigateway:POST";

/// One answered request.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    text: String,
}

impl Answered {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }

    /// The error SHAPE a restJson1 failure names, where a restJson1 client reads it.
    fn error_code(&self) -> String {
        self.headers
            .get("x-amzn-errortype")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_else(|| "<absent>".to_string())
    }

    /// The `message` member of an error body.
    fn error_message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("<no message>")
            .to_string()
    }
}

/// A SigV4 `Authorization` whose credential names `access_key_id` and whose scope names
/// `signing_name` and [`REGION`], which is what routes the request and what Region the decision
/// is taken in. The signature itself is never checked here.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    access_key_id: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answered {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let response = router
        .dispatch(
            http::Request::builder()
                .method(method)
                .uri(format!("https://{host}{path}"))
                .header("host", &host)
                .header("authorization", authorization(access_key_id, signing_name))
                .header("content-type", content_type)
                .body(bytes::Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Answered {
        status,
        headers,
        text: String::from_utf8_lossy(response.body().as_ref()).to_string(),
    }
}

/// One awsQuery call that must be served, answered with its XML body.
async fn served_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        signing_name,
        ACCOUNT_KEY_ID,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        form.into_bytes(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "setup: {signing_name} {action} must answer 200, got {} with {}",
        answered.status, answered.text
    );
    answered.text
}

/// One API Gateway REST control-plane call, signed by `access_key_id`.
async fn control_plane(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Answered {
    dispatch(
        router,
        "apigateway",
        access_key_id,
        method,
        path,
        "application/json",
        body.map(|value| value.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await
}

/// One API Gateway REST control-plane call the account's own credential must be served, answered
/// with its document.
async fn served_control_plane(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    expected_status: u16,
) -> Value {
    let answered = control_plane(router, ACCOUNT_KEY_ID, method, path, body).await;
    assert_eq!(
        answered.status, expected_status,
        "setup: {what} must answer HTTP {expected_status}, got {} {}",
        answered.status, answered.text
    );
    answered.json()
}

/// A required non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what}: expected a non-empty {member:?}, got {body}"))
}

/// The ARN of an API Gateway resource whose URI path is `path`, in `region`: this plane's ARNs
/// carry an EMPTY account field, and their resource portion IS the path of the operation that
/// reaches them.
fn apigateway_arn_in(region: &str, path: &str) -> String {
    format!("arn:aws:apigateway:{region}::{path}")
}

/// [`apigateway_arn_in`] for the Region every call below is signed for.
fn apigateway_arn(path: &str) -> String {
    apigateway_arn_in(REGION, path)
}

/// The sentence IAM refuses an unauthorized call with: who was refused, which action, on which
/// resource, and which of IAM's rules refused it — here always the default deny of a call no
/// readable document allowed, since no row states a `Deny`.
fn not_authorized(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// An identity-based policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// The trust policy of a role the account's own credential assumes.
fn account_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {
                "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()),
            },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The one name an inline document is put on a role under, so a put REPLACES what the last one
/// wrote: "an inline policy document that is embedded in the specified IAM role"
/// (`PutRolePolicy`).
const INLINE_POLICY: &str = "grants";

/// Hold one role named `role_name` trusting `trust`, carrying no policy at all, through IAM's own
/// `CreateRole`. Answers the role's ARN.
async fn given_role(router: &MultiServiceRouter, role_name: &str, trust: &str) -> String {
    let created = served_query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Put `document` on `role_name` as its one inline policy — what a client grants an existing role
/// a missing permission with.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &Value) {
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
}

/// Mint a session of the role `role_arn` through the token service's own `AssumeRole`. Answers
/// the access key id the session was minted under and the principal ARN it speaks for.
async fn assumed_session(
    router: &MultiServiceRouter,
    role_arn: &str,
    session_name: &str,
) -> (String, String) {
    let assumed = served_query(
        router,
        "sts",
        STS_VERSION,
        "AssumeRole",
        &[("RoleArn", role_arn), ("RoleSessionName", session_name)],
    )
    .await;
    (element(&assumed, "AccessKeyId"), element(&assumed, "Arn"))
}

/// The path of the collection a REST-API discovery read addresses.
const COLLECTION_PATH: &str = "/restapis";

/// The member one page of a collection of this plane reports its elements at, ON THE WIRE: the
/// page member's `jsonName` is `item` (`smithy_query(query="shape", shape="RestApis")`), which an
/// SDK surfaces to its caller as `items`.
const WIRE_PAGE_MEMBER: &str = "item";

/// One REST API this account holds, created the way a client creates one. Answers its id.
async fn given_api(router: &MultiServiceRouter, name: &str) -> String {
    let created = served_control_plane(
        router,
        "CreateRestApi",
        "POST",
        COLLECTION_PATH,
        Some(&json!({ "name": name, "endpointConfiguration": { "types": ["REGIONAL"] } })),
        201,
    )
    .await;
    as_str("setup: CreateRestApi", &created, "id")
}

/// The ids a page of this plane reports, as a reader of the raw body reads them.
fn reported_ids(body: &Value) -> Vec<String> {
    body[WIRE_PAGE_MEMBER]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a served read ANSWERED ABOUT the api `api`: a collection read reports it among its
/// page, and a single-api read reports it as the record's own id. So a row that expects to be
/// served asserts the plane answered its call rather than merely not refusing it.
fn names_the_api(body: &Value, api: &str) -> bool {
    if body.get("id").and_then(Value::as_str) == Some(api) {
        return true;
    }
    reported_ids(body).contains(&api.to_string())
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = ::aws_emulator_router::build_router(::aws_emulator_router::new_engine());
    // Signature verification belongs to the emulator's front door, not to this suite: what is
    // under test is the AUTHORIZATION of a credential, not its signature.
    router.set_request_authentication(false);
    router
}

/// The role the table calls under, and the name its session is opened with.
const SESSION_ROLE: &str = "apigw-cp-arn-matching-role";
const SESSION_NAME: &str = "arn-matching";

/// One row of the matching table: the action and the `Resource` element the row's one `Allow`
/// names, the path the row's call addresses, and whether the call is served.
struct Row {
    case: &'static str,
    /// The action that one `Allow` names, which is the read verb for every row but the one that
    /// shows a grant of another verb deciding no read.
    granted_action: &'static str,
    /// Built from the id of the api the plane holds, so no recorded id is written here.
    resource: fn(&str) -> String,
    /// The path the row's call addresses, built from that same id.
    addressed: fn(&str) -> String,
    served: bool,
}

/// The path of the collection, for a row that calls it.
fn collection_path(_api: &str) -> String {
    COLLECTION_PATH.to_string()
}

/// The path of one api beneath the collection, for a row that calls it.
fn api_path(api: &str) -> String {
    format!("{COLLECTION_PATH}/{api}")
}

/// The rows of the matching table. They vary where the wildcard sits in the `Resource` element,
/// which field of the ARN it varies, which path the call addresses and which action the `Allow`
/// names — the things the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The grant a client writes to open up this plane's api collection.
            case: "a trailing wildcard on the collection's own path, calling the collection",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn(&format!("{COLLECTION_PATH}*")),
            addressed: collection_path,
            served: true,
        },
        Row {
            // The same text decides a DEEPER path too: the wildcard is matched over the ARN as
            // one string, so it spans the separator the api's own segment begins with.
            case: "a trailing wildcard on the collection's own path, calling one api",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn(&format!("{COLLECTION_PATH}*")),
            addressed: api_path,
            served: true,
        },
        Row {
            case: "the collection's exact ARN with no wildcard, calling the collection",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn(COLLECTION_PATH),
            addressed: collection_path,
            served: true,
        },
        Row {
            case: "the collection's exact ARN with no wildcard, calling one api",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn(COLLECTION_PATH),
            addressed: api_path,
            served: false,
        },
        Row {
            case: "a wildcard after the collection's separator, calling the collection",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn(&format!("{COLLECTION_PATH}/*")),
            addressed: collection_path,
            served: false,
        },
        Row {
            case: "a wildcard after the collection's separator, calling one api",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn(&format!("{COLLECTION_PATH}/*")),
            addressed: api_path,
            served: true,
        },
        Row {
            // `?` stands for exactly one character, which the collection's last character is.
            case: "a single-character wildcard standing for the path's last character",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn("/restapi?"),
            addressed: collection_path,
            served: true,
        },
        Row {
            // And for exactly one, so it does not stand for none.
            case: "a single-character wildcard with no character left to stand for",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn(&format!("{COLLECTION_PATH}?")),
            addressed: collection_path,
            served: false,
        },
        Row {
            case: "every path of this plane in this Region",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn("*"),
            addressed: api_path,
            served: true,
        },
        Row {
            case: "every resource of every service",
            granted_action: READ_ACTION,
            resource: |_api| "*".to_string(),
            addressed: collection_path,
            served: true,
        },
        Row {
            case: "a wildcard in the Region field, which every Region matches",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn_in("*", &format!("{COLLECTION_PATH}*")),
            addressed: api_path,
            served: true,
        },
        Row {
            case: "the same path in another Region",
            granted_action: READ_ACTION,
            resource: |_api| apigateway_arn_in(OTHER_REGION, &format!("{COLLECTION_PATH}*")),
            addressed: collection_path,
            served: false,
        },
        Row {
            // This plane's ARNs leave the account field EMPTY, so a statement that writes an
            // account into it names nothing this plane addresses.
            case: "an ARN carrying an account, which this plane's ARNs never do",
            granted_action: READ_ACTION,
            resource: |_api| {
                format!(
                    "arn:aws:apigateway:{REGION}:{}:{COLLECTION_PATH}*",
                    ::engine::emulator_account_id()
                )
            },
            addressed: collection_path,
            served: false,
        },
        Row {
            // The verb is part of the question too: a grant of this plane's WRITE action decides
            // no read of it, whatever resource it names.
            case: "every resource of this plane under the write action alone",
            granted_action: WRITE_ACTION,
            resource: |_api| apigateway_arn("*"),
            addressed: collection_path,
            served: false,
        },
    ]
}

/// RULE: a control-plane read is served exactly where the document in force on the caller at the
/// instant of the call carries an `Allow` of `apigateway:GET` whose `Resource` element matches the
/// ARN of the path the request addressed; every other call is refused 403 `AccessDeniedException`
/// with IAM's own sentence.
#[tokio::test]
async fn a_grants_resource_element_decides_each_control_plane_read() {
    let router = emulator();
    let api = given_api(&router, "control-plane-grant-arn-matching").await;
    let role_arn = given_role(&router, SESSION_ROLE, &account_trust_policy()).await;
    // The credential is minted BEFORE any document exists on the role. Every row below replaces
    // the role's one inline document and calls again under this SAME session, so what each row
    // shows is the document in force at the instant of its own call rather than a snapshot taken
    // when the credential was minted.
    let (access_key_id, principal) = assumed_session(&router, &role_arn, SESSION_NAME).await;

    let case = "a session of a role carrying no document at all";
    let refused = control_plane(&router, &access_key_id, "GET", COLLECTION_PATH, None).await;
    assert_refused(
        case,
        &refused,
        &principal,
        READ_ACTION,
        &apigateway_arn(COLLECTION_PATH),
    );

    for row in rows() {
        let resource = (row.resource)(&api);
        let path = (row.addressed)(&api);
        let action = row.granted_action;
        put_role_policy(&router, SESSION_ROLE, &allow(action, &resource)).await;
        let answered = control_plane(&router, &access_key_id, "GET", &path, None).await;
        let case = row.case;
        if row.served {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected the grant of {action} on {resource:?} to serve GET \
                 {path}, got {} {}",
                answered.status, answered.text
            );
            let body = answered.json();
            assert!(
                names_the_api(&body, &api),
                "case {case:?}: expected the served read of {path} to answer about the api \
                 {api:?}, got {body}"
            );
        } else {
            assert_refused(
                case,
                &answered,
                &principal,
                READ_ACTION,
                &apigateway_arn(&path),
            );
        }
    }

    // The account's own published credential holds no IAM policies — the position AWS account
    // root credentials are in — so it is not bounded by this gate: the positive control that the
    // plane itself serves the very calls every refused row made.
    let case = "the account's own published credential";
    let read = control_plane(&router, ACCOUNT_KEY_ID, "GET", COLLECTION_PATH, None).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected the collection read to be served, got {} {}",
        read.status, read.text
    );
    let read = control_plane(&router, ACCOUNT_KEY_ID, "GET", &api_path(&api), None).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected the single-api read to be served, got {} {}",
        read.status, read.text
    );
}

/// Assert one refused call: the status, the error shape a client branches on, and IAM's own
/// sentence naming the principal, the action, the resource the request addressed and the default
/// deny that refused it.
fn assert_refused(
    case: &str,
    answered: &Answered,
    principal: &str,
    action: &str,
    resource: &str,
) {
    let code = answered.error_code();
    assert_eq!(
        (answered.status, code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {}",
        answered.status,
        answered.text
    );
    let expected = not_authorized(principal, action, resource);
    assert_eq!(
        answered.error_message(),
        expected,
        "case {case:?}: expected IAM's authorization sentence {expected:?}, got {}",
        answered.text
    );
}
