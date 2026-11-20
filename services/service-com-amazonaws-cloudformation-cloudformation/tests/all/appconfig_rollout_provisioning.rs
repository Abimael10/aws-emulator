// SPDX-License-Identifier: CC0-1.0

//! The AWS AppConfig ROLLOUT a stack declares, end to end: the hosted configuration version and
//! the deployment of it exist in AppConfig itself, under the identifiers that service minted.
//!
//! RULE: for a resource type whose owning service MINTS the NUMBER that identifies the resource,
//! the stack creates the resource through that service's own create and publishes what that
//! service answered — the number through `Fn::GetAtt`, and the resource's whole primary
//! identifier as its physical id — so a sibling resource whose property reads that number is
//! created against the resource that really exists, and every follow-on call a client makes with
//! the published values resolves. `AWS::AppConfig::HostedConfigurationVersion` and
//! `AWS::AppConfig::Deployment` are such resources: `CreateHostedConfigurationVersion` takes the
//! content and answers the `VersionNumber` AppConfig assigned, and `StartDeployment` takes no
//! identifier and answers the `DeploymentNumber` it allocated
//! (`smithy_query(query="operation",operation="CreateHostedConfigurationVersion,StartDeployment")`
//! on that service's model), and each type is addressed by the triple of its parents and that
//! number.
//!
//! The second half of the rule is the stabilisation: CloudFormation does not report a resource
//! created on the strength of an accepted call, and `StartDeployment` answers a rollout at its
//! first stage for any strategy that paces one — so the stack's create is held open until
//! AppConfig reports the deployment in the state it no longer leaves on its own, and the `State`
//! the stack publishes is the one that service reports THEN.
//!
//! The rows differ along what the rule generalises over: how many versions of one profile the
//! template hosts (the number is minted per version, so the second is not the first), whether a
//! deployment reads its `ConfigurationVersion` through `Fn::GetAtt VersionNumber` or states the
//! number outright, the media type and content the version carries, and whether the strategy
//! PACES the rollout — a strategy with no window and no bake time leaves AppConfig nothing to
//! pace, which the recorded service answers already complete, while one with a window makes the
//! deployment a resource the stack waits for. The refusing side is a deployment naming a strategy
//! AppConfig holds nothing under: nothing is reported created for it and the stack does not
//! complete.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds —
//! the CloudFormation calls over its awsQuery wire, the AppConfig reads over that service's own
//! `restJson1` wire — so what is asserted is what a client that deploys the template and then
//! calls AppConfig observes. The stack is always waited for by POLLING its status, never for a
//! span of time.

use serde_json::{json, Value};

use crate::appconfig_estate_provisioning::{
    appconfig, appconfig_payload, id_of, output, physical_ids, COMPOSITE_IDENTITY_SEPARATOR,
};
use crate::stack_lifecycle::{call, create_stack, member, router, xml_text, xml_texts};

/// The terminal `DeploymentState` a rollout that carried its configuration all the way out
/// reaches, which is what `Fn::GetAtt State` publishes for a completed deployment
/// (`smithy_query(query="shape",shape="DeploymentState")`).
const DEPLOYMENT_COMPLETE: &str = "COMPLETE";

/// The percentage of targets a finished rollout has reached, as `Deployment$PercentageComplete`
/// reports it — "the percentage of targets for which the deployment is available".
const FULL_PERCENTAGE: f64 = 100.0;

/// One row of the table: a whole rollout template and what the stack must publish for it.
struct Rollout {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The version numbers the template's hosted versions must be minted at, in declaration
    /// order — AppConfig allocates them per profile, starting at one.
    versions: &'static [&'static str],
    /// The content the DEPLOYED version carries, which is what the deployment's configuration is
    /// read back as.
    deployed_content: &'static str,
    /// The media type that version was hosted under.
    content_type: &'static str,
}

/// The rollouts the rule is asserted over.
fn rollouts() -> Vec<Rollout> {
    vec![
        Rollout {
            case: "one hosted version, the deployment reading its number through Fn::GetAtt, a \
                   strategy that paces nothing",
            stack: "appconfig-rollout-getatt",
            template: rollout_template(RolloutShape {
                extra_version: false,
                configuration_version: json!({ "Fn::GetAtt": ["Version", "VersionNumber"] }),
                content: "{\"enabled\": true}",
                content_type: "application/json",
                duration_in_minutes: 0,
            }),
            versions: &["1"],
            deployed_content: "{\"enabled\": true}",
            content_type: "application/json",
        },
        Rollout {
            case: "two hosted versions of one profile, the deployment stating the second \
                   number outright, plain text content, a strategy that paces a window",
            stack: "appconfig-rollout-stated",
            template: rollout_template(RolloutShape {
                extra_version: true,
                configuration_version: json!("2"),
                content: "a-flag=on",
                content_type: "text/plain",
                duration_in_minutes: 1,
            }),
            versions: &["1", "2"],
            deployed_content: "a-flag=on",
            content_type: "text/plain",
        },
    ]
}

/// What one row's template declares differently.
struct RolloutShape {
    /// Whether a SECOND hosted version of the same profile is declared, which is what makes the
    /// deployed version's number the second AppConfig minted rather than the first.
    extra_version: bool,
    /// How the deployment names the configuration version it deploys.
    configuration_version: Value,
    /// The content the DEPLOYED version carries, and the media type it is hosted under.
    content: &'static str,
    content_type: &'static str,
    /// The strategy's rollout window: zero paces nothing, which the owning service answers
    /// already complete, and a minute paces one stage the stack waits out.
    duration_in_minutes: u32,
}

/// One rollout template: the estate the two rollout types live inside, the hosted version(s), the
/// deployment, and the outputs a client reads the published values out of.
fn rollout_template(shape: RolloutShape) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Application".to_string(),
        json!({ "Type": "AWS::AppConfig::Application" }),
    );
    resources.insert(
        "Environment".to_string(),
        json!({
            "Type": "AWS::AppConfig::Environment",
            "Properties": { "ApplicationId": { "Ref": "Application" } }
        }),
    );
    resources.insert(
        "Profile".to_string(),
        json!({
            "Type": "AWS::AppConfig::ConfigurationProfile",
            "Properties": {
                "ApplicationId": { "Ref": "Application" },
                "LocationUri": "hosted"
            }
        }),
    );
    resources.insert(
        "Strategy".to_string(),
        json!({
            "Type": "AWS::AppConfig::DeploymentStrategy",
            "Properties": {
                "DeploymentDurationInMinutes": shape.duration_in_minutes,
                "FinalBakeTimeInMinutes": 0,
                "GrowthFactor": 100,
                "ReplicateTo": "NONE"
            }
        }),
    );
    // The FIRST version of the profile, declared only where the row hosts two: the deployed one
    // is always the LAST declared, so the row that hosts two deploys the second number.
    if shape.extra_version {
        resources.insert(
            "FirstVersion".to_string(),
            hosted_version("{\"enabled\": false}", "application/json", None),
        );
    }
    resources.insert(
        "Version".to_string(),
        hosted_version(
            shape.content,
            shape.content_type,
            shape.extra_version.then_some("FirstVersion"),
        ),
    );
    resources.insert(
        "Deployment".to_string(),
        json!({
            "Type": "AWS::AppConfig::Deployment",
            "Properties": {
                "ApplicationId": { "Ref": "Application" },
                "EnvironmentId": { "Ref": "Environment" },
                "ConfigurationProfileId": { "Ref": "Profile" },
                "ConfigurationVersion": shape.configuration_version,
                "DeploymentStrategyId": { "Ref": "Strategy" },
                "Description": "the declared rollout"
            }
        }),
    );
    let mut outputs = serde_json::Map::new();
    outputs.insert(
        "AppId".to_string(),
        json!({ "Value": { "Ref": "Application" } }),
    );
    outputs.insert(
        "EnvId".to_string(),
        json!({ "Value": { "Ref": "Environment" } }),
    );
    outputs.insert(
        "ProfileId".to_string(),
        json!({ "Value": { "Ref": "Profile" } }),
    );
    outputs.insert(
        "StrategyId".to_string(),
        json!({ "Value": { "Ref": "Strategy" } }),
    );
    outputs.insert(
        "VersionNumber".to_string(),
        json!({ "Value": { "Fn::GetAtt": ["Version", "VersionNumber"] } }),
    );
    if shape.extra_version {
        outputs.insert(
            "FirstVersionNumber".to_string(),
            json!({ "Value": { "Fn::GetAtt": ["FirstVersion", "VersionNumber"] } }),
        );
    }
    outputs.insert(
        "DeploymentNumber".to_string(),
        json!({ "Value": { "Fn::GetAtt": ["Deployment", "DeploymentNumber"] } }),
    );
    outputs.insert(
        "DeploymentState".to_string(),
        json!({ "Value": { "Fn::GetAtt": ["Deployment", "State"] } }),
    );
    json!({
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// One hosted configuration version of the template's profile. `after` is the sibling version it
/// is declared to follow, which is what fixes the order AppConfig mints the two numbers in.
fn hosted_version(content: &str, content_type: &str, after: Option<&str>) -> Value {
    let mut declared = json!({
        "Type": "AWS::AppConfig::HostedConfigurationVersion",
        "Properties": {
            "ApplicationId": { "Ref": "Application" },
            "ConfigurationProfileId": { "Ref": "Profile" },
            "ContentType": content_type,
            "Content": content
        }
    });
    if let Some(after) = after {
        declared["DependsOn"] = json!(after);
    }
    declared
}

/// The stack's outputs, keyed by `OutputKey`, read off the body the settled stack was reported
/// by.
///
/// The sibling suite's reader takes the stack NAME and calls `DescribeStacks` itself; this one
/// reads the body the shared waiter already answered with, so the outputs asserted are the ones
/// the settled read reported rather than those of a second call.
fn outputs(case: &str, body: &str) -> Vec<(String, String)> {
    let keys = xml_texts(body, "OutputKey");
    let values = xml_texts(body, "OutputValue");
    assert_eq!(
        keys.len(),
        values.len(),
        "case {case:?}: expected one OutputValue per OutputKey {keys:?}, got {values:?} in {body}"
    );
    keys.into_iter().zip(values).collect()
}

/// The three parts of a composite physical id, or a panic naming the case.
fn triple(case: &str, logical_id: &str, identity: &str) -> (String, String, String) {
    let parts: Vec<&str> = identity.split(COMPOSITE_IDENTITY_SEPARATOR).collect();
    assert_eq!(
        parts.len(),
        3,
        "case {case:?}: expected {logical_id:?} to be identified by the triple its own API \
         addresses one resource by, got {identity:?}"
    );
    (
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    )
}

/// Both halves of the rule, over every row: the numbers AppConfig minted are what the stack
/// publishes, the resources exist in that service under them, and the deployment the stack
/// reported created really carried the declared configuration out.
#[tokio::test]
async fn an_appconfig_rollout_exists_under_the_numbers_appconfig_minted() {
    for rollout in rollouts() {
        let router = router();
        let Rollout {
            case,
            stack,
            template,
            versions,
            deployed_content,
            content_type,
        } = &rollout;

        let (status, body) = create_stack(&router, stack, template, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} with {body}"
        );
        // The stack is POLLED until it settles: the rollout of a paced strategy is due at an
        // instant, so what is waited for is the status, never a span of time.
        let settled = crate::stack_lifecycle::settled_stack(&router, case, stack).await;
        assert_eq!(
            xml_text(&settled, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {settled}"
        );

        let published = outputs(case, &settled);
        let reported = physical_ids(&router, case, stack).await;
        let application_id = output(case, &published, "AppId");
        let environment_id = output(case, &published, "EnvId");
        let profile_id = output(case, &published, "ProfileId");
        let strategy_id = output(case, &published, "StrategyId");

        // The version numbers the template's versions were minted at, in declaration order: the
        // number is AppConfig's to allocate per profile, so hosting a second version of one
        // profile publishes the next number rather than the first again.
        let minted: Vec<String> = if versions.len() == 2 {
            vec![
                output(case, &published, "FirstVersionNumber"),
                output(case, &published, "VersionNumber"),
            ]
        } else {
            vec![output(case, &published, "VersionNumber")]
        };
        assert_eq!(
            minted, *versions,
            "case {case:?}: expected the version numbers {versions:?} AppConfig allocates per \
             profile, got {minted:?}"
        );

        // The hosted version's own identity is the triple `GetHostedConfigurationVersion`
        // addresses one by, carrying the ids the stack published for its parents.
        let version_number = output(case, &published, "VersionNumber");
        let version_identity = id_of(case, &reported, "Version");
        assert_eq!(
            triple(case, "Version", &version_identity),
            (
                application_id.clone(),
                profile_id.clone(),
                version_number.clone()
            ),
            "case {case:?}: expected the version to be identified by its application, its \
             profile and the number AppConfig minted, got {version_identity:?}"
        );

        // ...and the version really exists in AppConfig under it, carrying the content the
        // template declared: the call a client makes with the published values is served.
        let (status, served) = appconfig_payload(
            &router,
            "GET",
            &format!(
                "/applications/{application_id}/configurationprofiles/{profile_id}\
                 /hostedconfigurationversions/{version_number}"
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetHostedConfigurationVersion on the published ids to \
             answer 200, got {status} with {served}"
        );
        // The version's content is that operation's `@httpPayload`, so what comes back is the
        // content itself. A JSON document is compared as the document it is — the bytes a
        // serializer writes carry its own layout, and what the version holds is the
        // configuration rather than one formatting of it — and any other media type as text.
        match serde_json::from_str::<Value>(deployed_content) {
            Ok(declared) if *content_type == "application/json" => {
                let reported: Value = serde_json::from_str(&served).unwrap_or(Value::Null);
                assert_eq!(
                    reported, declared,
                    "case {case:?}: expected the hosted version to carry the declared content \
                     {deployed_content:?} (as {content_type}), got {served:?}"
                );
            }
            _ => assert_eq!(
                served, *deployed_content,
                "case {case:?}: expected the hosted version to carry the declared content \
                 {deployed_content:?} (as {content_type}), got {served:?}"
            ),
        }

        // The deployment's identity is the triple `GetDeployment` addresses one by, and the
        // number it ends in is what the stack published.
        let deployment_number = output(case, &published, "DeploymentNumber");
        let deployment_identity = id_of(case, &reported, "Deployment");
        assert_eq!(
            triple(case, "Deployment", &deployment_identity),
            (
                application_id.clone(),
                environment_id.clone(),
                deployment_number.clone()
            ),
            "case {case:?}: expected the deployment to be identified by its application, its \
             environment and the number AppConfig allocated, got {deployment_identity:?}"
        );
        assert!(
            deployment_number.chars().all(|c| c.is_ascii_digit())
                && !deployment_number.is_empty(),
            "case {case:?}: expected the DeploymentNumber AppConfig allocated, got \
             {deployment_number:?}"
        );

        // The state the stack publishes is the one AppConfig reports now, which for a create
        // CloudFormation reported COMPLETE is the terminal one: the resource is not reported
        // created while the rollout is still in flight.
        assert_eq!(
            output(case, &published, "DeploymentState"),
            DEPLOYMENT_COMPLETE,
            "case {case:?}: expected the stack to publish the state AppConfig reports the \
             settled rollout in, got {published:?}"
        );

        // ...and AppConfig itself says the same about the deployment, which is what makes the
        // success a backed one: the declared version was rolled out, by the declared strategy,
        // to every target.
        let (status, deployment) = appconfig(
            &router,
            "GET",
            &format!(
                "/applications/{application_id}/environments/{environment_id}\
                 /deployments/{deployment_number}"
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetDeployment on the published ids to answer 200, got \
             {status} with {deployment}"
        );
        for (member_name, expected) in [
            ("State", Value::String(DEPLOYMENT_COMPLETE.to_string())),
            ("ConfigurationVersion", Value::String(version_number.clone())),
            (
                "ConfigurationProfileId",
                Value::String(profile_id.clone()),
            ),
            (
                "DeploymentStrategyId",
                Value::String(strategy_id.clone()),
            ),
        ] {
            assert_eq!(
                deployment.get(member_name),
                Some(&expected),
                "case {case:?}: expected GetDeployment to report {member_name} as {expected:?}, \
                 got {deployment}"
            );
        }
        let percentage = deployment
            .get("PercentageComplete")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        assert!(
            (percentage - FULL_PERCENTAGE).abs() < f64::EPSILON,
            "case {case:?}: expected the settled rollout to have reached every target \
             ({FULL_PERCENTAGE}%), got {percentage} in {deployment}"
        );
    }
}

/// A deployment naming a strategy AppConfig holds nothing under cannot be started, so the
/// resource reports its own failure and the stack does not complete — nothing is reported created
/// for a rollout that never ran.
#[tokio::test]
async fn a_deployment_naming_no_strategy_fails_the_stack() {
    let router = router();
    let case = "a deployment strategy id nothing resolves to";
    let stack = "appconfig-rollout-unknown-strategy";
    // A well-formed `DeploymentStrategyId` the account holds nothing under: inside that member's
    // own modelled pattern (`^[a-z0-9]{4,7}$`), so what refuses the create is the LOOKUP rather
    // than the request's shape.
    let mut template: Value =
        serde_json::from_str(&rollout_template(RolloutShape {
            extra_version: false,
            configuration_version: json!({ "Fn::GetAtt": ["Version", "VersionNumber"] }),
            content: "{\"enabled\": true}",
            content_type: "application/json",
            duration_in_minutes: 0,
        }))
        .expect("the template is JSON");
    template["Resources"]["Deployment"]["Properties"]["DeploymentStrategyId"] = json!("zzzz999");
    let (status, body) = create_stack(&router, stack, &template.to_string(), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack answered {status} with {body}"
    );
    let settled = crate::stack_lifecycle::settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the create to be rolled back, got {settled}"
    );
    let reason = crate::stack_lifecycle::resource_event_reason(
        &router,
        stack,
        "Deployment",
        "CREATE_FAILED",
    )
    .await;
    assert!(
        reason.contains("zzzz999"),
        "case {case:?}: expected the failed resource to carry AppConfig's own refusal naming \
         the strategy, got {reason:?}"
    );
    // Nothing was created for the deployment: no identifier was minted for it at all, and the
    // stand-in CloudFormation computed was only what a reference resolved against until the
    // create answered — so the stack reports NO physical resource id for it, which is what says
    // no rollout ever happened.
    let reported = physical_ids(&router, case, stack).await;
    assert!(
        !reported
            .iter()
            .any(|(logical_id, _)| logical_id == "Deployment"),
        "case {case:?}: expected the refused deployment to carry no physical resource id, got \
         {reported:?}"
    );
    // ...and the hosted version the failed create was rolled back over is gone from AppConfig:
    // the rollback removed what it had made, so the number the stack had published resolves to
    // nothing.
    let version_identity = id_of(case, &reported, "Version");
    let (application_id, profile_id, version_number) =
        triple(case, "Version", &version_identity);
    let (status, body) = appconfig_payload(
        &router,
        "GET",
        &format!(
            "/applications/{application_id}/configurationprofiles/{profile_id}\
             /hostedconfigurationversions/{version_number}"
        ),
    )
    .await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected AppConfig to hold no version at {version_identity:?} after the \
         rollback, got {status} with {body}"
    );
}

/// A hosted configuration version declaring no content describes no version that could be
/// created, so the resource is failed with the validator's own sentence naming the member — and
/// nothing is asked of AppConfig for it.
#[tokio::test]
async fn a_hosted_version_without_content_is_refused() {
    let router = router();
    let case = "a hosted configuration version stating no Content";
    let stack = "appconfig-rollout-no-content";
    let template = json!({
        "Resources": {
            "Application": { "Type": "AWS::AppConfig::Application" },
            "Profile": {
                "Type": "AWS::AppConfig::ConfigurationProfile",
                "Properties": {
                    "ApplicationId": { "Ref": "Application" },
                    "LocationUri": "hosted"
                }
            },
            "Version": {
                "Type": "AWS::AppConfig::HostedConfigurationVersion",
                "Properties": {
                    "ApplicationId": { "Ref": "Application" },
                    "ConfigurationProfileId": { "Ref": "Profile" },
                    "ContentType": "application/json"
                }
            }
        }
    })
    .to_string();
    let (status, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack answered {status} with {body}"
    );
    let settled = crate::stack_lifecycle::settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the create to be rolled back, got {settled}"
    );
    let reason = crate::stack_lifecycle::resource_event_reason(
        &router,
        stack,
        "Version",
        "CREATE_FAILED",
    )
    .await;
    assert!(
        reason.contains("Content"),
        "case {case:?}: expected the failed resource to name the member that is not there, got \
         {reason:?}"
    );
}

/// The two types' own catalogue entries agree with what the stack publishes for them: the
/// attributes `Fn::GetAtt` reads, the identity being the owning service's to assign, and every
/// property being one a changed declaration REPLACES the resource over.
#[test]
fn the_rollout_types_publish_what_their_resources_report() {
    use service_com_amazonaws_cloudformation_cloudformation::state::template::{
        composite_identity_properties, identity_is_service_assigned, published_attribute_names,
        replacement_properties, APPCONFIG_DEPLOYMENT, APPCONFIG_HOSTED_CONFIGURATION_VERSION,
    };

    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            APPCONFIG_HOSTED_CONFIGURATION_VERSION,
            &["VersionNumber"],
            &["ApplicationId", "ConfigurationProfileId", "VersionNumber"],
        ),
        (
            APPCONFIG_DEPLOYMENT,
            &["DeploymentNumber", "State"],
            &["ApplicationId", "EnvironmentId", "DeploymentNumber"],
        ),
    ];
    for (resource_type, attributes, identity) in cases {
        assert_eq!(
            published_attribute_names(resource_type),
            *attributes,
            "case {resource_type:?}: expected the type's published return values {attributes:?}, \
             got {:?}",
            published_attribute_names(resource_type)
        );
        assert!(
            identity_is_service_assigned(resource_type),
            "case {resource_type:?}: expected the identity to be the owning service's to assign"
        );
        assert_eq!(
            composite_identity_properties(resource_type),
            *identity,
            "case {resource_type:?}: expected the primary identifier {identity:?}, got {:?}",
            composite_identity_properties(resource_type)
        );
        // Every property either type publishes is one a restated declaration replaces the
        // resource over: AppConfig publishes no call that edits a hosted version or a deployment.
        let replaced = replacement_properties(resource_type);
        for property in *identity {
            if *property == "VersionNumber" || *property == "DeploymentNumber" {
                continue;
            }
            assert!(
                replaced.contains(property),
                "case {resource_type:?}: expected {property:?} to replace the resource, got \
                 {replaced:?}"
            );
        }
    }
}

/// Every output of a settled rollout stack carries a value, read back through the operation a
/// client polls: `DescribeStacks` by STACK ID, which is what a waiter holds.
#[tokio::test]
async fn a_rollout_stack_reports_its_outputs_by_stack_id() {
    let router = router();
    let case = "the outputs of a rollout read by stack id";
    let stack = "appconfig-rollout-by-id";
    let template = rollout_template(RolloutShape {
        extra_version: false,
        configuration_version: json!({ "Fn::GetAtt": ["Version", "VersionNumber"] }),
        content: "{\"enabled\": true}",
        content_type: "application/json",
        duration_in_minutes: 0,
    });
    let (status, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack answered {status} with {body}"
    );
    let stack_id = xml_text(&body, "StackId").unwrap_or_default();
    assert!(
        !stack_id.is_empty(),
        "case {case:?}: expected CreateStack to answer a StackId, got {body}"
    );
    let settled = crate::stack_lifecycle::settled_stack(&router, case, &stack_id).await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {settled}"
    );
    let published = outputs(case, &settled);
    for key in [
        "AppId",
        "EnvId",
        "ProfileId",
        "StrategyId",
        "VersionNumber",
        "DeploymentNumber",
        "DeploymentState",
    ] {
        let value = output(case, &published, key);
        assert!(
            !value.is_empty(),
            "case {case:?}: expected the output {key:?} to carry a value, got {published:?}"
        );
    }
    // And the stack's own event stream reports the two rollout resources created rather than
    // failed, which is what a client reads when a deployment did not complete.
    let (status, events) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", &stack_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackEvents answered {status} with {events}"
    );
    assert!(
        !events.contains("CREATE_FAILED"),
        "case {case:?}: expected no resource of the rollout to fail, got {events}"
    );
}
