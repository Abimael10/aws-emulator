// SPDX-License-Identifier: CC0-1.0

//! The `AppConfig-GetConfiguration` action: the configuration a route reads for a client.
//!
//! The parameters are the members of that operation's own input
//! (`smithy_query(query="operation", operation="GetConfiguration")` on the AppConfig model),
//! which is what the action takes: the application, the environment and the configuration
//! profile the triple addresses, the client asking for it, and the version that client says
//! it already holds ([`GET_CONFIGURATION_PARAMETERS`]). The first four are the arguments the
//! action cannot be called without — the same four the control plane holds an integration of
//! this subtype to — and the fifth is optional, exactly as that input marks them.
//!
//! The triple is a parameter of the in-process call and NOT a destination this process
//! connects to: the client is the emulator's own transport, addressed at the AppConfig
//! endpoint of the Region the parent module resolved for this call, so the configuration read
//! is the one this emulator holds.
//!
//! What the read then answers is AppConfig's own: which deployed version is in force for that
//! triple, where its bytes live, and how far a gradual rollout has reached the client the
//! `ClientId` names. Nothing of that is decided here; this module maps the input and makes the
//! call.
//!
//! # The form an answer is reported in
//!
//! The action is performed over AppConfig's own api, a REST interface that binds this
//! operation's result to the response's own PAYLOAD: `Content` is the `@httpPayload` blob,
//! `ContentType` is bound onto `Content-Type` and `ConfigurationVersion` onto
//! `Configuration-Version`. So that is the form the caller is answered in
//! ([`Performed::payload`]): the configuration's bytes as the WHOLE body, under the media type
//! AppConfig stated describes them, with the version that content belongs to in the header
//! that api states it in. Nothing re-encodes or wraps those bytes — a client fronting
//! AppConfig with an HTTP api reads the document it would have read calling the action itself,
//! which is the whole point of putting an api in front of a configuration a browser or a
//! device has no credential for.

use smithy_server_runtime::OperationContext;

use super::credentials::SigningIdentity;
use super::parameters::Mapped;
use super::{Performed, Unperformed};

/// The `IntegrationSubtype` that names this action, taken from the catalog that states which
/// subtypes are first-class (`crate::state::integration_subtype`), so the value the control
/// plane admits and the one this data plane performs are one constant.
pub const GET_CONFIGURATION_SUBTYPE: &str =
    crate::state::integration_subtype::APP_CONFIG_GET_CONFIGURATION.name;

/// Every parameter an integration may map onto the read: the members of
/// `GetConfigurationRequest`, all of which this emulator carries into the call.
pub const GET_CONFIGURATION_PARAMETERS: &[&str] = &[
    "Application",
    "ClientConfigurationVersion",
    "ClientId",
    "Configuration",
    "Environment",
];

/// The parameters the read must carry a value for: the arguments the control plane already
/// holds an integration of this subtype to (`crate::state::integration_subtype`), which for
/// this action is exactly the set its own input marks required.
pub const GET_CONFIGURATION_REQUIRED_PARAMETERS: &[&str] =
    crate::state::integration_subtype::APP_CONFIG_GET_CONFIGURATION.required_parameters;

/// The `arnNamespace` AppConfig publishes its endpoint and signs under, which is the endpoint
/// label the in-process client is addressed at.
const APPCONFIG_ENDPOINT: &str = "appconfig";

/// The members of the mapping this action reads, spelled as an integration's
/// `RequestParameters` spell them.
const APPLICATION: &str = "Application";
const ENVIRONMENT: &str = "Environment";
const CONFIGURATION: &str = "Configuration";
const CLIENT_ID: &str = "ClientId";
const CLIENT_CONFIGURATION_VERSION: &str = "ClientConfigurationVersion";

/// The header AppConfig's own api binds `Configuration$ConfigurationVersion` onto, which is
/// where a client of this route reads the version the content it just received belongs to.
const CONFIGURATION_VERSION_HEADER: &str = "Configuration-Version";

/// The value mapped for the action's parameter `parameter`, or the reason there is no call to
/// make.
///
/// An argument the action cannot be called without that the integration's mapping defines
/// nothing for names no read at all, which is the "integration produced no response" class
/// ([`Unperformed::NotPerformed`]) rather than an answer nothing produced. Which parameters
/// those are is the catalogue's ([`GET_CONFIGURATION_REQUIRED_PARAMETERS`]), and the test
/// below holds this module to it, so the set the control plane admits an integration against
/// and the set this call is built from cannot drift apart.
fn argument<'m>(mapped: &'m Mapped, parameter: &str) -> Result<&'m str, Unperformed> {
    mapped.get(parameter).ok_or_else(|| {
        Unperformed::NotPerformed(format!(
            "the integration maps no {parameter}, which {GET_CONFIGURATION_SUBTYPE} requires"
        ))
    })
}

/// Read the configuration the mapping addresses, and answer it as AppConfig's own api states
/// one (see the module docs).
/// AppConfig itself marks this operation deprecated — "(Deprecated) Retrieves the latest
/// deployed configuration… Calls to receive configuration data should use the
/// StartConfigurationSession and GetLatestConfiguration APIs instead"
/// (`smithy_query(query="operation", operation="GetConfiguration")` on the AppConfig model) —
/// and it is the action this subtype NAMES, so performing the integration means calling
/// exactly it. The deprecation is a fact about the api being emulated rather than a choice
/// here, which is why the lint is allowed for this one call.
#[allow(deprecated)]
pub async fn get_configuration(
    ctx: &OperationContext<'_>,
    region: &str,
    identity: Option<&SigningIdentity>,
    mapped: &Mapped,
) -> Result<Performed, Unperformed> {
    // The four arguments the read is made with, each under the name the action spells.
    let application = argument(mapped, APPLICATION)?;
    let environment = argument(mapped, ENVIRONMENT)?;
    let configuration = argument(mapped, CONFIGURATION)?;
    let client_id = argument(mapped, CLIENT_ID)?;
    let client = action_client!(
        aws_sdk_appconfig,
        ctx.callback(),
        APPCONFIG_ENDPOINT,
        region,
        identity
    );
    let mut call = client
        .get_configuration()
        .application(application)
        .environment(environment)
        .configuration(configuration)
        .client_id(client_id);
    // "The configuration version returned in the most recent GetConfiguration response"
    // (`GetConfigurationRequest$ClientConfigurationVersion`): optional, so it travels only
    // when the integration maps a value this request resolved.
    if let Some(client_configuration_version) = mapped.get(CLIENT_CONFIGURATION_VERSION) {
        call = call.client_configuration_version(client_configuration_version);
    }
    let read = call
        .send()
        .await
        .map_err(|error| super::unperformed(GET_CONFIGURATION_SUBTYPE, &error))?;
    // The payload is the configuration's own bytes, as AppConfig answered them: a version
    // whose content that service reports nothing for is an empty payload rather than a body
    // of this gateway's own making, and a media type it stated none for is reported as none
    // rather than as a type this gateway guessed.
    let content = read
        .content()
        .map(|blob| blob.as_ref().to_vec())
        .unwrap_or_default();
    let content_type = read.content_type().map(str::to_string);
    let mut headers = Vec::new();
    if let Some(configuration_version) = read.configuration_version() {
        headers.push((
            CONFIGURATION_VERSION_HEADER,
            configuration_version.to_string(),
        ));
    }
    Ok(Performed::payload(content, content_type, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the arguments this module builds the call from are the ones the catalogue holds
    /// an integration of this subtype to, so the set the control plane admits a configuration
    /// against and the set this data plane calls the action with are one set.
    ///
    /// Two independent declarations compared, not a literal re-asserted: the catalogue states
    /// the required set for this action, and the four lookups in [`get_configuration`] state
    /// what the call cannot be made without. A change to either alone fails here rather than
    /// leaving the two planes to disagree at invoke time.
    #[test]
    fn the_call_is_built_from_the_arguments_the_catalogue_requires() {
        let built_from = [APPLICATION, ENVIRONMENT, CONFIGURATION, CLIENT_ID];
        assert_eq!(
            GET_CONFIGURATION_REQUIRED_PARAMETERS, built_from,
            "expected the catalogue's required set to be the arguments this module reads, got \
             {GET_CONFIGURATION_REQUIRED_PARAMETERS:?} against {built_from:?}"
        );
        // And every one of them is a parameter an integration may map at all, so a mapping
        // that defines the required set is never reported as mapping something uncarried.
        for parameter in GET_CONFIGURATION_REQUIRED_PARAMETERS {
            assert!(
                GET_CONFIGURATION_PARAMETERS.contains(parameter),
                "case {parameter:?}: expected a required parameter among the carried ones \
                 {GET_CONFIGURATION_PARAMETERS:?}"
            );
        }
    }

    /// RULE: an argument the action cannot be called without that this request's mapping
    /// resolved nothing for leaves no call to make, and the reason names that parameter.
    #[test]
    fn a_missing_argument_leaves_no_call_to_make() {
        let nothing = Mapped::default();
        for parameter in GET_CONFIGURATION_REQUIRED_PARAMETERS {
            let read = argument(&nothing, parameter);
            let reason = match read {
                Err(unperformed) => unperformed.reason(),
                Ok(value) => panic!(
                    "case {parameter:?}: expected no call to make for an empty mapping, got \
                     {value:?}"
                ),
            };
            assert_eq!(
                reason,
                format!(
                    "the integration maps no {parameter}, which {GET_CONFIGURATION_SUBTYPE} \
                     requires"
                ),
                "case {parameter:?}: expected the reason to name the parameter, got {reason:?}"
            );
        }
    }
}
