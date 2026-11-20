// SPDX-License-Identifier: CC0-1.0

//! The DRIFT of a stack's Elastic Load Balancing v2 resources, read off the service that owns
//! them.
//!
//! **The rule under test.** A drift check asks the service that OWNS each of a stack's
//! resources what that resource IS now, and compares it with the properties the template
//! declared: a load balancer, target group, listener or rule that service still holds and
//! reports as the template declares it is `IN_SYNC`, one it reports a declared property
//! differently for is `MODIFIED` carrying that leaf's own `PropertyDifference`, and only a read
//! that ANSWERS the resource is gone makes it `DELETED`. So a freshly deployed stack of these
//! types is `IN_SYNC` with no drifted resource, and a change made outside the stack is reported
//! against the one resource it was made to.
//!
//! Why it matters: a stack reported `DRIFTED` the moment it finished deploying tells a client
//! its estate does not match its template when it does, and a stack reported `IN_SYNC` after an
//! operator changed a load balancer by hand hides the change a template diff exists to find.
//! Either way the `DetectStackDrift` answer is useless for deciding whether to redeploy.
//!
//! The rows differ along what the rule generalises over: WHICH of the four types the change is
//! made to, WHICH kind of declared property it is made to — a scalar member of the type's own
//! read, an entry of the type's attribute collection (the one declared property that is a
//! second call of the owning service on both sides), and a rule's priority — and whether the
//! resource is changed or REMOVED outright. Every value change is also put back, so the next
//! check reads the service again rather than latching what it found.

use crate::ec2_network_stack_drift::{
    drift_report, physical_id, record_of, settled_detection, Difference,
};
use crate::elbv2_resource_provisioning::{poll_to_terminal, NETWORK};
use crate::stack_lifecycle::{create_stack, elbv2, member, router, xml_text};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    drift::difference_type, drift_detection_status, resource_drift_status, status,
    DRIFT_STATUS_DRIFTED, DRIFT_STATUS_IN_SYNC,
};
use smithy_server_runtime::MultiServiceRouter;

/// The stack every row drives, and the stem of the names its resources carry.
const STACK: &str = "elbv2-drift";

/// The logical ids the template declares, which is what a report may name and nothing else.
const DECLARED: &[&str] = &[
    "Vpc",
    "Igw",
    "IgwAttachment",
    "SubnetA",
    "SubnetB",
    "Alb",
    "Targets",
    "Listener",
    "FirstRule",
    "SecondRule",
];

/// The load balancer attribute the template declares and one row changes outside the stack:
/// "idle_timeout.timeout_seconds - The idle timeout value, in seconds", an Application Load
/// Balancer attribute of `ModifyLoadBalancerAttributes`.
const IDLE_TIMEOUT: &str = "idle_timeout.timeout_seconds";

/// The target group attribute the template declares: "deregistration_delay.timeout_seconds -
/// The amount of time for Elastic Load Balancing to wait before changing the state of a
/// deregistering target from draining to unused", a member of `ModifyTargetGroupAttributes`.
const DEREGISTRATION_DELAY: &str = "deregistration_delay.timeout_seconds";

/// The listener attribute the template declares: one of the mutual-TLS request header names an
/// Application Load Balancer listener takes, a member of `ModifyListenerAttributes`.
const MTLS_SUBJECT_HEADER: &str = "routing.http.request.x_amzn_mtls_clientcert_subject.header_name";

/// The health check path the template declares for the target group, and the one an operator
/// moves it to outside the stack.
const DECLARED_HEALTH_PATH: &str = "/health";
const CHANGED_HEALTH_PATH: &str = "/ready";

/// The priorities the two declared rules are evaluated at.
const FIRST_PRIORITY: &str = "10";
const SECOND_PRIORITY: &str = "20";

/// What one row does to its resource outside the stack.
enum Change {
    /// Nothing at all: the deployment as it was left.
    Untouched,
    /// One entry of the type's attribute collection set to another value: the property the
    /// declaration states the collection in, the call that sets it, the member that call takes
    /// the resource's ARN under, the key, and the declared and changed values.
    Attribute {
        property: &'static str,
        action: &'static str,
        identity: &'static str,
        key: &'static str,
        declared: &'static str,
        changed: &'static str,
    },
    /// One member of the type's own modify call set to another value.
    Member {
        action: &'static str,
        identity: &'static str,
        member: &'static str,
        declared: &'static str,
        changed: &'static str,
    },
    /// A rule moved to another priority, which is `SetRulePriorities` and no member of
    /// `ModifyRule`.
    Priority {
        declared: &'static str,
        changed: &'static str,
    },
    /// The resource deleted outright in the service that holds it.
    Deleted {
        action: &'static str,
        identity: &'static str,
    },
}

/// One row: the resource the change is made to, and the change.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    change: Change,
}

const ROWS: &[Row] = &[
    Row {
        case: "a deployment nothing was changed on outside the stack",
        logical_id: "Alb",
        change: Change::Untouched,
    },
    Row {
        case: "the load balancer's idle timeout changed outside the stack",
        logical_id: "Alb",
        change: Change::Attribute {
            property: "LoadBalancerAttributes",
            action: "ModifyLoadBalancerAttributes",
            identity: "LoadBalancerArn",
            key: IDLE_TIMEOUT,
            declared: "120",
            changed: "90",
        },
    },
    Row {
        case: "the target group's deregistration delay changed outside the stack",
        logical_id: "Targets",
        change: Change::Attribute {
            property: "TargetGroupAttributes",
            action: "ModifyTargetGroupAttributes",
            identity: "TargetGroupArn",
            key: DEREGISTRATION_DELAY,
            declared: "120",
            changed: "45",
        },
    },
    Row {
        case: "the listener's mutual-TLS header name changed outside the stack",
        logical_id: "Listener",
        change: Change::Attribute {
            property: "ListenerAttributes",
            action: "ModifyListenerAttributes",
            identity: "ListenerArn",
            key: MTLS_SUBJECT_HEADER,
            declared: "X-Client-Subject",
            changed: "X-Subject",
        },
    },
    Row {
        case: "the target group's health check path moved outside the stack",
        logical_id: "Targets",
        change: Change::Member {
            action: "ModifyTargetGroup",
            identity: "TargetGroupArn",
            member: "HealthCheckPath",
            declared: DECLARED_HEALTH_PATH,
            changed: CHANGED_HEALTH_PATH,
        },
    },
    Row {
        case: "a rule moved to another priority outside the stack",
        logical_id: "SecondRule",
        change: Change::Priority {
            declared: SECOND_PRIORITY,
            changed: "30",
        },
    },
    // Last, because the rule it removes is not put back: a resource the owning service no
    // longer holds is the one row `DELETED` belongs to.
    Row {
        case: "a rule deleted outside the stack",
        logical_id: "SecondRule",
        change: Change::Deleted {
            action: "DeleteRule",
            identity: "RuleArn",
        },
    },
];

/// The template the stack is deployed from: the network an internet-facing load balancer needs,
/// the load balancer, a target group, a listener and two rules — each declaring one entry of its
/// own attribute collection, which is the declared property that is a second call of the owning
/// service.
fn template() -> String {
    let mut template = String::from("Resources:\n");
    template.push_str(NETWORK);
    template.push_str(&format!(
        concat!(
            "  Alb:\n",
            "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
            "    Properties:\n",
            "      Name: {stack}-alb\n",
            "      Type: application\n",
            "      Scheme: internet-facing\n",
            "      Subnets:\n",
            "        - !Ref SubnetA\n",
            "        - !Ref SubnetB\n",
            "      LoadBalancerAttributes:\n",
            "        - Key: {idle_timeout}\n",
            "          Value: '120'\n",
            "  Targets:\n",
            "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
            "    Properties:\n",
            "      Name: {stack}-tg\n",
            "      Port: 80\n",
            "      Protocol: HTTP\n",
            "      TargetType: ip\n",
            "      VpcId: !Ref Vpc\n",
            "      HealthCheckPath: {health_path}\n",
            "      TargetGroupAttributes:\n",
            "        - Key: {deregistration_delay}\n",
            "          Value: '120'\n",
            "  Listener:\n",
            "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
            "    Properties:\n",
            "      LoadBalancerArn: !Ref Alb\n",
            "      Port: 80\n",
            "      Protocol: HTTP\n",
            "      DefaultActions:\n",
            "        - Type: forward\n",
            "          TargetGroupArn: !Ref Targets\n",
            "      ListenerAttributes:\n",
            "        - Key: {mtls_header}\n",
            "          Value: X-Client-Subject\n",
        ),
        stack = STACK,
        idle_timeout = IDLE_TIMEOUT,
        health_path = DECLARED_HEALTH_PATH,
        deregistration_delay = DEREGISTRATION_DELAY,
        mtls_header = MTLS_SUBJECT_HEADER,
    ));
    template.push_str(&rule("FirstRule", FIRST_PRIORITY, "/a", None));
    template.push_str(&rule(
        "SecondRule",
        SECOND_PRIORITY,
        "/b",
        Some("FirstRule"),
    ));
    template
}

/// One rule declaration on the stack's listener.
fn rule(logical_id: &str, priority: &str, path: &str, after: Option<&str>) -> String {
    let depends = after.map(|on| format!("    DependsOn: {on}\n")).unwrap_or_default();
    format!(
        concat!(
            "  {logical_id}:\n",
            "    Type: AWS::ElasticLoadBalancingV2::ListenerRule\n",
            "{depends}",
            "    Properties:\n",
            "      ListenerArn: !Ref Listener\n",
            "      Priority: {priority}\n",
            "      Conditions:\n",
            "        - Field: path-pattern\n",
            "          Values:\n",
            "            - {path}\n",
            "      Actions:\n",
            "        - Type: forward\n",
            "          TargetGroupArn: !Ref Targets\n",
        ),
        logical_id = logical_id,
        depends = depends,
        priority = priority,
        path = path,
    )
}

/// Drive one Elastic Load Balancing write and hold it to an accepted answer: a change an
/// operator makes outside the stack is a call that really went through, or the row proves
/// nothing.
async fn write(case: &str, router: &MultiServiceRouter, action: &str, form: &[(String, String)]) {
    let (status_code, body) = elbv2(router, action, form).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: {action} must answer 200, got {status_code} {body}"
    );
}

/// Make the row's change, or put it back: `value` is the value the resource is left holding.
async fn apply(
    case: &str,
    router: &MultiServiceRouter,
    change: &Change,
    arn: &str,
    restoring: bool,
) {
    match change {
        Change::Untouched => {}
        Change::Attribute {
            action,
            identity,
            key,
            declared,
            changed,
            ..
        } => {
            let value = if restoring { declared } else { changed };
            write(
                case,
                router,
                action,
                &[
                    member(identity, arn),
                    member("Attributes.member.1.Key", key),
                    member("Attributes.member.1.Value", value),
                ],
            )
            .await;
        }
        Change::Member {
            action,
            identity,
            member: name,
            declared,
            changed,
        } => {
            let value = if restoring { declared } else { changed };
            write(
                case,
                router,
                action,
                &[member(identity, arn), member(name, value)],
            )
            .await;
        }
        Change::Priority { declared, changed } => {
            let value = if restoring { declared } else { changed };
            write(
                case,
                router,
                "SetRulePriorities",
                &[
                    member("RulePriorities.member.1.RuleArn", arn),
                    member("RulePriorities.member.1.Priority", value),
                ],
            )
            .await;
        }
        Change::Deleted { action, identity } => {
            write(case, router, action, &[member(identity, arn)]).await;
        }
    }
}

/// What one resource's record in the report must say: the resource the row changed, the drift
/// status it is in and the differences it carries.
struct ExpectedDrift<'report> {
    logical_id: &'report str,
    status: &'report str,
    differences: &'report [Difference],
}

/// The drift status and the differences the row's change must be reported as.
fn expected(change: &Change) -> (&'static str, Vec<Difference>) {
    match change {
        Change::Untouched => (resource_drift_status::IN_SYNC, Vec::new()),
        Change::Attribute {
            property,
            declared,
            changed,
            ..
        } => (
            resource_drift_status::MODIFIED,
            // The entry the declaration states the attribute in is the first of the collection
            // it declares, so the leaf that differs is that entry's own value.
            vec![(
                format!("/{property}/0/Value"),
                (*declared).to_string(),
                (*changed).to_string(),
                difference_type::NOT_EQUAL.to_string(),
            )],
        ),
        Change::Member {
            member: name,
            declared,
            changed,
            ..
        } => (
            resource_drift_status::MODIFIED,
            vec![(
                format!("/{name}"),
                (*declared).to_string(),
                (*changed).to_string(),
                difference_type::NOT_EQUAL.to_string(),
            )],
        ),
        Change::Priority { declared, changed } => (
            resource_drift_status::MODIFIED,
            vec![(
                "/Priority".to_string(),
                (*declared).to_string(),
                (*changed).to_string(),
                difference_type::NOT_EQUAL.to_string(),
            )],
        ),
        Change::Deleted { .. } => (resource_drift_status::DELETED, Vec::new()),
    }
}

/// What one detection must come to, and the report it must carry.
async fn assert_detection(
    case: &str,
    router: &MultiServiceRouter,
    drifted: Option<ExpectedDrift<'_>>,
) {
    let (detection, stack_drift, count) = settled_detection(case, router, STACK).await;
    assert_eq!(
        detection,
        drift_detection_status::DETECTION_COMPLETE,
        "case {case:?}: expected the detection to complete, got {detection:?}"
    );
    let (wanted_status, wanted_count) = match drifted {
        Some(_) => (DRIFT_STATUS_DRIFTED, "1"),
        None => (DRIFT_STATUS_IN_SYNC, "0"),
    };
    let report = drift_report(case, router, STACK).await;
    let rendered: Vec<String> = report
        .iter()
        .map(|(id, record)| format!("{id}={} {:?}", record.status, record.differences))
        .collect();
    assert_eq!(
        stack_drift, wanted_status,
        "case {case:?}: expected the stack to report {wanted_status}, got {stack_drift:?}; its \
         report: {rendered:?}"
    );
    assert_eq!(
        count, wanted_count,
        "case {case:?}: expected {wanted_count} drifted resource(s), got {count:?}; its report: \
         {rendered:?}"
    );

    // Every resource the report names is one the template declares, and the four Elastic Load
    // Balancing resources are all compared rather than left out of the check.
    let named: Vec<&str> = report.iter().map(|(id, _)| id.as_str()).collect();
    for logical_id in &named {
        assert!(
            DECLARED.contains(logical_id),
            "case {case:?}: expected only declared resources in the report, got {logical_id:?}"
        );
    }
    for logical_id in ["Alb", "Targets", "Listener", "FirstRule"] {
        assert!(
            named.contains(&logical_id),
            "case {case:?}: expected {logical_id} to be checked, got {named:?}"
        );
    }

    let changed_id = drifted.as_ref().map(|wanted| wanted.logical_id);
    for (logical_id, record) in &report {
        if Some(logical_id.as_str()) == changed_id {
            continue;
        }
        assert_eq!(
            record.status,
            resource_drift_status::IN_SYNC,
            "case {case:?}: expected {logical_id} to match its declaration, got {status:?} with \
             {differences:?}",
            status = record.status,
            differences = record.differences,
        );
    }

    if let Some(ExpectedDrift {
        logical_id,
        status: wanted,
        differences,
    }) = drifted
    {
        let record = record_of(case, &report, logical_id);
        assert_eq!(
            record.status, wanted,
            "case {case:?}: expected {logical_id} to be {wanted}, got {status:?} with \
             {found:?}",
            status = record.status,
            found = record.differences,
        );
        assert_eq!(
            record.differences, differences,
            "case {case:?}: expected exactly {differences:?} for {logical_id}, got {found:?}",
            found = record.differences,
        );
    }
}

#[tokio::test]
async fn a_stacks_elbv2_resources_are_compared_with_what_that_service_reports() {
    let router = router();
    let (status_code, body) = create_stack(&router, STACK, &template(), &[]).await;
    assert_eq!(
        status_code, 200,
        "CreateStack must answer 200, got {status_code} {body}"
    );
    let described = poll_to_terminal(&router, "the deployment", STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the stack to deploy, got {described}"
    );

    for row in ROWS {
        let case = row.case;
        let arn = physical_id(case, &router, STACK, row.logical_id).await;
        apply(case, &router, &row.change, &arn, false).await;
        let (wanted, differences) = expected(&row.change);
        let drifted = (wanted != resource_drift_status::IN_SYNC).then_some(ExpectedDrift {
            logical_id: row.logical_id,
            status: wanted,
            differences: differences.as_slice(),
        });
        assert_detection(case, &router, drifted).await;

        // Put the value back where the row changed one: the next check reads the owning
        // service again, so the stack is in sync once the resource is.
        if matches!(
            row.change,
            Change::Attribute { .. } | Change::Member { .. } | Change::Priority { .. }
        ) {
            apply(case, &router, &row.change, &arn, true).await;
            assert_detection(&format!("{case} — put back"), &router, None).await;
        }
    }
}
