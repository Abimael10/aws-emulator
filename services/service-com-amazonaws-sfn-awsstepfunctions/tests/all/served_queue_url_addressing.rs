// SPDX-License-Identifier: CC0-1.0

//! Where a `Task` state's send is addressed when the queue url it carries names NO Region
//! — the url this emulator reports while it is SERVED at an address of its own — driven
//! over both services' own wire protocols through the production `MultiServiceRouter` with
//! Step Functions, Amazon SQS and IAM mounted on ONE engine.
//!
//! RULE: a service-integration call is performed against the TARGET's own Region — the
//! Region the queue its `QueueUrl` addresses lives in — and the resource its access request
//! is decided over is that queue's own ARN, Region included. A queue url AWS publishes
//! carries that Region in its authority (`sqs.<region>.amazonaws.com`); a url this emulator
//! reports while it is served carries the one address the emulator answers on, which serves
//! every Region at once, so where the queue is comes from the account's own queues instead
//! of from the Region the execution happens to run in.
//!
//! `tests/all/cross_region_task_targets.rs` is the same rule over urls that DO name a
//! Region, and this suite borrows its harness (the emulator, the role, the machine and the
//! queue readers) rather than restating the wiring.
//!
//! The rows span what the rule generalises over: the Region the far queue lives in (three
//! continents, and the execution's own Region as the boundary case), the ADDRESS the
//! emulator is served at (a loopback literal, a host name, an IPv6 literal, each with a
//! port of its own), the integration FAMILY the `Task` names the send through, and which
//! Region the grant on the queue names — the far queue's own, the same NAME in the
//! execution's Region, and nothing about the action at all. The two boundaries are their
//! own tests: a name taken in BOTH Regions, and a url that addresses no queue this endpoint
//! holds.

use serde_json::{json, Value};

use crate::cross_region_task_targets::{
    queue_arn, send_grant, Emulator, Family, BODY,
};
use crate::state_machine_lifecycle::{string, ACCOUNT, REGION};
use crate::task_refusal_session_principal::{definition, session_principal};

/// What a row's execution role grants `sqs:SendMessage` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The far queue's own ARN, in the Region that queue lives in.
    FarQueue,
    /// The same queue NAME in the Region the execution runs in, which is another queue.
    NearQueueName,
    /// Nothing about the action at all.
    Unrelated,
}

/// One row: the address the emulator is served at, the Region the target queue lives in,
/// the family the `Task` names the send through, and what the role grants.
struct Row {
    case: &'static str,
    /// The address the emulator is served at, as a data port publishes it.
    served_at: &'static str,
    /// The Region the target queue is created in.
    queue_region: &'static str,
    family: Family,
    grant: Grant,
}

const ROWS: &[Row] = &[
    Row {
        case: "another Region of the same continent, granted on the far queue",
        served_at: "https://127.0.0.1:9297",
        queue_region: "us-east-1",
        family: Family::Optimized,
        grant: Grant::FarQueue,
    },
    Row {
        case: "a Region on another continent, through the AWS SDK family",
        served_at: "https://emulator.test:8443",
        queue_region: "eu-west-1",
        family: Family::AwsSdk,
        grant: Grant::FarQueue,
    },
    Row {
        case: "a Region on a third continent, the emulator served at an IPv6 literal",
        served_at: "https://[::1]:36181",
        queue_region: "ap-southeast-2",
        family: Family::Optimized,
        grant: Grant::FarQueue,
    },
    Row {
        // The boundary: the execution's own Region is addressed by such a url no
        // differently from any other.
        case: "the execution's own Region, granted on that queue",
        served_at: "https://127.0.0.1:19297",
        queue_region: REGION,
        family: Family::Optimized,
        grant: Grant::FarQueue,
    },
    Row {
        case: "the grant names the same queue NAME in the execution's Region",
        served_at: "https://127.0.0.1:9297",
        queue_region: "us-east-1",
        family: Family::Optimized,
        grant: Grant::NearQueueName,
    },
    Row {
        case: "the grant says nothing about the action, through the AWS SDK family",
        served_at: "https://emulator.test:8443",
        queue_region: "sa-east-1",
        family: Family::AwsSdk,
        grant: Grant::Unrelated,
    },
];

/// Every row: the send is made in the Region that holds the queue the url addresses, and
/// the access request it is subject to is over THAT queue's ARN — so a grant on it is
/// what allows the send, and a grant on the same name elsewhere is a grant on another
/// queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_less_url_is_addressed_to_the_region_holding_its_queue() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::served_at(row.served_at);
        let name = format!("served-target-{index}");
        let url = emulator.queue_in(row.queue_region, &name).await;
        // The url a client is handed while the emulator is served names the address it is
        // served at and NOT the queue's Region, which is the whole point of the row.
        let expected_url = format!("{}/{ACCOUNT}/{name}", row.served_at);
        assert_eq!(
            url, expected_url,
            "case {case:?}: expected the served url {expected_url:?}, got {url:?}"
        );

        let far_arn = queue_arn(row.queue_region, &name);
        let policy = match row.grant {
            Grant::FarQueue => send_grant(&far_arn),
            Grant::NearQueueName => send_grant(&queue_arn(REGION, &name)),
            // The log writes a state machine makes, which say nothing about any send.
            Grant::Unrelated => json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": ["logs:CreateLogDelivery", "logs:PutLogEvents"],
                    "Resource": "*",
                }],
            }),
        };
        let role_arn = emulator
            .role(&format!("ServedUrlRole{index}"), &policy)
            .await;
        let machine = emulator
            .machine(
                &format!("served-url-{index}"),
                &definition(row.family.resource(), &row.family.parameters(&url)),
                &role_arn,
            )
            .await;
        let described = emulator.ran(&machine, "only", case).await;

        if row.grant == Grant::FarQueue {
            assert_eq!(
                (
                    described.get("status").and_then(Value::as_str),
                    described.get("error").and_then(Value::as_str),
                ),
                (Some("SUCCEEDED"), None),
                "case {case:?}: expected SUCCEEDED, got {described:?}"
            );
            let bodies = emulator.bodies_in(row.queue_region, &url).await;
            assert_eq!(
                bodies,
                vec![BODY.to_string()],
                "case {case:?}: expected the message in the queue of {}, got {bodies:?}",
                row.queue_region
            );
            continue;
        }

        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(row.family.refusal_error())),
            "case {case:?}: expected the send to be refused, got {described:?}"
        );
        // Amazon SQS's own answer to an unauthorized call: the session the call was made
        // as, the action in the lower case that service words a refusal in, and the ARN of
        // the queue the url ADDRESSED — the Region holding it, not the execution's. The
        // metadata each family's client writes after it is
        // `tests/all/task_target_authorization.rs`'s subject.
        let cause = string(&described, "cause");
        let principal = session_principal(&role_arn, &cause);
        let expected = format!(
            "User: {principal} is not authorized to perform: sqs:sendmessage on resource: \
             {far_arn} because no identity-based policy allows the sqs:sendmessage action ("
        );
        assert!(
            cause.starts_with(&expected),
            "case {case:?}: expected the cause to open {expected:?}, got {cause:?}"
        );
        // And nothing was sent: the call the policies withheld was never made.
        let depth = emulator.depth_in(row.queue_region, &url).await;
        assert_eq!(
            depth, "0",
            "case {case:?}: expected the queue of {} to stay empty, got {depth} message(s)",
            row.queue_region
        );
    }
}

/// RULE, where the name is taken in both Regions: this emulator answers every Region at
/// one address, so a url naming that address addresses the caller's OWN Region's queue —
/// exactly as Amazon SQS resolves such a url against the Region the request is signed for
/// — and the send is authorized over that queue's ARN.
///
/// The rows differ along the Region the OTHER queue of the same name lives in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_less_url_addresses_the_executions_own_region_when_it_holds_the_name() {
    // (case, the Region holding the other queue of the same name)
    let rows: [(&str, &str); 3] = [
        ("the other queue is on the same continent", "us-east-1"),
        ("the other queue is on another continent", "eu-west-1"),
        ("the other queue is on a third continent", "ap-northeast-1"),
    ];

    for (index, (case, other_region)) in rows.into_iter().enumerate() {
        let emulator = Emulator::served_at("https://127.0.0.1:9297");
        let name = format!("taken-in-both-{index}");
        let near_url = emulator.queue_in(REGION, &name).await;
        let far_url = emulator.queue_in(other_region, &name).await;
        // One served address, so both queues are addressed by the same string: which queue
        // it names is what the row is about.
        assert_eq!(
            near_url, far_url,
            "case {case:?}: expected one served url for the name in both Regions, got \
             {near_url:?} and {far_url:?}"
        );

        let near_arn = queue_arn(REGION, &name);
        let role_arn = emulator
            .role(&format!("NearNameRole{index}"), &send_grant(&near_arn))
            .await;
        let machine = emulator
            .machine(
                &format!("taken-in-both-{index}"),
                &definition(
                    Family::Optimized.resource(),
                    &Family::Optimized.parameters(&near_url),
                ),
                &role_arn,
            )
            .await;
        let described = emulator.ran(&machine, "only", case).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("SUCCEEDED"), None),
            "case {case:?}: expected SUCCEEDED under the grant on {near_arn}, got {described:?}"
        );
        let bodies = emulator.bodies_in(REGION, &near_url).await;
        assert_eq!(
            bodies,
            vec![BODY.to_string()],
            "case {case:?}: expected the message in the queue of {REGION}, got {bodies:?}"
        );
        let depth = emulator.depth_in(other_region, &far_url).await;
        assert_eq!(
            depth, "0",
            "case {case:?}: expected the queue of {other_region} to stay empty, got {depth} \
             message(s)"
        );
    }
}

/// RULE, at its other boundary: a url that addresses no queue THIS endpoint holds leaves
/// nothing to resolve, so the call stays in the Region the execution runs in — and the ARN
/// it is authorized over is the one in that Region.
///
/// An address of ANOTHER account is such a url: this endpoint answers for one account's
/// addresses, so a queue of the same name this account holds elsewhere is not what it names.
/// The rows differ along the Region this account's queue of that name lives in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_url_of_another_account_is_addressed_where_the_execution_runs() {
    /// An account this endpoint does not answer for, which is every account but
    /// [`ACCOUNT`].
    const OTHER_ACCOUNT: &str = "999999999999";

    // (case, the Region this account's queue of the name lives in)
    let rows: [(&str, &str); 3] = [
        ("this account holds the name on the same continent", "us-east-1"),
        ("this account holds the name on another continent", "eu-west-1"),
        ("this account holds the name on a third continent", "ap-northeast-1"),
    ];

    for (index, (case, other_region)) in rows.into_iter().enumerate() {
        let emulator = Emulator::served_at("https://127.0.0.1:9297");
        let name = format!("other-account-{index}");
        // This account's queue of the name, in a Region of its own: the url below names the
        // same NAME under another account, so this queue is not what it addresses.
        let held_url = emulator.queue_in(other_region, &name).await;
        let elsewhere = format!("https://127.0.0.1:9297/{OTHER_ACCOUNT}/{name}");
        // The role may send to this account's queue and to nothing else, so what refuses
        // the call is the Region an unresolvable address leaves it in.
        let role_arn = emulator
            .role(
                &format!("OtherAccountRole{index}"),
                &send_grant(&queue_arn(other_region, &name)),
            )
            .await;
        let machine = emulator
            .machine(
                &format!("other-account-{index}"),
                &definition(
                    Family::Optimized.resource(),
                    &Family::Optimized.parameters(&elsewhere),
                ),
                &role_arn,
            )
            .await;
        let described = emulator.ran(&machine, "only", case).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(Family::Optimized.refusal_error())),
            "case {case:?}: expected the send to be refused, got {described:?}"
        );
        let cause = string(&described, "cause");
        let principal = session_principal(&role_arn, &cause);
        let near_arn = queue_arn(REGION, &name);
        let expected = format!(
            "User: {principal} is not authorized to perform: sqs:sendmessage on resource: \
             {near_arn} because no identity-based policy allows the sqs:sendmessage action ("
        );
        assert!(
            cause.starts_with(&expected),
            "case {case:?}: expected the cause to open {expected:?}, got {cause:?}"
        );
        let depth = emulator.depth_in(other_region, &held_url).await;
        assert_eq!(
            depth, "0",
            "case {case:?}: expected the queue of {other_region} to stay empty, got {depth} \
             message(s)"
        );
    }
}
