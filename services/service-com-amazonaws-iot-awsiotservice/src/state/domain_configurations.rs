// SPDX-License-Identifier: CC0-1.0

//! The account's DOMAIN CONFIGURATIONS: the record one holds, the server certificate
//! it presents, and what every reader of one reports.
//!
//! A domain configuration is the endpoint settings a device connects under — which
//! domain name it dials, which certificate the service presents in the TLS
//! handshake, which custom authorizer runs — so a record is those settings plus the
//! status the configuration is in. The name is the configuration's identity: the
//! model binds it as the `@httpLabel` of
//! `/domainConfigurations/{domainConfigurationName}` on every single-configuration
//! operation (`smithy_query(query="operation",operation="CreateDomainConfiguration")`),
//! which is why the store is a `BTreeMap` keyed by it.
//!
//! One record carries everything a reader hands back, so
//! `DescribeDomainConfiguration` after `CreateDomainConfiguration` reports the
//! created configuration member for member.
//!
//! The SERVER CERTIFICATE a configuration presents belongs to Certificate Manager,
//! not to this service: "The ARNs of the certificates that IoT passes to the device
//! during the TLS handshake" (`CreateDomainConfiguration$serverCertificateArns`). A
//! configuration may only name a certificate that EXISTS, and whether one exists is
//! the holder's answer — which this service reaches through the one shared record of a
//! certificate's serving material that Certificate Manager writes
//! ([`certificate_key_material`]). A certificate that record holds nothing for is one
//! this service cannot present, so the create is refused and nothing is written.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use crate::ops::types::{
    AuthorizerConfig, ClientCertificateConfig, DescribeDomainConfigurationOutput,
    DomainConfigurationSummary, ServerCertificateConfig, ServerCertificateSummary, TlsConfig,
};

/// How many domain configurations this emulator keeps for the account.
///
/// AWS publishes a service quota on an account's domain configurations; the exact
/// figure is not in the model, so this is the emulator's own ceiling on a store a
/// caller can grow — the same reason [`super::MAX_POLICIES`] exists — and
/// `CreateDomainConfiguration` past it refuses with the modelled
/// `LimitExceededException` rather than growing without bound.
pub const MAX_DOMAIN_CONFIGURATIONS: usize = 100;

/// How many tags one domain configuration may carry: the same documented IoT tagging
/// limit [`super::MAX_POLICY_TAGS`] records, for the same reason.
pub const MAX_DOMAIN_CONFIGURATION_TAGS: usize = super::MAX_POLICY_TAGS;

/// The `serviceType` a configuration created without one carries.
///
/// `CreateDomainConfiguration$serviceType` is "The type of service delivered by the
/// endpoint", with the note "Amazon Web Services IoT Core currently supports only the
/// DATA service type", so a request that names none is a data endpoint. It is one of
/// [`crate::ops::types::SERVICE_TYPE_VALUES`].
pub const DEFAULT_SERVICE_TYPE: &str = "DATA";

/// The `domainConfigurationStatus` a freshly created configuration carries.
///
/// A configuration serves connections from the moment it is created —
/// `UpdateDomainConfiguration` is what DISABLES one — so a create leaves it enabled.
/// It is one of [`crate::ops::types::DOMAIN_CONFIGURATION_STATUS_VALUES`].
pub const DOMAIN_CONFIGURATION_ENABLED: &str = "ENABLED";

/// The `domainType` of a configuration that named its own `domainName`.
///
/// `DomainType` is `ENDPOINT | AWS_MANAGED | CUSTOMER_MANAGED`
/// (`smithy_query(query="shape",shape="DomainType")`), and a configuration carrying a
/// domain name of the customer's own with the certificate to present for it is the
/// customer-managed kind.
pub const DOMAIN_TYPE_CUSTOMER_MANAGED: &str = "CUSTOMER_MANAGED";

/// The `domainType` of a configuration that named no `domainName`.
///
/// `CreateDomainConfiguration$serverCertificateArns` is "not required for Amazon Web
/// Services-managed domains", so a configuration with neither a domain name nor a
/// certificate configures the AWS-managed domain the account's endpoint already
/// serves.
pub const DOMAIN_TYPE_AWS_MANAGED: &str = "AWS_MANAGED";

/// The `domainType` of a configuration the SERVICE manages for the account: the
/// settings of one of the account's own endpoints, which exists without anyone creating
/// it ([`RESERVED_DOMAIN_CONFIGURATIONS`]).
///
/// `DomainType` is `ENDPOINT | AWS_MANAGED | CUSTOMER_MANAGED`
/// (`smithy_query(query="shape",shape="DomainType")`); `ENDPOINT` is the kind that is an
/// endpoint of the account rather than a domain a caller configured.
pub const DOMAIN_TYPE_ENDPOINT: &str = "ENDPOINT";

/// The `domainConfigurationStatus` a configuration serves no connection in, and the one
/// a configuration has to be in before it can be deleted (see
/// [`UNDELETABLE_DOMAIN_CONFIGURATION_MESSAGE`]).
///
/// The other of the two values `DomainConfigurationStatus` admits
/// ([`crate::ops::types::DOMAIN_CONFIGURATION_STATUS_VALUES`]);
/// `UpdateDomainConfiguration$domainConfigurationStatus` ("The status to which the
/// domain configuration should be updated") is what moves a configuration into it.
pub const DOMAIN_CONFIGURATION_DISABLED: &str = "DISABLED";

/// The `applicationProtocol` a configuration that named none serves under.
///
/// `ApplicationProtocol` admits `SECURE_MQTT | MQTT_WSS | HTTPS | DEFAULT`, where
/// `DEFAULT` is "Use a combination of port and Application Layer Protocol Negotiation
/// (ALPN) to specify application_layer protocol" — the behaviour of an endpoint nobody
/// pinned to one protocol, so it is the effective value of a configuration created
/// without one rather than an absent member.
pub const DEFAULT_APPLICATION_PROTOCOL: &str = "DEFAULT";

/// The `authenticationType` a configuration that named none serves under.
///
/// `AuthenticationType` admits `CUSTOM_AUTH_X509 | CUSTOM_AUTH | AWS_X509 | AWS_SIGV4 |
/// DEFAULT`, where `DEFAULT` is "Use a combination of port and Application Layer
/// Protocol Negotiation (ALPN) to specify authentication type" — the behaviour of an
/// endpoint nobody pinned to one authentication type, so it is the effective value of a
/// configuration created without one.
pub const DEFAULT_AUTHENTICATION_TYPE: &str = "DEFAULT";

/// The `tlsConfig.securityPolicy` a configuration that named none serves under: the
/// service's current default TLS security policy, the one that admits TLS 1.3 and TLS
/// 1.2 (`TlsConfig$securityPolicy`, "The security policy for a domain configuration").
///
/// A domain configuration always serves under SOME security policy — TLS is how a
/// device reaches it — so a request that pins none is served the service's default, and
/// that is the policy a reader reports.
pub const DEFAULT_SECURITY_POLICY: &str = "IoTSecurityPolicy_TLS13_1_2_2022_10";

/// The `serverCertificateStatus` of a certificate a configuration presents.
///
/// A configuration may only name a certificate that exists, so the certificate it
/// holds is one the service could resolve — `VALID`, one of
/// [`crate::ops::types::SERVER_CERTIFICATE_STATUS_VALUES`].
pub const SERVER_CERTIFICATE_VALID: &str = "VALID";

/// The ARN of a domain configuration:
/// `arn:aws:iot:<region>:<account>:domainconfiguration/<domainConfigurationName>`.
///
/// `domainconfiguration/` is the resource type AWS IoT's domain-configuration ARNs
/// use — it is what `CreateDomainConfiguration$domainConfigurationArn` reports; the
/// namespace and account segments are the ones [`super::thing_arn`] documents.
pub fn domain_configuration_arn(region: &str, domain_configuration_name: &str) -> String {
    ::emulator_server::emulator_arn(
        "iot",
        region,
        &format!("domainconfiguration/{domain_configuration_name}"),
    )
}

/// The range the short id in a created configuration's ARN comes from: four decimal
/// digits, so an id is one of `0000`..`9999`.
const DOMAIN_CONFIGURATION_ID_RANGE: u32 = 10_000;

/// The ARN of a domain configuration a CALLER created:
/// `arn:aws:iot:<region>:<account>:domainconfiguration/<domainConfigurationName>/<id>`.
///
/// The resource part NAMES the configuration — which is what makes the ARN derivable
/// from the name by anyone writing an IAM resource statement — and carries after it the
/// short id the service mints for the configuration, which is the form AWS reports a
/// created configuration's ARN in. A configuration the SERVICE manages carries no such
/// id: its ARN is its reserved name alone ([`domain_configuration_arn`]).
pub fn created_domain_configuration_arn(
    region: &str,
    domain_configuration_name: &str,
    domain_configuration_id: &str,
) -> String {
    domain_configuration_arn(
        region,
        &format!("{domain_configuration_name}/{domain_configuration_id}"),
    )
}

/// A short id for one newly created domain configuration, minted from the same
/// randomness every other identifier of this service is ([`super::fresh_uuid`]).
///
/// It is minted ONCE, when the configuration is created, and lives in the ARN the record
/// carries, so every reader of that configuration reports the same ARN.
pub fn fresh_domain_configuration_id() -> String {
    let minted = super::fresh_uuid();
    let id = u32::from_str_radix(minted.get(..8).unwrap_or_default(), 16).unwrap_or_default()
        % DOMAIN_CONFIGURATION_ID_RANGE;
    format!("{id:04}")
}

/// The message every operation that addresses ONE domain configuration refuses a name
/// the account holds none under with.
///
/// The DOMAIN CONFIGURATION family words a miss after the two things that identify a
/// configuration — the account it would belong to and the name the request asked about
/// — rather than after the name alone, which is the fixed text AWS states this family's
/// miss with. The account id is this emulator's own ([`engine::emulator_account_id`]),
/// the account every record this service holds belongs to.
pub fn missing_domain_configuration_message(domain_configuration_name: &str) -> String {
    format!(
        "Could not find domain configuration for given account ID and domain configuration \
         name, AccountId: {}, DomainConfigurationName: {domain_configuration_name}",
        ::engine::emulator_account_id()
    )
}

/// How this service refuses a certificate ARN — the one a configuration PRESENTS
/// (`serverCertificateArns`) or the one it proves domain ownership with
/// (`validationCertificateArn`) — that names no certificate the holder has.
///
/// The fixed text is the class's — a certificate the holder does not have is not a
/// certificate this service can present or validate against — and the variable part is
/// the ARN the request named.
pub fn missing_acm_certificate_message(certificate_arn: &str) -> String {
    format!("Certificate {certificate_arn} does not exist.")
}

/// How this service refuses a custom-domain configuration whose server certificate is
/// not one a device can establish trust in.
///
/// AWS IoT presents the server certificate to a device, which chains it to a
/// certificate authority its own trust store holds; a certificate that chains to no CA
/// proves nothing about the domain on its own, so the configuration has to carry the
/// `validationCertificateArn` — "The certificate used to validate the server
/// certificate and prove domain name ownership. This certificate must be signed by a
/// public certificate authority" — instead. The sentence is the fixed text AWS states
/// the refusal with, verbatim: it states the rule and echoes nothing of the request.
pub const UNVALIDATED_SERVER_CERTIFICATE_MESSAGE: &str =
    "Must provide validation certificate when using server certificates not signed by trusted \
     CA or not within";

/// How this service refuses a configuration whose VALIDATION certificate is itself not
/// one a public authority signed.
///
/// `CreateDomainConfiguration$validationCertificateArn` is "The certificate used to
/// validate the server certificate and prove domain name ownership. This certificate
/// must be signed by a public certificate authority", so a validation certificate that
/// chains to no authority proves no ownership of the domain and cannot stand in for the
/// trust a device could not establish itself. The sentence is the fixed text AWS states
/// this refusal with, verbatim: it states the rule and echoes nothing of the request.
pub const UNTRUSTED_VALIDATION_CERTIFICATE_MESSAGE: &str =
    "Validation certificate must be signed by trusted CA";

/// How this service refuses an `authorizerConfig` whose `defaultAuthorizerName` names
/// no custom authorizer the account holds.
///
/// `AuthorizerConfig` is "An object that specifies the authorization service for a
/// domain", and that service is one of this account's OWN custom authorizers — the
/// records `CreateAuthorizer` writes — so a configuration may only name one the account
/// has: a name it holds none under names no authorization service, and a device
/// connecting under the configuration would reach none. The fixed text is the class's
/// and the variable part is the name the request carried, after the member that names
/// it.
pub fn missing_custom_authorizer_message(authorizer_name: &str) -> String {
    format!("Custom authorizer does not exist, AuthorizerName: {authorizer_name}")
}

/// How this service refuses the delete of a domain configuration the account still
/// serves connections under.
///
/// A configuration in `ENABLED` status is the live endpoint settings devices connect
/// under, and deleting it would take that endpoint out from under them in one call, so
/// the delete is admissible only once the status says it serves nobody — the same
/// precondition this service enforces on an authorizer
/// ([`super::undeletable_authorizer_message`]) and on a certificate. The sentence names
/// the status the configuration has to be moved to, because
/// `UpdateDomainConfiguration$domainConfigurationStatus` is what gets it there.
pub const UNDELETABLE_DOMAIN_CONFIGURATION_MESSAGE: &str =
    "Cannot delete a domain configuration in ENABLED status. Update the domain configuration \
     status to DISABLED and delete.";

/// How this service refuses an update addressed to a domain configuration the SERVICE
/// manages ([`RESERVED_DOMAIN_CONFIGURATIONS`]).
///
/// `UpdateDomainConfiguration` documents it itself — "Domain configurations for default
/// endpoints can't be updated" — because such a configuration is the account's own
/// endpoint rather than settings a caller supplied. The fixed text is the class's and
/// the variable part is the reserved name the request addressed.
pub fn unmodifiable_domain_configuration_message(domain_configuration_name: &str) -> String {
    format!(
        "Domain configuration {domain_configuration_name} is a default endpoint configuration \
         and cannot be updated"
    )
}

/// One domain configuration the SERVICE manages for every account: the settings of one
/// of the account's own endpoints, which exists without anyone creating it.
///
/// These are the `ENDPOINT`-type configurations, addressable by their RESERVED names:
/// every single-configuration reader takes a `ReservedDomainConfigurationName`, whose
/// pattern `^[\w.:-]+$` admits the `:` such a name carries, where
/// `CreateDomainConfiguration` and `DeleteDomainConfiguration` take a
/// `DomainConfigurationName` (`^[\w.-]+$`) that cannot spell one — so a caller can read
/// one and can neither mint nor remove it
/// (`smithy_query(query="operation",operation="DescribeDomainConfiguration")`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedDomainConfiguration {
    /// The reserved name the configuration is addressed by.
    pub name: &'static str,
    /// Which of [`crate::ops::types::SERVICE_TYPE_VALUES`] that endpoint delivers.
    pub service_type: &'static str,
}

/// The domain configurations every account holds without creating them.
///
/// The account's data endpoint is ATS-signed and is the one `DescribeEndpoint`'s
/// `endpointType` documentation recommends every client use ("`iot:Data-ATS` - Returns an
/// ATS signed data endpoint … We strongly recommend that customers use the newer
/// `iot:Data-ATS` endpoint type"), and beside it the account has the credentials-provider
/// endpoint ("`iot:CredentialProvider` - Returns an IoT credentials provider API
/// endpoint"). The settings of each — the security policy it serves TLS under, the
/// authentication type it accepts — are read through the domain-configuration plane under
/// that endpoint's own reserved name, so the account holds one configuration per
/// endpoint, each of [`DOMAIN_TYPE_ENDPOINT`] type and delivering that endpoint's
/// service, from the moment the account exists.
///
/// The ORDER is the order a listing reports them in: it is the service's own, which is
/// why it is not the alphabetical one the account's own configurations are listed in.
pub const RESERVED_DOMAIN_CONFIGURATIONS: &[ReservedDomainConfiguration] = &[
    ReservedDomainConfiguration {
        name: "iot:Data-ATS",
        service_type: DEFAULT_SERVICE_TYPE,
    },
    ReservedDomainConfiguration {
        name: "iot:CredentialProvider",
        service_type: "CREDENTIAL_PROVIDER",
    },
];

/// The domain configurations the SERVICE manages for this account, as this store holds
/// them: the instant they came into being, which is the instant the account's store did.
///
/// A reserved configuration carries no setting a caller supplied — everything about it
/// follows from the catalog entry, the account and the Region addressed — so the only
/// thing to persist is WHEN the account got it, which is what
/// `DescribeDomainConfiguration$lastStatusChangeDate` reports for it and what makes that
/// value stable across reads. The record is projected per request
/// ([`ReservedDomainConfigurations::resolve`]) rather than copied into
/// [`super::ComAmazonawsIotAwsiotserviceState::domain_configurations`], so a read never
/// writes and a caller can neither create over nor delete one.
#[derive(Debug, Clone)]
pub struct ReservedDomainConfigurations {
    /// When the account's reserved configurations came into being.
    since: SystemTime,
}

impl Default for ReservedDomainConfigurations {
    fn default() -> Self {
        Self {
            since: SystemTime::now(),
        }
    }
}

impl ReservedDomainConfigurations {
    /// The reserved configuration `domain_configuration_name` addresses, as a record a
    /// reader projects exactly like one a caller created, or `None` for a name that is
    /// no reserved name.
    ///
    /// `region` scopes the ARN to the Region the request addressed, the way every minted
    /// ARN of this service is scoped, and `data_endpoint` is the address the account's
    /// endpoint answers at ([`account_data_endpoint`]) — the domain a device dials it
    /// under, which is what `domainName` is for an endpoint AWS runs.
    pub fn resolve(
        &self,
        domain_configuration_name: &str,
        region: &str,
        data_endpoint: Option<&str>,
    ) -> Option<RegisteredDomainConfiguration> {
        let reserved = RESERVED_DOMAIN_CONFIGURATIONS
            .iter()
            .find(|reserved| reserved.name == domain_configuration_name)?;
        Some(RegisteredDomainConfiguration {
            domain_configuration_name: reserved.name.to_string(),
            domain_configuration_arn: domain_configuration_arn(region, reserved.name),
            domain_name: data_endpoint.map(str::to_string),
            server_certificate_arns: Vec::new(),
            validation_certificate_arn: None,
            authorizer_config: None,
            service_type: reserved.service_type.to_string(),
            domain_type: DOMAIN_TYPE_ENDPOINT.to_string(),
            status: DOMAIN_CONFIGURATION_ENABLED.to_string(),
            tls_config: Some(effective_tls_config(None)),
            server_certificate_config: None,
            authentication_type: Some(DEFAULT_AUTHENTICATION_TYPE.to_string()),
            application_protocol: Some(DEFAULT_APPLICATION_PROTOCOL.to_string()),
            client_certificate_config: None,
            tags: BTreeMap::new(),
            last_status_change_date: self.since,
            pending_status: None,
        })
    }

    /// Every reserved configuration of this account, resolved as [`Self::resolve`]
    /// resolves one.
    pub fn all(
        &self,
        region: &str,
        data_endpoint: Option<&str>,
    ) -> Vec<RegisteredDomainConfiguration> {
        RESERVED_DOMAIN_CONFIGURATIONS
            .iter()
            .filter_map(|reserved| self.resolve(reserved.name, region, data_endpoint))
            .collect()
    }
}

/// The TLS configuration a configuration serves under, given the one its request
/// carried: the policy the request pinned, or the service's default
/// ([`DEFAULT_SECURITY_POLICY`]) when it pinned none.
///
/// A domain configuration always serves TLS under some security policy, so this is the
/// value PERSISTED on a create or an update rather than one a reader fills in: what the
/// record holds and what every reader of it reports are one value.
pub fn effective_tls_config(requested: Option<TlsConfig>) -> TlsConfig {
    let security_policy = requested
        .and_then(|tls_config| tls_config.security_policy)
        .filter(|security_policy| !security_policy.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SECURITY_POLICY.to_string());
    TlsConfig {
        security_policy: Some(security_policy),
    }
}

/// The address this account's IoT DATA endpoint answers at, as the domain a device
/// dials it under.
///
/// A configuration for an AWS-managed domain names no domain of the caller's own: the
/// domain it configures is the account's own endpoint, so its `domainName` is the
/// address that endpoint is published at — the one value `DescribeEndpoint` answers
/// with, read from the same place
/// ([`::service_common::data_plane_addressing::emulator_endpoint_authority`]), so the
/// endpoint a client discovers and the domain a configuration names cannot drift.
///
/// `None` for an emulator that is served at no address at all, rather than a
/// `domainName` naming nothing.
pub fn account_data_endpoint(ctx: &::smithy_server_runtime::OperationContext<'_>) -> Option<String> {
    let endpoint = ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx);
    (!endpoint.trim().is_empty()).then_some(endpoint)
}

/// Whether a device presented the certificate `certificate_arn` can chain it to a
/// certificate authority its own trust store holds, as Certificate Manager's own record
/// of that certificate says.
///
/// `cert_type` is what decides it: a certificate ACM ISSUED is signed by a public
/// authority a device already trusts, and one the account IMPORTED is signed by whatever
/// authority the account used, which is none of them. So a configuration presenting an
/// imported certificate has to prove ownership of the domain another way — the
/// `validationCertificateArn` — while one presenting an ACM-issued certificate needs
/// nothing further.
///
/// `None` for an ARN the holder has no certificate under: that is a certificate this
/// service cannot present at all, which its caller refuses first.
pub fn chains_to_a_certificate_authority(
    certificate: &::service_com_amazonaws_acm_certificatemanager::state::Certificate,
) -> bool {
    !certificate.is_imported()
}

/// Certificate Manager's OWN record of this account's certificates, in the shared
/// engine: call it with a handler's `&*self.engine`.
///
/// Whether a `serverCertificateArn` names a certificate at all, and what kind of
/// certificate it is, is the HOLDER's answer and not this service's — "The ARNs of the
/// certificates that IoT passes to the device during the TLS handshake"
/// (`CreateDomainConfiguration$serverCertificateArns`) — so it is read from that
/// service's own store in this one shared engine rather than from a copy of its own
/// here: the engine keys state by TYPE, so a copy here would be invisible to the holder
/// and a certificate the holder has would address nothing.
///
/// LOCK ORDER: take [`super::service_state`] first and this one second, which is the
/// order every other reader of another service's store takes.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn acm_certificates<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<
    ::engine::ServiceState<
        ::service_com_amazonaws_acm_certificatemanager::state::ComAmazonawsAcmCertificatemanagerState,
    >,
> {
    ::service_com_amazonaws_acm_certificatemanager::state::service_state(engine)
}

/// The SHARED record of each Certificate Manager certificate's serving material, in
/// the shared engine: call it with a handler's `&*self.engine`.
///
/// A certificate is HELD by Certificate Manager and PRESENTED here, so the material is
/// one directory under one shared type
/// (`service_common::acm_certificate_key_material::CertificateKeyMaterialDirectory`)
/// and not a copy inside [`super::ComAmazonawsIotAwsiotserviceState`]: the engine keys
/// state by TYPE, so a copy of its own here would be invisible to the holder.
///
/// LOCK ORDER: take [`super::service_state`] first and this one second, which is the
/// order every other reader of a shared directory takes.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn certificate_key_material<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<
    ::engine::ServiceState<
        ::service_common::acm_certificate_key_material::CertificateKeyMaterialDirectory,
    >,
> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(
            ::engine::InMemoryEngine::state::<
                ::service_common::acm_certificate_key_material::CertificateKeyMaterialDirectory,
            >,
        )
}

/// One domain configuration in the store, as this service persists it.
///
/// The optional configuration blocks are kept as the MODELLED structures the request
/// carried, so a reader hands back what the writer sent rather than a reassembled
/// copy that could drift from it.
#[derive(Debug, Clone)]
pub struct RegisteredDomainConfiguration {
    /// The name the configuration was created under; also its key in the store.
    pub domain_configuration_name: String,
    /// `arn:aws:iot:<region>:<account>:domainconfiguration/<name>`, minted on create.
    pub domain_configuration_arn: String,
    /// `CreateDomainConfiguration$domainName`, when the request carried one.
    pub domain_name: Option<String>,
    /// The ARNs of the certificates the service presents for this domain, each one
    /// resolved before the record was written.
    pub server_certificate_arns: Vec<String>,
    /// `CreateDomainConfiguration$validationCertificateArn`, when the request carried
    /// one.
    pub validation_certificate_arn: Option<String>,
    /// `CreateDomainConfiguration$authorizerConfig`, when the request carried one.
    pub authorizer_config: Option<AuthorizerConfig>,
    /// Which of [`crate::ops::types::SERVICE_TYPE_VALUES`] the endpoint delivers.
    pub service_type: String,
    /// Which of [`crate::ops::types::DOMAIN_TYPE_VALUES`] the configuration is of.
    pub domain_type: String,
    /// Which of [`crate::ops::types::DOMAIN_CONFIGURATION_STATUS_VALUES`] it is in.
    pub status: String,
    /// `CreateDomainConfiguration$tlsConfig`, when the request carried one.
    pub tls_config: Option<TlsConfig>,
    /// `CreateDomainConfiguration$serverCertificateConfig`, when carried.
    pub server_certificate_config: Option<ServerCertificateConfig>,
    /// `CreateDomainConfiguration$authenticationType`, when carried.
    pub authentication_type: Option<String>,
    /// `CreateDomainConfiguration$applicationProtocol`, when carried.
    pub application_protocol: Option<String>,
    /// `CreateDomainConfiguration$clientCertificateConfig`, when carried.
    pub client_certificate_config: Option<ClientCertificateConfig>,
    /// `Tag$Key` -> `Tag$Value` for the tags the create request carried.
    pub tags: BTreeMap<String, Option<String>>,
    /// When the configuration last changed status —
    /// `DescribeDomainConfiguration$lastStatusChangeDate`. The create is the first
    /// such change.
    pub last_status_change_date: SystemTime,
    /// The status change this configuration is MOVING to and when it falls due, or
    /// `None` for one whose status is settled.
    ///
    /// Asking for a status is not being in it: the endpoint has to stop serving the
    /// connections it is serving before it is disabled, which is work the service does
    /// after acknowledging the request ([`DOMAIN_CONFIGURATION_STATUS_CHANGE_SPAN`]). So
    /// a caller that disables a configuration and acts in the same breath still sees the
    /// configuration it had — and the state errors that status carries, which is what
    /// refuses a delete that comes too early.
    pub pending_status: Option<PendingStatusChange>,
}

/// A status change one domain configuration is MOVING to: which status, and the instant
/// the clock makes it due.
#[derive(Debug, Clone)]
pub struct PendingStatusChange {
    /// Which of [`crate::ops::types::DOMAIN_CONFIGURATION_STATUS_VALUES`] the
    /// configuration moves to.
    pub status: String,
    /// When the move falls due. Nothing sleeps for it: every reader of the configuration
    /// compares it with its own `now`.
    pub due: SystemTime,
}

/// How long a requested `domainConfigurationStatus` change takes to come into effect.
///
/// A domain configuration is the live endpoint settings devices are connected under, so
/// moving it between `ENABLED` and `DISABLED` is work on the endpoint and not a flag
/// flip: the service acknowledges the request and the configuration changes status when
/// that work is done. The model carries no transitional status to report meanwhile, so
/// the configuration keeps the status it HAS until the change falls due. The span is the
/// emulator's own — AWS publishes no figure for it — chosen like every other transition
/// of this emulator: long enough that a caller acting in the same breath still meets the
/// state it had, short enough that a caller polling for the new status gets there at
/// once.
pub const DOMAIN_CONFIGURATION_STATUS_CHANGE_SPAN: Duration = Duration::from_secs(3);

impl RegisteredDomainConfiguration {
    /// Whether this configuration serves no connection — its persisted status is
    /// [`DOMAIN_CONFIGURATION_DISABLED`] — and so may be deleted.
    ///
    /// Read from the record rather than from the create request, so a configuration
    /// created `ENABLED` and later disabled is deletable and one enabled again is not.
    /// A disable that is asked for but not yet in effect is NOT disabled: settle the
    /// record against the clock ([`Self::settle_status`]) before asking.
    pub fn is_disabled(&self) -> bool {
        self.status == DOMAIN_CONFIGURATION_DISABLED
    }

    /// Ask for the status `status`, which comes into effect
    /// [`DOMAIN_CONFIGURATION_STATUS_CHANGE_SPAN`] after `now`.
    ///
    /// Asking for the status the configuration is already in, or already moving to,
    /// changes nothing: there is no new work to do, and so no new instant to report as
    /// the last status change.
    pub fn request_status(&mut self, status: String, now: SystemTime) {
        self.settle_status(now);
        let asked_for = self
            .pending_status
            .as_ref()
            .map_or(self.status.as_str(), |pending| pending.status.as_str());
        if asked_for == status {
            return;
        }
        self.pending_status = Some(PendingStatusChange {
            due: now + DOMAIN_CONFIGURATION_STATUS_CHANGE_SPAN,
            status,
        });
    }

    /// Fold a status change the clock has ALREADY made due into the record: the
    /// configuration is in that status from the instant the change fell due, which is
    /// also when it last changed status.
    ///
    /// Materialises only what the clock owed at `now` — a change that was due whether or
    /// not anybody called — so every reader sees the same status, whoever asks first.
    pub fn settle_status(&mut self, now: SystemTime) {
        let Some(pending) = self.pending_status.as_ref() else {
            return;
        };
        if now < pending.due {
            return;
        }
        self.status = pending.status.clone();
        self.last_status_change_date = pending.due;
        self.pending_status = None;
    }

    /// This record as of `now`: the same record with every status change the clock has
    /// made due folded in, which is what a reader of it reports.
    pub fn as_of(&self, now: SystemTime) -> Self {
        let mut settled = self.clone();
        settled.settle_status(now);
        settled
    }

    /// The record as one entry of the `DomainConfigurationSummary` list a LISTING hands
    /// back: the subset of members that shape declares, projected from the same stored
    /// record a single-configuration reader projects, so the two cannot drift.
    pub fn to_summary(&self) -> DomainConfigurationSummary {
        DomainConfigurationSummary {
            domain_configuration_name: Some(self.domain_configuration_name.clone()),
            domain_configuration_arn: Some(self.domain_configuration_arn.clone()),
            service_type: Some(self.service_type.clone()),
        }
    }

    /// The record as `DescribeDomainConfiguration` reports it — every member of the
    /// stored record, so the read and the create cannot drift.
    pub fn to_description(&self) -> DescribeDomainConfigurationOutput {
        DescribeDomainConfigurationOutput {
            domain_configuration_name: Some(self.domain_configuration_name.clone()),
            domain_configuration_arn: Some(self.domain_configuration_arn.clone()),
            domain_name: self.domain_name.clone(),
            server_certificates: Some(
                self.server_certificate_arns
                    .iter()
                    .map(|certificate_arn| ServerCertificateSummary {
                        server_certificate_arn: Some(certificate_arn.clone()),
                        server_certificate_status: Some(SERVER_CERTIFICATE_VALID.to_string()),
                        server_certificate_status_detail: None,
                    })
                    .collect(),
            ),
            authorizer_config: self.authorizer_config.clone(),
            domain_configuration_status: Some(self.status.clone()),
            service_type: Some(self.service_type.clone()),
            domain_type: Some(self.domain_type.clone()),
            last_status_change_date: Some(::aws_smithy_types::DateTime::from(
                self.last_status_change_date,
            )),
            tls_config: self.tls_config.clone(),
            server_certificate_config: self.server_certificate_config.clone(),
            authentication_type: self.authentication_type.clone(),
            application_protocol: self.application_protocol.clone(),
            client_certificate_config: self.client_certificate_config.clone(),
        }
    }
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// Every domain configuration this account holds under `region`, in the order a
    /// listing reports them: the ones the service manages for the account's own endpoints
    /// ([`RESERVED_DOMAIN_CONFIGURATIONS`], in the service's own order) and then the ones
    /// a caller CREATED, in name order — the order the store itself keeps.
    ///
    /// One view over both, so a reader and a listing report the same set and a reserved
    /// configuration is not a special case any caller of this has to remember. A caller
    /// cannot create over a reserved name — `CreateDomainConfiguration`'s
    /// `DomainConfigurationName` cannot spell one — so the two sets do not overlap.
    ///
    /// Each row carries the [`ListedDomainConfiguration::position`] a page marker names
    /// it by, because this order is not the NAME's: the service's own endpoints come
    /// first, in the service's order, so a marker carrying a name could not say where the
    /// page stopped.
    pub fn domain_configurations_of(
        &self,
        region: &str,
        data_endpoint: Option<&str>,
        now: SystemTime,
    ) -> Vec<ListedDomainConfiguration> {
        let reserved = self
            .reserved_domain_configurations
            .all(region, data_endpoint)
            .into_iter()
            .enumerate()
            .map(|(index, configuration)| ListedDomainConfiguration {
                position: format!("{RESERVED_POSITION_PREFIX}{index:04}"),
                configuration,
            });
        let created = self
            .domain_configurations
            .values()
            .map(|configuration| configuration.as_of(now))
            .map(|configuration| ListedDomainConfiguration {
                position: format!(
                    "{CREATED_POSITION_PREFIX}{}",
                    configuration.domain_configuration_name
                ),
                configuration,
            });
        reserved.chain(created).collect()
    }

    /// The domain configuration `domain_configuration_name` addresses in `region` —
    /// stored or reserved, as of `now` — or `None` for a name this account holds none
    /// under, which is what every reader refuses with
    /// [`missing_domain_configuration_message`].
    pub fn domain_configuration_of(
        &self,
        domain_configuration_name: &str,
        region: &str,
        data_endpoint: Option<&str>,
        now: SystemTime,
    ) -> Option<RegisteredDomainConfiguration> {
        self.domain_configurations
            .get(domain_configuration_name)
            .map(|configuration| configuration.as_of(now))
            .or_else(|| {
                self.reserved_domain_configurations
                    .resolve(domain_configuration_name, region, data_endpoint)
            })
    }
}

/// What a page marker names a configuration the SERVICE manages by: its place in
/// [`RESERVED_DOMAIN_CONFIGURATIONS`], under a prefix that sorts ahead of every
/// [`CREATED_POSITION_PREFIX`] position, which is what puts those configurations first.
const RESERVED_POSITION_PREFIX: &str = "0";

/// What a page marker names a configuration a CALLER created by: its name, under a prefix
/// that sorts after every [`RESERVED_POSITION_PREFIX`] position, so the positions of the
/// whole listing are ascending (what [`super::page_of`] pages over) and a position stays
/// with its row as the account's configurations come and go.
const CREATED_POSITION_PREFIX: &str = "1";

/// One row of the domain-configuration listing: the record, and the POSITION a page
/// marker names it by.
#[derive(Debug, Clone)]
pub struct ListedDomainConfiguration {
    /// Where the row is in the listing's total order — ascending across the whole
    /// listing, so one page resumes exactly where the last ended.
    pub position: String,
    /// The configuration itself, projected like any other.
    pub configuration: RegisteredDomainConfiguration,
}
