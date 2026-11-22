// SPDX-License-Identifier: CC0-1.0

//! WHICH of Lambda's two sentences a request is refused with when the function's execution
//! role is short of a grant.
//!
//! RULE: such a request is refused with the modelled `InvalidParameterValueException` (HTTP
//! 400), persisting nothing, and the sentence it carries is fixed text apart from the CALL
//! the role may not make and WHAT that call would have been made on; which of the two fixed
//! openings it carries follows from WHOSE call is withheld. A call Lambda makes on the running
//! FUNCTION's behalf — reading the event source a mapping polls, writing the record of a batch
//! that mapping gives up on — is refused "The function execution role does not have
//! permissions to call `<Call>` on `<On>`", and a call Lambda makes to stand a resource UP for
//! the function — the network interfaces an attachment to a VPC takes, a dead-letter target —
//! is refused "The provided execution role does not have permissions to call `<Call>` on
//! `<On>`". Neither sentence names the role, the function or the resource the request carried.
//!
//! The rows differ along what the rule generalises over: WHICH gate decides, and so which of
//! the two openings follows; WHICH call of that gate's set is the one withheld; WHICH service
//! that call would have been made to; and whether the role is granted it at all — the served
//! rows are the control, so a sentence is the answer to a withheld call and not to the request.
//!
//! The second test pins the discrimination itself on ONE withheld call: the same
//! `sqs:SendMessage` on the same queue, asked for by a gate of each kind in the same engine,
//! is worded by whose call it is and by nothing else. That pairing is what keeps the two
//! openings from drifting into each other, since a client matching on this error class's fixed
//! text is how "fix the execution role" is told from the other refusals carrying this one code.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with IAM
//! mounted beside Lambda on ONE engine — each role is created through the real `CreateRole`
//! and `PutRolePolicy`, so what each gate reads is the document IAM persisted — and reads the
//! outcome back through `GetEventSourceMapping`, `ListEventSourceMappings` and
//! `GetFunctionConfiguration`, so anything a refusal left behind fails here.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{
    allow, assert_refused, call_refusal, create_function_as, given_role, logs_only,
    router_with_iam, QUEUE_READS,
};
use crate::event_source_mappings::{
    call, create_mapping, create_request, get_mapping, list_mappings, Answer, QUEUE_SOURCE,
};

/// Where `CreateFunction` is bound, and `GetFunctionConfiguration` under it.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The IAM action namespace the Amazon SQS calls below are named in.
const SQS_NAMESPACE: &str = "sqs";

/// The same for the calls an attachment to a VPC takes.
const EC2_NAMESPACE: &str = "ec2";

/// The queue BOTH write gates name: the failure destination a mapping states, and the
/// dead-letter target a function states. One resource and one withheld write, so what the two
/// refusals differ in is only whose call Lambda would have been making.
const LETTER_QUEUE: &str = "arn:aws:sqs:us-east-1:000000000000:order-letters";

/// The write delivering a record to a queue takes, as a statement names it and as a refusal
/// names it.
const QUEUE_WRITE: &str = "SendMessage";

/// The service that write is made to, as a refusal names it.
const SQS: &str = "SQS";

/// The calls attaching a function to a VPC takes, in the order a refusal reports them.
/// Source: the actions AWS's own managed execution-role policy for VPC access
/// (`AWSLambdaVPCAccessExecutionRole`) grants.
const ENI_CALLS: [&str; 3] = [
    "CreateNetworkInterface",
    "DescribeNetworkInterfaces",
    "DeleteNetworkInterface",
];

/// The service those calls are made to, as a refusal names it.
const EC2: &str = "EC2";

/// This account's network interfaces in the Region the functions below are configured in,
/// which is the resource those calls are made on.
const INTERFACES: &str = "arn:aws:ec2:us-east-1:000000000000:network-interface/*";

/// The subnet an attaching `VpcConfig` names, and the security group applied to the interface
/// stood up in it.
const SUBNET: &str = "subnet-0a1b2c3d4e5f60718";
/// See [`SUBNET`].
const SECURITY_GROUP: &str = "sg-0a1b2c3d4e5f60718";

/// How a sentence about a call Lambda makes on the running function's behalf OPENS.
const FUNCTION_OPENING: &str = "The function execution role";

/// How a sentence about a call Lambda makes to stand a resource UP for the function OPENS.
const PROVIDED_OPENING: &str = "The provided execution role";

/// How AWS words the refusal of a role that may not make a call Lambda would have made to
/// STAND UP a resource for the function, written out here rather than imported from the
/// emulator so a wrong value there fails a row.
///
/// Fixed text apart from the call and what it would have been made on.
fn provided_role_sentence(action: &str, on: &str) -> String {
    format!("{PROVIDED_OPENING} does not have permissions to call {action} on {on}")
}

/// `CreateFunction` running as `role_arn`, with whatever `extras` states beside the members
/// every create needs.
async fn create_function_with(
    router: &MultiServiceRouter,
    name: &str,
    role_arn: &str,
    extras: &Value,
) -> Answer {
    let mut body = json!({
        "FunctionName": name,
        "Role": role_arn,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Code": { "ZipFile": "" },
    });
    for (member, value) in extras.as_object().expect("the extras are an object") {
        body[member.as_str()] = value.clone();
    }
    call(router, "POST", FUNCTIONS_URI, &body).await
}

/// `GetFunctionConfiguration` on the function `name`.
async fn get_configuration(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await
}

/// An attaching `VpcConfig`: the subnet to stand an interface up in, and the group to apply
/// to it.
fn attaching_config() -> Value {
    json!({ "SubnetIds": [SUBNET], "SecurityGroupIds": [SECURITY_GROUP] })
}

/// WHOSE call a row's gate decides, which is what the opening of its sentence follows from.
#[derive(Clone, Copy, Debug)]
enum Gate {
    /// Reading the event source a mapping would poll, decided by `CreateEventSourceMapping`.
    SourceRead,
    /// Writing the record of a batch that mapping gives up on, decided by the same call
    /// through its `DestinationConfig`.
    MappingFailureWrite,
    /// Writing a function's dead-letter target, decided by `CreateFunction`.
    DeadLetterTarget,
    /// Standing up the network interfaces an attachment to a VPC takes, decided by the same
    /// call through its `VpcConfig`.
    VpcAttachment,
}

impl Gate {
    /// How this gate's sentence opens: a call made on the running function's behalf, or one
    /// made to stand a resource up for it.
    fn opening(self) -> &'static str {
        match self {
            Self::SourceRead | Self::MappingFailureWrite => FUNCTION_OPENING,
            Self::DeadLetterTarget | Self::VpcAttachment => PROVIDED_OPENING,
        }
    }

    /// The OTHER class's opening, which this gate's sentence must never carry.
    fn other_opening(self) -> &'static str {
        if self.opening() == FUNCTION_OPENING {
            PROVIDED_OPENING
        } else {
            FUNCTION_OPENING
        }
    }

    /// What the call this gate decides answers when the role may make it.
    fn served_status(self) -> u16 {
        match self {
            Self::SourceRead | Self::MappingFailureWrite => 202,
            Self::DeadLetterTarget | Self::VpcAttachment => 201,
        }
    }

    /// Make the call this gate decides, as `role` and against the function `function`.
    async fn attempt(
        self,
        router: &MultiServiceRouter,
        case: &str,
        function: &str,
        role: &str,
    ) -> Answer {
        match self {
            // The two mapping gates are decided by the call that WIRES an existing function to
            // a source, so the function itself is stood up first.
            Self::SourceRead | Self::MappingFailureWrite => {
                create_function_as(router, case, function, role).await;
                let extras = if matches!(self, Self::MappingFailureWrite) {
                    json!({ "DestinationConfig": { "OnFailure": { "Destination": LETTER_QUEUE } } })
                } else {
                    json!({})
                };
                create_mapping(
                    router,
                    &create_request(function, Some(QUEUE_SOURCE), &extras),
                )
                .await
            }
            Self::DeadLetterTarget => {
                create_function_with(
                    router,
                    function,
                    role,
                    &json!({ "DeadLetterConfig": { "TargetArn": LETTER_QUEUE } }),
                )
                .await
            }
            Self::VpcAttachment => {
                create_function_with(router, function, role, &json!({ "VpcConfig": attaching_config() }))
                    .await
            }
        }
    }

    /// Assert the refusal was instead of the resource and not beside it.
    async fn assert_nothing_persisted(self, router: &MultiServiceRouter, case: &str, function: &str) {
        match self {
            Self::SourceRead | Self::MappingFailureWrite => {
                let listed = list_mappings(router, &format!("FunctionName={function}")).await;
                assert_eq!(
                    (listed.status, listed.listed_uuids()),
                    (200, Vec::new()),
                    "case {case:?}: a refused create must leave no mapping behind, got {} {}",
                    listed.status,
                    listed.text()
                );
            }
            Self::DeadLetterTarget | Self::VpcAttachment => {
                let reading = get_configuration(router, function).await;
                assert_eq!(
                    reading.status, 404,
                    "case {case:?}: a refused create must store no function, got {} {}",
                    reading.status,
                    reading.text()
                );
            }
        }
    }

    /// Round trip for a row the role was granted its call on: what the served call stated is
    /// what a later read reports.
    async fn assert_persisted(
        self,
        router: &MultiServiceRouter,
        case: &str,
        function: &str,
        answer: &Answer,
    ) {
        match self {
            Self::SourceRead | Self::MappingFailureWrite => {
                let uuid = answer
                    .string("UUID")
                    .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", answer.text()));
                let reading = get_mapping(router, &uuid).await;
                assert_eq!(
                    (reading.status, reading.string("EventSourceArn")),
                    (200, Some(QUEUE_SOURCE.to_string())),
                    "case {case:?}: the mapping must read back on {QUEUE_SOURCE}, got {} {}",
                    reading.status,
                    reading.text()
                );
            }
            Self::DeadLetterTarget => {
                let reading = get_configuration(router, function).await;
                let held = reading
                    .json()
                    .get("DeadLetterConfig")
                    .and_then(|config| config.get("TargetArn"))
                    .cloned()
                    .unwrap_or(Value::Null);
                assert_eq!(
                    held,
                    json!(LETTER_QUEUE),
                    "case {case:?}: the stored dead-letter target must be {LETTER_QUEUE}, got \
                     {held} in {}",
                    reading.text()
                );
            }
            Self::VpcAttachment => {
                let reading = get_configuration(router, function).await;
                let held = reading
                    .json()
                    .get("VpcConfig")
                    .and_then(|config| config.get("SubnetIds"))
                    .cloned()
                    .unwrap_or(Value::Null);
                assert_eq!(
                    held,
                    json!([SUBNET]),
                    "case {case:?}: the stored attachment must name the subnet it asked for, \
                     got {held} in {}",
                    reading.text()
                );
            }
        }
    }
}

/// One row: the gate that decides, the statements of the role's one inline policy, and the
/// WHOLE sentence the refusal has to carry — `None` for a row that must be served.
struct Row {
    case: &'static str,
    gate: Gate,
    statements: Value,
    refusal: Option<String>,
}

/// The catalog: which gate decides, which of its calls the role is short of, and the sentence
/// that follows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a queue source read, with no queue call granted at all",
            gate: Gate::SourceRead,
            statements: json!([logs_only()]),
            refusal: Some(call_refusal(QUEUE_READS[0], SQS)),
        },
        Row {
            // One read short of the set: the sentence names the first read that is withheld,
            // so the CALL in it varies with the grant and not with the request.
            case: "a queue source read, with only the receive granted",
            gate: Gate::SourceRead,
            statements: json!([
                logs_only(),
                allow(SQS_NAMESPACE, &[QUEUE_READS[0]], QUEUE_SOURCE)
            ]),
            refusal: Some(call_refusal(QUEUE_READS[1], SQS)),
        },
        Row {
            case: "a queue source read, with every queue call granted on it",
            gate: Gate::SourceRead,
            statements: json!([
                logs_only(),
                allow(SQS_NAMESPACE, &QUEUE_READS, QUEUE_SOURCE)
            ]),
            refusal: None,
        },
        Row {
            // The source reads are granted, so this row reaches the DESTINATION gate: the
            // write of a discarded batch's record is a call made on the function's behalf too.
            case: "a mapping's failure destination, with the source reads granted and the \
                   send withheld",
            gate: Gate::MappingFailureWrite,
            statements: json!([
                logs_only(),
                allow(SQS_NAMESPACE, &QUEUE_READS, QUEUE_SOURCE)
            ]),
            refusal: Some(call_refusal(QUEUE_WRITE, SQS)),
        },
        Row {
            case: "a mapping's failure destination, with the send granted on it",
            gate: Gate::MappingFailureWrite,
            statements: json!([
                logs_only(),
                allow(SQS_NAMESPACE, &QUEUE_READS, QUEUE_SOURCE),
                allow(SQS_NAMESPACE, &[QUEUE_WRITE], LETTER_QUEUE)
            ]),
            refusal: None,
        },
        Row {
            // The same withheld write as the destination row above, asked for by a gate that
            // stands a resource up instead: the other opening follows.
            case: "a dead-letter target, with only the log writes granted",
            gate: Gate::DeadLetterTarget,
            statements: json!([logs_only()]),
            refusal: Some(provided_role_sentence(QUEUE_WRITE, SQS)),
        },
        Row {
            case: "a dead-letter target, with the send granted on it",
            gate: Gate::DeadLetterTarget,
            statements: json!([logs_only(), allow(SQS_NAMESPACE, &[QUEUE_WRITE], LETTER_QUEUE)]),
            refusal: None,
        },
        Row {
            case: "a vpc attachment, with no interface call granted at all",
            gate: Gate::VpcAttachment,
            statements: json!([logs_only()]),
            refusal: Some(provided_role_sentence(ENI_CALLS[0], EC2)),
        },
        Row {
            // One call short of the set, so this row's sentence names a different call of the
            // same opening.
            case: "a vpc attachment, with the delete missing from the grant",
            gate: Gate::VpcAttachment,
            statements: json!([
                logs_only(),
                allow(EC2_NAMESPACE, &[ENI_CALLS[0], ENI_CALLS[1]], INTERFACES)
            ]),
            refusal: Some(provided_role_sentence(ENI_CALLS[2], EC2)),
        },
        Row {
            case: "a vpc attachment, with every interface call granted",
            gate: Gate::VpcAttachment,
            statements: json!([logs_only(), allow(EC2_NAMESPACE, &ENI_CALLS, INTERFACES)]),
            refusal: None,
        },
    ]
}

/// Assert `message` carries the opening `gate` words its sentence with, and nothing of the
/// other class: the two are told apart by exactly this phrase.
fn assert_opening(case: &str, gate: Gate, message: &str) {
    assert!(
        message.starts_with(gate.opening()),
        "case {case:?}: expected the sentence to open {:?}, got {message:?}",
        gate.opening()
    );
    assert!(
        !message.contains(gate.other_opening()),
        "case {case:?}: the sentence must not carry the other class's opening {:?}, got \
         {message:?}",
        gate.other_opening()
    );
}

/// Assert `message` is fixed text apart from the call and what it would have been made on: it
/// names nothing the request carried.
fn assert_names_nothing_of_the_request(case: &str, message: &str, named: &[&str]) {
    for value in named {
        assert!(
            !message.contains(value),
            "case {case:?}: the sentence is fixed text and must not name {value:?}, got \
             {message:?}"
        );
    }
}

#[tokio::test]
async fn a_withheld_call_is_refused_with_the_sentence_of_whose_call_it_is() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = router_with_iam();
        let role = given_role(
            &router,
            case,
            &format!("role-opening-{index}"),
            Some(row.statements.clone()),
        )
        .await;
        let function = format!("role-opening-fn-{index}");

        let answer = row.gate.attempt(&router, case, &function, &role).await;

        let Some(expected) = row.refusal.as_deref() else {
            assert_eq!(
                answer.status,
                row.gate.served_status(),
                "case {case:?}: expected HTTP {}, got {} {}",
                row.gate.served_status(),
                answer.status,
                answer.text()
            );
            row.gate
                .assert_persisted(&router, case, &function, &answer)
                .await;
            continue;
        };

        assert_refused(case, &answer, expected);
        let message = answer
            .string("message")
            .or_else(|| answer.string("Message"))
            .unwrap_or_default();
        assert_opening(case, row.gate, &message);
        assert_names_nothing_of_the_request(
            case,
            &message,
            &[&role, &function, QUEUE_SOURCE, LETTER_QUEUE, SUBNET],
        );
        row.gate
            .assert_nothing_persisted(&router, case, &function)
            .await;
    }
}

#[tokio::test]
async fn one_withheld_write_is_worded_by_whose_call_it_would_have_been() {
    // The SAME write on the SAME queue, withheld from the SAME role, asked for by a gate of
    // each kind: the call and the service in the two sentences are identical, and only the
    // opening differs, so a client reading this error class can tell the two apart.
    let case = "one role short of the queue write, asked for by both kinds of gate";
    let router = router_with_iam();
    let role = given_role(
        &router,
        case,
        "role-opening-pair",
        Some(json!([
            logs_only(),
            allow(SQS_NAMESPACE, &QUEUE_READS, QUEUE_SOURCE)
        ])),
    )
    .await;

    let on_the_functions_behalf = Gate::MappingFailureWrite
        .attempt(&router, case, "role-opening-pair-mapping", &role)
        .await;
    let standing_a_resource_up = Gate::DeadLetterTarget
        .attempt(&router, case, "role-opening-pair-function", &role)
        .await;

    assert_refused(
        case,
        &on_the_functions_behalf,
        &call_refusal(QUEUE_WRITE, SQS),
    );
    assert_refused(
        case,
        &standing_a_resource_up,
        &provided_role_sentence(QUEUE_WRITE, SQS),
    );

    let on_behalf = on_the_functions_behalf
        .string("message")
        .or_else(|| on_the_functions_behalf.string("Message"))
        .unwrap_or_default();
    let standing_up = standing_a_resource_up
        .string("message")
        .or_else(|| standing_a_resource_up.string("Message"))
        .unwrap_or_default();
    assert_opening(case, Gate::MappingFailureWrite, &on_behalf);
    assert_opening(case, Gate::DeadLetterTarget, &standing_up);
    assert_ne!(
        on_behalf, standing_up,
        "case {case:?}: the two gates must word the same withheld write differently, both \
         answered {on_behalf:?}"
    );
    // What is the same in both: the call, and the service it would have been made to.
    let tail = format!("does not have permissions to call {QUEUE_WRITE} on {SQS}");
    for (gate, message) in [
        ("on the function's behalf", &on_behalf),
        ("standing a resource up", &standing_up),
    ] {
        assert!(
            message.ends_with(&tail),
            "case {case:?}: the {gate} sentence must end {tail:?}, got {message:?}"
        );
    }
}
