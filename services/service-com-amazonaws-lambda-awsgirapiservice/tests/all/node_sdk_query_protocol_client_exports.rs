// SPDX-License-Identifier: CC0-1.0

//! The AWS SDK for JavaScript v3 a `nodejs*` runtime BUNDLES, for the services whose models
//! declare a protocol other than one of the JSON ones: an `index.mjs` whose first lines are
//! `import { STSClient, GetCallerIdentityCommand } from '@aws-sdk/client-sts'` and the same for
//! the identity and notification services, every name it binds held against the shapes those
//! services' own models declare, and every command it sent held against the state the service
//! that answered it then reports.
//!
//! RULE: a managed Node.js runtime ships the AWS SDK for JavaScript v3 in the execution
//! environment, and the `@aws-sdk/client-<token>` package it supplies for a service exports —
//! as the named exports an ES module's `import { … } from` binds when the module is LINKED, so
//! before any of them is read — that service's client class, a `<Operation>Command` class for
//! every operation its own model declares and an error class for every error shape it declares.
//! That surface is the model's and not the wire protocol's: a service whose model declares
//! `awsQuery` exports the same classes as one declaring an AWS JSON protocol, and a command
//! sent through its client is BUILT and PARSED as the protocol that model declares, so what the
//! handler reads back is the emulated service's own answer and what the call wrote is what that
//! service then reports.
//!
//! The first suite's rows differ along what the rule generalises over: which service's package
//! is imported and which protocol its model declares (`awsQuery` for the token, identity,
//! notification and email services, `awsJson1_0` for the queue service), and therefore how many
//! operations and error shapes that model declares. No row writes a command or an error class
//! name of its own: each expected name is read out of that service's own Smithy model, as it
//! ships beside its emulator crate.
//!
//! The second suite's rows differ along the SERVICE called, the shape of the input the command
//! carries (none at all, a scalar, a policy document) and whether the service serves or refuses
//! the call — with the refusal held against what the SAME call answers on the wire, so the
//! status and the sentence a JS consumer reads are the emulated service's own.
//!
//! Both suites RUN handlers, so they take this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{
    allow, element, form_encode, given_role, iam, logs_only,
};
use crate::event_source_mappings::{
    await_function_active, call, router_on, CLOCK_BUDGET, LOOK_AGAIN,
};
use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
// The one .zip writer this target packages a node handler with, owned by the suite that first
// needed it rather than copied here.
use crate::node_invoke_run::package;

/// The Region every request below is signed for, and the one the handlers' clients are built
/// with.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The managed Node release every function below runs: one that bundles the SDK.
const RUNTIME: &str = "nodejs22.x";

/// How long one of these runs is given, well above what a handler making a handful of
/// in-process calls needs.
const TIMEOUT_SECONDS: i64 = 30;

/// A well-formed execution role ARN for the suite that makes no call at all, satisfying the
/// model's `RoleArn` pattern: that handler only READS the names its imports bound, so no
/// session of it is minted and no identity service is mounted.
const PLAIN_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// One service whose client package the first suite asks about: the token the SDK publishes its
/// package under, the class that package exports its client as, that service's own Smithy model
/// as it ships beside its emulator crate, and one command the package must export — the class a
/// handler of the shape this suite is about imports by name.
struct Package {
    token: &'static str,
    client: &'static str,
    model_path: &'static str,
    command: &'static str,
}

/// The packages the first suite reads, spanning the protocols a called service's model declares:
/// four whose models declare `awsQuery` and one whose model declares an AWS JSON protocol, which
/// is what makes the exported surface the MODEL's rather than the protocol's.
const PACKAGES: &[Package] = &[
    Package {
        token: "sts",
        client: "STSClient",
        model_path: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../service-com-amazonaws-sts-awssecuritytokenservicev20110615/smithy-model/\
             com-amazonaws-sts-awssecuritytokenservicev20110615.json"
        ),
        command: "GetCallerIdentityCommand",
    },
    Package {
        token: "iam",
        client: "IAMClient",
        model_path: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/\
             com-amazonaws-iam-awsidentitymanagementv20100508.json"
        ),
        command: "GetRoleCommand",
    },
    Package {
        token: "sns",
        client: "SNSClient",
        model_path: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../service-com-amazonaws-sns-amazonsimplenotificationservice/smithy-model/\
             com-amazonaws-sns-amazonsimplenotificationservice.json"
        ),
        command: "CreateTopicCommand",
    },
    Package {
        token: "ses",
        client: "SESClient",
        model_path: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../service-com-amazonaws-ses-simpleemailservice/smithy-model/\
             com-amazonaws-ses-simpleemailservice.json"
        ),
        command: "SendEmailCommand",
    },
    Package {
        token: "sqs",
        client: "SQSClient",
        model_path: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json"
        ),
        command: "SendMessageCommand",
    },
];

/// The handler the first suite runs: an ES module importing each bundled client package by
/// NAMESPACE, which binds every name the package exports, and reporting those names — the
/// command classes apart from every other class it exports.
const EXPORTS_HANDLER: &str = r#"
import * as sts from "@aws-sdk/client-sts";
import * as iam from "@aws-sdk/client-iam";
import * as sns from "@aws-sdk/client-sns";
import * as ses from "@aws-sdk/client-ses";
import * as sqs from "@aws-sdk/client-sqs";

const PACKAGES = { sts: sts, iam: iam, sns: sns, ses: ses, sqs: sqs };

export const handler = async (event) => {
    const report = {};
    for (const token of event.tokens) {
        const held = PACKAGES[token];
        if (held === undefined) {
            report[token] = { imported: false };
            continue;
        }
        const names = Object.keys(held);
        report[token] = {
            commands: names.filter((name) => name.endsWith("Command")).sort(),
            classes: names
                .filter((name) => !name.endsWith("Command") && typeof held[name] === "function")
                .sort(),
        };
    }
    return report;
};
"#;

/// The function the first suite runs.
const EXPORTS_FUNCTION: &str = "query-protocol-sdk-exports";

/// Every operation shape and every ERROR shape the model at `path` declares, by their own
/// names: the names the SDK's package for that service exports a command and an error class
/// for, read out of the same file that service's emulator crate is generated from.
fn model_names(case: &str, path: &str) -> (Vec<String>, Vec<String>) {
    let source = std::fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("case {case:?}: the model at {path} must be readable: {error}")
    });
    let document: Value = serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("case {case:?}: the model at {path} must parse: {error}"));
    let shapes = document["shapes"]
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: the model at {path} must declare shapes"));
    let mut operations: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (id, shape) in shapes {
        let Some(name) = id.split('#').next_back().filter(|name| !name.is_empty()) else {
            continue;
        };
        let is_error = shape
            .get("traits")
            .and_then(Value::as_object)
            .is_some_and(|traits| traits.contains_key("smithy.api#error"));
        match shape["type"].as_str() {
            Some("operation") => operations.push(name.to_string()),
            Some("structure") if is_error => errors.push(name.to_string()),
            _ => {}
        }
    }
    operations.sort();
    operations.dedup();
    errors.sort();
    errors.dedup();
    (operations, errors)
}

/// The list of names a handler reported under `member`, which it answered sorted.
fn names(case: &str, reported: &Value, member: &str) -> Vec<String> {
    reported[member]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the handler to report {member} as a list, got {reported}"
            )
        })
        .iter()
        .map(|name| {
            name.as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected text in {member}, got {name}"))
                .to_string()
        })
        .collect()
}

/// Create a `nodejs*` function whose deployment package holds `source` as its one ES module, and
/// wait — looking at the function's own configuration again rather than sleeping for a span —
/// until it reports the state an invocation needs.
async fn create_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role: &str,
    source: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("index.mjs", source)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// `Invoke` the function synchronously with `event`, answering the `FunctionError` signal the
/// response carried and the payload the run produced.
///
/// An answer reporting one of this emulator's own HOST bounds — the run slots every suite of
/// this target shares — is not an answer to this request
/// ([`crate::invoke_request_id::is_host_bound`]), so the request is made again until one is or
/// the budget is spent.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let answered = call(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/invocations"),
            event,
        )
        .await;
        if !is_host_bound(answered.status, &answered.text()) {
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
                answered.status,
                answered.text()
            );
            return (answered.header("x-amz-function-error"), answered.json());
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every invocation was answered with one of this emulator's own host \
             bounds: {}",
            answered.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: the package a managed Node runtime bundles for a service exports that service's client
/// class, a `<Operation>Command` class for every operation its own model declares and an error
/// class for every error shape it declares — whichever wire protocol that model declares — and
/// an ES module's named imports of them resolve, because the names are stated before any of them
/// is read.
#[tokio::test]
async fn every_bundled_client_package_exports_the_classes_its_services_model_names() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let case = "an ES module handler importing every bundled client package";
    create_function(&router, case, EXPORTS_FUNCTION, PLAIN_ROLE, EXPORTS_HANDLER).await;
    let tokens: Vec<&str> = PACKAGES.iter().map(|package| package.token).collect();
    let (signal, payload) = invoke(
        &router,
        case,
        EXPORTS_FUNCTION,
        &json!({ "tokens": tokens }),
    )
    .await;
    assert_eq!(
        signal, None,
        "case {case:?}: the handler must link and run to completion with no function error, got \
         {signal:?} with {payload}"
    );

    for held in PACKAGES {
        let case = held.token;
        let reported = &payload[held.token];
        assert!(
            reported.is_object(),
            "case {case:?}: expected the handler to report this package, got {payload}"
        );
        let (operations, errors) = model_names(case, held.model_path);
        // Sorted as the CLASS names, which is the order the handler reported them in: the two
        // orders differ wherever one operation's name is a prefix of another's.
        let mut expected: Vec<String> = operations
            .iter()
            .map(|operation| format!("{operation}Command"))
            .collect();
        expected.sort();
        let commands = names(case, reported, "commands");
        assert_eq!(
            commands.len(),
            expected.len(),
            "case {case:?}: expected one command class per operation the model declares ({} of \
             them), got {} of them",
            expected.len(),
            commands.len()
        );
        assert_eq!(
            commands, expected,
            "case {case:?}: expected the command classes the model's operations name, got \
             {commands:?}"
        );
        assert!(
            commands.iter().any(|name| name == held.command),
            "case {case:?}: expected {} among the exported command classes, got {commands:?}",
            held.command
        );
        let classes = names(case, reported, "classes");
        for shape in &errors {
            assert!(
                classes.contains(shape),
                "case {case:?}: expected the error class the model's {shape} shape names to be \
                 exported, got {classes:?}"
            );
        }
        assert!(
            classes.iter().any(|name| name == held.client),
            "case {case:?}: expected the client class {} among the exported classes, got \
             {classes:?}",
            held.client
        );
    }
}

/// The function the second suite runs, the execution role it runs as, the role its own call
/// writes, the topic it creates and the role name no role answers to.
const CALLER_FUNCTION: &str = "query-protocol-sdk-caller";
const CALLER_ROLE: &str = "query-protocol-sdk-caller-exec";
const WRITTEN_ROLE: &str = "query-protocol-written-by-the-handler";
const WRITTEN_TOPIC: &str = "query-protocol-written-topic";
const ABSENT_ROLE: &str = "query-protocol-no-such-role";

/// The handler the second suite runs: an ES module importing each client and each command class
/// BY NAME — the form a JS v3 consumer writes, whose names are bound when the module is linked —
/// and sending the one command each probe names, reporting what the service answered or refused.
const CALLER_HANDLER: &str = r#"
import { STSClient, GetCallerIdentityCommand, STSServiceException } from "@aws-sdk/client-sts";
import {
    IAMClient,
    CreateRoleCommand,
    GetRoleCommand,
    IAMServiceException,
} from "@aws-sdk/client-iam";
import { SNSClient, CreateTopicCommand, SNSServiceException } from "@aws-sdk/client-sns";

const CLIENTS = {
    sts: { Client: STSClient, Exception: STSServiceException },
    iam: { Client: IAMClient, Exception: IAMServiceException },
    sns: { Client: SNSClient, Exception: SNSServiceException },
};

const COMMANDS = {
    "sts:GetCallerIdentity": GetCallerIdentityCommand,
    "iam:CreateRole": CreateRoleCommand,
    "iam:GetRole": GetRoleCommand,
    "sns:CreateTopic": CreateTopicCommand,
};

export const handler = async (event) => {
    const probes = {};
    for (const probe of event.probes) {
        const held = CLIENTS[probe.token];
        const Command = COMMANDS[probe.token + ":" + probe.operation];
        if (held === undefined || Command === undefined) {
            probes[probe.case] = { bound: false };
            continue;
        }
        const client = new held.Client({ region: event.region });
        try {
            const answered = await client.send(new Command(probe.input));
            probes[probe.case] = {
                ok: true,
                httpStatusCode: answered.$metadata.httpStatusCode,
                members: Object.keys(answered)
                    .filter((name) => name !== "$metadata")
                    .sort(),
                answered: answered,
            };
        } catch (error) {
            probes[probe.case] = {
                ok: false,
                name: error.name,
                message: error.message,
                isServiceException: error instanceof held.Exception,
                isError: error instanceof Error,
                httpStatusCode: error.$metadata === undefined
                    ? null
                    : error.$metadata.httpStatusCode,
            };
        }
    }
    return probes;
};
"#;

/// One probe the second suite's handler makes: the service it calls, the operation the command
/// addresses, the input it carries and whether that service refuses it.
struct Probe {
    case: &'static str,
    token: &'static str,
    operation: &'static str,
    refused: bool,
}

/// The probes, in the order the handler makes them.
const PROBES: &[Probe] = &[
    Probe {
        case: "the identity of the session the run calls as, from the token service",
        token: "sts",
        operation: "GetCallerIdentity",
        refused: false,
    },
    Probe {
        case: "a role written through the identity service",
        token: "iam",
        operation: "CreateRole",
        refused: false,
    },
    Probe {
        case: "a read of the role that write left behind",
        token: "iam",
        operation: "GetRole",
        refused: false,
    },
    Probe {
        case: "a topic written through the notification service",
        token: "sns",
        operation: "CreateTopic",
        refused: false,
    },
    Probe {
        case: "a read of a role the identity service does not hold",
        token: "iam",
        operation: "GetRole",
        refused: true,
    },
];

/// A trust policy naming this service as the principal that may assume the role, which is what
/// the role the handler writes carries.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The input each probe's command carries.
fn input(case: &str) -> Value {
    match case {
        "a role written through the identity service" => json!({
            "RoleName": WRITTEN_ROLE,
            "AssumeRolePolicyDocument": trust_policy(),
        }),
        "a read of the role that write left behind" => json!({ "RoleName": WRITTEN_ROLE }),
        "a topic written through the notification service" => json!({ "Name": WRITTEN_TOPIC }),
        "a read of a role the identity service does not hold" => {
            json!({ "RoleName": ABSENT_ROLE })
        }
        // The token service's identity read takes no input at all.
        _ => json!({}),
    }
}

/// One `awsQuery` call to Amazon SNS, as the account's own credential — the way this suite reads
/// back what the handler's own call left behind.
async fn sns_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("sns.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The assumed-role session ARN a call an invocation of `function_name` running as `role_name`
/// makes speaks for: the session is named for the FUNCTION.
fn session_arn(role_name: &str, function_name: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{function_name}",
        engine::emulator_account_id()
    )
}

/// RULE: a command class the bundled SDK exports for a service whose model declares a
/// query-family protocol sends the call that protocol binds, so the answer a handler reads is
/// that service's own — a success whose values are the state it then reports to every other
/// client, and a refusal carrying the status and the sentence the same call answers on the wire.
#[tokio::test]
async fn an_es_module_handler_sends_imported_commands_to_the_query_protocol_services() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(engine),
    );
    let case = "an ES module handler driving the bundled query-protocol clients";
    // The role the function runs as, granted exactly the actions its probes call and the log
    // writes every execution role makes — so each call is judged against the policies in force
    // on that role by the service that serves it.
    let role_arn = given_role(
        &router,
        case,
        CALLER_ROLE,
        Some(json!([
            logs_only(),
            allow("sts", &["GetCallerIdentity"], "*"),
            allow("iam", &["CreateRole", "GetRole"], "*"),
            allow("sns", &["CreateTopic"], "*"),
        ])),
    )
    .await;
    create_function(&router, case, CALLER_FUNCTION, &role_arn, CALLER_HANDLER).await;

    let asked: Vec<Value> = PROBES
        .iter()
        .map(|probe| {
            json!({
                "case": probe.case,
                "token": probe.token,
                "operation": probe.operation,
                "input": input(probe.case),
            })
        })
        .collect();
    let (signal, payload) = invoke(
        &router,
        case,
        CALLER_FUNCTION,
        &json!({ "region": REGION, "probes": asked }),
    )
    .await;
    assert_eq!(
        signal, None,
        "case {case:?}: the handler must link and run to completion with no function error, got \
         {signal:?} with {payload}"
    );

    for probe in PROBES {
        let case = probe.case;
        let reported = &payload[case];
        assert!(
            reported.is_object(),
            "case {case:?}: expected the handler to report this probe, got {payload}"
        );
        assert_eq!(
            reported["ok"],
            json!(!probe.refused),
            "case {case:?}: expected ok={}, got {reported}",
            !probe.refused
        );
        if probe.refused {
            continue;
        }
        assert_eq!(
            reported["httpStatusCode"],
            json!(200),
            "case {case:?}: expected the served call to report HTTP 200, got {reported}"
        );
        assert!(
            reported["members"]
                .as_array()
                .is_some_and(|members| !members.is_empty()),
            "case {case:?}: expected the answer to carry the operation's own output members, got \
             {reported}"
        );
    }

    // The identity the run calls as: a session of the function's own execution role, named for
    // the function, in the account this emulator serves.
    let identity = &payload["the identity of the session the run calls as, from the token service"]
        ["answered"];
    assert_eq!(
        identity["Arn"],
        json!(session_arn(CALLER_ROLE, CALLER_FUNCTION)),
        "case {case:?}: expected the token service to report the function's own session, got \
         {identity}"
    );
    assert_eq!(
        identity["Account"],
        json!(engine::emulator_account_id()),
        "case {case:?}: expected the account this emulator serves, got {identity}"
    );
    assert!(
        identity["UserId"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected the session's own user id, got {identity}"
    );

    // What the IDENTITY SERVICE holds afterwards: the role the handler's own `CreateRole` wrote,
    // read back through that service's own `GetRole`.
    let (status, read) = iam(&router, "GetRole", &[("RoleName", WRITTEN_ROLE)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the role the handler wrote to be readable through IAM's own \
         GetRole, got {status}: {read}"
    );
    let written_arn = element(&read, "Arn");
    let handler_read = &payload["a read of the role that write left behind"]["answered"]["Role"];
    assert_eq!(
        handler_read["Arn"],
        json!(written_arn),
        "case {case:?}: expected the handler's own read to carry the ARN IAM holds for that role \
         ({written_arn}), got {handler_read}"
    );
    assert_eq!(
        handler_read["RoleName"],
        json!(WRITTEN_ROLE),
        "case {case:?}: expected the role's own name, got {handler_read}"
    );

    // What the NOTIFICATION SERVICE holds afterwards: the topic the handler created, listed by
    // that service's own `ListTopics`.
    let created_topic = payload["a topic written through the notification service"]["answered"]
        ["TopicArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a TopicArn, got {payload}"))
        .to_string();
    let (status, listed) = sns_call(&router, "ListTopics", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SNS ListTopics to answer 200, got {status}: {listed}"
    );
    assert!(
        listed.contains(&created_topic),
        "case {case:?}: expected the topic the handler created ({created_topic}) among the topics \
         the service lists, got {listed}"
    );
    assert!(
        created_topic.ends_with(&format!(":{WRITTEN_TOPIC}")),
        "case {case:?}: expected the created topic's ARN to name the topic, got {created_topic}"
    );

    // The REFUSAL, held against what the same call answers on the wire: the status and the
    // sentence a JS consumer reads are the ones the identity service itself answered.
    let (status, refused) = iam(&router, "GetRole", &[("RoleName", ABSENT_ROLE)]).await;
    assert!(
        status >= 400,
        "case {case:?}: this probe is a refusal, so the same call must be refused on the wire, \
         got {status}: {refused}"
    );
    let sentence = quick_xml::escape::unescape(&element(&refused, "Message"))
        .unwrap_or_else(|error| panic!("case {case:?}: the refusal's sentence must read: {error}"))
        .to_string();
    let reported = &payload["a read of a role the identity service does not hold"];
    assert_eq!(
        reported["httpStatusCode"],
        json!(status),
        "case {case:?}: expected the status the service answered ({status}), got {reported}"
    );
    assert_eq!(
        reported["message"],
        json!(sentence),
        "case {case:?}: expected the service's own sentence ({sentence:?}), got {reported}"
    );
    assert_eq!(
        reported["isServiceException"],
        json!(true),
        "case {case:?}: expected the refusal to be an instance of the service's own base \
         exception class, got {reported}"
    );
    assert_eq!(
        reported["isError"],
        json!(true),
        "case {case:?}: expected the refusal to be an Error, got {reported}"
    );
}
