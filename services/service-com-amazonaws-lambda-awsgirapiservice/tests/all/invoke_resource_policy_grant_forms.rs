// SPDX-License-Identifier: CC0-1.0

//! WHICH invocations one statement `AddPermission` wrote authorizes — the complement of
//! `tests/all/invoke_resource_policy_denial.rs` (what a function's own policy DENIES) and of
//! `tests/all/invoke_identity_authorization.rs` (what the CALLER's own policies say).
//!
//! RULE: a statement `AddPermission` added to a function's resource-based policy authorizes, on
//! its own and from the next call on, exactly the invocations whose caller is one of the
//! spellings its `Principal` element names — for an assumed-role session both the session ARN
//! STS reports and the ARN of the IAM role it is a session of, because a statement names a role
//! by that ARN — whose action its `Action` element covers, and whose addressed qualifier is the
//! one the grant was attached to; every other invocation by a caller holding no identity-based
//! grant falls to IAM's default deny and is refused HTTP 403 `AccessDeniedException` carrying
//! the sentence AWS words an authorization failure with. A statement that names the ACCOUNT
//! delegates to that account's own identity-based policies rather than granting.
//!
//! Every principal here is made with IAM's and STS's OWN operations over the same engine
//! (`CreateUser` + `CreateAccessKey`, `CreateRole` + `AssumeRole`) and holds no identity-based
//! policy at all, so the only thing that can admit its invocation is the statement the row
//! wrote — and each row takes its statement back out again, so the policy a row is decided on
//! is its own. A served row is asserted on the handler's ANSWER, which echoes the payload the
//! invocation carried, so a row that was admitted but never ran fails here.
//!
//! The rows differ along what the rule generalises over: the FORM the grant names its principal
//! in (every one `AddPermission$Principal` documents — "the Amazon Web Services service, Amazon
//! Web Services account, IAM user, or IAM role that invokes the function" — plus the wildcard
//! the policy language adds), WHO invokes (an IAM user, an assumed-role session), the ACTION the
//! grant names (this action, both wildcards its `@pattern` admits, another action of this
//! service, the deprecated asynchronous one) and the QUALIFIER the grant is attached to against
//! the one the invocation addresses.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{element, form_encode};
use crate::invoke_identity_authorization::{no_allow, not_authorized};
// Every test below RUNS a handler, so each one takes this target's shared invocation stream:
// see the helper's own documentation for why a suite that runs handlers holds it.
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The Region every call below is signed for, so the ARNs the decisions are made about are the
/// ones this Region's functions carry.
const REGION: &str = "us-east-1";

/// Where `CreateFunction`, `AddPermission`, `RemovePermission` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// An access-key id neither STS nor IAM holds a record for, so it resolves to the account's own
/// identity — the credential the fixtures below are made with.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The module the handler of the package below lives in.
const HANDLER: &str = "lambda_function.lambda_handler";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The handler every function here runs: its answer is computed from the event it was handed,
/// so a served row reports that the deployment package really ran with the invocation's payload.
const SOURCE: &str = "def lambda_handler(event, context):\n    return {'echo': event}\n";

/// The alias one function carries, which is a qualifier a grant can be attached to.
const ALIAS: &str = "live";

/// The action the synchronous entry point is authorized under. Source: the action AWS publishes
/// for invoking a function ("Actions defined by AWS Lambda", IAM Service Authorization
/// Reference), which is the one `AddPermission` grants.
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// How often to look while waiting for a created function to report itself runnable, and how
/// many looks to take. The transition is owed by the CLOCK, so nothing here sleeps a span on the
/// emulator's behalf; the deadline is many times the span the create documents, so a loaded host
/// cannot decide the outcome, and short enough that a fixture that never settles is reported as
/// this suite's own failure with the last state it read.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 600;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    error_code: Option<String>,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn message(&self) -> String {
        let document = self.json();
        document
            .get("Message")
            .or_else(|| document.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal must
/// blame it by — what `sts:GetCallerIdentity` reports for that key.
struct Principal {
    access_key_id: String,
    arn: String,
}

/// The emulator this suite drives: one shared engine carrying Lambda beside the IAM and STS
/// services the principals are made with, as the production router mounts them.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// One call to Lambda, with `access_key_id` in the SigV4 credential scope.
    async fn call(&self, access_key_id: &str, method: &str, uri: &str, body: &[u8]) -> Answer {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/lambda/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(bytes::Bytes::from(body.to_vec()))
            .expect("the test request must build");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        Answer {
            status: parts.status.as_u16(),
            error_code: parts
                .headers
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            body,
        }
    }

    /// One awsQuery call to IAM or STS, made as the account's own credential.
    async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.amazonaws.com/"))
            .header("host", format!("{signing_name}.amazonaws.com"))
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
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "the fixture {action} must succeed, got {status} with {text}"
        );
        text
    }

    /// Create an IAM user holding NO policy of its own and mint a long-term access key for it.
    async fn user(&self, user_name: &str) -> Principal {
        let created = self
            .query(
                "iam",
                "2010-05-08",
                "CreateUser",
                &[("UserName", user_name.to_string())],
            )
            .await;
        let arn = element(&created, "Arn");
        let minted = self
            .query(
                "iam",
                "2010-05-08",
                "CreateAccessKey",
                &[("UserName", user_name.to_string())],
            )
            .await;
        Principal {
            access_key_id: element(&minted, "AccessKeyId"),
            arn,
        }
    }

    /// Create an IAM role holding NO policy of its own, and answer the ARN the account holds it
    /// under.
    async fn role(&self, role_name: &str) -> String {
        let account = ::engine::emulator_account_id();
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let created = self
            .query(
                "iam",
                "2010-05-08",
                "CreateRole",
                &[
                    ("RoleName", role_name.to_string()),
                    ("AssumeRolePolicyDocument", trust),
                ],
            )
            .await;
        element(&created, "Arn")
    }

    /// Assume the role `role_arn` through STS and answer the session's key with the session ARN
    /// STS reports for it.
    async fn session(&self, role_arn: &str) -> Principal {
        let assumed = self
            .query(
                "sts",
                "2011-06-15",
                "AssumeRole",
                &[
                    ("RoleArn", role_arn.to_string()),
                    ("RoleSessionName", "probe".to_string()),
                ],
            )
            .await;
        Principal {
            access_key_id: element(&assumed, "AccessKeyId"),
            arn: element(&assumed, "Arn"),
        }
    }

    /// Create the function `name` running [`SOURCE`], publish version 1, point the alias
    /// [`ALIAS`] at it, and wait until the service reports both the function and the alias
    /// runnable.
    async fn function(&self, name: &str) {
        let package = crate::synchronous_invoke_run::package(SOURCE);
        let created = self
            .call(
                ACCOUNT_KEY,
                "POST",
                FUNCTIONS_URI,
                &serde_json::to_vec(&json!({
                    "FunctionName": name,
                    "Role": ROLE,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Publish": true,
                    "Timeout": 120,
                    "Code": {
                        "ZipFile": base64::engine::general_purpose::STANDARD.encode(package),
                    },
                }))
                .expect("the create body serializes"),
            )
            .await;
        assert_eq!(
            created.status,
            201,
            "the fixture CreateFunction {name:?} must succeed, got {} {}",
            created.status,
            created.text()
        );
        let alias = self
            .call(
                ACCOUNT_KEY,
                "POST",
                &format!("{FUNCTIONS_URI}/{name}/aliases"),
                &serde_json::to_vec(&json!({ "Name": ALIAS, "FunctionVersion": "1" }))
                    .expect("the alias body serializes"),
            )
            .await;
        assert_eq!(
            alias.status,
            201,
            "the fixture CreateAlias on {name:?} must succeed, got {} {}",
            alias.status,
            alias.text()
        );
        for reference in [name.to_string(), format!("{name}:{ALIAS}")] {
            self.await_active(&reference).await;
        }
    }

    /// Look until the service reports `reference` `Active`, which is the state it takes
    /// invocations in. The span is the service's own, so this waits for the CONDITION.
    async fn await_active(&self, reference: &str) {
        let mut last = Value::Null;
        for _ in 0..LOOKS {
            last = self
                .call(
                    ACCOUNT_KEY,
                    "GET",
                    &format!("{FUNCTIONS_URI}/{reference}/configuration"),
                    b"",
                )
                .await
                .json();
            if last.get("State").and_then(Value::as_str) == Some("Active") {
                return;
            }
            tokio::time::sleep(LOOK).await;
        }
        panic!("the fixture function {reference:?} never reported itself Active, last read {last}");
    }

    /// Add one statement to the function's resource-based policy through `AddPermission`, which
    /// is how a client states a grant, optionally attached to `qualifier`.
    async fn add_permission(
        &self,
        case: &str,
        name: &str,
        statement_id: &str,
        principal: &str,
        action: &str,
        qualifier: Option<&str>,
    ) {
        let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
        let added = self
            .call(
                ACCOUNT_KEY,
                "POST",
                &format!("{FUNCTIONS_URI}/{name}/policy{query}"),
                &serde_json::to_vec(&json!({
                    "StatementId": statement_id,
                    "Action": action,
                    "Principal": principal,
                }))
                .expect("the permission body serializes"),
            )
            .await;
        assert_eq!(
            added.status,
            201,
            "case {case:?}: AddPermission({principal}, {action}) must answer 201, got {} {}",
            added.status,
            added.text()
        );
    }

    /// Take that statement back out again, so the next row is decided on its own grant alone.
    async fn remove_permission(
        &self,
        case: &str,
        name: &str,
        statement_id: &str,
        qualifier: Option<&str>,
    ) {
        let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
        let removed = self
            .call(
                ACCOUNT_KEY,
                "DELETE",
                &format!("{FUNCTIONS_URI}/{name}/policy/{statement_id}{query}"),
                b"",
            )
            .await;
        assert_eq!(
            removed.status,
            204,
            "case {case:?}: RemovePermission({statement_id}) must answer 204, got {} {}",
            removed.status,
            removed.text()
        );
    }

    /// `Invoke` the function `reference` names, as `access_key_id`, carrying `payload`.
    async fn invoke(&self, access_key_id: &str, reference: &str, payload: &Value) -> Answer {
        self.call(
            access_key_id,
            "POST",
            &format!("{FUNCTIONS_URI}/{reference}/invocations"),
            &serde_json::to_vec(payload).expect("the payload serializes"),
        )
        .await
    }
}

/// The ARN of the function `name` of this account, in the Region every call is signed for.
fn function_arn(name: &str) -> String {
    format!(
        "arn:aws:lambda:{REGION}:{}:function:{name}",
        ::engine::emulator_account_id()
    )
}

/// The payload one row's invocation carries, which the handler echoes back.
fn payload(marker: &str) -> Value {
    json!({ "marker": marker })
}

/// What the handler answers for [`payload`].
fn echoed(marker: &str) -> Value {
    json!({ "echo": { "marker": marker } })
}

/// Assert that `answer` is the RUN: HTTP 200 carrying the handler's own answer for the payload
/// the invocation was made with.
fn assert_invoked(case: &str, answer: &Answer, marker: &str) {
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected the invocation to be served, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.json(),
        echoed(marker),
        "case {case:?}: expected the handler's echo {}, got {}",
        echoed(marker),
        answer.text()
    );
}

/// Assert that `answer` is the authorization refusal: HTTP 403, the `AccessDeniedException` code
/// and the sentence AWS words it with.
fn assert_denied(case: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        answer.status,
        403,
        "case {case:?}: expected the invocation to be refused with 403, got {} {}",
        answer.status,
        answer.text()
    );
    // The wire code carries the shape's namespace on some protocols, so the comparison is
    // against the shape NAME alone — another 403 channel of this service must not pass as this
    // one.
    let code = answer.error_code.clone().unwrap_or_default();
    let named = code.rsplit('#').next().unwrap_or(&code).to_string();
    assert_eq!(
        named, "AccessDeniedException",
        "case {case:?}: expected AccessDeniedException, got {code:?}: {}",
        answer.text()
    );
    let message = answer.message();
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
}

/// Who a row invokes as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    /// An IAM user of the account, with a long-term key IAM minted for it.
    User,
    /// A session of an IAM role of the account, with the key STS minted for it.
    Session,
}

/// The FORM a row's statement names its principal in. The first four are the forms
/// `AddPermission$Principal` documents ("The Amazon Web Services service, Amazon Web Services
/// account, IAM user, or IAM role that invokes the function", a service being "a domain-style
/// identifier that the service defines"); the wildcard is the one the policy language adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Names {
    /// The caller's own ARN — the IAM user's, or the session's as STS reports it.
    Caller,
    /// The ARN of the IAM role the caller's session is a session OF, which is the spelling a
    /// statement names a role by.
    CallersRole,
    /// Every principal.
    Everyone,
    /// Another IAM user of the account.
    OtherUser,
    /// Another IAM role of the account.
    OtherRole,
    /// The account, by its twelve-digit id.
    Account,
    /// A service, by the domain-style identifier it defines.
    Service,
}

/// One row of the principal-form table.
struct PrincipalRow {
    case: &'static str,
    who: Who,
    names: Names,
    /// Whether the statement authorizes the row's caller.
    allowed: bool,
}

/// RULE: a statement `AddPermission` wrote authorizes exactly the callers its `Principal` element
/// names — an assumed-role session by its own ARN and by the ARN of the role it is a session of —
/// and nobody else, so a caller no statement names falls to IAM's default deny.
#[tokio::test]
async fn a_statement_authorizes_exactly_the_principals_it_names() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();
    let function = "granted";
    emulator.function(function).await;
    let arn = function_arn(function);

    let user = emulator.user("the-caller").await;
    let other_user = emulator.user("the-bystander").await;
    let role_arn = emulator.role("the-callers-role").await;
    let other_role_arn = emulator.role("another-role").await;
    let session = emulator.session(&role_arn).await;

    let rows: Vec<PrincipalRow> = vec![
        PrincipalRow {
            case: "a user named by its own ARN",
            who: Who::User,
            names: Names::Caller,
            allowed: true,
        },
        PrincipalRow {
            case: "a user under a grant to every principal",
            who: Who::User,
            names: Names::Everyone,
            allowed: true,
        },
        PrincipalRow {
            case: "a user under a grant naming another user",
            who: Who::User,
            names: Names::OtherUser,
            allowed: false,
        },
        PrincipalRow {
            case: "a user under a grant naming the account, which delegates",
            who: Who::User,
            names: Names::Account,
            allowed: false,
        },
        PrincipalRow {
            case: "a user under a grant naming a service",
            who: Who::User,
            names: Names::Service,
            allowed: false,
        },
        PrincipalRow {
            case: "a session named by the session ARN",
            who: Who::Session,
            names: Names::Caller,
            allowed: true,
        },
        PrincipalRow {
            case: "a session named by the ARN of the role it is a session of",
            who: Who::Session,
            names: Names::CallersRole,
            allowed: true,
        },
        PrincipalRow {
            case: "a session under a grant naming another role",
            who: Who::Session,
            names: Names::OtherRole,
            allowed: false,
        },
        PrincipalRow {
            case: "a session under a grant to every principal",
            who: Who::Session,
            names: Names::Everyone,
            allowed: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let caller = match row.who {
            Who::User => &user,
            Who::Session => &session,
        };
        let granted_to = match row.names {
            Names::Caller => caller.arn.clone(),
            Names::CallersRole => role_arn.clone(),
            Names::Everyone => "*".to_string(),
            Names::OtherUser => other_user.arn.clone(),
            Names::OtherRole => other_role_arn.clone(),
            Names::Account => ::engine::emulator_account_id().to_string(),
            Names::Service => "s3.amazonaws.com".to_string(),
        };
        let statement_id = format!("principal-form-{index}");
        let marker = format!("principal-form-{index}");
        emulator
            .add_permission(
                row.case,
                function,
                &statement_id,
                &granted_to,
                INVOKE_FUNCTION,
                None,
            )
            .await;

        let answer = emulator
            .invoke(&caller.access_key_id, function, &payload(&marker))
            .await;
        if row.allowed {
            assert_invoked(row.case, &answer, &marker);
        } else {
            assert_denied(
                row.case,
                &answer,
                &not_authorized(
                    &caller.arn,
                    INVOKE_FUNCTION,
                    &arn,
                    &no_allow(INVOKE_FUNCTION),
                ),
            );
        }

        // The statement goes back out, so the next row is decided on its own grant alone — and
        // the caller the row admitted is refused again straight after.
        emulator
            .remove_permission(row.case, function, &statement_id, None)
            .await;
        if row.allowed {
            let withdrawn = emulator
                .invoke(&caller.access_key_id, function, &payload(&marker))
                .await;
            assert_denied(
                row.case,
                &withdrawn,
                &not_authorized(
                    &caller.arn,
                    INVOKE_FUNCTION,
                    &arn,
                    &no_allow(INVOKE_FUNCTION),
                ),
            );
        }
    }
}

/// RULE: the statement authorizes the invocation only when its `Action` element covers the action
/// the entry point is performed under, so a grant of another action of this service — or of the
/// deprecated asynchronous one — admits nothing.
#[tokio::test]
async fn a_statement_authorizes_only_the_action_it_names() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();
    let function = "action-scoped";
    emulator.function(function).await;
    let arn = function_arn(function);
    let caller = emulator.user("action-probe").await;

    // (case, the Action the grant names, whether it authorizes the invocation). The values are
    // the ones `AddPermission$Action` admits (pattern `^(lambda:[*]|lambda:[a-zA-Z]+|[*])$`,
    // documented "for example, lambda:InvokeFunction or lambda:GetFunction").
    let rows: &[(&str, &str, bool)] = &[
        ("the action itself", INVOKE_FUNCTION, true),
        ("every action of this service", "lambda:*", true),
        ("every action of every service", "*", true),
        ("another action of this service", "lambda:GetFunction", false),
        (
            "the deprecated asynchronous action",
            "lambda:InvokeAsync",
            false,
        ),
    ];

    for (index, (case, action, allowed)) in rows.iter().enumerate() {
        let statement_id = format!("action-form-{index}");
        let marker = format!("action-form-{index}");
        emulator
            .add_permission(case, function, &statement_id, &caller.arn, action, None)
            .await;

        let answer = emulator
            .invoke(&caller.access_key_id, function, &payload(&marker))
            .await;
        if *allowed {
            assert_invoked(case, &answer, &marker);
        } else {
            assert_denied(
                case,
                &answer,
                &not_authorized(
                    &caller.arn,
                    INVOKE_FUNCTION,
                    &arn,
                    &no_allow(INVOKE_FUNCTION),
                ),
            );
        }

        emulator
            .remove_permission(case, function, &statement_id, None)
            .await;
    }
}

/// RULE: the grant is attached to the ARN the permission NAMES — "if you use a qualifier, the
/// invoker must use the full Amazon Resource Name (ARN) of that version or alias to invoke the
/// function" — so a function-level statement authorizes the unqualified invocation alone and one
/// attached to a qualifier authorizes that qualifier's alone.
#[tokio::test]
async fn a_grant_authorizes_the_qualifier_it_is_attached_to() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();
    let function = "qualifier-scoped";
    emulator.function(function).await;
    let unqualified = function_arn(function);
    let qualified = format!("{unqualified}:{ALIAS}");
    let caller = emulator.user("qualifier-probe").await;
    let refusal = |resource: &str| {
        not_authorized(
            &caller.arn,
            INVOKE_FUNCTION,
            resource,
            &no_allow(INVOKE_FUNCTION),
        )
    };

    // The function-level grant: the unqualified invocation runs, the alias's is refused.
    let case = "a grant attached to the function";
    emulator
        .add_permission(
            case,
            function,
            "function-level",
            &caller.arn,
            INVOKE_FUNCTION,
            None,
        )
        .await;
    assert_invoked(
        case,
        &emulator
            .invoke(&caller.access_key_id, function, &payload("function-level"))
            .await,
        "function-level",
    );
    assert_denied(
        case,
        &emulator
            .invoke(
                &caller.access_key_id,
                &format!("{function}:{ALIAS}"),
                &payload("function-level"),
            )
            .await,
        &refusal(&qualified),
    );
    emulator
        .remove_permission(case, function, "function-level", None)
        .await;

    // The grant attached to the alias: the other way round.
    let case = "a grant attached to the alias";
    emulator
        .add_permission(
            case,
            function,
            "alias-level",
            &caller.arn,
            INVOKE_FUNCTION,
            Some(ALIAS),
        )
        .await;
    assert_invoked(
        case,
        &emulator
            .invoke(
                &caller.access_key_id,
                &format!("{function}:{ALIAS}"),
                &payload("alias-level"),
            )
            .await,
        "alias-level",
    );
    assert_denied(
        case,
        &emulator
            .invoke(&caller.access_key_id, function, &payload("alias-level"))
            .await,
        &refusal(&unqualified),
    );
    emulator
        .remove_permission(case, function, "alias-level", Some(ALIAS))
        .await;
}
