// SPDX-License-Identifier: CC0-1.0

//! The connector profiles of this account: what `CreateConnectorProfile` stores, what
//! `UpdateConnectorProfile` re-configures, what `DescribeConnectorProfiles` reports and what
//! `DeleteConnectorProfile` takes away.
//!
//! A connector profile is "an instance of a connector. This includes the provided name,
//! credentials ARN, connection-mode, and so on" (`smithy_query(query="shape",
//! shape="ConnectorProfile")`), and it is NAMED: "the name is unique for each
//! `ConnectorProfile` in the Amazon Web Services account", and that name is the one handle
//! every call on a profile takes — the create, the update and the delete each address one by
//! `connectorProfileName`. So the name is the key this account's profiles are held under
//! ([`super::ComAmazonawsAppflowSandstoneconfigurationservicelambdaState::connector_profiles`]).
//!
//! What a read of a profile reports beyond the name is AppFlow's own: the ARN it minted for the
//! profile, the handle it holds the profile's credentials under, the connector-specific
//! PROPERTIES (never the credentials, which are sensitive and leave the service in no answer),
//! and the two instants it recorded. All of them are read off the stored record
//! ([`ConnectorProfileRecord::reported`]) rather than recomputed per call, so two reads of one
//! profile report the same values.

use std::time::SystemTime;

use crate::ops::types::{ConnectorProfile, ConnectorProfileProperties, RedshiftConnectorProfileProperties};

/// The listing a `DescribeConnectorProfiles` cursor belongs to.
pub const CONNECTOR_PROFILE_CURSOR: &str = "connector-profiles";

/// The page `DescribeConnectorProfiles` serves a caller that names no `maxResults`
/// (`smithy_query(query="doc", shape="DescribeConnectorProfilesRequest", member="maxResults")`:
/// "Specifies the maximum number of items that should be returned in the result set. The
/// default for maxResults is 20 (for all paginated API operations)").
pub const DEFAULT_CONNECTOR_PROFILE_PAGE_SIZE: usize = 20;

/// The most connector profiles one account may hold, which `CreateConnectorProfile` refuses to
/// exceed with the modelled `ServiceQuotaExceededException`.
///
/// DOCUMENTED: "There is a soft quota of 100 connector profiles per Amazon Web Services
/// account. If you need more connector profiles than this quota allows, you can submit a
/// request to the Amazon AppFlow team through the Amazon AppFlow support channel"
/// (`smithy_query(query="doc", shape="CreateConnectorProfile")`). It also bounds
/// [`super::ComAmazonawsAppflowSandstoneconfigurationservicelambdaState::connector_profiles`],
/// so a long-lived process cannot grow without limit.
pub const MAX_CONNECTOR_PROFILES_PER_ACCOUNT: usize = 100;

/// How AppFlow names the request it refused, in the envelope a definition-time refusal's
/// message opens with ([`super::request_failed`]) — the same envelope the Amazon S3
/// pre-flight of a flow definition is stated in, so one service words one class of refusal one
/// way.
pub const CREATE_CONNECTOR_PROFILE_REQUEST: &str = "Create Connector Profile";
/// See [`CREATE_CONNECTOR_PROFILE_REQUEST`].
pub const UPDATE_CONNECTOR_PROFILE_REQUEST: &str = "Update Connector Profile";

/// One connector profile of this account, as the two writes stored it.
///
/// The CREDENTIALS are held here and reported in no answer: `ConnectorProfile` carries no
/// credential member at all, only the [`ConnectorProfileRecord::credentials_arn`] handle AppFlow
/// holds them under, so a read of a profile cannot disclose them. They are kept because the
/// profile is the instance of the connector the credentials belong to: an update that restates
/// only the properties leaves the credentials the create stored in force.
#[derive(Debug, Clone)]
pub struct ConnectorProfileRecord {
    /// The ARN AppFlow minted for the profile ([`connector_profile_arn`]), which is what the
    /// create and the update answer with.
    pub connector_profile_arn: String,
    /// The name the profile is held under, which is its identity in this account.
    pub connector_profile_name: String,
    /// The `connectorType` the profile is an instance of.
    pub connector_type: String,
    /// The `connectorLabel` of the registered connector this profile is for, for a
    /// `CustomConnector` profile; absent for every connector AppFlow vends.
    pub connector_label: Option<String>,
    /// Whether flows using the profile reach the application over the public internet or over
    /// AWS PrivateLink (`ConnectionMode`).
    pub connection_mode: String,
    /// The handle AppFlow holds one profile's credentials under, as the `credentialsArn` a read
    /// of the profile reports: the ARN Secrets Manager allocated for the secret
    /// [`super::mint_connector_profile_credentials_secret`] created there, never a value this
    /// crate built — a handle a client reads has to resolve where the client would look it up.
    pub credentials_arn: String,
    /// The connector-specific properties of the profile configuration, exactly as the last
    /// write stated them.
    pub connector_profile_properties: ConnectorProfileProperties,
    /// The connector-specific credentials the last write that carried any stated. Never
    /// reported, and never what a flow reads: the credential material a flow uses is in the
    /// secret [`ConnectorProfileRecord::credentials_arn`] names. This is the CONFIGURATION the
    /// last write stated, which is what answers a retried create carrying the same
    /// `@idempotencyToken` ([`connector_profile_descriptor`]).
    pub connector_profile_credentials: Option<crate::ops::types::ConnectorProfileCredentials>,
    /// The KMS key the profile's credentials are encrypted with: the one the create named, or
    /// the AppFlow-managed key of the account
    /// ([`super::ComAmazonawsAppflowSandstoneconfigurationservicelambdaState::managed_kms_key_arn`]).
    pub kms_arn: String,
    /// The opaque id Secrets Manager holds this profile's credentials secret under the NAME
    /// built from ([`connector_profile_credentials_secret_name`]): minted once for the profile,
    /// so a re-valued secret keeps the name and the ARN the create answered.
    pub credentials_secret_id: String,
    /// The `@idempotencyToken` of the `CreateConnectorProfile` that stored this profile, when it
    /// carried one. Held HERE rather than in a store-wide map, so the token is reclaimed by
    /// `DeleteConnectorProfile` with the profile it identifies.
    pub client_token: Option<String>,
    /// The `@idempotencyToken` of the LAST `UpdateConnectorProfile` that re-configured this
    /// profile, when it carried one. Held beside the configuration that token applied, so a
    /// retry of that same update is answered without re-applying it. Separate from
    /// [`ConnectorProfileRecord::client_token`], which belongs to the create: the two
    /// operations are different calls and their tokens are their own.
    pub last_update_client_token: Option<String>,
    pub created_at: SystemTime,
    pub last_updated_at: SystemTime,
}

impl ConnectorProfileRecord {
    /// The profile as `DescribeConnectorProfiles` reports it: every member of the modelled
    /// `ConnectorProfile` this service holds a value for.
    ///
    /// `privateConnectionProvisioningState` is reported for NO profile: it "[s]pecifies the
    /// private connection provisioning state", and this emulator provisions no AWS PrivateLink
    /// connection for a `Private` profile — so reporting a state for one would be a status for
    /// work nothing performed. The member is optional, and a profile over the public internet
    /// has no private connection to report either.
    pub fn reported(&self) -> ConnectorProfile {
        ConnectorProfile {
            connector_profile_arn: Some(self.connector_profile_arn.clone()),
            connector_profile_name: Some(self.connector_profile_name.clone()),
            connector_type: Some(self.connector_type.clone()),
            connector_label: self.connector_label.clone(),
            connection_mode: Some(self.connection_mode.clone()),
            credentials_arn: Some(self.credentials_arn.clone()),
            connector_profile_properties: Some(self.connector_profile_properties.clone()),
            created_at: Some(super::to_date_time(self.created_at)),
            last_updated_at: Some(super::to_date_time(self.last_updated_at)),
            private_connection_provisioning_state: None,
        }
    }
}

/// A connector profile's ARN:
/// `arn:aws:appflow:<region>:<account>:connectorprofile/<connectorProfileName>`.
///
/// The name is what identifies one `ConnectorProfile` in an account, so it is the resource part
/// of the ARN, built the same way a flow's is ([`super::flow_arn`]). The result satisfies
/// `ConnectorProfileArn`'s `^arn:aws:appflow:.*:[0-9]+:`
/// (`smithy_query(query="shape", shape="ConnectorProfileArn")`).
pub fn connector_profile_arn(region: &str, connector_profile_name: &str) -> String {
    ::emulator_server::emulator_arn(
        "appflow",
        region,
        &format!("connectorprofile/{connector_profile_name}"),
    )
}

/// Why an IAM role a connector profile's configuration names is one AppFlow could not use, as
/// the sentence the refusal carries — or `None` when every role it names is one AppFlow could
/// take on.
///
/// The principal AppFlow takes such a role on AS is the SAME one a bucket policy has to grant a
/// flow's reads and writes to ([`super::APPFLOW_SERVICE_PRINCIPAL`]), because AppFlow acts on the
/// caller's resources through its own service principal rather than as the caller.
///
/// The ROLE AppFlow itself assumes is the Data API role of an Amazon Redshift profile: "the
/// Amazon Resource Name (ARN) of an IAM role that permits Amazon AppFlow to access your Amazon
/// Redshift database through the Data API" (`smithy_query(query="doc",
/// shape="RedshiftConnectorProfileProperties", member="dataApiRoleArn")`). A role whose trust
/// policy does not admit [`appflow_service_principal`] is a role AppFlow cannot take on, so a
/// profile naming it could never reach the database — it is refused when the profile is defined
/// rather than stored and left to fail every flow.
///
/// The profile's other role is NOT judged here: `roleArn` is "the ARN of IAM role that grants
/// Amazon Redshift read-only access to Amazon S3", which Amazon Redshift assumes rather than
/// AppFlow, so what its trust policy says about AppFlow decides nothing.
///
/// A refusal is grounded in what IAM ANSWERED about the role
/// ([`::service_common::iam_role::role_is_unassumable`]): this account holds no role under the
/// name the ARN carries, or the role it holds carries a trust policy that does not admit that
/// principal. A client creates the Data API role before it creates the profile that names it —
/// that is the order the published setup prescribes — so a role this account's IAM holds nothing
/// under is a role AppFlow could not take on either. Nothing is refused on no answer at all: a
/// value naming no role of this account, and an emulator with no IAM to ask, establish nothing.
///
/// Shared by every operation that stores a configuration (`CreateConnectorProfile`,
/// `UpdateConnectorProfile`), so one cannot accept a profile the other would refuse; `request`
/// is how the refusal names the call it refused ([`CREATE_CONNECTOR_PROFILE_REQUEST`],
/// [`UPDATE_CONNECTOR_PROFILE_REQUEST`]).
pub async fn unusable_connector_profile_role(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    request: &str,
    properties: &ConnectorProfileProperties,
) -> Option<String> {
    let role_arn = properties
        .redshift
        .as_ref()
        .and_then(|redshift: &RedshiftConnectorProfileProperties| {
            redshift.data_api_role_arn.as_deref()
        })
        .filter(|role_arn| !role_arn.is_empty())?;
    if ::service_common::iam_role::role_is_unassumable(
        ctx,
        region,
        role_arn,
        super::APPFLOW_SERVICE_PRINCIPAL,
    )
    .await
    {
        return Some(super::request_failed(request, &unassumable_role(role_arn)));
    }
    None
}

/// What the caller has to repair, named as AppFlow names it: the role it could not take on and
/// the principal that role's trust policy has to admit.
///
/// The wording stays inside the pattern every message this service words has to satisfy in the
/// contexts it is re-stated in, so it carries no comma, colon or apostrophe. It states only what
/// was CHECKED — what the account's IAM answered about the role — and claims no connection
/// attempt, no query and no round trip to the warehouse, none of which this emulator makes.
pub fn unassumable_role(role_arn: &str) -> String {
    format!(
        "Amazon AppFlow is not authorized to assume the role [{role_arn}]. Update the trust \
         policy of the role so that it can be assumed by the [{principal}] service principal",
        principal = super::APPFLOW_SERVICE_PRINCIPAL
    )
}

/// The message of the `ConflictException` a `CreateConnectorProfile` is refused with when the
/// account already holds a profile of that name — "the name is unique for each
/// `ConnectorProfile` in the Amazon Web Services account".
///
/// Worded like the conflict a second flow of one name is refused with
/// ([`super::flow_name_already_exists`]), so one service words one class of refusal one way.
pub fn connector_profile_name_already_exists(connector_profile_name: &str) -> String {
    super::conflict(&format!(
        "Connector Profile with name {connector_profile_name} already exists."
    ))
}

/// A fresh opaque id for the secret one profile's credentials are held under, from the same
/// uuid-backed generator every other opaque id this service mints comes from
/// ([`super::new_execution_id`]).
pub fn new_credentials_secret_id() -> String {
    super::new_execution_id()
}

/// The message of the `ConflictException` an unforced `DeleteConnectorProfile` is refused with
/// while a flow of this account still names the profile, naming the flow that uses it.
///
/// "Indicates whether Amazon AppFlow should delete the profile, even if it is currently in use
/// in one or more flows" (`smithy_query(query="doc",
/// shape="DeleteConnectorProfileRequest", member="forceDelete")`), so an unforced delete of a
/// profile in use is the conflict — worded like the one an unforced `DeleteFlow` of an active
/// flow is refused with ([`super::flow_is_active`]).
pub fn connector_profile_is_in_use(connector_profile_name: &str, flow_name: &str) -> String {
    super::conflict(&format!(
        "Connector Profile {connector_profile_name} is in use by the flow {flow_name}, please \
         set forceDelete to true or delete the flow first."
    ))
}

/// The configuration one `CreateConnectorProfile` carries, as one comparable value: every
/// member the operation persists, with the connector-specific configuration rendered by the
/// service's own codec.
///
/// It exists for ONE comparison — the contract's "If you specify input parameters that differ
/// from your first request, an error occurs" — and `serde_json::Value`'s own equality decides
/// it. Built on demand for that comparison, never stored and never put on the wire, exactly as
/// a flow's definition descriptor is ([`super::definition_descriptor`]).
///
/// `None` when the codec cannot render the configuration: the comparison is then impossible
/// rather than false, which the caller reports as a server fault instead of blaming the
/// request.
pub fn connector_profile_descriptor(
    connector_profile_name: &str,
    connector_type: &str,
    connector_label: Option<&str>,
    connection_mode: &str,
    kms_arn: &str,
    connector_profile_config: &crate::ops::types::ConnectorProfileConfig,
) -> Option<::serde_json::Value> {
    let config = super::rendered_shape(
        &crate::ops::types::ConnectorProfileConfig::SCHEMA,
        connector_profile_config,
    )?;
    Some(::serde_json::json!({
        "connectorProfileName": connector_profile_name,
        "connectorType": connector_type,
        "connectorLabel": connector_label,
        "connectionMode": connection_mode,
        "kmsArn": kms_arn,
        "connectorProfileConfig": config,
    }))
}
