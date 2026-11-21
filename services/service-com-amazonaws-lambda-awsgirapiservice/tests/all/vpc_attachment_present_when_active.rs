// SPDX-License-Identifier: CC0-1.0

//! The VPC attachment of a function is in the account's Amazon EC2 inventory for the WHOLE
//! life of the function, so no read of the function ever reports it as running in a VPC it
//! has no presence in.
//!
//! RULE: "To connect a function to a VPC, Lambda creates an elastic network interface for
//! each subnet in your function's VPC configuration" — the create that applies the
//! configuration is what stands those interfaces up, so from the moment `CreateFunction`
//! answers (while the function is still `Pending`) and for as long as the function reports
//! `Active`, the account's own `DescribeNetworkInterfaces` reports exactly one interface per
//! configured subnet, each sitting in that subnet, carrying EVERY configured security group,
//! held (`in-use`) and attributable to the function; deleting the function gives them back,
//! so the same lookup then reports none.
//!
//! This is the lookup a client that holds only the VPC resources can make: a filter per
//! configured security group AND the subnet it is looking in — several filters are an AND —
//! which is how an in-VPC topology is checked without knowing anything else about the
//! interface. It is asserted at the state the client waits for: the function is POLLED until
//! its configuration reports `Active` rather than being read once, and the ids found while it
//! was still being provisioned must be the ids it holds afterwards, since the attachment the
//! function reaches `Active` with is the one it was created with.
//!
//! Every VPC, subnet and security group here is MINTED through Amazon EC2's own
//! `CreateVpc`/`CreateSubnet`/`CreateSecurityGroup`, and every interface is read back through
//! its own `DescribeNetworkInterfaces`, two services on the ONE engine the router mounts them
//! on — so nothing passes off a record of its own as one the account holds.
//!
//! The rows differ along what the rule generalises over: how many subnets the configuration
//! names and how many security groups those interfaces carry. A function created with no
//! `VpcConfig` at all is the control: the same lookups over the same subnets find nothing of
//! it.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use aws_sdk_ec2::types::{Filter, NetworkInterface};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, package_bytes, zip_file, ROLE,
};
use crate::vpc_attachment_interfaces::{
    create_attached, emulator, function_interfaces, interface_description, network,
    LAMBDA_INTERFACE_TYPE,
};
use crate::vpc_attachment_role_grant::another_group;

/// The state a function settles in, from the model's `State` enum (`Pending | Active | ...`),
/// which is the state a client waits for before using the function.
const STATE_ACTIVE: &str = "Active";

/// The state a function being provisioned is in, from the same enum: "When you create a
/// function, Lambda provisions an instance of the function and its supporting resources."
const STATE_PENDING: &str = "Pending";

/// "If the network interface is not attached to an instance, the status is `available`; if a
/// network interface is attached to an instance the status is `in-use`"
/// (`DescribeNetworkInterfaces` `status` filter): an interface a function's attachment holds
/// is held.
const STATUS_IN_USE: &str = "in-use";

/// How long the poll for the settled state may take before the case gives up — generous, so a
/// loaded host cannot turn a settled attachment into a failure. The poll waits for the
/// CONDITION, never for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two reads of the configuration.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One row of the table: its name, the CIDR prefix its own VPC takes, how many subnets the
/// configuration names and how many security groups its interfaces carry.
struct Row {
    case: &'static str,
    prefix: u8,
    subnets: usize,
    groups: usize,
}

/// `GetFunctionConfiguration` on the function `name`, as the body it answers.
async fn configuration(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let read = json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetFunctionConfiguration for {name:?}, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    read.json()
}

/// The `State` a configuration body reports.
fn reported_state(case: &str, body: &Value) -> String {
    body.get("State")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the configuration to report its State, got {body}")
        })
        .to_string()
}

/// Read the configuration of `name` until it reports [`STATE_ACTIVE`], or until
/// [`SETTLE_DEADLINE`] has passed — the condition a client waits for, polled rather than
/// slept for.
async fn settled(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let give_up = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = configuration(router, case, name).await;
        let state = reported_state(case, &body);
        if state == STATE_ACTIVE {
            return body;
        }
        assert!(
            Instant::now() < give_up,
            "case {case:?}: expected {name:?} to report State {STATE_ACTIVE:?}, got {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The interfaces `DescribeNetworkInterfaces` reports for a lookup naming EVERY group of
/// `group_ids` and the subnet `subnet_id` — one filter each, which the operation ANDs — the
/// lookup a client holding only the VPC resources makes.
async fn looked_up_in(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    group_ids: &[String],
    subnet_id: &str,
) -> Vec<NetworkInterface> {
    let mut request = ec2.describe_network_interfaces();
    for group_id in group_ids {
        request = request.filters(Filter::builder().name("group-id").values(group_id).build());
    }
    request = request.filters(
        Filter::builder()
            .name("subnet-id")
            .values(subnet_id)
            .build(),
    );
    let described = request.send().await.unwrap_or_else(|error| {
        panic!("case {case:?}: DescribeNetworkInterfaces must answer the lookup: {error:?}")
    });
    described.network_interfaces().to_vec()
}

/// The one interface the lookup over `group_ids` in `subnet_id` must find, with what makes it
/// attributable to the function `name` asserted on it.
async fn attributable_interface(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    label: &str,
    name: &str,
    vpc_id: &str,
    group_ids: &[String],
    subnet_id: &str,
) -> String {
    let found = looked_up_in(ec2, case, group_ids, subnet_id).await;
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected {label} to find one interface of {name:?} in {subnet_id:?} \
         carrying {group_ids:?}, got {found:?}"
    );
    let interface = &found[0];
    assert_eq!(
        interface.subnet_id(),
        Some(subnet_id),
        "case {case:?}: expected {label} to report the interface in {subnet_id:?}, got \
         {interface:?}"
    );
    assert_eq!(
        interface.vpc_id(),
        Some(vpc_id),
        "case {case:?}: expected {label} to report the interface in {vpc_id:?}, got \
         {interface:?}"
    );
    assert_eq!(
        interface.description(),
        Some(interface_description(name).as_str()),
        "case {case:?}: expected {label} to report a description naming {name:?}, got \
         {interface:?}"
    );
    assert_eq!(
        interface.interface_type().map(|kind| kind.as_str()),
        Some(LAMBDA_INTERFACE_TYPE),
        "case {case:?}: expected {label} to report the interface type of a function's own \
         interface, got {interface:?}"
    );
    assert_eq!(
        interface.status().map(|status| status.as_str()),
        Some(STATUS_IN_USE),
        "case {case:?}: expected {label} to report the interface as held, got {interface:?}"
    );
    let carried: BTreeSet<String> = interface
        .groups()
        .iter()
        .filter_map(|group| group.group_id().map(str::to_string))
        .collect();
    let configured: BTreeSet<String> = group_ids.iter().cloned().collect();
    assert_eq!(
        carried, configured,
        "case {case:?}: expected {label} to report the interface carrying {configured:?}, got \
         {carried:?}"
    );
    interface
        .network_interface_id()
        .unwrap_or_else(|| panic!("case {case:?}: expected {label} to report the interface's id"))
        .to_string()
}

/// `DeleteFunction` on the function `name`.
async fn delete_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let answer = json_call(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{name}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        204,
        "case {case:?}: expected HTTP 204 from DeleteFunction, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// RULE: a function created with a `VpcConfig` naming subnets and security groups holds one
/// interface per subnet in the account's EC2 inventory from the create's own answer onward and
/// for as long as it reports `Active`; deleting the function gives them back.
#[tokio::test]
async fn an_attached_function_holds_its_interfaces_from_the_create_until_it_is_deleted() {
    let emulator = emulator();
    let rows = [
        Row {
            case: "one subnet, one group",
            prefix: 91,
            subnets: 1,
            groups: 1,
        },
        Row {
            case: "two subnets, one group",
            prefix: 92,
            subnets: 2,
            groups: 1,
        },
        Row {
            case: "three subnets, two groups",
            prefix: 93,
            subnets: 3,
            groups: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let held = network(&emulator.ec2, case, row.prefix, row.subnets).await;
        let mut group_ids = vec![held.group_id.clone()];
        while group_ids.len() < row.groups {
            let next = group_ids.len();
            group_ids.push(another_group(&emulator.ec2, case, &held.vpc_id, next).await);
        }

        let name = format!("vpc-active-probe-{index}");
        let answer = create_attached(&emulator.router, &name, &held.subnet_ids, &group_ids).await;
        assert_eq!(
            answer.status,
            201,
            "case {case:?}: expected HTTP 201 from CreateFunction, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );

        // The attachment is already there while the function is still being provisioned: no
        // read of the function can report it as attached to a VPC it has no interface in.
        let mut placed: Vec<String> = Vec::with_capacity(held.subnet_ids.len());
        for subnet_id in &held.subnet_ids {
            placed.push(
                attributable_interface(
                    &emulator.ec2,
                    case,
                    "the lookup made while the create was settling",
                    &name,
                    &held.vpc_id,
                    &group_ids,
                    subnet_id,
                )
                .await,
            );
        }
        let state = reported_state(case, &configuration(&emulator.router, case, &name).await);
        assert!(
            state == STATE_PENDING || state == STATE_ACTIVE,
            "case {case:?}: expected a created function to report {STATE_PENDING:?} or \
             {STATE_ACTIVE:?}, got {state:?}"
        );

        // The provisioning span the clock owes is brought forward rather than waited out, and
        // the settled state is then POLLED for.
        make_activation_due(&emulator.engine);
        let settled_body = settled(&emulator.router, case, &name).await;
        assert_eq!(
            settled_body
                .pointer("/VpcConfig/VpcId")
                .and_then(Value::as_str),
            Some(held.vpc_id.as_str()),
            "case {case:?}: expected the settled configuration to report the VPC its subnets \
             belong to, got {settled_body}"
        );

        // The same interfaces, found by the same lookup, once the function is usable.
        let mut after: Vec<String> = Vec::with_capacity(held.subnet_ids.len());
        for subnet_id in &held.subnet_ids {
            after.push(
                attributable_interface(
                    &emulator.ec2,
                    case,
                    "the lookup made once the function reported Active",
                    &name,
                    &held.vpc_id,
                    &group_ids,
                    subnet_id,
                )
                .await,
            );
        }
        assert_eq!(
            after, placed,
            "case {case:?}: expected the function to keep the interfaces it was created with, \
             expected {placed:?}, got {after:?}"
        );

        // The control: a function of the SAME request but with no VpcConfig at all has no
        // presence in any of those subnets.
        let unattached = format!("vpc-absent-probe-{index}");
        let answer = create_function(
            &emulator.router,
            &json!({
                "FunctionName": unattached,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": { "ZipFile": zip_file(&package_bytes(64)) },
            }),
        )
        .await;
        assert_eq!(
            answer.status,
            201,
            "case {case:?}: expected HTTP 201 from the unattached CreateFunction, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let control = function_interfaces(&emulator.ec2, &unattached).await;
        assert!(
            control.is_empty(),
            "case {case:?}: expected a function with no VpcConfig to hold no interface, got \
             {control:?}"
        );

        // Deleting the attached function gives every interface back, so the lookup that found
        // them finds none.
        delete_function(&emulator.router, case, &name).await;
        for subnet_id in &held.subnet_ids {
            let found = looked_up_in(&emulator.ec2, case, &group_ids, subnet_id).await;
            assert!(
                found.is_empty(),
                "case {case:?}: expected the interfaces of a deleted function not to outlive \
                 it in {subnet_id:?}, got {found:?}"
            );
        }
        let left = function_interfaces(&emulator.ec2, &name).await;
        assert!(
            left.is_empty(),
            "case {case:?}: expected no interface of {name:?} to be left, got {left:?}"
        );
    }
}
