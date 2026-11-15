// SPDX-License-Identifier: CC0-1.0

//! The URLs a reported action carries: the deep link an action's state names for the
//! resource the action acts on.
//!
//! # The rule this module implements
//!
//! An action type publishes URL TEMPLATES rather than URLs
//! (`ActionTypeSettings$entityUrlTemplate` is "[t]he URL returned to the CodePipeline
//! console that provides a deep link to the resources of the external system"), and a
//! template refers to the action's own configuration by name: `CreateCustomActionType`
//! documents "[y]ou can refer to a name in the configuration properties of the custom
//! action within the URL templates by following the format of {Config:name}".
//! `ActionState$entityUrl` is the other end of that: "[a] URL link for more information
//! about the state of the action, such as a deployment group details page". So the URL a
//! state reports is the template of the action type the action NAMES, with every
//! `{Config:<property>}` reference replaced by the value that action's configuration
//! gives that property — a fact about the stored declaration, which is why it is reported
//! for a declared action whether or not an execution has run it yet.
//!
//! Nothing here opens a connection or resolves a name: the template is a response string
//! and the result is one too.

use super::*;

/// The reference an action type's URL template names one of the action's configuration
/// properties with, as `CreateCustomActionTypeInput$configurationProperties` documents
/// it (`{Config:<property>}`).
const CONFIG_REFERENCE_PREFIX: &str = "{Config:";

/// The most characters a reported URL carries: `ActionState$entityUrl` targets `Url`,
/// `@length(min: 1, max: 2048)` (`smithy_query query="shape" shape="ActionState"`).
const MAX_URL: usize = 2_048;

/// The settings the action type `id` names publishes to a caller in `region`, or `None`
/// when this account can reach no action type with that identity, or that action type
/// publishes none.
///
/// A registration of the identity wins over the catalog the service provides itself,
/// exactly as the listing's own view of both does
/// ([`ComAmazonawsCodepipelineCodepipeline20150709State::action_type_catalog`]), so one
/// action type cannot publish two different sets of URLs.
pub fn action_type_settings(
    store: &ComAmazonawsCodepipelineCodepipeline20150709State,
    id: &ActionTypeId,
    region: &str,
) -> Option<ActionTypeSettings> {
    match store.registered_action_type(&ActionTypeKey::from_id(id)) {
        Some(record) => record.settings.clone(),
        None => built_in_action_type(id)?.record(region).settings,
    }
}

/// The `ActionState$entityUrl` of the declared action `action`, or `None` when its action
/// type publishes no `entityUrlTemplate` or the template names a configuration property
/// the action does not configure.
///
/// `execution` is the execution whose run of the action is being reported, when one has
/// run it: its variables are what the action's configuration placeholders resolve to
/// ([`execution::resolved_configuration`]), so the link names the resource the action
/// really acted on rather than the unresolved `#{namespace.key}` text.
pub fn action_entity_url(
    store: &ComAmazonawsCodepipelineCodepipeline20150709State,
    action: &ActionDeclaration,
    execution: Option<&PipelineExecutionRecord>,
    region: &str,
) -> Option<String> {
    let template =
        action_type_settings(store, &action.action_type_id, region)?.entity_url_template?;
    let variables = execution
        .map(PipelineExecutionRecord::resolution_variables)
        .unwrap_or_default();
    let configuration = execution::resolved_configuration(action, &variables);
    resolve_url_template(&template, &configuration)
}

/// `template` with every `{Config:<property>}` reference replaced by `configuration`'s
/// value for that property.
///
/// `None` — the action reports no URL of this kind at all — when a reference names a
/// property the action does not configure (the link would carry template text where a
/// resource name belongs) or when the result would pass [`MAX_URL`], the length the
/// member carrying it is modelled for. A template with no reference in it resolves to
/// itself.
///
/// A replacement's own text is never re-scanned: a configuration value that itself looks
/// like a reference is substituted literally, so no chain of configuration values can
/// expand one template beyond the single pass this makes.
///
/// The walk is [`references::substitute_references`], the one scanner both reference forms
/// of this service are walked by, under the
/// [`Unresolved::NoResolution`](references::Unresolved::NoResolution) policy: a URL is
/// either the resource's or absent.
pub fn resolve_url_template(
    template: &str,
    configuration: &BTreeMap<String, String>,
) -> Option<String> {
    references::substitute_references(
        template,
        CONFIG_REFERENCE_PREFIX,
        MAX_URL,
        references::Unresolved::NoResolution,
        &references::value_in(configuration),
    )
}
