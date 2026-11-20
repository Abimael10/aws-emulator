// SPDX-License-Identifier: CC0-1.0

//! The AUTHORIZATION every request to this service is subject to, driven over each service's
//! own wire protocol through the real `MultiServiceRouter` with EventBridge Scheduler, IAM and
//! the token service mounted on ONE engine — the shape `core/router/src/lib.rs` mounts them in
//! — so the policies a decision reads are the documents IAM's own handlers wrote and the caller
//! is the principal that holds the key the request signed with.
//!
//! The RULE asserted here: AWS decides authorization at the IAM layer ahead of the service's
//! own code, so a request is served only where the identity-based policies in force on the
//! calling principal allow it `scheduler:<OperationName>` on the resource the request addresses;
//! otherwise it is refused with HTTP 403 `AccessDeniedException`, worded as the principal, that
//! action, that resource and which of IAM's rules withheld the grant — and it performs nothing.
//!
//! The rows span the class rather than one operation: there is one per operation this service
//! binds (`smithy_query(query="digest")` lists twelve), so they differ along the action the
//! request performs, the method and URI it is bound to, and the resource IAM judges it on — a
//! schedule, a schedule group, the ARN a tagging request carries, and the `*` IAM writes for a
//! listing, which addresses the account rather than one resource. Each row is driven three
//! times: by a caller whose documents DENY that one action, by a caller whose documents say
//! nothing about this service at all, and by a caller granted every action of it — the two IAM
//! reasons a refusal can rest on, and the grant that lifts both.
//!
//! The sibling suite `crate::pass_role_authorization` covers the other permission a request may
//! need: the pass of a target's execution role, which is an action on a different resource.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::SCHEDULER_SERVICE_PRINCIPAL;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{account_id, encode_label, error_code, error_message, xml_text};

/// The Region every call below is signed for.
const REGION: &str = "eu-west-2";

/// The access key id the account's own administrative calls present: the account holds no
/// record of it, so it resolves to the account's own published credential, which no identity
/// policy governs.
const ACCOUNT_KEY: &str = "AKIDTESTTESTTEST";

/// The code and the status AWS refuses an authorization failure with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The namespace this service publishes its IAM actions under.
const ACTION_PREFIX: &str = "scheduler";

/// How IAM writes the resource of a request that addresses no single resource of its own.
const ANY_RESOURCE: &str = "*";

/// The estate every row stands on: a schedule group, a schedule inside it, and the role those
/// schedules name as their execution role.
const GROUP: &str = "authz-group";
const SCHEDULE: &str = "authz-schedule";
const TARGET_ROLE: &str = "authz-target-runner";

/// The tag key a tagging row writes and removes.
const TAG_KEY: &str = "team";

/// The API versions the two awsQuery services below are called at.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// EventBridge Scheduler, IAM and the token service over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One restJson1 call against EventBridge Scheduler presenting `access_key_id`, which is what
/// selects the principal the request speaks for.
async fn scheduler_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(bytes::Bytes::new, |json| {
        bytes::Bytes::from(json.to_string())
    });
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("scheduler.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/scheduler/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(payload)
        .expect("a well-formed restJson1 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One awsQuery call against `signing_name`, answered as the XML text it is.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&encode_label(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// An identity-based document holding `statements`.
fn document(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The pass of the target role, which every caller below holds: a row is about the request's
/// OWN action, so nothing of it turns on whether the caller may hand over a role.
fn passing_the_target_role() -> Value {
    json!({
        "Effect": "Allow",
        "Action": "iam:PassRole",
        "Resource": format!("arn:aws:iam::{}:role/{TARGET_ROLE}", account_id()),
    })
}

/// A document allowing every action of this service.
fn allowing_everything() -> Value {
    document(vec![
        json!({ "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" }),
        passing_the_target_role(),
    ])
}

/// A document allowing every action of this service EXCEPT `operation`, which it denies —
/// IAM's explicit deny, which wins over the allow beside it.
fn denying(operation: &str) -> Value {
    document(vec![
        json!({ "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" }),
        json!({
            "Effect": "Deny",
            "Action": format!("{ACTION_PREFIX}:{operation}"),
            "Resource": "*",
        }),
        passing_the_target_role(),
    ])
}

/// A document that says nothing about this service at all, so every request to it falls to
/// IAM's default deny.
fn saying_nothing() -> Value {
    document(vec![passing_the_target_role()])
}

/// An IAM user holding `policy` and a long-term access key, answered as `(key, principal ARN)`.
async fn given_user(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy: &Value,
) -> (String, String) {
    let (status, body) = query(
        router,
        "iam",
        IAM_VERSION,
        "CreateUser",
        &[("UserName", name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateUser {name} answered {status} {body}"
    );
    let principal = xml_text(&body, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateUser {name} answered no Arn: {body}"));
    let (status, body) = query(
        router,
        "iam",
        IAM_VERSION,
        "PutUserPolicy",
        &[
            ("UserName", name.to_string()),
            ("PolicyName", "scheduler-actions".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutUserPolicy for {name} answered {status} {body}"
    );
    let (status, body) = query(
        router,
        "iam",
        IAM_VERSION,
        "CreateAccessKey",
        &[("UserName", name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAccessKey for {name} answered {status} {body}"
    );
    let key = xml_text(&body, "AccessKeyId").unwrap_or_else(|| {
        panic!("case {case:?}: CreateAccessKey for {name} answered no AccessKeyId: {body}")
    });
    (key, principal)
}

/// The estate a row stands on, written with the account's own credential: the execution role
/// the schedules name, the group, the schedule inside it, and one tag on the group.
async fn given_estate(router: &MultiServiceRouter, case: &str) {
    let trust = document(vec![json!({
        "Effect": "Allow",
        "Principal": { "Service": SCHEDULER_SERVICE_PRINCIPAL },
        "Action": "sts:AssumeRole",
    })]);
    let (status, body) = query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", TARGET_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole {TARGET_ROLE} answered {status} {body}"
    );
    // The token service is mounted for the same reason the sibling suite mounts it: a
    // credential the account holds a record for is what makes a caller bounded at all.
    let (status, body) = query(router, "sts", STS_VERSION, "GetCallerIdentity", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCallerIdentity answered {status} {body}"
    );
    let (status, body) = scheduler_as(
        router,
        ACCOUNT_KEY,
        "POST",
        &format!("/schedule-groups/{GROUP}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateScheduleGroup answered {status} {body}"
    );
    let (status, body) = scheduler_as(
        router,
        ACCOUNT_KEY,
        "POST",
        &format!("/schedules/{SCHEDULE}"),
        Some(schedule_body()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule answered {status} {body}"
    );
    let (status, body) = scheduler_as(
        router,
        ACCOUNT_KEY,
        "POST",
        &format!("/tags/{}", encode_label(&group_arn())),
        Some(json!({ "Tags": [{ "Key": TAG_KEY, "Value": "orders" }] })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TagResource answered {status} {body}"
    );
}

/// A whole `CreateSchedule`/`UpdateSchedule` body for the estate's group.
///
/// The expression is a one-time instant far ahead, so no occurrence of a schedule this suite
/// writes is ever due and a row asserts the control plane alone.
fn schedule_body() -> Value {
    json!({
        "GroupName": GROUP,
        "ScheduleExpression": "at(2099-12-31T23:59:59)",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "Target": {
            "Arn": format!("arn:aws:sqs:{REGION}:{}:target", account_id()),
            "RoleArn": format!("arn:aws:iam::{}:role/{TARGET_ROLE}", account_id()),
        },
    })
}

/// The ARN of the estate's schedule group.
fn group_arn() -> String {
    format!(
        "arn:aws:scheduler:{REGION}:{}:schedule-group/{GROUP}",
        account_id()
    )
}

/// The ARN of a schedule of the estate's group.
fn schedule_arn(name: &str) -> String {
    format!(
        "arn:aws:scheduler:{REGION}:{}:schedule/{GROUP}/{name}",
        account_id()
    )
}

/// One row: an operation of this service, a request to it that is served when the caller is
/// granted the action, and the resource IAM judges that request on.
struct Row {
    /// The operation's own name, which is the action it performs under this service's
    /// namespace.
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// The ARN the request addresses, as the refusal names it.
    resource: String,
}

/// One row per operation this service binds.
fn rows() -> Vec<Row> {
    let encoded_group = encode_label(&group_arn());
    vec![
        Row {
            operation: "CreateSchedule",
            method: "POST",
            uri: "/schedules/minted".to_string(),
            body: Some(schedule_body()),
            resource: schedule_arn("minted"),
        },
        Row {
            operation: "GetSchedule",
            method: "GET",
            uri: format!("/schedules/{SCHEDULE}?groupName={GROUP}"),
            body: None,
            resource: schedule_arn(SCHEDULE),
        },
        Row {
            operation: "UpdateSchedule",
            method: "PUT",
            uri: format!("/schedules/{SCHEDULE}"),
            body: Some(schedule_body()),
            resource: schedule_arn(SCHEDULE),
        },
        Row {
            operation: "DeleteSchedule",
            method: "DELETE",
            uri: format!("/schedules/{SCHEDULE}?groupName={GROUP}"),
            body: None,
            resource: schedule_arn(SCHEDULE),
        },
        Row {
            // A listing addresses the account rather than one schedule, which is where IAM
            // writes `*`.
            operation: "ListSchedules",
            method: "GET",
            uri: format!("/schedules?ScheduleGroup={GROUP}"),
            body: None,
            resource: ANY_RESOURCE.to_string(),
        },
        Row {
            operation: "CreateScheduleGroup",
            method: "POST",
            uri: "/schedule-groups/minted-group".to_string(),
            body: None,
            resource: format!(
                "arn:aws:scheduler:{REGION}:{}:schedule-group/minted-group",
                account_id()
            ),
        },
        Row {
            operation: "GetScheduleGroup",
            method: "GET",
            uri: format!("/schedule-groups/{GROUP}"),
            body: None,
            resource: group_arn(),
        },
        Row {
            operation: "DeleteScheduleGroup",
            method: "DELETE",
            uri: format!("/schedule-groups/{GROUP}"),
            body: None,
            resource: group_arn(),
        },
        Row {
            operation: "ListScheduleGroups",
            method: "GET",
            uri: "/schedule-groups".to_string(),
            body: None,
            resource: ANY_RESOURCE.to_string(),
        },
        Row {
            // A tagging request addresses the resource by the ARN it carries, so that ARN is
            // what IAM judges it on.
            operation: "TagResource",
            method: "POST",
            uri: format!("/tags/{encoded_group}"),
            body: Some(json!({ "Tags": [{ "Key": "owner", "Value": "orders" }] })),
            resource: group_arn(),
        },
        Row {
            operation: "UntagResource",
            method: "DELETE",
            uri: format!("/tags/{encoded_group}?TagKeys={TAG_KEY}"),
            body: None,
            resource: group_arn(),
        },
        Row {
            operation: "ListTagsForResource",
            method: "GET",
            uri: format!("/tags/{encoded_group}"),
            body: None,
            resource: group_arn(),
        },
    ]
}

/// The sentence AWS refuses an unauthorized request with: the principal, the action, the
/// resource and which of IAM's rules withheld the grant.
fn refusal(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// RULE: every operation of this service is served only where the caller's identity-based
/// policies allow it `scheduler:<OperationName>` on the resource the request addresses, and the
/// refusal names all four parts of that decision.
#[tokio::test]
async fn every_operation_is_judged_on_its_own_action_and_resource() {
    for row in rows() {
        let case = row.operation;
        let action = format!("{ACTION_PREFIX}:{}", row.operation);
        // A fresh emulator per row: the row's own request is the only thing that changes
        // state, so a destructive operation cannot decide another row's outcome.
        let router = emulator();
        given_estate(&router, case).await;
        let (denying_key, denying_principal) =
            given_user(&router, case, "denied-author", &denying(row.operation)).await;
        let (silent_key, silent_principal) =
            given_user(&router, case, "ungranted-author", &saying_nothing()).await;
        let (allowed_key, _allowed_principal) =
            given_user(&router, case, "granted-author", &allowing_everything()).await;

        // An explicit `Deny` about this one action refuses the request, whatever the allow
        // beside it says, and the refusal names that document as the rule that refused.
        let (status, body) = scheduler_as(
            &router,
            &denying_key,
            row.method,
            &row.uri,
            row.body.clone(),
        )
        .await;
        assert_eq!(
            status, ACCESS_DENIED_STATUS,
            "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for a denied caller, got \
             {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            ACCESS_DENIED,
            "case {case:?}: expected {ACCESS_DENIED}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            refusal(
                &denying_principal,
                &action,
                &row.resource,
                "with an explicit deny in an identity-based policy"
            ),
            "case {case:?}: expected AWS's wording for an explicit deny, got {body}"
        );

        // A caller whose documents say nothing about this service falls to IAM's default deny,
        // which names the action nothing granted.
        let (status, body) = scheduler_as(
            &router,
            &silent_key,
            row.method,
            &row.uri,
            row.body.clone(),
        )
        .await;
        assert_eq!(
            status, ACCESS_DENIED_STATUS,
            "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for an ungranted caller, got \
             {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            refusal(
                &silent_principal,
                &action,
                &row.resource,
                &format!("because no identity-based policy allows the {action} action")
            ),
            "case {case:?}: expected AWS's wording for IAM's default deny, got {body}"
        );

        // Neither refusal performed anything: the estate is exactly what the setup wrote, read
        // with the account's own credential.
        let (status, held) = scheduler_as(
            &router,
            ACCOUNT_KEY,
            "GET",
            &format!("/schedules/{SCHEDULE}?groupName={GROUP}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the refused calls to leave the schedule as it was, got \
             {status} with {held}"
        );
        let (status, listed) = scheduler_as(
            &router,
            ACCOUNT_KEY,
            "GET",
            &format!("/schedules?ScheduleGroup={GROUP}"),
            None,
        )
        .await;
        let count = listed["Schedules"].as_array().map(Vec::len).unwrap_or(0);
        assert!(
            status == 200 && count == 1,
            "case {case:?}: expected the group to hold the one schedule the setup wrote, got \
             {status} with {listed}"
        );

        // And the grant lifts both refusals: the same request, from a caller allowed every
        // action of this service, is served.
        let (status, body) =
            scheduler_as(&router, &allowed_key, row.method, &row.uri, row.body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 once the action is granted, got {status} with \
             {body}"
        );
    }
}
