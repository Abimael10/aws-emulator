// SPDX-License-Identifier: CC0-1.0

//! WHICH RESOURCE an authorization refusal of this service names, driven end to end over
//! the real wire path (a restJson1 request through the multi-service router into the
//! mounted service), so what is asserted is what a client holding those credentials
//! reads.
//!
//! THE RULE UNDER TEST: a request refused at the IAM layer is decided — and its refusal
//! worded — against the ARN of the resource the REQUEST ITSELF names: the resource its URI
//! addresses, or, for a create, the resource its own required input member names, built in
//! the Region the caller signed for. IAM's any-resource wildcard appears only where the
//! request names no single resource of its own. The answer is HTTP 403
//! `AccessDeniedException` carrying IAM's sentence, and nothing is created or changed.
//!
//! The rows span what the rule generalises over: every resource family this service mints
//! ARNs for (an email identity, a configuration set, a contact list, a dedicated IP pool,
//! an email template), WHERE the request states the resource (a URI label, a deeper path
//! under one resource, a create's own input member, a label a client percent-encoded), the
//! method it arrives on, the Region it is signed for, and the requests that name no single
//! resource (an account-level read, a listing, a send).
//!
//! The resource types and the input members a create names its resource in come from this
//! model's own `@http` URIs and required members
//! (`smithy_query(query="operation", operation="CreateConfigurationSet,CreateContactList,
//! CreateEmailIdentity,CreateDedicatedIpPool,CreateEmailTemplate,GetConfigurationSet,
//! GetEmailIdentity,GetContactList,DeleteDedicatedIpPool,
//! GetConfigurationSetEventDestinations,GetAccount,SendEmail,ListContactLists")`), and
//! cover families and operations no recorded case here drives.
//!
//! The role, its policy and the session live in the IAM and STS stores of the SAME shared
//! engine — the stores those services' own handlers write — so the documents under test
//! are the ones `iam:PutRolePolicy` stores and the principal a refusal blames is the one
//! `sts:GetCallerIdentity` answers for the key the request signed with.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

/// This service's IAM action namespace: its `arnNamespace`, which is also its `sigv4Name`
/// (`smithy_query(query="service_info")`).
const ACTION_PREFIX: &str = "ses";

/// The error code an authorization refusal carries, and the status it is carried at.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The access key id of the assumed-role session every refused row signs with, the role it
/// is assumed from and the session's own name.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESV2SESS";
const ROLE: &str = "sesv2-sender";
const SESSION: &str = "sender-session";

/// The access key id of the account's own published credential, which this gate does not
/// bound: a read-back that must see what the refused calls did (and did not) leave behind
/// signs with it.
const ACCOUNT_KEY_ID: &str = "AKIAEMULATOR0000000";

/// The name of the inline policy the role carries.
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// One answered request: the status, the error code when there was one, and the body.
struct Answer {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    /// The message the answer carried, or `None` when it carried none.
    fn message(&self) -> Option<&str> {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
    }
}

/// This service beside IAM and the token service over ONE shared engine — the shape
/// `core/router/src/lib.rs` mounts every service in — so the policies and the session a
/// case seeds are the ones those services hold.
struct Gate {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Gate {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The subject here is the IAM decision, not SigV4: the credential scope still
        // names the access key id and the Region, which is what the gate reads.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// The request `method path` with `body`, signed by `access_key_id` for `region`.
    async fn call_as(
        &self,
        access_key_id: &str,
        region: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Answer {
        let payload = match body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let host = format!("email.{region}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            // The credential scope carries the access key id the gate resolves the
            // principal from and the Region the ARNs of the decision are built in.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/ses/aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Answer {
            status,
            error_type,
            body,
        }
    }
}

/// The ARN the account holds the role under — the identity whose policies the session's
/// requests are judged on.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/{ROLE}",
        ::engine::emulator_account_id()
    )
}

/// The principal ARN the seeded session speaks for — what its refusals must name.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
        ::engine::emulator_account_id()
    )
}

/// An ARN of this service in `region`, as this service publishes one.
fn ses_arn(region: &str, resource: &str) -> String {
    format!(
        "arn:aws:{ACTION_PREFIX}:{region}:{}:{resource}",
        ::engine::emulator_account_id()
    )
}

/// Record the role and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them.
fn given_session(engine: &Arc<engine::InMemoryEngine>) {
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLESESV2ROLE".to_string(),
            arn: role_arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: None,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let vend = sts::NewSession {
        role_arn: Some(role_arn.clone()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn),
                account: ::engine::emulator_account_id().to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let store = sts::service_state(&**engine).expect("the token service's store");
    store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the session is recorded");
}

/// Put `document` in force on the role the session is of, as `iam:PutRolePolicy` does.
fn given_role_policy(engine: &Arc<engine::InMemoryEngine>, document: &Value) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.role_inline_policies.insert(
        iam::name_key(ROLE),
        BTreeMap::from([(
            iam::name_key(POLICY_NAME),
            iam::InlinePolicy {
                policy_name: POLICY_NAME.to_string(),
                policy_document: document.to_string(),
            },
        )]),
    );
}

/// A policy granting `actions` on `resource` and nothing else.
fn allow(actions: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }],
    })
}

/// The sentence IAM refuses a request with when no document in force allows it: who, the
/// action, the resource the request named, and which of IAM's rules withheld the grant.
fn not_authorized(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// Assert `answer` is the authorization refusal, carrying `message`.
fn assert_refused(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        (answer.status, answer.error_type.as_deref()),
        (ACCESS_DENIED_STATUS, Some(ACCESS_DENIED)),
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {}",
        answer.status,
        answer.error_type,
        answer.body
    );
    assert_eq!(
        answer.message(),
        Some(message),
        "case {case:?}: expected IAM's wording, got {:?}",
        answer.message()
    );
}

/// One request under the gate: its route, a body the modelled constraints admit (so a
/// refused row is refused by the gate rather than by the request layer), the Region it is
/// signed for, the operation it asks for, and the resource segment its refusal must name.
struct Row {
    case: &'static str,
    region: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    operation: &'static str,
    resource: String,
}

/// The names the rows below address. Nothing creates them: the decision is made against
/// the ARN the request carried whether or not the account holds that resource.
const SET: &str = "gated-set";
const HELD_SET: &str = "held-set";
const LIST: &str = "weekly-digest";
const IDENTITY: &str = "notices.example.org";
const LABELLED_IDENTITY: &str = "sender+tag@example.com";
const POOL: &str = "warm-pool";
const COLD_POOL: &str = "cold-pool";
const TEMPLATE: &str = "receipt";

/// Every row of the table, spanning the families, the request forms and the Regions.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the create of a configuration set names the set it is about to make",
            region: "us-east-1",
            method: "POST",
            path: "/v2/email/configuration-sets".to_string(),
            body: Some(json!({ "ConfigurationSetName": SET })),
            operation: "CreateConfigurationSet",
            resource: ses_arn("us-east-1", &format!("configuration-set/{SET}")),
        },
        Row {
            case: "the create of a contact list, signed for another Region",
            region: "us-west-2",
            method: "POST",
            path: "/v2/email/contact-lists".to_string(),
            body: Some(json!({ "ContactListName": LIST })),
            operation: "CreateContactList",
            resource: ses_arn("us-west-2", &format!("contact-list/{LIST}")),
        },
        Row {
            case: "the create of an email identity, signed for a third Region",
            region: "eu-central-1",
            method: "POST",
            path: "/v2/email/identities".to_string(),
            body: Some(json!({ "EmailIdentity": IDENTITY })),
            operation: "CreateEmailIdentity",
            resource: ses_arn("eu-central-1", &format!("identity/{IDENTITY}")),
        },
        Row {
            case: "the create of a dedicated IP pool",
            region: "us-east-1",
            method: "POST",
            path: "/v2/email/dedicated-ip-pools".to_string(),
            body: Some(json!({ "PoolName": POOL })),
            operation: "CreateDedicatedIpPool",
            resource: ses_arn("us-east-1", &format!("dedicated-ip-pool/{POOL}")),
        },
        Row {
            case: "the create of an email template",
            region: "ap-southeast-2",
            method: "POST",
            path: "/v2/email/templates".to_string(),
            body: Some(json!({
                "TemplateName": TEMPLATE,
                "TemplateContent": { "Subject": "Your receipt", "Text": "Thank you" },
            })),
            operation: "CreateEmailTemplate",
            resource: ses_arn("ap-southeast-2", &format!("template/{TEMPLATE}")),
        },
        Row {
            case: "a configuration set addressed by a URI label",
            region: "us-east-1",
            method: "GET",
            path: format!("/v2/email/configuration-sets/{HELD_SET}"),
            body: None,
            operation: "GetConfigurationSet",
            resource: ses_arn("us-east-1", &format!("configuration-set/{HELD_SET}")),
        },
        Row {
            case: "a contact list addressed by a URI label",
            region: "us-east-1",
            method: "GET",
            path: format!("/v2/email/contact-lists/{LIST}"),
            body: None,
            operation: "GetContactList",
            resource: ses_arn("us-east-1", &format!("contact-list/{LIST}")),
        },
        Row {
            case: "an email identity whose label a client percent-encoded",
            region: "us-east-1",
            method: "GET",
            path: "/v2/email/identities/sender%2Btag%40example.com".to_string(),
            body: None,
            operation: "GetEmailIdentity",
            resource: ses_arn("us-east-1", &format!("identity/{LABELLED_IDENTITY}")),
        },
        Row {
            case: "a dedicated IP pool a delete addresses",
            region: "us-east-1",
            method: "DELETE",
            path: format!("/v2/email/dedicated-ip-pools/{COLD_POOL}"),
            body: None,
            operation: "DeleteDedicatedIpPool",
            resource: ses_arn("us-east-1", &format!("dedicated-ip-pool/{COLD_POOL}")),
        },
        Row {
            case: "a deeper path under one configuration set",
            region: "us-east-1",
            method: "GET",
            path: format!("/v2/email/configuration-sets/{HELD_SET}/event-destinations"),
            body: None,
            operation: "GetConfigurationSetEventDestinations",
            resource: ses_arn("us-east-1", &format!("configuration-set/{HELD_SET}")),
        },
        Row {
            case: "an account-level read, which names no single resource",
            region: "us-east-1",
            method: "GET",
            path: "/v2/email/account".to_string(),
            body: None,
            operation: "GetAccount",
            resource: "*".to_string(),
        },
        Row {
            case: "a listing over a collection, which names no single resource",
            region: "us-east-1",
            method: "GET",
            path: "/v2/email/contact-lists".to_string(),
            body: None,
            operation: "ListContactLists",
            resource: "*".to_string(),
        },
        Row {
            case: "a send, which names no single resource",
            region: "us-east-1",
            method: "POST",
            path: "/v2/email/outbound-emails".to_string(),
            body: Some(json!({
                "FromEmailAddress": "sender@example.com",
                "Destination": { "ToAddresses": ["recipient@example.com"] },
                "Content": {
                    "Simple": {
                        "Subject": { "Data": "Hello" },
                        "Body": { "Text": { "Data": "Hello" } },
                    },
                },
            })),
            operation: "SendEmail",
            resource: "*".to_string(),
        },
    ]
}

/// RULE: the refusal of a request no document in force allows names the ARN of the
/// resource THE REQUEST named — read off its URI, or off the required input member a
/// create names the resource it is about to make in — and IAM's any-resource wildcard only
/// where the request names no single resource.
#[tokio::test]
async fn a_refusal_names_the_arn_of_the_resource_the_request_named() {
    let gate = Gate::new();
    given_session(&gate.engine);
    // A grant of another service's actions: nothing of this service is allowed, so every
    // row falls to IAM's default deny.
    given_role_policy(&gate.engine, &allow("logs:*", "*"));

    for row in rows() {
        let answer = gate
            .call_as(
                SESSION_KEY_ID,
                row.region,
                row.method,
                &row.path,
                row.body.as_ref(),
            )
            .await;
        let action = format!("{ACTION_PREFIX}:{}", row.operation);
        assert_refused(
            row.case,
            &answer,
            &not_authorized(&session_arn(), &action, &row.resource),
        );
    }

    // A refused create stores nothing: the resources the rows named are not held by the
    // account afterwards, read back with the credential this gate does not bound.
    for (case, path, region) in [
        (
            "the configuration set a refused create named",
            format!("/v2/email/configuration-sets/{SET}"),
            "us-east-1",
        ),
        (
            "the contact list a refused create named",
            format!("/v2/email/contact-lists/{LIST}"),
            "us-west-2",
        ),
        (
            "the email identity a refused create named",
            format!("/v2/email/identities/{IDENTITY}"),
            "eu-central-1",
        ),
        (
            "the dedicated IP pool a refused create named",
            format!("/v2/email/dedicated-ip-pools/{POOL}"),
            "us-east-1",
        ),
        (
            "the email template a refused create named",
            format!("/v2/email/templates/{TEMPLATE}"),
            "ap-southeast-2",
        ),
    ] {
        let read = gate
            .call_as(ACCOUNT_KEY_ID, region, "GET", &path, None)
            .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused create must store nothing, got {} with {}",
            read.status, read.body
        );
    }
}

/// RULE: the ARN the decision is made against is the request's own, so a statement scoped
/// to exactly one resource serves a call naming THAT resource and refuses a call naming
/// another — including the same name in another Region, since the ARN carries the Region
/// the caller signed for.
#[tokio::test]
async fn a_statement_scoped_to_one_resource_decides_only_that_resource() {
    let gate = Gate::new();
    given_session(&gate.engine);
    let region = "us-west-2";
    let action = format!("{ACTION_PREFIX}:CreateConfigurationSet");
    let scoped = "scoped-set";
    let other = "other-set";
    given_role_policy(
        &gate.engine,
        &allow(
            &action,
            &ses_arn(region, &format!("configuration-set/{scoped}")),
        ),
    );

    // The create of exactly the set the statement names reaches the operation.
    let served = gate
        .call_as(
            SESSION_KEY_ID,
            region,
            "POST",
            "/v2/email/configuration-sets",
            Some(&json!({ "ConfigurationSetName": scoped })),
        )
        .await;
    assert_eq!(
        served.status, 200,
        "case \"the set the statement names\": expected the create to be served, got {} with {}",
        served.status, served.body
    );
    let read = gate
        .call_as(
            ACCOUNT_KEY_ID,
            region,
            "GET",
            &format!("/v2/email/configuration-sets/{scoped}"),
            None,
        )
        .await;
    assert_eq!(
        read.status, 200,
        "case \"the set the statement names\": the served create must persist it, got {} with {}",
        read.status, read.body
    );

    // The create of another set under the same statement is refused, and the refusal names
    // THAT set's ARN rather than the one the statement allows.
    let refused = gate
        .call_as(
            SESSION_KEY_ID,
            region,
            "POST",
            "/v2/email/configuration-sets",
            Some(&json!({ "ConfigurationSetName": other })),
        )
        .await;
    assert_refused(
        "another set than the statement names",
        &refused,
        &not_authorized(
            &session_arn(),
            &action,
            &ses_arn(region, &format!("configuration-set/{other}")),
        ),
    );

    // The same create signed for another Region addresses another ARN, which the statement
    // does not cover — and the refusal names the Region the caller signed for.
    let elsewhere = "eu-west-1";
    let refused_elsewhere = gate
        .call_as(
            SESSION_KEY_ID,
            elsewhere,
            "POST",
            "/v2/email/configuration-sets",
            Some(&json!({ "ConfigurationSetName": scoped })),
        )
        .await;
    assert_refused(
        "the same set in another Region",
        &refused_elsewhere,
        &not_authorized(
            &session_arn(),
            &action,
            &ses_arn(elsewhere, &format!("configuration-set/{scoped}")),
        ),
    );

    // The refused create left nothing behind: the set it named is still not held. (The
    // set the Region row named is the one the SERVED create above persisted, which a
    // configuration set's account-wide store holds whichever Region a read signs for, so
    // the absent name is the one no served call ever made.)
    let read = gate
        .call_as(
            ACCOUNT_KEY_ID,
            region,
            "GET",
            &format!("/v2/email/configuration-sets/{other}"),
            None,
        )
        .await;
    assert_eq!(
        read.status, 404,
        "case \"another set than the statement names\": a refused create must store nothing, got \
         {} with {}",
        read.status, read.body
    );
}
