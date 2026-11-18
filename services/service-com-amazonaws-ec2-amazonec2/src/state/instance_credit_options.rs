// SPDX-License-Identifier: CC0-1.0

//! The credit option for CPU usage ONE instance runs its vCPUs under — the instance-level
//! half of the burstable performance credit model, beside the account-level default per
//! Region and family ([`ComAmazonawsEc2Amazonec2State::default_cpu_credits_for`]) a launch
//! takes it from.
//!
//! Only a burstable performance instance has an option of its own: "If you specify an
//! instance ID that is not a burstable performance instance, Amazon EC2 returns the standard
//! credit option" (`DescribeInstanceCreditSpecifications`), so an instance of any other type
//! holds none and is reported `standard`.
//!
//! The option is a fact of the INSTANCE from the moment it is launched
//! ([`ComAmazonawsEc2Amazonec2State::launched_cpu_credits`] resolves it once, onto
//! [`InstanceRecord::cpu_credits`]), not a reading of the family's default taken whenever
//! somebody asks: "instances launch using the default credit option"
//! (`ModifyDefaultCreditSpecification`), so a later modify of that default leaves an
//! already-launched instance where it was.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// The burstable performance family `instance_type` belongs to, or `None` when it belongs to
/// none.
///
/// The family is the part of a type's name before its size ([`instance_family_of`]), resolved
/// against the model's own `UnlimitedSupportedInstanceFamily` catalog
/// ([`unlimited_supported_instance_family`]) — the one list of the families whose instances
/// have a credit option at all, so a family the model gains is burstable here without this
/// file being touched.
pub fn burstable_instance_family(instance_type: &str) -> Option<&'static str> {
    unlimited_supported_instance_family(instance_family_of(instance_type))
}

/// The credit option an instance REPORTS: the one it runs under, or [`CPU_CREDITS_STANDARD`]
/// for an instance that holds none.
///
/// "If you specify an instance ID that is not a burstable performance instance, Amazon EC2
/// returns the standard credit option" (`DescribeInstanceCreditSpecifications`): a credit
/// option is a setting of a burstable performance instance, and an instance of any other type
/// spends no CPU credits at all, so `standard` is what it reports.
pub fn instance_cpu_credits(instance: &InstanceRecord) -> &'static str {
    instance.cpu_credits.unwrap_or(CPU_CREDITS_STANDARD)
}

/// The sentence EC2 answers a `CpuCredits` outside the documented pair with: it names the
/// parameter, the two values it requires and what to do about it, and quotes no value at
/// fault — which is why [`invalid_cpu_credits`] takes no argument.
///
/// A fixed sentence, as EC2 words this refusal; the pair it names is [`CPU_CREDITS_VALUES`],
/// which carries the documentation citation for it.
pub const INVALID_CPU_CREDITS_MESSAGE: &str = "The CpuCredits parameter requires a value of \
                                               either Standard or Unlimited. Change the value \
                                               and try again.";

/// Refuse a `CpuCredits` outside the documented pair the way EC2 does: HTTP 400
/// `InvalidParameterValue` with [`INVALID_CPU_CREDITS_MESSAGE`].
///
/// Shared, because the member is a plain `String` in the model on every request that carries
/// one — a launch's `CreditSpecificationRequest$CpuCredits` as much as the account default's
/// own — so nothing refuses a word outside the pair before a handler runs, and the operations
/// that resolve one cannot word that refusal two ways.
pub fn invalid_cpu_credits<E>() -> Scaffolded<E> {
    invalid_parameter_value(INVALID_CPU_CREDITS_MESSAGE)
}

impl ComAmazonawsEc2Amazonec2State {
    /// The credit option an instance of `instance_type`, launched in `region`, runs under:
    /// `requested` where its launch named one of the documented options, the account's default
    /// for the type's family in that Region otherwise — and `None` for a type that has no
    /// credit option at all.
    ///
    /// "Instances launch using the default credit option"
    /// (`ModifyDefaultCreditSpecification`), and that default "is set at the account level per
    /// Amazon Web Services Region, and is specified per instance family" — read here through
    /// the ONE reading of it ([`ComAmazonawsEc2Amazonec2State::default_cpu_credits_for`]), so
    /// a launch and `GetDefaultCreditSpecification` cannot disagree about which option a
    /// family launches under.
    ///
    /// `requested` is `&'static str` so only a documented option can be carried onto an
    /// instance ([`cpu_credits_value`] is what turns a request's word into one), never the
    /// request's own text.
    pub fn launched_cpu_credits(
        &self,
        region: &str,
        instance_type: &str,
        requested: Option<&'static str>,
    ) -> Option<&'static str> {
        let family = burstable_instance_family(instance_type)?;
        Some(requested.unwrap_or_else(|| self.default_cpu_credits_for(region, family)))
    }

    /// Put the credit option the instance `instance_id` runs under at `cpu_credits`, and
    /// answer the option it REPORTS from then on — `None` when this store holds no instance
    /// under that id.
    ///
    /// The one writer of [`InstanceRecord::cpu_credits`] after a launch: "modifies the credit
    /// option for CPU usage" (`ModifyInstanceCreditSpecification`) moves the option of the
    /// instance itself, which is why it is held on the record rather than re-read from the
    /// family's default per request. `cpu_credits` is a `&'static str` so only a documented
    /// option can reach an instance ([`cpu_credits_value`] is what turns a request's word into
    /// one), never the request's own text, and the answer is read back OUT of the record
    /// through the one projection every reader of it takes
    /// ([`instance_cpu_credits`]), so a writer reports exactly what a later
    /// `DescribeInstanceCreditSpecifications` will. Idempotent: setting an instance to the
    /// option it already holds changes nothing and answers the same option.
    pub fn set_instance_cpu_credits(
        &mut self,
        instance_id: &str,
        cpu_credits: &'static str,
    ) -> Option<&'static str> {
        let instance = self.instances.get_mut(instance_id)?;
        instance.cpu_credits = Some(cpu_credits);
        Some(instance_cpu_credits(instance))
    }
}
