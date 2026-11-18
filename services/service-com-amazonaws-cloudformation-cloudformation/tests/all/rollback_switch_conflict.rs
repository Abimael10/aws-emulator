// SPDX-License-Identifier: CC0-1.0

//! The two rollback switches a stack operation carries, read as the ONE thing they state.
//!
//! `CreateStack` and `UpdateStack` each carry a top-level `DisableRollback` and a
//! `DeploymentConfig` whose `DisableRollback` says the same thing in the same words. A
//! request that sets the two to DIFFERENT values has asked for two incompatible things at
//! once, so it is refused pre-flight with `ValidationError` and nothing is created or
//! changed; a request that states only one of them, or states both in agreement, has asked
//! for one thing and is accepted with that value in force.
//!
//! Every case drives the real awsQuery request path (form body in, XML envelope out) through
//! this target's wire harness, so what it asserts is what a client sees.

use smithy_server_runtime::MultiServiceRouter;

use super::harness::{call, create_stack, member, router, settled_stack_status, xml_text, REGION};
use super::stack_lifecycle::describe_stack;

/// CloudFormation's refusal for the conflicting pair. The sentence is entirely fixed text:
/// it carries no variable part.
const CONFLICTING: &str = "Conflicting values for DisableRollback: the top-level \
                           DisableRollback and DeploymentConfig.DisableRollback must not be \
                           set to different values.";

/// The template every case deploys: the rule is about the REQUEST's members, so the body is
/// the smallest one this service accepts.
const TEMPLATE: &str = r#"{"Description":"rollback switches"}"#;

/// One shape of request, as the two switches it states.
struct SwitchCase {
    case: &'static str,
    /// The top-level `DisableRollback`, where the request states it.
    top_level: Option<&'static str>,
    /// The `DeploymentConfig$Mode`, where the request states one.
    mode: Option<&'static str>,
    /// The `DeploymentConfig$DisableRollback`, where the request states it.
    configured: Option<&'static str>,
    /// The rollback setting in force where the request is accepted, or `None` where the two
    /// switches conflict and the request must be refused.
    in_force: Option<&'static str>,
}

impl SwitchCase {
    /// The request members this case states beside the stack name and the template.
    fn members(&self) -> Vec<(String, String)> {
        let mut members = Vec::new();
        if let Some(flag) = self.top_level {
            members.push(member("DisableRollback", flag));
        }
        if let Some(mode) = self.mode {
            members.push(member("DeploymentConfig.Mode", mode));
        }
        if let Some(flag) = self.configured {
            members.push(member("DeploymentConfig.DisableRollback", flag));
        }
        members
    }
}

/// The class the rule spans: both switches stated and disagreeing, in either direction and
/// under either deployment mode, against every shape that states one value or none.
fn switch_cases() -> Vec<SwitchCase> {
    vec![
        SwitchCase {
            case: "the top-level flag true against a configured false, express mode",
            top_level: Some("true"),
            mode: Some("EXPRESS"),
            configured: Some("false"),
            in_force: None,
        },
        SwitchCase {
            case: "the top-level flag false against a configured true, express mode",
            top_level: Some("false"),
            mode: Some("EXPRESS"),
            configured: Some("true"),
            in_force: None,
        },
        SwitchCase {
            case: "the top-level flag true against a configured false, standard mode",
            top_level: Some("true"),
            mode: Some("STANDARD"),
            configured: Some("false"),
            in_force: None,
        },
        SwitchCase {
            case: "the two flags disagreeing with no mode stated",
            top_level: Some("false"),
            mode: None,
            configured: Some("true"),
            in_force: None,
        },
        SwitchCase {
            case: "both flags true",
            top_level: Some("true"),
            mode: Some("EXPRESS"),
            configured: Some("true"),
            in_force: Some("true"),
        },
        SwitchCase {
            case: "both flags false",
            top_level: Some("false"),
            mode: Some("STANDARD"),
            configured: Some("false"),
            in_force: Some("false"),
        },
        SwitchCase {
            case: "the top-level flag alone",
            top_level: Some("true"),
            mode: Some("EXPRESS"),
            configured: None,
            in_force: Some("true"),
        },
        SwitchCase {
            case: "the configured flag alone",
            top_level: None,
            mode: Some("EXPRESS"),
            configured: Some("false"),
            in_force: Some("false"),
        },
        SwitchCase {
            case: "neither flag stated",
            top_level: None,
            mode: Some("STANDARD"),
            configured: None,
            in_force: Some("false"),
        },
    ]
}

/// The `DeploymentConfig$DisableRollback` a described stack reports: the stack carries the
/// flag twice — its own `Stack$DisableRollback` and the one inside its deployment
/// configuration — and the configuration's is the last of the pair in the envelope.
fn reported_disable_rollback(body: &str) -> Option<String> {
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<DisableRollback>") {
        let after = &rest[start + "<DisableRollback>".len()..];
        let Some(end) = after.find("</DisableRollback>") else {
            break;
        };
        found.push(after[..end].to_string());
        rest = &after[end..];
    }
    found.pop()
}

/// The error `Code` and `Message` of a refusal envelope.
fn refusal(body: &str) -> (Option<String>, Option<String>) {
    (xml_text(body, "Code"), xml_text(body, "Message"))
}

#[tokio::test]
async fn create_stack_refuses_conflicting_rollback_switches_and_creates_nothing() {
    let router = router();
    for (index, case) in switch_cases().iter().enumerate() {
        let stack = format!("drconf-create-{index}");
        let (status, body) = create_stack(&router, &stack, TEMPLATE, &case.members()).await;
        match case.in_force {
            None => {
                assert_eq!(
                    status, 400,
                    "case {:?}: expected the conflicting request to be refused, got {status} \
                     {body}",
                    case.case
                );
                let (code, message) = refusal(&body);
                assert_eq!(
                    code.as_deref(),
                    Some("ValidationError"),
                    "case {:?}: expected a ValidationError, got {body}",
                    case.case
                );
                assert_eq!(
                    message.as_deref(),
                    Some(CONFLICTING),
                    "case {:?}: expected the conflict wording, got {message:?}",
                    case.case
                );
                // The refusal has no effect: nothing was minted under that name, so the
                // stack is one this service cannot resolve.
                let (status, body) = describe_stack(&router, &stack).await;
                assert_eq!(
                    status, 400,
                    "case {:?}: the refused create must leave no stack, got {status} {body}",
                    case.case
                );
                let (code, message) = refusal(&body);
                assert_eq!(
                    code.as_deref(),
                    Some("ValidationError"),
                    "case {:?}: expected a ValidationError for the absent stack, got {body}",
                    case.case
                );
                assert_eq!(
                    message.as_deref(),
                    Some(format!("Stack with id {stack} does not exist").as_str()),
                    "case {:?}: expected the does-not-exist wording, got {message:?}",
                    case.case
                );
            }
            Some(expected) => {
                assert_eq!(
                    status, 200,
                    "case {:?}: expected the request to be accepted, got {status} {body}",
                    case.case
                );
                assert!(
                    xml_text(&body, "StackId").is_some(),
                    "case {:?}: an accepted create answers a StackId, got {body}",
                    case.case
                );
                let (status, described) = describe_stack(&router, &stack).await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected DescribeStacks to succeed, got {status} {described}",
                    case.case
                );
                let reported = reported_disable_rollback(&described);
                assert_eq!(
                    reported.as_deref(),
                    Some(expected),
                    "case {:?}: expected DisableRollback {expected} in force, got \
                     {reported:?}",
                    case.case
                );
            }
        }
    }
}

/// `UpdateStack` on a stack that already exists, so the same pair can be stated on an
/// update.
async fn update_stack(
    router: &MultiServiceRouter,
    stack: &str,
    template: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut members = vec![
        member("StackName", stack),
        member("TemplateBody", template),
    ];
    members.extend_from_slice(extra);
    call(router, "UpdateStack", &members).await
}

#[tokio::test]
async fn update_stack_refuses_conflicting_rollback_switches_and_changes_nothing() {
    let router = router();
    // One stack per row, created with neither switch stated, so what every row is weighed
    // against is the same starting point: the template in force and the default rollback
    // setting.
    let revised = r#"{"Description":"revised"}"#;
    for (index, case) in switch_cases().iter().enumerate() {
        let stack = format!("drconf-update-{index}");
        let (status, body) = create_stack(&router, &stack, TEMPLATE, &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: the stack under test must be created, got {status} {body}",
            case.case
        );
        let settled = settled_stack_status(case.case, &router, REGION, &stack).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {:?}: the stack under test must settle created, got {settled}",
            case.case
        );

        let (status, body) = update_stack(&router, &stack, revised, &case.members()).await;
        match case.in_force {
            None => {
                assert_eq!(
                    status, 400,
                    "case {:?}: expected the conflicting update to be refused, got {status} \
                     {body}",
                    case.case
                );
                let (code, message) = refusal(&body);
                assert_eq!(
                    code.as_deref(),
                    Some("ValidationError"),
                    "case {:?}: expected a ValidationError, got {body}",
                    case.case
                );
                assert_eq!(
                    message.as_deref(),
                    Some(CONFLICTING),
                    "case {:?}: expected the conflict wording, got {message:?}",
                    case.case
                );
                // The refusal has no effect on the stack: the template it was deploying is
                // still the one in force, and the stack is still the one the create left.
                let (status, template) =
                    call(&router, "GetTemplate", &[member("StackName", &stack)]).await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected GetTemplate to succeed, got {status} {template}",
                    case.case
                );
                let in_force = xml_text(&template, "TemplateBody");
                assert_eq!(
                    in_force.as_deref(),
                    Some(TEMPLATE),
                    "case {:?}: the refused update must change nothing, got {in_force:?}",
                    case.case
                );
                let (status, described) = describe_stack(&router, &stack).await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected DescribeStacks to succeed, got {status} {described}",
                    case.case
                );
                let reported = xml_text(&described, "StackStatus");
                assert_eq!(
                    reported.as_deref(),
                    Some("CREATE_COMPLETE"),
                    "case {:?}: the refused update must leave the status alone, got \
                     {reported:?}",
                    case.case
                );
            }
            Some(expected) => {
                assert_eq!(
                    status, 200,
                    "case {:?}: expected the update to be accepted, got {status} {body}",
                    case.case
                );
                let settled = settled_stack_status(case.case, &router, REGION, &stack).await;
                assert_eq!(
                    settled, "UPDATE_COMPLETE",
                    "case {:?}: the accepted update must complete, got {settled}",
                    case.case
                );
                let (status, described) = describe_stack(&router, &stack).await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected DescribeStacks to succeed, got {status} {described}",
                    case.case
                );
                let reported = reported_disable_rollback(&described);
                assert_eq!(
                    reported.as_deref(),
                    Some(expected),
                    "case {:?}: expected DisableRollback {expected} in force, got \
                     {reported:?}",
                    case.case
                );
            }
        }
    }
}
