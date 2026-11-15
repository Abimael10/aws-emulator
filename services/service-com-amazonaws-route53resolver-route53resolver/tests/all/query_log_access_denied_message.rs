// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the body a DENIAL of the Resolver query-logging slice
//! carries.
//!
//! The rule under test: every Resolver fault answers HTTP 400 and more than one
//! of them answers `AccessDeniedException`, so the only thing that tells two
//! denials apart is the service error code inside `Message` — and that code is
//! the code of the SLICE that denied the request. A denial the query-logging
//! slice answers states that slice's fixed sentence, `[RSLVR-01600] Access
//! denied`, carrying nothing from the request; a denial another slice answers
//! (the per-VPC DNSSEC validation configuration) states its own,
//! `[RSLVR-01803] Access to resource is denied.`. Where the service closes a
//! refusal with its trace clause, that clause follows the sentence and is the
//! only part of the body that is not fixed text.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! shared [`super::harness`].

use serde_json::{json, Value};

use super::harness::{
    call, call_tracing, emulator, error_type, ok, string_member, traced_message, tracing_chain,
    Emulator,
};

/// The sentence the query-logging slice words its denials with, as the recorded
/// responses of that slice state it: the slice's service error code in brackets
/// and the fault, with no terminating period and nothing from the request.
const QUERY_LOG_DENIAL: &str = "[RSLVR-01600] Access denied";

/// The sentence the per-VPC configuration slice words ITS denial with, as that
/// slice's own recorded response states it — a different code and different
/// prose, which is what makes the two denials distinguishable.
const VPC_CONFIG_DENIAL: &str = "[RSLVR-01803] Access to resource is denied.";

/// A VPC the account does not hold, in the 17-hex-digit form AWS mints: what
/// makes the DNSSEC configuration read a denial of the other slice.
const UNHELD_VPC: &str = "vpc-0123456789abcdef0";

/// One `DestinationArn` the create is refused for, of one of the kinds Resolver
/// delivers query logs to.
struct Destination {
    case: &'static str,
    arn: &'static str,
}

/// The rows differ along what the rule generalises over: the KIND of destination
/// the refused create named, and the ARN form it named it in. Every kind
/// `CreateResolverQueryLogConfig$DestinationArn` documents is here — "an S3
/// bucket, a CloudWatch Logs log group, or a Kinesis Data Firehose delivery
/// stream" — a bucket bare and under the file prefix that doc allows, a log
/// group with and without the `:*` log-stream wildcard, and a stream under both
/// the `kinesis` and the `firehose` service names. None of them is held by the
/// service that owns it when the row runs, and each name differs from the ones
/// the other suites use so no row depends on another's store.
const DESTINATIONS: &[Destination] = &[
    Destination {
        case: "an S3 bucket",
        arn: "arn:aws:s3:::amzn-s3-demo-bucket-denied",
    },
    Destination {
        case: "an S3 bucket under a file prefix",
        arn: "arn:aws:s3:::amzn-s3-demo-bucket-denied-prefixed/queries/",
    },
    Destination {
        case: "a log group named with the stream wildcard",
        arn: "arn:aws:logs:us-west-2:123456789012:log-group:/denied-wildcard:*",
    },
    Destination {
        case: "a log group named without the wildcard",
        arn: "arn:aws:logs:us-west-2:123456789012:log-group:/denied-bare",
    },
    Destination {
        case: "a Kinesis data stream",
        arn: "arn:aws:kinesis:us-east-2:012345678901:stream/denied-stream",
    },
    Destination {
        case: "a Firehose delivery stream",
        arn: "arn:aws:firehose:eu-west-1:012345678901:deliverystream/denied-delivery",
    },
];

/// The `Id`s `ListResolverQueryLogConfigs` reports.
async fn listed_ids(router: &Emulator) -> Vec<String> {
    let page = ok(router, "ListResolverQueryLogConfigs", json!({})).await;
    page.get("ResolverQueryLogConfigs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|config| string_member(config, "Id"))
        .collect()
}

/// A create naming a destination the account does not hold is denied with the
/// query-logging slice's own coded sentence, whichever kind of destination it
/// named and in whichever ARN form — and the very same create is served once
/// the account holds that destination, so the denial is about the destination
/// and never about the operation.
#[tokio::test]
async fn a_denied_query_log_create_states_the_slice_coded_sentence() {
    let router = emulator();

    for (index, destination) in DESTINATIONS.iter().enumerate() {
        // Half the rows name their own trace chain and half name none, so the
        // clause is asserted both where the caller supplies the trace and where
        // the service mints one for the request.
        let chain = (index % 2 == 0).then(|| tracing_chain(index as u32 + 1));
        let case = format!(
            "{} / {}",
            destination.case,
            if chain.is_some() {
                "a caller naming a chain"
            } else {
                "a caller naming none"
            }
        );
        let name = format!("denied-{index}");
        let creator = format!("req-denied-{index}");
        let body = json!({
            "CreatorRequestId": creator.clone(),
            "Name": name.clone(),
            "DestinationArn": destination.arn,
        });

        let (status, refused) = call_tracing(
            &router,
            "CreateResolverQueryLogConfig",
            body.clone(),
            chain.as_ref().map(|(header, _)| header.as_str()),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a destination the account does not hold must be refused, got \
             {status}: {refused}"
        );
        assert!(
            error_type(&refused).contains("AccessDeniedException"),
            "case {case:?}: the denial must name AccessDeniedException, got {refused}"
        );
        assert!(
            refused.get("ResolverQueryLogConfig").is_none(),
            "case {case:?}: a denial carries no configuration, got {refused}"
        );

        let message = string_member(&refused, "Message");
        let (stated, trace) = traced_message(&message).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the denial must close with its trace clause, carrying a trace id \
                 in quotes, got {message:?}"
            )
        });
        assert_eq!(
            stated, QUERY_LOG_DENIAL,
            "case {case:?}: the denial must state the query-logging slice's coded sentence, got \
             {message:?}"
        );
        if let Some((_, root)) = &chain {
            assert_eq!(
                trace, root,
                "case {case:?}: the clause must carry the Root the request named, got {message:?}"
            );
        }
        // The fixed sentence names nothing the caller asked about: not the ARN
        // it was handed, not the name it chose, not its request id.
        for (part, value) in [
            ("the destination", destination.arn),
            ("the configuration name", name.as_str()),
            ("the creator request id", creator.as_str()),
        ] {
            assert!(
                !stated.contains(value),
                "case {case:?}: the denial must disclose nothing of the request, it named {part} \
                 in {message:?}"
            );
        }

        let listed = listed_ids(&router).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: a denied create must store no configuration, got {listed:?}"
        );

        // Held, the very same create is served and the configuration reads back
        // through the store.
        router.hold_query_log_destinations([destination.arn]);
        let created = ok(&router, "CreateResolverQueryLogConfig", body).await;
        let config = created.get("ResolverQueryLogConfig").unwrap_or_else(|| {
            panic!("case {case:?}: the create answers a configuration, got {created}")
        });
        let id = string_member(config, "Id");
        let fetched = ok(
            &router,
            "GetResolverQueryLogConfig",
            json!({ "ResolverQueryLogConfigId": id.clone() }),
        )
        .await;
        let read = fetched.get("ResolverQueryLogConfig").unwrap_or_else(|| {
            panic!("case {case:?}: the read answers a configuration, got {fetched}")
        });
        assert_eq!(
            string_member(read, "Id"),
            id,
            "case {case:?}: the read must answer the configuration the create wrote, got {fetched}"
        );

        // Each row starts from an empty collection, so its own listing
        // assertion is about its own denial.
        ok(
            &router,
            "DeleteResolverQueryLogConfig",
            json!({ "ResolverQueryLogConfigId": id }),
        )
        .await;
    }
}

/// Both denials of the query-logging slice state the ONE coded sentence — the
/// create that named a destination the account does not hold, and the read of a
/// configuration's resource policy while no policy is attached — while the
/// denial of another slice states its own code, which is what lets a caller
/// tell two denials apart under the one error shape and the one status.
#[tokio::test]
async fn the_denials_of_the_slice_share_one_code_and_other_slices_keep_theirs() {
    let router = emulator();
    let held = "arn:aws:s3:::amzn-s3-demo-bucket-shared-denial";
    let unheld = "arn:aws:s3:::amzn-s3-demo-bucket-no-such-destination";

    // A configuration of a destination the account DOES hold, so its policy read
    // is denied for the policy and not for the destination.
    router.hold_query_log_destinations([held]);
    let created = ok(
        &router,
        "CreateResolverQueryLogConfig",
        json!({
            "CreatorRequestId": "req-shared-denial",
            "Name": "shared-denial",
            "DestinationArn": held,
        }),
    )
    .await;
    let arn = string_member(
        created
            .get("ResolverQueryLogConfig")
            .unwrap_or_else(|| panic!("the create answers a configuration, got {created}")),
        "Arn",
    );

    let denials: [(&str, &str, Value); 2] = [
        (
            "a create naming a destination the account does not hold",
            "CreateResolverQueryLogConfig",
            json!({
                "CreatorRequestId": "req-shared-denial-2",
                "Name": "shared-denial-2",
                "DestinationArn": unheld,
            }),
        ),
        (
            "a policy read while no policy is attached",
            "GetResolverQueryLogConfigPolicy",
            json!({ "Arn": arn }),
        ),
    ];

    for (case, operation, body) in denials {
        let (status, refused) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must answer HTTP 400, got {status}: {refused}"
        );
        assert!(
            error_type(&refused).contains("AccessDeniedException"),
            "case {case:?}: the denial must name AccessDeniedException, got {refused}"
        );
        let message = string_member(&refused, "Message");
        // The clause, where the service appends one, closes the sentence: what
        // both denials share is the sentence they state before it.
        let stated = traced_message(&message)
            .map(|(stated, _)| stated)
            .unwrap_or(message.as_str());
        assert_eq!(
            stated, QUERY_LOG_DENIAL,
            "case {case:?}: every denial of this slice must state its one coded sentence, got \
             {message:?}"
        );
    }

    // The denial another slice answers keeps its own code and its own prose, so
    // the two faults are not reported as one.
    let (status, other) = call(
        &router,
        "GetResolverDnssecConfig",
        json!({ "ResourceId": UNHELD_VPC }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a VPC the account does not hold must be refused, got {status}: {other}"
    );
    assert!(
        error_type(&other).contains("AccessDeniedException"),
        "the refusal must name AccessDeniedException, got {other}"
    );
    let message = string_member(&other, "Message");
    let (stated, _) = traced_message(&message).unwrap_or_else(|| {
        panic!("the refusal must close with its trace clause, got {message:?}")
    });
    assert_eq!(
        stated, VPC_CONFIG_DENIAL,
        "the denial of another slice must keep its own coded sentence, got {message:?}"
    );
    assert_ne!(
        stated, QUERY_LOG_DENIAL,
        "two denials of different slices must not be reported under one code, got {message:?}"
    );
}
