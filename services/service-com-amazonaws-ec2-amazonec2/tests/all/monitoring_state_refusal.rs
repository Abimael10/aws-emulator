// SPDX-License-Identifier: CC0-1.0

//! How the detailed-monitoring pair refuses an instance whose lifecycle state has nothing
//! running to monitor.
//!
//! The transport is the crate's shared [`super::ec2_query`] harness, so every call here is
//! an ec2Query request a client could have sent and every refusal is read out of the error
//! envelope the caller receives rather than out of the store.
//!
//! THE RULE: an operation that changes an instance's detailed-monitoring state
//! (`MonitorInstances`, `UnmonitorInstances`) refuses an instance whose lifecycle state is
//! not one it supports with HTTP 400, the code `InvalidState` and the FIXED sentence
//! [`NOT_SUPPORTED`] — which interpolates nothing at all: neither the instance the request
//! named, nor the state it is in, nor the states the operation does support. That is a
//! different refusal from the `IncorrectInstanceState` sentence the lifecycle operations
//! answer (`tests/all/incorrect_instance_state_refusals.rs`), which names the instance and
//! the action its state does not admit.
//!
//! Every assertion is driven from a TABLE whose rows differ along what the rule
//! generalises over — which half of the monitoring move is called, which unsupported state
//! the instance is held in, and whether the request names that instance alone or in a
//! batch beside one it can act on — so one recorded call about one terminated instance is a
//! single row of it. The positive control is beside it: the states the pair DOES support
//! are accepted over the same transport, so what the rows pin is the state and not the
//! operation.

use service_com_amazonaws_ec2_amazonec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, InstanceStateName,
};

use super::ec2_query;
use super::wire::{child, child_text, document};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// Both halves of the monitoring move, which the rule holds over.
const MONITORING_ACTIONS: [&str; 2] = ["MonitorInstances", "UnmonitorInstances"];

/// The code the refusal carries.
const INVALID_STATE: &str = "InvalidState";

/// The whole sentence the refusal carries, with nothing interpolated into it.
const NOT_SUPPORTED: &str = "Instance is not in a supported state.";

/// The lifecycle states the pair does NOT support: an instance on its way down or already
/// down has no guest being measured, so there is no monitoring cadence to change.
const UNSUPPORTED: &[InstanceStateName] = &[
    InstanceStateName::Stopped,
    InstanceStateName::Stopping,
    InstanceStateName::ShuttingDown,
    InstanceStateName::Terminated,
];

/// The lifecycle states it does support — the positive control of every row below.
const SUPPORTED: &[InstanceStateName] = &[InstanceStateName::Running, InstanceStateName::Pending];

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// The `InstanceId.N` fields one request carries for `instance_ids`, as ec2Query flattens
/// the list both operations spell `InstanceId` on the wire.
fn instance_fields(instance_ids: &[String]) -> Vec<(String, String)> {
    instance_ids
        .iter()
        .enumerate()
        .map(|(index, instance_id)| p(&format!("InstanceId.{}", index + 1), instance_id.clone()))
        .collect()
}

/// One EC2 emulator over its own engine. The engine is kept so an instance can be held in
/// a state no sequence of calls leaves it in long enough to call about.
struct Ec2 {
    router: smithy_server_runtime::MultiServiceRouter,
    engine: ::std::sync::Arc<engine::InMemoryEngine>,
}

impl Ec2 {
    fn new() -> Self {
        let (router, engine) = ec2_query::emulator();
        Self { router, engine }
    }

    async fn call(&self, action: &str, params: Vec<(String, String)>) -> (u16, String) {
        ec2_query::call(&self.router, REGION, action, &params).await
    }

    /// Launch one instance and answer the id this account issued it.
    async fn launch(&self, case: &str) -> String {
        let (status, body) = self
            .call(
                "RunInstances",
                vec![p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from RunInstances, got {status} with body {body}"
        );
        ec2_query::element(&body, "instanceId").unwrap_or_else(|| {
            panic!("case {case:?}: RunInstances must report the instance it minted: {body}")
        })
    }

    /// Hold one instance still in `state`.
    ///
    /// The transient lifecycle states cannot be held across a call through the operations
    /// that start them — the clock moves the instance on whether or not anybody calls — so
    /// the state is written into the store the way the sibling suites seed the branches no
    /// sequence of calls can hold still.
    fn park(&self, case: &str, instance_id: &str, state: InstanceStateName) {
        let store = self.engine.state::<Ec2State>();
        let mut guard = store.lock();
        let instance = guard.instances.get_mut(instance_id).unwrap_or_else(|| {
            panic!("case {case:?}: the store must hold the launched instance {instance_id}")
        });
        instance.state = state;
        instance.pending_transition = None;
    }

    /// The detailed-monitoring state `DescribeInstances` reports for one instance, read
    /// through the document so the instance's own `<monitoring><state>` is never taken for
    /// an identically named member of something nested inside it.
    async fn monitoring_state(&self, case: &str, instance_id: &str) -> String {
        let (status, body) = self
            .call("DescribeInstances", vec![p("InstanceId.1", instance_id)])
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeInstances, got {status} with \
             body {body}"
        );
        let parsed = document(case, &body);
        let instance = parsed
            .descendants()
            .find(|node| {
                node.is_element() && child_text(*node, "instanceId").as_deref() == Some(instance_id)
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: DescribeInstances must report {instance_id}: {body}")
            });
        child(instance, "monitoring")
            .and_then(|monitoring| child_text(monitoring, "state"))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the report of {instance_id} must carry its monitoring \
                     state: {body}"
                )
            })
    }
}

/// How one row names the instance whose state is refused.
#[derive(Debug, Clone, Copy)]
enum Names {
    /// That instance alone.
    Alone,
    /// It second in a batch, behind one the call can act on.
    SecondInABatch,
    /// It twice over, which deduplicates to the one instance.
    Twice,
}

/// The rule, over every (operation, unsupported state, request shape) triple: HTTP 400,
/// `InvalidState`, and the one sentence with nothing of the request in it.
#[tokio::test]
async fn a_monitoring_call_refuses_an_unsupported_state_with_the_fixed_sentence() {
    for action in MONITORING_ACTIONS {
        for state in UNSUPPORTED {
            for names in [Names::Alone, Names::SecondInABatch, Names::Twice] {
                let case = format!("{action} on an instance in {} {names:?}", state.as_str());
                let ec2 = Ec2::new();
                // Still `pending`, which both halves support: a batch is refused for the
                // parked instance alone.
                let up = ec2.launch(&case).await;
                let parked = ec2.launch(&case).await;
                ec2.park(&case, &parked, *state);

                let named = match names {
                    Names::Alone => vec![parked.clone()],
                    Names::SecondInABatch => vec![up.clone(), parked.clone()],
                    Names::Twice => vec![parked.clone(), parked.clone()],
                };
                let before = ec2.monitoring_state(&case, &up).await;

                let (status, body) = ec2.call(action, instance_fields(&named)).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with body {body}"
                );
                let code = ec2_query::error_code(&body);
                assert_eq!(
                    code, INVALID_STATE,
                    "case {case:?}: expected {INVALID_STATE}, got {code:?} in {body}"
                );
                let message = ec2_query::error_message(&body);
                assert_eq!(
                    message, NOT_SUPPORTED,
                    "case {case:?}: expected {NOT_SUPPORTED:?}, got {message:?}"
                );
                // Nothing of the request or the store is in the sentence.
                assert!(
                    !message.contains(&parked)
                        && !message.contains(&up)
                        && !message.contains(state.as_str()),
                    "case {case:?}: the refusal must name neither an instance nor a state, \
                     got {message:?}"
                );
                // And the instance the call could have acted on was not acted on.
                let after = ec2.monitoring_state(&case, &up).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: a refused {action} must leave the monitoring of {up} as \
                     it was, expected {before:?}, got {after:?}"
                );
            }
        }
    }
}

/// The same rule read as an invariant: for ONE instance and ONE operation the text is
/// byte-identical whichever unsupported state the instance is held in, and the same text
/// for both halves of the move — so nothing in it is read out of the store.
#[tokio::test]
async fn the_refusal_text_varies_with_neither_the_state_nor_the_half_of_the_move() {
    for action in MONITORING_ACTIONS {
        let case = format!("{action} across every state it does not support");
        let ec2 = Ec2::new();
        let instance_id = ec2.launch(&case).await;

        for state in UNSUPPORTED {
            ec2.park(&case, &instance_id, *state);
            let (status, body) = ec2
                .call(action, instance_fields(std::slice::from_ref(&instance_id)))
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: in {} the call must be refused, got {status} with body {body}",
                state.as_str()
            );
            let code = ec2_query::error_code(&body);
            assert_eq!(
                code, INVALID_STATE,
                "case {case:?}: in {} the code must be {INVALID_STATE}, got {code:?}",
                state.as_str()
            );
            let message = ec2_query::error_message(&body);
            assert_eq!(
                message, NOT_SUPPORTED,
                "case {case:?}: in {} the text must still be {NOT_SUPPORTED:?}, got \
                 {message:?}",
                state.as_str()
            );
        }
    }
}

/// The positive control: in the states the pair DOES support the same request over the
/// same transport is served, so what the rows above pin is the instance's state and not
/// the operation.
#[tokio::test]
async fn a_supported_state_is_served_rather_than_refused() {
    for action in MONITORING_ACTIONS {
        for state in SUPPORTED {
            let case = format!("{action} on an instance in {}", state.as_str());
            let ec2 = Ec2::new();
            let instance_id = ec2.launch(&case).await;
            ec2.park(&case, &instance_id, *state);

            let (status, body) = ec2
                .call(action, instance_fields(std::slice::from_ref(&instance_id)))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            let answered = ec2_query::elements(&body, "instanceId");
            assert_eq!(
                answered,
                vec![instance_id.clone()],
                "case {case:?}: the answer must report the instance it acted on, got \
                 {answered:?} in {body}"
            );
        }
    }
}
