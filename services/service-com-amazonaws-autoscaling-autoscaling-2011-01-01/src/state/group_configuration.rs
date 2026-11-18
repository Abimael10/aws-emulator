// SPDX-License-Identifier: CC0-1.0

//! The rules a request that CONFIGURES an Auto Scaling group is taken against, beyond the
//! ones the model can state: which launch source the request may name, whether the places
//! and the resources it names exist, and the cross-member rules of the policies the group
//! carries.
//!
//! Every one of them is a rule about the REQUEST, decided before anything is stored, and
//! every one is shared by `CreateAutoScalingGroup` and `UpdateAutoScalingGroup`: the two
//! operations write the same group, so a configuration one of them refuses is not a
//! configuration the other may store. This module is the one place each rule is spelled, for
//! the reason [`super::iam_instance_profiles`] is one place: a rule stated twice is a rule
//! two handlers can disagree about.
//!
//! The references a group's launch source carries are resolved against the stores of the
//! services that OWN them — the Availability Zones and the launch templates are Amazon
//! EC2's — so a group this service stores can always launch what it promises, and
//! `DescribeAutoScalingGroups` never advertises a resource Amazon EC2 does not have.
//!
//! LOCKING. The one pass that resolves a launch template takes Amazon EC2's store lock and
//! releases it before answering; the zone catalog needs no lock at all. Callers in
//! `src/ops/` ask these questions BEFORE taking this service's own lock, so no two stores
//! are ever held at once, and Amazon EC2 never reaches this service, so no cycle exists.

use ::std::collections::BTreeSet;

use ::service_com_amazonaws_ec2_amazonec2::scaffolded::Scaffolded as Ec2Refusal;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::ops::types::{
    InstanceLifecyclePolicy, InstanceMaintenancePolicy, LaunchTemplateSpecification,
    MixedInstancesPolicy,
};
use crate::scaffolded::Scaffolded;

/// The sentence Amazon EC2 Auto Scaling opens every refusal of a launch source it could not
/// use with, followed by the detail that says what about it could not be used.
///
/// The service validates the launch source by asking Amazon EC2 to resolve it, so the detail
/// is Amazon EC2's own wording for the value that did not resolve and this prefix is Auto
/// Scaling's own — the same two-part sentence [`super::iam_instance_profiles`] answers for an
/// instance profile a template names and nothing holds.
const INVALID_LAUNCH_SOURCE: &str = "You must use a valid fully-formed launch template.";

/// What Amazon EC2 Auto Scaling answers a request that does not name exactly ONE launch
/// source.
///
/// Fixed text with no variable part: it names the four members, one of which a valid request
/// carries, and says nothing about which of them this request sent. The rule is about the
/// REQUEST's members alone, so it is decided before any of the values is looked up — a
/// request naming two of them is refused for naming two, whether or not either resolves.
pub const ONE_LAUNCH_SOURCE_MESSAGE: &str = "Valid requests must contain either LaunchTemplate, \
                                             LaunchConfigurationName, InstanceId or \
                                             MixedInstancesPolicy parameter.";

/// What Amazon EC2 Auto Scaling answers a request that states one healthy-percentage
/// threshold and not the other.
///
/// "Both `MinHealthyPercentage` and `MaxHealthyPercentage` must be specified"
/// (`smithy_query(query="shape",shape="InstanceMaintenancePolicy")`): fixed text naming the
/// two members and no value of the request.
pub const HEALTHY_PERCENTAGE_PAIR_MESSAGE: &str =
    "Both MinHealthyPercentage and MaxHealthyPercentage must be specified or not at all.";

/// The same sentence for a request that CHANGES a pair the group already carries, where a
/// threshold may also be CLEARED.
///
/// "To clear a previously set value, specify a value of `-1`" (the same members): a threshold
/// set to [`RESET_PERCENTAGE`] states none, so the rule holds over the thresholds that remain
/// and the service says so in the parenthesis. A request that states the pair for the first
/// time has nothing to clear, and is answered [`HEALTHY_PERCENTAGE_PAIR_MESSAGE`] instead.
pub const HEALTHY_PERCENTAGE_PAIR_RESET_MESSAGE: &str =
    "Both MinHealthyPercentage and MaxHealthyPercentage must be specified (excluding -1) or \
     not at all.";
/// The same rule again as the preferences of ONE instance refresh are answered it.
///
/// The preferences carry the same two members (`RefreshPreferences$MinHealthyPercentage` and
/// `$MaxHealthyPercentage`) under the same rule, and the service words it for them as an
/// instruction about the request it just refused: which member to add and which to remove.
pub const HEALTHY_PERCENTAGE_PAIR_REFRESH_MESSAGE: &str =
    "MinHealthyPercentage is required to use MaxHealthyPercentage. Specify MinHealthyPercentage, \
     or remove MaxHealthyPercentage, and try again.";

/// Whether a request STATES a pair of healthy-percentage thresholds for the first time,
/// CHANGES a pair that is already stored, or states the pair as the preferences of one
/// instance refresh — which is the only difference between the sentences the service answers
/// the pair rule with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthyThresholds {
    /// A create, which states the pair outright: there is no previously set value to clear.
    Created,
    /// An update, which changes the pair a group already carries, where a threshold may be
    /// cleared with [`RESET_PERCENTAGE`].
    Changed,
    /// The preferences of one instance refresh, which state the pair for that refresh alone.
    Refreshed,
}

impl HealthyThresholds {
    /// The sentence the service answers the pair rule with for this kind of request.
    #[must_use]
    pub fn pair_message(self) -> &'static str {
        match self {
            Self::Created => HEALTHY_PERCENTAGE_PAIR_MESSAGE,
            Self::Changed => HEALTHY_PERCENTAGE_PAIR_RESET_MESSAGE,
            Self::Refreshed => HEALTHY_PERCENTAGE_PAIR_REFRESH_MESSAGE,
        }
    }
}

/// What it answers a request whose two healthy-percentage thresholds are further apart than
/// the service allows.
///
/// "the difference between them cannot be greater than 100" (the same member's
/// documentation), which is the bound this sentence states. Fixed text: it names the two
/// members and the bound, and no value of the request.
pub const HEALTHY_PERCENTAGE_SPREAD_MESSAGE: &str =
    "The difference between MaxHealthyPercentage and MinHealthyPercentage must be less than or \
     equal to 100.";

/// The widest spread the two healthy-percentage thresholds of one request may have, from
/// that member's own documentation (see [`HEALTHY_PERCENTAGE_SPREAD_MESSAGE`]).
pub const MAX_HEALTHY_PERCENTAGE_SPREAD: i32 = 100;

/// The value either healthy-percentage threshold carries to CLEAR a previously set one: "To
/// clear a previously set value, specify a value of `-1`"
/// (`smithy_query(query="shape",shape="InstanceMaintenancePolicy")`).
///
/// A threshold being reset states no threshold, so the cross-field rules below are about the
/// thresholds that remain.
pub const RESET_PERCENTAGE: i32 = -1;

/// The `RetentionAction` that keeps an instance instead of terminating it: "set to `retain`
/// to move instances to a retained state"
/// (`smithy_query(query="shape",shape="RetentionAction")`, whose values are `retain` and
/// `terminate`).
pub const RETENTION_ACTION_RETAIN: &str = "retain";

/// What Amazon EC2 Auto Scaling answers a forced delete of a group whose abandoned-terminate
/// hook retains its instances.
///
/// Fixed text with no variable part: it names neither the group nor the trigger's value,
/// because the trigger it names is the only one that can hold the delete, and it closes with
/// the change the caller has to make.
pub const FORCE_DELETE_RETAINS_INSTANCES_MESSAGE: &str =
    "You can't force delete this Auto Scaling group because the TerminateHookAbandon retention \
     trigger is set to retain. Change the retention trigger to terminate and try again.";

/// The refusal for an Availability Zone (or Availability Zone id) the Region does not have.
///
/// The value the request supplied is the one variable part, in the brackets the service puts
/// it in, after the launch-source sentence every refusal of a group's placement and launch
/// source opens with ([`INVALID_LAUNCH_SOURCE`]).
#[must_use]
pub fn invalid_availability_zone_message(zone: &str) -> String {
    format!("{INVALID_LAUNCH_SOURCE} Invalid availability zone: [{zone}]")
}

/// The refusal for a request whose group would be placed in a zone its Region does not have,
/// or `None` when every zone it names is one of the Region's own.
///
/// A group places instances in the Availability Zones it names, and a zone exists in exactly
/// one Region, so a zone of no Region is a placement no instance could ever be launched into:
/// the request is refused instead of stored. Which zones a Region has is Amazon EC2's own
/// catalog ([`ec2::availability_zones_in_region`]) — the same one its subnets are placed from
/// and the same one [`super::ec2_subnets`] resolves a subnet's zone against — so one zone is
/// never known to two services differently.
///
/// Both spellings are resolved against that catalog: `AvailabilityZones` names a zone by its
/// NAME and `AvailabilityZoneIds` by its ID, and a value that is neither a name nor an id of
/// the Region names nothing. Zones are resolved in the order the request wrote them, so the
/// first value that names nothing is the one the refusal is about.
pub fn unknown_availability_zone_error<E>(
    region: &str,
    availability_zones: Option<&[String]>,
    availability_zone_ids: Option<&[String]>,
) -> Option<Scaffolded<E>> {
    let catalog = ec2::availability_zones_in_region(region);
    let unknown = availability_zones
        .unwrap_or_default()
        .iter()
        .find(|zone| !catalog.iter().any(|held| held.zone_name == **zone))
        .or_else(|| {
            availability_zone_ids
                .unwrap_or_default()
                .iter()
                .find(|zone_id| !catalog.iter().any(|held| held.zone_id == **zone_id))
        })?;
    Some(Scaffolded::validation_error(
        invalid_availability_zone_message(unknown),
    ))
}

/// Whether a request names exactly ONE launch source, and the refusal when it does not.
///
/// "Valid requests must contain either LaunchTemplate, LaunchConfigurationName, InstanceId or
/// MixedInstancesPolicy parameter": the four members are alternative ways of saying what a
/// group launches from, so a group has exactly one launch source and a request names exactly
/// one member for it. A request naming none cannot describe a group, and one naming several
/// describes two groups.
///
/// Decided from the members ALONE, which is why it is answered before any of their values is
/// resolved: a request naming two launch sources is refused for naming two, and the caller is
/// told to remove one rather than to go and create the launch configuration it also named.
pub fn launch_source_error<E>(
    instance_id: Option<&str>,
    launch_configuration_name: Option<&str>,
    launch_template: Option<&LaunchTemplateSpecification>,
    mixed_instances_policy: Option<&MixedInstancesPolicy>,
) -> Option<Scaffolded<E>> {
    let stated = [
        instance_id.is_some(),
        launch_configuration_name.is_some(),
        launch_template.is_some(),
        mixed_instances_policy.is_some(),
    ]
    .into_iter()
    .filter(|stated| *stated)
    .count();
    (stated != 1).then(|| Scaffolded::validation_error(ONE_LAUNCH_SOURCE_MESSAGE))
}

/// The refusal for a launch source naming a launch template — or a VERSION of one — the
/// account does not hold, or `None` when every reference it names resolves.
///
/// A launch template is Amazon EC2's resource, and Amazon EC2 Auto Scaling resolves the one a
/// group launches from when the group is configured — which is why a group reports both
/// halves of the specification it was given ([`super::ec2_instances`]). A specification that
/// resolves to nothing names a template the group could never launch from, so the request is
/// refused instead of stored: a group created against it could launch nothing, and every read
/// of it would advertise a template Amazon EC2 does not have.
///
/// Every specification the launch source carries is resolved — the group's own
/// `LaunchTemplate`, the launch template of its `MixedInstancesPolicy` and the one each
/// override of that policy may name ([`super::ec2_instances::specifications`]) — because each
/// is a template instances of the group are launched from. A specification stating neither
/// half names no template to resolve and is left to the rule about the halves themselves.
///
/// `None` also covers an engine carrying no Amazon EC2 store at all: a refusal has to be
/// grounded in what the account's Amazon EC2 state answered, never assumed.
pub fn unresolvable_launch_template_error<Eng: Send + Sync + 'static, E>(
    engine: &Eng,
    launch_template: Option<&LaunchTemplateSpecification>,
    mixed_instances_policy: Option<&MixedInstancesPolicy>,
) -> Option<Scaffolded<E>> {
    let references: Vec<TemplateReference> =
        super::ec2_instances::specifications(launch_template, mixed_instances_policy)
            .into_iter()
            .filter_map(TemplateReference::of)
            .collect();
    if references.is_empty() {
        return None;
    }
    let state = ec2::service_state(engine)?;
    let store = state.lock();
    references
        .into_iter()
        .find_map(|reference| reference.refusal(&store))
        .map(Scaffolded::validation_error)
}

/// ONE launch-template reference a launch source carries: the template it names, and the
/// version of it the group would launch from.
#[derive(Debug, Clone)]
struct TemplateReference {
    /// How the specification names the template.
    template: TemplateIdentity,
    /// The `Version` the specification states, as it wrote it, or `None` when it states
    /// none: "the default value is `$Default`"
    /// (`smithy_query(query="shape",shape="LaunchTemplateSpecification")`), which is the
    /// version a group naming no version launches from.
    version: Option<String>,
}

impl TemplateReference {
    /// The reference `specification` carries, or `None` when it names no template — the
    /// empty string names none either.
    fn of(specification: &LaunchTemplateSpecification) -> Option<Self> {
        Some(Self {
            template: TemplateIdentity::of(specification)?,
            version: specification
                .version
                .as_deref()
                .filter(|version| !version.is_empty())
                .map(::std::string::ToString::to_string),
        })
    }

    /// What AWS answers a request whose launch source carries this reference, or `None`
    /// when the account holds the launch-template version it names.
    ///
    /// Both halves are resolved, because a group launches from ONE VERSION of one template:
    /// a version the template never minted is a launch source the group could never launch
    /// from, exactly as a template the account does not hold is. Either way the sentence is
    /// Amazon EC2's own for the half that did not resolve, surfaced as this service's
    /// refusal and not re-worded — the version half read from Amazon EC2's own resolution of
    /// it ([`ec2::resolve_launch_template_version`], the one reading of that member in the
    /// tree), so the two services cannot disagree about which version a word names or about
    /// how a caller is told it does not exist.
    fn refusal(&self, store: &ec2::ComAmazonawsEc2Amazonec2State) -> Option<String> {
        let Some(record) = self.template.record(store) else {
            return Some(self.template.absent_template_refusal());
        };
        let version = self
            .version
            .as_deref()
            .unwrap_or(ec2::LAUNCH_TEMPLATE_VERSION_DEFAULT);
        match ec2::resolve_launch_template_version::<()>(record, version) {
            Ok(_) => None,
            Err(Ec2Refusal::ServiceError(refusal)) => Some(refusal.message),
            // The resolution answers nothing but that refusal; a version it could not place
            // is unusable all the same, and the sentence for the version half is Amazon
            // EC2's own either way.
            Err(_) => Some(ec2::specified_launch_template_version_not_found_message(
                record.launch_template_id(),
                version,
            )),
        }
    }
}

/// How ONE launch-template specification names its template: "you must specify either a
/// `LaunchTemplateId` or a `LaunchTemplateName`" (`LaunchTemplateSpecification`).
#[derive(Debug, Clone)]
enum TemplateIdentity {
    /// The template's id, as the specification's `LaunchTemplateId` states it.
    Id(String),
    /// The template's name, as its `LaunchTemplateName` states it.
    Name(String),
}

impl TemplateIdentity {
    /// The identity `specification` carries, or `None` when it carries neither half — the
    /// empty string names no template either.
    fn of(specification: &LaunchTemplateSpecification) -> Option<Self> {
        if let Some(id) = specification
            .launch_template_id
            .as_deref()
            .filter(|id| !id.is_empty())
        {
            return Some(Self::Id(id.to_string()));
        }
        let name = specification
            .launch_template_name
            .as_deref()
            .filter(|name| !name.is_empty())?;
        Some(Self::Name(name.to_string()))
    }

    /// The launch template of the account's own Amazon EC2 store this identity names, looked
    /// up by the half the specification stated — the same two lookups Amazon EC2 resolves its
    /// own launch-template references by — or `None` when the account holds none.
    fn record<'store>(
        &self,
        store: &'store ec2::ComAmazonawsEc2Amazonec2State,
    ) -> Option<&'store ec2::LaunchTemplateRecord> {
        match self {
            Self::Id(id) => store.launch_templates.get(id),
            Self::Name(name) => store
                .launch_template_id_for_name(name)
                .and_then(|id| store.launch_templates.get(&id)),
        }
    }

    /// What AWS answers a request whose launch source names a template the account does not
    /// hold: the sentence Amazon EC2 — the service that holds launch templates, and the one
    /// Amazon EC2 Auto Scaling resolves the specification against — words for the half the
    /// request stated, surfaced as this service's own refusal.
    ///
    /// An ID the account holds no template under is not an id of a launch template at all,
    /// which is the sentence Amazon EC2 states for an id it cannot read
    /// (`Malformed launch template Id: <id>`, its `invalid_launch_template_id_malformed`, whose
    /// own documentation names this service as the one that surfaces it); a NAME it holds none
    /// under is the sentence Amazon EC2 states for that (its
    /// `invalid_launch_template_name_not_found`), which names no value because a request names
    /// one name and the service does not repeat it back.
    fn absent_template_refusal(&self) -> String {
        match self {
            Self::Id(id) => format!("Malformed launch template Id: {id}"),
            Self::Name(_) => {
                "At least one of the launch templates specified in the request does not exist."
                    .to_string()
            }
        }
    }
}

/// The refusal for a pair of healthy-percentage thresholds that cannot hold together, or
/// `None` when they can.
///
/// The thresholds bound how much of a group may be replaced at once: "both
/// `MinHealthyPercentage` and `MaxHealthyPercentage` must be specified, and the difference
/// between them cannot be greater than 100"
/// (`smithy_query(query="shape",shape="InstanceMaintenancePolicy")`). Neither rule is about
/// one member's value, so the model cannot state either and the service enforces both.
///
/// The ONE reading of that pair, for every request that carries one: the maintenance policy
/// a group is created or updated with ([`instance_maintenance_policy_error`]) and the
/// preferences an instance refresh is started with, which carry the same two members and are
/// answered the same sentences. A threshold of [`RESET_PERCENTAGE`] states no threshold, and
/// `thresholds` says which wording the pair rule is answered with ([`HealthyThresholds`]).
///
/// The pair rule is read in ONE direction, which is the direction the service enforces: an
/// upper threshold without a lower one is refused, while a LOWER threshold alone is served —
/// `MinHealthyPercentage` is the member that bounds a replacement on its own, and the
/// recorded refreshes that state it alone are accepted.
pub fn healthy_percentage_error<E>(
    min_healthy_percentage: Option<i32>,
    max_healthy_percentage: Option<i32>,
    thresholds: HealthyThresholds,
) -> Option<Scaffolded<E>> {
    let stated = |value: Option<i32>| value.filter(|percentage| *percentage != RESET_PERCENTAGE);
    match (
        stated(min_healthy_percentage),
        stated(max_healthy_percentage),
    ) {
        // One threshold without the other: an upper threshold bounds a lower one, so there is
        // nothing for it to bound when the request states none.
        (None, Some(_)) => Some(Scaffolded::validation_error(thresholds.pair_message())),
        (Some(min), Some(max)) if max.saturating_sub(min) > MAX_HEALTHY_PERCENTAGE_SPREAD => Some(
            Scaffolded::validation_error(HEALTHY_PERCENTAGE_SPREAD_MESSAGE),
        ),
        _ => None,
    }
}

/// The refusal for an instance maintenance policy whose two thresholds cannot hold together,
/// or `None` when they can — the group's own copy of [`healthy_percentage_error`].
pub fn instance_maintenance_policy_error<E>(
    policy: Option<&InstanceMaintenancePolicy>,
    thresholds: HealthyThresholds,
) -> Option<Scaffolded<E>> {
    let policy = policy?;
    healthy_percentage_error(
        policy.min_healthy_percentage,
        policy.max_healthy_percentage,
        thresholds,
    )
}

/// What Amazon EC2 Auto Scaling answers a mixed instances policy that overrides an instance
/// type more than once; the types the request stated twice are the one variable part, named
/// in the order the request first repeated each of them.
#[must_use]
pub fn duplicate_override_message(instance_types: &[&str]) -> String {
    format!(
        "Cannot add same instance type override more than once. Remove these duplicates from \
         the request and try again: [{}]",
        instance_types.join(", ")
    )
}

/// The refusal for a mixed instances policy that overrides the same instance type more than
/// once, or `None` when each type it names is named once.
///
/// An override says what to use INSTEAD of the launch template's own instance type, and
/// carries the weight that type counts for ("the number of capacity units provided by each
/// instance type", `LaunchTemplateOverrides$WeightedCapacity`): one type therefore has one
/// override, since two would give the group two weights for one type and no answer to which
/// of them a launch costs. The refusal names EVERY type the request repeated, which is what
/// the caller has to remove.
///
/// An override that names no instance type names it zero times — `InstanceRequirements` is
/// the alternative way of choosing types, and "if you specify `InstanceRequirements`, you
/// can't specify `InstanceType`" — so it is passed over rather than counted as a duplicate
/// of the other overrides that name none.
pub fn duplicate_override_error<E>(
    mixed_instances_policy: Option<&MixedInstancesPolicy>,
) -> Option<Scaffolded<E>> {
    let overrides = mixed_instances_policy?
        .launch_template
        .as_ref()?
        .overrides
        .as_ref()?;
    let mut named: BTreeSet<&str> = BTreeSet::new();
    let mut repeated_once: BTreeSet<&str> = BTreeSet::new();
    let mut repeated: Vec<&str> = Vec::new();
    for instance_type in overrides
        .iter()
        .filter_map(|entry| entry.instance_type.as_deref())
        .filter(|instance_type| !instance_type.is_empty())
    {
        // Both memberships are set lookups, so the pass costs the same per override however
        // many the request carries: nothing here is the product of the request's own size.
        if !named.insert(instance_type) && repeated_once.insert(instance_type) {
            repeated.push(instance_type);
        }
    }
    (!repeated.is_empty())
        .then(|| Scaffolded::validation_error(duplicate_override_message(&repeated)))
}

/// Whether this instance lifecycle policy keeps a group's instances when a termination
/// lifecycle hook is abandoned.
///
/// "Specifies the action when a termination lifecycle hook is abandoned … set to `retain` to
/// move instances to a retained state" (`RetentionTriggers$TerminateHookAbandon`), and
/// "retained instances … remain until you call `TerminateInstanceInAutoScalingGroup`" — so a
/// group under this trigger has instances the service may not terminate on its own, which is
/// what a forced delete would have to do.
#[must_use]
pub fn retains_abandoned_terminations(policy: Option<&InstanceLifecyclePolicy>) -> bool {
    policy
        .and_then(|policy| policy.retention_triggers.as_ref())
        .and_then(|triggers| triggers.terminate_hook_abandon.as_deref())
        .is_some_and(|action| action == RETENTION_ACTION_RETAIN)
}

/// The refusal for a forced delete of a group whose abandoned-terminate hook retains its
/// instances.
///
/// A forced delete is "the group … deleted along with all instances associated with the
/// group" (`DeleteAutoScalingGroupType$ForceDelete`), and this trigger is the group's standing
/// instruction to RETAIN the instances a termination would otherwise take: the two cannot both
/// be honoured, so the call is refused and the group survives.
pub fn force_delete_retention_error<E>() -> Scaffolded<E> {
    Scaffolded::validation_error(FORCE_DELETE_RETAINS_INSTANCES_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the launch-source table: the case, whether the request names an instance,
    /// a launch configuration, a launch template and a mixed instances policy, and whether
    /// it is refused.
    type LaunchSourceRow = (&'static str, bool, bool, bool, bool, bool);

    /// One row of the threshold table: the case, `MinHealthyPercentage`,
    /// `MaxHealthyPercentage`, and the refusal expected of the pair.
    type ThresholdRow = (
        &'static str,
        Option<i32>,
        Option<i32>,
        Option<&'static str>,
    );

    /// The wire code and message of a refusal, for an assertion that reads both.
    fn refused<E>(error: &Scaffolded<E>) -> String {
        match error {
            Scaffolded::ValidationError(exception) => exception.message.clone(),
            _ => "not a ValidationError".to_string(),
        }
    }

    /// RULE: a request names exactly one launch source; naming none and naming several are
    /// the same refusal, and it is the one AWS states.
    #[test]
    fn exactly_one_launch_source_is_accepted() {
        let template = LaunchTemplateSpecification {
            launch_template_name: Some("lt".to_string()),
            ..LaunchTemplateSpecification::default()
        };
        let policy = MixedInstancesPolicy::default();
        // (case, instance id, launch configuration, launch template, mixed instances policy,
        //  whether the request is refused)
        let rows: &[LaunchSourceRow] = &[
            ("no launch source at all", false, false, false, false, true),
            ("a launch template alone", false, false, true, false, false),
            ("a launch configuration alone", false, true, false, false, false),
            ("a mixed instances policy alone", false, false, false, true, false),
            ("an instance alone", true, false, false, false, false),
            ("a launch template and a configuration", false, true, true, false, true),
            ("a launch template and a policy", false, false, true, true, true),
            ("an instance and a launch template", true, false, true, false, true),
            ("all four", true, true, true, true, true),
        ];
        for (case, instance, configuration, launch_template, mixed, expected) in rows {
            let error = launch_source_error::<()>(
                instance.then_some("i-0"),
                configuration.then_some("lc"),
                launch_template.then_some(&template),
                mixed.then_some(&policy),
            );
            assert_eq!(
                error.is_some(),
                *expected,
                "case {case:?}: expected refused {expected}, got {error:?}"
            );
            if let Some(error) = error {
                assert_eq!(
                    refused(&error),
                    ONE_LAUNCH_SOURCE_MESSAGE,
                    "case {case:?}: expected the one-of sentence"
                );
            }
        }
    }

    /// RULE: a maintenance policy states a lower threshold for every upper one, and the two
    /// are at most 100 apart; a threshold being reset states none.
    #[test]
    fn the_maintenance_policy_thresholds_must_hold_together() {
        // (case, MinHealthyPercentage, MaxHealthyPercentage, the expected refusal)
        let rows: &[ThresholdRow] = &[
            ("no policy member at all", None, None, None),
            ("a lower threshold alone", Some(90), None, None),
            ("both, 10 apart", Some(90), Some(100), None),
            ("both, exactly 100 apart", Some(0), Some(100), None),
            (
                "both, 101 apart",
                Some(49),
                Some(150),
                Some(HEALTHY_PERCENTAGE_SPREAD_MESSAGE),
            ),
            (
                "an upper threshold alone",
                None,
                Some(150),
                Some(HEALTHY_PERCENTAGE_PAIR_RESET_MESSAGE),
            ),
            (
                "an upper threshold beside a reset lower one",
                Some(RESET_PERCENTAGE),
                Some(150),
                Some(HEALTHY_PERCENTAGE_PAIR_RESET_MESSAGE),
            ),
            (
                "both reset",
                Some(RESET_PERCENTAGE),
                Some(RESET_PERCENTAGE),
                None,
            ),
        ];
        for (case, min, max, expected) in rows {
            let policy = InstanceMaintenancePolicy {
                min_healthy_percentage: *min,
                max_healthy_percentage: *max,
            };
            let error =
                instance_maintenance_policy_error::<()>(Some(&policy), HealthyThresholds::Changed);            match expected {
                Some(message) => {
                    let error = error.unwrap_or_else(|| {
                        panic!("case {case:?}: expected {message:?}, got no refusal")
                    });
                    assert_eq!(
                        refused(&error),
                        *message,
                        "case {case:?}: expected AWS's sentence for this rule"
                    );
                }
                None => assert!(
                    error.is_none(),
                    "case {case:?}: expected the policy to be accepted, got {error:?}"
                ),
            }
        }
    }
}
