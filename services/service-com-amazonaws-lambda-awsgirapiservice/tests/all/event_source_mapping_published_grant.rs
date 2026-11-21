// SPDX-License-Identifier: CC0-1.0

//! Wiring a function to an event source with the grant AWS PUBLISHES for that source: the
//! managed policy a client attaches to the execution role rather than a document it writes
//! itself.
//!
//! RULE: Lambda reads an event source acting as the invoked function's execution role, and the
//! permissions in force on that role are what its attached managed policies allow — so a role
//! whose only policy is the execution-role policy AWS publishes for ONE kind of event source
//! may read that source and no other: `CreateEventSourceMapping` is served for the source that
//! policy is published for, and refused with the modelled `InvalidParameterValueException`
//! (HTTP 400), persisting nothing, for every other source; and the mapping it served then
//! really consumes, because the published grant covers the reads its poller makes.
//!
//! The policies are AWS's own (`arn:aws:iam::aws:policy/service-role/...`), served by the IAM
//! emulator's catalog of them, and attached here through the real `AttachRolePolicy` — so what
//! this gate reads is the document IAM resolves for that ARN and not a copy written in the
//! test. The owning crate's `tests/all/published_lambda_event_source_policies.rs` pins the
//! catalog side: that each of those policies is served and what its document allows.
//!
//! The rows differ along what the rule generalises over: WHICH published policy the role
//! carries (the queue's, and each shard source's) and WHICH source the mapping is made on
//! (the same three), so each policy is driven against its own source and against the sources
//! it does not cover. `tests/all/event_source_mapping_role_access.rs` owns the same rule for a
//! grant the client WRITES; what is added here is the published policy a client attaches
//! instead, which is how the role of an SQS-triggered function is granted in practice.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{create_function_as, given_role, iam};
use crate::event_source_mappings::{
    await_mapping_state, call, create_mapping, create_queue, list_mappings, router_on,
    send_message, taken_bodies, DYNAMODB_SOURCE, FORWARDING_HANDLER, KINESIS_SOURCE, LOOK_AGAIN,
    RESULTS_QUEUE_URL, RUNNING_HANDLER, RUNNING_RUNTIME, STATE_ENABLED,
};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The wire code of the modelled 400 a mapping whose role may not read the source is refused
/// with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The published policy covering the send the forwarding handler makes with the AWS SDK: its
/// own call, which no event source's execution-role policy grants. AWS files it at the
/// account root rather than under the service-role path, so this carries its whole ARN.
const HANDLER_SEND_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSQSFullAccess";

/// The ARN AWS publishes an execution-role policy at: the path AWS files the policies of a
/// service role under, and the policy's own friendly name.
fn published(policy_name: &str) -> String {
    format!("arn:aws:iam::aws:policy/service-role/{policy_name}")
}

/// One kind of event source, as a `CreateEventSourceMapping` names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    /// An Amazon SQS queue, created per test so the mapping names the ARN the queue reports.
    Queue,
    /// An Amazon Kinesis data stream.
    KinesisStream,
    /// An Amazon DynamoDB stream.
    DynamodbStream,
}

impl Source {
    /// How the request names this source, with `queue_arn` standing for the queue this test
    /// created.
    fn arn(self, queue_arn: &str) -> String {
        match self {
            Self::Queue => queue_arn.to_string(),
            Self::KinesisStream => KINESIS_SOURCE.to_string(),
            Self::DynamodbStream => DYNAMODB_SOURCE.to_string(),
        }
    }

    /// The members a mapping on this source requires beside the pair: a shard source is read
    /// from a position, which `CreateEventSourceMapping` requires for it and takes for no
    /// queue.
    fn extras(self) -> Value {
        match self {
            Self::Queue => json!({}),
            Self::KinesisStream | Self::DynamodbStream => json!({ "StartingPosition": "LATEST" }),
        }
    }

    /// The published policy whose grant covers this source's own reads.
    fn published_policy(self) -> &'static str {
        match self {
            Self::Queue => "AWSLambdaSQSQueueExecutionRole",
            Self::KinesisStream => "AWSLambdaKinesisExecutionRole",
            Self::DynamodbStream => "AWSLambdaDynamoDBExecutionRole",
        }
    }

    /// A name for this source in an assertion message.
    fn label(self) -> &'static str {
        match self {
            Self::Queue => "an Amazon SQS queue",
            Self::KinesisStream => "an Amazon Kinesis data stream",
            Self::DynamodbStream => "an Amazon DynamoDB stream",
        }
    }
}

/// Every source the table drives, and so every published policy it attaches.
const SOURCES: [Source; 3] = [
    Source::Queue,
    Source::KinesisStream,
    Source::DynamodbStream,
];

/// Lambda over the SAME engine as Amazon SQS and IAM: the mapping resolves its role in the
/// service that owns roles and its queue in the service that owns queues, exactly as
/// `core/router` assembles them.
fn router_with_queues_and_iam() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// A role whose only policies are the published ones `policy_arns` lists, each attached
/// through the real `AttachRolePolicy`; answers the ARN IAM persisted.
async fn role_carrying(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy_arns: &[String],
) -> String {
    let role_arn = given_role(router, case, role_name, None).await;
    for policy_arn in policy_arns {
        let (status, attached) = iam(
            router,
            "AttachRolePolicy",
            &[("RoleName", role_name), ("PolicyArn", policy_arn.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: AWS publishes {policy_arn}, so attaching it must be served, got \
             {status}: {attached}"
        );
    }
    role_arn
}

/// RULE: the published policy a role carries decides WHICH source its function may be wired
/// to — its own source is served, every other source is refused and nothing is persisted.
#[tokio::test]
async fn a_role_carrying_a_published_policy_may_be_wired_to_that_sources_mappings_alone() {
    for (attached_index, attached) in SOURCES.iter().enumerate() {
        for (source_index, source) in SOURCES.iter().enumerate() {
            let policy_name = attached.published_policy();
            let case = format!(
                "a role carrying {policy_name} mapped onto {}",
                source.label()
            );
            let (router, _engine) = router_with_queues_and_iam();
            let (_url, queue_arn) = create_queue(
                &router,
                &case,
                &format!("published-grant-{attached_index}-{source_index}"),
                &json!({}),
            )
            .await;
            let role_name = format!("published-grant-role-{attached_index}-{source_index}");
            let role_arn =
                role_carrying(&router, &case, &role_name, &[published(policy_name)]).await;
            let function = format!("published-grant-fn-{attached_index}-{source_index}");
            create_function_as(&router, &case, &function, &role_arn).await;

            let mut request = json!({
                "FunctionName": function,
                "EventSourceArn": source.arn(&queue_arn),
            });
            if let Some(extras) = source.extras().as_object() {
                for (member, value) in extras {
                    request[member] = value.clone();
                }
            }
            let created = create_mapping(&router, &request).await;

            if attached == source {
                assert_eq!(
                    created.status,
                    202,
                    "case {case:?}: the policy AWS publishes for this source grants its own \
                     reads, so the mapping must be served, got {} {}",
                    created.status,
                    created.text()
                );
                let uuid = created.string("UUID").unwrap_or_else(|| {
                    panic!("case {case:?}: the created mapping reports no UUID: {}", created.text())
                });
                // ROUND TRIP: the mapping the create served is one a client reads back, and it
                // settles into the state its `Enabled` default asks for.
                await_mapping_state(&router, &case, &uuid, STATE_ENABLED).await;
                let listed = list_mappings(&router, &format!("?FunctionName={function}")).await;
                assert_eq!(
                    listed.listed_uuids(),
                    vec![uuid.clone()],
                    "case {case:?}: the function must carry exactly the mapping that was \
                     served: {}",
                    listed.text()
                );
            } else {
                assert_eq!(
                    created.status,
                    400,
                    "case {case:?}: {policy_name} grants no read of this source, so the mapping \
                     must be refused, got {} {}",
                    created.status,
                    created.text()
                );
                assert_eq!(
                    created.error_code(),
                    INVALID_PARAMETER,
                    "case {case:?}: expected the modelled {INVALID_PARAMETER}, got {} {}",
                    created.error_code(),
                    created.text()
                );
                let listed = list_mappings(&router, &format!("?FunctionName={function}")).await;
                assert!(
                    listed.listed_uuids().is_empty(),
                    "case {case:?}: a refused create persists no mapping, got {}",
                    listed.text()
                );
            }
        }
    }
}

/// RULE (the consuming half): the published queue policy covers the reads the poller makes, so
/// a mapping served to a role carrying nothing else really consumes its queue — the message is
/// handed to the function, which forwards it.
#[tokio::test]
async fn a_queue_mapping_granted_by_the_published_policy_consumes_its_queue() {
    let case = "a queue mapping whose role carries only AWSLambdaSQSQueueExecutionRole";
    let (router, _engine) = router_with_queues_and_iam();
    let (results_url, _results_arn) =
        create_queue(&router, case, "published-grant-results", &json!({})).await;
    let (source_url, source_arn) =
        create_queue(&router, case, "published-grant-source", &json!({})).await;

    // The queue source's own published policy, which is what the POLLER's reads are decided
    // by, beside the published policy covering the send the HANDLER itself makes: Lambda reads
    // the source as this role and the handler runs as it, so the two grants are different
    // calls and neither policy covers the other's.
    let role_arn = role_carrying(
        &router,
        case,
        "published-grant-consumer",
        &[
            published(Source::Queue.published_policy()),
            HANDLER_SEND_POLICY.to_string(),
        ],
    )
    .await;
    let function = "published-grant-consumer-fn";
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": function,
            "Role": role_arn,
            "Runtime": RUNNING_RUNTIME,
            "Handler": RUNNING_HANDLER,
            "Timeout": 10,
            "Environment": { "Variables": { RESULTS_QUEUE_URL: results_url } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(FORWARDING_HANDLER)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );

    let mapping = create_mapping(
        &router,
        &json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": 1,
        }),
    )
    .await;
    assert_eq!(
        mapping.status,
        202,
        "case {case:?}: the published grant covers the queue's reads, so the mapping must be \
         served, got {} {}",
        mapping.status,
        mapping.text()
    );
    let uuid = mapping
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", mapping.text()));
    await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

    let body = "granted-by-the-published-policy";
    send_message(&router, case, &source_url, body, None).await;

    // The wait is on the CONDITION — the body forwarded — with a deadline generous enough that
    // only work that never happened fails it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(taken_bodies(&router, case, &results_url).await);
        if seen.iter().any(|forwarded| forwarded == body) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the mapping must hand its function the message it was sent, the \
             handler forwarded {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
