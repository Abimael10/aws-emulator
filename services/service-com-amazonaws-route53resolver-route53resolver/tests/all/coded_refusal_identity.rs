// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the bracketed service error code inside a Resolver
//! refusal's `Message` IDENTIFIES.
//!
//! Every fault of this service answers HTTP 400, and several distinct faults
//! answer the one modelled error shape (`AccessDeniedException` is the answer of
//! the query-logging slice and of the per-VPC DNSSEC validation slice alike), so
//! the coded sentence inside `Message` — `[RSLVR-nnnnn]` followed by that
//! fault's fixed prose — is the only thing a caller can tell two refusals apart
//! by. The rule these tests pin: the code and the sentence a refusal states are
//! the ones of the fault that refused it, so two refusals of ONE fault state the
//! same code and the same sentence, two refusals of DIFFERENT faults never share
//! a code, and no refusal carries another fault's code — whatever the request
//! asked for.
//!
//! Each sentence below is the one the service's own recorded refusal of that
//! fault states; the trace clause a refusal closes with carries the trace of the
//! request and is the only part of the body that is not fixed text (asserted in
//! [`super::query_log_refusal_trace_clause`]).
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! shared [`super::harness`].

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::harness::{
    call_tracing, emulator, error_type, ok, string_member, traced_message, tracing_chain,
};

/// One fault of the service: the modelled error shape it answers and the coded
/// sentence that fault's refusals state.
struct Fault {
    /// What the fault IS, as the case names it.
    key: &'static str,
    /// The modelled error shape every refusal of this fault names.
    error: &'static str,
    /// The fixed coded sentence every refusal of this fault states, before the
    /// trace clause where the service appends one.
    sentence: &'static str,
}

/// The faults these tests span: the two denials that answer the one
/// `AccessDeniedException` — the query-logging slice's and the per-VPC DNSSEC
/// validation slice's — and the two can't-address faults of the query-logging
/// slice that answer the one `ResourceNotFoundException`. Each pair shares an
/// error shape and an HTTP status, so each pair is told apart by its code alone,
/// which is what makes them the cases the rule has to hold for.
const FAULTS: &[Fault] = &[
    Fault {
        key: "the query-logging slice denies the caller",
        error: "AccessDeniedException",
        sentence: "[RSLVR-01600] Access denied",
    },
    Fault {
        key: "the per-VPC DNSSEC validation slice denies the caller",
        error: "AccessDeniedException",
        sentence: "[RSLVR-01803] Access to resource is denied.",
    },
    Fault {
        key: "the query logging configuration cannot be addressed",
        error: "ResourceNotFoundException",
        sentence: "[RSLVR-01601] The specified query logging configuration doesn't exist.",
    },
    Fault {
        key: "the association cannot be addressed",
        error: "ResourceNotFoundException",
        sentence: "[RSLVR-01602] The specified query logging configuration association doesn't \
                   exist.",
    },
];

/// A VPC of the account, so a refusal of a query-logging call that names one is
/// about the query-logging resource and never about the VPC.
const HELD_VPC: &str = "vpc-0123456789abcdef1";

/// A VPC the account does not hold, in the 17-hex-digit form AWS mints: what
/// makes the per-VPC DNSSEC validation calls answer their own denial.
const UNHELD_VPC: &str = "vpc-0123456789abcdef2";

/// A `ResolverQueryLogConfigId` no create ever minted, well formed for the
/// modelled `ResourceId` (length 1..64).
const UNKNOWN_CONFIG: &str = "rqlc-00000000000000011";

/// A `ResolverQueryLogConfigAssociationId` no associate ever minted.
const UNKNOWN_ASSOCIATION: &str = "rqlca-00000000000000011";

/// The fault `key` names.
fn fault(key: &str) -> &'static Fault {
    FAULTS
        .iter()
        .find(|fault| fault.key == key)
        .unwrap_or_else(|| panic!("the catalog states the fault {key:?}"))
}

/// The service error code a coded sentence carries, read out of the brackets it
/// opens with.
fn code_of(sentence: &str) -> &str {
    sentence
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .map(|(code, _)| code)
        .unwrap_or_else(|| panic!("a coded sentence opens with its code in brackets: {sentence:?}"))
}

/// The sentence a refusal states, with the trace clause the service may close it
/// with taken off.
fn stated_sentence(message: &str) -> &str {
    traced_message(message)
        .map(|(stated, _)| stated)
        .unwrap_or(message)
}

/// One refused call: the operation, the request, and the fault that refuses it.
struct Refused {
    case: String,
    operation: &'static str,
    body: Value,
    fault: &'static str,
}

/// Every refusal states the coded sentence of its OWN fault and carries no other
/// fault's code; refusals of one fault agree on that sentence, and two faults
/// never share a code — which is what lets a caller tell them apart under the
/// one error shape and the one HTTP status.
#[tokio::test]
async fn a_refusal_states_the_code_of_its_own_fault_and_of_no_other() {
    let router = emulator();
    router.hold_vpcs([HELD_VPC]);

    // The rows differ along what the rule generalises over: the fault, the
    // operation that reaches it, and — for the one fault reached by more than
    // one kind of request — the kind of destination the refused create named,
    // in the ARN forms `CreateResolverQueryLogConfig$DestinationArn` documents.
    let refusals: Vec<Refused> = vec![
        Refused {
            case: "a create naming a log group the account does not hold".to_string(),
            operation: "CreateResolverQueryLogConfig",
            body: json!({
                "CreatorRequestId": "req-code-identity-logs",
                "Name": "code-identity-logs",
                "DestinationArn":
                    "arn:aws:logs:us-west-2:000000000000:log-group:/code-identity-group:*",
            }),
            fault: "the query-logging slice denies the caller",
        },
        Refused {
            case: "a create naming an S3 bucket the account does not hold".to_string(),
            operation: "CreateResolverQueryLogConfig",
            body: json!({
                "CreatorRequestId": "req-code-identity-s3",
                "Name": "code-identity-s3",
                "DestinationArn": "arn:aws:s3:::amzn-s3-demo-bucket-code-identity",
            }),
            fault: "the query-logging slice denies the caller",
        },
        Refused {
            case: "a create naming a delivery stream the account does not hold".to_string(),
            operation: "CreateResolverQueryLogConfig",
            body: json!({
                "CreatorRequestId": "req-code-identity-firehose",
                "Name": "code-identity-firehose",
                "DestinationArn":
                    "arn:aws:firehose:us-west-2:000000000000:deliverystream/code-identity",
            }),
            fault: "the query-logging slice denies the caller",
        },
        Refused {
            case: "a DNSSEC validation read of a VPC the account does not hold".to_string(),
            operation: "GetResolverDnssecConfig",
            body: json!({ "ResourceId": UNHELD_VPC }),
            fault: "the per-VPC DNSSEC validation slice denies the caller",
        },
        Refused {
            case: "a DNSSEC validation write of a VPC the account does not hold".to_string(),
            operation: "UpdateResolverDnssecConfig",
            body: json!({ "ResourceId": UNHELD_VPC, "Validation": "ENABLE" }),
            fault: "the per-VPC DNSSEC validation slice denies the caller",
        },
        Refused {
            case: "a read of a configuration the store holds nothing under".to_string(),
            operation: "GetResolverQueryLogConfig",
            body: json!({ "ResolverQueryLogConfigId": UNKNOWN_CONFIG }),
            fault: "the query logging configuration cannot be addressed",
        },
        Refused {
            case: "a delete of a configuration the store holds nothing under".to_string(),
            operation: "DeleteResolverQueryLogConfig",
            body: json!({ "ResolverQueryLogConfigId": UNKNOWN_CONFIG }),
            fault: "the query logging configuration cannot be addressed",
        },
        Refused {
            case: "a read of an association the store holds nothing under".to_string(),
            operation: "GetResolverQueryLogConfigAssociation",
            body: json!({ "ResolverQueryLogConfigAssociationId": UNKNOWN_ASSOCIATION }),
            fault: "the association cannot be addressed",
        },
        Refused {
            case: "a disassociate of a pair the store holds no association for".to_string(),
            operation: "DisassociateResolverQueryLogConfig",
            body: json!({ "ResolverQueryLogConfigId": UNKNOWN_CONFIG, "ResourceId": HELD_VPC }),
            fault: "the association cannot be addressed",
        },
    ];

    // What each fault was actually reported as, so one disagreement between two
    // refusals of one fault is reported with both cases.
    let mut stated_per_fault: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();

    for (index, refused) in refusals.iter().enumerate() {
        let expected = fault(refused.fault);
        // Half the rows name their own trace chain and half name none, so the
        // code is asserted both where the caller supplies the trace and where
        // the service mints one for the request.
        let chain = (index % 2 == 0).then(|| tracing_chain(index as u32 + 1));
        let case = format!(
            "{} / {}",
            refused.case,
            if chain.is_some() {
                "a caller naming a chain"
            } else {
                "a caller naming none"
            }
        );

        let (status, body) = call_tracing(
            &router,
            refused.operation,
            refused.body.clone(),
            chain.as_ref().map(|(header, _)| header.as_str()),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {} must answer HTTP 400, got {status}: {body}",
            refused.operation
        );
        assert!(
            error_type(&body).contains(expected.error),
            "case {case:?}: the refusal must name {}, got {body}",
            expected.error
        );

        let message = string_member(&body, "Message");
        let stated = stated_sentence(&message);
        assert_eq!(
            stated, expected.sentence,
            "case {case:?}: the refusal must state the coded sentence of {:?}, got {message:?}",
            expected.key
        );

        // No other fault's code may appear anywhere in the body a caller reads,
        // so a refusal is never pointed at an unrelated documented fault.
        let serialized = serde_json::to_string(&body).expect("the refusal serializes");
        for other in FAULTS {
            if other.key == expected.key {
                continue;
            }
            let other_code = code_of(other.sentence);
            assert!(
                !serialized.contains(other_code),
                "case {case:?}: the refusal must carry no code of {:?} ({other_code}), got {body}",
                other.key
            );
        }

        stated_per_fault
            .entry(expected.key)
            .or_default()
            .push((case, stated.to_string()));
    }

    // Every fault the catalog states was exercised, so a fault that stops being
    // reachable is not silently dropped from the rule.
    assert_eq!(
        stated_per_fault.len(),
        FAULTS.len(),
        "every fault of the catalog must be exercised, got {:?}",
        stated_per_fault.keys().collect::<Vec<_>>()
    );

    // Two refusals of one fault state one sentence, byte for byte.
    for (key, seen) in &stated_per_fault {
        let (first_case, first) = &seen[0];
        for (case, stated) in &seen[1..] {
            assert_eq!(
                stated, first,
                "fault {key:?}: case {case:?} and case {first_case:?} must state one sentence, \
                 got {stated:?} and {first:?}"
            );
        }
    }

    // Two refusals of different faults never share a code.
    let mut owner_of_code: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, seen) in &stated_per_fault {
        let code = code_of(&seen[0].1);
        if let Some(other) = owner_of_code.insert(code, key) {
            panic!(
                "fault {key:?} and fault {other:?} must not be reported under one code, both \
                 state {code}"
            );
        }
    }
}

/// One refused create: the request shape it was made with.
struct Shape {
    case: &'static str,
    name: &'static str,
    creator_request_id: &'static str,
    destination_arn: &'static str,
    /// Whether the create asks for a tag, so the denial is asserted both for a
    /// request that carries `Tags` and one that carries none.
    tagged: bool,
}

/// The coded sentence of a fault is FIXED text, so it is the same for every
/// request that reaches that fault: these rows differ along every member the
/// refused create carries — the `Name` (both ends of the modelled length 1..64,
/// and the punctuation its pattern admits), the `CreatorRequestId`, the kind and
/// the ARN form of the `DestinationArn`, and whether `Tags` are asked for — and
/// not one of them may change the sentence or appear in it.
const SHAPES: &[Shape] = &[
    Shape {
        case: "a one-character name, the modelled minimum",
        name: "a",
        creator_request_id: "req-shape-min",
        destination_arn: "arn:aws:s3:::amzn-s3-demo-bucket-shape-min",
        tagged: false,
    },
    Shape {
        case: "a name of punctuation the pattern admits",
        name: "o'brien dns_log-01",
        creator_request_id: "req-shape-punctuation",
        destination_arn: "arn:aws:s3:::amzn-s3-demo-bucket-shape-punctuation/queries/",
        tagged: false,
    },
    Shape {
        case: "a name at the modelled maximum of 64 characters",
        name: "shape-at-the-modelled-maximum-of-sixty-four-characters-aaaaaaaaa",
        creator_request_id: "req-shape-max",
        destination_arn: "arn:aws:logs:eu-west-1:000000000000:log-group:/shape-max",
        tagged: false,
    },
    Shape {
        case: "a create asking for tags",
        name: "shape-tagged",
        creator_request_id: "req-shape-tagged",
        destination_arn: "arn:aws:kinesis:us-east-2:000000000000:stream/shape-tagged",
        tagged: true,
    },
];

/// The denial a create draws for a destination the account does not hold states
/// one fixed coded sentence whatever the request asked for: nothing of the
/// `Name`, the `CreatorRequestId`, the `DestinationArn` or the `Tags` reaches
/// it, and no configuration is stored for a denied create.
#[tokio::test]
async fn the_denial_of_a_create_is_fixed_text_whatever_the_request_asked_for() {
    let router = emulator();
    let expected = fault("the query-logging slice denies the caller");

    let mut seen: Vec<(&str, String)> = Vec::new();
    for shape in SHAPES {
        let mut body = json!({
            "CreatorRequestId": shape.creator_request_id,
            "Name": shape.name,
            "DestinationArn": shape.destination_arn,
        });
        if shape.tagged {
            body.as_object_mut()
                .expect("the request body is an object")
                .insert(
                    "Tags".to_string(),
                    json!([{ "Key": "env", "Value": "integration" }]),
                );
        }

        let (status, refused) =
            call_tracing(&router, "CreateResolverQueryLogConfig", body, None).await;
        let case = shape.case;
        assert_eq!(
            status, 400,
            "case {case:?}: a destination the account does not hold must be refused, got \
             {status}: {refused}"
        );
        assert!(
            error_type(&refused).contains(expected.error),
            "case {case:?}: the denial must name {}, got {refused}",
            expected.error
        );

        let message = string_member(&refused, "Message");
        let stated = stated_sentence(&message);
        assert_eq!(
            stated, expected.sentence,
            "case {case:?}: the denial must state the slice's coded sentence, got {message:?}"
        );
        for (part, value) in [
            ("the configuration name", shape.name),
            ("the creator request id", shape.creator_request_id),
            ("the destination", shape.destination_arn),
        ] {
            assert!(
                !stated.contains(value),
                "case {case:?}: the denial must disclose nothing of the request, it named {part} \
                 in {message:?}"
            );
        }
        seen.push((case, stated.to_string()));
    }

    let (first_case, first) = &seen[0];
    for (case, stated) in &seen[1..] {
        assert_eq!(
            stated, first,
            "case {case:?} and case {first_case:?} must state one sentence, got {stated:?} and \
             {first:?}"
        );
    }

    // A denied create writes nothing, so the account holds no configuration
    // after all of them.
    let page = ok(&router, "ListResolverQueryLogConfigs", json!({})).await;
    let stored = page
        .get("ResolverQueryLogConfigs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        stored, 0,
        "a denied create must store no configuration, the listing reports {page}"
    );
}
