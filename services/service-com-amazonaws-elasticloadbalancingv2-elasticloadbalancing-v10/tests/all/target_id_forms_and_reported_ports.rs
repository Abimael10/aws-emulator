// SPDX-License-Identifier: CC0-1.0

//! The FORM a target id has to be written in, on every operation that takes a `Targets`
//! list, and the PORT a reported target description carries.
//!
//! Two rules, each asserted over a set of inputs:
//!
//! * THE FORM OF AN ID. What a target id names depends on the target group's own
//!   `TargetType` (`smithy_query(query="doc",shape="TargetDescription",member="Id")`: "if
//!   the target type of the target group is instance, specify an instance ID. If the target
//!   type is ip, specify an IP address. If the target type is lambda, specify the ARN of the
//!   Lambda function. If the target type is alb, specify the ARN of the Application Load
//!   Balancer target"), and a value of none of those forms names no target of any account.
//!   So it is refused — `ValidationError`, naming the value and the form it failed to take
//!   — whatever is being ASKED of it: the deregistration that removes a target is judged
//!   exactly as the registration that adds one, because a value that could never have been
//!   registered is not the no-op a well-formed target that was simply never registered
//!   earns.
//! * THE PORT A REPORTED TARGET CARRIES. `TargetGroup$Port` is "the port on which the
//!   targets receive traffic", which a registration may override ("you can override the
//!   port for a target when you register it"), so the port of a reported target is the one
//!   the request named, else the target group's own — for a target that is registered and
//!   for one that is not, since the port is what decides WHICH registration a request is
//!   about.
//!
//! The rows differ along what the rules generalise over: the group's target type, the form
//! the offending id takes (an id of another target type, a near miss of the right form, a
//! value of no form at all), whether a well-formed id was ever registered, and whether the
//! request names a port of its own.

use crate::provisioning_loop::{action, call, emulator, first_text, members, p, Reply};
use crate::target_group_attributes_and_modification::unescaped;
use crate::target_group_health_check_port::create_group;
use smithy_server_runtime::MultiServiceRouter;

/// The error code this service answers a member whose form the model states in prose with.
const VALIDATION_ERROR: &str = "ValidationError";

/// One row: the case, the group's `TargetType`, the id the request carries, and the
/// sentence the refusal states.
struct FormRow {
    case: &'static str,
    target_type: &'static str,
    id: &'static str,
    message: &'static str,
}

/// Every KIND of value that is no target of the group it is handed to: the id form of
/// ANOTHER target type, a near miss of the right form, and a value of no form at all.
///
/// The sentences are the ones the service states for each form — "The IP address '<id>' is
/// not a valid IPv4 address" for an `ip` group, "Instance ID '<id>' is not valid" for an
/// `instance` group — each with the value from the request as its only variable part.
const FORM_ROWS: &[FormRow] = &[
    FormRow {
        case: "an instance id handed to an ip target group",
        target_type: "ip",
        id: "i-0123456789abcdef0",
        message: "The IP address 'i-0123456789abcdef0' is not a valid IPv4 address",
    },
    FormRow {
        case: "a host name handed to an ip target group",
        target_type: "ip",
        id: "target.example.com",
        message: "The IP address 'target.example.com' is not a valid IPv4 address",
    },
    FormRow {
        case: "an address with a prefix length handed to an ip target group",
        target_type: "ip",
        id: "10.0.0.7/32",
        message: "The IP address '10.0.0.7/32' is not a valid IPv4 address",
    },
    FormRow {
        case: "an address handed to an instance target group",
        target_type: "instance",
        id: "10.0.0.7",
        message: "Instance ID '10.0.0.7' is not valid",
    },
    FormRow {
        case: "an instance id of the wrong length",
        target_type: "instance",
        id: "i-123",
        message: "Instance ID 'i-123' is not valid",
    },
    FormRow {
        case: "an instance id with a character no instance id carries",
        target_type: "instance",
        id: "i-0123456789abcdefg",
        message: "Instance ID 'i-0123456789abcdefg' is not valid",
    },
];

/// One target as `DescribeTargetHealth` reports it: its id, the port traffic to it goes
/// to, and the health state and reason it is under.
#[derive(Debug, PartialEq, Eq)]
struct ReportedTarget {
    id: Option<String>,
    port: Option<String>,
    state: Option<String>,
    reason: Option<String>,
}

/// The status, code and message of a refusal, as a caller reads them.
fn refusal(reply: &Reply) -> (u16, String, String) {
    (
        reply.status,
        reply.error_code(),
        unescaped(&reply.text("Message").unwrap_or_default()),
    )
}

/// A target group of `target_type` on port 80, the shape each row starts from.
async fn target_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target_type: &str,
) -> String {
    let fields = vec![
        p("Protocol", "HTTP"),
        p("Port", "80"),
        p("VpcId", "vpc-1"),
        p("TargetType", target_type),
    ];
    create_group(router, case, name, &fields).await
}

/// Drive one `Targets`-taking operation with a single target.
async fn one_target(
    router: &MultiServiceRouter,
    operation: &str,
    arn: &str,
    id: &str,
    port: Option<&str>,
) -> Reply {
    let mut form = action(operation);
    form.push(p("TargetGroupArn", arn));
    form.push(p("Targets.member.1.Id", id));
    if let Some(port) = port {
        form.push(p("Targets.member.1.Port", port));
    }
    call(router, &form).await
}

/// Every target the health read reports for `arn`, in the order it reports them.
async fn reported_targets(
    router: &MultiServiceRouter,
    arn: &str,
    named: Option<(&str, Option<&str>)>,
) -> Vec<ReportedTarget> {
    let mut form = action("DescribeTargetHealth");
    form.push(p("TargetGroupArn", arn));
    if let Some((id, port)) = named {
        form.push(p("Targets.member.1.Id", id));
        if let Some(port) = port {
            form.push(p("Targets.member.1.Port", port));
        }
    }
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "a health read must be accepted, got {} body {}",
        reply.status, reply.body
    );
    members(&reply.body, "TargetHealthDescriptions")
        .iter()
        .map(|description| ReportedTarget {
            id: first_text(description, "Id"),
            port: first_text(description, "Port"),
            state: first_text(description, "State"),
            reason: first_text(description, "Reason"),
        })
        .collect()
}

#[tokio::test]
async fn deregister_targets_judges_a_target_id_by_the_groups_target_type() {
    let (router, _engine) = emulator();

    for (index, row) in FORM_ROWS.iter().enumerate() {
        let name = format!("dereg-form-{index}");
        let arn = target_group(&router, row.case, &name, row.target_type).await;
        // A well-formed target of this group, so the refusal below cannot be mistaken for
        // "the group holds nothing": this one is still registered afterwards.
        let held = match row.target_type {
            "ip" => "10.0.0.11",
            _ => "i-00000000000000011",
        };
        let registered = one_target(&router, "RegisterTargets", &arn, held, None).await;
        assert_eq!(
            registered.status, 200,
            "case {}: the well-formed target must register, got {} body {}",
            row.case, registered.status, registered.body
        );

        let reply = one_target(&router, "DeregisterTargets", &arn, row.id, None).await;
        assert_eq!(
            refusal(&reply),
            (400, VALIDATION_ERROR.to_string(), row.message.to_string()),
            "case {}: expected the form refusal, got {}",
            row.case,
            reply.body
        );
        // A refused deregistration removes nothing.
        assert_eq!(
            reported_targets(&router, &arn, None)
                .await
                .into_iter()
                .map(|reported| reported.id)
                .collect::<Vec<Option<String>>>(),
            vec![Some(held.to_string())],
            "case {}: a refused deregistration must leave the group as it was",
            row.case
        );
    }
}

#[tokio::test]
async fn register_targets_refuses_the_same_forms_in_the_same_words() {
    // The rule belongs to the MEMBER, so the registration that adds a target and the
    // deregistration that removes one answer the same sentence for the same value.
    let (router, _engine) = emulator();

    for (index, row) in FORM_ROWS.iter().enumerate() {
        let name = format!("reg-form-{index}");
        let arn = target_group(&router, row.case, &name, row.target_type).await;
        let reply = one_target(&router, "RegisterTargets", &arn, row.id, None).await;
        assert_eq!(
            refusal(&reply),
            (400, VALIDATION_ERROR.to_string(), row.message.to_string()),
            "case {}: expected the form refusal, got {}",
            row.case,
            reply.body
        );
        assert!(
            reported_targets(&router, &arn, None).await.is_empty(),
            "case {}: a refused registration must store nothing",
            row.case
        );
    }
}

#[tokio::test]
async fn a_well_formed_target_that_was_never_registered_is_still_a_no_op_to_deregister() {
    // The refusal above is about the FORM of the value, not about whether the group holds
    // it: a target that could have been registered and simply was not is the no-op the
    // model documents, on an `ip` group and on an `instance` group alike.
    let (router, _engine) = emulator();

    for (case, target_type, id) in [
        ("an address never registered", "ip", "10.0.0.250"),
        ("an instance never registered", "instance", "i-0123456789abcdef0"),
    ] {
        let name = format!("dereg-noop-{target_type}");
        let arn = target_group(&router, case, &name, target_type).await;
        let reply = one_target(&router, "DeregisterTargets", &arn, id, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: deregistering a well-formed target nothing registered is a no-op, \
             got {} body {}",
            reply.status, reply.body
        );
    }
}

/// One row of the reported-port table: the case, the id the read names, the port it names,
/// the port the answer must carry, and the health state and reason that target is under.
struct PortRow {
    case: &'static str,
    id: &'static str,
    names_port: Option<&'static str>,
    reports_port: &'static str,
    state: &'static str,
    reason: &'static str,
}

#[tokio::test]
async fn a_reported_target_carries_the_port_the_request_named_else_the_groups_own() {
    // The group's own port, and a registration that overrides it: each is reported under
    // the port traffic to that target goes to, whether or not the target is registered.
    let (router, _engine) = emulator();
    let arn = target_group(&router, "the reported port", "reported-port-tg", "instance").await;
    let registered = "i-00000000000000022";
    let never = "i-00000000000000033";

    let reply = one_target(&router, "RegisterTargets", &arn, registered, None).await;
    assert_eq!(
        reply.status, 200,
        "the registration must be accepted, got {} body {}",
        reply.status, reply.body
    );

    // A registered target of a group no load balancer forwards to has nothing checking it,
    // which is what `Target.NotInUse` words; one that was never registered reads
    // `Target.NotRegistered`. Either way the PORT is the one traffic to it would go to.
    let rows: &[PortRow] = &[
        PortRow {
            case: "a registered target read with no port",
            id: registered,
            names_port: None,
            reports_port: "80",
            state: "unused",
            reason: "Target.NotInUse",
        },
        PortRow {
            case: "a registered target read on the group's port",
            id: registered,
            names_port: Some("80"),
            reports_port: "80",
            state: "unused",
            reason: "Target.NotInUse",
        },
        PortRow {
            case: "an unregistered target read with no port",
            id: never,
            names_port: None,
            reports_port: "80",
            state: "unused",
            reason: "Target.NotRegistered",
        },
        PortRow {
            case: "an unregistered target read on a port of its own",
            id: never,
            names_port: Some("8080"),
            reports_port: "8080",
            state: "unused",
            reason: "Target.NotRegistered",
        },
    ];
    for row in rows {
        let reported = reported_targets(&router, &arn, Some((row.id, row.names_port))).await;
        assert_eq!(
            reported,
            vec![ReportedTarget {
                id: Some(row.id.to_string()),
                port: Some(row.reports_port.to_string()),
                state: Some(row.state.to_string()),
                reason: Some(row.reason.to_string()),
            }],
            "case {}: expected the port {:?} on the reported target",
            row.case,
            row.reports_port
        );
    }
}
