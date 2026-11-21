// SPDX-License-Identifier: CC0-1.0

//! The DEPLOYMENT STYLE a CodeDeploy deployment group may run by, driven over the
//! wire through the shared [`Emulator`] harness of `codedeploy_lifecycle`.
//!
//! RULE: a deployment group's `deploymentStyle` has to describe a deployment the
//! compute platform of the application it belongs to can perform, so the style the
//! group would HAVE is judged against that platform before anything is stored. An
//! Amazon ECS deployment installs a replacement task set and then moves the
//! production traffic route onto it, which is the blue/green, traffic-controlled
//! style and the only one that platform runs: a request that would create or leave
//! an Amazon ECS group running any other style — including the server-side default
//! a request naming no style at all gets — is refused HTTP 400
//! `InvalidDeploymentStyleException` with the fixed sentence
//! [`ECS_DEPLOYMENT_STYLE_REQUIRED`], and nothing is stored for it. A group on any
//! other compute platform runs whichever style it names.
//!
//! The rows differ along what the rule generalises over: the application's compute
//! platform (every member of `ComputePlatform`,
//! `smithy_query(query="shape",shape="ComputePlatform")`: Server | Lambda | ECS),
//! and which style the request names — each pair of `DeploymentType` (IN_PLACE |
//! BLUE_GREEN) and `DeploymentOption` (WITH_TRAFFIC_CONTROL |
//! WITHOUT_TRAFFIC_CONTROL), each member named on its own so the other defaults,
//! and the member omitted entirely — on a create and on an update.

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{error_code, Emulator};

/// The modelled error a group whose style its platform cannot run earns
/// (`smithy_query(query="shape",shape="InvalidDeploymentStyleException")`: a client
/// error, HTTP 400).
const INVALID_DEPLOYMENT_STYLE: &str = "InvalidDeploymentStyleException";

/// CodeDeploy's sentence for an Amazon ECS deployment group whose style is not the
/// one that platform runs: entirely fixed text, no part of it taken from the
/// request or from state.
const ECS_DEPLOYMENT_STYLE_REQUIRED: &str =
    "For ECS deployment, the deployment type must be BLUE_GREEN, and deployment option must be \
     WITH_TRAFFIC_CONTROL.";

/// The service role every deployment group here is created with.
const ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The style the service materialises for a member a request left unset
/// (`crate::deployment_group_defaults` asserts that defaulting on its own).
const DEFAULT_TYPE: &str = "IN_PLACE";
/// See [`DEFAULT_TYPE`].
const DEFAULT_OPTION: &str = "WITHOUT_TRAFFIC_CONTROL";

/// A `deploymentStyle` naming the members the row states.
fn style(deployment_type: Option<&str>, deployment_option: Option<&str>) -> Value {
    let mut named = json!({});
    if let Some(deployment_type) = deployment_type {
        named["deploymentType"] = json!(deployment_type);
    }
    if let Some(deployment_option) = deployment_option {
        named["deploymentOption"] = json!(deployment_option);
    }
    named
}

/// One row of the create table: the case, the application's compute platform, the
/// `deploymentStyle` the request names, and the `(deploymentType,
/// deploymentOption)` the created group reports — `None` for a row the service
/// refuses.
type CreateRow = (
    &'static str,
    &'static str,
    Option<Value>,
    Option<(&'static str, &'static str)>,
);

/// The `deploymentGroupInfo` `GetDeploymentGroup` reports for `group`.
async fn group_info(emulator: &Emulator, application: &str, group: &str) -> Value {
    emulator
        .ok(
            "GetDeploymentGroup",
            json!({"applicationName": application, "deploymentGroupName": group}),
        )
        .await["deploymentGroupInfo"]
        .clone()
}

/// The deployment group names `ListDeploymentGroups` reports for `application`.
async fn listed_groups(emulator: &Emulator, application: &str) -> Vec<String> {
    emulator
        .ok(
            "ListDeploymentGroups",
            json!({"applicationName": application}),
        )
        .await["deploymentGroups"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// RULE on a CREATE: an Amazon ECS deployment group is created only as a
/// blue/green group with traffic control; any other style, the defaulted one
/// included, is refused with the fixed sentence and stores nothing. A group on
/// another platform is created with whatever style it named.
#[tokio::test]
async fn an_ecs_group_is_created_only_as_blue_green_with_traffic_control() {
    let emulator = Emulator::new();
    let rows: &[CreateRow] = &[
        (
            "an ECS group that is blue/green with traffic control",
            "ECS",
            Some(style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL"))),
            Some(("BLUE_GREEN", "WITH_TRAFFIC_CONTROL")),
        ),
        (
            "an ECS group that is blue/green WITHOUT traffic control",
            "ECS",
            Some(style(Some("BLUE_GREEN"), Some("WITHOUT_TRAFFIC_CONTROL"))),
            None,
        ),
        (
            "an ECS group that is in-place with traffic control",
            "ECS",
            Some(style(Some("IN_PLACE"), Some("WITH_TRAFFIC_CONTROL"))),
            None,
        ),
        (
            "an ECS group that is in-place without traffic control",
            "ECS",
            Some(style(Some("IN_PLACE"), Some("WITHOUT_TRAFFIC_CONTROL"))),
            None,
        ),
        (
            "an ECS group naming only the blue/green type, so the option defaults",
            "ECS",
            Some(style(Some("BLUE_GREEN"), None)),
            None,
        ),
        (
            "an ECS group naming only traffic control, so the type defaults",
            "ECS",
            Some(style(None, Some("WITH_TRAFFIC_CONTROL"))),
            None,
        ),
        ("an ECS group naming no style at all", "ECS", None, None),
        (
            "an ECS group naming an all-empty style",
            "ECS",
            Some(json!({})),
            None,
        ),
        (
            "a Server group that is in-place without traffic control",
            "Server",
            Some(style(Some("IN_PLACE"), Some("WITHOUT_TRAFFIC_CONTROL"))),
            Some(("IN_PLACE", "WITHOUT_TRAFFIC_CONTROL")),
        ),
        (
            "a Server group that is blue/green without traffic control",
            "Server",
            Some(style(Some("BLUE_GREEN"), Some("WITHOUT_TRAFFIC_CONTROL"))),
            Some(("BLUE_GREEN", "WITHOUT_TRAFFIC_CONTROL")),
        ),
        (
            "a Server group naming no style at all",
            "Server",
            None,
            Some((DEFAULT_TYPE, DEFAULT_OPTION)),
        ),
        (
            "a Lambda group that is blue/green without traffic control",
            "Lambda",
            Some(style(Some("BLUE_GREEN"), Some("WITHOUT_TRAFFIC_CONTROL"))),
            Some(("BLUE_GREEN", "WITHOUT_TRAFFIC_CONTROL")),
        ),
        (
            "a Lambda group naming only the in-place type, so the option defaults",
            "Lambda",
            Some(style(Some("IN_PLACE"), None)),
            Some(("IN_PLACE", DEFAULT_OPTION)),
        ),
        (
            "a Lambda group naming no style at all",
            "Lambda",
            None,
            Some((DEFAULT_TYPE, DEFAULT_OPTION)),
        ),
    ];

    for (index, (case, platform, named, expected)) in rows.iter().enumerate() {
        let application = format!("style-app-{index:02}");
        let group = format!("{application}-dg");
        emulator
            .ok(
                "CreateApplication",
                json!({"applicationName": application, "computePlatform": platform}),
            )
            .await;
        let mut request = json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "serviceRoleArn": ROLE,
        });
        if let Some(named) = named {
            request["deploymentStyle"] = named.clone();
        }
        if *platform == "ECS" {
            // The load balancer an Amazon ECS group's traffic-controlled style
            // routes traffic behind is a precondition of its own, so every row of
            // this table carries one: what is under test here is the style.
            request["loadBalancerInfo"] = json!({"targetGroupInfoList": [{"name": "tg-blue"}]});
        }

        let (status, body) = emulator.call("CreateDeploymentGroup", request).await;
        let Some((deployment_type, deployment_option)) = expected else {
            assert_eq!(
                (
                    status,
                    error_code(&body).as_str(),
                    body["message"].as_str()
                ),
                (
                    400,
                    INVALID_DEPLOYMENT_STYLE,
                    Some(ECS_DEPLOYMENT_STYLE_REQUIRED)
                ),
                "case {case:?}: expected HTTP 400 {INVALID_DEPLOYMENT_STYLE} with \
                 {ECS_DEPLOYMENT_STYLE_REQUIRED:?}, got {status} with {body:?}"
            );
            // Nothing is stored for a refused create: the application holds no
            // group at all.
            let listed = listed_groups(&emulator, &application).await;
            assert!(
                listed.is_empty(),
                "case {case:?}: expected the refused group not to be stored, got {listed:?}"
            );
            continue;
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected the group to be created, got {status} with {body:?}"
        );
        // Read the group back through its own Get: the style the group runs by is
        // what it reports.
        let info = group_info(&emulator, &application, &group).await;
        assert_eq!(
            (
                info["deploymentStyle"]["deploymentType"].as_str(),
                info["deploymentStyle"]["deploymentOption"].as_str(),
            ),
            (Some(*deployment_type), Some(*deployment_option)),
            "case {case:?}: expected the style {deployment_type}/{deployment_option}, got {info:?}"
        );
        let listed = listed_groups(&emulator, &application).await;
        assert_eq!(
            listed,
            vec![group.clone()],
            "case {case:?}: expected the application to hold the created group, got {listed:?}"
        );
    }
}

/// RULE on an UPDATE: the style the group would END UP with is what the
/// precondition is judged on, so an update that would move an Amazon ECS group off
/// the blue/green, traffic-controlled style is refused and changes nothing, while
/// an update that names that style again, or names no style at all, is served.
#[tokio::test]
async fn an_update_may_not_move_an_ecs_group_off_blue_green_with_traffic_control() {
    let emulator = Emulator::new();
    const APPLICATION: &str = "style-update-ecs";
    const GROUP: &str = "style-update-ecs-dg";
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": APPLICATION, "computePlatform": "ECS"}),
        )
        .await;
    emulator
        .ok(
            "CreateDeploymentGroup",
            json!({
                "applicationName": APPLICATION,
                "deploymentGroupName": GROUP,
                "serviceRoleArn": ROLE,
                "deploymentStyle": style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL")),
                "loadBalancerInfo": {"targetGroupInfoList": [{"name": "tg-blue"}]},
            }),
        )
        .await;

    // Rows are successive updates on ONE group: each refused row must leave the
    // group running the style it already had.
    let rows: &[(&str, Option<Value>, bool)] = &[
        (
            "off traffic control",
            Some(style(Some("BLUE_GREEN"), Some("WITHOUT_TRAFFIC_CONTROL"))),
            false,
        ),
        (
            "onto an in-place deployment",
            Some(style(Some("IN_PLACE"), Some("WITH_TRAFFIC_CONTROL"))),
            false,
        ),
        (
            "naming only the in-place type, so the option defaults",
            Some(style(Some("IN_PLACE"), None)),
            false,
        ),
        ("an all-empty style", Some(json!({})), false),
        (
            "naming the admissible style again",
            Some(style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL"))),
            true,
        ),
        ("naming no style at all", None, true),
    ];

    for (case, named, served) in rows {
        let mut request = json!({
            "applicationName": APPLICATION,
            "currentDeploymentGroupName": GROUP,
        });
        if let Some(named) = named {
            request["deploymentStyle"] = named.clone();
        }
        let (status, body) = emulator.call("UpdateDeploymentGroup", request).await;
        if *served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be served, got {status} with {body:?}"
            );
        } else {
            assert_eq!(
                (
                    status,
                    error_code(&body).as_str(),
                    body["message"].as_str()
                ),
                (
                    400,
                    INVALID_DEPLOYMENT_STYLE,
                    Some(ECS_DEPLOYMENT_STYLE_REQUIRED)
                ),
                "case {case:?}: expected HTTP 400 {INVALID_DEPLOYMENT_STYLE} with \
                 {ECS_DEPLOYMENT_STYLE_REQUIRED:?}, got {status} with {body:?}"
            );
        }
        // Served or refused, the group still runs the one style its platform
        // admits.
        let info = group_info(&emulator, APPLICATION, GROUP).await;
        assert_eq!(
            (
                info["deploymentStyle"]["deploymentType"].as_str(),
                info["deploymentStyle"]["deploymentOption"].as_str(),
            ),
            (Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL")),
            "case {case:?}: expected the group to keep its admissible style, got {info:?}"
        );
    }
}

/// RULE on an UPDATE, the other side: a group on a platform that runs every style
/// is moved onto whichever style the update names, and reports it.
#[tokio::test]
async fn an_update_moves_a_group_of_another_platform_onto_any_style() {
    let emulator = Emulator::new();
    const APPLICATION: &str = "style-update-server";
    const GROUP: &str = "style-update-server-dg";
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": APPLICATION, "computePlatform": "Server"}),
        )
        .await;
    emulator
        .ok(
            "CreateDeploymentGroup",
            json!({
                "applicationName": APPLICATION,
                "deploymentGroupName": GROUP,
                "serviceRoleArn": ROLE,
            }),
        )
        .await;

    let rows: &[(&str, Value, (&str, &str))] = &[
        (
            "onto blue/green with traffic control",
            style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL")),
            ("BLUE_GREEN", "WITH_TRAFFIC_CONTROL"),
        ),
        (
            "onto blue/green without traffic control",
            style(Some("BLUE_GREEN"), Some("WITHOUT_TRAFFIC_CONTROL")),
            ("BLUE_GREEN", "WITHOUT_TRAFFIC_CONTROL"),
        ),
        (
            "naming only the type, so the option defaults",
            style(Some("IN_PLACE"), None),
            ("IN_PLACE", DEFAULT_OPTION),
        ),
        (
            "naming only the option, so the type defaults",
            style(None, Some("WITH_TRAFFIC_CONTROL")),
            (DEFAULT_TYPE, "WITH_TRAFFIC_CONTROL"),
        ),
    ];

    for (case, named, (deployment_type, deployment_option)) in rows {
        emulator
            .ok(
                "UpdateDeploymentGroup",
                json!({
                    "applicationName": APPLICATION,
                    "currentDeploymentGroupName": GROUP,
                    "deploymentStyle": named,
                }),
            )
            .await;
        let info = group_info(&emulator, APPLICATION, GROUP).await;
        assert_eq!(
            (
                info["deploymentStyle"]["deploymentType"].as_str(),
                info["deploymentStyle"]["deploymentOption"].as_str(),
            ),
            (Some(*deployment_type), Some(*deployment_option)),
            "case {case:?}: expected the style {deployment_type}/{deployment_option}, got {info:?}"
        );
    }
}
