// SPDX-License-Identifier: CC0-1.0

//! WHAT A DEPLOYED HANDLER'S ANSWER IS COMPUTED FROM, source class by source class: AWS
//! Lambda, Amazon SES API v2, IAM and the token service on ONE engine, over the deploy path a
//! command-line client drives — the execution role written with IAM's own operations, the code
//! uploaded as a .zip, the provisioning window waited out by reading the function's own state,
//! then `Invoke`.
//!
//! RULE: an `Invoke` of an active function whose `Runtime` names a language this emulator
//! embeds an engine for RUNS that function's stored deployment package and answers HTTP 200
//! carrying the document the handler itself returned, with no `X-Amz-Function-Error` for a
//! handler that returned — whatever the handler computed that document from. The invocation is
//! never refused on the grounds that the package cannot be run: the operation's model
//! enumerates the failures `Invoke` may answer
//! (`smithy_query(query="operation",operation="Invoke")`) and none of them reports that the
//! service cannot run a package it accepted, so a refusal of a runnable invocation is an answer
//! no client can classify.
//!
//! The rows differ along what the rule generalises over: WHICH source class the handler's
//! answer is computed from — the event payload alone (twice, over payloads differing in how
//! many numbers they carry and in their order), the run's own invocation state (the context
//! object and the runtime environment, which is how a handler reports its own configuration),
//! another service's state read through the AWS SDK the runtime supplies (a verified sending
//! domain and a verified address identity, which report different `IdentityType`s), and that
//! same service's REFUSAL of such a read (an identity the account holds nothing under, whose
//! modelled error reaches the handler and is still answered as a completed run). The identity
//! rows are asserted twice: once on what the handler read back, and once on what SES v2 itself
//! answers a client for the same identity, so the handler's answer is the service's state
//! rather than anything this suite put in the payload.
//!
//! What the sibling suites pin, and this one does not repeat: the deploy path's own steps are
//! [`crate::cli_deployed_email_sender`]'s and are DRIVEN from there rather than copied;
//! `handler_email_sdk.rs` drives the table of PROTOCOL BINDING classes a handler's SES v2 calls
//! take; `cli_deployed_sender_outcome_classes.rs` drives the OUTCOME classes one deployed
//! sender's invocation settles on; `synchronous_invoke_run.rs` pins the response headers of an
//! invocation (`X-Amz-Executed-Version`, the function-error signal) and the modelled refusal of
//! a `Runtime` this emulator embeds no engine for; and `python_runtime_bundled_sdk.rs` drives
//! the table of `python3.x` identifiers. What this suite adds is the SOURCE-CLASS table around
//! one deployed function: every class of thing a handler's answer can be computed from comes
//! back as that handler's own run.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `state::execution::max_concurrent_runs` invocations at once for the whole process.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The whole command-line deploy path — the role IAM holds, the verified identities, the
// packaged upload, the wait for `Active` and the invocation that answers terminally — belongs
// to the sibling suite, so this one drives those steps rather than a second copy of them.
use crate::cli_deployed_email_sender::{
    email, emulator, given_active_function, given_execution_role, given_verified_identity, invoke,
};
// The statement builders every execution role in this target is written with.
use crate::event_source_mapping_role_access::{allow, logs_only};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The Region the shared deploy path signs every request for, which is therefore the Region the
/// function is created in, the Region an invocation of it is addressed to, and the Region its
/// own SDK calls in. Spelled here because it is what the assertions on the run's own reported
/// Region are made against.
const REGION: &str = "us-east-1";

/// The managed runtime the shared deploy path creates its function with, which is the
/// identifier `AWS_EXECUTION_ENV` states for the run.
const RUNTIME: &str = "python3.12";

/// The `Handler` that same path configures: the module `index` and its attribute `handler`,
/// which is what `_HANDLER` names for the run.
const HANDLER: &str = "index.handler";

/// The execution role the function runs as, and the function itself.
const ROLE_NAME: &str = "cli-deployed-answer-sources";
const FUNCTION: &str = "cli-deployed-answer-sources";

/// The `MemorySize` a create that names none leaves the function with: "The default value is
/// 128 MB", as AWS documents the member (the model carries only its 128..32768 range), and the
/// shared deploy path names none.
const DEFAULT_MEMORY_MB: i64 = 128;

/// The version an invocation that names no qualifier resolves to, which is what the run reports
/// as its own (`InvocationResponse$ExecutedVersion`, whose `Version` pattern admits it).
const LATEST: &str = "$LATEST";

/// The identities the account verifies: a sending DOMAIN, and one ADDRESS identity in another
/// domain — the two identity classes a read of them reports differently.
const VERIFIED_DOMAIN: &str = "answers.example.com";
const VERIFIED_ADDRESS: &str = "reader@answers.example.net";

/// A name the account holds no identity under at all, which is the refused read.
const ABSENT_IDENTITY: &str = "nobody@absent.example.org";

/// The `IdentityType` SES v2 reports for each of those classes
/// (`smithy_query(query="shape",shape="IdentityType")`: `DOMAIN | EMAIL_ADDRESS |
/// MANAGED_DOMAIN`).
const DOMAIN_IDENTITY_TYPE: &str = "DOMAIN";
const ADDRESS_IDENTITY_TYPE: &str = "EMAIL_ADDRESS";

/// The status SES v2's model binds `GetEmailIdentity`'s success to, and the error code and
/// status it binds a read of an identity the account does not hold to
/// (`smithy_query(query="operation",operation="GetEmailIdentity")`: 200, and
/// `NotFoundException` at 404).
const READ_SUCCESS_STATUS: i64 = 200;
const NOT_FOUND: &str = "NotFoundException";
const NOT_FOUND_STATUS: i64 = 404;

/// The log destination a function that configures none writes to: "By default, Lambda functions
/// send logs to a default log group named `/aws/lambda/<function name>`" (the model's own
/// `LoggingConfig$LogGroup` documentation), which is what the run reports as its own log group.
fn expected_log_group(function: &str) -> String {
    format!("/aws/lambda/{function}")
}

/// The ARN an unqualified invocation of `function` addresses, which is what the run reports as
/// `context.invoked_function_arn`: the function ARN form of this account and Region.
fn expected_function_arn(function: &str) -> String {
    format!(
        "arn:aws:lambda:{REGION}:{}:function:{function}",
        engine::emulator_account_id()
    )
}

/// How SES v2 words `NotFoundException` for an identity it does not hold on the reads that
/// QUOTE the identity: fixed text around the one variable part, the `EmailIdentity` the request
/// named, which the service delimits with angle brackets.
///
/// Spelled here rather than read from the service's own helper
/// (`service-com-amazonaws-sesv2-simpleemailservice-v2/src/state.rs`,
/// `email_identity_not_found`), so the wording is pinned independently of the code under test
/// and the two have to be corrected together.
fn expected_identity_not_found(identity: &str) -> String {
    format!("Email identity <{identity}> does not exist.")
}

/// The handler the deployed function runs: one entry point whose answer is computed from
/// whichever source the event names — the payload itself, the run's own invocation state, or
/// the email service's answer to a read the SDK makes — so the response payload IS what the
/// handler produced in each case.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3
import botocore.exceptions


def from_the_event(event):
    numbers = event["numbers"]
    return {
        "total": sum(numbers),
        "largest": max(numbers),
        "spelled": ",".join(str(number) for number in numbers),
        "label": event["label"].upper(),
    }


def from_its_own_state(context):
    return {
        "functionName": context.function_name,
        "version": context.function_version,
        "memory": int(context.memory_limit_in_mb),
        "arn": context.invoked_function_arn,
        "logGroup": context.log_group_name,
        "environmentName": os.environ["AWS_LAMBDA_FUNCTION_NAME"],
        "environmentVersion": os.environ["AWS_LAMBDA_FUNCTION_VERSION"],
        "environmentMemory": int(os.environ["AWS_LAMBDA_FUNCTION_MEMORY_SIZE"]),
        "executionEnv": os.environ["AWS_EXECUTION_ENV"],
        "region": os.environ["AWS_REGION"],
        "handler": os.environ["_HANDLER"],
    }


def from_the_email_service(identity):
    client = boto3.client("sesv2")
    try:
        answered = client.get_email_identity(EmailIdentity=identity)
    except botocore.exceptions.ClientError as error:
        return {
            "read": False,
            "errorCode": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "read": True,
        "identityType": answered["IdentityType"],
        "verifiedForSending": answered["VerifiedForSendingStatus"],
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
    }


def handler(event, context):
    source = event["source"]
    if source == "event":
        return from_the_event(event)
    if source == "configuration":
        return from_its_own_state(context)
    if source == "identity":
        return from_the_email_service(event["identity"])
    raise RuntimeError("the event named no source this handler computes from: %s"
                       % json.dumps(event))
"#;

/// WHERE one row's handler computes its answer from.
enum Source {
    /// The event payload alone: the numbers it carries and the label it names.
    Event {
        numbers: Vec<i64>,
        label: &'static str,
    },
    /// The run's own invocation state: the context object and the runtime environment, which is
    /// how a handler reports its own configuration.
    OwnConfiguration,
    /// Another service's state, read through the SDK the runtime supplies: the identity the
    /// account holds under this name, which that service reports under this `IdentityType`.
    Identity {
        identity: &'static str,
        identity_type: &'static str,
    },
    /// That same service's refusal of such a read: a name the account holds no identity under.
    AbsentIdentity { identity: &'static str },
}

/// One row: which source class the handler's answer is computed from.
struct Row {
    case: &'static str,
    source: Source,
}

/// The rows, in the order they run: one per source class, with two payload-only rows that
/// differ in how many numbers the payload carries and in their order.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the event payload alone",
            source: Source::Event {
                numbers: vec![4, 9, 2],
                label: "deploy",
            },
        },
        Row {
            case: "the event payload alone, one number and a different order",
            source: Source::Event {
                numbers: vec![31, 7],
                label: "second pass",
            },
        },
        Row {
            case: "the run's own invocation state",
            source: Source::OwnConfiguration,
        },
        Row {
            case: "another service's state: a verified sending domain",
            source: Source::Identity {
                identity: VERIFIED_DOMAIN,
                identity_type: DOMAIN_IDENTITY_TYPE,
            },
        },
        Row {
            case: "another service's state: a verified address identity",
            source: Source::Identity {
                identity: VERIFIED_ADDRESS,
                identity_type: ADDRESS_IDENTITY_TYPE,
            },
        },
        Row {
            case: "another service's refusal: an identity the account does not hold",
            source: Source::AbsentIdentity {
                identity: ABSENT_IDENTITY,
            },
        },
    ]
}

/// The event one row hands its handler: the source it names, and whatever that source needs.
fn event(row: &Row) -> Value {
    match &row.source {
        Source::Event { numbers, label } => json!({
            "source": "event",
            "numbers": numbers,
            "label": label,
        }),
        Source::OwnConfiguration => json!({ "source": "configuration" }),
        Source::Identity { identity, .. } => json!({
            "source": "identity",
            "identity": identity,
        }),
        Source::AbsentIdentity { identity } => json!({
            "source": "identity",
            "identity": identity,
        }),
    }
}

/// What SES v2 answers a CLIENT asking for `identity`, which is the state the handler's own
/// read was answered from.
async fn identity_read(router: &MultiServiceRouter, case: &str, identity: &str) -> Value {
    email(
        router,
        case,
        "GET",
        &format!("/v2/email/identities/{identity}"),
        &[],
        200,
    )
    .await
}

/// One invocation per source class, each asserted on the document the handler returned — and
/// the identity rows asserted again against what the email service itself answers.
#[tokio::test]
async fn every_invocation_of_a_deployed_function_answers_what_its_handler_computed() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();

    let role_arn = given_execution_role(
        &router,
        ROLE_NAME,
        json!([logs_only(), allow("ses", &["GetEmailIdentity"], "*")]),
    )
    .await;
    given_verified_identity(&router, VERIFIED_DOMAIN).await;
    given_verified_identity(&router, VERIFIED_ADDRESS).await;
    given_active_function(&router, FUNCTION, HANDLER_SOURCE, &role_arn).await;

    // What each identity row's handler reported about the identity it read, which the closing
    // block holds against the service's own answer for the same identity.
    let mut reported: Vec<(&str, String)> = Vec::new();

    for row in rows() {
        let case = row.case;
        let (signal, payload) = invoke(&router, case, FUNCTION, &event(&row)).await;

        // Whatever the handler computed its answer from, the RUN is what answered: the
        // invocation carries the handler's own document and no function error.
        assert_eq!(
            signal, None,
            "case {case:?}: the invocation must carry no function error, got {signal:?} with \
             {payload}"
        );
        assert!(
            payload.is_object(),
            "case {case:?}: expected the handler's own document, got {payload}"
        );

        match &row.source {
            Source::Event { numbers, label } => {
                let total: i64 = numbers.iter().sum();
                let largest = numbers.iter().copied().max().unwrap_or_default();
                let spelled = numbers
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<String>>()
                    .join(",");
                assert_eq!(
                    payload["total"],
                    json!(total),
                    "case {case:?}: expected the sum of the payload's numbers, got {payload}"
                );
                assert_eq!(
                    payload["largest"],
                    json!(largest),
                    "case {case:?}: expected the largest of the payload's numbers, got {payload}"
                );
                assert_eq!(
                    payload["spelled"],
                    json!(spelled),
                    "case {case:?}: expected the payload's numbers in the order they were sent, \
                     got {payload}"
                );
                assert_eq!(
                    payload["label"],
                    json!(label.to_uppercase()),
                    "case {case:?}: expected the payload's label upper-cased, got {payload}"
                );
            }
            Source::OwnConfiguration => {
                assert_eq!(
                    payload["functionName"],
                    json!(FUNCTION),
                    "case {case:?}: expected the function's own name, got {payload}"
                );
                assert_eq!(
                    payload["version"],
                    json!(LATEST),
                    "case {case:?}: expected the version an unqualified call resolves to, got \
                     {payload}"
                );
                assert_eq!(
                    payload["memory"],
                    json!(DEFAULT_MEMORY_MB),
                    "case {case:?}: expected the MemorySize the create left the function with, \
                     got {payload}"
                );
                assert_eq!(
                    payload["arn"],
                    json!(expected_function_arn(FUNCTION)),
                    "case {case:?}: expected the ARN the call addressed, got {payload}"
                );
                assert_eq!(
                    payload["logGroup"],
                    json!(expected_log_group(FUNCTION)),
                    "case {case:?}: expected the default log group of this function, got {payload}"
                );
                assert_eq!(
                    payload["executionEnv"],
                    json!(format!("AWS_Lambda_{RUNTIME}")),
                    "case {case:?}: expected the runtime the function was created with, got \
                     {payload}"
                );
                assert_eq!(
                    payload["region"],
                    json!(REGION),
                    "case {case:?}: expected the Region the call was addressed to, got {payload}"
                );
                assert_eq!(
                    payload["handler"],
                    json!(HANDLER),
                    "case {case:?}: expected the Handler the function was created with, got \
                     {payload}"
                );
                // The two surfaces a handler reads its own configuration through agree: the
                // context object and the environment report one function, not two.
                assert_eq!(
                    payload["environmentName"], payload["functionName"],
                    "case {case:?}: the environment and the context must name one function, got \
                     {payload}"
                );
                assert_eq!(
                    payload["environmentVersion"], payload["version"],
                    "case {case:?}: the environment and the context must name one version, got \
                     {payload}"
                );
                assert_eq!(
                    payload["environmentMemory"], payload["memory"],
                    "case {case:?}: the environment and the context must report one memory size, \
                     got {payload}"
                );
            }
            Source::Identity {
                identity,
                identity_type,
            } => {
                assert_eq!(
                    payload["read"],
                    json!(true),
                    "case {case:?}: expected the read to be served, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(READ_SUCCESS_STATUS),
                    "case {case:?}: expected the modelled success status, got {payload}"
                );
                assert_eq!(
                    payload["identityType"],
                    json!(identity_type),
                    "case {case:?}: expected the identity class the service holds, got {payload}"
                );
                assert_eq!(
                    payload["verifiedForSending"],
                    json!(true),
                    "case {case:?}: expected the verification the service holds, got {payload}"
                );
                reported.push((
                    identity,
                    payload["identityType"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ));
            }
            Source::AbsentIdentity { identity } => {
                assert_eq!(
                    payload["read"],
                    json!(false),
                    "case {case:?}: expected the service to refuse the read, got {payload}"
                );
                assert_eq!(
                    payload["errorCode"],
                    json!(NOT_FOUND),
                    "case {case:?}: expected the service's own error code, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(NOT_FOUND_STATUS),
                    "case {case:?}: expected the status the model binds {NOT_FOUND} to, got \
                     {payload}"
                );
                assert_eq!(
                    payload["message"],
                    json!(expected_identity_not_found(identity)),
                    "case {case:?}: expected the service's own refusal sentence, got {payload}"
                );
            }
        }
    }

    // The other half of the identity rows: what the handler read is what the service answers a
    // client for the same identity, so the payload came out of that service's state.
    let expected_identities = rows()
        .iter()
        .filter(|row| matches!(row.source, Source::Identity { .. }))
        .count();
    assert_eq!(
        reported.len(),
        expected_identities,
        "expected one read per identity row, got {reported:?}"
    );
    for (identity, identity_type) in &reported {
        let case = format!("the service's own answer for {identity}");
        let read = identity_read(&router, &case, identity).await;
        assert_eq!(
            read["IdentityType"],
            json!(identity_type),
            "case {case:?}: expected the class the handler read back, got {read}"
        );
        assert_eq!(
            read["VerifiedForSendingStatus"],
            json!(true),
            "case {case:?}: expected the identity the handler read to be verified, got {read}"
        );
    }

    // And the other half of the refused row: the same read made by a CLIENT is refused the same
    // way, so what reached the handler was that service's own answer rather than anything this
    // emulator's runtime made up for it — and the refused read left no identity behind.
    let case = format!("the service's own answer for {ABSENT_IDENTITY}");
    let refused = email(
        &router,
        &case,
        "GET",
        &format!("/v2/email/identities/{ABSENT_IDENTITY}"),
        &[],
        NOT_FOUND_STATUS
            .try_into()
            .expect("the modelled status fits a status code"),
    )
    .await;
    let sentence = refused["message"]
        .as_str()
        .or_else(|| refused["Message"].as_str())
        .unwrap_or_default();
    assert_eq!(
        sentence,
        expected_identity_not_found(ABSENT_IDENTITY),
        "case {case:?}: expected the sentence the handler read back, got {refused}"
    );
}
