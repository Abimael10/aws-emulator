// SPDX-License-Identifier: CC0-1.0

//! The instance-lifecycle WRITES are region-scoped.
//!
//! The rule under test: EC2 is a regional service reached through a regional endpoint, so
//! the instances an operation of it acts on are the ones of the Region the request was
//! signed for. An instance that runs in another Region is invisible there, and an id naming
//! one names no instance at all — `InvalidInstanceID.NotFound`, the same refusal
//! `DescribeInstances` answers for such an id — so the instance is left exactly as it was.
//!
//! The table's rows are the operations that MOVE an instance: a cross-region call to each
//! must refuse and change nothing, and the same call in the instance's own Region must be
//! served. A write accepted in the wrong Region is the damaging half: it moves a resource
//! AWS would have protected by refusing.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, described_instance_state, element, error_code, error_message, indexed};

/// The Region every instance of this suite is launched in.
const HOME: &str = "us-west-2";

/// Another Region of the same partition, reached through its own endpoint: the instances of
/// `HOME` are invisible here.
const ELSEWHERE: &str = "us-east-1";

/// An AMI id of the right FORM that this account holds no image for: which image an instance
/// came from has nothing to do with which Region it runs in, so one serves every row.
const AMI: &str = "ami-0123456789abcdef0";

/// The EC2 code for an instance id the account holds no instance for IN THE SIGNED REGION.
const NOT_FOUND: &str = "InvalidInstanceID.NotFound";

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// Launch one instance in `HOME` and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = call(
        router,
        HOME,
        "RunInstances",
        &[p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the launch must be served, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: expected an instanceId in {body}"))
}

/// Every operation that moves an instance refuses an id of another Region, leaves the
/// instance where it was, and serves the same call in the instance's own Region.
#[tokio::test]
async fn an_instance_of_another_region_cannot_be_moved() {
    // The operations that move an instance, each named by the action a caller sends. Every
    // one of them is served against a RUNNING instance in its own Region: a start reports
    // the running instance it found, a reboot queues one, a stop and a terminate begin their
    // own transition.
    let rows: [&str; 4] = [
        "StartInstances",
        "RebootInstances",
        "StopInstances",
        "TerminateInstances",
    ];
    for action in rows {
        let case = action;
        let (router, _engine) = super::ec2_query::emulator();
        let instance_id = launch(&router, case).await;
        let before = described_instance_state(&router, HOME, &instance_id).await;

        let (status, body) = call(
            &router,
            ELSEWHERE,
            action,
            &indexed("InstanceId", &[&instance_id]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: an instance of another Region names no instance here, so the \
             call is refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND}, got {body}"
        );
        assert!(
            error_message(&body).contains(&instance_id),
            "case {case:?}: the refusal names the id the request sent, got {body}"
        );

        // Nothing moved: the refusal is a refusal, not a partial write.
        let after = described_instance_state(&router, HOME, &instance_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: the instance must be left exactly as it was, expected \
             {before:?}, got {after:?}"
        );

        // And the same call in the instance's OWN Region is served, so what the rows above
        // prove is the Region scope rather than a refusal of the operation itself.
        let (status, body) = call(
            &router,
            HOME,
            action,
            &indexed("InstanceId", &[&instance_id]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the same call in the instance's own Region must be served, got \
             {status} with body {body}"
        );
    }
}
