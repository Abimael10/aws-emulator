// SPDX-License-Identifier: CC0-1.0

//! What a resource AWS APPCONFIG refused reports on its own `*_FAILED` stack event.
//!
//! RULE under test: **a CloudFormation resource provider words no diagnosis of its own for a
//! call the owning service refused — what the failed resource's `ResourceStatusReason` quotes is
//! the exception that provider's SDK raised: the service's own sentence followed by that SDK's
//! diagnostic of the call, naming the service as its own SDK names it, the HTTP status that
//! service answered and the id of the call, ahead of the `RequestToken` and `HandlerErrorCode`
//! suffix CloudFormation adds — and it holds whichever call of that service was refused, on the
//! CREATE of a resource and on the UPDATE of one the stack already has, leaving the resource
//! with the configuration that was in force.**
//!
//! Why it matters: tooling that attributes a failed deployment reads the stack event's reason and
//! splits on ` (Service: ` to recover which service refused the call and with what HTTP status —
//! a CDK diagnostic, a log-scraping runbook, any matcher anchored on AWS's reason format. A
//! quote that closes straight after the service's sentence carries neither, so a genuinely failed
//! resource is attributable to no service at all; and an update reported as failed while the
//! owning service holds a half-applied declaration makes every later read of the resource wrong.
//!
//! Two crates have to agree for the rule to hold (CloudFormation provisions the resource, AWS
//! AppConfig owns it and words the refusal), so the suite lives here rather than in either one.
//!
//! The rows differ along what the rule generalises over: WHICH stack operation ran into the
//! refusal (the create of a resource, and the update of one that already exists), WHICH member of
//! the owning call that service refused and at which published bound (`Name`,
//! `@length(min: 1, max: 64)`, and `Description`, `@length(min: 0, max: 1024)` —
//! `smithy_query(query="operation",operation="CreateApplication")`, `…="CreateEnvironment"` and
//! `…="UpdateApplication"` on that service's model), and WHICH type of that service the refused
//! resource is. The control is the same update with the member AT its published maximum: nothing
//! refuses it, the stack reports the update complete and AppConfig itself reports the declared
//! value.
//!
//! Every expected value is computed here from what the row declares and from that service's own
//! published bounds — never read back out of the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, form_encode, members_of, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The SDK's own name for AWS AppConfig, as the exception its SDK raises states it: that
/// model's `sdkId`, which is also its `awsService.cloudFormationName`
/// (`smithy_query(query="service_info")` on that service's model).
const SDK_SERVICE: &str = "AppConfig";

/// The status AppConfig answers for a call whose request it refuses: the `httpError` of its
/// modelled `BadRequestException`, which every create and update of these types declares.
const REFUSED_REQUEST_STATUS: u16 = 400;

/// The `HandlerErrorCode` class such a refusal falls in — the owning service refused what the
/// declaration asked for. One of the CloudFormation model's own `HandlerErrorCode` values.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The most characters the `Name` of an application, an environment or a deployment strategy
/// carries: `@length(min: 1, max: 64)` on AppConfig's `Name` shape.
const NAME_MAX_CHARACTERS: usize = 64;

/// The most characters the `Description` of any of these resources carries: `@length(min: 0,
/// max: 1024)` on that model's `Description` shape, which every one of these creates and
/// updates takes.
const DESCRIPTION_MAX_CHARACTERS: usize = 1024;

/// How long a wait for a stack to reach a terminal status, or for an event to be recorded, is
/// given.
///
/// Generous on purpose: what is waited FOR is the deployment settling, which is on the stack's
/// own clock, and every wait below ends as soon as the condition holds rather than after a fixed
/// span.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
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
        service_com_amazonaws_appconfig_amazonappconfig::EmulatedService::new(engine),
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

/// Call one AWS AppConfig operation over the `restJson1` wire its model declares, on the method
/// and URI that operation's own `@http` binding names — which is how a client reads back what
/// the stack reported.
async fn appconfig(router: &MultiServiceRouter, method: &str, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("appconfig.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("appconfig"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.body();
    let document = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).unwrap_or(Value::Null)
    };
    (status, document)
}

/// Submit `template` as `stack`, asserting the submission itself was accepted.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// Submit `template` as a change to `stack`, keeping the failed state where `keep_failed` asks
/// for it, and asserting the submission itself was accepted.
async fn update_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    keep_failed: bool,
) {
    let mut members = vec![("StackName", stack), ("TemplateBody", template)];
    if keep_failed {
        members.push(("DisableRollback", "true"));
    }
    let (status, body) = cloudformation(router, "UpdateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in.
///
/// The wait is on that CONDITION — the deployment reaching a terminal status, which is owed from
/// the instant the owning service has answered for every resource — rather than on a fixed span,
/// so it ends as soon as the stack has settled.
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

/// The `ResourceStatusReason` of the event `logical_id` reached `reported` in, or `None` where
/// the stack has recorded no such event.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together, so
/// a reason is never read off another resource's — or another transition's — event.
async fn recorded_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Option<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    members_of(&body).into_iter().find_map(|member| {
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
    })
}

/// The same reason, looked for again until the stack's own clock has recorded it or
/// [`SETTLE_DEADLINE`] passes.
async fn failure_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        if let Some(reason) = recorded_reason(router, case, stack, logical_id, reported).await {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} reported no {reported} reason within {SETTLE_DEADLINE:?}"
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

/// Hold one quoted exception to the shape an AWS AppConfig SDK raises: that service's own
/// sentence, then the diagnostic of the refused call — the SDK's name for the service, the HTTP
/// status it answered and the id of the call, and nothing past the call itself. Answers the
/// sentence.
fn asserted_sentence(case: &str, quoted: &str, status: u16) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
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
    let envelope = diagnostic.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the diagnostic to close, got {quoted:?}")
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, got \
         {envelope:?}"
    );
    assert_eq!(
        fields[0], SDK_SERVICE,
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

/// A value of exactly `characters` characters, built from a stem that says what it is for.
fn value_of(stem: &str, characters: usize) -> String {
    let mut value: String = stem.chars().take(characters).collect();
    while value.chars().count() < characters {
        value.push('x');
    }
    value
}

/// AWS's clause for a value longer than the member's maximum: the constraint sentence is its
/// fixed text, and the submitted value, the member as the model's own lowerCamel spelling names
/// it and that member's published maximum are its variable parts.
fn too_long(value: &str, member: &str, maximum: usize) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
         constraint: Member must have length less than or equal to {maximum}"
    )
}

/// A template declaring one application under `name`, described by `description`.
fn application_template(name: &str, description: &str) -> String {
    json!({
        "Resources": {
            "App": {
                "Type": "AWS::AppConfig::Application",
                "Properties": { "Name": name, "Description": description }
            }
        }
    })
    .to_string()
}

/// One row whose resource AppConfig refuses to CREATE.
struct RefusedCreate {
    case: &'static str,
    stack: &'static str,
    /// The logical id of the resource the row is about.
    logical_id: &'static str,
    template: String,
    /// The sentence AppConfig itself words for the refusal, which the quoted exception opens
    /// with.
    says: String,
}

/// One refused create per member, per bound and per type the rule covers.
fn refused_creates() -> Vec<RefusedCreate> {
    let over_long_name = value_of("appconfig-diag-name", NAME_MAX_CHARACTERS + 1);
    let over_long_description =
        value_of("appconfig-diag-description", DESCRIPTION_MAX_CHARACTERS + 1);
    let environment_description =
        value_of("appconfig-diag-environment", DESCRIPTION_MAX_CHARACTERS * 2);
    vec![
        RefusedCreate {
            case: "an application whose declared name is past the maximum",
            stack: "appconfig-diag-create-name",
            logical_id: "App",
            template: json!({
                "Resources": {
                    "App": {
                        "Type": "AWS::AppConfig::Application",
                        "Properties": { "Name": over_long_name }
                    }
                }
            })
            .to_string(),
            says: too_long(&over_long_name, "name", NAME_MAX_CHARACTERS),
        },
        RefusedCreate {
            case: "an application whose declared description is past the maximum",
            stack: "appconfig-diag-create-description",
            logical_id: "App",
            template: application_template(
                "appconfig-diag-create-description-app",
                &over_long_description,
            ),
            says: too_long(
                &over_long_description,
                "description",
                DESCRIPTION_MAX_CHARACTERS,
            ),
        },
        RefusedCreate {
            case: "an environment, under a created application, described far past the maximum",
            stack: "appconfig-diag-create-environment",
            logical_id: "Env",
            template: json!({
                "Resources": {
                    "App": {
                        "Type": "AWS::AppConfig::Application",
                        "Properties": { "Name": "appconfig-diag-environment-host" }
                    },
                    "Env": {
                        "Type": "AWS::AppConfig::Environment",
                        "Properties": {
                            "ApplicationId": { "Ref": "App" },
                            "Name": "appconfig-diag-environment",
                            "Description": environment_description
                        }
                    }
                }
            })
            .to_string(),
            says: too_long(
                &environment_description,
                "description",
                DESCRIPTION_MAX_CHARACTERS,
            ),
        },
    ]
}

/// The physical id the stack publishes for `logical_id` — the id AppConfig minted, which every
/// read of that resource is addressed by.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    physical_ids_of(&body)
        .get(logical_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a physical id for {logical_id:?}, got {body}")
        })
}

/// The `Description` AWS AppConfig itself reports for the application `id`, read through that
/// service's own wire.
async fn reported_description(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let (status, document) = appconfig(router, "GET", &format!("/applications/{id}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetApplication({id}), got {status} with \
         {document}"
    );
    document
        .get("Description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_create_appconfig_refuses_reports_the_appconfig_sdk_exception() {
    for row in refused_creates() {
        let case = row.case;
        let router = build_router();
        create_stack(&router, case, row.stack, &row.template).await;
        let settled = settled_status(&router, case, row.stack).await;
        assert!(
            settled.contains("ROLLBACK") || settled.ends_with("_FAILED"),
            "case {case:?}: expected the deployment to fail on the refused resource, got \
             {settled:?}"
        );

        let reason =
            failure_reason(&router, case, row.stack, row.logical_id, "CREATE_FAILED").await;
        let (quoted, class) = quoted_and_class(case, &reason);
        let sentence = asserted_sentence(case, &quoted, REFUSED_REQUEST_STATUS);
        assert_eq!(
            sentence, row.says,
            "case {case:?}: expected the sentence AppConfig words for the member it refused, \
             got {sentence:?} in {reason:?}"
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the class a refusal of the request falls in, got {class:?} \
             in {reason:?}"
        );
    }
}

/// The same rule on an UPDATE of a resource that already exists: the reason does not depend on
/// which call of the provider ran into the refusal, and the resource keeps the configuration
/// that was in force.
#[tokio::test]
async fn an_update_appconfig_refuses_reports_the_same_exception_and_changes_nothing() {
    let case = "an application re-described past the maximum";
    let stack = "appconfig-diag-update-refused";
    let application = "appconfig-diag-update-refused-app";
    let in_force = "the description in force before the update";
    let router = build_router();

    create_stack(
        &router,
        case,
        stack,
        &application_template(application, in_force),
    )
    .await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the application to be created, got {settled:?}"
    );
    let id = physical_id(&router, case, stack, "App").await;

    let over_long = value_of("appconfig-diag-update", DESCRIPTION_MAX_CHARACTERS + 1);
    update_stack(
        &router,
        case,
        stack,
        &application_template(application, &over_long),
        true,
    )
    .await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "UPDATE_FAILED",
        "case {case:?}: expected the refused update to leave the stack UPDATE_FAILED, got \
         {settled:?}"
    );

    let reason = failure_reason(&router, case, stack, "App", "UPDATE_FAILED").await;
    let (quoted, class) = quoted_and_class(case, &reason);
    let sentence = asserted_sentence(case, &quoted, REFUSED_REQUEST_STATUS);
    assert_eq!(
        sentence,
        too_long(&over_long, "description", DESCRIPTION_MAX_CHARACTERS),
        "case {case:?}: expected the sentence AppConfig words for the member it refused, got \
         {sentence:?} in {reason:?}"
    );
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the class a refusal of the request falls in, got {class:?} in \
         {reason:?}"
    );

    // Nothing of the refused declaration reached the resource: AppConfig still reports the
    // description that was in force.
    let described = reported_description(&router, case, &id).await;
    assert_eq!(
        described, in_force,
        "case {case:?}: expected AppConfig to still report {in_force:?} for {id}, got \
         {described:?}"
    );
}

/// The CONTROL: the same update with the member AT its published maximum. Nothing refuses it, so
/// the stack reports the update complete, the resource records no failure and AppConfig itself
/// reports the declared value — which is what makes each refusal above one character past a
/// bound rather than a deployment that fails whatever it declares.
#[tokio::test]
async fn an_update_at_the_published_bound_is_applied_and_reports_no_refusal() {
    let case = "an application re-described at exactly the maximum";
    let stack = "appconfig-diag-update-accepted";
    let application = "appconfig-diag-update-accepted-app";
    let router = build_router();

    create_stack(
        &router,
        case,
        stack,
        &application_template(application, "the description the create stated"),
    )
    .await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the application to be created, got {settled:?}"
    );
    let id = physical_id(&router, case, stack, "App").await;

    let at_the_bound = value_of("appconfig-diag-accepted", DESCRIPTION_MAX_CHARACTERS);
    update_stack(
        &router,
        case,
        stack,
        &application_template(application, &at_the_bound),
        false,
    )
    .await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {settled:?}"
    );
    let failed = recorded_reason(&router, case, stack, "App", "UPDATE_FAILED").await;
    assert!(
        failed.is_none(),
        "case {case:?}: expected no refused update, got {failed:?}"
    );
    let described = reported_description(&router, case, &id).await;
    assert_eq!(
        described, at_the_bound,
        "case {case:?}: expected AppConfig to report the declared description for {id}, got \
         {described:?}"
    );
}
