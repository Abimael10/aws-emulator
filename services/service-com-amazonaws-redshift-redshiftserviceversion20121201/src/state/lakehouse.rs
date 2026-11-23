// SPDX-License-Identifier: CC0-1.0

//! The lakehouse configuration a cluster carries: whether the cluster is
//! registered with Amazon Redshift federated permissions, the Glue data catalog
//! that registration is against, and the IAM Identity Center application whose
//! trusted identity propagation it is associated with.
//!
//! Two of the three are MODELLED members of the cluster
//! (`Cluster$LakehouseRegistrationStatus` and `$CatalogArn`), so they live on the
//! cluster's own record and a `DescribeClusters` reports what a
//! `ModifyLakehouseConfiguration` wrote; the third has no `Cluster` member and is
//! held beside it ([`StoredCluster::lakehouse_idc_application_arn`]). One rule
//! renders all three into the configuration a response reports
//! ([`ComAmazonawsRedshiftRedshiftserviceversion20121201State::lakehouse_configuration`]),
//! so the write path and the read path cannot report a different configuration.

use super::*;

use crate::ops::types::ModifyLakehouseConfigurationOutput;

/// `LakehouseRegistration` asking for the cluster to be registered with Amazon
/// Redshift federated permissions.
///
/// Source: `smithy_query(query="shape", shape="LakehouseRegistration")`, whose
/// values are `Register` and `Deregister`.
pub const LAKEHOUSE_REGISTER: &str = "Register";

/// `LakehouseRegistration` asking for that registration to be withdrawn. See
/// [`LAKEHOUSE_REGISTER`].
pub const LAKEHOUSE_DEREGISTER: &str = "Deregister";

/// `LakehouseIdcRegistration` asking for the Identity Center application to be
/// associated with the cluster.
///
/// Source: `smithy_query(query="shape", shape="LakehouseIdcRegistration")`, whose
/// values are `Associate` and `Disassociate`.
pub const LAKEHOUSE_IDC_ASSOCIATE: &str = "Associate";

/// `LakehouseIdcRegistration` asking for that association to be withdrawn. See
/// [`LAKEHOUSE_IDC_ASSOCIATE`].
pub const LAKEHOUSE_IDC_DISASSOCIATE: &str = "Disassociate";

/// `Cluster$LakehouseRegistrationStatus` of a cluster that IS registered with
/// Amazon Redshift federated permissions.
///
/// The member is "the current status of the lakehouse registration. Indicates
/// whether the cluster is successfully registered with the lakehouse"
/// (`smithy_query(query="shape", shape="LakehouseConfiguration")`) and the model
/// publishes no value list for it, so the vocabulary is the recorded one: a
/// state token spelled in upper case with words joined by underscores, which is
/// what a client's poll loop compares against. This is the TERMINAL status of a
/// registration ([`LAKEHOUSE_REGISTER`]).
pub const LAKEHOUSE_REGISTRATION_REGISTERED: &str = "REGISTERED";

/// `Cluster$LakehouseRegistrationStatus` while the registration a
/// [`LAKEHOUSE_REGISTER`] asked for is still being made.
///
/// The registration is not immediate: the status moves through this transient
/// value and settles on [`LAKEHOUSE_REGISTRATION_REGISTERED`] once
/// [`LAKEHOUSE_REGISTRATION_SPAN`] has passed, which is what a client polling
/// for the terminal status waits out.
pub const LAKEHOUSE_REGISTRATION_REQUESTED: &str = "REGISTRATION_REQUESTED";

/// `Cluster$LakehouseRegistrationStatus` of a cluster whose registration has been
/// withdrawn. See [`LAKEHOUSE_REGISTRATION_REGISTERED`].
pub const LAKEHOUSE_REGISTRATION_DEREGISTERED: &str = "DEREGISTERED";

/// `Cluster$LakehouseRegistrationStatus` while the withdrawal a
/// [`LAKEHOUSE_DEREGISTER`] asked for is still being made, spelled in the same
/// vocabulary as [`LAKEHOUSE_REGISTRATION_REQUESTED`] because it is the same
/// kind of token for the other direction.
pub const LAKEHOUSE_DEREGISTRATION_REQUESTED: &str = "DEREGISTRATION_REQUESTED";

/// How long after the call that asks for it a lakehouse registration — or its
/// withdrawal — falls due.
///
/// The registration is made asynchronously, so the span is the same one every
/// other transition of this service is compressed into ([`PROVISIONING_SPAN`]):
/// Amazon Redshift documents no duration for it, the span is deliberately
/// non-zero so the caller still observes the transient status its own call
/// reports, and the due instant is STORED rather than slept for.
pub const LAKEHOUSE_REGISTRATION_SPAN: Duration = PROVISIONING_SPAN;

/// What a `DryRun` request that would have succeeded is refused with.
///
/// `ModifyLakehouseConfigurationMessage$DryRun` is "a boolean value that, if
/// true, validates the request without actually modifying the lakehouse
/// configuration. Use this to check for errors before making changes", and the
/// model declares no error for it: the validated outcome is signalled as a
/// REFUSAL carrying the error code every Amazon Web Services dry run is refused
/// with ([`crate::scaffolded::Scaffolded::dry_run_operation`]) and this fixed
/// sentence, which has no variable part — so a probe can tell a request that
/// would have succeeded from one a precondition refuses.
pub const DRY_RUN_WOULD_HAVE_SUCCEEDED: &str =
    "Request would have succeeded, but DryRun flag is set.";

/// The lakehouse registration change a `ModifyLakehouseConfiguration` started
/// and the clock has not finished yet.
///
/// Held on the cluster the change is against
/// ([`StoredCluster::lakehouse_registration`]) so the status a read reports is
/// the one the clock already owes — see
/// [`StoredCluster::settle_lakehouse_registration`].
#[derive(Debug, Clone)]
pub struct PendingLakehouseRegistration {
    /// When the move to [`PendingLakehouseRegistration::settles_to`] falls due.
    pub settles_at: Instant,
    /// The terminal `LakehouseRegistrationStatus` this change settles on:
    /// [`LAKEHOUSE_REGISTRATION_REGISTERED`] or
    /// [`LAKEHOUSE_REGISTRATION_DEREGISTERED`].
    pub settles_to: String,
}

impl StoredCluster {
    /// Start the lakehouse registration change `registration` asks for, at
    /// `now`.
    ///
    /// The cluster reports the TRANSIENT status from this instant — which is
    /// what the call that asked for the change answers — and the terminal one
    /// from [`LAKEHOUSE_REGISTRATION_SPAN`] later, whether or not anybody calls
    /// again.
    pub fn start_lakehouse_registration(&mut self, registration: &str, now: Instant) {
        let registering = registration == LAKEHOUSE_REGISTER;
        self.cluster.lakehouse_registration_status = Some(
            if registering {
                LAKEHOUSE_REGISTRATION_REQUESTED
            } else {
                LAKEHOUSE_DEREGISTRATION_REQUESTED
            }
            .to_string(),
        );
        self.lakehouse_registration = Some(PendingLakehouseRegistration {
            settles_at: now + LAKEHOUSE_REGISTRATION_SPAN,
            settles_to: if registering {
                LAKEHOUSE_REGISTRATION_REGISTERED
            } else {
                LAKEHOUSE_REGISTRATION_DEREGISTERED
            }
            .to_string(),
        });
    }

    /// Materialise the lakehouse registration change the clock already owes at
    /// `now`.
    ///
    /// Called by the store's settle, so every read of the cluster reports the
    /// status the registration has reached by the instant it is read and no
    /// caller has to ask twice for a change that was already due.
    pub fn settle_lakehouse_registration(&mut self, now: Instant) {
        let Some(pending) = self.lakehouse_registration.as_ref() else {
            return;
        };
        if now < pending.settles_at {
            return;
        }
        let settled = pending.settles_to.clone();
        self.lakehouse_registration = None;
        self.cluster.lakehouse_registration_status = Some(settled);
    }
}

/// The ARN namespace of the Glue data catalog a lakehouse registration is
/// against.
///
/// Source: `smithy_query(query="doc", shape="ModifyLakehouseConfigurationMessage",
/// member="CatalogName")` — "the name of the Glue data catalog that will be
/// associated with the cluster" — so the catalog is a resource of Glue, named
/// under that service's own namespace, exactly as `CreateCluster` derives the
/// `CatalogArn` of the catalog its own `CatalogName` names.
pub const GLUE_ARN_NAMESPACE: &str = "glue";

/// The ARN of the Glue data catalog named `catalog_name`, in `region`.
///
/// One rule for every path that reports a cluster's `CatalogArn`, so the create
/// path and the lakehouse-configuration path cannot name the same catalog
/// differently.
#[must_use]
pub fn catalog_arn(catalog_name: &str, region: &str) -> String {
    ::engine::emulator_arn(
        GLUE_ARN_NAMESPACE,
        region,
        &format!("catalog/{catalog_name}"),
    )
}

impl ComAmazonawsRedshiftRedshiftserviceversion20121201State {
    /// Whether `application_arn` names an IAM Identity Center application this
    /// account registered with Amazon Redshift whose trusted identity
    /// propagation a cluster can be associated with.
    ///
    /// `LakehouseIdcApplicationArn` is "the ARN of the IAM Identity Center
    /// application used for enabling Amazon Web Services IAM Identity Center
    /// trusted identity propagation on a cluster" — the IDENTITY CENTER
    /// application, which a Redshift IdC application record carries as its
    /// `IdcManagedApplicationArn`, beside the ARN of the Redshift application
    /// that manages it (`RedshiftIdcApplicationArn`). Either ARN names the same
    /// registered application, so either resolves.
    ///
    /// Resolving is not existence alone: an application still onboarding
    /// propagates no identity yet, so the record also has to report the
    /// onboarding finished ([`REDSHIFT_IDC_ONBOARD_STATUS`]) — which is what the
    /// refusal this answers states
    /// ([`RedshiftIdcApplicationNotExistsWording::ArnWasNotFoundOrStatusNotCompleted`]).
    #[must_use]
    pub fn identity_center_application_associable(&self, application_arn: &str) -> bool {
        self.redshift_idc_applications
            .iter()
            .filter(|application| {
                application.idc_managed_application_arn.as_deref() == Some(application_arn)
                    || application.redshift_idc_application_arn.as_deref() == Some(application_arn)
            })
            .any(|application| {
                application.idc_onboard_status.as_deref() == Some(REDSHIFT_IDC_ONBOARD_STATUS)
            })
    }

    /// The lakehouse configuration the cluster at `position` carries, as a
    /// response reports it.
    ///
    /// Every value is read off the cluster's own stored record, so what this
    /// reports and what a `DescribeClusters` reports for the same cluster cannot
    /// disagree.
    #[must_use]
    pub fn lakehouse_configuration(&self, position: usize) -> ModifyLakehouseConfigurationOutput {
        let stored = &self.clusters[position];
        ModifyLakehouseConfigurationOutput {
            cluster_identifier: stored.cluster.cluster_identifier.clone(),
            lakehouse_idc_application_arn: stored.lakehouse_idc_application_arn.clone(),
            lakehouse_registration_status: stored.cluster.lakehouse_registration_status.clone(),
            catalog_arn: stored.cluster.catalog_arn.clone(),
        }
    }
}
