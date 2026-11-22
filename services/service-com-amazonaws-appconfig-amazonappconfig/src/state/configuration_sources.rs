// SPDX-License-Identifier: CC0-1.0

//! Where a configuration profile's configuration LIVES, and the content a deployed
//! version of it has when that location is not AppConfig's own hosted store.
//!
//! A configuration profile's `LocationUri` is "a URI to locate the configuration", and
//! the hosted store is only one of the locations it may name: `hosted` for the AppConfig
//! hosted configuration store, `ssm-parameter://<parameter name>` or a parameter ARN for a
//! Systems Manager Parameter Store parameter, `s3://<bucket>/<objectKey>`,
//! `secretsmanager://<secret name>`, `ssm-document://<document name>` and
//! `codepipeline://<pipeline name>` for the other stores
//! (`smithy_query(query="doc", shape="CreateConfigurationProfileRequest",
//! member="LocationUri")`). A deployment of such a profile names a version of the SOURCE —
//! "the configuration version to deploy. If deploying an AppConfig hosted configuration
//! version, you can specify either the version number or version label. For all other
//! configurations, you must specify the version number"
//! (`smithy_query(query="doc", shape="StartDeploymentRequest",
//! member="ConfigurationVersion")`) — so what a reader of a deployed configuration serves
//! is that version's content AT THE SOURCE, and not something out of the hosted store the
//! profile never used.
//!
//! The content is resolved per READ, out of the source's own store and for the version the
//! deployment recorded, which is what pins a deployment to the version it named: a later
//! write at the source mints a NEW version and leaves the deployed one where it was, so
//! nothing a client is served changes until a deployment names the new version.
//!
//! One resolver, so every reader of a deployed configuration — this service's own
//! `GetConfiguration` and the `appconfigdata` poll — serves one answer for one deployment.

use super::HOSTED_LOCATION_URI;

/// The `LocationUri` prefix that selects a Systems Manager Parameter Store parameter by
/// name: "specify either the parameter name in the format `ssm-parameter://<parameter
/// name>` or the ARN".
pub const SSM_PARAMETER_LOCATION_PREFIX: &str = "ssm-parameter://";

/// The `LocationUri` prefix that selects an SSM document: "specify either the document
/// name in the format `ssm-document://<document name>` or the Amazon Resource Name (ARN)".
pub const SSM_DOCUMENT_LOCATION_PREFIX: &str = "ssm-document://";

/// The `LocationUri` prefix that selects a Secrets Manager secret: "specify the URI in the
/// following format: `secretsmanager`://<secret name>".
pub const SECRETS_MANAGER_LOCATION_PREFIX: &str = "secretsmanager://";

/// The `LocationUri` prefix that selects an Amazon S3 object: "specify the URI in the
/// following format: `s3://<bucket>/<objectKey>`".
pub const S3_LOCATION_PREFIX: &str = "s3://";

/// The media type a Parameter Store parameter's content is described by.
///
/// A parameter's value is TEXT whatever its `ParameterType` is — `String` and
/// `SecureString` hold one string and `StringList` "a comma-separated list of values"
/// (`smithy_query(query="shape", shape="ParameterType")`, Systems Manager's model) — so the
/// standard MIME type describing the bytes of a deployed parameter version is plain text.
pub const PARAMETER_CONTENT_TYPE: &str = "text/plain";

/// What a configuration profile's `LocationUri` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationSource<'a> {
    /// AppConfig's own hosted configuration store, whose versions this service holds
    /// itself ([`super::ConfigurationProfile::hosted_versions`]).
    Hosted,
    /// A Systems Manager Parameter Store parameter, carrying the identifier the location
    /// named it by: a bare parameter name for the `ssm-parameter://` form, and the whole
    /// ARN for the ARN form, which is how Parameter Store itself takes "the name or Amazon
    /// Resource Name (ARN) of the parameter".
    SsmParameter(&'a str),
    /// One of the other documented external stores, carrying the whole location: the
    /// deployment names a version of it, and this emulator holds no reader that resolves
    /// that store's versions, so nothing is served for it.
    Elsewhere(&'a str),
}

/// What `location_uri` names ([`ConfigurationSource`]).
///
/// The ARN form is recognised by the resource an `ssm` ARN addresses, so a parameter ARN
/// resolves as a parameter and an ARN naming any other Systems Manager resource — a
/// document, say — does not.
pub fn source_of(location_uri: &str) -> ConfigurationSource<'_> {
    if location_uri == HOSTED_LOCATION_URI {
        return ConfigurationSource::Hosted;
    }
    if let Some(name) = location_uri.strip_prefix(SSM_PARAMETER_LOCATION_PREFIX) {
        return ConfigurationSource::SsmParameter(name);
    }
    if ::service_common::arn::parse_of(location_uri, SSM_VENDOR)
        .is_some_and(|arn| arn.resource_type() == PARAMETER_ARN_RESOURCE_TYPE)
    {
        return ConfigurationSource::SsmParameter(location_uri);
    }
    ConfigurationSource::Elsewhere(location_uri)
}

/// The `ssm` ARN namespace, as the `LocationUri` of a parameter-backed profile spells it.
const SSM_VENDOR: &str = "ssm";

/// The resource type a Parameter Store parameter's ARN carries
/// (`arn:aws:ssm:<region>:<account>:parameter/<name>`).
const PARAMETER_ARN_RESOURCE_TYPE: &str = "parameter";

/// The content one version of an external configuration source holds.
#[derive(Debug, Clone)]
pub struct SourceConfiguration {
    /// The source version's bytes, as the source holds them.
    pub content: Vec<u8>,
    /// The standard MIME type describing those bytes.
    pub content_type: String,
}

/// Why the source `location_uri` names has no content for the version a deployment
/// recorded. Carries the sentence the reader refuses with.
#[derive(Debug, Clone)]
pub struct SourceUnavailable {
    /// What the refusal says, derived from the location, the version and what the source's
    /// own store holds.
    pub message: String,
}

impl SourceUnavailable {
    /// `Message` for a configuration version a deployment names and the EXTERNAL source the
    /// profile points at has no content for.
    ///
    /// Derived from the request, the profile's own location and what the source's store
    /// answered — never from the hosted store, which a profile pointing elsewhere does not
    /// use. Shared by every reader of a configuration version, so one unretrievable source
    /// version is reported one way.
    pub fn refusal_message(
        &self,
        configuration_profile_identifier: &str,
        configuration_version: &str,
        location_uri: &str,
    ) -> String {
        format!(
            "Configuration version '{configuration_version}' of configuration profile \
             '{configuration_profile_identifier}' could not be retrieved from {location_uri}: {}",
            self.message
        )
    }
}

/// The content the version `configuration_version` names at the external source
/// `location_uri`.
///
/// `region` is the Region the reading request was signed for, which is the endpoint the
/// source is resolved through: a configuration profile and the source it points at live at
/// the same endpoint, and a source ARN naming another Region names nothing reachable here.
///
/// A guard-free read of the source's own store: it takes that store's lock, holds no other
/// while it does and hands back owned values, so it is called with no guard of AppConfig's
/// own store held. Nothing is written — what a client is served is a read.
///
/// `location_uri` must not be the hosted store ([`ConfigurationSource::Hosted`]), whose
/// versions this service holds itself; a caller that passes it is told so rather than
/// served the hosted version behind this resolver's back.
pub fn source_configuration<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    location_uri: &str,
    configuration_version: &str,
) -> Result<SourceConfiguration, SourceUnavailable> {
    match source_of(location_uri) {
        ConfigurationSource::Hosted => Err(SourceUnavailable {
            message: format!(
                "Configuration version '{configuration_version}' is held in the AppConfig \
                 hosted configuration store, not at an external source."
            ),
        }),
        ConfigurationSource::SsmParameter(identifier) => {
            parameter_version(engine, region, identifier, configuration_version)
        }
        ConfigurationSource::Elsewhere(location) => Err(SourceUnavailable {
            message: format!(
                "Configuration version '{configuration_version}' could not be retrieved from \
                 {location}."
            ),
        }),
    }
}

/// The content of the Parameter Store version `configuration_version` names of the
/// parameter `identifier` names.
///
/// The identifier is read exactly as Parameter Store reads "the name or Amazon Resource
/// Name (ARN) of the parameter", through that service's own parse of it, and the version is
/// its own selector — "either the version number or the label used to retrieve the
/// parameter value" — so one spelling cannot resolve two ways between the two services.
fn parameter_version<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    identifier: &str,
    configuration_version: &str,
) -> Result<SourceConfiguration, SourceUnavailable> {
    use ::service_com_amazonaws_ssm_amazonssm::state as parameter_store;

    let unavailable = |message: String| SourceUnavailable { message };
    let Some(query) = parameter_store::parse_parameter_query(identifier) else {
        return Err(unavailable(format!(
            "{identifier} does not name a Systems Manager parameter."
        )));
    };
    // A parameter is reached through the endpoint it is asked for, so an identifier naming
    // another account or another Region than this read was signed for names nothing
    // reachable here — the same rule Parameter Store itself resolves a name or ARN by.
    if !query.names_this_endpoint(region) {
        return Err(unavailable(format!(
            "{} does not name a Systems Manager parameter in {region}.",
            query.path
        )));
    }
    let Some(state) = parameter_store::service_state(engine) else {
        return Err(unavailable(
            "no Systems Manager service is mounted beside AppConfig, so a parameter-backed \
             configuration cannot be retrieved"
                .to_string(),
        ));
    };
    let store = state.lock();
    let Some(record) = store.parameter(region, query.name) else {
        return Err(unavailable(
            parameter_store::parameter_not_found_message(query.path),
        ));
    };
    let Some(version) = record.select(Some(configuration_version)) else {
        return Err(unavailable(parameter_store::selector_not_found_message(
            query.path,
            configuration_version,
        )));
    };
    // A `SecureString` version's value is held as the ciphertext it was sealed into, and
    // AppConfig reads such a parameter decrypted through the profile's own retrieval role:
    // the ciphertext is not the configuration, so nothing is served for it rather than the
    // sealed bytes being passed off as the content.
    if version.type_ == parameter_store::TYPE_SECURE_STRING {
        return Err(unavailable(format!(
            "The value of {} is encrypted, so it cannot be retrieved as a configuration.",
            query.path
        )));
    }
    Ok(SourceConfiguration {
        content: version.value.clone().into_bytes(),
        content_type: PARAMETER_CONTENT_TYPE.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a `LocationUri` names the store the configuration lives in, and only `hosted`
    /// names AppConfig's own one.
    ///
    /// The rows are the documented forms of the member
    /// (`smithy_query(query="doc", shape="CreateConfigurationProfileRequest",
    /// member="LocationUri")`), each with what it names.
    #[test]
    fn a_location_uri_names_the_store_its_configuration_lives_in() {
        let rows: &[(&str, ConfigurationSource<'_>)] = &[
            ("hosted", ConfigurationSource::Hosted),
            (
                "ssm-parameter://my-config",
                ConfigurationSource::SsmParameter("my-config"),
            ),
            (
                "ssm-parameter:///fleet/eu/my-config",
                ConfigurationSource::SsmParameter("/fleet/eu/my-config"),
            ),
            (
                "arn:aws:ssm:eu-west-2:123456789012:parameter/my-config",
                ConfigurationSource::SsmParameter(
                    "arn:aws:ssm:eu-west-2:123456789012:parameter/my-config",
                ),
            ),
            (
                "s3://amzn-s3-demo-bucket/my-app/us-east-1/my-config.json",
                ConfigurationSource::Elsewhere(
                    "s3://amzn-s3-demo-bucket/my-app/us-east-1/my-config.json",
                ),
            ),
            (
                "secretsmanager://my-secret",
                ConfigurationSource::Elsewhere("secretsmanager://my-secret"),
            ),
            (
                "ssm-document://my-document",
                ConfigurationSource::Elsewhere("ssm-document://my-document"),
            ),
            (
                "codepipeline://my-pipeline",
                ConfigurationSource::Elsewhere("codepipeline://my-pipeline"),
            ),
            (
                "arn:aws:ssm:eu-west-2:123456789012:document/my-document",
                ConfigurationSource::Elsewhere(
                    "arn:aws:ssm:eu-west-2:123456789012:document/my-document",
                ),
            ),
        ];
        for (location_uri, expected) in rows {
            let actual = source_of(location_uri);
            assert_eq!(
                actual, *expected,
                "case {location_uri:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
