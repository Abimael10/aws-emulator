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
//! The same resolution decides whether a deployment can be STARTED at all for a source
//! AppConfig reads before it records one ([`source_version_refusal`]).

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

/// The media type a Parameter Store parameter's content is described by.
///
/// A parameter's value is TEXT whatever its `ParameterType` is — `String` and
/// `SecureString` hold one string and `StringList` "a comma-separated list of values"
/// (`smithy_query(query="shape", shape="ParameterType")`, Systems Manager's model) — so the
/// standard MIME type describing the bytes of a deployed parameter version is plain text.
pub const PARAMETER_CONTENT_TYPE: &str = "text/plain";

/// What a configuration profile's `LocationUri` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationSource<'a> {
    /// AppConfig's own hosted configuration store, whose versions this service holds
    /// itself ([`super::ConfigurationProfile::hosted_versions`]).
    Hosted,
    /// A Systems Manager Parameter Store parameter, carrying the identifier the location
    /// named it by: a bare parameter name for the `ssm-parameter://` form, and the whole
    /// ARN for the ARN form, which is how Parameter Store itself takes "the name or Amazon
    /// Resource Name (ARN) of the parameter".
    SsmParameter(&'a str),
    /// An Amazon S3 object, carrying the bucket and the object key the location named —
    /// "for an Amazon S3 object, specify the URI in the following format:
    /// `s3://<bucket>/<objectKey>`". The version a deployment of such a profile names is
    /// that object's `VersionId`.
    S3Object(S3ObjectLocation),
    /// One of the other documented external stores, carrying the whole location: the
    /// deployment names a version of it, and this emulator holds no reader that resolves
    /// that store's versions, so nothing is served for it.
    Elsewhere(&'a str),
}

/// The object an `s3://<bucket>/<objectKey>` location addresses, in the two parts Amazon S3
/// itself addresses an object by.
///
/// Read by the tree's ONE reading of such a URI
/// ([`::service_common::s3_uri::object_address`]), so a location this service resolves and
/// one another service resolves name the same object — and a URI Amazon S3 holds no bucket
/// under names no object here either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3ObjectLocation {
    /// The bucket the object lives in.
    pub bucket: String,
    /// The object key inside that bucket.
    pub key: String,
}

/// What `location_uri` names ([`ConfigurationSource`]).
///
/// The ARN form is recognised by the resource an `ssm` ARN addresses, so a parameter ARN
/// resolves as a parameter and an ARN naming any other Systems Manager resource — a
/// document, say — does not. An `s3://` location is read by the shared reader of that URI,
/// so one naming no object — a bucket alone, or an authority Amazon S3 holds no bucket under
/// — addresses nothing a read could resolve and is no S3 object location.
pub fn source_of(location_uri: &str) -> ConfigurationSource<'_> {
    if location_uri == HOSTED_LOCATION_URI {
        return ConfigurationSource::Hosted;
    }
    if let Some(name) = location_uri.strip_prefix(SSM_PARAMETER_LOCATION_PREFIX) {
        return ConfigurationSource::SsmParameter(name);
    }
    if let Some((bucket, key)) = ::service_common::s3_uri::object_address(location_uri) {
        return ConfigurationSource::S3Object(S3ObjectLocation { bucket, key });
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
        ConfigurationSource::S3Object(object) => {
            s3_object_version(engine, location_uri, &object, configuration_version)
        }
        ConfigurationSource::Elsewhere(location) => Err(SourceUnavailable {
            message: source_version_unavailable_message(configuration_version, location),
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

/// The content of the Amazon S3 object version `configuration_version` names of the object
/// `object` addresses.
///
/// The version is the object's own selector — an S3 `VersionId` — so the bytes served are
/// the ones that stored version holds, whatever is currently at the key: a later write at
/// the key mints a NEW version and leaves this one where it was. The object is read out of
/// S3's own store, through that service's own addressing of a bucket and of one version of
/// a key, so one spelling cannot resolve two ways between the two services.
fn s3_object_version<Eng: Send + Sync + 'static>(
    engine: &Eng,
    location_uri: &str,
    object: &S3ObjectLocation,
    configuration_version: &str,
) -> Result<SourceConfiguration, SourceUnavailable> {
    use ::service_com_amazonaws_s3_amazons3::state as object_store;

    let unavailable = |message: String| SourceUnavailable { message };
    let Some(state) = object_store::service_state(engine) else {
        return Err(unavailable(
            "no Amazon S3 service is mounted beside AppConfig, so an S3-backed \
             configuration cannot be retrieved"
                .to_string(),
        ));
    };
    let store = state.lock();
    let stored = store
        .bucket(&object.bucket)
        .and_then(|bucket| bucket.object_version(&object.key, configuration_version));
    let Some(stored) = stored else {
        return Err(unavailable(source_version_unavailable_message(
            configuration_version,
            location_uri,
        )));
    };
    Ok(SourceConfiguration {
        content: stored.body.clone(),
        // The media type the OBJECT is described by, which is what S3 answers a read of it
        // with; a write that named none stored S3's own default, so there is always one.
        content_type: stored
            .content_type
            .clone()
            .unwrap_or_else(|| object_store::DEFAULT_CONTENT_TYPE.to_string()),
    })
}

/// The sentence a reader states for a source version the source's own store does not hold.
///
/// Confined to exactly that: a version that genuinely does not exist at the location the
/// profile points at, rather than a store this emulator cannot read.
fn source_version_unavailable_message(configuration_version: &str, location_uri: &str) -> String {
    format!(
        "Configuration version '{configuration_version}' could not be retrieved from \
         {location_uri}."
    )
}

/// Why a deployment of the configuration version `configuration_version` of the profile at
/// `location_uri` is refused by the READ AppConfig makes at the source before it records
/// one — and `None` when nothing about that read refuses it, which is every deployment that
/// is accepted.
///
/// AppConfig resolves the version a deployment names at the source as the deployment is
/// started, so a version the source does not hold is refused there and then rather than
/// recorded and found missing when a client polls for it. The read is Amazon S3's own
/// metadata read of the object version, and S3 answers a read of a version of a bucket it
/// holds that the caller cannot read — one the object does not have among them — `403
/// Forbidden`, which AppConfig surfaces as the failure of that call
/// ([`s3_metadata_read_forbidden`]).
///
/// Confined to exactly that answer, which is why the bucket has to be one the store HOLDS:
/// a read addressed at a bucket that does not exist is answered something else, and a
/// refusal worded here for it would report an answer the store never gave — such a
/// deployment is left to the reader of the configuration, which reports what it could not
/// retrieve. Only an S3 location is asked about at all: another store's reader answers in
/// its own words.
pub fn source_version_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    location_uri: &str,
    configuration_version: &str,
) -> Option<String> {
    let ConfigurationSource::S3Object(object) = source_of(location_uri) else {
        return None;
    };
    // The existence of the version, and nothing else: the gate reads no bytes, so the
    // configuration is not copied out of the object store to decide a refusal.
    let state = ::service_com_amazonaws_s3_amazons3::state::service_state(engine)?;
    let store = state.lock();
    let bucket = store.bucket(&object.bucket)?;
    bucket
        .object_version(&object.key, configuration_version)
        .is_none()
        .then(s3_metadata_read_forbidden)
}

/// How AppConfig reports an Amazon S3 metadata read of a configuration object that was
/// answered `403 Forbidden`.
///
/// The envelope is the AWS SDK's own rendering of a service error — the operation that was
/// called, the status and error code S3 answered with, and the pair of correlation ids that
/// one call is named by — so the only parts that differ between two calls are those ids,
/// which are minted by S3's own mint ([`::service_com_amazonaws_s3_amazons3::state::request_ids`]).
fn s3_metadata_read_forbidden() -> String {
    let correlation =
        ::service_com_amazonaws_s3_amazons3::state::request_ids::RequestCorrelation::mint();
    format!(
        "An error occurred when calling the S3 GetObjectMetadata operation: Forbidden \
         (Service: Amazon S3; Status Code: 403; Error Code: 403 Forbidden; Request ID: {}; \
         S3 Extended Request ID: {}; Proxy: null)",
        correlation.request_id, correlation.extended_request_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a `LocationUri` names the store the configuration lives in, and only `hosted`
    /// names AppConfig's own one.
    ///
    /// The rows are the documented forms of the member
    /// (`smithy_query(query="doc", shape="CreateConfigurationProfileRequest",
    /// member="LocationUri")`), each with what it names — and, for the `s3://` form, the
    /// authorities Amazon S3 holds no bucket under, which name no object to read.
    #[test]
    fn a_location_uri_names_the_store_its_configuration_lives_in() {
        /// The object an `s3://` row names.
        fn object(bucket: &str, key: &str) -> ConfigurationSource<'static> {
            ConfigurationSource::S3Object(S3ObjectLocation {
                bucket: bucket.to_string(),
                key: key.to_string(),
            })
        }

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
                "s3://fleet-config-objects/my-app/us-east-1/my-config.json",
                object(
                    "fleet-config-objects",
                    "my-app/us-east-1/my-config.json",
                ),
            ),
            (
                "s3://fleet-config-objects/config.json",
                object("fleet-config-objects", "config.json"),
            ),
            // An `s3://` location naming no object addresses nothing a read can resolve, so
            // it is not one of them — a bucket alone, and an authority Amazon S3 holds no
            // bucket under (an upper-case name, one carrying a port, one under the published
            // length floor, and the documentation's own placeholder, whose prefix is
            // reserved).
            (
                "s3://fleet-config-objects",
                ConfigurationSource::Elsewhere("s3://fleet-config-objects"),
            ),
            (
                "s3://fleet-config-objects/",
                ConfigurationSource::Elsewhere("s3://fleet-config-objects/"),
            ),
            (
                "s3://Fleet-Config-Objects/config.json",
                ConfigurationSource::Elsewhere("s3://Fleet-Config-Objects/config.json"),
            ),
            (
                "s3://fleet-config-objects:443/config.json",
                ConfigurationSource::Elsewhere("s3://fleet-config-objects:443/config.json"),
            ),
            (
                "s3://ab/config.json",
                ConfigurationSource::Elsewhere("s3://ab/config.json"),
            ),
            (
                "s3://amzn-s3-demo-bucket/my-config.json",
                ConfigurationSource::Elsewhere("s3://amzn-s3-demo-bucket/my-config.json"),
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
