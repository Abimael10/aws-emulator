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
//! # Where that URL is ANNOUNCED
//!
//! A template names the console of the system the action acts on, and this emulator stands
//! in for that system: it neither serves that console nor is reached on its host. A link it
//! reports for a caller to FOLLOW is therefore announced on the address the request reached
//! this emulator at ([`super::announced_authority`], the one address a client of this
//! emulator resolves), and what the template contributes to it is the part that names the
//! RESOURCE — the path, query and fragment it resolves to ([`announced_link`]). A link on
//! any other host takes the caller out of the emulated environment: nothing answers there,
//! or what answers is the real service, where the resource does not exist.
//!
//! The catalog's own `ActionTypeSettings` are NOT read that way: a template is the text of a
//! modelled member this service PUBLISHES about another system
//! ([`super::built_in_catalog`]), not a URL handed to a caller, so it is reported as that
//! system publishes it and only the link resolved FROM it is re-announced.
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

/// The scheme a link this emulator announces carries: every listener of this process
/// presents the core's certificate, so an address it publishes is an HTTPS one.
const ANNOUNCED_SCHEME: &str = "https://";

/// What separates a URL's scheme from the authority that follows it.
const SCHEME_SEPARATOR: &str = "://";

/// What a reference naming an AUTHORITY but no scheme begins with (RFC 3986's
/// network-path reference, `//host/path`): the other form a template can name a host in.
const AUTHORITY_PREFIX: &str = "//";

/// Where the links a reported action carries are read FOR.
///
/// The two readings a link needs, carried together so a caller cannot pass one for the
/// other: WHICH console page the action type's template names, and WHERE the link that
/// comes out of it is announced.
#[derive(Debug, Clone, Copy)]
pub struct ReportedLinkSite<'a> {
    /// The Region the state is being read for — the Region whose console an action type's
    /// template names (`ListActionTypes$regionFilter`, or the Region the request was
    /// signed for).
    pub region: &'a str,
    /// The `host[:port]` this emulator was reached at for the request being answered
    /// ([`super::announced_authority`]).
    pub authority: &'a str,
}

/// `url` announced on `authority`, the address this emulator is reached at.
///
/// The template's own scheme and host name the external system's console (see the module
/// note), so what survives is the part that names the RESOURCE — everything from the first
/// `/`, `?` or `#` on. WHICH part of a template is the host is RFC 3986's reading of the
/// reference it wrote, so a template a caller registered is not held to the catalog's
/// habits: a reference carrying a scheme (`https://host/path`) and one carrying an
/// authority alone (`//host/path`) both name a host, which is dropped; a reference with
/// neither names a PATH, not a host, and is published as the resource it already is — which
/// is also what a reference whose own path or query CARRIES `://` is
/// ([`names_a_scheme`]), so no text a caller wrote inside a path becomes the host this
/// link's authority replaces.
///
/// `None` when the result would pass [`MAX_URL`], the length the member carrying it is
/// modelled for: a link is either the resource's or absent.
#[must_use]
pub fn announced_link(url: &str, authority: &str) -> Option<String> {
    let resource = match url
        .split_once(SCHEME_SEPARATOR)
        .filter(|(scheme, _)| names_a_scheme(scheme))
    {
        Some((_, after_scheme)) => resource_of(after_scheme),
        None => match url.strip_prefix(AUTHORITY_PREFIX) {
            Some(after_prefix) => resource_of(after_prefix),
            None => url,
        },
    };
    let separator = if resource.starts_with(['/', '?', '#']) {
        ""
    } else {
        "/"
    };
    let link = format!("{ANNOUNCED_SCHEME}{authority}{separator}{resource}");
    (link.len() <= MAX_URL).then_some(link)
}

/// Whether the text before a `://` is a SCHEME — RFC 3986's
/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` — so a reference is read as naming a host
/// only when it begins with one.
///
/// The same rule `service_common::data_plane_addressing` applies to an absolute-form
/// request target, restated here because the function that holds it there is private to
/// that module: a reference whose path or query carries a `://` of its own (a callback
/// parameter is the ordinary case) names no authority at all.
fn names_a_scheme(scheme: &str) -> bool {
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|next| next.is_ascii_alphanumeric() || matches!(next, '+' | '-' | '.'))
}

/// The resource part of `authority_and_resource` — everything from the first `/`, `?` or
/// `#` on — for a reference whose leading text is an AUTHORITY. `"/"` for a reference that
/// names only a host, which names that host's root.
fn resource_of(authority_and_resource: &str) -> &str {
    authority_and_resource
        .find(['/', '?', '#'])
        .map_or("/", |start| &authority_and_resource[start..])
}

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
///
/// The resolved link is announced on the address this emulator is reached at
/// ([`announced_link`]), which is where a caller can follow it.
pub fn action_entity_url(
    store: &ComAmazonawsCodepipelineCodepipeline20150709State,
    action: &ActionDeclaration,
    execution: Option<&PipelineExecutionRecord>,
    site: ReportedLinkSite<'_>,
) -> Option<String> {
    let template =
        action_type_settings(store, &action.action_type_id, site.region)?.entity_url_template?;
    let variables = execution
        .map(PipelineExecutionRecord::resolution_variables)
        .unwrap_or_default();
    let configuration = execution::resolved_configuration(action, &variables);
    let resolved = resolve_url_template(&template, &configuration)?;
    announced_link(&resolved, site.authority)
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
