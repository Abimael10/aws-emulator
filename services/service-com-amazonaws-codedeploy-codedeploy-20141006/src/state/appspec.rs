// SPDX-License-Identifier: CC0-1.0

//! What a revision's AppSpec DECLARES a deployment deploys.
//!
//! A deployment of the `Lambda` or the `ECS` compute platform deploys a revision that
//! IS the AppSpec file: "For an Lambda deployment, the content includes a Lambda
//! function name, the alias for its original version, and the alias for its
//! replacement version. The deployment shifts traffic from the original version of
//! the Lambda function to the replacement version. … For an Amazon ECS deployment,
//! the content includes the task name, information about the load balancer that
//! serves traffic to the container, and more"
//! (`smithy_query(query="shape",shape="AppSpecContent")`). This module is the one
//! place that document is READ, for both of them: what it declares is resolved once,
//! when the deployment is recorded, so no later request re-parses caller-supplied
//! content. One traversal reads it ([`declaration_in_app_spec`]) and one leaf reader
//! per platform says what a declaration of it means — the Lambda traffic shift
//! ([`LambdaTrafficShift`]) and the Amazon ECS replacement task set
//! ([`EcsTaskSetDeclaration`]). Carrying the declared shift out is
//! [`super::lambda_traffic_shift`]'s, the Amazon ECS deployment's steps are
//! [`super::ecs_blue_green`]'s, and the increments either is carried out in are
//! [`super::traffic_shift`]'s.
//!
//! The document is YAML or JSON (YAML 1.2 is a superset of JSON, so one parser
//! reads both), and it is caller-supplied, so it is read through a maintained YAML
//! implementation under bounds this module states and checks BEFORE any parser
//! walks it. Nothing is inferred into the stored revision: the revision is held
//! verbatim, and this only reports what the document already says.

use ::yaml_rust2::Yaml;

use crate::ops::types::RevisionLocation;

/// The `ComputePlatform` whose deployments shift traffic between two versions of a
/// Lambda function (`crate::ops::types::COMPUTE_PLATFORM_VALUES`: ECS | Lambda |
/// Server).
pub const COMPUTE_PLATFORM_LAMBDA: &str = "Lambda";

/// The AppSpec mapping holding the resources a revision deploys.
const RESOURCES_KEY: &str = "Resources";
/// The member of one resource declaration naming what kind of resource it is.
const TYPE_KEY: &str = "Type";
/// The member of one resource declaration carrying what is deployed about it.
const PROPERTIES_KEY: &str = "Properties";
/// The resource type of the Lambda function an Lambda AppSpec deploys, which is
/// the only declaration that describes a traffic shift.
const LAMBDA_FUNCTION_RESOURCE_TYPE: &str = "AWS::Lambda::Function";
/// The property naming the function whose alias the deployment moves.
const NAME_PROPERTY: &str = "Name";
/// The property naming the alias the deployment moves.
const ALIAS_PROPERTY: &str = "Alias";
/// The property naming the version the alias invoked before the deployment.
const CURRENT_VERSION_PROPERTY: &str = "CurrentVersion";
/// The property naming the version the alias invokes after the deployment.
const TARGET_VERSION_PROPERTY: &str = "TargetVersion";
/// The `ComputePlatform` whose deployments replace an Amazon ECS service's task set
/// (`crate::ops::types::COMPUTE_PLATFORM_VALUES`: ECS | Lambda | Server).
pub const COMPUTE_PLATFORM_ECS: &str = "ECS";
/// The resource type of the Amazon ECS service an Amazon ECS AppSpec deploys, which
/// is the only declaration that describes a replacement task set.
const ECS_SERVICE_RESOURCE_TYPE: &str = "AWS::ECS::Service";
/// The property naming the revision the replacement task set runs.
const TASK_DEFINITION_PROPERTY: &str = "TaskDefinition";
/// The property carrying "information about the load balancer that serves traffic to
/// the container", whose two members name that container and its port.
const LOAD_BALANCER_INFO_PROPERTY: &str = "LoadBalancerInfo";
/// See [`LOAD_BALANCER_INFO_PROPERTY`].
const CONTAINER_NAME_PROPERTY: &str = "ContainerName";
/// See [`LOAD_BALANCER_INFO_PROPERTY`].
const CONTAINER_PORT_PROPERTY: &str = "ContainerPort";
/// The property naming the Fargate platform version the replacement runs on.
const PLATFORM_VERSION_PROPERTY: &str = "PlatformVersion";
/// The property carrying the networking the replacement task set is placed with, and
/// the one member of it that describes an `awsvpc` placement.
const NETWORK_CONFIGURATION_PROPERTY: &str = "NetworkConfiguration";
/// See [`NETWORK_CONFIGURATION_PROPERTY`].
const AWSVPC_CONFIGURATION_PROPERTY: &str = "AwsvpcConfiguration";
/// The members of that placement: the subnets its interfaces are created in, the
/// security groups governing them, and whether it takes a public address.
const SUBNETS_PROPERTY: &str = "Subnets";
/// See [`SUBNETS_PROPERTY`].
const SECURITY_GROUPS_PROPERTY: &str = "SecurityGroups";
/// See [`SUBNETS_PROPERTY`].
const ASSIGN_PUBLIC_IP_PROPERTY: &str = "AssignPublicIp";
/// How many entries one list-valued AppSpec property is read from.
///
/// The content is caller-supplied and the document states no length, so this is
/// EMULATOR-CHOSEN, well above the handful of subnets and security groups an
/// `awsvpc` placement names (the ceiling of the Amazon VPC quota on security groups
/// per network interface is 16, and a placement names one subnet per Availability
/// Zone).
const MAX_DECLARED_LIST_ENTRIES: usize = 16;
/// The AppSpec mapping holding the lifecycle event hooks a deployment runs:
/// "A deployment lifecycle hook is specified in the hooks section of the AppSpec
/// file"
/// (`smithy_query(query="doc",shape="PutLifecycleEventHookExecutionStatusInput",member="lifecycleEventHookExecutionId")`).
///
/// The key is matched without regard to case, as every key of the document is
/// ([`mapping_get`]): the AppSpec reference spells the section `Hooks` for the
/// Lambda and Amazon ECS compute platforms and `hooks` for EC2/on-premises, so a
/// document is read for the section it HAS rather than for one spelling of it.
const HOOKS_KEY: &str = "Hooks";

/// The longest revision content this emulator parses an AppSpec out of.
///
/// `RawStringContent` carries no modelled length
/// (`smithy_query(query="shape",shape="RawStringContent")`), and the content is
/// caller-supplied, so the bound is EMULATOR-CHOSEN: it sits far above the few
/// hundred bytes an AppSpec file takes, and keeps a document nobody could have
/// written as an AppSpec from reaching the parser at all.
const MAX_APP_SPEC_BYTES: usize = 64 * 1024;

/// The longest revision content this emulator parses an AppSpec out of; see
/// [`MAX_APP_SPEC_BYTES`].
///
/// Public so a reader that FETCHES the document rather than being handed it
/// ([`super::s3_revision`]) can apply the same bound to the stored bytes BEFORE it
/// copies them, instead of stating a second bound of its own.
#[must_use]
pub fn max_app_spec_bytes() -> usize {
    MAX_APP_SPEC_BYTES
}

/// How deeply this emulator parses an AppSpec: the nesting a parser would walk,
/// bounded on the TEXT before it sees it — the flow collection openers (`[`, `{`),
/// the block sequence and complex-key indicators (`-`, `?`) standing on their own,
/// and the indentation a block mapping nests by.
///
/// Each level of nesting costs as little as a byte, so the byte bound above does
/// not bound the depth such a document reaches: this does, before either parser
/// sees the content. Also EMULATOR-CHOSEN, an order of magnitude above the handful
/// an AppSpec nests.
const MAX_APP_SPEC_NESTING: usize = 256;

/// The deepest AppSpec document this emulator reads, measured on the parser's own
/// event stream before the loader builds a tree of it.
///
/// EMULATOR-CHOSEN: an AppSpec's own nesting is the document, its `Resources`
/// list, one entry, that resource's body and its `Properties` — five levels — so
/// this sits well above any document that declares a traffic shift, and keeps the
/// loader (and the recursive drop of what it built) off a depth no AppSpec has.
const MAX_APP_SPEC_DEPTH: usize = 32;

/// The traffic shift one Lambda deployment performs, as its revision's AppSpec
/// declares it.
///
/// Read when the deployment is recorded and then held, so the shift a deployment
/// performs is fixed by the revision it was created with — editing nothing and
/// re-reading nothing later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaTrafficShift {
    /// The function whose alias moves: the AppSpec's `Name`.
    pub function_name: String,
    /// The alias that moves: the AppSpec's `Alias`.
    pub alias_name: String,
    /// The version the alias invoked before the deployment: the AppSpec's
    /// `CurrentVersion`, when it states one.
    pub current_version: Option<String>,
    /// The version the alias invokes once the shift is complete: the AppSpec's
    /// `TargetVersion`.
    pub target_version: String,
}

/// The traffic shift `revision` declares for a deployment of `compute_platform`,
/// or `None` when it declares none.
///
/// Only an Lambda deployment shifts traffic between function versions, so the
/// platform is what decides whether the revision's content is read as an Lambda
/// AppSpec at all ([`declared_in_revision`] is that read).
#[must_use]
pub fn declared_shift(
    compute_platform: &str,
    revision: &RevisionLocation,
) -> Option<LambdaTrafficShift> {
    if compute_platform != COMPUTE_PLATFORM_LAMBDA {
        return None;
    }
    declared_in_revision(revision)
}

/// The traffic shift `revision` declares, read out of the content it carries.
///
/// Only the revision forms that CARRY the AppSpec — `appSpecContent`, and the
/// `string` form the model describes as "the location of an Lambda deployment
/// revision stored as a RawString"
/// (`smithy_query(query="shape",shape="RevisionLocation")`) — carry it where this
/// emulator can read it. A revision that names an S3 object or a GitHub commit
/// keeps the AppSpec in that artifact, which this emulator does not fetch, so it
/// declares no shift here.
#[must_use]
pub fn declared_in_revision(revision: &RevisionLocation) -> Option<LambdaTrafficShift> {
    declared_in_app_spec(revision_content(revision)?)
}

/// The AppSpec document `revision` CARRIES, or `None` for a revision that keeps
/// it somewhere this emulator does not read.
///
/// Only the revision forms that carry the document — `appSpecContent`, and the
/// `string` form the model describes as "the location of an Lambda deployment
/// revision stored as a RawString"
/// (`smithy_query(query="shape",shape="RevisionLocation")`) — carry it where this
/// emulator can read it. A revision that names an S3 object or a GitHub commit
/// keeps the AppSpec in that artifact, which this emulator does not fetch.
#[must_use]
pub fn revision_content(revision: &RevisionLocation) -> Option<&str> {
    revision
        .app_spec_content
        .as_ref()
        .and_then(|content| content.content.as_deref())
        .or_else(|| {
            revision
                .string
                .as_ref()
                .and_then(|content| content.content.as_deref())
        })
}

/// The traffic shift an AppSpec document declares, or `None` for content that
/// declares none — including content that is no YAML (or JSON, which YAML 1.2 is a
/// superset of) document at all, and content outside the bounds this emulator
/// parses one within.
///
/// Public so a test can hold the reading of a document to this rule without
/// standing a deployment up for every row.
#[must_use]
pub fn declared_in_app_spec(content: &str) -> Option<LambdaTrafficShift> {
    declaration_in_app_spec(content, LAMBDA_FUNCTION_RESOURCE_TYPE, lambda_shift_of)
}

/// The declaration of `resource_type` an AppSpec document carries, as `properties`
/// reads it, or `None` for content that carries none — including content that is no
/// YAML (or JSON, which YAML 1.2 is a superset of) document at all, and content
/// outside the bounds this emulator parses one within.
///
/// The ONE traversal of an AppSpec, for every platform that declares something in
/// one: the bounds, the `Resources` section written as a sequence or as a mapping,
/// the entry that names its resource or IS the resource's body, and the `Type` that
/// must be the resource kind asked for. What a declaration of that kind MEANS is the
/// leaf reader's — [`lambda_shift_of`], [`ecs_task_set_of`] — so neither platform
/// carries a copy of the walk.
fn declaration_in_app_spec<T>(
    content: &str,
    resource_type: &str,
    properties: impl Fn(&Yaml) -> Option<T> + Copy,
) -> Option<T> {
    if !within_bounds(content) || !parses_within_depth(content) {
        return None;
    }
    ::yaml_rust2::YamlLoader::load_from_str(content)
        .ok()?
        .iter()
        .find_map(|document| declaration_in_document(document, resource_type, properties))
}

/// The declaration one document's `Resources` section carries: the first entry that
/// describes a resource of `resource_type`.
fn declaration_in_document<T>(
    document: &Yaml,
    resource_type: &str,
    properties: impl Fn(&Yaml) -> Option<T> + Copy,
) -> Option<T> {
    let resources = mapping_get(document, RESOURCES_KEY)?;
    match resources {
        Yaml::Array(declarations) => declarations
            .iter()
            .find_map(|declaration| declaration_in_entry(declaration, resource_type, properties)),
        mapping => declaration_in_entry(mapping, resource_type, properties),
    }
}

/// The declaration one `Resources` entry carries.
///
/// An AppSpec names the resource it deploys, so an entry is a mapping from that
/// logical name to the resource's own body; an entry that IS the body carries the
/// `Properties` mapping itself, and is read the same way.
fn declaration_in_entry<T>(
    entry: &Yaml,
    resource_type: &str,
    properties: impl Fn(&Yaml) -> Option<T> + Copy,
) -> Option<T> {
    let Yaml::Hash(named) = entry else {
        return None;
    };
    named
        .iter()
        .map(|(_, body)| body)
        .chain(::std::iter::once(entry))
        .find_map(|body| declaration_in_body(body, resource_type, properties))
}

/// The declaration one resource body carries: its `Properties`, when the body is a
/// resource of `resource_type` at all.
///
/// The resource's `Type` is REQUIRED to be that kind: a body that states another type
/// declares nothing about this platform's deployment, and a body that states no type
/// is no resource declaration this emulator reads one out of.
fn declaration_in_body<T>(
    body: &Yaml,
    resource_type: &str,
    properties: impl Fn(&Yaml) -> Option<T>,
) -> Option<T> {
    if mapping_get(body, TYPE_KEY)
        .and_then(scalar)
        .is_none_or(|declared| declared.trim() != resource_type)
    {
        return None;
    }
    properties(mapping_get(body, PROPERTIES_KEY)?)
}

/// One property of a declaration as the text it carries, or `None` where it states
/// none.
fn property(properties: &Yaml, name: &str) -> Option<String> {
    mapping_get(properties, name)
        .and_then(scalar)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// One lifecycle event hook a revision's AppSpec declares: the lifecycle event,
/// and the Lambda validation function that runs for it.
///
/// "A deployment lifecycle hook is specified in the hooks section of the AppSpec
/// file"
/// (`smithy_query(query="doc",shape="PutLifecycleEventHookExecutionStatusInput",member="lifecycleEventHookExecutionId")`),
/// and what it names for a Lambda or Amazon ECS deployment is the validation
/// function whose result that event's outcome IS — the function CodeDeploy invokes
/// with the event, and that reports back through
/// `PutLifecycleEventHookExecutionStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredHook {
    /// Which lifecycle event the hook runs for, as the document spells it.
    pub lifecycle_event_name: String,
    /// The validation function the document names for it.
    pub function_name: String,
}

/// The lifecycle event hooks `revision` declares, in document order.
///
/// Read when the deployment is recorded and then held, exactly as the traffic
/// shift is, so the hooks a deployment runs are fixed by the revision it was
/// created with.
#[must_use]
pub fn declared_hooks_in_revision(revision: &RevisionLocation) -> Vec<DeclaredHook> {
    revision_content(revision).map_or_else(Vec::new, declared_hooks_in_app_spec)
}

/// The lifecycle event hooks one AppSpec document declares, in document order;
/// empty for content that declares none, and for content outside the bounds this
/// emulator parses a document within ([`declared_in_app_spec`] states them).
///
/// Public so a test can hold the reading of a document to this rule without
/// standing a deployment up for every row.
#[must_use]
pub fn declared_hooks_in_app_spec(content: &str) -> Vec<DeclaredHook> {
    if !within_bounds(content) || !parses_within_depth(content) {
        return Vec::new();
    }
    ::yaml_rust2::YamlLoader::load_from_str(content)
        .unwrap_or_default()
        .iter()
        .flat_map(hooks_in_document)
        .collect()
}

/// The hooks one document's `Hooks` section declares.
///
/// The section is written as a sequence of single-entry mappings (the form AWS
/// documents) or as one mapping of event name to function, so both are read.
fn hooks_in_document(document: &Yaml) -> Vec<DeclaredHook> {
    let Some(hooks) = mapping_get(document, HOOKS_KEY) else {
        return Vec::new();
    };
    match hooks {
        Yaml::Array(entries) => entries.iter().flat_map(hooks_in_entry).collect(),
        mapping => hooks_in_entry(mapping),
    }
}

/// The hooks one entry of that section declares: each key is the lifecycle event
/// and each value the validation function it runs.
///
/// A value that is no scalar names no validation function — an EC2/on-premises
/// AppSpec's hooks are a sequence of script locations the CodeDeploy agent runs,
/// not a function this service invokes — so such an entry declares nothing here.
fn hooks_in_entry(entry: &Yaml) -> Vec<DeclaredHook> {
    let Yaml::Hash(declared) = entry else {
        return Vec::new();
    };
    declared
        .iter()
        .filter_map(|(event, function)| {
            let lifecycle_event_name = event.as_str()?.trim();
            let function_name = scalar(function)?;
            let function_name = function_name.trim();
            (!lifecycle_event_name.is_empty() && !function_name.is_empty()).then(|| DeclaredHook {
                lifecycle_event_name: lifecycle_event_name.to_string(),
                function_name: function_name.to_string(),
            })
        })
        .collect()
}

/// Whether the content is small enough and flat enough to hand to a parser at
/// all: the two bounds on work a caller chooses the size of
/// ([`MAX_APP_SPEC_BYTES`], [`MAX_APP_SPEC_NESTING`]), checked on the bytes before
/// any parser sees them.
///
/// The nesting is bounded by the shared scan every service that reads a
/// caller-supplied YAML document this way uses (`service_common::yaml_nesting`),
/// rather than by a scan of this crate's own.
fn within_bounds(content: &str) -> bool {
    content.len() <= MAX_APP_SPEC_BYTES
        && ::service_common::yaml_nesting::nesting_depth_bound(content, MAX_APP_SPEC_NESTING)
            <= MAX_APP_SPEC_NESTING
}

/// Whether the document parses, uses no YAML alias, and nests no deeper than
/// [`MAX_APP_SPEC_DEPTH`] — all three read off the parser's own event stream,
/// which copies nothing and builds no tree.
///
/// The depth is measured HERE rather than on the loaded document because the
/// loader resolves an alias by COPYING the anchored node and builds (and then
/// recursively drops) the whole tree: by the time there is a tree to measure, the
/// expansion and the recursion have already happened.
fn parses_within_depth(content: &str) -> bool {
    use ::yaml_rust2::parser::Event;

    #[derive(Default)]
    struct Guard {
        /// Whether an alias was reported, whose expansion copies a node.
        saw_alias: bool,
        /// How many collections are open at this event.
        depth: usize,
        /// The most that were open at once.
        deepest: usize,
    }
    impl ::yaml_rust2::parser::EventReceiver for Guard {
        fn on_event(&mut self, event: Event) {
            match event {
                Event::Alias(_) => self.saw_alias = true,
                Event::SequenceStart(..) | Event::MappingStart(..) => {
                    self.depth = self.depth.saturating_add(1);
                    self.deepest = self.deepest.max(self.depth);
                }
                Event::SequenceEnd | Event::MappingEnd => {
                    self.depth = self.depth.saturating_sub(1);
                }
                _ => {}
            }
        }
    }

    let mut guard = Guard::default();
    let parsed = ::yaml_rust2::parser::Parser::new_from_str(content).load(&mut guard, true);
    parsed.is_ok() && !guard.saw_alias && guard.deepest <= MAX_APP_SPEC_DEPTH
}

/// The traffic shift one Lambda function declaration's `Properties` describe: the
/// leaf reader of the walk above, for the `Lambda` compute platform.
fn lambda_shift_of(properties: &Yaml) -> Option<LambdaTrafficShift> {
    Some(LambdaTrafficShift {
        function_name: property(properties, NAME_PROPERTY)?,
        alias_name: property(properties, ALIAS_PROPERTY)?,
        current_version: property(properties, CURRENT_VERSION_PROPERTY),
        target_version: property(properties, TARGET_VERSION_PROPERTY)?,
    })
}

/// What one Amazon ECS deployment replaces its service's task set WITH, as its
/// revision's AppSpec declares it.
///
/// "For an Amazon ECS deployment, the content includes the task name, information
/// about the load balancer that serves traffic to the container, and more"
/// (`smithy_query(query="shape",shape="AppSpecContent")`), so the replacement task
/// set's own configuration is read out of the document — once, when the deployment
/// is recorded, exactly as a Lambda deployment's shift is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EcsTaskSetDeclaration {
    /// The revision the replacement task set runs: the `TaskDefinition` property,
    /// as the document gives it (an ARN, a `family:revision` or a family).
    pub task_definition: String,
    /// The container the load balancer serves traffic to, and the port on it:
    /// `LoadBalancerInfo`'s `ContainerName` and `ContainerPort`.
    pub container_name: Option<String>,
    /// See [`Self::container_name`].
    pub container_port: Option<i32>,
    /// The Fargate platform version the declaration asks for, when it states one.
    pub platform_version: Option<String>,
    /// The subnets, security groups and public-address setting the declaration's
    /// `NetworkConfiguration.AwsvpcConfiguration` names, when it states one; empty
    /// and `None` where it does not, which leaves the task set on the service's own
    /// networking.
    pub subnets: Vec<String>,
    /// See [`Self::subnets`].
    pub security_groups: Vec<String>,
    /// See [`Self::subnets`].
    pub assign_public_ip: Option<String>,
}

/// The replacement task set `revision` declares, read out of the content it
/// carries; the Amazon ECS counterpart of [`declared_in_revision`], and `None` for
/// a revision that keeps its AppSpec where this emulator does not read it.
///
/// Whether a deployment PERFORMS what it declares is the compute platform's
/// question, answered where the Amazon ECS deployment is resolved
/// ([`super::ecs_blue_green::declared`]), so the gate is spelled once.
#[must_use]
pub fn declared_ecs_task_set_in_revision(
    revision: &RevisionLocation,
) -> Option<EcsTaskSetDeclaration> {
    declared_ecs_task_set_in_app_spec(revision_content(revision)?)
}

/// The replacement task set an AppSpec document declares, under the same bounds
/// [`declared_in_app_spec`] reads a Lambda one within, and through the same
/// traversal ([`declaration_in_app_spec`]).
///
/// Public so a test can hold the reading of a document to this rule without
/// standing a deployment up for every row.
#[must_use]
pub fn declared_ecs_task_set_in_app_spec(content: &str) -> Option<EcsTaskSetDeclaration> {
    declaration_in_app_spec(content, ECS_SERVICE_RESOURCE_TYPE, ecs_task_set_of)
}

/// The replacement task set one Amazon ECS service declaration's `Properties`
/// describe: the leaf reader of that traversal, for the `ECS` compute platform.
///
/// The `TaskDefinition` is required, since a replacement task set with no revision
/// to run is no declaration at all.
fn ecs_task_set_of(properties: &Yaml) -> Option<EcsTaskSetDeclaration> {
    let load_balancer_info = mapping_get(properties, LOAD_BALANCER_INFO_PROPERTY);
    let awsvpc = mapping_get(properties, NETWORK_CONFIGURATION_PROPERTY)
        .and_then(|configuration| mapping_get(configuration, AWSVPC_CONFIGURATION_PROPERTY));
    Some(EcsTaskSetDeclaration {
        task_definition: property(properties, TASK_DEFINITION_PROPERTY)?,
        container_name: load_balancer_info
            .and_then(|info| property(info, CONTAINER_NAME_PROPERTY)),
        container_port: load_balancer_info
            .and_then(|info| property(info, CONTAINER_PORT_PROPERTY))
            .and_then(|port| port.parse().ok()),
        platform_version: property(properties, PLATFORM_VERSION_PROPERTY),
        subnets: awsvpc
            .map(|awsvpc| text_list(awsvpc, SUBNETS_PROPERTY))
            .unwrap_or_default(),
        security_groups: awsvpc
            .map(|awsvpc| text_list(awsvpc, SECURITY_GROUPS_PROPERTY))
            .unwrap_or_default(),
        assign_public_ip: awsvpc.and_then(|awsvpc| property(awsvpc, ASSIGN_PUBLIC_IP_PROPERTY)),
    })
}

/// One list-valued AppSpec property as the texts it carries, bounded by
/// [`MAX_DECLARED_LIST_ENTRIES`]; a property that is one scalar is the one-entry
/// list it describes.
fn text_list(node: &Yaml, key: &str) -> Vec<String> {
    let Some(value) = mapping_get(node, key) else {
        return Vec::new();
    };
    let entries: Vec<String> = match value {
        Yaml::Array(entries) => entries.iter().filter_map(scalar).collect(),
        single => scalar(single).into_iter().collect(),
    };
    entries
        .into_iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .take(MAX_DECLARED_LIST_ENTRIES)
        .collect()
}

/// One key of a YAML mapping; `None` for a node that is no mapping, and for a
/// mapping that does not carry the key.
///
/// The key is matched WITHOUT REGARD TO CASE, for every section and every
/// property of the document alike: CodeDeploy reads an AppSpec for the section
/// it HAS rather than for one spelling of it — the AppSpec reference itself
/// spells the hooks section `Hooks` for the Lambda and Amazon ECS compute
/// platforms and `hooks` for EC2/on-premises — so a document whose keys are
/// written in another case declares exactly what the same document in the
/// reference's case declares.
fn mapping_get<'a>(node: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    let Yaml::Hash(mapping) = node else {
        return None;
    };
    mapping
        .iter()
        .find(|(stored, _)| {
            stored
                .as_str()
                .is_some_and(|stored| stored.eq_ignore_ascii_case(key))
        })
        .map(|(_, value)| value)
}

/// One AppSpec scalar as the text the property carries.
///
/// A version number written unquoted is a YAML integer, and the member it fills —
/// Lambda's own `Version` (pattern `^(\$LATEST|[0-9]+)$`) — is text, so the
/// integer spelling is read as the digits it is.
fn scalar(node: &Yaml) -> Option<String> {
    match node {
        Yaml::String(text) | Yaml::Real(text) => Some(text.clone()),
        Yaml::Integer(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! What [`declared_in_app_spec`] reads out of a revision's content, over the
    //! forms an AppSpec is written in — cheap here, where a row costs a parse
    //! rather than a function, three versions, an alias and a deployment.

    use super::*;

    /// A shift with no `CurrentVersion`, for the rows that state none.
    fn shift(function_name: &str, alias_name: &str, target_version: &str) -> LambdaTrafficShift {
        LambdaTrafficShift {
            function_name: function_name.to_string(),
            alias_name: alias_name.to_string(),
            current_version: None,
            target_version: target_version.to_string(),
        }
    }

    /// RULE: the AppSpec's Lambda function declaration is what names the function,
    /// the alias and the versions of the shift — in either of the two formats the
    /// model says the content is written in, with the version numbers written as
    /// text or as numbers.
    #[test]
    fn the_app_spec_declares_the_shift() {
        let yaml = "version: 0.0\n\
                    Resources:\n\
                    \x20 - handler:\n\
                    \x20     Type: AWS::Lambda::Function\n\
                    \x20     Properties:\n\
                    \x20       Name: handler\n\
                    \x20       Alias: live\n\
                    \x20       CurrentVersion: \"1\"\n\
                    \x20       TargetVersion: \"2\"\n";
        let unquoted = "version: 0.0\n\
                        Resources:\n\
                        \x20 - handler:\n\
                        \x20     Type: AWS::Lambda::Function\n\
                        \x20     Properties:\n\
                        \x20       Name: handler\n\
                        \x20       Alias: live\n\
                        \x20       CurrentVersion: 1\n\
                        \x20       TargetVersion: 2\n";
        let json = r#"{"version": 0.0, "Resources": [{"handler": {
            "Type": "AWS::Lambda::Function",
            "Properties": {"Name": "handler", "Alias": "live",
                           "CurrentVersion": "1", "TargetVersion": "2"}}}]}"#;
        let mapping = "version: 0.0\n\
                       Resources:\n\
                       \x20 handler:\n\
                       \x20   Type: AWS::Lambda::Function\n\
                       \x20   Properties:\n\
                       \x20     Name: orders\n\
                       \x20     Alias: blue\n\
                       \x20     TargetVersion: 7\n";
        let ecs = "version: 0.0\n\
                   Resources:\n\
                   \x20 - target-service:\n\
                   \x20     Type: AWS::ECS::Service\n\
                   \x20     Properties:\n\
                   \x20       TaskDefinition: arn:aws:ecs:us-east-1:1234:task-definition/web:1\n";
        let aliased = "version: &anchor 0.0\n\
                       Resources: *anchor\n";
        // The SAME declaration with its keys spelled in another case, in both
        // forms the content is written in: CodeDeploy reads the document for the
        // sections and properties it HAS rather than for one spelling of them.
        let lower_case = r#"{"version": "0.0", "resources": [{"handler": {
            "type": "AWS::Lambda::Function",
            "properties": {"name": "handler", "alias": "live",
                           "currentversion": "1", "targetversion": "2"}}}]}"#;
        let upper_case = "VERSION: 0.0\n\
                          RESOURCES:\n\
                          \x20 - handler:\n\
                          \x20     TYPE: AWS::Lambda::Function\n\
                          \x20     PROPERTIES:\n\
                          \x20       NAME: handler\n\
                          \x20       ALIAS: live\n\
                          \x20       CURRENTVERSION: \"1\"\n\
                          \x20       TARGETVERSION: \"2\"\n";

        let declared = shift("handler", "live", "2");
        let with_current = LambdaTrafficShift {
            current_version: Some("1".to_string()),
            ..declared
        };
        let cases: &[(&str, &str, Option<LambdaTrafficShift>)] = &[
            ("a YAML AppSpec", yaml, Some(with_current.clone())),
            (
                "a YAML AppSpec whose versions are unquoted numbers",
                unquoted,
                Some(with_current.clone()),
            ),
            ("a JSON AppSpec", json, Some(with_current.clone())),
            (
                "a JSON AppSpec whose keys are in lower case",
                lower_case,
                Some(with_current.clone()),
            ),
            (
                "a YAML AppSpec whose keys are in upper case",
                upper_case,
                Some(with_current),
            ),
            (
                "a YAML AppSpec whose Resources is a mapping",
                mapping,
                Some(shift("orders", "blue", "7")),
            ),
            ("an AppSpec that declares no resource", "version: 0.0", None),
            ("an AppSpec of another compute platform", ecs, None),
            ("content that is no document at all", "\tnot: yaml\n", None),
            ("an AppSpec that uses a YAML alias", aliased, None),
            ("empty content", "", None),
        ];
        for (case, content, expected) in cases {
            let read = declared_in_app_spec(content);
            assert_eq!(
                read.as_ref(),
                expected.as_ref(),
                "case {case:?}: expected the declared shift {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: content outside the bounds this emulator parses an AppSpec within is
    /// not parsed at all, so it declares no shift — each bound held at its own
    /// value and one step past it.
    #[test]
    fn content_outside_the_parsing_bounds_declares_no_shift() {
        let document = "version: 0.0\n\
                        Resources:\n\
                        \x20 - handler:\n\
                        \x20     Type: AWS::Lambda::Function\n\
                        \x20     Properties:\n\
                        \x20       Name: handler\n\
                        \x20       Alias: live\n\
                        \x20       TargetVersion: 2\n";
        // The same AppSpec, padded with a comment line to exactly the byte bound
        // and to one byte past it: the comment is `# `, the padding, and the
        // newline that ends it, so three bytes beside the padding itself.
        let padded = |len: usize| {
            let mut content = String::from("# ");
            content.push_str(&"=".repeat(len - document.len() - 3));
            content.push('\n');
            content.push_str(document);
            content
        };
        let at_bytes = padded(MAX_APP_SPEC_BYTES);
        let past_bytes = padded(MAX_APP_SPEC_BYTES + 1);
        // A block sequence of `n` entries, which is `n` nesting indicators plus
        // the two columns each entry is indented at: the spelling the byte bound
        // alone would not hold, since each entry costs a handful of bytes.
        let entries = |count: usize| {
            let mut content = String::from("Resources:\n");
            for _ in 0..count {
                content.push_str("  - a\n");
            }
            content
        };
        let at_nesting = entries(MAX_APP_SPEC_NESTING - 2);
        let past_nesting = entries(MAX_APP_SPEC_NESTING - 1);
        // A block mapping nested by INDENTATION alone, which carries no nesting
        // indicator at all and is bounded by the columns it is written at.
        let indented = |depth: usize| {
            (0..depth)
                .map(|level| format!("{}k{level}:\n", " ".repeat(level)))
                .collect::<String>()
        };
        let at_indentation = indented(MAX_APP_SPEC_NESTING + 1);
        let past_indentation = indented(MAX_APP_SPEC_NESTING + 2);
        // A document nested to a chosen depth: the mapping it opens with, plus one
        // level per flow collection inside it.
        let nested = |depth: usize| {
            format!(
                "Resources: {}{}",
                "[".repeat(depth - 1),
                "]".repeat(depth - 1)
            )
        };
        let at_depth = nested(MAX_APP_SPEC_DEPTH);
        let past_depth = nested(MAX_APP_SPEC_DEPTH + 1);

        let declared = shift("handler", "live", "2");
        let cases: &[(&str, &str, bool, bool, Option<&LambdaTrafficShift>)] = &[
            (
                "an AppSpec padded to exactly the byte bound",
                &at_bytes,
                true,
                true,
                Some(&declared),
            ),
            (
                "an AppSpec padded one byte past the byte bound",
                &past_bytes,
                false,
                true,
                None,
            ),
            (
                "a document at the nesting bound",
                &at_nesting,
                true,
                true,
                None,
            ),
            (
                "a document one entry past the nesting bound",
                &past_nesting,
                false,
                true,
                None,
            ),
            (
                "a block mapping indented to exactly the nesting bound",
                &at_indentation,
                true,
                // Inside the TEXT bound and still far past the DEPTH bound the
                // parser's own event stream measures: the scan is generous where
                // the parser is exact, which is the safe direction for a bound
                // taken before the parse.
                false,
                None,
            ),
            (
                "a block mapping indented one column past the nesting bound",
                &past_indentation,
                false,
                false,
                None,
            ),
            (
                "a document nested to exactly the depth bound",
                &at_depth,
                true,
                true,
                None,
            ),
            (
                "a document nested one level past the depth bound",
                &past_depth,
                true,
                false,
                None,
            ),
        ];

        for (case, content, bounded, within_depth, expected) in cases {
            let within = within_bounds(content);
            assert_eq!(
                within, *bounded,
                "case {case:?}: expected the content to be within bounds: {bounded}, got {within}"
            );
            let depth = parses_within_depth(content);
            assert_eq!(
                depth, *within_depth,
                "case {case:?}: expected the content to be within the depth bound: \
                 {within_depth}, got {depth}"
            );
            let read = declared_in_app_spec(content);
            assert_eq!(
                read.as_ref(),
                *expected,
                "case {case:?}: expected the declared shift {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: only an Lambda deployment shifts traffic between function versions,
    /// and only the revision forms that carry the AppSpec carry the shift.
    #[test]
    fn only_a_lambda_revision_that_carries_the_app_spec_declares_a_shift() {
        let content = "version: 0.0\n\
                       Resources:\n\
                       \x20 - handler:\n\
                       \x20     Type: AWS::Lambda::Function\n\
                       \x20     Properties:\n\
                       \x20       Name: handler\n\
                       \x20       Alias: live\n\
                       \x20       TargetVersion: 2\n";
        let app_spec_content = RevisionLocation {
            revision_type: Some("AppSpecContent".to_string()),
            app_spec_content: Some(crate::ops::types::AppSpecContent {
                content: Some(content.to_string()),
                sha256: None,
            }),
            ..RevisionLocation::default()
        };
        let raw_string = RevisionLocation {
            revision_type: Some("String".to_string()),
            string: Some(crate::ops::types::RawString {
                content: Some(content.to_string()),
                sha256: None,
            }),
            ..RevisionLocation::default()
        };
        let s3 = RevisionLocation {
            revision_type: Some("S3".to_string()),
            s3_location: Some(crate::ops::types::S3Location {
                bucket: Some("artifacts".to_string()),
                key: Some("app.zip".to_string()),
                ..crate::ops::types::S3Location::default()
            }),
            ..RevisionLocation::default()
        };

        let declared = shift("handler", "live", "2");
        let cases: &[(&str, &str, &RevisionLocation, Option<LambdaTrafficShift>)] = &[
            (
                "an AppSpecContent revision of a Lambda deployment",
                COMPUTE_PLATFORM_LAMBDA,
                &app_spec_content,
                Some(declared.clone()),
            ),
            (
                "a String revision of a Lambda deployment",
                COMPUTE_PLATFORM_LAMBDA,
                &raw_string,
                Some(declared),
            ),
            (
                "an S3 revision of a Lambda deployment",
                COMPUTE_PLATFORM_LAMBDA,
                &s3,
                None,
            ),
            (
                "the same AppSpec on an EC2/on-premises deployment",
                "Server",
                &app_spec_content,
                None,
            ),
            (
                "the same AppSpec on an Amazon ECS deployment",
                "ECS",
                &app_spec_content,
                None,
            ),
        ];

        for (case, platform, revision, expected) in cases {
            let read = declared_shift(platform, revision);
            assert_eq!(
                read.as_ref(),
                expected.as_ref(),
                "case {case:?}: expected the declared shift {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the AppSpec's `Hooks` section is what names the validation function
    /// of each lifecycle event, in either format the content is written in and in
    /// either spelling of that section — and a hook that names no function (an
    /// EC2/on-premises AppSpec's script locations, say) declares none.
    #[test]
    fn the_app_spec_declares_the_lifecycle_event_hooks() {
        let both = "version: 0.0\n\
                    Hooks:\n\
                    \x20 - BeforeAllowTraffic: \"check-before\"\n\
                    \x20 - AfterAllowTraffic: \"check-after\"\n";
        let mapping = "version: 0.0\n\
                       Hooks:\n\
                       \x20 BeforeAllowTraffic: check-before\n";
        let json = r#"{"version": 0.0, "Hooks": [{"AfterAllowTraffic": "check-after"}]}"#;
        let ecs = "version: 0.0\n\
                   Hooks:\n\
                   \x20 - AfterAllowTestTraffic: \"check-test-traffic\"\n";
        let agent = "version: 0.0\n\
                     hooks:\n\
                     \x20 AfterInstall:\n\
                     \x20   - location: scripts/install.sh\n";
        let scripts = "version: 0.0\n\
                       Hooks:\n\
                       \x20 AfterInstall:\n\
                       \x20   - location: scripts/install.sh\n";
        let lower_case = "version: 0.0\n\
                          hooks:\n\
                          \x20 - BeforeAllowTraffic: check-before\n";
        let upper_case = "VERSION: 0.0\n\
                          HOOKS:\n\
                          \x20 - BEFOREALLOWTRAFFIC: check-before\n";

        let hook = |event: &str, function: &str| DeclaredHook {
            lifecycle_event_name: event.to_string(),
            function_name: function.to_string(),
        };
        let cases: &[(&str, &str, Vec<DeclaredHook>)] = &[
            (
                "both Lambda hooks",
                both,
                vec![
                    hook("BeforeAllowTraffic", "check-before"),
                    hook("AfterAllowTraffic", "check-after"),
                ],
            ),
            (
                "a Hooks section written as one mapping",
                mapping,
                vec![hook("BeforeAllowTraffic", "check-before")],
            ),
            (
                "a JSON AppSpec",
                json,
                vec![hook("AfterAllowTraffic", "check-after")],
            ),
            (
                "an Amazon ECS lifecycle event",
                ecs,
                vec![hook("AfterAllowTestTraffic", "check-test-traffic")],
            ),
            ("an AppSpec that declares no hook", "version: 0.0", vec![]),
            (
                // The AppSpec reference spells the section `hooks` for
                // EC2/on-premises, so the section is read whichever case it
                // carries; its ENTRIES here name script locations rather than a
                // validation function, so they declare none.
                "a lower-case hooks section naming script locations",
                agent,
                vec![],
            ),
            (
                "a lower-case hooks section naming a function",
                lower_case,
                vec![hook("BeforeAllowTraffic", "check-before")],
            ),
            (
                // The document's own spelling is what a declaration reports;
                // the hook EXECUTION it becomes takes the platform's spelling
                // of the event (`executions_of` in `super::lifecycle_hook`),
                // which is pinned by the end-to-end suite.
                "an upper-case hooks section naming a function",
                upper_case,
                vec![hook("BEFOREALLOWTRAFFIC", "check-before")],
            ),
            ("hooks that name script locations", scripts, vec![]),
            ("content that is no document at all", "\tnot: yaml\n", vec![]),
        ];

        for (case, content, expected) in cases {
            let read = declared_hooks_in_app_spec(content);
            assert_eq!(
                &read, expected,
                "case {case:?}: expected the declared hooks {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: "For an Amazon ECS deployment, the content includes the task name,
    /// information about the load balancer that serves traffic to the container, and
    /// more" — so an `AWS::ECS::Service` declaration is what names the revision the
    /// replacement task set runs, the container the load balancer serves and the
    /// networking it is placed with, in any of the forms an AppSpec is written in.
    #[test]
    fn the_app_spec_declares_the_replacement_task_set() {
        let declared = |task_definition: &str| EcsTaskSetDeclaration {
            task_definition: task_definition.to_string(),
            container_name: Some("web".to_string()),
            container_port: Some(80),
            ..EcsTaskSetDeclaration::default()
        };
        let yaml = "version: 0.0\n\
                    Resources:\n\
                    \x20 - TargetService:\n\
                    \x20     Type: AWS::ECS::Service\n\
                    \x20     Properties:\n\
                    \x20       TaskDefinition: \"web:2\"\n\
                    \x20       LoadBalancerInfo:\n\
                    \x20         ContainerName: web\n\
                    \x20         ContainerPort: 80\n";
        let json = r#"{"version": 0.0, "Resources": [{"TargetService": {
            "Type": "AWS::ECS::Service",
            "Properties": {"TaskDefinition": "web:2",
                           "LoadBalancerInfo": {"ContainerName": "web",
                                                "ContainerPort": "80"}}}}]}"#;
        let mapping = "version: 0.0\n\
                       Resources:\n\
                       \x20 TargetService:\n\
                       \x20   Type: AWS::ECS::Service\n\
                       \x20   Properties:\n\
                       \x20     TaskDefinition: arn:aws:ecs:us-east-1:1234:task-definition/web:7\n\
                       \x20     LoadBalancerInfo:\n\
                       \x20       ContainerName: web\n\
                       \x20       ContainerPort: 80\n";
        let placed = "version: 0.0\n\
                      Resources:\n\
                      \x20 - TargetService:\n\
                      \x20     Type: AWS::ECS::Service\n\
                      \x20     Properties:\n\
                      \x20       TaskDefinition: \"web:2\"\n\
                      \x20       PlatformVersion: 1.4.0\n\
                      \x20       NetworkConfiguration:\n\
                      \x20         AwsvpcConfiguration:\n\
                      \x20           Subnets: [\"subnet-a\", \"subnet-b\"]\n\
                      \x20           SecurityGroups: [\"sg-a\"]\n\
                      \x20           AssignPublicIp: ENABLED\n";
        let no_revision = "version: 0.0\n\
                           Resources:\n\
                           \x20 - TargetService:\n\
                           \x20     Type: AWS::ECS::Service\n\
                           \x20     Properties:\n\
                           \x20       LoadBalancerInfo:\n\
                           \x20         ContainerName: web\n";
        let lambda = "version: 0.0\n\
                      Resources:\n\
                      \x20 - handler:\n\
                      \x20     Type: AWS::Lambda::Function\n\
                      \x20     Properties:\n\
                      \x20       Name: handler\n\
                      \x20       Alias: live\n\
                      \x20       TargetVersion: \"2\"\n";
        let cases: &[(&str, &str, Option<EcsTaskSetDeclaration>)] = &[
            ("a YAML AppSpec", yaml, Some(declared("web:2"))),
            (
                "a JSON AppSpec whose container port is text",
                json,
                Some(declared("web:2")),
            ),
            (
                "a YAML AppSpec whose Resources is a mapping, naming the revision by ARN",
                mapping,
                Some(declared(
                    "arn:aws:ecs:us-east-1:1234:task-definition/web:7",
                )),
            ),
            (
                "a declaration that states the platform version and the networking",
                placed,
                Some(EcsTaskSetDeclaration {
                    task_definition: "web:2".to_string(),
                    platform_version: Some("1.4.0".to_string()),
                    subnets: vec!["subnet-a".to_string(), "subnet-b".to_string()],
                    security_groups: vec!["sg-a".to_string()],
                    assign_public_ip: Some("ENABLED".to_string()),
                    ..EcsTaskSetDeclaration::default()
                }),
            ),
            (
                "a declaration with no revision to run",
                no_revision,
                None,
            ),
            ("an AppSpec of another compute platform", lambda, None),
            ("an AppSpec that declares no resource", "version: 0.0", None),
            ("content that is no document at all", "\tnot: yaml\n", None),
            ("empty content", "", None),
        ];
        for (case, content, expected) in cases {
            let read = declared_ecs_task_set_in_app_spec(content);
            assert_eq!(
                read.as_ref(),
                expected.as_ref(),
                "case {case:?}: expected the declared task set {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: a list-valued property is read as the entries it carries, bounded by
    /// [`MAX_DECLARED_LIST_ENTRIES`] — so a document naming more subnets than one
    /// deployment reads has the first of them read and no more.
    #[test]
    fn a_declared_list_is_read_up_to_the_bound() {
        let subnets: Vec<String> = (0..MAX_DECLARED_LIST_ENTRIES + 4)
            .map(|index| format!("\"subnet-{index}\""))
            .collect();
        let content = format!(
            "version: 0.0\n\
             Resources:\n\
             \x20 - TargetService:\n\
             \x20     Type: AWS::ECS::Service\n\
             \x20     Properties:\n\
             \x20       TaskDefinition: \"web:2\"\n\
             \x20       NetworkConfiguration:\n\
             \x20         AwsvpcConfiguration:\n\
             \x20           Subnets: [{}]\n",
            subnets.join(", ")
        );
        let read = declared_ecs_task_set_in_app_spec(&content)
            .expect("the declaration names a revision to run");
        assert_eq!(
            read.subnets.len(),
            MAX_DECLARED_LIST_ENTRIES,
            "case \"more subnets than one deployment reads\": expected the bound \
             {MAX_DECLARED_LIST_ENTRIES}, got {:?}",
            read.subnets
        );
    }
}
