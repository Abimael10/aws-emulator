// SPDX-License-Identifier: CC0-1.0

//! The `Test` side of this service's store.
//!
//! A test exists only WITHIN a service — `CreateTest` "creates a test for a service by
//! configuring a test template" and every operation that reaches one names the service's
//! ARN — so the records live inside the parent service's record
//! ([`super::ServiceRecord::tests`]), the way its assertions and service functions do:
//! removing the service removes its tests with it, and no lookup can reach a test whose
//! service is gone.
//!
//! Every `Test` body this crate returns is projected from a record here through
//! [`TestRecord::to_test`], so the writer and the readers cannot drift from one another or
//! from what was stored.
//!
//! Two readings a create has to make before it writes live here as well, so every
//! operation that configures a test makes them the same way:
//!
//! * [`catalog_template`] resolves the `testTemplateArn` a request configures against the
//!   service-owned catalog ([`super::TEST_TEMPLATE_CATALOG`]), because a template is not a
//!   caller's resource and an ARN of the modelled form says nothing about whether the
//!   service publishes the template it names.
//! * [`multi_az_rto_rejection`] refuses a create for a service whose resilience policy
//!   states no multi-AZ RTO: a test measures recovery against the recovery-time objective
//!   in force for the service, so without that objective there is nothing to measure
//!   against.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::{
    EffectivePolicyValues, LoggingConfiguration, ResourceNotFoundException, StopCondition, Test,
    ValidationException,
};

use super::{
    arn_resource_id, not_found_message, to_date_time, ServiceRecord, TestTemplateCatalogEntry,
    TEST_TEMPLATE_CATALOG, TEST_TEMPLATE_RESOURCE,
};

/// `resourceType` for a test — the `Test` shape's own name, which is what the type member
/// of a refusal reports. The prose a refusal's MESSAGE names the resource with is the same
/// word here, so one constant serves both.
pub const TEST_RESOURCE_TYPE: &str = "Test";

/// `resourceType` for a test template, as [`TEST_RESOURCE_TYPE`] is for a test: the
/// `TestTemplate` shape's own name.
pub const TEST_TEMPLATE_RESOURCE_TYPE: &str = "TestTemplate";

/// One test created on a service, and the sole source of every `Test` body this crate
/// returns.
///
/// What the caller CONFIGURED is kept exactly as the request stated it (the logging
/// destinations, the stop conditions, the execution role, the parameter values), because
/// those members are the test's configuration and a reader has to see the configuration it
/// asked for. Everything else is derived: the id is minted, the name is the configured
/// template's, and the two run counters start at zero because nothing has run the test yet.
#[derive(Debug, Clone)]
pub struct TestRecord {
    /// The minted `testId`; also this record's key in [`super::ServiceRecord::tests`].
    pub test_id: String,
    /// The ARN of the test template this test was created from, as the request configured
    /// it.
    pub test_template_arn: String,
    /// `Test$name` — "the name of the test". A test is created by configuring a template
    /// and the request states no name of its own, so the name is the TEMPLATE's, read off
    /// the catalog entry the request resolved to.
    pub name: String,
    pub logging_configuration: Option<LoggingConfiguration>,
    pub stop_conditions: Option<Vec<StopCondition>>,
    pub role_name: Option<String>,
    /// The configured parameter values. Held ordered so a read answers the same way every
    /// time, whatever order the map arrived in.
    pub parameters: Option<BTreeMap<String, Vec<String>>>,
    /// How many runs of this test there have been, and how many of them succeeded — both
    /// `@required` on `Test`. A freshly created test has had none of either; the operations
    /// that run a test are what move them.
    pub total_test_runs: i32,
    /// See [`Self::total_test_runs`].
    pub successful_test_runs: i32,
    pub created_at: SystemTime,
    /// Creation order, from the store's monotonic counter — the order a listing scans in,
    /// and the position a page token names.
    pub sequence: u64,
}

impl TestRecord {
    /// This record as the `Test` shape `CreateTest` and `GetTest` return.
    ///
    /// `serviceArn` is the ARN of the service this test is stored under — the test is
    /// nested in that service's record, so it is passed in rather than duplicated in every
    /// record.
    ///
    /// `actions` ("the fault actions the test runs") is left ABSENT: the fault actions of a
    /// test are the actions of the template it was created from, and this service's
    /// template catalog publishes none to derive them from, so reporting any would be
    /// reporting actions nothing backs.
    pub fn to_test(&self, service_arn: &str) -> Test {
        Test {
            test_id: self.test_id.clone(),
            test_template_arn: self.test_template_arn.clone(),
            service_arn: service_arn.to_string(),
            name: self.name.clone(),
            actions: None,
            logging_configuration: self.logging_configuration.clone(),
            stop_conditions: self.stop_conditions.clone(),
            role_name: self.role_name.clone(),
            parameters: self
                .parameters
                .as_ref()
                .map(|values| values.clone().into_iter().collect()),
            total_test_runs: self.total_test_runs,
            successful_test_runs: self.successful_test_runs,
            creation_time: to_date_time(self.created_at),
        }
    }
}

impl ServiceRecord {
    /// The test this service holds for the template whose catalog id is `template_id`, if
    /// any.
    ///
    /// "Each service has one test per template" (the modelled `CreateTest` documentation),
    /// so this is what a create consults before storing a second test for a template the
    /// service already has one for. Matched on the template's CATALOG id rather than on the
    /// ARN as a whole, for the reason [`catalog_template`] resolves one that way.
    pub fn test_for_template(&self, template_id: &str) -> Option<&TestRecord> {
        self.tests
            .values()
            .find(|test| template_id_of(&test.test_template_arn) == template_id)
    }
}

/// The catalog entry the `testTemplateArn` `arn` names, or `None` when this service
/// publishes no template under it.
///
/// Resolved on the template's catalog id — the part of the ARN's resource segment after
/// `test-template/` — because that id is which template the value names, while the
/// partition, Region and account segments say where it is addressed from: the catalog is
/// SERVICE-owned and the same template is published in every Region, so a request is not
/// configuring a different template by addressing it in another one.
pub fn catalog_template(arn: &str) -> Option<&'static TestTemplateCatalogEntry> {
    let template_id = template_id_of(arn);
    TEST_TEMPLATE_CATALOG
        .iter()
        .find(|entry| entry.resource_id == template_id)
}

/// The catalog id a test-template ARN carries: its resource segment with the
/// `test-template/` qualifier removed, which is how [`super::test_template_arn`] mints one.
///
/// A value whose resource segment carries no such qualifier has no id to take, so it is its
/// own: the caller sent it, and nothing of it can be dropped without inventing one.
fn template_id_of(arn: &str) -> &str {
    let resource_id = arn_resource_id(arn);
    resource_id
        .strip_prefix(TEST_TEMPLATE_RESOURCE)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(resource_id)
}

/// The refusal for a `testId` that names no stored test of its service.
///
/// A test is identified by that id within its service rather than by an ARN of its own, so
/// the id is both the `resourceId` and what the message names — the same shape the refusal
/// for an assertion or a service function takes.
pub fn test_not_found(test_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: not_found_message(TEST_RESOURCE_TYPE, test_id),
        resource_id: Some(test_id.to_string()),
        resource_type: Some(TEST_RESOURCE_TYPE.to_string()),
    }
}

/// The refusal for a `testTemplateArn` that names no template this service publishes.
///
/// The `resourceId` is the template's catalog id, the identifier the catalog itself holds
/// for the template, rather than the whole ARN whose partition, Region and account segments
/// say where the template would be addressed rather than which template it is — the same
/// reading every other refusal in this crate takes of an ARN.
pub fn test_template_not_found(arn: &str) -> ResourceNotFoundException {
    let template_id = template_id_of(arn);
    ResourceNotFoundException {
        message: not_found_message(TEST_TEMPLATE_RESOURCE_TYPE, template_id),
        resource_id: Some(template_id.to_string()),
        resource_type: Some(TEST_TEMPLATE_RESOURCE_TYPE.to_string()),
    }
}

/// The sentence a create for a service with no multi-AZ RTO in force is refused with.
///
/// Worded the way this service words its other refusal of a request whose SUBJECT is well
/// formed and whose surrounding state is not ready for it (see
/// [`super::unassumable_message`]): what cannot be done, around the one variable part the
/// request supplied — the service the test was to be created for — then the condition the
/// caller has to satisfy. The policy's own ARN and name are no part of it: the caller fixes
/// this by giving the service a policy that states the objective.
pub fn multi_az_rto_required_message(service_arn: &str) -> String {
    format!(
        "Cannot create test for service '{service_arn}'. Ensure the resilience policy \
         associated with the service defines a multi-AZ RTO."
    )
}

/// The refusal a request configuring a test for a service whose policy states no multi-AZ
/// RTO is answered with, or `None` when the objective is in force.
///
/// A test measures a service's recovery against the recovery-time objective in force for it
/// — the multi-AZ RTO of the resilience policy the service references, as
/// `EffectivePolicyValues` reports it — so a service with no such objective has nothing for
/// a test to be measured against and the create is refused with the modelled
/// `ValidationException` (HTTP 400) rather than stored. Read from the EFFECTIVE values, so
/// one reading covers every way the objective can be missing: a service with no policy
/// attached, one whose `policyArn` no longer resolves to a stored policy, and a policy that
/// states other targets but not this one.
pub fn multi_az_rto_rejection(
    service_arn: &str,
    effective: &EffectivePolicyValues,
) -> Option<ValidationException> {
    if effective
        .multi_az_rto
        .as_ref()
        .is_some_and(|target| target.value.is_some())
    {
        return None;
    }
    Some(ValidationException {
        message: multi_az_rto_required_message(service_arn),
        // `reason` classifies the VALUE of a request field (invalid, duplicated, missing);
        // nothing is wrong with any field of this request — it is the state of the policy
        // the service references that is not ready for a test — so none of the modelled
        // reasons states this refusal and the sentence carries it alone. `fieldList`
        // likewise carries a per-field sentence nothing fixes for this refusal.
        reason: None,
        field_list: None,
    })
}

/// Mint a fresh `testId`.
///
/// `TestId` is constrained `len 1..`, which a UUID from this crate's one minting path
/// satisfies; none of its bits comes from the request, so two tests configured from the
/// same template in different services never collide.
pub fn mint_test_id(region: &str) -> String {
    super::mint_uuid(region)
}
