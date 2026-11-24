// SPDX-License-Identifier: CC0-1.0

//! A deploying stack read by MANY callers at once still completes, and the properties
//! CloudFormation applies with calls of the owning service's own — an
//! `AWS::DirectoryService::MicrosoftAD`'s `CreateAlias` and `EnableSso` — reach the directory
//! exactly once.
//!
//! RULE under test: **a provisioning pass carries no claim on the resource it answers for — one
//! is entered per read of the stack — so two passes can reach the same resource with the same
//! work left to do, and a call the owning service refuses because the OTHER pass already did
//! that work is not the resource's failure: what decides is the resource's own state after the
//! call, never the wording of the refusal.**
//!
//! Why it matters: the alias CloudFormation asks for is derived from the id Directory Service
//! minted, so two overlapping passes ask for the SAME alias and the loser is told the alias is
//! taken — by its own twin. Reported as the resource's failure, a stack that deployed perfectly
//! well rolls back, and whether it does depends on how fast the host is and how often a client
//! polls. The same stack deployed by a single poller completes, which is what makes such a
//! failure so hard to read.
//!
//! The cases differ along what the rule generalises over: HOW MANY callers read the stack at the
//! same instant — one, and a batch of eight — which is what opens the window at all. Each waits
//! for the CONDITION, the stack resting in a status that is not in progress, under the generous
//! deadline the sibling suite uses, so a loaded or a faster host changes nothing about the
//! outcome.
//!
//! The directory, the network it sits on and the calls that drive all three services come from
//! `cloudformation_provisions_directory_service_directories.rs`, which pins what a stack
//! declaring either directory type publishes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{opt_element, physical_ids_of};
use crate::cloudformation_provisions_directory_service_directories::{
    cloudformation, described, emulator, network, LOOK_AGAIN_AFTER, PASSWORD, SETTLE_DEADLINE,
};

/// The logical id every template here declares the directory under.
const DIRECTORY: &str = "Directory";

/// The members a described directory reports that say the two flags were applied: the alias
/// assigned to it — a directory with none reports its own id — and the single sign-on switch.
const ALIAS_MEMBER: &str = "Alias";
const SSO_ENABLED_MEMBER: &str = "SsoEnabled";

/// The template of one case: one Managed Microsoft AD asking for both of the properties
/// CloudFormation applies with calls of Directory Service's own, once the directory serves.
fn template_of(vpc_id: &str, first: &str, second: &str) -> String {
    json!({
        "Resources": {
            DIRECTORY: {
                "Type": "AWS::DirectoryService::MicrosoftAD",
                "Properties": {
                    "Name": "flags.example.com",
                    "Password": PASSWORD,
                    "CreateAlias": true,
                    "EnableSso": true,
                    "VpcSettings": {"VpcId": vpc_id, "SubnetIds": [first, second]},
                },
            }
        },
        "Outputs": {"DirectoryRef": {"Value": {"Ref": DIRECTORY}}}
    })
    .to_string()
}

/// One batch of reads of one stack: ONE read, or EIGHT of them in flight together, which is what
/// makes the provisioning passes they drive overlap.
async fn read_batch(
    router: &MultiServiceRouter,
    stack: &str,
    readers: Readers,
) -> Vec<(u16, String)> {
    // The parameter list outlives every read of the batch, so each future borrows it rather than
    // a temporary of its own.
    let parameters = [("StackName", stack)];
    let read = || cloudformation(router, "DescribeStacks", &parameters);
    match readers {
        Readers::One => vec![read().await],
        Readers::Eight => {
            let answers = ::tokio::join!(
                read(),
                read(),
                read(),
                read(),
                read(),
                read(),
                read(),
                read()
            );
            vec![
                answers.0, answers.1, answers.2, answers.3, answers.4, answers.5, answers.6,
                answers.7,
            ]
        }
    }
}

/// How many callers read the stack at the same instant.
#[derive(Debug, Clone, Copy)]
enum Readers {
    One,
    Eight,
}

/// Deploy one stack and wait for it to settle, reading it in batches of `readers`; answers a
/// FRESH read of the settled stack.
///
/// The reads of one batch are answered at instants of their own — eight calls in flight together
/// each drive a provisioning pass, and the batch straddles the instant the stack settles — so no
/// body from the batch that saw it settle is the stack's final word. The verdict is read once
/// more, after the wait is over.
async fn deployed_under(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    readers: Readers,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut last = String::new();
        let mut settled = false;
        for (status, body) in read_batch(router, stack, readers).await {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
            );
            settled |= opt_element(&body, "StackStatus")
                .is_some_and(|reported| !reported.ends_with("_IN_PROGRESS"));
            last = body;
        }
        if settled {
            let (status, body) =
                cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
            );
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {stack} to settle, last read {last}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The two flags, as Directory Service reports them for the directory the stack created: the
/// alias assigned to it and whether single sign-on is on.
async fn reported_flags(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (String, String, Value) {
    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
         {resources}"
    );
    let directory_id = physical_ids_of(&resources)
        .get(DIRECTORY)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no physical id for {DIRECTORY} in {resources}"));
    let description = described(router, &directory_id, case)
        .await
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected Directory Service to hold {directory_id}")
        });
    let alias = description
        .get(ALIAS_MEMBER)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (directory_id, alias, description)
}

/// What every case asserts: the stack completed, and the two flags reached the directory — the
/// alias assigned (a directory with none reports its own id) and single sign-on on.
async fn assert_deployed_with_both_flags(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    settled: &str,
) {
    assert_eq!(
        opt_element(settled, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete however many callers read the stack, got \
         {settled}"
    );
    let (directory_id, alias, description) = reported_flags(router, case, stack).await;
    assert_ne!(
        alias, directory_id,
        "case {case:?}: expected an alias assigned to {directory_id}, got {description}"
    );
    assert!(
        !alias.is_empty(),
        "case {case:?}: expected an alias assigned to {directory_id}, got {description}"
    );
    assert_eq!(
        description.get(SSO_ENABLED_MEMBER),
        Some(&Value::Bool(true)),
        "case {case:?}: expected single sign-on on for {directory_id}, got {description}"
    );
}

#[tokio::test]
async fn one_reader_of_a_deploying_stack_leaves_both_flags_applied() {
    let case = "a stack read by one caller while it deploys";
    let stack = "ds-flags-one-reader";
    let router = emulator();
    let net = network(
        &router,
        "10.60.0.0/16",
        &[("10.60.0.0/24", "us-west-2a"), ("10.60.1.0/24", "us-west-2b")],
    )
    .await;
    let template = template_of(&net.vpc_id, &net.subnets[0].id, &net.subnets[1].id);
    let settled = deployed_under(&router, case, stack, &template, Readers::One).await;
    assert_deployed_with_both_flags(&router, case, stack, &settled).await;
}

#[tokio::test]
async fn eight_readers_of_a_deploying_stack_leave_both_flags_applied() {
    let case = "a stack read by eight callers at the same instant";
    let stack = "ds-flags-eight-readers";
    let router = emulator();
    let net = network(
        &router,
        "10.61.0.0/16",
        &[("10.61.0.0/24", "us-west-2b"), ("10.61.1.0/24", "us-west-2c")],
    )
    .await;
    let template = template_of(&net.vpc_id, &net.subnets[0].id, &net.subnets[1].id);
    let settled = deployed_under(&router, case, stack, &template, Readers::Eight).await;
    assert_deployed_with_both_flags(&router, case, stack, &settled).await;
}
