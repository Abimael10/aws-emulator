// SPDX-License-Identifier: CC0-1.0

//! The environments `ComposeEnvironments` composes, and the ENVIRONMENT
//! MANIFEST in a version's source bundle that describes each one.
//!
//! `ComposeEnvironments` "creates or updates a group of environments that each
//! run a separate component of a single application" and "takes a list of
//! version labels that specify application source bundles for each of the
//! environments to create or update", where "the name of each environment and
//! other required information must be included in the source bundles in an
//! environment manifest named `env.yaml`". So WHAT to compose is read out of
//! the bundle each version label resolves to — the manifest's own
//! `EnvironmentName`, `SolutionStack`, `EnvironmentTier`, `OptionSettings` and
//! `EnvironmentLinks` — and nothing about a composed environment is invented
//! from the label, the group or the application name.
//!
//! The bundle is the one the version already carries
//! ([`super::deployed_application`], read from object storage when the version
//! was created), so the manifest this reads is the file the caller really
//! uploaded. A version whose bundle carries no manifest describes no
//! environment, and the call is refused rather than composing one whose name
//! was made up.
//!
//! `GroupName` is "the name of the group to which the target environments
//! belong. Specify a group name only if the environment name defined in each
//! target environment's manifest ends with a + (plus) character": such a name is
//! a TEMPLATE for a per-group environment name, which is what
//! [`composed_environment_name`] completes.
//!
//! Split out of [`super`], which holds the store the environments live in; the
//! record a composed launch produces is [`super::environment_launch`]'s, shared
//! with `CreateEnvironment`.

use super::*;

/// The operation, as the service names it in the events one composition
/// records: the call that performed the launch or the update, which is what a
/// client following this request filters the log on.
const COMPOSE_OPERATION_NAME: &str = "composeEnvironments";

/// The member of the manifest that names the environment it describes.
const MANIFEST_ENVIRONMENT_NAME: &str = "EnvironmentName";

/// The member that names the solution stack to launch it on ("Each source
/// bundle must include an environment manifest that specifies the name of the
/// environment and the name of the solution stack to use").
const MANIFEST_SOLUTION_STACK: &str = "SolutionStack";

/// The member that carries its tier, and the three members of that tier —
/// `EnvironmentTier`'s own modelled members.
const MANIFEST_ENVIRONMENT_TIER: &str = "EnvironmentTier";
const MANIFEST_TIER_NAME: &str = "Name";
const MANIFEST_TIER_TYPE: &str = "Type";
const MANIFEST_TIER_VERSION: &str = "Version";

/// The member that carries its configuration: a mapping of option NAMESPACE to
/// the options set in that namespace, which is how Elastic Beanstalk's
/// environment-manifest reference lays `OptionSettings` out.
const MANIFEST_OPTION_SETTINGS: &str = "OptionSettings";

/// The member that carries its links to the other environments of the group
/// ("optionally can specify environment links to create"): a mapping of link
/// name to the manifest name of the environment linked to.
const MANIFEST_ENVIRONMENT_LINKS: &str = "EnvironmentLinks";

/// The character a manifest's environment name ends with when the name is
/// completed by the group name: "specify a group name only if the environment
/// name defined in each target environment's manifest ends with a + (plus)
/// character".
const GROUP_NAME_PLACEHOLDER: char = '+';

/// How many links one manifest may declare.
///
/// A bound on CALLER content — the manifest is a file out of an uploaded source
/// bundle — kept well above the environments one composition can name, so a
/// manifest cannot make an environment record grow without end.
const MAX_MANIFEST_ENVIRONMENT_LINKS: usize = 32;

/// What one environment manifest describes about the environment it names.
///
/// Every member is OPTIONAL here: a manifest is caller content, and what it
/// leaves out is what the composition fills in from the model's own defaults
/// (the tier) or reports absent (the solution stack). The one member a
/// composition cannot do without is the environment's NAME, which is why a
/// manifest that names none composes nothing.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentManifest {
    /// `EnvironmentName`, as written in the manifest — still carrying its
    /// trailing `+` when the group name completes it.
    pub environment_name: Option<String>,
    pub solution_stack: Option<String>,
    pub tier: Option<EnvironmentTier>,
    pub option_settings: Vec<ConfigurationOptionSetting>,
    /// `EnvironmentLinks`, as `(link name, linked environment's manifest name)`
    /// pairs in the order the manifest lists them.
    pub links: Vec<(String, String)>,
}

/// The manifest a source bundle carries, or `None` when it carries none this
/// emulator can read — no `env.yaml` at the bundle's root, a file that is not
/// text, or text that is not a YAML mapping.
///
/// The file name and the parser are the ones the pre-processing of the same
/// bundle already uses ([`super::version_processing`]), so the manifest a
/// version is validated against and the manifest an environment is composed
/// from are the same file read the same way.
pub fn environment_manifest(bundle: &ApplicationBundle) -> Option<EnvironmentManifest> {
    let source = bundle.files.get(ENVIRONMENT_MANIFEST)?;
    let text = ::std::str::from_utf8(source).ok()?;
    // The same bounded read the pre-processing of this bundle performs, so the
    // nesting of a caller's manifest is counted before any parser walks it.
    let BoundedYaml::Documents(documents) = read_bounded_yaml(text) else {
        return None;
    };
    let manifest = documents.first()?.as_hash()?;
    Some(EnvironmentManifest {
        environment_name: manifest_get(manifest, MANIFEST_ENVIRONMENT_NAME).and_then(scalar_text),
        solution_stack: manifest_get(manifest, MANIFEST_SOLUTION_STACK).and_then(scalar_text),
        tier: manifest_tier(manifest),
        option_settings: manifest_option_settings(manifest),
        links: manifest_links(manifest),
    })
}

/// The name a composed environment carries, for the `manifest_name` its own
/// manifest declares and the `group_name` the request named.
///
/// A manifest name ending in the group-name placeholder is completed by the
/// group: the placeholder is dropped and the group name is appended to it, so
/// one application composed into two groups has two environments per manifest.
/// A name that does not end in it is the environment's name as written, and a
/// request that named no group has no group to complete one with.
pub fn composed_environment_name(manifest_name: &str, group_name: Option<&str>) -> String {
    match manifest_name.strip_suffix(GROUP_NAME_PLACEHOLDER) {
        Some(stem) => match group_name {
            Some(group) => format!("{stem}-{group}"),
            None => stem.to_string(),
        },
        None => manifest_name.to_string(),
    }
}

/// One environment a composition is about to compose: the manifest that
/// describes it, the version label whose bundle carried that manifest, and the
/// name the group completed.
#[derive(Debug)]
pub struct ComposedEnvironment<'a> {
    pub region: &'a str,
    /// The `host:port` every environment's published address is served on.
    pub plane_authority: &'a str,
    pub application_name: &'a str,
    /// The version label this environment runs — "a list of version labels,
    /// specifying one or more application source bundles that belong to the
    /// target application".
    pub version_label: &'a str,
    /// The environment's name, as [`composed_environment_name`] completed it.
    pub environment_name: &'a str,
    pub manifest: &'a EnvironmentManifest,
    /// The links this environment reports, with every target already resolved
    /// to the name the group completed for it.
    pub environment_links: Vec<EnvironmentLink>,
    /// Why this launch cannot be provisioned, where it cannot: the manifest
    /// associates no IAM instance profile with the environment's instances, or
    /// IAM will not let this call's caller pass the one it does — the diagnosis
    /// the launch then fails with, see
    /// [`super::environment_launch_failure`]. `None` where the launch can be
    /// provisioned.
    pub launch_refusal: Option<String>,
    pub request_id: &'a str,
}

impl ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState {
    /// Compose ONE environment: launch it when the group has no environment of
    /// that name, and otherwise deploy this version to the one it already has —
    /// "create OR UPDATE a group of environments".
    ///
    /// Answers the stored record either way, which is what the response
    /// describes: "returns an EnvironmentDescription list".
    pub fn compose_environment(&mut self, composed: ComposedEnvironment<'_>) -> Environment {
        let ComposedEnvironment {
            region,
            plane_authority,
            application_name,
            version_label,
            environment_name,
            manifest,
            environment_links,
            launch_refusal,
            request_id,
        } = composed;
        if let Some(updated) = self.recompose_environment(
            environment_name,
            version_label,
            manifest,
            &environment_links,
            request_id,
        ) {
            return updated;
        }
        let environment_id = self.next_environment_id();
        let sequence = self.next_counter();
        // A platform version has two names and an environment reports both, so
        // the ARN of the version the manifest's solution stack names is
        // resolved from the service's published catalog — exactly as a launch
        // from the API resolves it. A stack this catalog does not publish
        // resolves to no ARN, and the environment reports the stack the
        // manifest named and no ARN rather than an ARN nothing answers to.
        let platform_arn = named_platform_version(region, manifest.solution_stack.as_deref(), None)
            .map(|version| version.platform_arn(region));
        let environment = Environment::launching(EnvironmentLaunch {
            environment_id,
            environment_name: environment_name.to_string(),
            sequence,
            region,
            plane_authority,
            // A manifest declares no CNAME prefix, so a composed environment
            // gets the generated one every launch that names none gets.
            cname_prefix: None,
            application_name,
            version_label: Some(version_label.to_string()),
            solution_stack_name: manifest.solution_stack.clone(),
            platform_arn,
            // "The name of the configuration template used to originally
            // launch this environment": a composition launches from a manifest,
            // not from a saved configuration.
            template_name: None,
            description: None,
            tier: Some(environment_tier_with_defaults(manifest.tier.as_ref())),
            option_settings: merge_option_settings(&[], &manifest.option_settings),
            operations_role: None,
            tags: Vec::new(),
            environment_links,
            request_id,
        });
        let launched = environment.clone();
        self.store_launched_environment(environment, request_id, COMPOSE_OPERATION_NAME);
        // The instances this launch would run are configured by the manifest
        // alone, and are given the IAM instance profile it associates with them
        // — which only a caller allowed to pass that role may hand over. A
        // launch with no profile to hand them, or none this caller may pass,
        // cannot be provisioned: it carries the refusal as an event and ends in
        // its terminal status rather than reaching `Ready`.
        if let Some(diagnosis) = launch_refusal {
            self.fail_environment_launch(&launched.environment_id, diagnosis, request_id);
        }
        launched
    }

    /// The UPDATE half of [`Self::compose_environment`]: deploy `version_label`
    /// to the live environment the group already has under `environment_name`,
    /// and `None` when it has none.
    ///
    /// An environment the clock has terminated ("Environment is not running")
    /// is not one a composition updates: its name is free again, so the
    /// composition launches a new environment under it.
    fn recompose_environment(
        &mut self,
        environment_name: &str,
        version_label: &str,
        manifest: &EnvironmentManifest,
        environment_links: &[EnvironmentLink],
        request_id: &str,
    ) -> Option<Environment> {
        let environment_id = self.environment_name_index.get(environment_name)?.clone();
        let environment = self
            .environments
            .get_mut(&environment_id)
            .filter(|environment| !environment.is_terminated())?;
        // The composition deploys the version whose bundle carried the manifest
        // — "the application version deployed in this environment" — and the
        // links the manifest now declares.
        environment.version_label = Some(version_label.to_string());
        environment.environment_links = environment_links.to_vec();
        // The manifest's configuration is deployed the way `UpdateEnvironment`
        // deploys one: as the environment's DRAFT until the clock completes the
        // update, and as no draft at all when it is the configuration the
        // environment already runs.
        let settings = merge_option_settings(&[], &manifest.option_settings);
        let draft = (!same_option_settings(&environment.option_settings, &settings)).then(|| {
            DraftConfiguration::pending(
                settings,
                environment.solution_stack_name.clone(),
                environment.platform_arn.clone(),
            )
        });
        environment.begin_update(request_id, draft);
        let updated = environment.clone();
        let subject = updated.event_subject();
        // The update has STARTED here; the event for its completion is recorded
        // when the clock makes that completion due.
        self.record_activity_start(request_id, COMPOSE_OPERATION_NAME, subject);
        Some(updated)
    }
}

/// The value of `key` in a manifest mapping.
fn manifest_get<'a>(
    manifest: &'a ::yaml_rust2::yaml::Hash,
    key: &str,
) -> Option<&'a ::yaml_rust2::Yaml> {
    manifest.get(&::yaml_rust2::Yaml::String(key.to_string()))
}

/// One manifest scalar as the text the API carries: a quoted string as written,
/// and a number or a boolean as YAML spells it, since every member these fill
/// in is a string on the wire.
fn scalar_text(value: &::yaml_rust2::Yaml) -> Option<String> {
    match value {
        ::yaml_rust2::Yaml::String(text) | ::yaml_rust2::Yaml::Real(text) => Some(text.clone()),
        ::yaml_rust2::Yaml::Integer(number) => Some(number.to_string()),
        ::yaml_rust2::Yaml::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// The tier the manifest declares, by the three members `EnvironmentTier` has.
/// The defaults for the members it leaves out are the model's, applied where
/// every other launch applies them ([`environment_tier_with_defaults`]).
fn manifest_tier(manifest: &::yaml_rust2::yaml::Hash) -> Option<EnvironmentTier> {
    let tier = manifest_get(manifest, MANIFEST_ENVIRONMENT_TIER)?.as_hash()?;
    Some(EnvironmentTier {
        name: manifest_get(tier, MANIFEST_TIER_NAME).and_then(scalar_text),
        r#type: manifest_get(tier, MANIFEST_TIER_TYPE).and_then(scalar_text),
        version: manifest_get(tier, MANIFEST_TIER_VERSION).and_then(scalar_text),
    })
}

/// The option settings the manifest declares: every option of every namespace
/// it lists, as the configuration the composed environment runs.
///
/// Bounded by [`MAX_CONFIGURATION_OPTION_SETTINGS`], the same bound a request
/// that states its own settings is held to, since this list ends up on an
/// environment record exactly as that one does.
fn manifest_option_settings(
    manifest: &::yaml_rust2::yaml::Hash,
) -> Vec<ConfigurationOptionSetting> {
    let Some(namespaces) = manifest_get(manifest, MANIFEST_OPTION_SETTINGS).and_then(
        ::yaml_rust2::Yaml::as_hash,
    ) else {
        return Vec::new();
    };
    let mut settings = Vec::new();
    for (namespace, options) in namespaces {
        let (Some(namespace), Some(options)) = (scalar_text(namespace), options.as_hash()) else {
            continue;
        };
        for (option_name, value) in options {
            if settings.len() >= MAX_CONFIGURATION_OPTION_SETTINGS {
                return settings;
            }
            let (Some(option_name), Some(value)) = (scalar_text(option_name), scalar_text(value))
            else {
                continue;
            };
            settings.push(ConfigurationOptionSetting {
                resource_name: None,
                namespace: Some(namespace.clone()),
                option_name: Some(option_name),
                value: Some(value),
            });
        }
    }
    settings
}

/// The links the manifest declares, as `(link name, linked environment's
/// manifest name)` pairs — bounded by [`MAX_MANIFEST_ENVIRONMENT_LINKS`].
fn manifest_links(manifest: &::yaml_rust2::yaml::Hash) -> Vec<(String, String)> {
    let Some(links) =
        manifest_get(manifest, MANIFEST_ENVIRONMENT_LINKS).and_then(::yaml_rust2::Yaml::as_hash)
    else {
        return Vec::new();
    };
    links
        .iter()
        .filter_map(|(link_name, target)| Some((scalar_text(link_name)?, scalar_text(target)?)))
        .take(MAX_MANIFEST_ENVIRONMENT_LINKS)
        .collect()
}
