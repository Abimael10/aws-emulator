// SPDX-License-Identifier: CC0-1.0

//! How EC2 WORDS the refusal of an instance-ATTRIBUTE write or reset the instance's current
//! state does not admit, over the wire the service speaks (`ec2Query`, through the crate's
//! shared [`super::ec2_query`] harness).
//!
//! The rule under test: **an attribute that may only be written while the instance is
//! STOPPED, asked of an instance in another state, is refused `IncorrectInstanceState` with
//! one sentence naming the instance the request named and the state the operation requires —
//! `The instance '<instance-id>' is not in the 'stopped' state.`** "To modify some
//! attributes, the instance must be stopped" (`ModifyInstanceAttribute`) and "to reset the
//! kernel or ramdisk, the instance must be in a stopped state" (`ResetInstanceAttribute`) are
//! the same requirement, so both operations state it in the same sentence: the attribute is
//! not part of it, and neither is the state the instance is actually in.
//!
//! The rows differ along what the rule generalises over: the OPERATION (the write and the
//! reset), WHICH attribute of each is asked for, and WHICH state the instance is held in that
//! is not `stopped`. The negative half is a row too — the attributes each operation admits
//! while the instance runs are not refused at all.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INSTANCE_ATTRIBUTE_KERNEL,
    INSTANCE_ATTRIBUTE_RAMDISK, INSTANCE_ATTRIBUTE_SOURCE_DEST_CHECK, InstanceStateName,
    TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// The state both operations require for the attributes the rows below ask for. Spelled out
/// rather than imported, so a wrong constant in the implementation cannot make this suite
/// pass.
const REQUIRED_STATE: &str = "stopped";

/// The code EC2 answers this class with.
const INCORRECT_INSTANCE_STATE: &str = "IncorrectInstanceState";

/// The sentence the refusal must carry for `instance_id`.
fn expected_message(instance_id: &str) -> String {
    format!("The instance '{instance_id}' is not in the '{REQUIRED_STATE}' state.")
}

/// Launch one instance and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// Hold one instance still in `state`.
///
/// The transient lifecycle states cannot be held across a call through the operations that
/// start them — the clock moves the instance on whether or not anybody calls — so the state
/// is written into the store the way the sibling suites seed the branches no sequence of
/// calls can hold still.
fn park(
    engine: &engine::InMemoryEngine,
    case: &str,
    instance_id: &str,
    state: InstanceStateName,
) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let instance = guard.instances.get_mut(instance_id).unwrap_or_else(|| {
        panic!("case {case:?}: the store must hold the launched instance {instance_id}")
    });
    instance.state = state;
    instance.pending_transition = None;
}

/// Look again until `instance_id` reports `wanted`, or a generous deadline passes: the
/// condition is polled rather than waited out, so nothing here depends on how fast the host
/// runs.
async fn poll_until_state(router: &MultiServiceRouter, case: &str, instance_id: &str, wanted: &str) {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let state = super::ec2_query::described_instance_state(router, REGION, instance_id).await;
        if state == wanted {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {instance_id} must reach {wanted:?}, last read {state:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    }
}

/// One operation of the family, and the attribute it is asked for: the ec2Query action and
/// the fields that name that attribute beside the instance id.
struct Asked {
    case: &'static str,
    action: &'static str,
    fields: &'static [(&'static str, &'static str)],
    /// Whether a STOPPED instance admits this very request, so the positive half below can
    /// ask for it: the rows that name a resource of their own (a kernel image id) or a
    /// capability of the instance type are refused for THAT reason once the state admits
    /// them, which each attribute's own suite covers.
    admitted_when_stopped: bool,
}

/// The attribute writes that need a stopped instance — "the ones whose value is part of the
/// machine the instance RUNS AS" — and the resets of the pair
/// `ResetInstanceAttribute` documents a stopped instance for.
const STOPPED_ONLY: &[Asked] = &[
    Asked {
        case: "the instance type a modify would give it",
        action: "ModifyInstanceAttribute",
        fields: &[("InstanceType.Value", "t3.small")],
        admitted_when_stopped: true,
    },
    Asked {
        case: "the user data its next boot would read",
        action: "ModifyInstanceAttribute",
        // base64 of `#!/bin/sh`, which is how a client sends user data.
        fields: &[("UserData.Value", "IyEvYmluL3No")],
        admitted_when_stopped: true,
    },
    Asked {
        case: "the kernel a modify would boot",
        action: "ModifyInstanceAttribute",
        fields: &[("Kernel.Value", "aki-0123456789abcdef0")],
        admitted_when_stopped: false,
    },
    Asked {
        case: "its EBS optimization",
        action: "ModifyInstanceAttribute",
        fields: &[("EbsOptimized.Value", "true")],
        admitted_when_stopped: false,
    },
    Asked {
        case: "its Nitro Enclaves support",
        action: "ModifyInstanceAttribute",
        fields: &[("EnclaveOptions.Value", "true")],
        admitted_when_stopped: false,
    },
    Asked {
        case: "the kernel a reset would put back",
        action: "ResetInstanceAttribute",
        fields: &[("Attribute", INSTANCE_ATTRIBUTE_KERNEL)],
        admitted_when_stopped: true,
    },
    Asked {
        case: "the RAM disk a reset would put back",
        action: "ResetInstanceAttribute",
        fields: &[("Attribute", INSTANCE_ATTRIBUTE_RAMDISK)],
        admitted_when_stopped: true,
    },
];

/// The states that are not `stopped`, each of which the whole family refuses the attributes
/// above in.
const NOT_STOPPED: &[InstanceStateName] = &[
    InstanceStateName::Pending,
    InstanceStateName::Running,
    InstanceStateName::Stopping,
    InstanceStateName::ShuttingDown,
    InstanceStateName::Terminated,
];

impl Asked {
    /// The request fields for this attribute of `instance_id`.
    fn params(&self, instance_id: &str) -> Vec<Param> {
        let mut params = vec![param("InstanceId", instance_id)];
        params.extend(
            self.fields
                .iter()
                .map(|(name, value)| param(name, value)),
        );
        params
    }
}

/// THE RULE, over every (operation, attribute, state) the table spans.
#[tokio::test]
async fn an_attribute_that_needs_a_stopped_instance_names_that_state() {
    let (router, engine) = super::ec2_query::emulator();

    for asked in STOPPED_ONLY {
        for state in NOT_STOPPED {
            let case = format!(
                "{} of an instance in {} ({})",
                asked.action,
                state.as_str(),
                asked.case
            );
            let instance_id = launch(&router, &case).await;
            park(&engine, &case, &instance_id, *state);

            let (status, body) = call(
                &router,
                REGION,
                asked.action,
                &asked.params(&instance_id),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, INCORRECT_INSTANCE_STATE,
                "case {case:?}: expected {INCORRECT_INSTANCE_STATE}, got {code:?} in {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message,
                expected_message(&instance_id),
                "case {case:?}: expected {:?}, got {message:?}",
                expected_message(&instance_id)
            );
            assert!(
                !message.contains(state.as_str()),
                "case {case:?}: the refusal must not report the state the instance is in, \
                 got {message:?}"
            );
        }
    }
}

/// The same rule read as an invariant: for ONE attribute and ONE instance the text is
/// byte-identical whichever state that is not `stopped` the instance is held in, and it
/// names no attribute — so nothing in the sentence is read off the request's attribute or
/// off the store.
#[tokio::test]
async fn the_sentence_varies_with_neither_the_state_nor_the_attribute() {
    let (router, engine) = super::ec2_query::emulator();
    let case = "one instance across every state and attribute that is refused";
    let instance_id = launch(&router, case).await;
    let expected = expected_message(&instance_id);

    for asked in STOPPED_ONLY {
        for state in NOT_STOPPED {
            park(&engine, case, &instance_id, *state);
            let (status, body) = call(
                &router,
                REGION,
                asked.action,
                &asked.params(&instance_id),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: {} of {} in {} must be refused, got {status} with body {body}",
                asked.case,
                instance_id,
                state.as_str()
            );
            let message = error_message(&body);
            assert_eq!(
                message, expected,
                "case {case:?}: {} in {} must still answer {expected:?}, got {message:?}",
                asked.case,
                state.as_str()
            );
        }
    }
}

/// The NEGATIVE half: the attributes each operation admits while the instance RUNS are not
/// refused at all, so the sentence above is the answer to a state requirement rather than to
/// every call.
#[tokio::test]
async fn an_attribute_a_running_instance_admits_is_not_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "the attributes a running instance admits";
    let instance_id = launch(&router, case).await;
    poll_until_state(&router, case, &instance_id, "running").await;

    // (case, action, fields) — one policy attribute of the write, and the one attribute the
    // reset documents as resettable while the instance runs.
    let admitted: Vec<(&str, &str, Vec<Param>)> = vec![
        (
            "stop protection, a policy of the instance",
            "ModifyInstanceAttribute",
            vec![
                param("InstanceId", &instance_id),
                param("DisableApiStop.Value", "true"),
            ],
        ),
        (
            "the source/destination check a reset puts back",
            "ResetInstanceAttribute",
            vec![
                param("InstanceId", &instance_id),
                param("Attribute", INSTANCE_ATTRIBUTE_SOURCE_DEST_CHECK),
            ],
        ),
    ];

    for (case, action, params) in admitted {
        let (status, body) = call(&router, REGION, action, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {action} must succeed on a running instance, got {status} with \
             body {body}"
        );
    }
}

/// And the positive half of the requirement itself: once the instance IS stopped, the very
/// calls refused above succeed — so the refusal is about the state and nothing else.
#[tokio::test]
async fn a_stopped_instance_admits_the_attributes_that_need_one() {
    let (router, engine) = super::ec2_query::emulator();

    for asked in STOPPED_ONLY
        .iter()
        .filter(|asked| asked.admitted_when_stopped)
    {
        let case = format!("{} of a stopped instance ({})", asked.action, asked.case);
        let instance_id = launch(&router, &case).await;
        park(&engine, &case, &instance_id, InstanceStateName::Stopped);
        let (status, body) = call(
            &router,
            REGION,
            asked.action,
            &asked.params(&instance_id),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a stopped instance admits it, got {status} with body {body}"
        );
    }
}
