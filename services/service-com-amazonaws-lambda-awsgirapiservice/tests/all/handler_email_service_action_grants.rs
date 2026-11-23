// SPDX-License-Identifier: CC0-1.0

//! WHAT A RUNNING HANDLER'S Amazon SES API v2 CALLS SETTLE ON, action by action: AWS Lambda,
//! Amazon SES API v2, IAM and the token service on ONE engine, a `python3.x` function whose
//! code does `import boto3` and builds a client for the email service and one for the token
//! service, and what each call read back where a client reads it.
//!
//! RULE: a managed runtime's bundled SDK builds a client for any service this emulator serves,
//! and every call such a client makes is decided by the IAM documents in force on the
//! function's EXECUTION ROLE before the service reads or writes anything — a call a document
//! in force allows is served with that operation's own modelled output, and one no document in
//! force allows is refused HTTP 403 under the error code `AccessDeniedException` carrying IAM's
//! own sentence: the session the run is signed as, the `<namespace>:<OperationName>` action it
//! asked for, the resource the request acts on, and the clause naming what withheld the grant.
//! So a handler can always tell a missing permission from the service's own refusals, and never
//! reads back a client-side report that the SDK knows no such service or a failure carrying no
//! error code at all.
//!
//! The rows differ along what the rule generalises over: WHICH operation of the service the
//! handler calls and so which action is decided — a create naming the resource in its own
//! input, a create of another resource family, a read addressed by one path label, a write
//! under a deeper path of one resource, a write over another family's resource, a tagging whose
//! target is an ARN in the request body, an account-level write, a read taking no input, a
//! listing bound to its own URI, and a delete — and WHICH role the run is made under: one whose
//! policy allows exactly those actions on every resource, one that allows only the log writes a
//! function makes, and one that allows the same actions on ONE resource alone, which is what
//! separates the action half of the decision from the resource half. Every served row is
//! asserted on both sides: what the guest read back, and what SES v2 itself holds, read through
//! its own API — and the refused run is asserted to have left the account exactly as it was.
//!
//! The deploy path (the role IAM holds, the packaged upload, the wait for `Active`, the
//! invocation that answers terminally) is [`crate::cli_deployed_email_sender`]'s, so this suite
//! drives those steps rather than a second copy of them. What the sibling suites pin and this
//! one does not repeat: `handler_email_sdk.rs` drives the BINDING CLASSES of a handler's SES v2
//! calls, `handler_sdk_service_inventory.rs` which services a handler's SDK can build a client
//! for at all, and `cli_deployed_sender_outcome_classes.rs` the outcome classes one send
//! settles on.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `state::execution::max_concurrent_runs` invocations at once for the whole process.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The whole deploy path — the role IAM holds, the packaged upload, the wait for `Active` and the
// invocation that answers terminally — belongs to the suite that first needed it.
use crate::cli_deployed_email_sender::{
    email, emulator, given_active_function, given_execution_role, invoke,
};
// The statement builders every role in this target is written with.
use crate::event_source_mapping_role_access::{allow, logs_only};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The Region the suite signs for, which is the Region the handler's own clients call in and so
/// the one the ARNs the decisions are taken against carry.
const REGION: &str = "us-east-1";

/// The action namespace this service's operations are published under, which a policy statement
/// and a refusal both spell (`smithy_query(query="service_info")`: `arnNamespace` and
/// `sigv4Name` alike are `ses`).
const ACTION_NAMESPACE: &str = "ses";

/// The error code the IAM layer in front of the service refuses a call no identity-based policy
/// in force allows with, and the status such a refusal carries. It is not one of this model's
/// own errors: the decision is taken ahead of the service, as it is on AWS.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The status every one of these operations binds its success to
/// (`smithy_query(query="operation")`: `code` 200 for each).
const SUCCESS_STATUS: i64 = 200;

/// The roles the runs are made under: one allowing exactly the actions the calls perform, one
/// allowing only the log writes a function makes, and one allowing those same actions on ONE
/// resource alone.
const ALLOWING_ROLE: &str = "handler-ses-grants-allowing";
const LOGS_ONLY_ROLE: &str = "handler-ses-grants-logs-only";
const SCOPED_ROLE: &str = "handler-ses-grants-scoped";

/// The function running as each of those roles.
const ALLOWING_FUNCTION: &str = "handler-ses-grants";
const LOGS_ONLY_FUNCTION: &str = "handler-ses-grants-no-grant";
const SCOPED_FUNCTION: &str = "handler-ses-grants-one-resource";

/// The tag the tagging row writes, whose target is the ARN in the request's body.
const TAG_KEY: &str = "owner";
const TAG_VALUE: &str = "the-handler";

/// The suppression reason the account-level write asks for, one of the model's
/// `SuppressionListReason` values (`smithy_query(query="shape",shape="SuppressionListReason")`:
/// `BOUNCE`, `COMPLAINT`). It is ONE of them, so the write it asks for differs from the account's
/// own standing configuration and a read tells the two apart.
const SUPPRESSED_REASON: &str = "BOUNCE";

/// The behaviour the custom MAIL FROM write asks for, one of the model's
/// `BehaviorOnMxFailure` values
/// (`smithy_query(query="shape",shape="BehaviorOnMxFailure")`: `USE_DEFAULT_VALUE`,
/// `REJECT_MESSAGE`).
const MX_FAILURE_BEHAVIOR: &str = "USE_DEFAULT_VALUE";

/// The names one run's calls address: its own domain identity, its own address identity and its
/// own configuration set, so what one run did and what another was refused are told apart by
/// the state the service holds rather than by this suite's bookkeeping.
struct Names {
    domain: &'static str,
    address: &'static str,
    set: &'static str,
}

/// The ARN SES mints for the email identity `identity` of this account, in the Region the call
/// was signed for: the resource an identity-scoped request acts on.
///
/// Spelled here rather than read from the service's own gate, so the form is pinned
/// independently of the code under test.
fn identity_arn(identity: &str) -> String {
    format!(
        "arn:aws:{ACTION_NAMESPACE}:{REGION}:{}:identity/{identity}",
        engine::emulator_account_id()
    )
}

/// The same for a configuration set.
fn configuration_set_arn(name: &str) -> String {
    format!(
        "arn:aws:{ACTION_NAMESPACE}:{REGION}:{}:configuration-set/{name}",
        engine::emulator_account_id()
    )
}

/// IAM's any-resource wildcard, which is what a request acting on no single resource of its own
/// is decided against.
const EVERY_RESOURCE: &str = "*";

/// How IAM words the refusal of a call no identity-based policy in force allows: the principal
/// the call was signed as — the session of the function's own execution role, which is what
/// `sts:GetCallerIdentity` reports to the same handler — the action it asked to perform, the
/// resource it asked to perform it on, and the clause naming IAM's default deny.
///
/// Spelled here rather than read from the evaluator under test, so the wording is pinned
/// independently of it and the two have to be corrected together.
fn expected_denial(role: &str, function: &str, operation: &str, resource: &str) -> String {
    let action = format!("{ACTION_NAMESPACE}:{operation}");
    format!(
        "User: {} is not authorized to perform: {action} on resource: {resource} because no \
         identity-based policy allows the {action} action",
        session_arn(role, function)
    )
}

/// The assumed-role session a call made by an invocation of `function` running as `role` is
/// signed as, as `sts:GetCallerIdentity` reports it.
fn session_arn(role: &str, function: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        engine::emulator_account_id()
    )
}

/// The handler every run executes: it builds a client for the email service and one for the
/// token service at module scope, reports the identity the run is signed as, then makes each
/// call the event names in the order the event names them — reporting, per call, the operation's
/// own output members, or the error code, message and status of the refusal it got, or the
/// exception that stopped the call being made at all. So the invocation's payload IS what the
/// SDK gave the guest.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

SES = boto3.client("sesv2")
IDENTITIES = boto3.client("sts")


def _served(answered, read):
    return {
        "served": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "members": sorted(key for key in answered if key != "ResponseMetadata"),
        # Text, because a member the service answers as a timestamp is no JSON value.
        "read": {name: str(answered.get(name)) for name in read},
    }


def _refused(error):
    return {
        "served": False,
        "code": error.response["Error"]["Code"],
        "message": error.response["Error"]["Message"],
        "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
    }


def handler(event, context):
    made = {}
    for call in event["calls"]:
        try:
            method = getattr(SES, call["method"])
            answered = method(**call["parameters"])
        except botocore.exceptions.ClientError as error:
            made[call["case"]] = _refused(error)
        except Exception as error:
            made[call["case"]] = {
                "served": False,
                "not_made": "%s: %s" % (type(error).__name__, error),
            }
        else:
            made[call["case"]] = _served(answered, call["read"])
    return {"identity": IDENTITIES.get_caller_identity()["Arn"], "made": made}
"#;

/// One call the handler makes: the client method, the operation it addresses, the keyword
/// arguments it is called with, the resource that request acts on, and what a served answer
/// must carry.
struct Call {
    case: &'static str,
    /// The client method, which the SDK turns into the operation below.
    method: &'static str,
    /// The operation's own name, which the IAM action is formed from.
    operation: &'static str,
    /// The operation's input document, as the keyword arguments of that method.
    parameters: fn(&Names) -> Value,
    /// The ARN the request ACTS ON, which the decision is taken against.
    resource: fn(&Names) -> String,
    /// The output members a served answer must carry; empty for an operation whose own output
    /// shape declares none, where the answer must carry no member at all.
    members: &'static [&'static str],
    /// The output members the handler reports as text, each with what it must read.
    read: &'static [(&'static str, &'static str)],
}

/// Every call the runs below make, in the order a run makes them: each one's state is the state
/// the next reads, so what is asserted is what the service answered rather than what this suite
/// assumed.
///
/// Every operation named here is one this model binds, with the URI the resource is read off
/// (`smithy_query(query="operation",operation="CreateEmailIdentity,CreateConfigurationSet,\
/// GetEmailIdentity,PutEmailIdentityMailFromAttributes,PutConfigurationSetSendingOptions,\
/// TagResource,PutAccountSuppressionAttributes,GetAccount,ListEmailIdentities,\
/// DeleteEmailIdentity")`), and the member list each served row expects is that operation's own
/// output shape's.
fn calls() -> Vec<Call> {
    vec![
        Call {
            case: "a create naming the resource in its own input",
            method: "create_email_identity",
            operation: "CreateEmailIdentity",
            parameters: |names| json!({ "EmailIdentity": names.domain }),
            resource: |names| identity_arn(names.domain),
            members: &[
                "DkimAttributes",
                "IdentityType",
                "VerifiedForSendingStatus",
            ],
            read: &[("IdentityType", "DOMAIN")],
        },
        Call {
            case: "the same create for an identity whose name carries an at sign",
            method: "create_email_identity",
            operation: "CreateEmailIdentity",
            parameters: |names| json!({ "EmailIdentity": names.address }),
            resource: |names| identity_arn(names.address),
            members: &[
                "DkimAttributes",
                "IdentityType",
                "VerifiedForSendingStatus",
            ],
            read: &[("IdentityType", "EMAIL_ADDRESS")],
        },
        Call {
            case: "a create of another resource family",
            method: "create_configuration_set",
            operation: "CreateConfigurationSet",
            parameters: |names| json!({ "ConfigurationSetName": names.set }),
            resource: |names| configuration_set_arn(names.set),
            members: &[],
            read: &[],
        },
        Call {
            case: "a read addressed by one path label",
            method: "get_email_identity",
            operation: "GetEmailIdentity",
            parameters: |names| json!({ "EmailIdentity": names.domain }),
            resource: |names| identity_arn(names.domain),
            members: &["IdentityType", "VerificationStatus"],
            read: &[("IdentityType", "DOMAIN")],
        },
        Call {
            case: "a write under a deeper path of one resource",
            method: "put_email_identity_mail_from_attributes",
            operation: "PutEmailIdentityMailFromAttributes",
            parameters: |names| {
                json!({
                    "EmailIdentity": names.domain,
                    "MailFromDomain": format!("mail.{}", names.domain),
                    "BehaviorOnMxFailure": MX_FAILURE_BEHAVIOR,
                })
            },
            resource: |names| identity_arn(names.domain),
            members: &[],
            read: &[],
        },
        Call {
            case: "a write over another family's resource",
            method: "put_configuration_set_sending_options",
            operation: "PutConfigurationSetSendingOptions",
            parameters: |names| {
                json!({ "ConfigurationSetName": names.set, "SendingEnabled": true })
            },
            resource: |names| configuration_set_arn(names.set),
            members: &[],
            read: &[],
        },
        Call {
            case: "a tagging whose target is an ARN in the request body",
            method: "tag_resource",
            operation: "TagResource",
            parameters: |names| {
                json!({
                    "ResourceArn": identity_arn(names.domain),
                    "Tags": [{ "Key": TAG_KEY, "Value": TAG_VALUE }],
                })
            },
            // The target is an ARN in the BODY rather than a resource the URI addresses, so
            // the request acts on no single resource of this model's own URI families and the
            // decision falls on IAM's any-resource wildcard. WHETHER a body-carried target
            // should narrow the resource the action is decided against is the authorization
            // layer's own question, not this runtime's: what this row pins is that the handler
            // reads back the service's decision with its code, status and whole sentence, which
            // is what this suite is about.
            resource: |_names| EVERY_RESOURCE.to_string(),
            members: &[],
            read: &[],
        },
        Call {
            case: "an account-level write",
            method: "put_account_suppression_attributes",
            operation: "PutAccountSuppressionAttributes",
            parameters: |_names| json!({ "SuppressedReasons": [SUPPRESSED_REASON] }),
            resource: |_names| EVERY_RESOURCE.to_string(),
            members: &[],
            read: &[],
        },
        Call {
            case: "a read taking no input at all",
            method: "get_account",
            operation: "GetAccount",
            parameters: |_names| json!({}),
            resource: |_names| EVERY_RESOURCE.to_string(),
            members: &["SendQuota", "SendingEnabled", "SuppressionAttributes"],
            read: &[],
        },
        Call {
            case: "a listing bound to its own URI",
            method: "list_email_identities",
            operation: "ListEmailIdentities",
            parameters: |_names| json!({}),
            resource: |_names| EVERY_RESOURCE.to_string(),
            members: &["EmailIdentities"],
            read: &[],
        },
        Call {
            case: "a delete addressed by one path label",
            method: "delete_email_identity",
            operation: "DeleteEmailIdentity",
            parameters: |names| json!({ "EmailIdentity": names.address }),
            resource: |names| identity_arn(names.address),
            members: &[],
            read: &[],
        },
    ]
}

/// The call named `case`, which every run's plan names its calls by.
fn call_named(case: &str) -> Call {
    calls()
        .into_iter()
        .find(|call| call.case == case)
        .unwrap_or_else(|| panic!("the plans name only calls this suite declares, not {case:?}"))
}

/// Whether the role in force on a run lets one of its calls through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Allowed {
    /// The documents in force allow the action on that resource: the service serves the call.
    Served,
    /// They do not: the call is refused ahead of the service.
    Refused,
}

/// One run: the function it invokes, the role that function runs as, the names its calls
/// address, and which of this suite's calls it makes with what must come of each.
struct Run {
    what: &'static str,
    function: &'static str,
    role: &'static str,
    names: Names,
    made: Vec<(&'static str, Allowed)>,
}

/// Every call of this suite, in order, with the same verdict for each.
fn every_call(verdict: Allowed) -> Vec<(&'static str, Allowed)> {
    calls()
        .iter()
        .map(|call| (call.case, verdict))
        .collect()
}

/// The event one run hands its handler: the calls it names, in order, each with its own input.
fn event(run: &Run) -> Value {
    let calls: Vec<Value> = run
        .made
        .iter()
        .map(|(case, _verdict)| {
            let call = call_named(case);
            json!({
                "case": call.case,
                "method": call.method,
                "parameters": (call.parameters)(&run.names),
                "read": call.read.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "calls": calls })
}

/// Assert what one run's handler reported, call by call.
fn assert_run(run: &Run, payload: &Value) {
    let what = run.what;
    // The SDK reached the services rather than failing to build a client for either of them, so
    // the identity it reported is the session the run is signed as.
    assert_eq!(
        payload["identity"],
        json!(session_arn(run.role, run.function)),
        "case {what:?}: expected the run's own assumed-role session, got {payload}"
    );
    for (case, verdict) in &run.made {
        let call = call_named(case);
        let reported = &payload["made"][case];
        let resource = (call.resource)(&run.names);
        assert!(
            reported.is_object(),
            "case {what:?}/{case:?}: expected the handler to report the call, got {reported}"
        );
        // Whatever came of it, the call WAS made: a client-side failure of the SDK itself — a
        // service it cannot build a client for, a method it does not carry — is not an answer
        // from the service.
        assert_eq!(
            reported["not_made"],
            Value::Null,
            "case {what:?}/{case:?}: expected the SDK to make the call, got {reported}"
        );
        match verdict {
            Allowed::Served => {
                assert_eq!(
                    reported["served"],
                    json!(true),
                    "case {what:?}/{case:?}: expected the call to be served, got {reported}"
                );
                assert_eq!(
                    reported["status"],
                    json!(SUCCESS_STATUS),
                    "case {what:?}/{case:?}: expected the modelled success status, got {reported}"
                );
                for member in call.members {
                    assert!(
                        reported["members"]
                            .as_array()
                            .is_some_and(|members| members.contains(&json!(member))),
                        "case {what:?}/{case:?}: expected the output member {member:?}, got \
                         {reported}"
                    );
                }
                if call.members.is_empty() {
                    assert_eq!(
                        reported["members"],
                        json!([]),
                        "case {what:?}/{case:?}: expected an answer carrying no output member, \
                         got {reported}"
                    );
                }
                for (member, expected) in call.read {
                    assert_eq!(
                        reported["read"][member],
                        json!(expected),
                        "case {what:?}/{case:?}: expected {member:?} to read {expected:?}, got \
                         {reported}"
                    );
                }
            }
            Allowed::Refused => {
                assert_eq!(
                    reported["served"],
                    json!(false),
                    "case {what:?}/{case:?}: expected the call to be refused, got {reported}"
                );
                // A refusal always carries a real AWS error code, which is what tells a missing
                // permission from the service's own refusals.
                assert_eq!(
                    reported["code"],
                    json!(ACCESS_DENIED),
                    "case {what:?}/{case:?}: expected the IAM layer's own error code, got \
                     {reported}"
                );
                assert_eq!(
                    reported["status"],
                    json!(ACCESS_DENIED_STATUS),
                    "case {what:?}/{case:?}: expected the status of that refusal, got {reported}"
                );
                assert_eq!(
                    reported["message"],
                    json!(expected_denial(
                        run.role,
                        run.function,
                        call.operation,
                        &resource
                    )),
                    "case {what:?}/{case:?}: expected IAM's own sentence, got {reported}"
                );
            }
        }
    }
}

/// Read the email identity `identity` back through SES v2's own API, at the status the read must
/// answer.
async fn read_identity(
    router: &MultiServiceRouter,
    what: &str,
    identity: &str,
    status: u16,
) -> Value {
    email(
        router,
        what,
        "GET",
        &format!("/v2/email/identities/{identity}"),
        &[],
        status,
    )
    .await
}

/// Read the configuration set `name` back through the same API.
async fn read_configuration_set(
    router: &MultiServiceRouter,
    what: &str,
    name: &str,
    status: u16,
) -> Value {
    email(
        router,
        what,
        "GET",
        &format!("/v2/email/configuration-sets/{name}"),
        &[],
        status,
    )
    .await
}

/// The account's own view, which is where the account-level write shows.
async fn read_account(router: &MultiServiceRouter, what: &str) -> Value {
    email(router, what, "GET", "/v2/email/account", &[], 200).await
}

/// One invocation per run: the refused run first, so what the allowed run then did is told from
/// the state the service holds rather than from this suite's bookkeeping.
#[tokio::test]
async fn a_handlers_email_service_calls_are_decided_by_its_execution_roles_documents() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();

    // Every action the calls below perform, which is exactly what the allowing role grants.
    let mut actions: Vec<&str> = calls().iter().map(|call| call.operation).collect();
    actions.sort_unstable();
    actions.dedup();

    let scoped_names = Names {
        domain: "scoped.grants.example.com",
        address: "scoped-probe@grants.example.net",
        set: "handler-ses-grants-scoped-set",
    };
    let allowing_role = given_execution_role(
        &router,
        ALLOWING_ROLE,
        json!([logs_only(), allow(ACTION_NAMESPACE, &actions, EVERY_RESOURCE)]),
    )
    .await;
    let logs_only_role = given_execution_role(&router, LOGS_ONLY_ROLE, json!([logs_only()])).await;
    // The same actions as the allowing role, on ONE resource: the scoped run's own domain
    // identity. What it may do therefore differs from the allowing run's by the RESOURCE alone.
    let scoped_role = given_execution_role(
        &router,
        SCOPED_ROLE,
        json!([
            logs_only(),
            allow(
                ACTION_NAMESPACE,
                &actions,
                &identity_arn(scoped_names.domain)
            )
        ]),
    )
    .await;
    given_active_function(&router, ALLOWING_FUNCTION, HANDLER_SOURCE, &allowing_role).await;
    given_active_function(
        &router,
        LOGS_ONLY_FUNCTION,
        HANDLER_SOURCE,
        &logs_only_role,
    )
    .await;
    given_active_function(&router, SCOPED_FUNCTION, HANDLER_SOURCE, &scoped_role).await;

    // The account's own standing suppression configuration, before any run has asked for
    // another: what the refused run must leave it at.
    let baseline_suppression = read_account(&router, "the account before any run").await
        ["SuppressionAttributes"]["SuppressedReasons"]
        .clone();

    let runs = vec![
        Run {
            what: "a function whose execution role allows only the log writes",
            function: LOGS_ONLY_FUNCTION,
            role: LOGS_ONLY_ROLE,
            names: Names {
                domain: "refused.grants.example.com",
                address: "refused-probe@grants.example.net",
                set: "handler-ses-grants-refused-set",
            },
            made: every_call(Allowed::Refused),
        },
        Run {
            what: "a function whose execution role allows exactly these actions",
            function: ALLOWING_FUNCTION,
            role: ALLOWING_ROLE,
            names: Names {
                domain: "allowed.grants.example.com",
                address: "allowed-probe@grants.example.net",
                set: "handler-ses-grants-allowed-set",
            },
            made: every_call(Allowed::Served),
        },
        Run {
            what: "a function whose execution role allows those actions on one resource alone",
            function: SCOPED_FUNCTION,
            role: SCOPED_ROLE,
            names: scoped_names,
            // The identity its statement names is the identity it may act on; the same actions
            // over another family's resource are not allowed on that resource.
            made: vec![
                ("a create naming the resource in its own input", Allowed::Served),
                ("a read addressed by one path label", Allowed::Served),
                (
                    "a write under a deeper path of one resource",
                    Allowed::Served,
                ),
                ("a create of another resource family", Allowed::Refused),
                (
                    "a write over another family's resource",
                    Allowed::Refused,
                ),
            ],
        },
    ];

    for run in &runs {
        let (signal, payload) = invoke(&router, run.what, run.function, &event(run)).await;
        // However the calls went, the RUN completed: the handler's own document is the answer.
        assert_eq!(
            signal, None,
            "case {:?}: the invocation must carry no function error, got {signal:?} with \
             {payload}",
            run.what
        );
        assert_run(run, &payload);

        // The other half of the refused run: a call refused ahead of the service performed
        // nothing, so the account holds nothing of what it asked for.
        if run.function == LOGS_ONLY_FUNCTION {
            let what = "what the account holds after the refused run";
            read_identity(&router, what, run.names.domain, 404).await;
            read_identity(&router, what, run.names.address, 404).await;
            read_configuration_set(&router, what, run.names.set, 404).await;
            let account = read_account(&router, what).await;
            assert_eq!(
                account["SuppressionAttributes"]["SuppressedReasons"],
                baseline_suppression,
                "case {what:?}: expected the refused account write to have changed nothing, got \
                 {account}"
            );
        }
    }

    // What SES v2 itself holds once the allowed run has made its calls: the work the handler
    // reported is the work that happened.
    let what = "what the account holds after the allowed run";
    let allowed = &runs[1].names;
    let identity = read_identity(&router, what, allowed.domain, 200).await;
    assert_eq!(
        identity["MailFromAttributes"]["MailFromDomain"],
        json!(format!("mail.{}", allowed.domain)),
        "case {what:?}: expected the custom MAIL FROM domain the handler wrote, got {identity}"
    );
    assert_eq!(
        identity["Tags"],
        json!([{ "Key": TAG_KEY, "Value": TAG_VALUE }]),
        "case {what:?}: expected the tag the handler wrote, got {identity}"
    );
    // The delete the handler made is the delete the service took: the address identity is gone,
    // and the domain identity beside it is not.
    read_identity(&router, what, allowed.address, 404).await;
    let set = read_configuration_set(&router, what, allowed.set, 200).await;
    assert_eq!(
        set["SendingOptions"]["SendingEnabled"],
        json!(true),
        "case {what:?}: expected the sending switch the handler wrote, got {set}"
    );
    let account = read_account(&router, what).await;
    assert_eq!(
        account["SuppressionAttributes"]["SuppressedReasons"],
        json!([SUPPRESSED_REASON]),
        "case {what:?}: expected the suppression reason the handler wrote, got {account}"
    );
    assert_ne!(
        account["SuppressionAttributes"]["SuppressedReasons"],
        baseline_suppression,
        "case {what:?}: expected the handler's write to differ from the account's standing \
         configuration, got {account}"
    );

    // And the scoped run's own: the calls its statement covered took effect, and the ones it did
    // not cover left nothing behind.
    let what = "what the account holds after the scoped run";
    let scoped = &runs[2].names;
    let identity = read_identity(&router, what, scoped.domain, 200).await;
    assert_eq!(
        identity["MailFromAttributes"]["MailFromDomain"],
        json!(format!("mail.{}", scoped.domain)),
        "case {what:?}: expected the write its statement covered, got {identity}"
    );
    read_configuration_set(&router, what, scoped.set, 404).await;
}
