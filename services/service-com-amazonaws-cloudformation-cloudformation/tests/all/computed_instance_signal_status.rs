// SPDX-License-Identifier: CC0-1.0

//! The status an instance's boot configuration asks it to SIGNAL, over the awsQuery wire.
//!
//! RULE: **the status a signalled resource's instance asks its stack for is the status
//! PERFORMING that instance's boot configuration asks for — not one read off the text of the
//! command line.** A bootstrap decides what it reports the way a program does: a conditional
//! over the exit code its own configuration states, a variable it keeps the word in, a retry
//! loop around the call, the option joined to its value. So a configuration whose performance
//! invokes `cfn-signal` or `aws cloudformation signal-resource` with a FAILURE asks for that
//! failure however it arrived at the word, and "a failure signal causes CloudFormation to
//! immediately fail the stack creation or update" (`SignalResource$Status`) — while one whose
//! performance arrives at a success releases the `CreationPolicy`'s `ResourceSignal` wait. The
//! signal is still recorded under the id of the instance the OWNING service reports serving
//! ("when you signal Amazon EC2 instances or Auto Scaling groups, specify the instance ID that
//! you are signaling as the unique ID"), whatever the invocation itself named.
//!
//! Why a client cares: the standard signal-based deploy writes its bootstrap as a script —
//! `cfn-init`, then a test, then `cfn-signal` with the outcome of that test — so the word
//! `FAILURE` is almost never a literal on the signalling line. A deploy whose fleet reports
//! failure must be the deploy that fails, and an `AutoScalingReplacingUpdate` is exactly the
//! policy a client reaches for to have a bad revision caught before it takes over.
//!
//! The rows span what the rule generalises over: where the status comes from (the word on the
//! line, a variable the bootstrap assigned it to, a variable a conditional over the exit code
//! assigned, the helper script's exit code out of a variable, and the exit code of the
//! INITIALISATION the documented bootstrap runs first — which is work this emulator did not
//! run, so nothing of its status is read as a bootstrap that failed), which command sends it
//! (the API's own command and the helper script `CreationPolicy` documents), how the option is
//! written (separated and joined to its value), how the bootstrap is laid out (one command per
//! line, an and-or list, and a script under `set -e`), and which status is asked for (a success
//! and a failure of each form). The UPDATE side of the same rule — an `UpdatePolicy`
//! `AutoScalingReplacingUpdate` whose replacement group's instances compute a FAILURE is
//! rejected and the stack rolled back — is `replacing_update_signal_verdict.rs`, which drives
//! the template and the readers declared here.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, router, xml_blocks, xml_text,
};

/// The AMI the rows' instances launch from, in Amazon EC2's own id form.
const IMAGE_ID: &str = "ami-00000000000000001";

/// A published instance type, which is what Amazon EC2 launches from.
const INSTANCE_TYPE: &str = "t3.micro";

/// The logical id the gate is declared on, which the signals name.
pub(crate) const GATE: &str = "AutoScalingGroup";

/// How many instances every row's group brings up, which is also its gate's `Count`.
const INSTANCES: usize = 2;

/// How long a row gives one stack operation to reach a terminal status: generous room for the
/// spans this emulator's own clock owes on a loaded machine, and far short of the `PT15M` every
/// row's `CreationPolicy` asks the wait to run for — so a row that fails here failed because
/// nothing terminal was ever reported rather than because the host was slow.
pub(crate) const SETTLING_DEADLINE: Duration = Duration::from_secs(120);

/// How often a row re-reads `DescribeStacks`, exactly as a client's waiter polls.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The bootstrap of an instance that signals through the API's own command with the status
/// written on the command line — `separator` is how the option carries its value, which a CLI
/// invocation may write either way.
fn states_the_status(status: &str, separator: &str) -> String {
    format!(
        "#!/bin/bash\nset -u\nINSTANCE_ID=$(curl -sS \
         http://169.254.169.254/latest/meta-data/instance-id)\naws cloudformation \
         signal-resource --region us-east-1 --stack-name my-stack --logical-resource-id {GATE} \
         --unique-id \"$INSTANCE_ID\" --status{separator}{status}\n"
    )
}

/// The bootstrap of an instance that keeps the status in a variable of its own and signals
/// THAT, which is how a script written once for either outcome states it.
fn keeps_the_status_in_a_variable(status: &str) -> String {
    format!(
        "#!/bin/bash\nset -u\nSIGNAL_STATUS=\"{status}\"\nINSTANCE_ID=$(curl -sS \
         http://169.254.169.254/latest/meta-data/instance-id)\naws cloudformation \
         signal-resource --region us-east-1 --stack-name my-stack --logical-resource-id {GATE} \
         --unique-id \"$INSTANCE_ID\" --status \"$SIGNAL_STATUS\"\n"
    )
}

/// The bootstrap AWS's own documented pattern comes to: the exit code of the work the instance
/// did, a conditional that turns it into the status to report, and the signalling call written
/// across continued lines inside a retry loop. The word `FAILURE` is nowhere near the
/// signalling line: only performing the program says which status it asks for.
pub(crate) fn computes_the_status_from_an_exit_code(exit_code: &str) -> String {
    format!(
        "#!/bin/bash\nset -u\nEXIT_CODE={exit_code}\nif [ \"$EXIT_CODE\" = \"0\" ]; then\n  \
         STATUS=SUCCESS\nelse\n  STATUS=FAILURE\nfi\nINSTANCE_ID=$(curl -sS \
         http://169.254.169.254/latest/meta-data/instance-id || echo \"\")\nfor _ in $(seq 1 \
         30); do\n  if aws cloudformation signal-resource \\\n    --region us-east-1 \\\n    \
         --stack-name \"my-stack\" \\\n    --logical-resource-id \"{GATE}\" \\\n    --unique-id \
         \"$INSTANCE_ID\" \\\n    --status \"${{STATUS}}\"; then\n    exit 0\n  fi\n  sleep \
         10\ndone\nexit 1\n"
    )
}

/// The same through the helper script `CreationPolicy` documents, which states the outcome as
/// the exit code it reports — here out of the variable the bootstrap kept it in.
fn reports_the_exit_code_through_the_helper(exit_code: &str) -> String {
    format!(
        "#!/bin/bash\nset -u\nEXIT_CODE={exit_code}\n/opt/aws/bin/cfn-signal -e \"$EXIT_CODE\" \
         --stack my-stack --resource {GATE} --region us-east-1\n"
    )
}

/// The canonical bootstrap `CreationPolicy` itself documents: `cfn-init` for the resource's
/// metadata, then `cfn-signal` reporting the status of THAT work. The exit code the signal
/// carries is the status of a command this emulator did not run, so it says nothing about the
/// bootstrap: the instance the owning service reports serving is one whose bootstrap came out.
fn signals_the_status_of_the_initialisation() -> String {
    format!(
        "#!/bin/bash\n/opt/aws/bin/cfn-init -v --stack my-stack --resource {GATE} --region \
         us-east-1\n/opt/aws/bin/cfn-signal -e $? --stack my-stack --resource {GATE} --region \
         us-east-1\n"
    )
}

/// The same bootstrap written as an and-or list, so the signalling command runs only where the
/// initialisation succeeded.
fn signals_only_if_the_initialisation_succeeded() -> String {
    format!(
        "#!/bin/bash\n/opt/aws/bin/cfn-init -v --stack my-stack --resource {GATE} --region \
         us-east-1 && /opt/aws/bin/cfn-signal -e 0 --stack my-stack --resource {GATE} --region \
         us-east-1\n"
    )
}

/// ...and the same under `set -e`, where any command that failed would end the script before
/// it ever signalled.
fn signals_under_exit_on_failure() -> String {
    format!(
        "#!/bin/bash\nset -e\nyum install -y httpd\nsystemctl enable \
         httpd\n/opt/aws/bin/cfn-signal -e 0 --stack my-stack --resource {GATE} --region \
         us-east-1\n"
    )
}

/// A template whose Auto Scaling group carries the `CreationPolicy` its instances signal, the
/// launch template they boot from, and the `UpdatePolicy` that asks for the GROUP to be
/// replaced by an update.
pub(crate) fn group_template(user_data: &str) -> String {
    json!({"Resources": {
        "Launch": {
            "Type": "AWS::EC2::LaunchTemplate",
            "Properties": {"LaunchTemplateData": {
                "ImageId": IMAGE_ID,
                "InstanceType": INSTANCE_TYPE,
                "UserData": {"Fn::Base64": user_data},
            }},
        },
        GATE: {
            "Type": "AWS::AutoScaling::AutoScalingGroup",
            "Properties": {
                "MinSize": "1",
                "MaxSize": "4",
                "DesiredCapacity": INSTANCES.to_string(),
                "AvailabilityZones": ["us-east-1a"],
                "LaunchTemplate": {
                    "LaunchTemplateId": {"Ref": "Launch"},
                    "Version": {"Fn::GetAtt": ["Launch", "LatestVersionNumber"]},
                },
            },
            "CreationPolicy": {
                "AutoScalingCreationPolicy": {"MinSuccessfulInstancesPercent": 100},
                "ResourceSignal": {"Count": INSTANCES, "Timeout": "PT15M"},
            },
            "UpdatePolicy": {"AutoScalingReplacingUpdate": {"WillReplace": true}},
        },
    }})
    .to_string()
}

/// One row: a bootstrap, the status the gate must receive from it, and the status the create
/// settles in.
struct Row {
    case: &'static str,
    stack: &'static str,
    bootstrap: String,
    /// The signal status the gate's events must report, as `SignalResource$Status` spells it.
    signalled: &'static str,
    /// The `StackStatus` the create settles in: a success quorum completes it, and a failure
    /// signal fails it at once and rolls it back.
    settles: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the status written on the command line, separated from its option",
            stack: "computed-signal-stated-success",
            bootstrap: states_the_status("SUCCESS", " "),
            signalled: "SUCCESS",
            settles: "CREATE_COMPLETE",
        },
        Row {
            case: "a FAILURE written on the command line, joined to its option",
            stack: "computed-signal-stated-failure",
            bootstrap: states_the_status("FAILURE", "="),
            signalled: "FAILURE",
            settles: "ROLLBACK_COMPLETE",
        },
        Row {
            case: "a SUCCESS the bootstrap keeps in a variable of its own",
            stack: "computed-signal-variable-success",
            bootstrap: keeps_the_status_in_a_variable("SUCCESS"),
            signalled: "SUCCESS",
            settles: "CREATE_COMPLETE",
        },
        Row {
            case: "a FAILURE the bootstrap keeps in a variable of its own",
            stack: "computed-signal-variable-failure",
            bootstrap: keeps_the_status_in_a_variable("FAILURE"),
            signalled: "FAILURE",
            settles: "ROLLBACK_COMPLETE",
        },
        Row {
            case: "a status a conditional over a zero exit code computed, signalled from a \
                   retry loop",
            stack: "computed-signal-conditional-success",
            bootstrap: computes_the_status_from_an_exit_code("0"),
            signalled: "SUCCESS",
            settles: "CREATE_COMPLETE",
        },
        Row {
            case: "a status a conditional over a non-zero exit code computed, signalled from a \
                   retry loop",
            stack: "computed-signal-conditional-failure",
            bootstrap: computes_the_status_from_an_exit_code("1"),
            signalled: "FAILURE",
            settles: "ROLLBACK_COMPLETE",
        },
        Row {
            case: "a zero exit code the helper script reports out of a variable",
            stack: "computed-signal-helper-success",
            bootstrap: reports_the_exit_code_through_the_helper("0"),
            signalled: "SUCCESS",
            settles: "CREATE_COMPLETE",
        },
        Row {
            case: "a non-zero exit code the helper script reports out of a variable",
            stack: "computed-signal-helper-failure",
            bootstrap: reports_the_exit_code_through_the_helper("1"),
            signalled: "FAILURE",
            settles: "ROLLBACK_COMPLETE",
        },
        Row {
            case: "the documented bootstrap: cfn-init, then cfn-signal reporting ITS exit code",
            stack: "computed-signal-after-init",
            bootstrap: signals_the_status_of_the_initialisation(),
            signalled: "SUCCESS",
            settles: "CREATE_COMPLETE",
        },
        Row {
            case: "the same bootstrap as an and-or list, signalling only where the \
                   initialisation succeeded",
            stack: "computed-signal-and-init",
            bootstrap: signals_only_if_the_initialisation_succeeded(),
            signalled: "SUCCESS",
            settles: "CREATE_COMPLETE",
        },
        Row {
            case: "a bootstrap under set -e, where a failed command would end it before it \
                   signalled",
            stack: "computed-signal-exit-on-failure",
            bootstrap: signals_under_exit_on_failure(),
            signalled: "SUCCESS",
            settles: "CREATE_COMPLETE",
        },
    ]
}

/// The `StackStatus` a stack reports right now.
async fn stack_status(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    let (code, body) = describe_stack(router, stack).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStacks({stack}) to answer, got {code} {body}"
    );
    xml_text(&body, "StackStatus").unwrap_or_default()
}

/// The status one stack operation settles in, polled until it is terminal exactly as a
/// client's waiter polls.
pub(crate) async fn settled_status(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> String {
    let deadline = Instant::now() + SETTLING_DEADLINE;
    loop {
        let status = stack_status(case, router, stack).await;
        if !status.ends_with("_IN_PROGRESS") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the operation to reach a terminal status within \
             {SETTLING_DEADLINE:?}, got {status:?}; events {:?}",
            event_trail(router, stack).await
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `UniqueId`s of the signals of one STATUS the stack's event stream reports for a
/// resource, sorted — which is how this service reports a received signal.
pub(crate) async fn signals_of_status(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    signal_status: &str,
) -> Vec<String> {
    let (_, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    let prefix = format!("Received {signal_status} signal with UniqueId ");
    let mut unique_ids: Vec<String> = xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(logical_id))
        .filter_map(|event| xml_text(event, "ResourceStatusReason"))
        .filter_map(|reason| reason.strip_prefix(&prefix).map(str::to_string))
        .collect();
    unique_ids.sort();
    unique_ids.dedup();
    unique_ids
}

/// Every `logical:status:reason` triple the stack's event stream carries, newest first: what a
/// failing row reports, so a failure names which resource failed and why.
pub(crate) async fn event_trail(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    xml_blocks(&body, "member")
        .iter()
        .map(|event| {
            format!(
                "{}:{}:{}",
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "ResourceStatus").unwrap_or_default(),
                xml_text(event, "ResourceStatusReason").unwrap_or_default()
            )
        })
        .collect()
}

#[tokio::test]
async fn the_status_a_bootstrap_computes_is_the_status_its_gate_receives() {
    for row in rows() {
        let case = row.case;
        let stack = row.stack;
        let router = router();
        let (code, body) = create_stack(&router, stack, &group_template(&row.bootstrap), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to succeed, got {code} {body}"
        );
        let status = settled_status(case, &router, stack).await;
        assert_eq!(
            status, row.settles,
            "case {case:?}: expected the create to settle on {:?}, got {status:?}; events {:?}",
            row.settles,
            event_trail(&router, stack).await
        );

        // The signal the gate received is the one the bootstrap's own performance asked for,
        // reported on the gated resource under the id of the instance that sent it.
        let signalled = signals_of_status(&router, stack, GATE, row.signalled).await;
        assert!(
            !signalled.is_empty(),
            "case {case:?}: expected a {} signal to be reported for {GATE}, got the events {:?}",
            row.signalled,
            event_trail(&router, stack).await
        );
        for unique_id in &signalled {
            assert!(
                crate::harness::is_service_assigned_id(unique_id, "i"),
                "case {case:?}: expected the signal's UniqueId to be the instance id that sent \
                 it, got {unique_id:?}"
            );
        }
        // ...and nothing of the other status was reported: a bootstrap asks for one of the
        // two, and which one is what the performance decided.
        let other = match row.signalled {
            "SUCCESS" => "FAILURE",
            _ => "SUCCESS",
        };
        let reported = signals_of_status(&router, stack, GATE, other).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: expected no {other} signal beside the {} one, got {reported:?}",
            row.signalled
        );
    }
}

#[tokio::test]
async fn nothing_but_the_computed_status_releases_the_gate() {
    const CASE: &str = "a bootstrap whose signalling command names ANOTHER resource";
    let stack = "computed-signal-elsewhere";
    let router = router();
    // The same computed status, signalled for a resource that is not the gate: the gate
    // receives nothing, so the create is still in flight once the instances are up.
    let elsewhere = computes_the_status_from_an_exit_code("0")
        .replace(&format!("\"{GATE}\""), "\"SomeOtherResource\"");
    let (code, body) = create_stack(&router, stack, &group_template(&elsewhere), &[]).await;
    assert_eq!(
        code, 200,
        "case {CASE:?}: expected CreateStack to succeed, got {code} {body}"
    );
    // Polled for the condition the gate is waiting on: the stack holds at its
    // `CreationPolicy` for the `PT15M` the declaration asks for, so what this waits for is the
    // instances the owning service brings up — and the gate having received nothing from them.
    let signalled = held_without_a_signal(CASE, &router, stack).await;
    assert!(
        signalled.is_empty(),
        "case {CASE:?}: expected no signal to be reported for {GATE}, got {signalled:?}"
    );
}

/// The signals the gate of `stack` has received once its instances have come up, for a stack
/// whose create is still being HELD at that gate.
///
/// Polled for the CONDITION rather than waited out: the group's instances come up on the
/// owning service's own clock, so this looks again until the stack reports them — and a create
/// that reached a terminal status in the meantime is one the gate did not hold, which is the
/// failure this reports.
async fn held_without_a_signal(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> Vec<String> {
    let deadline = Instant::now() + SETTLING_DEADLINE;
    loop {
        let status = stack_status(case, router, stack).await;
        assert_eq!(
            status, "CREATE_IN_PROGRESS",
            "case {case:?}: expected the create to be held at its unsignalled gate, got \
             {status:?}; events {:?}",
            event_trail(router, stack).await
        );
        let group = physical_id(router, stack, GATE).await;
        if !group.is_empty() {
            let signals = signals_of_status(router, stack, GATE, "SUCCESS").await;
            let mut reported = signals;
            reported.extend(signals_of_status(router, stack, GATE, "FAILURE").await);
            // The group exists and the gate is still holding: whatever its instances were told
            // to do, nothing of it reached this gate.
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to report its group within \
             {SETTLING_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The physical id the stack reports for one of its resources, or the empty string where it
/// reports none.
pub(crate) async fn physical_id(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> String {
    let (_, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}
