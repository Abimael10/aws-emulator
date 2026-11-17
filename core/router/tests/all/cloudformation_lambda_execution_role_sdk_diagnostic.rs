// SPDX-License-Identifier: CC0-1.0

//! What a `AWS::Lambda::Function` whose declared execution role AWS Lambda cannot be handed
//! reports on its own `*_FAILED` stack event.
//!
//! RULE under test: **a CloudFormation resource provider reports a refusal of the owning
//! service as the EXCEPTION that service's own SDK raises — the service's sentence followed by
//! that SDK's diagnostic of the refused call (the SDK's name for the service, the HTTP status
//! of the refusal, the id of the call and the attempts the SDK made at it) — and the whole of
//! that exception is what the resource's `ResourceStatusReason` quotes, ahead of the
//! `RequestToken` and `HandlerErrorCode` suffix CloudFormation adds. It holds whichever call of
//! that service was refused, whichever of its statuses the refusal carries, and whether the
//! provider learnt the refusal by making the call or refused ahead of making it.**
//!
//! Why it matters: tooling that attributes a failed deployment reads the stack event's reason —
//! splitting on ` (Service: ` to recover the handler's own sentence, or reading the service and
//! the status to decide whether the failure was a client error from the owning service. A
//! reason whose quote closes straight after the sentence carries neither, so the failure is
//! attributable to no service at all and the reason cannot be parsed by a client that expects
//! the SDK's exception there.
//!
//! Three crates have to agree for the rule to hold (CloudFormation provisions the function,
//! Lambda owns it, IAM answers for the execution role), so the suite lives here rather than in
//! any one crate.
//!
//! The rows differ along what the rule generalises over: WHAT makes the declared role one
//! Lambda cannot be handed (a trust policy admitting another service, one admitting Lambda for
//! another action, one that denies the assume outright, and a role ARN this account's IAM holds
//! nothing under), and — in the tests beside the table — whether the refusal arises on the
//! CREATE or on an UPDATE of a function that already exists, and whether the provider MADE the
//! refused call (the control, a Lambda refusal answered over the wire at a different status and
//! in a different class).
//!
//! Every expected value is computed here from what the row declares, from the account every ARN
//! this emulator mints carries and from the owning service's own refusal constant — never read
//! back out of the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, form_encode, members_of, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The SDK's own name for AWS Lambda, as the exception its SDK raises states it.
const LAMBDA_SERVICE: &str = "Lambda";

/// The HTTP status AWS Lambda refuses a `CreateFunction` naming a role it cannot assume with:
/// `InvalidParameterValueException`'s own `httpError`
/// (`smithy_query(query="operation",operation="CreateFunction")`).
const REFUSED_PARAMETER_STATUS: u16 = 400;

/// The status it answers for a function version it does not hold: `ResourceNotFoundException`'s
/// own `httpError` in the same model
/// (`smithy_query(query="operation",operation="CreateAlias")`).
const NOT_FOUND_STATUS: u16 = 404;

/// The `HandlerErrorCode` classes the refusals below fall in, both values of the
/// CloudFormation model's own `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

/// The sentence AWS Lambda words for a role it cannot be handed, taken from that service's own
/// refusal rather than restated here, so this suite cannot drift from the service it is about.
fn not_assumable() -> String {
    service_com_amazonaws_lambda_awsgirapiservice::state::ROLE_NOT_ASSUMABLE_MESSAGE.to_string()
}

/// How long a wait for a stack to reach a terminal status is given.
///
/// Generous on purpose: what is waited FOR is the deployment settling, which is on the stack's
/// own clock, and every wait below ends as soon as the condition holds rather than after a
/// fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Deploy `template` as `stack`, asserting the submission itself was accepted.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in.
///
/// The wait is on that CONDITION — the deployment reaching a terminal status, which is owed
/// from the instant the owning services have answered for every resource — rather than on a
/// fixed span, so it ends as soon as the stack has settled.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return reached;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceStatusReason` of the event `logical_id` reached `reported` in, looked for
/// again until the stack's own clock has recorded it or [`SETTLE_DEADLINE`] passes.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together,
/// so a reason is never read off another resource's — or another transition's — event.
async fn failure_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
             {body}"
        );
        let found = members_of(&body).into_iter().find_map(|member| {
            let named = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id);
            let failed = elements(member, "ResourceStatus")
                .into_iter()
                .next()
                .is_some_and(|status| status == reported);
            (named && failed)
                .then(|| elements(member, "ResourceStatusReason").into_iter().next())
                .flatten()
                .filter(|reason| !reason.is_empty())
        });
        if let Some(reason) = found {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} reported no {reported} reason within \
             {SETTLE_DEADLINE:?}: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The provider exception a `ResourceStatusReason` quotes, and the `HandlerErrorCode` class it
/// reports beside it.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one quoted exception to the shape an AWS Lambda SDK raises: the service's own sentence,
/// then the diagnostic of the refused call — the SDK's name for the service, the HTTP status it
/// answered, the id of the call, and the attempts the SDK made at it. Answers the sentence.
fn asserted_sentence(case: &str, quoted: &str, status: u16) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    const ATTEMPTS: &str = ") (SDK Attempt Count: ";
    let (sentence, diagnostic) = quoted.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, after the \
             service's own sentence, got {quoted:?}"
        )
    });
    assert!(
        !sentence.trim().is_empty(),
        "case {case:?}: expected the service's own sentence ahead of the diagnostic, got \
         {quoted:?}"
    );
    let (envelope, attempts) = diagnostic.split_once(ATTEMPTS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the attempt count this service's SDK words after the \
             envelope, got {quoted:?}"
        )
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, got \
         {envelope:?}"
    );
    assert_eq!(
        fields[0], LAMBDA_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {status}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    let request_id = fields[2]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[2]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
    );
    let counted = attempts
        .strip_suffix(')')
        .and_then(|counted| counted.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a whole attempt count, got {attempts:?}")
        });
    assert!(
        counted >= 1,
        "case {case:?}: expected at least the one attempt the SDK was asked for, got {counted}"
    );
    sentence.to_string()
}

/// Whether `value` has the 8-4-4-4-12 lowercase-hexadecimal form an AWS request id carries.
fn is_a_request_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .chars()
                .all(|held| held.is_ascii_hexdigit() && !held.is_ascii_uppercase())
        })
}

/// An `AWS::IAM::Role` declaration carrying `document` as its trust policy.
fn role(document: Value) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": { "AssumeRolePolicyDocument": document }
    })
}

/// A trust policy with ONE statement, as the three parts a row varies.
fn trust(effect: &str, principal: &str, action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principal },
            "Action": action
        }]
    })
}

/// The trust policy a function's execution role has to carry: Lambda allowed to assume it.
fn assumable_by_lambda() -> Value {
    trust(
        "Allow",
        service_com_amazonaws_lambda_awsgirapiservice::state::EXECUTION_ROLE_PRINCIPAL,
        "sts:AssumeRole",
    )
}

/// An `AWS::Lambda::Function` declaration whose `Role` is `role_reference`.
fn function(name: &str, role_reference: Value) -> Value {
    json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": name,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Role": role_reference,
            "Code": { "ZipFile": "def handler(event, context):\n    return 1\n" }
        }
    })
}

/// The logical id every row's function is declared under.
const FUNCTION_ID: &str = "Fn";

/// One row of the table: what makes the declared execution role one Lambda cannot be handed.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    /// The resources the row declares beside the function, and the `Role` the function names.
    resources: fn() -> (Value, Value),
}

/// A role whose trust policy admits ANOTHER service, which is the recorded shape of the
/// failure: the declared role exists and names a principal other than Lambda's.
fn another_service() -> (Value, Value) {
    (
        role(trust("Allow", "ec2.amazonaws.com", "sts:AssumeRole")),
        json!({ "Fn::GetAtt": ["ExecRole", "Arn"] }),
    )
}

/// A role whose trust policy names Lambda for an action that is not the assume.
fn another_action() -> (Value, Value) {
    (
        role(trust(
            "Allow",
            service_com_amazonaws_lambda_awsgirapiservice::state::EXECUTION_ROLE_PRINCIPAL,
            "sts:TagSession",
        )),
        json!({ "Fn::GetAtt": ["ExecRole", "Arn"] }),
    )
}

/// A role whose trust policy DENIES Lambda the assume it names.
fn denied_assume() -> (Value, Value) {
    (
        role(trust(
            "Deny",
            service_com_amazonaws_lambda_awsgirapiservice::state::EXECUTION_ROLE_PRINCIPAL,
            "sts:AssumeRole",
        )),
        json!({ "Fn::GetAtt": ["ExecRole", "Arn"] }),
    )
}

/// No role at all: a well-formed role ARN of this account that IAM holds nothing under.
fn absent_role() -> (Value, Value) {
    (
        Value::Null,
        json!(format!("arn:aws:iam::{ACCOUNT}:role/cfn-lambda-no-such-exec-role")),
    )
}

const ROWS: &[Row] = &[
    Row {
        case: "a role whose trust policy admits another service",
        stack: "lambda-role-diag-other-service",
        function: "role-diag-other-service",
        resources: another_service,
    },
    Row {
        case: "a role whose trust policy names Lambda for another action",
        stack: "lambda-role-diag-other-action",
        function: "role-diag-other-action",
        resources: another_action,
    },
    Row {
        case: "a role whose trust policy denies Lambda the assume",
        stack: "lambda-role-diag-denied",
        function: "role-diag-denied",
        resources: denied_assume,
    },
    Row {
        case: "a role ARN this account's IAM holds nothing under",
        stack: "lambda-role-diag-absent",
        function: "role-diag-absent",
        resources: absent_role,
    },
];

/// Every row: the function fails, and its reason quotes AWS Lambda's own sentence followed by
/// that SDK's diagnostic of the refused call, with CloudFormation's own token and class after
/// the closing quote.
#[tokio::test]
async fn a_function_whose_role_lambda_cannot_be_handed_reports_the_lambda_sdk_exception() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let (declared_role, role_reference) = (row.resources)();
        let mut resources = json!({ "Fn": function(row.function, role_reference) });
        if !declared_role.is_null() {
            resources["ExecRole"] = declared_role;
        }
        let template = json!({ "Resources": resources }).to_string();

        create_stack(&router, case, row.stack, &template).await;
        let settled = settled_status(&router, case, row.stack).await;
        assert!(
            settled.contains("ROLLBACK") || settled.ends_with("_FAILED"),
            "case {case:?}: expected the deployment to fail on the function's role, got {settled:?}"
        );

        let reason = failure_reason(&router, case, row.stack, FUNCTION_ID, "CREATE_FAILED").await;
        let (quoted, class) = quoted_and_class(case, &reason);
        let sentence = asserted_sentence(case, &quoted, REFUSED_PARAMETER_STATUS);
        assert_eq!(
            sentence,
            not_assumable(),
            "case {case:?}: expected the sentence AWS Lambda words for a role it cannot be \
             handed, got {sentence:?} in {reason:?}"
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the class a refusal of the request falls in, got {class:?} \
             in {reason:?}"
        );
        // The regression this suite exists for: a quote that closes straight after the
        // sentence carries no service attribution and no status at all.
        assert!(
            !reason.contains(&format!("{}\" (RequestToken: ", not_assumable())),
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, got \
             {reason:?}"
        );
    }
}

/// The same rule on an UPDATE: a stack whose function is re-pointed at a role Lambda cannot be
/// handed fails that resource with the same exception, so the reason does not depend on which
/// call of the provider made it.
#[tokio::test]
async fn an_update_that_repoints_the_role_reports_the_same_lambda_sdk_exception() {
    let case = "an update re-pointing the role at one that admits another service";
    let stack = "lambda-role-diag-update";
    let router = build_router();

    let resources = |role_reference: Value| {
        json!({
            "Good": role(assumable_by_lambda()),
            "Bad": role(trust("Allow", "ec2.amazonaws.com", "sts:AssumeRole")),
            "Fn": function("role-diag-update", role_reference),
        })
    };
    let deployed = json!({ "Resources": resources(json!({ "Fn::GetAtt": ["Good", "Arn"] })) })
        .to_string();
    create_stack(&router, case, stack, &deployed).await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the function to be created with a role Lambda can assume, got \
         {settled:?}"
    );

    let updated = json!({ "Resources": resources(json!({ "Fn::GetAtt": ["Bad", "Arn"] })) })
        .to_string();
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &updated),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );

    let settled = settled_status(&router, case, stack).await;
    assert!(
        settled.contains("ROLLBACK") || settled.ends_with("_FAILED"),
        "case {case:?}: expected the update to fail on the function's new role, got {settled:?}"
    );
    let reason = failure_reason(&router, case, stack, FUNCTION_ID, "UPDATE_FAILED").await;
    let (quoted, class) = quoted_and_class(case, &reason);
    let sentence = asserted_sentence(case, &quoted, REFUSED_PARAMETER_STATUS);
    assert_eq!(
        sentence,
        not_assumable(),
        "case {case:?}: expected the sentence AWS Lambda words for a role it cannot be handed, \
         got {sentence:?} in {reason:?}"
    );
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the class a refusal of the request falls in, got {class:?} in \
         {reason:?}"
    );
}

/// The CONTROL: a refusal the provider learnt by MAKING the call — an alias naming a version
/// the function does not have, which AWS Lambda answers at another status and in another class
/// — reports the exception in exactly the same shape, so the form is the SDK's and not one
/// written for the role check.
#[tokio::test]
async fn a_refusal_lambda_answered_over_the_wire_carries_the_same_diagnostic() {
    let case = "an alias naming a version the function does not have";
    let stack = "lambda-role-diag-control";
    let router = build_router();

    // A version number no function of this stack ever published.
    const MISSING_VERSION: &str = "593";
    let template = json!({
        "Resources": {
            "ExecRole": role(assumable_by_lambda()),
            "Fn": function("role-diag-control", json!({ "Fn::GetAtt": ["ExecRole", "Arn"] })),
            "FnAlias": {
                "Type": "AWS::Lambda::Alias",
                "Properties": {
                    "FunctionName": { "Ref": FUNCTION_ID },
                    "Name": "live",
                    "FunctionVersion": MISSING_VERSION
                }
            }
        }
    })
    .to_string();

    create_stack(&router, case, stack, &template).await;
    let settled = settled_status(&router, case, stack).await;
    assert!(
        settled.contains("ROLLBACK") || settled.ends_with("_FAILED"),
        "case {case:?}: expected the deployment to fail on the alias, got {settled:?}"
    );

    let reason = failure_reason(&router, case, stack, "FnAlias", "CREATE_FAILED").await;
    let (quoted, class) = quoted_and_class(case, &reason);
    let sentence = asserted_sentence(case, &quoted, NOT_FOUND_STATUS);
    assert!(
        sentence.contains(MISSING_VERSION),
        "case {case:?}: expected the service's own sentence to name the version it does not \
         hold, got {sentence:?} in {reason:?}"
    );
    assert_eq!(
        class, NOT_FOUND,
        "case {case:?}: expected the class a refusal about an absent resource falls in, got \
         {class:?} in {reason:?}"
    );
}
