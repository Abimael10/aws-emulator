// SPDX-License-Identifier: CC0-1.0

//! Which of a running handler's own Amazon SQS and Secrets Manager calls the function's
//! EXECUTION ROLE lets through: Lambda, Amazon SQS, Secrets Manager, IAM and the token
//! service on ONE engine, a `python3.x` function that does `import boto3` and sends a message
//! and reads a secret, with every queue, secret and policy written through those services'
//! own operations.
//!
//! RULE: a call a handler's own SDK makes is authorized like any other call of the service it
//! reaches — the request's action and the ARN of the resource it names are matched against the
//! identity-based policies in force on the execution role at the instant of the call — so a
//! call no statement allows is REFUSED with that service's own authorization code and IAM's
//! implicit-deny sentence ("User: <principal> is not authorized to perform: <action> on
//! resource: <resource> because no identity-based policy allows the <action> action"), and the
//! grant reaches only the resources its `Resource` element covers: a statement scoped to one
//! queue's ARN leaves a send to ANY other queue refused. The principal the sentence names is
//! the assumed-role session a function's calls are made under, and a refused call changes
//! nothing in the service it was refused by.
//!
//! What grounds it: "The execution role grants the function permission to use Amazon Web
//! Services services" (`FunctionConfiguration$Role`) — a grant, so a call outside it is not
//! permitted. The action is `sqs:SendMessage` for a send (`SendMessage`, Amazon SQS's own
//! model) and `secretsmanager:GetSecretValue` for a read (`GetSecretValue`, Secrets Manager's
//! own model); neither service models an authorization error of its own, so each answers the
//! AWS-common one it publishes — `AccessDenied` at HTTP 403 for Amazon SQS, and
//! `AccessDeniedException` for Secrets Manager, whose `awsJson1_1` envelope carries every
//! client error at HTTP 400.
//!
//! The rows differ along what the rule generalises over: WHICH grant the role carries (one
//! naming neither action, each action scoped to a single resource ARN, and each action over
//! every resource of its service) and WHICH resource the call names under it (the resource the
//! scoped statement names, and another resource of the same service that no statement names).
//! Both services are driven under every grant, so what is pinned is the rule and not one
//! service's gate, and the two forms a Secrets Manager read may name its secret by (the name,
//! the complete ARN) both appear, because the refusal states the identifier the caller
//! supplied. The grant is rewritten BETWEEN invocations, so each row also pins that the
//! document in force when the handler calls is the one that decides. Every row asserts both
//! halves: what the SDK handed the handler, and what the QUEUE holds afterwards, read back
//! where a client reads it.
//!
//! What the neighbours pin and this suite does not repeat: `handler_secret_relayed_to_queue`
//! drives the relay itself and the refusals each service answers for a resource that is not
//! there, `handler_query_compatible_error_code` drives the code vocabulary a refusal reaches
//! the handler in, and `handler_s3_wildcard_grant` drives the breadth of a wildcard grant on
//! one service. What only this one reaches is the SCOPE of a grant over two services a handler
//! calls.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{given_role, iam};
use crate::event_source_mappings::{await_function_active, call, call_sqs, create_queue, router_on};
use crate::handler_s3_object_access::session_arn;
use crate::handler_secret_relayed_to_queue::secrets;
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The function, the role it runs as, and the name of the one inline identity policy the rows
/// rewrite on that role — the name [`given_role`] writes it under, so each row replaces the
/// document the row before it left rather than adding a second one.
const FUNCTION: &str = "queue-and-secret-grant-scoping";
const ROLE_NAME: &str = "queue-and-secret-grant-scoping-role";
const INLINE_POLICY: &str = "event-source-reads";

/// The two queues of the table: the one a scoped statement names, and another the account
/// holds that no statement ever names.
const NAMED_QUEUE: &str = "grant-scoping-named";
const OTHER_QUEUE: &str = "grant-scoping-other";

/// The two secrets of the table, under the same split, and the values they hold.
const NAMED_SECRET: &str = "grant-scoping/named";
const OTHER_SECRET: &str = "grant-scoping/other";
const NAMED_VALUE: &str = "{\"username\":\"named\"}";
const OTHER_VALUE: &str = "{\"username\":\"other\"}";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization of its calls and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: i64 = 120;

/// How long a wait for work the clock owes may take before it is a failure, and how often to
/// look while waiting. The deadline is generous so a loaded host cannot decide the outcome.
const BUDGET: Duration = Duration::from_secs(180);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The authorization refusal each service answers, with the status it carries: the AWS-common
/// `AccessDenied` at HTTP 403 for Amazon SQS, and `AccessDeniedException` for Secrets Manager,
/// whose every client error travels at HTTP 400.
const QUEUE_DENIED: &str = "AccessDenied";
const QUEUE_DENIED_STATUS: i64 = 403;
const SECRET_DENIED: &str = "AccessDeniedException";
const SECRET_DENIED_STATUS: i64 = 400;

/// The actions the two calls are judged under, as each service's own refusal NAMES them:
/// Amazon SQS words a refusal with the action case-folded, Secrets Manager with the action as
/// the model spells the operation.
const QUEUE_ACTION: &str = "sqs:sendmessage";
const SECRET_ACTION: &str = "secretsmanager:GetSecretValue";

/// The actions a policy STATES, which IAM compares case-insensitively.
const QUEUE_GRANT_ACTION: &str = "sqs:SendMessage";
const SECRET_GRANT_ACTION: &str = "secretsmanager:GetSecretValue";

/// The handler every row runs: build both clients during INIT, then make exactly the calls the
/// event names, in order, and report what each answered — so the invocation's payload IS what
/// the SDK gave the guest.
const SOURCE: &str = r#"
import boto3
import botocore.exceptions

QUEUES = boto3.client("sqs")
SECRETS = boto3.client("secretsmanager")


def lambda_handler(event, context):
    results = []
    for made in event["calls"]:
        try:
            if made["service"] == "sqs":
                answered = QUEUES.send_message(
                    QueueUrl=made["target"], MessageBody=made["body"]
                )
                results.append({"ok": True, "value": answered["MessageId"]})
            else:
                answered = SECRETS.get_secret_value(SecretId=made["target"])
                results.append({"ok": True, "value": answered["SecretString"]})
        except botocore.exceptions.ClientError as error:
            results.append({
                "ok": False,
                "error": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            })
    return {"results": results}
"#;

/// Which service a call of a row reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reached {
    Queue,
    Secret,
}

/// Which resource of that service the call names: the one a scoped statement names, or another
/// the account holds that no statement does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    Named,
    Other,
}

/// How a Secrets Manager read spells the secret it names — both forms `GetSecretValue` accepts
/// for an existing secret, and what a refusal states back is whichever the caller supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelled {
    /// The secret's name. Every Amazon SQS call uses this, naming the queue by its url.
    Name,
    /// The secret's complete ARN.
    CompleteArn,
}

/// What a call must come to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The service served it.
    Served,
    /// The service refused it, no statement having allowed the action on that resource.
    Denied,
}

/// One call a row's invocation makes.
#[derive(Debug, Clone, Copy)]
struct Made {
    label: &'static str,
    reached: Reached,
    addressed: Addressed,
    spelled: Spelled,
    expected: Expected,
}

/// One row: the grant the execution role's inline identity policy carries, and the calls the
/// handler makes under it.
struct Row {
    case: &'static str,
    /// The statements of that document, built from the resources this suite created.
    statements: Vec<Value>,
    calls: &'static [Made],
}

/// The queues and secrets the rows address, as the services that own them report them.
struct Resources {
    named_queue_url: String,
    named_queue_arn: String,
    other_queue_url: String,
    other_queue_arn: String,
    named_secret_arn: String,
    other_secret_arn: String,
}

impl Resources {
    /// The url a send names the queue `addressed` by.
    fn queue_url(&self, addressed: Addressed) -> &str {
        match addressed {
            Addressed::Named => &self.named_queue_url,
            Addressed::Other => &self.other_queue_url,
        }
    }

    /// The ARN a refusal of that send states as the resource: the queue's own, which is what a
    /// policy statement matches.
    fn queue_arn(&self, addressed: Addressed) -> &str {
        match addressed {
            Addressed::Named => &self.named_queue_arn,
            Addressed::Other => &self.other_queue_arn,
        }
    }

    /// The `SecretId` a read names the secret `addressed` by, spelled as `spelled` says.
    fn secret_id(&self, addressed: Addressed, spelled: Spelled) -> String {
        match (addressed, spelled) {
            (Addressed::Named, Spelled::Name) => NAMED_SECRET.to_string(),
            (Addressed::Other, Spelled::Name) => OTHER_SECRET.to_string(),
            (Addressed::Named, Spelled::CompleteArn) => self.named_secret_arn.clone(),
            (Addressed::Other, Spelled::CompleteArn) => self.other_secret_arn.clone(),
        }
    }

    /// The value a served read of that secret answers.
    fn secret_value(addressed: Addressed) -> &'static str {
        match addressed {
            Addressed::Named => NAMED_VALUE,
            Addressed::Other => OTHER_VALUE,
        }
    }

    /// The target and the refusal's resource clause of one call.
    fn target(&self, made: &Made) -> (String, String) {
        match made.reached {
            Reached::Queue => (
                self.queue_url(made.addressed).to_string(),
                self.queue_arn(made.addressed).to_string(),
            ),
            Reached::Secret => {
                let id = self.secret_id(made.addressed, made.spelled);
                (id.clone(), id)
            }
        }
    }
}

/// One `Allow` statement over `action` and `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({"Effect": "Allow", "Action": action, "Resource": resource})
}

/// The rows: the grant's scope, varied from one that covers neither call through one scoped to
/// a single resource of each service to one that covers every resource of both.
fn rows(resources: &Resources) -> Vec<Row> {
    vec![
    Row {
        case: "a role whose policy names another action of each service: neither call is \
               allowed",
        statements: vec![
                // Another action of each service, so the role is still one carrying grants on
                // both: what refuses the calls below is an implicit deny about THEIR action
                // and not an empty policy set.
                allow("sqs:ReceiveMessage", "*"),
                allow("secretsmanager:DescribeSecret", "*"),
        ],
        calls: &[
            Made {
                label: "the send to the queue the next row's statement names",
                reached: Reached::Queue,
                addressed: Addressed::Named,
                spelled: Spelled::Name,
                expected: Expected::Denied,
            },
            Made {
                label: "the read of the secret the next row's statement names",
                reached: Reached::Secret,
                addressed: Addressed::Named,
                spelled: Spelled::Name,
                expected: Expected::Denied,
            },
        ],
    },
    Row {
        case: "each action scoped to ONE resource's ARN: that resource is served and the other \
               of the same service is not",
        statements: vec![
            allow(QUEUE_GRANT_ACTION, &resources.named_queue_arn),
            allow(SECRET_GRANT_ACTION, &resources.named_secret_arn),
        ],
        calls: &[
            Made {
                label: "the send to the queue the statement names",
                reached: Reached::Queue,
                addressed: Addressed::Named,
                spelled: Spelled::Name,
                expected: Expected::Served,
            },
            Made {
                label: "the send to another queue of the account",
                reached: Reached::Queue,
                addressed: Addressed::Other,
                spelled: Spelled::Name,
                expected: Expected::Denied,
            },
            Made {
                label: "the read of the secret the statement names, by its name",
                reached: Reached::Secret,
                addressed: Addressed::Named,
                spelled: Spelled::Name,
                expected: Expected::Served,
            },
            Made {
                label: "the read of that same secret, by its complete ARN",
                reached: Reached::Secret,
                addressed: Addressed::Named,
                spelled: Spelled::CompleteArn,
                expected: Expected::Served,
            },
            Made {
                label: "the read of another secret of the account, by its complete ARN",
                reached: Reached::Secret,
                addressed: Addressed::Other,
                spelled: Spelled::CompleteArn,
                expected: Expected::Denied,
            },
        ],
    },
    Row {
        case: "each action over every resource of its service: the resources no scoped \
               statement named are served too",
        statements: vec![
            allow(QUEUE_GRANT_ACTION, "*"),
            allow(SECRET_GRANT_ACTION, "*"),
        ],
        calls: &[
            Made {
                label: "the send to the queue no scoped statement named",
                reached: Reached::Queue,
                addressed: Addressed::Other,
                spelled: Spelled::Name,
                expected: Expected::Served,
            },
            Made {
                label: "the read of the secret no scoped statement named, by its name",
                reached: Reached::Secret,
                addressed: Addressed::Other,
                spelled: Spelled::Name,
                expected: Expected::Served,
            },
        ],
    },
    ]
}

/// The body one call of a row sends, which identifies that call in the queue it reaches.
fn body_of(case_index: usize, call_index: usize) -> String {
    format!("grant-scoping-row-{case_index}-call-{call_index}")
}

/// The sentence IAM words an implicit deny with: who was refused, which action, on which
/// resource, and that no identity-based policy allows it.
fn implicit_deny(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// Lambda over the SAME engine as the two services the handler calls, plus the IAM store the
/// execution role lives in and the token service its session is minted by — exactly what
/// `core/router` assembles. A handler's own SDK call is dispatched back into this same router,
/// so the queue it sends to and the secret it reads are the ones this suite wrote.
fn scoping_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
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

/// The identity policy in force on the execution role from now on: `statements` as its one
/// inline document, written with IAM's own operation.
async fn given_policy(router: &MultiServiceRouter, case: &str, statements: &[Value]) {
    let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", document.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutRolePolicy to answer 200, got {status} {written}"
    );
}

/// Create a secret of `name` holding `value`, and answer the ARN Secrets Manager allocated.
async fn given_secret(router: &MultiServiceRouter, case: &str, name: &str, value: &str) -> String {
    let created = secrets(
        router,
        case,
        "CreateSecret",
        &json!({
            "Name": name,
            "ClientRequestToken": format!("{name}-version-000000").replace('/', "-"),
            "SecretString": value,
        }),
    )
    .await;
    created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecret reported no ARN in {created}"))
        .to_string()
}

/// Take every message the queue at `url` holds, deleting each one, and answer their bodies
/// sorted.
///
/// `wanted` is how many the caller expects: the loop looks again until it has them or the
/// generous deadline passes, so a host that is slow to make a message available cannot decide
/// the outcome, and a caller expecting none takes one look and reports what it found.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<String> = Vec::new();
    loop {
        let (status, answered) = call_sqs(
            router,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ReceiveMessage to answer 200, got {status} {answered}"
        );
        for message in answered["Messages"].as_array().cloned().unwrap_or_default() {
            held.push(message["Body"].as_str().unwrap_or_default().to_string());
            let (status, deleted) = call_sqs(
                router,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DeleteMessage to answer 200, got {status} {deleted}"
            );
        }
        if held.len() >= wanted || Instant::now() >= deadline {
            held.sort();
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What one call must come to: the value the service served, or that service's own
/// authorization refusal, naming the session the call was made as, the action, and the
/// resource the request named.
fn assert_call(case: &str, principal: &str, made: &Made, resource: &str, answered: &Value) {
    let label = made.label;
    match made.expected {
        Expected::Served => {
            assert_eq!(
                answered["ok"],
                json!(true),
                "case {case:?}: expected {label} to be served, got {answered}"
            );
            if made.reached == Reached::Secret {
                assert_eq!(
                    answered["value"],
                    json!(Resources::secret_value(made.addressed)),
                    "case {case:?}: expected {label} to answer the value the secret holds, got \
                     {answered}"
                );
            }
        }
        Expected::Denied => {
            let (code, status, action) = match made.reached {
                Reached::Queue => (QUEUE_DENIED, QUEUE_DENIED_STATUS, QUEUE_ACTION),
                Reached::Secret => (SECRET_DENIED, SECRET_DENIED_STATUS, SECRET_ACTION),
            };
            assert_eq!(
                answered["ok"],
                json!(false),
                "case {case:?}: expected {label} to be refused, got {answered}"
            );
            assert_eq!(
                answered["error"],
                json!(code),
                "case {case:?}: expected the authorization code of the service {label} reached, \
                 got {answered}"
            );
            assert_eq!(
                answered["status"],
                json!(status),
                "case {case:?}: expected the status that service answers a refusal with for \
                 {label}, got {answered}"
            );
            let expected = implicit_deny(principal, action, resource);
            assert_eq!(
                answered["message"],
                json!(expected),
                "case {case:?}: expected the refusal {expected:?} for {label}, got {answered}"
            );
        }
    }
}

/// RULE: a handler's own SDK call is decided against the identity-based policies in force on
/// the execution role at the instant of the call, matched on the action and on the ARN of the
/// resource the request names — so a call no statement allows is refused with that service's
/// own authorization code and IAM's implicit-deny sentence naming the function's assumed-role
/// session, a grant scoped to one resource ARN reaches that resource and no other of the same
/// service, and a refused call leaves the service it was refused by exactly as it was.
#[tokio::test]
async fn an_execution_roles_grant_decides_a_handlers_queue_and_secret_calls() {
    let _stream = one_invocation_stream_at_a_time().await;
    let setup = "the function, its role, the queues and the secrets of the scoping table";
    let router = scoping_router();

    let (named_queue_url, named_queue_arn) =
        create_queue(&router, setup, NAMED_QUEUE, &json!({})).await;
    let (other_queue_url, other_queue_arn) =
        create_queue(&router, setup, OTHER_QUEUE, &json!({})).await;
    let resources = Resources {
        named_queue_url,
        named_queue_arn,
        other_queue_url,
        other_queue_arn,
        named_secret_arn: given_secret(&router, setup, NAMED_SECRET, NAMED_VALUE).await,
        other_secret_arn: given_secret(&router, setup, OTHER_SECRET, OTHER_VALUE).await,
    };

    // The role starts out carrying no grant at all; each row writes the document it is about
    // before it invokes.
    let role_arn = given_role(&router, setup, ROLE_NAME, None).await;
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {setup:?}: expected CreateFunction {FUNCTION:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(&router, setup, FUNCTION).await;

    let principal = session_arn(ROLE_NAME, FUNCTION);
    for (index, row) in rows(&resources).iter().enumerate() {
        let case = row.case;
        given_policy(&router, case, &row.statements).await;

        let calls: Vec<Value> = row
            .calls
            .iter()
            .enumerate()
            .map(|(position, made)| {
                let (target, _) = resources.target(made);
                json!({
                    "service": match made.reached {
                        Reached::Queue => "sqs",
                        Reached::Secret => "secretsmanager",
                    },
                    "target": target,
                    "body": body_of(index, position),
                })
            })
            .collect();
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            &json!({"calls": calls}),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        let signal = answered.header("x-amz-function-error");
        let payload = answered.json();
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        let results = payload["results"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            results.len(),
            row.calls.len(),
            "case {case:?}: expected one answer per call, got {payload}"
        );
        for (made, result) in row.calls.iter().zip(&results) {
            let (_, resource) = resources.target(made);
            assert_call(case, &principal, made, &resource, result);
        }

        // What each QUEUE holds: the bodies of the sends this row had served and nothing else,
        // read back where a client reads them.
        for addressed in [Addressed::Named, Addressed::Other] {
            let mut expected: Vec<String> = row
                .calls
                .iter()
                .enumerate()
                .filter(|(_, made)| {
                    made.reached == Reached::Queue
                        && made.addressed == addressed
                        && made.expected == Expected::Served
                })
                .map(|(position, _)| body_of(index, position))
                .collect();
            expected.sort();
            let held = drained(
                &router,
                case,
                resources.queue_url(addressed),
                expected.len(),
            )
            .await;
            assert_eq!(
                held, expected,
                "case {case:?}: expected the queue {:?} to hold {expected:?}, got {held:?}",
                resources.queue_url(addressed)
            );
        }
    }
}
