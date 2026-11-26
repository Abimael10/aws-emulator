// SPDX-License-Identifier: CC0-1.0

//! The action types this service PROVIDES ITSELF — the catalog no caller created —
//! with the per-provider detail an `ActionType` reports for each one: its artifact
//! counts, the configuration properties an action declaration of it must carry (each
//! with the text a console displays for it), and the deep links the console follows to
//! the resource the action acts on.
//!
//! Split out of [`super::action_types`] so one file holds one concern (and so neither
//! file grows past the project's file-length limit); every name stays re-exported from
//! `crate::state`, which is where the handlers reach it.
//!
//! # Where the entries come from
//!
//! The page `ActionTypeId$provider` and `ActionTypeId$owner` both link to in their model
//! documentation, "Valid action types and providers in CodePipeline"
//! (<https://docs.aws.amazon.com/codepipeline/latest/userguide/reference-pipeline-structure.html#actions-valid-providers>),
//! which lists each category's providers and their owners, together with that page's
//! per-action structure reference, which documents for each provider:
//!
//! * the number of input and output artifacts the action takes and produces
//!   ([`BuiltInActionType::input_artifacts`] / [`BuiltInActionType::output_artifacts`]);
//! * its configuration parameters, which of them are required, and what each one is
//!   ([`BuiltInActionType::configuration_properties`]).
//!
//! # What every entry of this catalog reports
//!
//! Three facts hold for every action type the SERVICE publishes, as against a custom
//! action type, whose every member is whatever the caller registered:
//!
//! * each configuration property carries a `description` — the text
//!   `ActionConfigurationProperty$description` is documented as ("[t]he description of
//!   the action configuration property that is displayed to users"), which for a
//!   provider the service publishes is what that provider's documentation says the
//!   parameter is (see the note below on which rows quote it and which paraphrase it). A
//!   catalog entry without one tells a console nothing to display beside the field it
//!   is asking a user to fill in;
//! * each configuration property is reported as a `key` (`key: true`), as CodePipeline's
//!   own catalog reports them: the properties of a published action type are the keys of
//!   the `configuration` map an action declaration of it carries, required and optional
//!   alike;
//! * `queryable` is false on every entry, because `ActionConfigurationProperty$queryable`
//!   documents it as the member that "is used with `PollForJobs`" "[w]hen creating a custom
//!   action" — none of these action types is run by a job worker of the caller's account.
//!
//! # This table IS the set the service publishes
//!
//! One set of `AWS`-owned action types is published, and this table is it: an entry here
//! is what `ListActionTypes` reports and what a pipeline declaration may name
//! ([`super::action_type_is_published`]). An identity published for a declaration but
//! reported by no listing is one a client cannot discover and can still declare, so a
//! published provider is entered HERE rather than in a second list whenever `ActionType`
//! can carry what its reference page documents.
//!
//! Two modelled bounds decide what "can carry" means, and each is answered differently:
//!
//! * `inputArtifactDetails` and `outputArtifactDetails` are REQUIRED members whose two
//!   counts are modelled `@range(min: 0, max: 5)`, so a provider whose documented count is
//!   above that (the CloudFormation deploy action's ten input artifacts, the `Commands`
//!   action's) could only be entered here under a bound that refuses a declaration the real
//!   service accepts — the bound an action is judged against is this table's
//!   ([`super::validate_action_type`]). Those identities stay OUT of this table and are
//!   published by [`super::AWS_ACTION_TYPES_WITHOUT_DETAIL`] alone, which holds an action
//!   of theirs to no artifact bound of this service's;
//! * `actionConfigurationProperties` is OPTIONAL and modelled `@length(min: 0, max: 10)`,
//!   so a provider whose documented parameter table is longer than ten rows IS entered
//!   here — with its identity and its artifact bounds, and no parameter table. A table cut
//!   to ten of its rows would tell a client the parameters it dropped do not exist, which
//!   is worse than a table it does not claim to carry.
//!
//! # Which rows are the catalog's own words
//!
//! Two kinds of text live in the table below, and a later reader should not have to guess
//! which is which:
//!
//! * the entries whose detail the service's own catalog is observed to publish —
//!   `Source/AWS/S3`, `Invoke/AWS/Lambda` and `Test/ThirdParty/GhostInspector` — carry that
//!   text verbatim, property descriptions and URL templates alike (with the Region written as
//!   [`CATALOG_REGION_PLACEHOLDER`]);
//! * every other entry's descriptions and console templates are this file's own wording of
//!   what the provider's reference page documents for that parameter and that console page.
//!   They state the same facts the reference page states, but they are a paraphrase rather
//!   than a quotation, so a row of that kind is the place to correct first when a provider's
//!   published wording turns out to differ.
//!
//! # The Region a deep link names
//!
//! A console deep link is a link into ONE Region's console, so the URL templates below
//! write the Region as [`CATALOG_REGION_PLACEHOLDER`] and
//! [`BuiltInActionType::record`] substitutes the Region the catalog is being read for
//! (`ListActionTypes$regionFilter`, or the Region the request itself was signed for).
//! A partner's own pages carry no Region and name none.
//!
//! A provider whose reference page states no console page for the resource its action acts
//! on carries no template at all. These URLs are REPORTED to a console that follows them,
//! so an entry publishes a link its provider publishes or publishes none; it does not
//! publish a link composed here.
//!
//! The URL templates below are REPORTED, never followed: each is the text of a modelled
//! member (`ActionTypeSettings$entityUrlTemplate` is documented as "[t]he URL returned to
//! the CodePipeline console that provides a deep link to the resources of the external
//! system"), so this module is a table of response strings and nothing here opens a
//! connection, resolves a name or reads a file.

use super::*;

/// What a URL template in this table writes the Region as, substituted by
/// [`BuiltInActionType::record`].
///
/// It is not a `{Config:<property>}` reference ([`super::action_urls`] is what resolves
/// those, against an action's own configuration), so no action's configuration can be
/// read as a Region and no Region can be read as a configuration value.
pub const CATALOG_REGION_PLACEHOLDER: &str = "{region}";

/// One configuration property of an action type the service provides: the members
/// `ActionConfigurationProperty` carries that the provider's reference page states.
///
/// `key` and `queryable` are not here — both are the same for every entry of this
/// catalog; see this module's note on them.
#[derive(Debug, Clone, Copy)]
pub struct BuiltInConfigurationProperty {
    /// `ActionConfigurationProperty$name`: the parameter's name, as an action's
    /// `configuration` map keys it.
    pub name: &'static str,
    /// `ActionConfigurationProperty$description`: what this parameter is, as the
    /// provider's reference page states it, for a console to display beside the field.
    pub description: &'static str,
    /// Whether the provider's reference page documents the parameter as required.
    pub required: bool,
    /// Whether the value is a secret the API hides from a read of the pipeline — a
    /// credential the caller hands the provider.
    pub secret: bool,
}

/// Declare one configuration property, in the order
/// `ActionConfigurationProperty`'s members are declared.
const fn property(
    name: &'static str,
    description: &'static str,
    required: bool,
    secret: bool,
) -> BuiltInConfigurationProperty {
    BuiltInConfigurationProperty {
        name,
        description,
        required,
        secret,
    }
}

/// One entry of the catalog of action types this service provides itself: the four
/// `ActionTypeId` members plus everything else `ActionType` reports for it.
#[derive(Debug, Clone, Copy)]
pub struct BuiltInActionType {
    /// An `ActionCategory` value.
    pub category: &'static str,
    /// An `ActionOwner` value — `AWS` for an action the service runs itself,
    /// `ThirdParty` for one a partner integration runs.
    pub owner: &'static str,
    /// The provider of the service the action calls.
    pub provider: &'static str,
    /// The action type's version.
    pub version: &'static str,
    /// `(minimumCount, maximumCount)` of the action's input artifacts.
    pub input_artifacts: (i32, i32),
    /// `(minimumCount, maximumCount)` of the action's output artifacts.
    pub output_artifacts: (i32, i32),
    /// `ActionTypeSettings$entityUrlTemplate`: the deep link to the resource the action
    /// acts on, with `{Config:<property>}` standing for the value the action's
    /// configuration gives that property and [`CATALOG_REGION_PLACEHOLDER`] for the
    /// Region the catalog is read for.
    pub entity_url_template: Option<&'static str>,
    /// `ActionTypeSettings$executionUrlTemplate`: the link to the external system's own
    /// view of the work this action ran.
    pub execution_url_template: Option<&'static str>,
    /// `ActionTypeSettings$revisionUrlTemplate`: the link to the revision the action
    /// consumed.
    pub revision_url_template: Option<&'static str>,
    /// `ActionTypeSettings$thirdPartyConfigurationUrl`: where a user signs up for the
    /// external service and configures it. Only a `ThirdParty` provider has one.
    pub third_party_configuration_url: Option<&'static str>,
    /// The action's documented configuration parameters, in the order its reference page
    /// lists them, bounded by `ActionConfigurationPropertyList`'s `@length(max: 10)`.
    pub configuration_properties: &'static [BuiltInConfigurationProperty],
}

impl BuiltInActionType {
    /// This entry's identity.
    pub fn key(&self) -> ActionTypeKey {
        ActionTypeKey {
            category: self.category.to_string(),
            owner: self.owner.to_string(),
            provider: self.provider.to_string(),
            version: self.version.to_string(),
        }
    }

    /// The URLs this entry reports for a reader in `region`, or `None` when it names none
    /// at all.
    ///
    /// `None` and an empty `ActionTypeSettings` state the same fact, and the member is
    /// reported either way ([`ActionTypeRecord::to_action_type`] always carries it), so
    /// this is the record's own shape rather than the wire's.
    fn settings(&self, region: &str) -> Option<ActionTypeSettings> {
        let in_region = |template: Option<&'static str>| {
            template.map(|template| template.replace(CATALOG_REGION_PLACEHOLDER, region))
        };
        let settings = ActionTypeSettings {
            third_party_configuration_url: in_region(self.third_party_configuration_url),
            entity_url_template: in_region(self.entity_url_template),
            execution_url_template: in_region(self.execution_url_template),
            revision_url_template: in_region(self.revision_url_template),
        };
        [
            &settings.third_party_configuration_url,
            &settings.entity_url_template,
            &settings.execution_url_template,
            &settings.revision_url_template,
        ]
        .iter()
        .any(|url| url.is_some())
        .then_some(settings)
    }

    /// This entry as a stored record, as a caller in `region` reads it: available in
    /// every Region, carrying the URLs of that Region's console and the configuration
    /// properties its provider documents, and tagged by nobody.
    pub fn record(&self, region: &str) -> ActionTypeRecord {
        ActionTypeRecord {
            settings: self.settings(region),
            configuration_properties: self
                .configuration_properties
                .iter()
                .map(|property| ActionConfigurationProperty {
                    name: property.name.to_string(),
                    required: property.required,
                    // See the module note on both members: every property of an action
                    // type the service publishes is a key, and none of them is queryable.
                    key: true,
                    secret: property.secret,
                    queryable: false,
                    description: Some(property.description.to_string()),
                    r#type: None,
                })
                .collect(),
            input_artifact_details: ArtifactDetails {
                minimum_count: self.input_artifacts.0,
                maximum_count: self.input_artifacts.1,
            },
            output_artifact_details: ArtifactDetails {
                minimum_count: self.output_artifacts.0,
                maximum_count: self.output_artifacts.1,
            },
            region: None,
            // Nobody tagged an action type the service provides itself.
            tags: Vec::new(),
        }
    }
}

/// Declare one catalog entry that names no URL and no configuration property, so an
/// entry below states only what it has.
const fn entry(
    category: &'static str,
    owner: &'static str,
    provider: &'static str,
    version: &'static str,
    input_artifacts: (i32, i32),
    output_artifacts: (i32, i32),
) -> BuiltInActionType {
    BuiltInActionType {
        category,
        owner,
        provider,
        version,
        input_artifacts,
        output_artifacts,
        entity_url_template: None,
        execution_url_template: None,
        revision_url_template: None,
        third_party_configuration_url: None,
        configuration_properties: &[],
    }
}

/// The action types this service provides on its own — the ones no caller created —
/// as `ListActionTypes` ("a summary of all CodePipeline action types associated with
/// your account") reports them beside the account's own registrations.
///
/// See this module's note for where every member of every entry comes from. Every version
/// is `1`, the only version these actions publish.
pub const BUILT_IN_ACTION_TYPES: [BuiltInActionType; 27] = [
    // ---- Approval -------------------------------------------------------------------
    BuiltInActionType {
        // The manual-approval action takes and produces no artifact, and its three
        // parameters are all optional: it carries the message a reviewer reads.
        configuration_properties: &[
            property(
                "NotificationArn",
                "The Amazon SNS topic the approval request is published to",
                false,
                false,
            ),
            property(
                "CustomData",
                "The message displayed to the reviewer with the approval request",
                false,
                false,
            ),
            property(
                "ExternalEntityLink",
                "The URL of the information the reviewer needs to approve or reject",
                false,
                false,
            ),
        ],
        ..entry("Approval", "AWS", "Manual", "1", (0, 0), (0, 0))
    },
    // ---- Build ----------------------------------------------------------------------
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/codesuite/codebuild/projects/{Config:ProjectName}?region={region}",
        ),
        execution_url_template: Some(
            "https://console.aws.amazon.com/codesuite/codebuild/projects/{Config:ProjectName}/history?region={region}",
        ),
        configuration_properties: CODEBUILD_PROPERTIES,
        ..entry("Build", "AWS", "CodeBuild", "1", (1, 5), (0, 5))
    },
    BuiltInActionType {
        configuration_properties: ECR_BUILD_AND_PUBLISH_PROPERTIES,
        ..entry("Build", "AWS", "ECRBuildAndPublish", "1", (1, 1), (0, 0))
    },
    // ---- Deploy ---------------------------------------------------------------------
    BuiltInActionType {
        // The deployment is made of a configuration the input artifact carries, into the
        // application, environment and configuration profile the parameters name.
        configuration_properties: &[
            property(
                "Application",
                "The AppConfig application the configuration is deployed in",
                true,
                false,
            ),
            property(
                "Environment",
                "The AppConfig environment the configuration is deployed to",
                true,
                false,
            ),
            property(
                "ConfigurationProfile",
                "The AppConfig configuration profile that is deployed",
                true,
                false,
            ),
            property(
                "DeploymentStrategy",
                "The AppConfig deployment strategy the deployment follows",
                true,
                false,
            ),
            property(
                "InputArtifactConfigurationPath",
                "The path to the configuration inside the input artifact",
                true,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "AppConfig", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        configuration_properties: &[
            property(
                "StackSetName",
                "The stack set the stack instances are created in",
                true,
                false,
            ),
            property(
                "DeploymentTargets",
                "The accounts or organizational units the instances are created in",
                false,
                false,
            ),
            property(
                "Regions",
                "The Regions the stack instances are created in",
                false,
                false,
            ),
            property(
                "ParameterOverrides",
                "The stack set parameters overridden for these instances",
                false,
                false,
            ),
            property(
                "FailureTolerancePercentage",
                "The percentage of instances that may fail before the operation stops",
                false,
                false,
            ),
            property(
                "MaxConcurrentPercentage",
                "The percentage of accounts the operation deploys to at once",
                false,
                false,
            ),
        ],
        ..entry(
            "Deploy",
            "AWS",
            "CloudFormationStackInstances",
            "1",
            (0, 1),
            (0, 0),
        )
    },
    // The stack-set action's documented parameter table is longer than
    // `ActionConfigurationPropertyList`'s `@length(max: 10)` (the stack set's own members,
    // the permission model and its two roles, the deployment targets and Regions, and the
    // concurrency figures), so this entry publishes the identity and the artifact bounds
    // and no table: see this module's note on that bound.
    entry(
        "Deploy",
        "AWS",
        "CloudFormationStackSet",
        "1",
        (0, 1),
        (0, 0),
    ),
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/codesuite/codedeploy/applications/{Config:ApplicationName}/deployment-groups/{Config:DeploymentGroupName}?region={region}",
        ),
        configuration_properties: &[
            property(
                "ApplicationName",
                "The CodeDeploy application the deployment is made with",
                true,
                false,
            ),
            property(
                "DeploymentGroupName",
                "The CodeDeploy deployment group the revision is deployed to",
                true,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "CodeDeploy", "1", (1, 1), (0, 0))
    },
    // The ECS blue/green action's documented parameter table is longer than
    // `ActionConfigurationPropertyList`'s `@length(max: 10)` (the CodeDeploy application
    // and deployment group, the task definition and AppSpec artifacts and their paths, and
    // an artifact and container name for each image it replaces), so this entry publishes
    // the identity and the artifact bounds and no table: see this module's note on that
    // bound.
    entry("Deploy", "AWS", "CodeDeployToECS", "1", (1, 5), (0, 0)),
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/ecs/home?region={region}#/clusters/{Config:ClusterName}/services/{Config:ServiceName}/details",
        ),
        configuration_properties: &[
            property(
                "ClusterName",
                "The Amazon ECS cluster the service to update runs in",
                true,
                false,
            ),
            property(
                "ServiceName",
                "The Amazon ECS service whose task definition is updated",
                true,
                false,
            ),
            property(
                "FileName",
                "The name of the image definitions file in the input artifact",
                false,
                false,
            ),
            property(
                "DeploymentTimeout",
                "How many minutes the deployment is given to become stable",
                false,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "ECS", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        // The action applies the Kubernetes manifests, or the Helm chart, that the input
        // artifact carries to a cluster; the network parameters are how it reaches a
        // cluster whose endpoint is private.
        configuration_properties: &[
            property(
                "ClusterName",
                "The Amazon EKS cluster the manifests or chart are applied to",
                true,
                false,
            ),
            property(
                "Namespace",
                "The Kubernetes namespace the resources are applied in",
                false,
                false,
            ),
            property(
                "ManifestFilePaths",
                "The paths to the Kubernetes manifests in the input artifact",
                false,
                false,
            ),
            property(
                "HelmChartLocation",
                "The path to the Helm chart in the input artifact",
                false,
                false,
            ),
            property(
                "HelmReleaseName",
                "The name of the Helm release the chart is installed as",
                false,
                false,
            ),
            property(
                "HelmValuesFiles",
                "The paths to the Helm values files in the input artifact",
                false,
                false,
            ),
            property(
                "SubnetIds",
                "The subnets the action reaches a private cluster through",
                false,
                false,
            ),
            property(
                "SecurityGroupIds",
                "The security groups the action reaches a private cluster with",
                false,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "EKS", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/elasticbeanstalk/home?region={region}#/environment/dashboard?applicationName={Config:ApplicationName}&environmentName={Config:EnvironmentName}",
        ),
        configuration_properties: &[
            property(
                "ApplicationName",
                "The Elastic Beanstalk application the revision is deployed to",
                true,
                false,
            ),
            property(
                "EnvironmentName",
                "The Elastic Beanstalk environment the revision is deployed to",
                true,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "ElasticBeanstalk", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/opsworks/home?region={region}#/stack/{Config:Stack}",
        ),
        configuration_properties: &[
            property(
                "Stack",
                "The OpsWorks Stacks stack the app is deployed in",
                true,
                false,
            ),
            property("App", "The OpsWorks Stacks app that is deployed", true, false),
            property(
                "Layer",
                "The OpsWorks Stacks layer whose instances the app is deployed to",
                false,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "OpsWorks", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/s3/buckets/{Config:BucketName}?region={region}",
        ),
        configuration_properties: &[
            property(
                "BucketName",
                "The Amazon S3 bucket the input artifact is deployed to",
                true,
                false,
            ),
            property(
                "Extract",
                "Whether the input artifact's archive is extracted into the bucket",
                true,
                false,
            ),
            property(
                "ObjectKey",
                "The Amazon S3 object key the artifact is deployed under",
                false,
                false,
            ),
            property(
                "KMSEncryptionKeyARN",
                "The AWS KMS key the deployed object is encrypted with",
                false,
                false,
            ),
            property(
                "CannedACL",
                "The canned ACL applied to the deployed object",
                false,
                false,
            ),
            property(
                "CacheControl",
                "The cache control headers the deployed object is served with",
                false,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "S3", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        // The action creates a new version of a Service Catalog product from the template
        // the input artifact carries, described either by the parameters below or by a
        // configuration file inside that artifact.
        configuration_properties: &[
            property(
                "ProductId",
                "The Service Catalog product the new version is created for",
                true,
                false,
            ),
            property(
                "ProductType",
                "The type of product the new version describes",
                false,
                false,
            ),
            property(
                "ProductVersionName",
                "The name the new product version is created under",
                false,
                false,
            ),
            property(
                "ProductVersionDescription",
                "The description the new product version carries",
                false,
                false,
            ),
            property(
                "TemplateFilePath",
                "The path to the product template in the input artifact",
                false,
                false,
            ),
            property(
                "ConfigurationFilePath",
                "The path to the product configuration file in the input artifact",
                false,
                false,
            ),
        ],
        ..entry("Deploy", "AWS", "ServiceCatalog", "1", (1, 1), (0, 0))
    },
    // ---- Invoke ---------------------------------------------------------------------
    BuiltInActionType {
        configuration_properties: ECR_BUILD_AND_PUBLISH_PROPERTIES,
        ..entry("Invoke", "AWS", "ECRBuildAndPublish", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        configuration_properties: INSPECTOR_SCAN_PROPERTIES,
        ..entry("Invoke", "AWS", "InspectorScan", "1", (1, 1), (0, 1))
    },
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/lambda/home?region={region}#/functions/{Config:FunctionName}",
        ),
        configuration_properties: &[
            property("FunctionName", "The AWS Lambda function to invoke", true, false),
            property(
                "UserParameters",
                "Extra parameters for the AWS Lambda function",
                false,
                false,
            ),
        ],
        ..entry("Invoke", "AWS", "Lambda", "1", (0, 5), (0, 5))
    },
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/states/home?region={region}#/statemachines/view/{Config:StateMachineArn}",
        ),
        configuration_properties: &[
            property(
                "StateMachineArn",
                "The state machine an execution is started of",
                true,
                false,
            ),
            property(
                "ExecutionNamePrefix",
                "The prefix the name of the started execution carries",
                false,
                false,
            ),
            property(
                "InputType",
                "Where the execution's input comes from: a literal value or a file",
                false,
                false,
            ),
            property(
                "Input",
                "The input the execution is started with",
                false,
                false,
            ),
        ],
        ..entry("Invoke", "AWS", "StepFunctions", "1", (0, 1), (0, 1))
    },
    // ---- Source ---------------------------------------------------------------------
    BuiltInActionType {
        // The console page for a source action's resource is the branch the action detects
        // changes on, named by the action's own `BranchName` configuration: the template
        // takes that name, not a git ref built around it.
        entity_url_template: Some(
            "https://console.aws.amazon.com/codesuite/codecommit/repositories/{Config:RepositoryName}/browse/{Config:BranchName}?region={region}",
        ),
        configuration_properties: &[
            property(
                "RepositoryName",
                "The CodeCommit repository source changes are detected in",
                true,
                false,
            ),
            property(
                "BranchName",
                "The branch source changes are detected on",
                true,
                false,
            ),
            property(
                "PollForSourceChanges",
                "The option to poll for source repository changes",
                false,
                false,
            ),
            property(
                "OutputArtifactFormat",
                "The output artifact format: the zipped source, or a clone reference",
                false,
                false,
            ),
        ],
        ..entry("Source", "AWS", "CodeCommit", "1", (0, 0), (1, 1))
    },
    BuiltInActionType {
        // The repository lives behind a connection to the external provider, so the
        // console links the connection rather than a resource of this account.
        configuration_properties: &[
            property(
                "ConnectionArn",
                "The connection to the external source provider",
                true,
                false,
            ),
            property(
                "FullRepositoryId",
                "The owner and name of the repository source changes are detected in",
                true,
                false,
            ),
            property(
                "BranchName",
                "The branch source changes are detected on",
                true,
                false,
            ),
            property(
                "OutputArtifactFormat",
                "The output artifact format: the zipped source, or a clone reference",
                false,
                false,
            ),
            property(
                "DetectChanges",
                "Whether a change to the branch starts the pipeline",
                false,
                false,
            ),
        ],
        ..entry(
            "Source",
            "AWS",
            "CodeStarSourceConnection",
            "1",
            (0, 0),
            (1, 1),
        )
    },
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/ecr/repositories/{Config:RepositoryName}?region={region}",
        ),
        configuration_properties: &[
            property(
                "RepositoryName",
                "The Amazon ECR repository image pushes are detected in",
                true,
                false,
            ),
            property(
                "ImageTag",
                "The image tag a push is detected for",
                false,
                false,
            ),
        ],
        ..entry("Source", "AWS", "ECR", "1", (0, 0), (1, 1))
    },
    BuiltInActionType {
        // The console page for this action's resource is the Region's bucket list: the
        // object the action polls is named by the action's own configuration, which the
        // console reads from the pipeline rather than from the link.
        entity_url_template: Some("https://console.aws.amazon.com/s3/home?region={region}#"),
        configuration_properties: &[
            property("S3Bucket", "The Amazon S3 Bucket", true, false),
            property("S3ObjectKey", "The Amazon S3 object key", true, false),
            property(
                "PollForSourceChanges",
                "The option to poll for source repository changes",
                false,
                false,
            ),
            property(
                "AllowOverrideForS3ObjectKey",
                "Whether S3 Object Key can be overridden during StartPipelineExecution",
                false,
                false,
            ),
        ],
        ..entry("Source", "AWS", "S3", "1", (0, 0), (1, 1))
    },
    BuiltInActionType {
        // The GitHub (OAuth) source action: the repository is the external system, so
        // every link is a github.com one and the token the caller hands over is secret.
        entity_url_template: Some("https://github.com/{Config:Owner}/{Config:Repo}/tree/{Config:Branch}"),
        revision_url_template: Some("https://github.com/{Config:Owner}/{Config:Repo}/commits"),
        third_party_configuration_url: Some("https://github.com/join"),
        configuration_properties: &[
            property(
                "Owner",
                "The GitHub account or organization that owns the repository",
                true,
                false,
            ),
            property(
                "Repo",
                "The repository source changes are detected in",
                true,
                false,
            ),
            property(
                "Branch",
                "The branch source changes are detected on",
                true,
                false,
            ),
            property(
                "OAuthToken",
                "The token the pipeline reads the repository with",
                true,
                true,
            ),
            property(
                "PollForSourceChanges",
                "The option to poll for source repository changes",
                false,
                false,
            ),
        ],
        ..entry("Source", "ThirdParty", "GitHub", "1", (0, 0), (1, 1))
    },
    // ---- Test -----------------------------------------------------------------------
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/codesuite/codebuild/projects/{Config:ProjectName}?region={region}",
        ),
        execution_url_template: Some(
            "https://console.aws.amazon.com/codesuite/codebuild/projects/{Config:ProjectName}/history?region={region}",
        ),
        configuration_properties: CODEBUILD_PROPERTIES,
        ..entry("Test", "AWS", "CodeBuild", "1", (1, 5), (0, 5))
    },
    BuiltInActionType {
        entity_url_template: Some(
            "https://console.aws.amazon.com/devicefarm/home?region={region}#/projects/{Config:ProjectId}/runs",
        ),
        configuration_properties: &[
            property(
                "ProjectId",
                "The Device Farm project the test run belongs to",
                true,
                false,
            ),
            property(
                "DevicePoolArn",
                "The device pool the test runs on",
                true,
                false,
            ),
            property(
                "AppType",
                "The type of application the test runs against",
                true,
                false,
            ),
            property(
                "App",
                "The path to the application in the input artifact",
                true,
                false,
            ),
            property("TestType", "The type of test that is run", true, false),
            property(
                "Test",
                "The path to the test definition in the input artifact",
                false,
                false,
            ),
            property(
                "TestSpec",
                "The path to the test spec file in the input artifact",
                false,
                false,
            ),
        ],
        ..entry("Test", "AWS", "DeviceFarm", "1", (1, 1), (0, 0))
    },
    BuiltInActionType {
        configuration_properties: INSPECTOR_SCAN_PROPERTIES,
        ..entry("Test", "AWS", "InspectorScan", "1", (1, 1), (0, 1))
    },
    BuiltInActionType {
        // A partner test integration: the account's pipeline calls the partner's service
        // with the caller's own credential for it, which is why the token is secret. The
        // partner's own pages are the deep links, and its sign-up page is where a user
        // configures the integration.
        entity_url_template: Some("https://app.ghostinspector.com/tests/{Config:TestId}"),
        execution_url_template: Some(
            "https://app.ghostinspector.com/results/{ExternalExecutionId}",
        ),
        third_party_configuration_url: Some("https://app.ghostinspector.com/code-pipeline"),
        configuration_properties: &[
            property("ClientId", "The user client id", true, false),
            property("ClientToken", "The user client token", true, true),
            property(
                "TestId",
                "The test id used by the Ghost Inspector tests",
                true,
                false,
            ),
            property(
                "ExtraParams",
                "A JSON-encoded string of extra parameters to provide to the test or suite at runtime.",
                false,
                false,
            ),
        ],
        ..entry("Test", "ThirdParty", "GhostInspector", "1", (0, 1), (0, 0))
    },
];

/// The CodeBuild action's configuration parameters, shared by the two categories it is
/// published in (`Build` and `Test`): one provider, one documented parameter list.
const CODEBUILD_PROPERTIES: &[BuiltInConfigurationProperty] = &[
    property(
        "ProjectName",
        "The CodeBuild build project that runs the action",
        true,
        false,
    ),
    property(
        "PrimarySource",
        "The input artifact whose source the build runs against",
        false,
        false,
    ),
    property(
        "BatchEnabled",
        "Whether the action runs the project as a batch build",
        false,
        false,
    ),
    property(
        "CombineArtifacts",
        "Whether a batch build combines its artifacts into one output artifact",
        false,
        false,
    ),
    property(
        "EnvironmentVariables",
        "The environment variables the build runs with, as a JSON array",
        false,
        false,
    ),
];

/// The ECR build-and-publish action's configuration parameters, shared by the two
/// categories it is published in (`Build` and `Invoke`): one provider, one documented
/// parameter list.
const ECR_BUILD_AND_PUBLISH_PROPERTIES: &[BuiltInConfigurationProperty] = &[
    property(
        "ECRRepositoryName",
        "The Amazon ECR repository the built image is pushed to",
        true,
        false,
    ),
    property(
        "DockerFilePath",
        "The path to the Dockerfile in the input artifact",
        false,
        false,
    ),
    property(
        "ImageTags",
        "The tags the pushed image is published under",
        false,
        false,
    ),
    property(
        "RegistryType",
        "Whether the repository is a private or a public registry",
        false,
        false,
    ),
];

/// The Amazon Inspector scan action's configuration parameters, shared by the two
/// categories it is published in (`Invoke` and `Test`): one provider, one documented
/// parameter list.
///
/// The run mode decides what the scan reads, and the four thresholds how many findings of
/// each severity fail the action.
const INSPECTOR_SCAN_PROPERTIES: &[BuiltInConfigurationProperty] = &[
    property(
        "InspectorRunMode",
        "Whether the scan reads the source in the input artifact or a container image",
        true,
        false,
    ),
    property(
        "ECRRepositoryName",
        "The Amazon ECR repository holding the image a container scan reads",
        false,
        false,
    ),
    property(
        "ImageTag",
        "The tag of the image a container scan reads",
        false,
        false,
    ),
    property(
        "CriticalThreshold",
        "How many critical findings fail the action",
        false,
        false,
    ),
    property(
        "HighThreshold",
        "How many high findings fail the action",
        false,
        false,
    ),
    property(
        "MediumThreshold",
        "How many medium findings fail the action",
        false,
        false,
    ),
    property(
        "LowThreshold",
        "How many low findings fail the action",
        false,
        false,
    ),
];
