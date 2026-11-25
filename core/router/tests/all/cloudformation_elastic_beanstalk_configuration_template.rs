// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::ElasticBeanstalk::ConfigurationTemplate`
//! really creates that saved configuration in Elastic Beanstalk, publishes the value of
//! every output declared over it, and removes it when the stack goes.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API, and the attribute its type
//! publishes is recorded from what that service answered — so an independent read of the
//! owning service finds the resource, every declared `Outputs` entry over it carries an
//! `OutputValue`, and a create that owning service REFUSES fails the resource with that
//! service's own sentence and rolls the stack back.** Two crates have to agree for that to
//! hold (CloudFormation makes the calls, Elastic Beanstalk serves them and owns the saved
//! configuration), so the suite lives here rather than in either of them.
//!
//! The rows of the first test differ along what the rule generalises over: whether the
//! template names its platform by `SolutionStackName` or by `PlatformArn`, how many
//! `OptionSettings` it declares (none, one, two), whether it declares a `Description`,
//! whether the application the configuration belongs to is one the STACK declares or one
//! that already existed in Elastic Beanstalk, and the Region the stack deploys in.
//!
//! The second test pins the refusal half of the same rule over the two Elastic Beanstalk
//! resource types whose create names a platform version: the `ResourceStatusReason` of the
//! failed resource carries Elastic Beanstalk's own sentence followed by the diagnostic an
//! AWS SDK appends to the exception it raises for a refused call — which is what a client
//! reading `DescribeStackEvents` is told why the deployment rolled back.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the GENERATED names, which are CloudFormation's to
//! compute: what is checked about those is that Elastic Beanstalk resolves them to the
//! resources the template declared and that the stack publishes the same name it created.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{
    block_of, element, elements, form_encode, members_of, opt_element, outputs_of,
    physical_ids_of,
};

/// Elastic Beanstalk's API version, as its model declares it (`version: 2010-12-01`), and
/// CloudFormation's.
const EB_API_VERSION: &str = "2010-12-01";
const CFN_API_VERSION: &str = "2010-05-15";

/// How long a poll below waits for a stack operation to reach a terminal status: generous,
/// so a loaded host is not what decides the outcome. The spans both services choose for the
/// transitions one stack operation starts are their own and far shorter.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll below waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(eb::EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one awsQuery operation of `service` in `region`, returning its status and body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    region: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service, region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`, failing the
/// named case unless it answered 200.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        region,
        CFN_API_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Call one Elastic Beanstalk operation over its awsQuery wire, signed for `region`, failing
/// the named case unless it answered 200.
async fn elasticbeanstalk(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "elasticbeanstalk",
        region,
        EB_API_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Look again until the stack `stack_id` reaches a status that is not `*_IN_PROGRESS`, and
/// answer the `DescribeStacks` body that reported it.
///
/// The stack is addressed by its ID rather than by its name, which is what a stack whose
/// DELETION has concluded is still described by.
///
/// The CONDITION is what is waited on, never a fixed time: the spans both services owe are
/// their own, and this only fails where the stack never leaves that status at all.
async fn settled(router: &MultiServiceRouter, case: &str, region: &str, stack_id: &str) -> String {
    let started = Instant::now();
    loop {
        let described =
            cloudformation(router, case, region, "DescribeStacks", &[("StackName", stack_id)])
                .await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if !status.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: the stack never reached a terminal status, last body {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The names the application `application` holds saved configurations under, as
/// `DescribeApplications` reports them — and `None` where that service holds no such
/// application at all.
async fn configuration_templates_of(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    application: &str,
) -> Option<Vec<String>> {
    let described = elasticbeanstalk(
        router,
        case,
        region,
        "DescribeApplications",
        &[("ApplicationNames.member.1", application)],
    )
    .await;
    let applications = block_of(&described, "Applications")?;
    // Read off the Applications block itself rather than off one member of it: a saved
    // configuration's name is carried by a `<member>` INSIDE the application's own member,
    // which is the one shape `members_of` does not read. The request names ONE application,
    // so the block holds that application alone.
    if !applications.contains("<member") {
        return None;
    }
    let templates = block_of(applications, "ConfigurationTemplates").unwrap_or_default();
    Some(elements(templates, "member"))
}

/// The `(Namespace, OptionName) -> Value` map `DescribeConfigurationSettings` reports for one
/// saved configuration, beside the whole answer for a failing assertion to quote.
async fn settings_of(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    application: &str,
    template: &str,
) -> (String, BTreeMap<(String, String), String>) {
    let described = elasticbeanstalk(
        router,
        case,
        region,
        "DescribeConfigurationSettings",
        &[("ApplicationName", application), ("TemplateName", template)],
    )
    .await;
    let settings = block_of(&described, "OptionSettings")
        .map(|options| {
            members_of(options)
                .into_iter()
                .filter_map(|member| {
                    let namespace = elements(member, "Namespace").into_iter().next()?;
                    let option = elements(member, "OptionName").into_iter().next()?;
                    let value = elements(member, "Value").into_iter().next()?;
                    Some(((namespace, option), value))
                })
                .collect()
        })
        .unwrap_or_default();
    (described, settings)
}

/// How a row names the platform version its saved configuration holds settings for: by the
/// solution stack NAME `ListAvailableSolutionStacks` offers, or by the ARN
/// `ListPlatformVersions` reports the same version under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    ByName,
    ByArn,
}

/// One row of the provisioning table: a stack declaring a saved configuration, and what the
/// rule says must then be true of Elastic Beanstalk.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region its resources are created in.
    region: &'static str,
    /// Which published catalog entry the row's platform version is (`eb::state::SOLUTION_STACKS`,
    /// what `ListAvailableSolutionStacks` reports), so no row is the only platform exercised.
    catalog_entry: usize,
    /// Which form the template names that platform version in.
    platform: Platform,
    /// The `(Namespace, OptionName, Value)` entries the template declares as `OptionSettings`,
    /// which the saved configuration then has to hold.
    option_settings: &'static [(&'static str, &'static str, &'static str)],
    /// The `Description` the template declares for the configuration, or `None`.
    description: Option<&'static str>,
    /// The application this configuration belongs to was created in Elastic Beanstalk
    /// DIRECTLY, outside the stack, and the template names it by name — so the stack's
    /// deletion removes the configuration and leaves that application standing.
    application_outside_the_stack: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a configuration on a solution stack name, with two option settings and a \
               description",
        region: "us-west-2",
        catalog_entry: 0,
        platform: Platform::ByName,
        option_settings: &[
            ("aws:autoscaling:asg", "MinSize", "2"),
            (
                "aws:elasticbeanstalk:environment",
                "EnvironmentType",
                "SingleInstance",
            ),
        ],
        description: Some("the saved configuration this stack deploys"),
        application_outside_the_stack: false,
    },
    Row {
        case: "a configuration on a platform ARN, declaring no option settings, in another \
               Region",
        region: "eu-west-1",
        catalog_entry: 1,
        platform: Platform::ByArn,
        option_settings: &[],
        description: None,
        application_outside_the_stack: false,
    },
    Row {
        case: "a configuration with one option setting, on an application the stack does not \
               own, in a third Region",
        region: "ap-southeast-2",
        catalog_entry: 2,
        platform: Platform::ByName,
        option_settings: &[("aws:autoscaling:asg", "MaxSize", "3")],
        description: None,
        application_outside_the_stack: true,
    },
];

/// The catalog entry a row runs on, named the way that row names it.
fn catalog_entry(index: usize) -> &'static eb::state::SolutionStack {
    eb::state::SOLUTION_STACKS
        .get(index)
        .expect("the service publishes several solution stacks")
}

/// The template one provisioning row deploys: the saved configuration, the application it
/// belongs to where the stack owns that application, and the outputs the rule is stated over.
fn template(row: &Row, application: &str) -> String {
    let entry = catalog_entry(row.catalog_entry);
    let mut configuration = json!({
        "ApplicationName": match row.application_outside_the_stack {
            true => json!(application),
            false => json!({ "Ref": "App" }),
        }
    });
    match row.platform {
        Platform::ByName => configuration["SolutionStackName"] = json!(entry.name()),
        Platform::ByArn => configuration["PlatformArn"] = json!(entry.platform_arn(row.region)),
    }
    if let Some(description) = row.description {
        configuration["Description"] = json!(description);
    }
    configuration["OptionSettings"] = Value::Array(
        row.option_settings
            .iter()
            .map(|(namespace, option, value)| {
                json!({ "Namespace": namespace, "OptionName": option, "Value": value })
            })
            .collect(),
    );

    let mut resources = json!({
        "ConfigTemplate": {
            "Type": "AWS::ElasticBeanstalk::ConfigurationTemplate",
            "Properties": configuration
        }
    });
    let mut outputs = json!({
        "ConfigTemplateRef": { "Value": { "Ref": "ConfigTemplate" } },
        "ConfigTemplateNameAtt": {
            "Value": { "Fn::GetAtt": ["ConfigTemplate", "TemplateName"] }
        }
    });
    if !row.application_outside_the_stack {
        resources["App"] = json!({ "Type": "AWS::ElasticBeanstalk::Application" });
        outputs["AppRef"] = json!({ "Value": { "Ref": "App" } });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// RULE: a `CREATE_COMPLETE` stack that declared a saved configuration really created it in
/// Elastic Beanstalk — the application reports it among its `ConfigurationTemplates` and
/// `DescribeConfigurationSettings` reports the platform version and the settings the
/// declaration asked for — every output declared over it carries a value, and the stack's
/// deletion removes it from that service.
#[tokio::test]
async fn a_stack_provisions_the_elastic_beanstalk_configuration_template_it_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("eb-config-template-stack-{index}");
        // The application a row does not let the stack own exists before the stack does, and
        // is this suite's to create — through that service's own API, as a client would.
        let seeded_application = format!("eb-config-template-app-{index}");
        if row.application_outside_the_stack {
            elasticbeanstalk(
                &router,
                case,
                region,
                "CreateApplication",
                &[("ApplicationName", &seeded_application)],
            )
            .await;
        }

        let body = template(row, &seeded_application);
        let created_stack = cloudformation(
            &router,
            case,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &body)],
        )
        .await;
        // Every later call addresses the stack by the ID the create answered with, which is
        // what a stack whose deletion has concluded is still described by.
        let stack_id = element(&created_stack, "StackId");

        let described = settled(&router, case, region, &stack_id).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        // THE defect this suite is about: every output the template declared is reported with
        // a value, so a client building {OutputKey: OutputValue} over them finds them all.
        let declared_outputs: Vec<&str> = match row.application_outside_the_stack {
            true => vec!["ConfigTemplateRef", "ConfigTemplateNameAtt"],
            false => vec!["ConfigTemplateRef", "ConfigTemplateNameAtt", "AppRef"],
        };
        let outputs = outputs_of(&described);
        for key in &declared_outputs {
            let value = outputs.get(*key);
            assert!(
                value.is_some_and(|value| !value.is_empty()),
                "case {case:?}: the {key} output must carry a value, got {value:?} in {described}"
            );
        }

        // And the value it publishes for the configuration is the name it created it under:
        // the physical id of that resource, which is what both its `Ref` and its one
        // published attribute resolve to.
        let resources = cloudformation(
            &router,
            case,
            region,
            "DescribeStackResources",
            &[("StackName", &stack_id)],
        )
        .await;
        let physical_ids = physical_ids_of(&resources);
        let created = physical_ids
            .get("ConfigTemplate")
            .unwrap_or_else(|| panic!("case {case:?}: no ConfigTemplate resource in {resources}"))
            .clone();
        for key in ["ConfigTemplateRef", "ConfigTemplateNameAtt"] {
            assert_eq!(
                outputs.get(key),
                Some(&created),
                "case {case:?}: the {key} output must publish the configuration the stack \
                 created ({created:?}), got {outputs:?}"
            );
        }

        // The independent read: Elastic Beanstalk itself holds that saved configuration on
        // the application the declaration named.
        let application = match row.application_outside_the_stack {
            true => seeded_application.clone(),
            false => outputs
                .get("AppRef")
                .unwrap_or_else(|| panic!("case {case:?}: no AppRef output in {described}"))
                .clone(),
        };
        assert_eq!(
            configuration_templates_of(&router, case, region, &application).await,
            Some(vec![created.clone()]),
            "case {case:?}: the application {application:?} must report the configuration the \
             stack created"
        );

        let (reported, settings) =
            settings_of(&router, case, region, &application, &created).await;
        let entry = catalog_entry(row.catalog_entry);
        let (member, expected) = match row.platform {
            Platform::ByName => ("SolutionStackName", entry.name()),
            Platform::ByArn => ("PlatformArn", entry.platform_arn(region)),
        };
        assert_eq!(
            opt_element(&reported, member),
            Some(expected.clone()),
            "case {case:?}: expected the configuration's {member} to be {expected:?}, got \
             {reported}"
        );
        for (namespace, option, value) in row.option_settings {
            let held = settings.get(&((*namespace).to_string(), (*option).to_string()));
            assert_eq!(
                held.map(String::as_str),
                Some(*value),
                "case {case:?}: expected the configuration to hold {namespace}/{option} = \
                 {value:?}, got {held:?} in {reported}"
            );
        }

        // And the deletion really removes it: the configuration is gone from that service,
        // and an application the stack did not own is still standing without it.
        cloudformation(
            &router,
            case,
            region,
            "DeleteStack",
            &[("StackName", &stack_id)],
        )
        .await;
        let deleted = settled(&router, case, region, &stack_id).await;
        assert_eq!(
            opt_element(&deleted, "StackStatus").as_deref(),
            Some("DELETE_COMPLETE"),
            "case {case:?}: expected the stack's deletion to complete, got {deleted}"
        );
        let remaining = configuration_templates_of(&router, case, region, &application).await;
        match row.application_outside_the_stack {
            true => assert_eq!(
                remaining,
                Some(Vec::new()),
                "case {case:?}: the application the stack did not own must remain, holding no \
                 configuration the stack created"
            ),
            false => assert_eq!(
                remaining, None,
                "case {case:?}: the application the stack owned must be gone with it"
            ),
        }
    }
}

/// RULE: what a stack reports for a resource is read from the service that OWNS it, so a saved
/// configuration removed in Elastic Beanstalk outside the stack is reported as `DELETED` drift
/// — and the stack's own deletion still concludes, because the state it asked for is already
/// the state that service is in.
///
/// The other side of the provisioning rule: the owning service's answer is what the stack
/// reports, whether or not it is what the stack applied.
#[tokio::test]
async fn a_configuration_template_removed_outside_the_stack_is_reported_as_deleted_drift() {
    let case = "a saved configuration deleted in Elastic Beanstalk, outside the stack";
    let region = "us-east-1";
    let router = build_router();

    let body = template(
        &Row {
            case,
            region,
            catalog_entry: 0,
            platform: Platform::ByName,
            option_settings: &[("aws:autoscaling:asg", "MinSize", "1")],
            description: None,
            application_outside_the_stack: false,
        },
        "",
    );
    let created_stack = cloudformation(
        &router,
        case,
        region,
        "CreateStack",
        &[("StackName", "eb-config-template-drift"), ("TemplateBody", &body)],
    )
    .await;
    let stack_id = element(&created_stack, "StackId");
    let described = settled(&router, case, region, &stack_id).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );
    let outputs = outputs_of(&described);
    let application = outputs
        .get("AppRef")
        .unwrap_or_else(|| panic!("case {case:?}: no AppRef output in {described}"))
        .clone();
    let configuration = outputs
        .get("ConfigTemplateRef")
        .unwrap_or_else(|| panic!("case {case:?}: no ConfigTemplateRef output in {described}"))
        .clone();

    // Removed through that service's own API, as a client outside the stack would.
    elasticbeanstalk(
        &router,
        case,
        region,
        "DeleteConfigurationTemplate",
        &[
            ("ApplicationName", &application),
            ("TemplateName", &configuration),
        ],
    )
    .await;
    assert_eq!(
        configuration_templates_of(&router, case, region, &application).await,
        Some(Vec::new()),
        "case {case:?}: the configuration must be gone from Elastic Beanstalk before the drift \
         detection runs"
    );

    cloudformation(
        &router,
        case,
        region,
        "DetectStackDrift",
        &[("StackName", &stack_id)],
    )
    .await;
    // The detection is asynchronous, so the CONDITION is polled: the resource's own drift
    // status is what the rule is stated about.
    let started = Instant::now();
    let drifted = loop {
        let resources = cloudformation(
            &router,
            case,
            region,
            "DescribeStackResources",
            &[("StackName", &stack_id)],
        )
        .await;
        let reported: BTreeMap<String, String> = members_of(&resources)
            .into_iter()
            .filter_map(|member| {
                let logical = elements(member, "LogicalResourceId").into_iter().next()?;
                let drift = elements(member, "StackResourceDriftStatus")
                    .into_iter()
                    .next()?;
                Some((logical, drift))
            })
            .collect();
        match reported.get("ConfigTemplate").map(String::as_str) {
            Some("NOT_CHECKED") | None => {}
            Some(_) => break reported,
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: the drift detection never reported the configuration, last body \
             {resources}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        drifted.get("ConfigTemplate").map(String::as_str),
        Some("DELETED"),
        "case {case:?}: a configuration Elastic Beanstalk no longer holds must be reported \
         DELETED, got {drifted:?}"
    );

    // And the stack's own deletion still concludes: the configuration is already in the state
    // the deletion asks for.
    cloudformation(
        &router,
        case,
        region,
        "DeleteStack",
        &[("StackName", &stack_id)],
    )
    .await;
    let deleted = settled(&router, case, region, &stack_id).await;
    assert_eq!(
        opt_element(&deleted, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the stack's deletion to complete, got {deleted}"
    );
}

/// A solution stack name the published catalog does not offer, which is what makes the create
/// below one Elastic Beanstalk refuses. Written here rather than read from the catalog: the
/// point is that it is NOT one of its entries.
const UNPUBLISHED_SOLUTION_STACK: &str = "64bit Amazon Linux 2 v0.0.0 running nothing";

/// One row of the refusal table: the resource type whose create names the platform version,
/// and the logical id the template declares it under.
struct RefusedRow {
    case: &'static str,
    region: &'static str,
    logical_id: &'static str,
    resource_type: &'static str,
    /// The properties the refused resource declares beside the solution stack name.
    extra_properties: fn() -> Value,
    /// What this type's PROVIDER puts around the exception its SDK raised, as the pair
    /// `(what opens it, what closes it)`: the configuration template's provider reports the
    /// call it made as an operation that failed and quotes the exception inside that sentence,
    /// where the environment's reports the exception alone.
    provider_wording: (&'static str, &'static str),
    /// The `HandlerErrorCode` class this type's provider files the refusal under.
    handler_error_code: &'static str,
}

/// The template one refusal row deploys: an application, and on it the resource whose create
/// names a solution stack Elastic Beanstalk does not publish.
fn refused_template(row: &RefusedRow) -> String {
    let mut properties = (row.extra_properties)();
    properties["ApplicationName"] = json!({ "Ref": "App" });
    properties["SolutionStackName"] = json!(UNPUBLISHED_SOLUTION_STACK);
    json!({
        "Resources": {
            "App": { "Type": "AWS::ElasticBeanstalk::Application" },
            row.logical_id: {
                "Type": row.resource_type,
                "Properties": properties
            }
        }
    })
    .to_string()
}

/// RULE: a resource whose create the owning service REFUSED fails with that service's own
/// sentence, inside the diagnostic an AWS SDK appends to the exception it raises for a refused
/// call — the service the SDK client names, the HTTP status it answered, the id of the call and
/// how many attempts the SDK made at it — wrapped in whatever that type's own provider words
/// around it and filed under the class that provider puts it in; and the stack rolls back,
/// leaving nothing it created behind.
#[tokio::test]
async fn a_refused_elastic_beanstalk_create_fails_the_resource_and_rolls_the_stack_back() {
    let rows = [
        RefusedRow {
            case: "a configuration template on a solution stack the catalog does not offer",
            region: "us-west-2",
            logical_id: "ConfigTemplate",
            resource_type: "AWS::ElasticBeanstalk::ConfigurationTemplate",
            extra_properties: || json!({ "OptionSettings": [] }),
            provider_wording: ("Error occurred during operation '", "'."),
            handler_error_code: "GeneralServiceException",
        },
        RefusedRow {
            case: "an environment on a solution stack the catalog does not offer, in another \
                   Region",
            region: "eu-central-1",
            logical_id: "Environment",
            resource_type: "AWS::ElasticBeanstalk::Environment",
            extra_properties: || json!({ "Description": "an environment that cannot launch" }),
            provider_wording: ("", ""),
            handler_error_code: "InvalidRequest",
        },
    ];

    let router = build_router();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("eb-refused-stack-{index}");
        let body = refused_template(row);
        let created_stack = cloudformation(
            &router,
            case,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &body)],
        )
        .await;
        let stack_id = element(&created_stack, "StackId");

        let described = settled(&router, case, region, &stack_id).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the refused stack to roll back, got {described}"
        );

        // The event for the failed resource says WHY in the words the owning service used,
        // inside the SDK's own diagnostic of the refused call.
        let events = cloudformation(
            &router,
            case,
            region,
            "DescribeStackEvents",
            &[("StackName", &stack_id)],
        )
        .await;
        let reason = members_of(&events)
            .into_iter()
            .filter(|member| {
                elements(member, "LogicalResourceId")
                    .first()
                    .is_some_and(|id| id == row.logical_id)
                    && elements(member, "ResourceStatus")
                        .first()
                        .is_some_and(|status| status == "CREATE_FAILED")
            })
            .filter_map(|member| elements(member, "ResourceStatusReason").into_iter().next())
            .next()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no CREATE_FAILED event for {}: {events}", row.logical_id)
            });
        // The service's own sentence, read from that service rather than restated here, inside
        // the SDK diagnostic of the refused call and inside whatever this type's provider
        // words around the two. The ids the diagnostic carries are per-run, so what is
        // asserted is the fixed text around them.
        let sentence = eb::state::missing_solution_stack_message(UNPUBLISHED_SOLUTION_STACK);
        let (opens, closes) = row.provider_wording;
        let expected_opening = format!(
            "Resource handler returned message: \"{opens}{sentence} (Service: ElasticBeanstalk, \
             Status Code: 400, Request ID: "
        );
        assert!(
            reason.starts_with(&expected_opening),
            "case {case:?}: expected the reason to open with {expected_opening:?}, got {reason:?}"
        );
        for fragment in [
            ") (SDK Attempt Count: ".to_string(),
            format!("{closes}\" (RequestToken: "),
            format!("HandlerErrorCode: {})", row.handler_error_code),
        ] {
            assert!(
                reason.contains(&fragment),
                "case {case:?}: expected the reason to carry {fragment:?}, got {reason:?}"
            );
        }

        // And the rollback left nothing it created behind: the application the stack made is
        // gone from Elastic Beanstalk.
        let applications = elasticbeanstalk(
            &router,
            case,
            region,
            "DescribeApplications",
            &[],
        )
        .await;
        let held = block_of(&applications, "Applications")
            .map(|applications| elements(applications, "ApplicationName"))
            .unwrap_or_default();
        assert!(
            held.is_empty(),
            "case {case:?}: the rollback must leave no application behind, got {held:?}"
        );
        // Nothing is read off the environment either: the create was refused, so that
        // service minted no environment at all.
        let environments = elasticbeanstalk(
            &router,
            case,
            region,
            "DescribeEnvironments",
            &[],
        )
        .await;
        let running = block_of(&environments, "Environments")
            .map(|environments| elements(environments, "EnvironmentName"))
            .unwrap_or_default();
        assert!(
            running.is_empty(),
            "case {case:?}: the rollback must leave no environment behind, got {running:?}"
        );
    }
}
