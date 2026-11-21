// SPDX-License-Identifier: CC0-1.0

//! The OAuth2 credential providers of this account's token vault: the record a
//! create persists, the ARN it mints, the configuration each vendor's
//! credentials are carried in, and the Secrets Manager secret the client secret
//! itself lives in.
//!
//! One concern, kept beside the token vault rather than inside one `ops/` file:
//! the provider's own ARN, the secret its client secret is stored under and the
//! response envelopes are shared by the create and by every later read, so two
//! operations cannot report one provider two ways.
//!
//! ## Which configuration a vendor's credentials are carried in
//!
//! `credentialProviderVendor` and `oauth2ProviderConfigInput` are one
//! configuration, not two independent members: the vendor decides which member
//! of the union carries the credentials ([`required_config_member`]), so a
//! request whose declared vendor and supplied member disagree is refused rather
//! than reinterpreted as the vendor the payload happens to name.
//!
//! ## The address an authorizing user is redirected back to
//!
//! A provider reports a `callbackUrl` ([`oauth2_authorization_callback_url`]):
//! the redirect URI the caller REGISTERS with its vendor's authorization server,
//! not an endpoint the API caller dials, and the one address of this emulator —
//! never a host AWS answers on — so a redirect carrying an authorization code
//! stays inside the emulated environment. Nothing of this service answers at that
//! path yet: the 3-legged flow a user agent would arrive there from is the data
//! plane's `GetResourceOauth2Token`, which still answers 501, so the address is
//! published for vendor registration alone and the flow behind it is not claimed
//! to work.
//!
//! ## Where the client secret lives
//!
//! Not in this service's own store. Each configuration carries a `MANAGED` or an
//! `EXTERNAL` secret source, exactly as the API key and payment providers beside
//! it do ([`super::api_key_credential_providers`], whose Secrets Manager helpers
//! this module shares): a managed secret is the one this create writes the
//! caller's `clientSecret` into, and an external one is the caller's own, read
//! through the call that DECRYPTS it so a secret whose KMS key will not decrypt
//! is the modelled `DecryptionFailure`.

use super::*;
use crate::ops::types::{
    AtlassianOauth2ProviderConfigOutput, CreateOauth2CredentialProviderOutput,
    CustomOauth2ProviderConfigOutput, GetOauth2CredentialProviderOutput,
    GithubOauth2ProviderConfigOutput, GoogleOauth2ProviderConfigOutput,
    IncludedOauth2ProviderConfigOutput, LinkedinOauth2ProviderConfigOutput,
    MicrosoftOauth2ProviderConfigOutput, Oauth2AuthorizationServerMetadata,
    Oauth2CredentialProviderItem, Oauth2Discovery, Oauth2ProviderConfigInput,
    Oauth2ProviderConfigOutput, SalesforceOauth2ProviderConfigOutput, Secret, SecretReference,
    SlackOauth2ProviderConfigOutput, UpdateOauth2CredentialProviderOutput,
};

/// The ARN path segment an OAuth2 credential provider of a token vault is
/// addressed under, from `CredentialProviderArnType`'s pattern
/// `^arn:(aws|aws-us-gov):acps:…:token-vault/…/oauth2credentialprovider/…$`
/// (`smithy_query shape=CredentialProviderArnType`).
pub const OAUTH2_CREDENTIAL_PROVIDER_ARN_TYPE: &str = "oauth2credentialprovider";

/// `arn:aws:bedrock-agentcore:{region}:{account}:token-vault/{tokenVaultId}/oauth2credentialprovider/{name}`
/// — the provider nested under the token vault that holds it, ending in the name
/// the caller chose, in the ARN namespace of the service holding the vault
/// ([`ARN_SERVICE`]), exactly as every other credential provider of the vault is
/// addressed ([`api_key_credential_provider_arn`]). The namespace is the one
/// `ConsentPortalIdpConfig.credentialProviderArn` — a consumer of THIS ARN —
/// requires: `arn:…:bedrock-agentcore:…:token-vault/…/oauth2credentialprovider/…`
/// (`smithy_query shape=ConsentPortalIdpConfig`), while
/// `CredentialProviderArnType`'s own pattern still spells the legacy `acps` one.
pub fn oauth2_credential_provider_arn(region: &str, token_vault_id: &str, name: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("token-vault/{token_vault_id}/{OAUTH2_CREDENTIAL_PROVIDER_ARN_TYPE}/{name}"),
    )
}

/// The path this service's identity plane receives an OAuth2 authorization
/// redirect at: the `/identities` prefix every credential-provider route of this
/// service is served under (`smithy_query operation=GetOauth2CredentialProvider`
/// reports the URI `/identities/GetOauth2CredentialProvider`), with the OAuth2
/// flow's callback beneath it.
pub const OAUTH2_AUTHORIZATION_CALLBACK_PATH: &str = "/identities/oauth2/callback";

/// Where an OAuth2 authorization server sends a user back after they authorize:
/// `callbackUrl` is "Callback URL to register on the OAuth2 credential provider
/// as an allowed callback URL. This URL is where the OAuth2 authorization server
/// redirects users after they complete the authorization flow" (`smithy_query
/// doc shape=GetOauth2CredentialProviderResponse member=callbackUrl`).
///
/// An address a user agent is REDIRECTED to, so the one published names the
/// endpoint THIS emulator is served at (`endpoint_base`, the base
/// [`::smithy_server_runtime::OperationContext::endpoint`] reports) rather than a
/// host a client here resolves nowhere near this process — the same reading every
/// other address this service publishes for a caller to FOLLOW carries
/// ([`consent_portal_url`], [`payment_connector_authorization_url`]).
///
/// ONE address per endpoint rather than one per provider: the callback belongs to
/// the identity plane that completes the authorization flow, not to the provider
/// whose vendor redirects to it, so every provider of the vault registers the
/// same redirect URI with its vendor.
///
/// What a caller DOES with it is register it with that vendor; nothing of this
/// service answers there yet, because the flow that would redirect a user agent
/// to it is the data plane's own `GetResourceOauth2Token`, still a 501 stub (see
/// this module's header).
pub fn oauth2_authorization_callback_url(endpoint_base: &str) -> String {
    format!(
        "{}{OAUTH2_AUTHORIZATION_CALLBACK_PATH}",
        endpoint_base.trim_end_matches('/')
    )
}

/// How this service words an OAuth2 credential provider no name of this account
/// resolves to: ENTIRELY FIXED text, naming neither the kind of credential
/// provider asked for nor the name the request addressed it by.
///
/// Each credential-provider family words its own refusal — the sentence belongs
/// to the route that answers it, not to a template shared across the families —
/// so this one is not derived from [`api_key_credential_provider_not_found`],
/// which does state the name its request carried. Saying nothing about the name
/// is also what keeps a read of one family from reporting whether ANOTHER
/// family holds that name: every absent name gets one answer.
pub fn oauth2_credential_provider_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(OAUTH2_CREDENTIAL_PROVIDER_NOT_FOUND_MESSAGE.to_string()),
    }
}

/// The whole of the sentence [`oauth2_credential_provider_not_found`] carries.
pub const OAUTH2_CREDENTIAL_PROVIDER_NOT_FOUND_MESSAGE: &str = "CredentialProvider not found";

/// How many OAuth2 credential providers this emulator will hold.
///
/// `CreateOauth2CredentialProvider` models `ServiceQuotaExceededException` (HTTP
/// 402) for exceeding the real service's quota; this number is not a fidelity
/// claim about that quota but the emulator's own ceiling, set generously so no
/// realistic workflow meets it, so a caller looping on create cannot grow this
/// process's memory without bound.
pub const MAX_OAUTH2_CREDENTIAL_PROVIDERS: usize = 1_000;

/// The segment of a managed secret's name that says what is in it, beside the
/// API key provider's own ([`MANAGED_SECRET_PREFIX`]).
const MANAGED_SECRET_KIND: &str = "oauth2";

/// The JSON key the service's own managed secret holds the client secret under:
/// a secret's value is a JSON document, so the value sits in it under a name of
/// the service's choosing — the member's own name on the wire. It is the layout
/// of a secret this SERVICE manages rather than a secret reference the caller
/// supplied, so it is not reported as the provider's `clientSecretJsonKey`.
pub const OAUTH2_MANAGED_SECRET_JSON_KEY: &str = "clientSecret";

/// `Status` for a credential provider whose secret this create has already
/// stored or resolved: there is nothing left for the clock to owe it. The
/// enum's full value list is `CREATING | CREATE_FAILED | UPDATING |
/// UPDATE_FAILED | READY | DELETING | DELETE_FAILED`
/// (`crate::ops::types::STATUS_VALUES`, from `smithy_query shape=Status`).
pub const OAUTH2_PROVIDER_STATUS_READY: &str = "READY";

/// The name the secret holding a provider's client secret is created under: the
/// service's own prefix, the token vault the provider belongs to, and the
/// provider's name, so the name is a function of the resource rather than of
/// anything else the caller sent.
pub fn managed_oauth2_secret_name(token_vault_id: &str, provider_name: &str) -> String {
    format!("{MANAGED_SECRET_PREFIX}/{token_vault_id}/{MANAGED_SECRET_KIND}/{provider_name}")
}

/// The request member that carries the vendor's configuration, named as the wire
/// spells it, so a refusal of it names what the caller sent.
pub const OAUTH2_PROVIDER_CONFIG_INPUT_MEMBER: &str = "oauth2ProviderConfigInput";

/// The `Oauth2ProviderConfigInput` member each vendor with a configuration of
/// its own is carried in, and the authorization-server metadata document that
/// vendor publishes.
///
/// The member is the vendor's name with its `Oauth2` suffix dropped and
/// `Oauth2ProviderConfig` appended, as the union spells it (`smithy_query
/// shape=Oauth2ProviderConfigInput`), paired with the
/// `CredentialProviderVendorType` value that selects it
/// (`crate::ops::types::CREDENTIAL_PROVIDER_VENDOR_TYPE_VALUES`).
///
/// A vendor configured through its own member supplies no authorization server
/// of its own — the member declares only the client it identifies itself as and
/// the secret it authenticates with — because the vendor runs ONE server, whose
/// metadata document it publishes at the location the standards put it: the
/// OpenID Connect Discovery 1.0 `/.well-known/openid-configuration`, or RFC
/// 8414's `/.well-known/oauth-authorization-server` for a vendor that serves
/// OAuth2 without OpenID Connect. That document is what a provider of this
/// vendor is reported with.
const VENDOR_CONFIG_MEMBERS: &[(&str, &str, &str)] = &[
    (
        "CustomOauth2",
        OAUTH2_CONFIG_MEMBER_CUSTOM,
        // A custom provider names its own authorization server, so no document
        // of the service's is reported for it.
        "",
    ),
    (
        "GoogleOauth2",
        "googleOauth2ProviderConfig",
        "https://accounts.google.com/.well-known/openid-configuration",
    ),
    (
        "GithubOauth2",
        "githubOauth2ProviderConfig",
        "https://github.com/.well-known/oauth-authorization-server",
    ),
    (
        "SlackOauth2",
        "slackOauth2ProviderConfig",
        "https://slack.com/.well-known/openid-configuration",
    ),
    (
        "SalesforceOauth2",
        "salesforceOauth2ProviderConfig",
        "https://login.salesforce.com/.well-known/openid-configuration",
    ),
    (
        "MicrosoftOauth2",
        "microsoftOauth2ProviderConfig",
        // One server per directory: the tenant the request names stands in the
        // path ([`microsoft_discovery_url`]).
        "",
    ),
    (
        "AtlassianOauth2",
        "atlassianOauth2ProviderConfig",
        "https://auth.atlassian.com/.well-known/openid-configuration",
    ),
    (
        "LinkedinOauth2",
        "linkedinOauth2ProviderConfig",
        "https://www.linkedin.com/oauth/.well-known/openid-configuration",
    ),
];

/// The directory a Microsoft provider's authorization server is scoped to when
/// the request names no `tenantId`: the multi-tenant endpoint, which is what a
/// client that did not pick a directory authenticates against.
const MICROSOFT_DEFAULT_TENANT: &str = "common";

/// The authorization-server metadata document of a Microsoft provider, which is
/// scoped to one directory: the `tenantId` the request named, else the
/// multi-tenant endpoint.
fn microsoft_discovery_url(tenant_id: Option<&str>) -> String {
    format!(
        "https://login.microsoftonline.com/{}/v2.0/.well-known/openid-configuration",
        tenant_id
            .filter(|tenant| !tenant.is_empty())
            .unwrap_or(MICROSOFT_DEFAULT_TENANT)
    )
}

/// The document `vendor` publishes its authorization server's metadata at, as
/// the discovery a response reports the provider's configuration with.
fn published_discovery(vendor: &str) -> Oauth2Discovery {
    let url = VENDOR_CONFIG_MEMBERS
        .iter()
        .find(|(declared, _, _)| *declared == vendor)
        .map_or("", |(_, _, url)| *url);
    Oauth2Discovery::discoveryUrl(url.to_string())
}

/// The union member a CUSTOM authorization server's configuration is carried in,
/// which is also the one whose `oauthDiscovery` the caller supplies.
pub const OAUTH2_CONFIG_MEMBER_CUSTOM: &str = "customOauth2ProviderConfig";

/// The union member every OTHER vendor's configuration is carried in: "the
/// configuration for a non-custom OAuth2 provider … supported OAuth2 providers
/// that have built-in integration support" (`smithy_query doc
/// shape=Oauth2ProviderConfigInput member=includedOauth2ProviderConfig`), so a
/// vendor the union declares no member of its own for is configured through this
/// one.
pub const OAUTH2_CONFIG_MEMBER_INCLUDED: &str = "includedOauth2ProviderConfig";

/// The `Oauth2ProviderConfigInput` member `vendor`'s credentials must be carried
/// in: the vendor's own member where the union declares one, and the included
/// provider's member for every other vendor.
pub fn required_config_member(vendor: &str) -> &'static str {
    VENDOR_CONFIG_MEMBERS
        .iter()
        .find(|(declared, _, _)| *declared == vendor)
        .map_or(OAUTH2_CONFIG_MEMBER_INCLUDED, |(_, member, _)| *member)
}

/// The `Oauth2ProviderConfigInput` member a request actually supplied, named as
/// the wire spells it.
pub fn supplied_config_member(config: &Oauth2ProviderConfigInput) -> &'static str {
    match config {
        Oauth2ProviderConfigInput::customOauth2ProviderConfig(_) => OAUTH2_CONFIG_MEMBER_CUSTOM,
        Oauth2ProviderConfigInput::googleOauth2ProviderConfig(_) => "googleOauth2ProviderConfig",
        Oauth2ProviderConfigInput::githubOauth2ProviderConfig(_) => "githubOauth2ProviderConfig",
        Oauth2ProviderConfigInput::slackOauth2ProviderConfig(_) => "slackOauth2ProviderConfig",
        Oauth2ProviderConfigInput::salesforceOauth2ProviderConfig(_) => {
            "salesforceOauth2ProviderConfig"
        }
        Oauth2ProviderConfigInput::microsoftOauth2ProviderConfig(_) => {
            "microsoftOauth2ProviderConfig"
        }
        Oauth2ProviderConfigInput::atlassianOauth2ProviderConfig(_) => {
            "atlassianOauth2ProviderConfig"
        }
        Oauth2ProviderConfigInput::linkedinOauth2ProviderConfig(_) => {
            "linkedinOauth2ProviderConfig"
        }
        Oauth2ProviderConfigInput::includedOauth2ProviderConfig(_) => OAUTH2_CONFIG_MEMBER_INCLUDED,
    }
}

/// The modelled validation error for a request whose `oauth2ProviderConfigInput`
/// is not the member its `credentialProviderVendor` selects.
///
/// The sentence is fixed and the vendor is the value the request declared, so
/// the caller is told which of the two members it sent is being honoured as the
/// selector.
pub fn oauth2_configuration_mismatch(vendor: &str) -> ValidationException {
    ValidationException {
        message: format!("Provided configuration does not match selected type: {vendor}"),
        reason: VALIDATION_REASON_FIELD_VALIDATION_FAILED.to_string(),
        field_list: None,
    }
}

/// The `@pattern` of `Oauth2Discovery$discoveryUrl`, which a discovery URL has
/// to satisfy and which the refusal of one that does not states verbatim.
///
/// The expression is the one the SERVICE enforces, which is not the one the
/// model shipped in this repo spells: `Oauth2Discovery.discoveryUrl` ->
/// `DiscoveryUrlType` (`smithy_query shape=Oauth2Discovery`) still carries the
/// older `^.+/\.well-known/(openid-configuration|oauth-authorization-server)$`,
/// while the service both quotes and enforces the one below — the issuer and
/// any trailing path segment made of characters a URL may carry before its
/// query or fragment begins, and the metadata document allowed a path segment
/// after it rather than having to end the URL.
pub const DISCOVERY_URL_PATTERN: &str =
    r"^[^\s?#]+/\.well-known/(openid-configuration|oauth-authorization-server)(/[^\s?#]+)?$";

/// The two documents an authorization server's metadata is published as, which
/// are the only ones [`DISCOVERY_URL_PATTERN`] admits.
const DISCOVERY_DOCUMENT_SUFFIXES: [&str; 2] = [
    "/.well-known/openid-configuration",
    "/.well-known/oauth-authorization-server",
];

/// The standard location of an authorization server's OpenID discovery
/// document, under the issuer that publishes it: the suffix
/// [`DISCOVERY_URL_PATTERN`] admits appended to the issuer, with the issuer's
/// trailing separator dropped so the path is not doubled.
pub fn discovery_url_for_issuer(issuer: &str) -> String {
    format!(
        "{}{}",
        issuer.trim_end_matches('/'),
        DISCOVERY_DOCUMENT_SUFFIXES[0]
    )
}

/// Whether `url` satisfies [`DISCOVERY_URL_PATTERN`]: one of the two published
/// metadata documents, preceded by at least one character of issuer and
/// followed by nothing or by one more path segment, with every character of the
/// issuer and of that segment one [`discovery_url_segment_is_admitted`] allows.
///
/// The check runs the expression the refusal QUOTES, so the set this emulator
/// accepts is the set it advertises.
///
/// LINEAR IN THE LENGTH OF A CALLER-SUPPLIED VALUE, which is what makes it safe
/// to run on a request: the character class is settled in ONE pass over the
/// whole value, and each candidate position of the document then costs a
/// constant check, so the work is the length of the URL plus the number of
/// candidate positions — never the product of the two. Nothing re-scans a
/// prefix or a remainder per candidate.
pub fn discovery_url_satisfies_pattern(url: &str) -> bool {
    // Both variable parts of the expression are drawn from `[^\s?#]`, and the
    // fixed middle carries none of those characters either, so one disallowed
    // character ANYWHERE in the value refuses it whatever the rest looks like.
    if !discovery_url_segment_is_admitted(url) {
        return false;
    }
    DISCOVERY_DOCUMENT_SUFFIXES.iter().any(|suffix| {
        // The document can occur more than once in one URL, and the expression
        // is satisfied if ANY occurrence leaves an issuer before it and an
        // admitted remainder after it, exactly as a regular-expression match
        // would be. With the character class already settled, each occurrence
        // asks only where it sits and what the next character is.
        url.match_indices(suffix).any(|(at, _)| {
            let trailing = &url[at + suffix.len()..];
            at > 0
                && (trailing.is_empty()
                    || trailing
                        .strip_prefix('/')
                        .is_some_and(|segment| !segment.is_empty()))
        })
    })
}

/// Whether every character of `segment` is one `[^\s?#]` admits: anything but
/// whitespace, which no URL carries unescaped, and the two delimiters that end
/// a URL's path — `?` opening the query and `#` opening the fragment, neither of
/// which is part of the discovery endpoint itself.
fn discovery_url_segment_is_admitted(segment: &str) -> bool {
    !segment.contains(|character: char| {
        character.is_whitespace() || character == '?' || character == '#'
    })
}

/// The modelled constraint-violation error for a member whose value is outside
/// its `@pattern`.
///
/// The SENTENCE is not spelled here: it is this service's own
/// ([`crate::service::validation`]), so a constraint a HANDLER enforces is worded
/// exactly as one the framework's boundary walk enforces — the envelope, the
/// unanchored rendering of the pattern (that module's rule 3) and whether the
/// value the caller sent is echoed (its rule 5, which the `/identities` family
/// suppresses) all come from there.
///
/// `member_path` is the full path of the offending member from the operation's
/// input inwards, `pattern` the member's own modelled pattern as the model spells
/// it, and `submitted` the value the request carried, whose fate the echo policy
/// decides.
pub fn member_pattern_violation(
    member_path: &str,
    pattern: &str,
    submitted: &str,
) -> ValidationException {
    let violation = ::aws_smithy_schema_server::constraints::Violation {
        path: member_path.to_string(),
        kind: ::aws_smithy_schema_server::constraints::ViolationKind::Pattern(pattern.to_string()),
        value: Some(submitted.to_string()),
        sensitive: false,
    };
    ValidationException {
        // One member outside its pattern is one violation of a class that module
        // words, so the message always renders; the fallback states the member
        // and the pattern rather than an empty sentence.
        message: crate::service::validation::handler_refusal_message(
            &[violation],
            crate::ops::types::CREATE_OAUTH2_CREDENTIAL_PROVIDER_INPUT_SCHEMA,
        )
        .unwrap_or_else(|| format!("{member_path} must satisfy {pattern}")),
        reason: VALIDATION_REASON_FIELD_VALIDATION_FAILED.to_string(),
        field_list: None,
    }
}

/// The path of the `discoveryUrl` inside the configuration member `config_member`
/// of an `oauth2ProviderConfigInput`, as the constraint-violation envelope spells
/// a member path: the trail of member names from the operation's input inwards.
pub fn discovery_url_member_path(config_member: &str) -> String {
    format!("{OAUTH2_PROVIDER_CONFIG_INPUT_MEMBER}.{config_member}.oauthDiscovery.discoveryUrl")
}

/// Check the discovery a configuration carries against the modelled pattern of
/// `discoveryUrl`.
///
/// Only the URL form has a pattern to satisfy: the metadata form carries the
/// endpoints themselves, whose own members the boundary walk already refused
/// before a handler ran.
pub fn validate_discovery(
    config_member: &str,
    discovery: &Oauth2Discovery,
) -> Result<(), ValidationException> {
    match discovery {
        Oauth2Discovery::discoveryUrl(url) if !discovery_url_satisfies_pattern(url) => {
            Err(member_pattern_violation(
                &discovery_url_member_path(config_member),
                DISCOVERY_URL_PATTERN,
                url,
            ))
        }
        _ => Ok(()),
    }
}

/// The members of a vendor's configuration this service needs whichever vendor
/// it is: the client it identifies itself as, and what is to become of the
/// client secret it authenticates with.
#[derive(Debug, Clone)]
pub struct RequestedOauth2Configuration {
    /// The `Oauth2ProviderConfigInput` member the request carried.
    pub config_member: &'static str,
    pub client_id: Option<String>,
    /// The secret value a MANAGED provider hands over for this service to store.
    pub client_secret: Option<String>,
    /// The caller's own secret an EXTERNAL provider references.
    pub client_secret_config: Option<SecretReference>,
    /// The source the request named, if it named one.
    pub requested_source: Option<String>,
    /// What every response reports the provider's configuration as: the
    /// authorization server the provider authenticates against, and the client
    /// it identifies itself as.
    pub config_output: Oauth2ProviderConfigOutput,
}

impl RequestedOauth2Configuration {
    /// What the configuration a request carried asks for, or the modelled
    /// validation failure it is refused for: a configuration that is not the
    /// member the vendor selects, a discovery URL outside its pattern, or an
    /// included provider whose authorization server the request does not name.
    pub fn from_input(
        vendor: &str,
        config: &Oauth2ProviderConfigInput,
    ) -> Result<Self, ValidationException> {
        let config_member = supplied_config_member(config);
        if config_member != required_config_member(vendor) {
            return Err(oauth2_configuration_mismatch(vendor));
        }
        match config {
            Oauth2ProviderConfigInput::customOauth2ProviderConfig(custom) => {
                validate_discovery(config_member, &custom.oauth_discovery)?;
                Ok(Self {
                    config_member,
                    client_id: custom.client_id.clone(),
                    client_secret: custom.client_secret.clone(),
                    client_secret_config: custom.client_secret_config.clone(),
                    requested_source: custom.client_secret_source.clone(),
                    // A custom provider's authorization server is the one the
                    // request named, so the configuration is reported with the
                    // discovery it carried.
                    config_output: Oauth2ProviderConfigOutput::customOauth2ProviderConfig(
                        CustomOauth2ProviderConfigOutput {
                            oauth_discovery: custom.oauth_discovery.clone(),
                            client_id: custom.client_id.clone(),
                            on_behalf_of_token_exchange_config: custom
                                .on_behalf_of_token_exchange_config
                                .clone(),
                            client_authentication_method: custom
                                .client_authentication_method
                                .clone(),
                            private_endpoint: custom.private_endpoint.clone(),
                            private_endpoint_overrides: custom.private_endpoint_overrides.clone(),
                            private_key_jwt_config: custom.private_key_jwt_config.clone(),
                        },
                    ),
                })
            }
            Oauth2ProviderConfigInput::includedOauth2ProviderConfig(included) => {
                // An included provider is a server this service was TOLD about:
                // the member declares the issuer and the two endpoints for
                // exactly that, and the provider's configuration cannot be
                // reported without an authorization server, so a request that
                // names none is refused rather than stored half-configured.
                let oauth_discovery = included_discovery(included.into()).ok_or_else(|| {
                    field_validation_failed(
                        &format!("{OAUTH2_PROVIDER_CONFIG_INPUT_MEMBER}.{config_member}.issuer"),
                        &format!(
                            "is required to identify the authorization server of a \
                             credentialProviderVendor of {vendor}"
                        ),
                    )
                })?;
                validate_discovery(config_member, &oauth_discovery)?;
                Ok(Self {
                    config_member,
                    client_id: Some(included.client_id.clone()),
                    client_secret: included.client_secret.clone(),
                    client_secret_config: included.client_secret_config.clone(),
                    requested_source: included.client_secret_source.clone(),
                    config_output: Oauth2ProviderConfigOutput::includedOauth2ProviderConfig(
                        IncludedOauth2ProviderConfigOutput {
                            oauth_discovery,
                            client_id: Some(included.client_id.clone()),
                        },
                    ),
                })
            }
            // Every remaining vendor declares the same three members and runs
            // one authorization server of its own, so the configuration a
            // response reports carries the document that vendor publishes.
            Oauth2ProviderConfigInput::googleOauth2ProviderConfig(vendor_config) => {
                Ok(Self::from_vendor_config(
                    config_member,
                    &vendor_config.client_id,
                    vendor_config.client_secret.clone(),
                    vendor_config.client_secret_config.clone(),
                    vendor_config.client_secret_source.clone(),
                    Oauth2ProviderConfigOutput::googleOauth2ProviderConfig(
                        GoogleOauth2ProviderConfigOutput {
                            oauth_discovery: published_discovery(vendor),
                            client_id: Some(vendor_config.client_id.clone()),
                        },
                    ),
                ))
            }
            Oauth2ProviderConfigInput::githubOauth2ProviderConfig(vendor_config) => {
                Ok(Self::from_vendor_config(
                    config_member,
                    &vendor_config.client_id,
                    vendor_config.client_secret.clone(),
                    vendor_config.client_secret_config.clone(),
                    vendor_config.client_secret_source.clone(),
                    Oauth2ProviderConfigOutput::githubOauth2ProviderConfig(
                        GithubOauth2ProviderConfigOutput {
                            oauth_discovery: published_discovery(vendor),
                            client_id: Some(vendor_config.client_id.clone()),
                        },
                    ),
                ))
            }
            Oauth2ProviderConfigInput::slackOauth2ProviderConfig(vendor_config) => {
                Ok(Self::from_vendor_config(
                    config_member,
                    &vendor_config.client_id,
                    vendor_config.client_secret.clone(),
                    vendor_config.client_secret_config.clone(),
                    vendor_config.client_secret_source.clone(),
                    Oauth2ProviderConfigOutput::slackOauth2ProviderConfig(
                        SlackOauth2ProviderConfigOutput {
                            oauth_discovery: published_discovery(vendor),
                            client_id: Some(vendor_config.client_id.clone()),
                        },
                    ),
                ))
            }
            Oauth2ProviderConfigInput::salesforceOauth2ProviderConfig(vendor_config) => {
                Ok(Self::from_vendor_config(
                    config_member,
                    &vendor_config.client_id,
                    vendor_config.client_secret.clone(),
                    vendor_config.client_secret_config.clone(),
                    vendor_config.client_secret_source.clone(),
                    Oauth2ProviderConfigOutput::salesforceOauth2ProviderConfig(
                        SalesforceOauth2ProviderConfigOutput {
                            oauth_discovery: published_discovery(vendor),
                            client_id: Some(vendor_config.client_id.clone()),
                        },
                    ),
                ))
            }
            Oauth2ProviderConfigInput::microsoftOauth2ProviderConfig(vendor_config) => {
                Ok(Self::from_vendor_config(
                    config_member,
                    &vendor_config.client_id,
                    vendor_config.client_secret.clone(),
                    vendor_config.client_secret_config.clone(),
                    vendor_config.client_secret_source.clone(),
                    Oauth2ProviderConfigOutput::microsoftOauth2ProviderConfig(
                        MicrosoftOauth2ProviderConfigOutput {
                            // One server per directory, so the tenant the
                            // request named is part of the document's location.
                            oauth_discovery: Oauth2Discovery::discoveryUrl(
                                microsoft_discovery_url(vendor_config.tenant_id.as_deref()),
                            ),
                            client_id: Some(vendor_config.client_id.clone()),
                        },
                    ),
                ))
            }
            Oauth2ProviderConfigInput::atlassianOauth2ProviderConfig(vendor_config) => {
                Ok(Self::from_vendor_config(
                    config_member,
                    &vendor_config.client_id,
                    vendor_config.client_secret.clone(),
                    vendor_config.client_secret_config.clone(),
                    vendor_config.client_secret_source.clone(),
                    Oauth2ProviderConfigOutput::atlassianOauth2ProviderConfig(
                        AtlassianOauth2ProviderConfigOutput {
                            oauth_discovery: published_discovery(vendor),
                            client_id: Some(vendor_config.client_id.clone()),
                        },
                    ),
                ))
            }
            Oauth2ProviderConfigInput::linkedinOauth2ProviderConfig(vendor_config) => {
                Ok(Self::from_vendor_config(
                    config_member,
                    &vendor_config.client_id,
                    vendor_config.client_secret.clone(),
                    vendor_config.client_secret_config.clone(),
                    vendor_config.client_secret_source.clone(),
                    Oauth2ProviderConfigOutput::linkedinOauth2ProviderConfig(
                        LinkedinOauth2ProviderConfigOutput {
                            oauth_discovery: published_discovery(vendor),
                            client_id: Some(vendor_config.client_id.clone()),
                        },
                    ),
                ))
            }
        }
    }

    /// The three secret members every built-in vendor's configuration declares,
    /// read off whichever of them the request carried, with the configuration
    /// that vendor's provider is reported as.
    fn from_vendor_config(
        config_member: &'static str,
        client_id: &str,
        client_secret: Option<String>,
        client_secret_config: Option<SecretReference>,
        requested_source: Option<String>,
        config_output: Oauth2ProviderConfigOutput,
    ) -> Self {
        Self {
            config_member,
            client_id: Some(client_id.to_string()),
            client_secret,
            client_secret_config,
            requested_source,
            config_output,
        }
    }

    /// Store or resolve this configuration's client secret against AWS Secrets
    /// Manager, and report the secret the provider is to carry.
    ///
    /// Call it with NO state guard held — it awaits.
    pub async fn resolve_secret(
        &self,
        ctx: &::smithy_server_runtime::OperationContext<'_>,
        region: &str,
        token_vault_id: &str,
        provider_name: &str,
    ) -> Result<StoredOauth2Secret, ApiKeySecretProblem> {
        let source = effective_secret_source(
            self.requested_source.as_deref(),
            self.client_secret_config.is_some(),
        );
        if source == SECRET_SOURCE_EXTERNAL {
            if let Some(reference) = &self.client_secret_config {
                return Ok(StoredOauth2Secret {
                    secret_arn: referenced_external_secret_arn(ctx, region, &reference.secret_id)
                        .await?,
                    json_key: Some(reference.json_key.clone()),
                    source: SECRET_SOURCE_EXTERNAL.to_string(),
                });
            }
        }
        // Nothing referenced, so the secret is the service's to manage: the
        // value the caller handed over goes into a secret named for the
        // provider, and that secret's ARN is what the provider reports.
        let value = ::serde_json::json!({
            OAUTH2_MANAGED_SECRET_JSON_KEY: self.client_secret.clone().unwrap_or_default(),
        })
        .to_string();
        Ok(StoredOauth2Secret {
            secret_arn: store_managed_secret(
                ctx,
                region,
                &managed_oauth2_secret_name(token_vault_id, provider_name),
                &value,
            )
            .await?,
            // The document's key is the service's own, named by no
            // `SecretReference` of the request, so the provider reports no JSON
            // key for a secret it manages.
            json_key: None,
            source: SECRET_SOURCE_MANAGED.to_string(),
        })
    }
}

/// The three members an included provider's authorization server can be
/// described by, read off the configuration the request carried.
struct IncludedEndpoints<'a> {
    issuer: Option<&'a String>,
    authorization_endpoint: Option<&'a String>,
    token_endpoint: Option<&'a String>,
}

impl<'a> From<&'a crate::ops::types::IncludedOauth2ProviderConfigInput> for IncludedEndpoints<'a> {
    fn from(config: &'a crate::ops::types::IncludedOauth2ProviderConfigInput) -> Self {
        Self {
            issuer: config.issuer.as_ref(),
            authorization_endpoint: config.authorization_endpoint.as_ref(),
            token_endpoint: config.token_endpoint.as_ref(),
        }
    }
}

/// The discovery an included provider's configuration is reported with: the
/// metadata itself when the request named the whole authorization server, the
/// standard discovery document under the issuer when it named only that, and
/// nothing when it named neither — this service resolves no authorization server
/// it was not told about.
fn included_discovery(endpoints: IncludedEndpoints<'_>) -> Option<Oauth2Discovery> {
    let issuer = endpoints.issuer?;
    match (endpoints.authorization_endpoint, endpoints.token_endpoint) {
        (Some(authorization_endpoint), Some(token_endpoint)) => Some(
            Oauth2Discovery::authorizationServerMetadata(Oauth2AuthorizationServerMetadata {
                issuer: issuer.clone(),
                authorization_endpoint: authorization_endpoint.clone(),
                token_endpoint: token_endpoint.clone(),
                response_types: None,
                token_endpoint_auth_methods: None,
            }),
        ),
        _ => Some(Oauth2Discovery::discoveryUrl(discovery_url_for_issuer(
            issuer,
        ))),
    }
}

/// The client secret of one provider as every response that reports it carries
/// it: who manages it, the ARN its owner holds it at and the JSON key the value
/// is extracted under — the one the caller's own `SecretReference` named, and
/// none for a secret the service manages. The VALUE is not here — it is in
/// Secrets Manager.
#[derive(Debug, Clone)]
pub struct StoredOauth2Secret {
    pub secret_arn: String,
    pub json_key: Option<String>,
    /// The EFFECTIVE [`SECRET_SOURCE_MANAGED`] / [`SECRET_SOURCE_EXTERNAL`].
    pub source: String,
}

/// One OAuth2 credential provider of this account, as
/// `CreateOauth2CredentialProvider` persisted it.
///
/// The name is the identifier: the read, the update and the delete all address a
/// provider by `name`, so the store is keyed by it.
#[derive(Debug, Clone)]
pub struct Oauth2CredentialProviderRecord {
    pub name: String,
    pub credential_provider_arn: String,
    pub credential_provider_vendor: String,
    /// The secret the provider's client secret lives in, as its owner reported
    /// it.
    pub client_secret: StoredOauth2Secret,
    /// The configuration every response reports: the authorization server the
    /// provider authenticates against, and the client it identifies itself as.
    pub config_output: Oauth2ProviderConfigOutput,
    /// The redirect URI this provider's vendor is to be configured with
    /// ([`oauth2_authorization_callback_url`]), held on the record so every read
    /// of the provider reports the address the create published and an update
    /// leaves it as it is.
    pub callback_url: String,
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl Oauth2CredentialProviderRecord {
    /// This provider as `CreateOauth2CredentialProvider`'s response envelope.
    pub fn create_output(&self) -> CreateOauth2CredentialProviderOutput {
        CreateOauth2CredentialProviderOutput {
            client_secret_arn: Secret {
                secret_arn: self.client_secret.secret_arn.clone(),
            },
            client_secret_json_key: self.client_secret.json_key.clone(),
            client_secret_source: Some(self.client_secret.source.clone()),
            name: self.name.clone(),
            credential_provider_arn: self.credential_provider_arn.clone(),
            // The address the provider's vendor redirects an authorizing user
            // back to, which every response declaring it reports off the one
            // record ([`oauth2_authorization_callback_url`]).
            callback_url: Some(self.callback_url.clone()),
            oauth2_provider_config_output: Some(self.config_output.clone()),
            // The create stored or resolved the provider's secret before
            // answering, so nothing is left for the clock to owe it.
            status: Some(OAUTH2_PROVIDER_STATUS_READY.to_string()),
        }
    }

    /// This provider as `GetOauth2CredentialProvider`'s response envelope — the
    /// members [`Self::create_output`] renders plus the vendor and the two
    /// instants the read reports, off the one record, so a create and a later
    /// read cannot disagree.
    pub fn get_output(&self) -> GetOauth2CredentialProviderOutput {
        let created = self.create_output();
        GetOauth2CredentialProviderOutput {
            client_secret_arn: created.client_secret_arn,
            client_secret_json_key: created.client_secret_json_key,
            client_secret_source: created.client_secret_source,
            name: created.name,
            credential_provider_arn: created.credential_provider_arn,
            credential_provider_vendor: self.credential_provider_vendor.clone(),
            callback_url: created.callback_url,
            oauth2_provider_config_output: self.config_output.clone(),
            created_time: timestamp(self.created_at),
            last_updated_time: timestamp(self.updated_at),
            status: created.status,
            // Reported only for a status that HAS a reason; a provider this
            // emulator creates is READY, which has none.
            failure_reason: None,
        }
    }

    /// Replace this provider's configuration with the one an update supplied.
    ///
    /// An update carries the whole configuration — the vendor, the configuration
    /// member that vendor selects and the client secret it authenticates with are
    /// all `@required` on the request — so the provider afterwards is configured
    /// exactly as the update asked, with nothing of the previous configuration
    /// left mixed in. What the update does NOT touch is what identifies the
    /// provider rather than configures it: the name it is addressed by, the ARN
    /// minted from that name, the redirect URI its vendor is configured with, the
    /// tags it carries and the instant it was created.
    pub fn apply_update(
        &mut self,
        vendor: String,
        config_output: Oauth2ProviderConfigOutput,
        client_secret: StoredOauth2Secret,
        at: SystemTime,
    ) {
        self.credential_provider_vendor = vendor;
        self.config_output = config_output;
        self.client_secret = client_secret;
        self.updated_at = at;
    }

    /// This provider as `UpdateOauth2CredentialProvider`'s response envelope —
    /// the members the read reports minus the failure reason the update's own
    /// output does not declare, off the one record the update has just written,
    /// so the update and a later read cannot disagree.
    pub fn update_output(&self) -> UpdateOauth2CredentialProviderOutput {
        let read = self.get_output();
        UpdateOauth2CredentialProviderOutput {
            client_secret_arn: read.client_secret_arn,
            client_secret_json_key: read.client_secret_json_key,
            client_secret_source: read.client_secret_source,
            name: read.name,
            credential_provider_vendor: read.credential_provider_vendor,
            credential_provider_arn: read.credential_provider_arn,
            callback_url: read.callback_url,
            oauth2_provider_config_output: read.oauth2_provider_config_output,
            created_time: read.created_time,
            last_updated_time: read.last_updated_time,
            status: read.status,
        }
    }

    /// This provider as `ListOauth2CredentialProviders` reports it: the members
    /// `Oauth2CredentialProviderItem` declares, off the same record the create
    /// answered from.
    pub fn item(&self) -> Oauth2CredentialProviderItem {
        Oauth2CredentialProviderItem {
            name: self.name.clone(),
            credential_provider_vendor: self.credential_provider_vendor.clone(),
            credential_provider_arn: self.credential_provider_arn.clone(),
            created_time: timestamp(self.created_at),
            last_updated_time: timestamp(self.updated_at),
        }
    }
}
