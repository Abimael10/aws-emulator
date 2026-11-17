// SPDX-License-Identifier: CC0-1.0

//! What a function whose execution role is scoped to ONE secret reads from inside its handler,
//! on the first invocation and on every later one: Lambda, Secrets Manager, IAM and the token
//! service over ONE engine, a `python3.x` function that does `import boto3`, and the two secret
//! identifiers handed to the handler as the function's own ENVIRONMENT.
//!
//! RULE: every run of a function is handed the environment variables its configuration holds,
//! and each AWS SDK call the handler makes with the SDK that runtime supplies is signed as a
//! session of the function's execution role and decided afresh by the service it names, against
//! the identity-based documents in force on that role and the resource the request addresses —
//! so a statement allowing one action on one resource serves every call addressing THAT
//! resource, invocation after invocation alike, and leaves a call addressing any other resource
//! of the same service refused with that service's own authorization code and IAM's
//! implicit-deny sentence, naming the function's assumed-role session, the action and the
//! identifier the request supplied.
//!
//! What grounds it: "The execution role grants the function permission to use Amazon Web
//! Services services" (`FunctionConfiguration$Role`), and "Environment – Environment variables
//! that are accessible from function code during execution"
//! (`FunctionConfiguration$Environment`), so what the handler reads out of its environment is
//! what the create stored. The action a read performs is `secretsmanager:GetSecretValue`
//! (`GetSecretValue`, Secrets Manager's own model), which answers that version's `ARN`, `Name`
//! and `SecretString`; that service models no authorization error of its own, so a read nothing
//! in force allows is the AWS-common `AccessDeniedException`, carried at HTTP 400 because its
//! `awsJson1_1` envelope travels every client error there.
//!
//! The rows differ along what the rule generalises over: HOW the statement names the resource it
//! allows the read on (the secret's complete ARN, that ARN inside a LIST beside another secret's
//! of the same account, and the ARN written against the secret's name with a trailing wildcard —
//! the form a statement takes for a secret whose ARN "includes the name of the secret followed
//! by six random characters", `CreateSecretResponse$ARN`), the FORM of the value the granted
//! read must answer with (plain text, a JSON document, text carrying newlines and a character
//! outside ASCII), and how the REFUSED read spells the secret it names (its complete ARN, its
//! name), because the refusal states back the identifier the caller supplied. Every row is
//! invoked [`ATTEMPTS`] times over, so the verdict is pinned for the run that BUILDS the
//! function's execution environment and for the ones served by the environment it leaves
//! standing (`crate::state::execution::warm_pool`) alike: a positive control that holds only
//! once is not a control.
//!
//! What the neighbours pin and this suite does not repeat: `handler_queue_and_secret_grant_scoping`
//! drives the breadth of one grant across the two services a handler calls inside one
//! invocation, `handler_secret_sdk` drives how a blob member and a customer managed key reach
//! such a read, and `execution_role_grant_withdrawal` drives a grant rewritten under a running
//! function. What only this one reaches is a handler told WHICH secrets to read by its own
//! environment, reading the one its role names and being refused the other, on every attempt.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at most
//! `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, router_on};
use crate::handler_s3_object_access::session_arn;
use crate::handler_secret_relayed_to_queue::secrets;
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction`, `GetFunctionConfiguration` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute every package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime every function below is created with: a managed Python runtime, whose SDK is
/// what the handler imports.
const RUNTIME: &str = "python3.12";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is which reads the role admits and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: i64 = 120;

/// How many times each row invokes its function. The first run builds the execution
/// environment; the ones after it are served by the environment that run left standing, and the
/// rule has to hold on each.
const ATTEMPTS: i64 = 3;

/// The action a read performs, as a statement names it and as a refusal reports it.
const READ_ACTION: &str = "secretsmanager:GetSecretValue";

/// The code and status Secrets Manager refuses a read nothing in force allows with: the
/// AWS-common `AccessDeniedException`, carried at this service's client-error status because
/// `awsJson1_1` discriminates a failure by the error type in the body rather than by the
/// transport status.
const DENIED_CODE: &str = "AccessDeniedException";
const DENIED_STATUS: i64 = 400;

/// The secret of the account that NO row's statement names: the one every row's second read
/// addresses.
const OTHER_SECRET: &str = "own-scoped-secret/elsewhere";
const OTHER_VALUE: &str = "the-other-secrets-own-value";

/// A third secret of the account, which exists so that one row's statement can name a LIST of
/// two ARNs allocated by the service rather than one. Nothing ever reads it.
const DECOY_SECRET: &str = "own-scoped-secret/decoy";
const DECOY_VALUE: &str = "never-read";

/// The environment variables the function carries the two identifiers in, which is where its
/// handler reads them.
const OWN_VARIABLE: &str = "OWN_SECRET_ARN";
const OTHER_VARIABLE: &str = "OTHER_SECRET_ID";

/// The handler every row runs: it builds the client and reads its two identifiers out of the
/// ENVIRONMENT during init, then reads each secret and reports what the SDK handed it — so the
/// invocation's payload is what Secrets Manager answered the run.
const SOURCE: &str = r#"
import os

import boto3
import botocore.exceptions

SECRETS = boto3.client("secretsmanager")
OWN = os.environ["OWN_SECRET_ARN"]
OTHER = os.environ["OTHER_SECRET_ID"]


def _read(secret_id):
    try:
        answered = SECRETS.get_secret_value(SecretId=secret_id)
    except botocore.exceptions.ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "arn": answered["ARN"],
        "name": answered["Name"],
        "value": answered["SecretString"],
    }


def lambda_handler(event, context):
    own = _read(OWN)
    other = _read(OTHER)
    return {
        "attempt": event["attempt"],
        "own": own,
        "other": other,
    }
"#;

/// How the statement of a row names the resource it allows the read on.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// The secret's complete ARN, as the create answered it.
    CompleteArn,
    /// A list of two ARNs the account holds, the row's own secret among them.
    InAList,
    /// The ARN written against the secret's NAME with a trailing wildcard, which covers
    /// whatever suffix the service allocated.
    NameWildcard,
}

/// How the refused read spells the secret it names — both forms `GetSecretValue` takes for a
/// secret that exists, and what its refusal states back is whichever the caller supplied.
#[derive(Debug, Clone, Copy)]
enum Spelled {
    CompleteArn,
    Name,
}

/// One row: the secret only this row's role is granted, the value it holds, how the statement
/// names it, and how the read of the OTHER secret spells that one.
struct Row {
    case: &'static str,
    secret: &'static str,
    value: &'static str,
    scope: Scope,
    spelled: Spelled,
}

/// The rows: the resource form of the grant, the form of the value it unlocks, and the spelling
/// the refused read supplies.
const ROWS: &[Row] = &[
    Row {
        case: "a statement scoped to the secret's complete ARN, holding plain text",
        secret: "own-scoped-secret/plain",
        value: "secret-a-value",
        scope: Scope::CompleteArn,
        spelled: Spelled::CompleteArn,
    },
    Row {
        case: "a statement naming a LIST of two of the account's secret ARNs, holding a JSON \
               document",
        secret: "own-scoped-secret/document",
        value: "{\"username\":\"scoped\",\"password\":\"p4ssw0rd\"}",
        scope: Scope::InAList,
        spelled: Spelled::Name,
    },
    Row {
        case: "a statement scoped to the secret's name with a trailing wildcard, holding text \
               with newlines and a character outside ASCII",
        secret: "own-scoped-secret/wildcard",
        value: "line one\nline two with £ and a space\n",
        scope: Scope::NameWildcard,
        spelled: Spelled::CompleteArn,
    },
];

/// The role one row's function runs as, and the function itself — named here because the
/// refusal's own sentence names both: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("own-scoped-secret-role-{index}")
}

fn function_name(index: usize) -> String {
    format!("own-scoped-secret-probe-{index}")
}

/// Lambda over the SAME engine as Secrets Manager, plus the IAM store the execution role lives
/// in and the token service its session is minted by — what `core/router` assembles. A handler's
/// own SDK call is dispatched back into this router, so the secret it reads is the one this suite
/// wrote.
fn scoped_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
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

/// Create the secret `name` holding `value` through Secrets Manager's own `CreateSecret`, and
/// answer the ARN it allocated.
async fn given_secret(router: &MultiServiceRouter, case: &str, name: &str, value: &str) -> String {
    let created = secrets(
        router,
        case,
        "CreateSecret",
        &json!({
            "Name": name,
            // "you must generate a ClientRequestToken and include it in the request"
            // (`CreateSecretRequest$ClientRequestToken`), inside its modelled 32..64 length.
            "ClientRequestToken": format!("{:0>36}", name.replace(['/', '-'], "")),
            "SecretString": value,
        }),
    )
    .await;
    created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecret reported no ARN in {created}"))
        .to_string()
}

/// A statement's `Resource` written against the secret's NAME with a trailing wildcard: the ARN
/// the service allocates "includes the name of the secret followed by six random characters"
/// (`CreateSecretResponse$ARN`), so a statement written for a secret by name covers whatever
/// suffix it was given.
fn name_wildcard(case: &str, arn: &str, name: &str) -> String {
    let (prefix, suffix) = arn
        .rsplit_once('-')
        .unwrap_or_else(|| panic!("case {case:?}: expected a suffixed secret ARN, got {arn:?}"));
    assert!(
        prefix.ends_with(name) && !suffix.is_empty(),
        "case {case:?}: expected the ARN of {name:?} to be its name followed by a suffix of the \
         service's own, got {arn:?}"
    );
    format!("{prefix}-*")
}

/// The `Resource` element of a row's one statement.
fn resource_of(case: &str, scope: Scope, own_arn: &str, own_name: &str, decoy_arn: &str) -> Value {
    match scope {
        Scope::CompleteArn => json!(own_arn),
        Scope::InAList => json!([decoy_arn, own_arn]),
        Scope::NameWildcard => json!(name_wildcard(case, own_arn, own_name)),
    }
}

/// The sentence IAM words an implicit deny with: who was refused, which action, on which
/// resource, and that no identity-based policy allows it. Written out here rather than built
/// from the service's own helper, so the wording has something to be wrong about.
fn implicit_deny(principal: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {READ_ACTION} on resource: {resource} \
         because no identity-based policy allows the {READ_ACTION} action"
    )
}

/// RULE: a handler is handed the environment its function carries, and the SDK call it makes
/// with it is decided by the service it names against the documents in force on the execution
/// role and the resource the request addresses — so a role scoped to ONE secret reads that
/// secret on every invocation and is refused another secret of the same account, with the
/// identifier the read supplied stated back in the refusal.
#[tokio::test]
async fn a_role_scoped_to_one_secret_reads_it_on_every_invocation_and_no_other() {
    let _stream = one_invocation_stream_at_a_time().await;
    let setup = "the secrets, the roles and the functions of the scoping table";
    let router = scoped_router();

    let other_arn = given_secret(&router, setup, OTHER_SECRET, OTHER_VALUE).await;
    let decoy_arn = given_secret(&router, setup, DECOY_SECRET, DECOY_VALUE).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let own_arn = given_secret(&router, case, row.secret, row.value).await;
        let other_id = match row.spelled {
            Spelled::CompleteArn => other_arn.clone(),
            Spelled::Name => OTHER_SECRET.to_string(),
        };

        // The role carries ONE statement, allowing the read on this row's secret alone.
        let role = role_name(index);
        let function = function_name(index);
        let role_arn = given_role(
            &router,
            case,
            &role,
            Some(json!([{
                "Effect": "Allow",
                "Action": READ_ACTION,
                "Resource": resource_of(case, row.scope, &own_arn, row.secret, &decoy_arn),
            }])),
        )
        .await;

        let variables = json!({OWN_VARIABLE: own_arn, OTHER_VARIABLE: other_id});
        let created = call(
            &router,
            "POST",
            FUNCTIONS_URI,
            &json!({
                "FunctionName": function,
                "Role": role_arn,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Environment": {"Variables": variables},
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(SOURCE)),
                },
            }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: expected CreateFunction {function:?} to answer 201, got {} {}",
            created.status,
            created.text()
        );
        await_function_active(&router, case, &function).await;

        // The environment the runs are handed is the one the create stored, read back where a
        // client reads it.
        let configuration = call(
            &router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            configuration.json()["Environment"]["Variables"],
            variables,
            "case {case:?}: expected the stored environment to be the one the create carried, \
             got {}",
            configuration.text()
        );

        let principal = session_arn(&role, &function);
        let refusal = implicit_deny(&principal, &other_id);
        for attempt in 1..=ATTEMPTS {
            let answered = call(
                &router,
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/invocations"),
                &json!({"attempt": attempt}),
            )
            .await;
            assert_eq!(
                answered.status,
                200,
                "case {case:?} attempt {attempt}: expected HTTP 200 from Invoke, got {} {}",
                answered.status,
                answered.text()
            );
            let signal = answered.header("x-amz-function-error");
            let payload = answered.json();
            assert_eq!(
                signal, None,
                "case {case:?} attempt {attempt}: the handler must run to completion, got the \
                 function error {signal:?} with {payload}"
            );
            assert_eq!(
                payload["attempt"],
                json!(attempt),
                "case {case:?} attempt {attempt}: expected the run to report the event it was \
                 handed, got {payload}"
            );

            // The read its own role names: served, with the stored value and the secret's own
            // identity.
            assert_eq!(
                payload["own"]["ok"],
                json!(true),
                "case {case:?} attempt {attempt}: expected the read of the granted secret to be \
                 served, got {payload}"
            );
            assert_eq!(
                payload["own"]["value"],
                json!(row.value),
                "case {case:?} attempt {attempt}: expected the value the secret holds, got \
                 {payload}"
            );
            assert_eq!(
                payload["own"]["arn"],
                json!(own_arn),
                "case {case:?} attempt {attempt}: expected the granted secret's own ARN, got \
                 {payload}"
            );
            assert_eq!(
                payload["own"]["name"],
                json!(row.secret),
                "case {case:?} attempt {attempt}: expected the granted secret's own name, got \
                 {payload}"
            );

            // The read of another secret of the account, which no statement names.
            assert_eq!(
                payload["other"]["ok"],
                json!(false),
                "case {case:?} attempt {attempt}: expected the read of the other secret to be \
                 refused, got {payload}"
            );
            assert_eq!(
                payload["other"]["code"],
                json!(DENIED_CODE),
                "case {case:?} attempt {attempt}: expected Secrets Manager's own authorization \
                 code, got {payload}"
            );
            assert_eq!(
                payload["other"]["status"],
                json!(DENIED_STATUS),
                "case {case:?} attempt {attempt}: expected the status this service answers a \
                 refusal with, got {payload}"
            );
            assert_eq!(
                payload["other"]["message"],
                json!(refusal),
                "case {case:?} attempt {attempt}: expected the refusal {refusal:?}, got {payload}"
            );
        }

        // A refused read changed nothing: the owner still reads that secret's own value.
        let held = secrets(
            &router,
            case,
            "GetSecretValue",
            &json!({"SecretId": other_arn}),
        )
        .await;
        assert_eq!(
            held["SecretString"],
            json!(OTHER_VALUE),
            "case {case:?}: expected the refused secret to be unchanged, got {held}"
        );
    }
}
