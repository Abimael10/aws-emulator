// SPDX-License-Identifier: CC0-1.0

//! The findings plane of this service's state: the findings Macie holds for an
//! account in one region, and the query a read runs over them.
//!
//! `CreateSampleFindings` mints them, `ListFindings` enumerates the ids of the
//! ones a request's criteria select and `GetFindings` projects the records
//! themselves — so the property extraction, the condition evaluation and the
//! ordering all live here, once, rather than in each of those handlers.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{CriterionAdditionalProperties, Finding, SortCriteria};

use super::{mix64, minted_at_bits};

/// Width of the id this service issues a finding, in lowercase hex digits.
/// `Finding$id` documents "a random string that Amazon Macie generates and
/// assigns to a finding when it creates the finding", so the model pins no
/// grammar; what matters is that it is opaque and unique, which
/// [`new_finding_id`] carries.
pub const FINDING_ID_LEN: usize = 32;

/// The most findings one emulator process holds, across every region. The
/// EMULATOR's own memory bound, on the same footing as
/// [`super::MAX_CUSTOM_DATA_IDENTIFIERS`]: a create past it answers the
/// `ServiceQuotaExceededException` the operation models.
pub const MAX_FINDINGS: usize = 10_000;

/// The page size `ListFindings` uses when a request names no `maxResults`. That
/// member is a bare `__integer` with neither a modelled range nor a documented
/// default, so this is the EMULATOR's choice, the same one the other paginated
/// read of this service makes ([`super::DEFAULT_PAGE_SIZE`]).
pub const DEFAULT_FINDING_PAGE_SIZE: usize = super::DEFAULT_PAGE_SIZE;

/// The most finding ids one `ListFindings` response carries, however large a
/// `maxResults` a request asks for. The EMULATOR's own bound on the
/// amplification between a tiny request and the response it produces.
pub const MAX_FINDING_PAGE_SIZE: usize = super::MAX_PAGE_SIZE;

/// The most finding ids one `GetFindings` request may name: its `findingIds`
/// member doc states "You can specify as many as 50 unique identifiers in this
/// array."
pub const MAX_GET_FINDINGS_IDS: usize = 50;

/// The member a `GetFindings` refusal about that cap names, as the operation's
/// input spells it.
pub const FINDING_IDS_MEMBER: &str = "findingIds";

/// How `GetFindings` refuses a request naming more finding ids than
/// [`MAX_GET_FINDINGS_IDS`].
///
/// The cap is documentation rather than a modelled `@length`, so no walk ahead
/// of the handler reports it; the sentence is still the one a constraint
/// violation gets ([`super::constraint_refusal::strings_over_cap_message`]), so
/// this service reports a size it enforces itself exactly as it reports one the
/// model declares.
pub fn too_many_finding_ids_message(finding_ids: &[String]) -> String {
    super::constraint_refusal::strings_over_cap_message(
        FINDING_IDS_MEMBER,
        finding_ids,
        MAX_GET_FINDINGS_IDS,
    )
}

/// The prefix every sensitive-data `FindingType` carries, which is what makes a
/// finding of that type a CLASSIFICATION finding.
pub const SENSITIVE_DATA_TYPE_PREFIX: &str = "SensitiveData:";

/// The prefix every policy `FindingType` carries, which is what makes a finding
/// of that type a POLICY finding.
pub const POLICY_TYPE_PREFIX: &str = "Policy:";

/// The `FindingCategory` of a sensitive data finding: its doc states
/// "CLASSIFICATION, for a sensitive data finding". An entry of
/// [`crate::ops::types::FINDING_CATEGORY_VALUES`].
pub const CLASSIFICATION_CATEGORY: &str = "CLASSIFICATION";

/// The `FindingCategory` of a policy finding: the same doc sentence continues
/// "and, POLICY, for a policy finding".
pub const POLICY_CATEGORY: &str = "POLICY";

/// The qualitative severities a finding may carry, LEAST SEVERE FIRST.
/// `Severity$description` documents the range as "from Low (least severe) to
/// High (most severe)", so this order is the rule rather than the alphabetical
/// order of [`crate::ops::types::SEVERITY_DESCRIPTION_VALUES`].
pub const SEVERITY_DESCRIPTIONS_ASCENDING: [&str; 3] = ["Low", "Medium", "High"];

/// The ascending sort order: an entry of
/// [`crate::ops::types::ORDER_BY_VALUES`], documented as "ASC, sort the results
/// in ascending order".
pub const ORDER_ASCENDING: &str = "ASC";

/// The descending one: "DESC, sort the results in descending order".
pub const ORDER_DESCENDING: &str = "DESC";

/// The numerical severity that goes with `description`, or `None` when that is
/// not a severity a finding may carry. `Severity$score` documents the range as
/// "from 1 (least severe) to 3 (most severe)", which is exactly the rank of the
/// description in [`SEVERITY_DESCRIPTIONS_ASCENDING`], counting from one — so
/// the two representations of one severity cannot disagree.
pub fn severity_score(description: &str) -> Option<i64> {
    let rank = SEVERITY_DESCRIPTIONS_ASCENDING
        .iter()
        .position(|known| *known == description)?;
    i64::try_from(rank.saturating_add(1)).ok()
}

/// The `FindingCategory` a finding of `finding_type` belongs to, or `None` when
/// the type is not one the model declares.
///
/// The category follows from the TYPE rather than from a table of its own: every
/// `FindingType` the model declares names the finding's category in its prefix
/// — `SensitiveData:` for the sensitive data findings Macie reports as
/// CLASSIFICATION, `Policy:` for the policy findings it reports as POLICY.
pub fn finding_category(finding_type: &str) -> Option<&'static str> {
    if finding_type.starts_with(SENSITIVE_DATA_TYPE_PREFIX) {
        Some(CLASSIFICATION_CATEGORY)
    } else if finding_type.starts_with(POLICY_TYPE_PREFIX) {
        Some(POLICY_CATEGORY)
    } else {
        None
    }
}

/// The id of a finding for `seed`: [`FINDING_ID_LEN`] lowercase hex digits.
///
/// INJECTIVE in `seed` — the first half is [`mix64`] of it and `mix64` is a
/// bijection on `u64` — so no two findings of one store can carry the same id
/// however the clock behaves. The second half carries the instant it was minted
/// at, so the ids of one store do not read as a counter.
pub fn new_finding_id(seed: u64, now: SystemTime) -> String {
    let unique = mix64(seed);
    let instant = mix64(minted_at_bits(now) ^ unique);
    format!("{unique:016x}{instant:016x}")
}

/// A finding this account's Macie holds, in one region.
///
/// The record IS the modelled `Finding` a read projects, plus the two things a
/// query needs beside it: the region the finding belongs to and the instant it
/// was created at, in the form the store orders by.
#[derive(Debug, Clone)]
pub struct StoredFinding {
    /// The region the call that created this finding addressed. Macie is a
    /// regional service, so a finding is only visible to a request naming the
    /// region it was created in — which is also the region the finding reports.
    pub region: String,
    /// When Macie created this finding, which `createdAt` reports.
    pub created_at: SystemTime,
    /// Every member of the modelled `Finding`, as a read reports them.
    pub finding: Finding,
}

impl StoredFinding {
    /// This finding's id, which is also the key the store holds it under.
    pub fn id(&self) -> &str {
        self.finding.id.as_deref().unwrap_or_default()
    }
}

/// One value a finding carries for a property a query names.
///
/// The three kinds are what the modelled `Finding` members reduce to on the
/// wire: a string (including an enum value), a `__long`, and a `__boolean`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    /// A string or enum member's value.
    Text(String),
    /// A `__long` member's value, or an instant in epoch milliseconds.
    Number(i64),
    /// A `__boolean` member's value.
    Flag(bool),
}

impl PropertyValue {
    /// This value as a condition's `eq`, `neq` and `eqExactMatch` name it: those
    /// operators carry `__listOf__string`, so every property is compared as the
    /// text it renders as on the wire.
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(number) => number.to_string(),
            Self::Flag(flag) => flag.to_string(),
        }
    }

    /// This value as a condition's `gt`, `gte`, `lt` and `lte` name it: those
    /// operators carry a `__long`, so only a numeric property is bounded by
    /// them.
    pub fn as_number(&self) -> Option<i64> {
        match self {
            Self::Number(number) => Some(*number),
            Self::Text(_) | Self::Flag(_) => None,
        }
    }
}

/// The properties a findings query may filter on: the scalar members of the
/// modelled `Finding`, each named by the dotted path of the `jsonName`s that
/// reach it (`smithy_query(query="shape",shape="Finding")` for the member list,
/// and the shape of each nested structure for the rest of each path).
///
/// A query naming anything else is refused rather than silently matching
/// nothing: a property this service cannot filter on is a request the caller has
/// to fix.
pub const FILTERABLE_PROPERTIES: &[&str] = &[
    "accountId",
    "archived",
    "category",
    "classificationDetails.originType",
    "count",
    "createdAt",
    "id",
    "partition",
    "policyDetails.action.actionType",
    "region",
    "resourcesAffected.s3Bucket.name",
    "resourcesAffected.s3Object.key",
    "sample",
    "schemaVersion",
    "severity.description",
    "severity.score",
    "title",
    "type",
    "updatedAt",
];

/// The value `finding` carries for `path`, or `None` when it carries none —
/// either because the path is not one of [`FILTERABLE_PROPERTIES`] or because
/// this finding does not report that member (a policy finding reports no
/// `classificationDetails`, and the other way round).
pub fn finding_property(finding: &Finding, path: &str) -> Option<PropertyValue> {
    match path {
        "accountId" => finding.account_id.clone().map(PropertyValue::Text),
        "archived" => finding.archived.map(PropertyValue::Flag),
        "category" => finding.category.clone().map(PropertyValue::Text),
        "classificationDetails.originType" => finding
            .classification_details
            .as_ref()?
            .origin_type
            .clone()
            .map(PropertyValue::Text),
        "count" => finding.count.map(PropertyValue::Number),
        "createdAt" => finding
            .created_at
            .as_ref()
            .map(|at| PropertyValue::Number(epoch_millis(at))),
        "id" => finding.id.clone().map(PropertyValue::Text),
        "partition" => finding.partition.clone().map(PropertyValue::Text),
        "policyDetails.action.actionType" => finding
            .policy_details
            .as_ref()?
            .action
            .as_ref()?
            .action_type
            .clone()
            .map(PropertyValue::Text),
        "region" => finding.region.clone().map(PropertyValue::Text),
        "resourcesAffected.s3Bucket.name" => finding
            .resources_affected
            .as_ref()?
            .s3_bucket
            .as_ref()?
            .name
            .clone()
            .map(PropertyValue::Text),
        "resourcesAffected.s3Object.key" => finding
            .resources_affected
            .as_ref()?
            .s3_object
            .as_ref()?
            .key
            .clone()
            .map(PropertyValue::Text),
        "sample" => finding.sample.map(PropertyValue::Flag),
        "schemaVersion" => finding.schema_version.clone().map(PropertyValue::Text),
        "severity.description" => finding
            .severity
            .as_ref()?
            .description
            .clone()
            .map(PropertyValue::Text),
        "severity.score" => finding.severity.as_ref()?.score.map(PropertyValue::Number),
        "title" => finding.title.clone().map(PropertyValue::Text),
        "type" => finding.r#type.clone().map(PropertyValue::Text),
        "updatedAt" => finding
            .updated_at
            .as_ref()
            .map(|at| PropertyValue::Number(epoch_millis(at))),
        _ => None,
    }
}

/// An instant as a numeric condition names it: epoch MILLISECONDS, the unit a
/// `gt`/`gte`/`lt`/`lte` bound on a date property can carry, since those
/// operators take a `__long` rather than the `date-time` string the member is
/// reported as.
fn epoch_millis(at: &::aws_smithy_types::DateTime) -> i64 {
    at.secs()
        .saturating_mul(1_000)
        .saturating_add(i64::from(at.subsec_nanos() / 1_000_000))
}

/// How the service refuses a query naming a property it cannot filter findings
/// on. The only variable part is the property the request named.
pub fn unfilterable_property_message(path: &str) -> String {
    format!("The findingCriteria names a property that findings cannot be filtered on: {path}")
}

/// The most conditions one `findingCriteria` may carry.
///
/// A criterion is a MAP keyed by the property it filters on, and
/// [`validate_criterion`] accepts only the keys [`FILTERABLE_PROPERTIES`] names,
/// so one request can never usefully carry more conditions than there are
/// filterable properties. Checked explicitly all the same, so the work a read
/// does is bounded by the code a reader can see: a `ListFindings` performs at
/// most `MAX_CONDITIONS_PER_CRITERION * MAX_VALUES_PER_CONDITION * MAX_FINDINGS`
/// comparisons.
pub const MAX_CONDITIONS_PER_CRITERION: usize = FILTERABLE_PROPERTIES.len();

/// The most values one condition may name across its `eq`, `eqExactMatch` and
/// `neq` lists, which are the operators that carry a `__listOf__string`. The
/// model constrains none of them, so this is the EMULATOR's own bound on the
/// other factor of the product above — well above the handful of values a
/// condition on any of the properties above is written with.
pub const MAX_VALUES_PER_CONDITION: usize = 50;

/// How the service refuses a `findingCriteria` carrying more conditions than it
/// can filter on.
pub fn too_many_conditions_message(count: usize) -> String {
    format!(
        "The findingCriteria can name as many as {MAX_CONDITIONS_PER_CRITERION} conditions; \
         the request named {count}."
    )
}

/// How the service refuses one condition naming more values than it accepts.
/// The variable parts are the property the condition filters on and the number
/// of values the request named for it.
pub fn too_many_values_message(path: &str, count: usize) -> String {
    format!(
        "The findingCriteria condition on {path} can name as many as \
         {MAX_VALUES_PER_CONDITION} values; the request named {count}."
    )
}

/// Refuse a `findingCriteria` that names a property findings cannot be filtered
/// on, or that is larger than this service filters on.
///
/// Checked ONCE per request, BEFORE the store is read, so an invalid criterion
/// is refused whatever the other conditions select, whatever order the map is
/// walked in, and even when the store holds no finding to test it against — and
/// so the work the enumeration then does is capped before it starts.
pub fn validate_criterion(
    criterion: &HashMap<String, CriterionAdditionalProperties>,
) -> Result<(), String> {
    if criterion.len() > MAX_CONDITIONS_PER_CRITERION {
        return Err(too_many_conditions_message(criterion.len()));
    }
    for (path, condition) in criterion {
        if !FILTERABLE_PROPERTIES.contains(&path.as_str()) {
            return Err(unfilterable_property_message(path));
        }
        let values = condition_value_count(condition);
        if values > MAX_VALUES_PER_CONDITION {
            return Err(too_many_values_message(path, values));
        }
    }
    Ok(())
}

/// How many values one condition names across the three operators that carry a
/// list of them — the count [`MAX_VALUES_PER_CONDITION`] bounds.
fn condition_value_count(condition: &CriterionAdditionalProperties) -> usize {
    [
        condition.eq.as_ref(),
        condition.eq_exact_match.as_ref(),
        condition.neq.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(Vec::len)
    .sum()
}

/// Whether `finding` satisfies every condition of `criterion`.
///
/// Each entry of the map is one condition, and the entries are joined with AND:
/// a finding is selected when it satisfies all of them. A property the finding
/// does not carry satisfies no operator that names a value for it, and
/// satisfies `neq` — the value it does not carry cannot equal one. The paths are
/// [`validate_criterion`]'s business, so one this service cannot filter on
/// selects nothing here.
pub fn matches_criteria(
    finding: &Finding,
    criterion: &HashMap<String, CriterionAdditionalProperties>,
) -> bool {
    criterion.iter().all(|(path, condition)| {
        FILTERABLE_PROPERTIES.contains(&path.as_str())
            && condition_holds(finding_property(finding, path).as_ref(), condition)
    })
}

/// Whether one condition holds for one property value, operator by operator as
/// `CriterionAdditionalProperties` documents them.
///
/// An operator naming an EMPTY list of values names no value to compare
/// against, so it states no condition and leaves the selection to the other
/// operators of the same condition.
fn condition_holds(value: Option<&PropertyValue>, condition: &CriterionAdditionalProperties) -> bool {
    let text = value.map(PropertyValue::as_text);
    let number = value.and_then(PropertyValue::as_number);

    // `eq` — "the value for the property matches (equals) the specified value.
    // If you specify multiple values, Macie uses OR logic to join the values."
    if let Some(values) = condition.eq.as_ref().filter(|values| !values.is_empty()) {
        if !text.as_ref().is_some_and(|value| values.contains(value)) {
            return false;
        }
    }
    // `eqExactMatch` — "the value for the property exclusively matches (equals
    // an exact match for) all the specified values": for a single-valued
    // property that holds only when every value named is the one it carries.
    if let Some(values) = condition
        .eq_exact_match
        .as_ref()
        .filter(|values| !values.is_empty())
    {
        if !text
            .as_ref()
            .is_some_and(|value| values.iter().all(|named| named == value))
        {
            return false;
        }
    }
    // `neq` — "the value for the property doesn't match (doesn't equal) the
    // specified value."
    if let Some(values) = condition.neq.as_ref().filter(|values| !values.is_empty()) {
        if text.as_ref().is_some_and(|value| values.contains(value)) {
            return false;
        }
    }
    // The four numeric bounds, each documented as the plain comparison its name
    // reads as.
    if let Some(bound) = condition.gt {
        if !number.is_some_and(|value| value > bound) {
            return false;
        }
    }
    if let Some(bound) = condition.gte {
        if !number.is_some_and(|value| value >= bound) {
            return false;
        }
    }
    if let Some(bound) = condition.lt {
        if !number.is_some_and(|value| value < bound) {
            return false;
        }
    }
    if let Some(bound) = condition.lte {
        if !number.is_some_and(|value| value <= bound) {
            return false;
        }
    }
    true
}

/// The properties findings may be SORTED by, from the `attributeName` member doc
/// of `SortCriteria`: "Valid values are: count, createdAt,
/// policyDetails.action.apiCallDetails.firstSeen,
/// policyDetails.action.apiCallDetails.lastSeen, resourcesAffected,
/// severity.score, type, and updatedAt."
pub const SORTABLE_ATTRIBUTES: &[&str] = &[
    "count",
    "createdAt",
    "policyDetails.action.apiCallDetails.firstSeen",
    "policyDetails.action.apiCallDetails.lastSeen",
    "resourcesAffected",
    "severity.score",
    "type",
    "updatedAt",
];

/// How the service refuses a `sortCriteria` naming a property findings cannot be
/// sorted by.
pub fn unsortable_attribute_message(name: &str) -> String {
    format!("The sortCriteria names a property that findings cannot be sorted by: {name}")
}

/// How the service refuses a sort order that is neither of the two the modelled
/// `OrderBy` declares.
pub fn unknown_order_message(order: &str) -> String {
    format!(
        "The sortCriteria names a sort order that is not {ORDER_ASCENDING} or \
         {ORDER_DESCENDING}: {order}"
    )
}

/// The value `finding` is sorted by for `attribute`.
///
/// Every sortable attribute but two is also a filterable property, so this
/// delegates to [`finding_property`]; the two that are not are the instants of
/// the API call a policy finding reports and `resourcesAffected`, which names
/// the affected resource rather than one member of it — the bucket, then the
/// object inside it.
pub fn sort_value(finding: &Finding, attribute: &str) -> Option<PropertyValue> {
    match attribute {
        "policyDetails.action.apiCallDetails.firstSeen" => finding
            .policy_details
            .as_ref()?
            .action
            .as_ref()?
            .api_call_details
            .as_ref()?
            .first_seen
            .as_ref()
            .map(|at| PropertyValue::Number(epoch_millis(at))),
        "policyDetails.action.apiCallDetails.lastSeen" => finding
            .policy_details
            .as_ref()?
            .action
            .as_ref()?
            .api_call_details
            .as_ref()?
            .last_seen
            .as_ref()
            .map(|at| PropertyValue::Number(epoch_millis(at))),
        "resourcesAffected" => {
            let affected = finding.resources_affected.as_ref()?;
            let bucket = affected
                .s3_bucket
                .as_ref()
                .and_then(|bucket| bucket.name.clone())
                .unwrap_or_default();
            let key = affected
                .s3_object
                .as_ref()
                .and_then(|object| object.key.clone())
                .unwrap_or_default();
            Some(PropertyValue::Text(format!("{bucket}/{key}")))
        }
        other => finding_property(finding, other),
    }
}

/// Order two property values: numbers numerically, anything else by the text it
/// renders as, and a finding carrying no value for the attribute before one that
/// does.
fn compare(left: Option<&PropertyValue>, right: Option<&PropertyValue>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => match (left.as_number(), right.as_number()) {
            (Some(left), Some(right)) => left.cmp(&right),
            _ => left.as_text().cmp(&right.as_text()),
        },
    }
}

/// The attribute to sort by and whether to reverse the order, or the refusal
/// for a `sortCriteria` this service cannot serve.
///
/// A request naming no `sortCriteria`, or naming an order without an
/// `attributeName`, is served in the store's own order — by id.
fn sort_plan(criteria: Option<&SortCriteria>) -> Result<(Option<String>, bool), String> {
    let Some(criteria) = criteria else {
        return Ok((None, false));
    };
    let descending = match criteria.order_by.as_deref() {
        None | Some(ORDER_ASCENDING) => false,
        Some(ORDER_DESCENDING) => true,
        Some(other) => return Err(unknown_order_message(other)),
    };
    match criteria.attribute_name.as_deref() {
        Some(attribute) if !SORTABLE_ATTRIBUTES.contains(&attribute) => {
            Err(unsortable_attribute_message(attribute))
        }
        attribute => Ok((attribute.map(str::to_string), descending)),
    }
}

/// Refuse a `sortCriteria` this service cannot serve, without reading the store:
/// a read checks this BEFORE it takes the lock, so a request naming an attribute
/// findings cannot be sorted by, or an order the model does not declare, is
/// refused without contending for it.
pub fn validate_sort_criteria(criteria: Option<&SortCriteria>) -> Result<(), String> {
    sort_plan(criteria).map(|_| ())
}

/// Sort `findings` as `criteria` asks, or refuse the criteria.
///
/// The id is the final tie-break under every attribute, so two findings carrying
/// the same value for it keep one fixed relative order across calls.
pub fn sort_findings(
    findings: &mut [&StoredFinding],
    criteria: Option<&SortCriteria>,
) -> Result<(), String> {
    let (attribute, descending) = sort_plan(criteria)?;

    findings.sort_by(|left, right| {
        let ordering = match attribute.as_deref() {
            Some(attribute) => compare(
                sort_value(&left.finding, attribute).as_ref(),
                sort_value(&right.finding, attribute).as_ref(),
            ),
            None => Ordering::Equal,
        }
        .then_with(|| left.id().cmp(right.id()));
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        compare, condition_holds, epoch_millis, finding_category, finding_property, new_finding_id,
        severity_score, sort_findings, sort_value, validate_criterion, PropertyValue, StoredFinding,
        CLASSIFICATION_CATEGORY, FILTERABLE_PROPERTIES, FINDING_ID_LEN,
        MAX_CONDITIONS_PER_CRITERION, MAX_VALUES_PER_CONDITION, POLICY_CATEGORY,
        SEVERITY_DESCRIPTIONS_ASCENDING, SORTABLE_ATTRIBUTES,
    };
    use crate::ops::types::{
        CriterionAdditionalProperties, Finding, Severity, SortCriteria, FINDING_CATEGORY_VALUES,
        FINDING_TYPE_VALUES, SEVERITY_DESCRIPTION_VALUES,
    };
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    /// A finding carrying just enough to be ordered and compared.
    fn finding(id: &str, severity: &str, finding_type: &str) -> StoredFinding {
        StoredFinding {
            region: "us-west-2".to_string(),
            created_at: SystemTime::UNIX_EPOCH,
            finding: Finding {
                id: Some(id.to_string()),
                r#type: Some(finding_type.to_string()),
                severity: Some(Severity {
                    description: Some(severity.to_string()),
                    score: severity_score(severity),
                }),
                ..Finding::default()
            },
        }
    }

    /// Every id is [`FINDING_ID_LEN`] lowercase hex digits, and two seeds never
    /// collide, over a span of seeds and of minting instants.
    #[test]
    fn minted_finding_ids_are_opaque_and_never_repeat() {
        let mut seen = std::collections::BTreeSet::new();
        for now in [
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_nanos(1),
            SystemTime::now(),
        ] {
            for seed in 1..=256_u64 {
                let id = new_finding_id(seed, now);
                let case = format!("seed {seed} at {now:?}");
                assert_eq!(
                    id.chars().count(),
                    FINDING_ID_LEN,
                    "case {case}: a finding id is {FINDING_ID_LEN} characters, got {id:?}"
                );
                assert!(
                    id.chars()
                        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                    "case {case}: a finding id is lowercase hex, got {id:?}"
                );
                assert!(
                    seen.insert(id.clone()),
                    "case {case}: {id:?} was already minted for another seed"
                );
            }
        }
    }

    /// The category of every `FindingType` the model declares follows from its
    /// prefix, and is one of the categories the model declares.
    #[test]
    fn every_modelled_finding_type_has_a_modelled_category() {
        for finding_type in FINDING_TYPE_VALUES {
            let category = finding_category(finding_type);
            assert!(
                category.is_some_and(|category| FINDING_CATEGORY_VALUES.contains(&category)),
                "case {finding_type:?}: expected a modelled FindingCategory, got {category:?}"
            );
        }
        assert_eq!(
            finding_category("SensitiveData:S3Object/Financial"),
            Some(CLASSIFICATION_CATEGORY),
            "a sensitive data type is a classification finding"
        );
        assert_eq!(
            finding_category("Policy:IAMUser/S3BucketPublic"),
            Some(POLICY_CATEGORY),
            "a policy type is a policy finding"
        );
        assert_eq!(
            finding_category("Something:Else"),
            None,
            "a type the model does not declare has no category"
        );
    }

    /// The qualitative and numerical severities agree: every description the
    /// model declares scores its rank in the documented order, counting from
    /// one, and nothing else scores at all.
    #[test]
    fn severity_scores_follow_the_documented_range() {
        for (rank, description) in SEVERITY_DESCRIPTIONS_ASCENDING.iter().enumerate() {
            let expected = i64::try_from(rank + 1).expect("three severities fit an i64");
            assert_eq!(
                severity_score(description),
                Some(expected),
                "case {description:?}: expected score {expected}, got {:?}",
                severity_score(description)
            );
            assert!(
                SEVERITY_DESCRIPTION_VALUES.contains(description),
                "case {description:?}: must be a modelled SeverityDescription"
            );
        }
        assert_eq!(
            severity_score("CRITICAL"),
            None,
            "a severity a finding cannot carry has no score"
        );
    }

    /// Every filterable property is extracted from a finding that carries it, so
    /// the catalog a query is checked against cannot drift from the extraction.
    #[test]
    fn every_filterable_property_is_extracted() {
        let populated = Finding {
            account_id: Some("000000000000".to_string()),
            archived: Some(false),
            category: Some(CLASSIFICATION_CATEGORY.to_string()),
            classification_details: Some(crate::ops::types::ClassificationDetails {
                origin_type: Some("AUTOMATED_SENSITIVE_DATA_DISCOVERY".to_string()),
                ..crate::ops::types::ClassificationDetails::default()
            }),
            count: Some(1),
            created_at: Some(::aws_smithy_types::DateTime::from_secs(1)),
            id: Some("f".repeat(FINDING_ID_LEN)),
            partition: Some("aws".to_string()),
            policy_details: Some(crate::ops::types::PolicyDetails {
                action: Some(crate::ops::types::FindingAction {
                    action_type: Some("AWS_API_CALL".to_string()),
                    api_call_details: Some(crate::ops::types::ApiCallDetails {
                        first_seen: Some(::aws_smithy_types::DateTime::from_secs(1)),
                        last_seen: Some(::aws_smithy_types::DateTime::from_secs(2)),
                        ..crate::ops::types::ApiCallDetails::default()
                    }),
                }),
                actor: None,
            }),
            region: Some("us-west-2".to_string()),
            resources_affected: Some(crate::ops::types::ResourcesAffected {
                s3_bucket: Some(crate::ops::types::S3Bucket {
                    name: Some("a-bucket".to_string()),
                    ..crate::ops::types::S3Bucket::default()
                }),
                s3_object: Some(crate::ops::types::S3Object {
                    key: Some("an/object".to_string()),
                    ..crate::ops::types::S3Object::default()
                }),
            }),
            sample: Some(true),
            schema_version: Some("1.0".to_string()),
            severity: Some(Severity {
                description: Some("Medium".to_string()),
                score: Some(2),
            }),
            title: Some("a title".to_string()),
            r#type: Some("SensitiveData:S3Object/Financial".to_string()),
            updated_at: Some(::aws_smithy_types::DateTime::from_secs(3)),
            description: Some("a description".to_string()),
        };
        for path in FILTERABLE_PROPERTIES {
            assert!(
                finding_property(&populated, path).is_some(),
                "case {path:?}: a finding carrying every member must report this property"
            );
        }
        for attribute in SORTABLE_ATTRIBUTES {
            assert!(
                sort_value(&populated, attribute).is_some(),
                "case {attribute:?}: a finding carrying every member must sort by this attribute"
            );
        }
        assert!(
            finding_property(&populated, "notAProperty").is_none(),
            "a path no finding carries reports no value"
        );
    }

    /// Each operator of a condition holds exactly where it is documented to.
    #[test]
    fn each_operator_holds_where_it_is_documented_to() {
        let text = PropertyValue::Text("Medium".to_string());
        let number = PropertyValue::Number(2);
        let flag = PropertyValue::Flag(false);
        for (case, value, condition, expected) in [
            (
                "eq matches one of several values",
                &text,
                CriterionAdditionalProperties {
                    eq: Some(vec!["Low".to_string(), "Medium".to_string()]),
                    ..CriterionAdditionalProperties::default()
                },
                true,
            ),
            (
                "eq matches none of them",
                &text,
                CriterionAdditionalProperties {
                    eq: Some(vec!["Low".to_string()]),
                    ..CriterionAdditionalProperties::default()
                },
                false,
            ),
            (
                "neq excludes the value",
                &text,
                CriterionAdditionalProperties {
                    neq: Some(vec!["Medium".to_string()]),
                    ..CriterionAdditionalProperties::default()
                },
                false,
            ),
            (
                "eqExactMatch over one value",
                &text,
                CriterionAdditionalProperties {
                    eq_exact_match: Some(vec!["Medium".to_string()]),
                    ..CriterionAdditionalProperties::default()
                },
                true,
            ),
            (
                "eqExactMatch over two different values",
                &text,
                CriterionAdditionalProperties {
                    eq_exact_match: Some(vec!["Medium".to_string(), "Low".to_string()]),
                    ..CriterionAdditionalProperties::default()
                },
                false,
            ),
            (
                "a boolean compares as its text",
                &flag,
                CriterionAdditionalProperties {
                    eq: Some(vec!["false".to_string()]),
                    ..CriterionAdditionalProperties::default()
                },
                true,
            ),
            (
                "gte at the bound",
                &number,
                CriterionAdditionalProperties {
                    gte: Some(2),
                    ..CriterionAdditionalProperties::default()
                },
                true,
            ),
            (
                "gt at the bound",
                &number,
                CriterionAdditionalProperties {
                    gt: Some(2),
                    ..CriterionAdditionalProperties::default()
                },
                false,
            ),
            (
                "lt above the value",
                &number,
                CriterionAdditionalProperties {
                    lt: Some(3),
                    ..CriterionAdditionalProperties::default()
                },
                true,
            ),
            (
                "lte below the value",
                &number,
                CriterionAdditionalProperties {
                    lte: Some(1),
                    ..CriterionAdditionalProperties::default()
                },
                false,
            ),
            (
                "a numeric bound on a text property",
                &text,
                CriterionAdditionalProperties {
                    gte: Some(1),
                    ..CriterionAdditionalProperties::default()
                },
                false,
            ),
        ] {
            let holds = condition_holds(Some(value), &condition);
            assert_eq!(
                holds, expected,
                "case {case:?}: expected {expected}, got {holds} for {value:?}"
            );
        }

        // A property the finding does not carry satisfies nothing that names a
        // value for it, and satisfies `neq`.
        assert!(
            !condition_holds(
                None,
                &CriterionAdditionalProperties {
                    eq: Some(vec!["Medium".to_string()]),
                    ..CriterionAdditionalProperties::default()
                }
            ),
            "case \"an absent property under eq\": nothing equals a value it does not carry"
        );
        assert!(
            condition_holds(
                None,
                &CriterionAdditionalProperties {
                    neq: Some(vec!["Medium".to_string()]),
                    ..CriterionAdditionalProperties::default()
                }
            ),
            "case \"an absent property under neq\": a value it does not carry cannot equal one"
        );
    }

    /// An instant is bounded in epoch milliseconds.
    #[test]
    fn an_instant_is_compared_in_epoch_milliseconds() {
        for (case, instant, expected) in [
            ("the epoch", ::aws_smithy_types::DateTime::from_secs(0), 0),
            ("one second", ::aws_smithy_types::DateTime::from_secs(1), 1_000),
            (
                "a fractional second",
                ::aws_smithy_types::DateTime::from_secs_and_nanos(1, 500_000_000),
                1_500,
            ),
        ] {
            let millis = epoch_millis(&instant);
            assert_eq!(
                millis, expected,
                "case {case:?}: expected {expected}, got {millis}"
            );
        }
    }

    /// Sorting follows the attribute and the order, falls back to the id, and
    /// refuses an attribute or an order the model does not declare.
    #[test]
    fn sorting_follows_the_criteria_it_is_given() {
        let low = finding("c", "Low", "Policy:IAMUser/S3BucketPublic");
        let medium = finding("b", "Medium", "SensitiveData:S3Object/Financial");
        let high = finding("a", "High", "SensitiveData:S3Object/Credentials");

        for (case, criteria, expected) in [
            ("no criteria sorts by id", None, vec!["a", "b", "c"]),
            (
                "ascending by severity score",
                Some(SortCriteria {
                    attribute_name: Some("severity.score".to_string()),
                    order_by: None,
                }),
                vec!["c", "b", "a"],
            ),
            (
                "descending by severity score",
                Some(SortCriteria {
                    attribute_name: Some("severity.score".to_string()),
                    order_by: Some("DESC".to_string()),
                }),
                vec!["a", "b", "c"],
            ),
            (
                "ascending by type",
                Some(SortCriteria {
                    attribute_name: Some("type".to_string()),
                    order_by: Some("ASC".to_string()),
                }),
                vec!["c", "a", "b"],
            ),
        ] {
            let mut findings = vec![&medium, &high, &low];
            sort_findings(&mut findings, criteria.as_ref())
                .unwrap_or_else(|e| panic!("case {case:?}: the criteria must be accepted: {e}"));
            let order: Vec<&str> = findings.iter().map(|finding| finding.id()).collect();
            assert_eq!(
                order, expected,
                "case {case:?}: expected {expected:?}, got {order:?}"
            );
        }

        for (case, criteria) in [
            (
                "an attribute findings cannot be sorted by",
                SortCriteria {
                    attribute_name: Some("title".to_string()),
                    order_by: None,
                },
            ),
            (
                "an order the model does not declare",
                SortCriteria {
                    attribute_name: Some("count".to_string()),
                    order_by: Some("RANDOM".to_string()),
                },
            ),
        ] {
            let mut findings = vec![&medium, &high, &low];
            assert!(
                sort_findings(&mut findings, Some(&criteria)).is_err(),
                "case {case:?}: must be refused"
            );
        }
    }

    /// A criterion larger than this service filters on is refused before any
    /// finding is tested, so the work one request can charge is capped: the rows
    /// are the two factors, each at its bound and one past it.
    #[test]
    fn a_criterion_larger_than_the_bound_is_refused() {
        let value_rows = [
            ("at the value bound", MAX_VALUES_PER_CONDITION, true),
            ("one value past the bound", MAX_VALUES_PER_CONDITION + 1, false),
        ];
        for (case, count, accepted) in value_rows {
            let mut criterion = HashMap::new();
            criterion.insert(
                "type".to_string(),
                CriterionAdditionalProperties {
                    eq: Some(vec!["SensitiveData:S3Object/Financial".to_string(); count]),
                    ..CriterionAdditionalProperties::default()
                },
            );
            assert_eq!(
                validate_criterion(&criterion).is_ok(),
                accepted,
                "case {case:?}: {count} values must {} accepted",
                if accepted { "be" } else { "not be" }
            );
        }

        // The values of one condition are counted ACROSS the three operators
        // that carry a list, so they cannot be split to evade the bound.
        let mut split = HashMap::new();
        split.insert(
            "type".to_string(),
            CriterionAdditionalProperties {
                eq: Some(vec!["a".to_string(); MAX_VALUES_PER_CONDITION]),
                neq: Some(vec!["b".to_string()]),
                ..CriterionAdditionalProperties::default()
            },
        );
        assert!(
            validate_criterion(&split).is_err(),
            "case \"values split across operators\": the count is the sum over the operators"
        );

        // A property this service cannot filter on is refused whatever its size.
        let mut unknown = HashMap::new();
        unknown.insert(
            "notAProperty".to_string(),
            CriterionAdditionalProperties::default(),
        );
        assert!(
            validate_criterion(&unknown).is_err(),
            "case \"an unfilterable property\": must be refused"
        );

        // Every filterable property at once is inside the condition bound: a
        // criterion is keyed by property, so it can carry no more.
        let every: HashMap<String, CriterionAdditionalProperties> = FILTERABLE_PROPERTIES
            .iter()
            .map(|path| {
                (
                    (*path).to_string(),
                    CriterionAdditionalProperties::default(),
                )
            })
            .collect();
        assert!(
            validate_criterion(&every).is_ok(),
            "case \"every filterable property\": {} conditions must fit the bound of \
             {MAX_CONDITIONS_PER_CRITERION}",
            every.len()
        );
    }

    /// A finding carrying no value for the attribute sorts before one that does,
    /// in ascending order.
    #[test]
    fn a_missing_value_sorts_first_ascending() {
        let present = PropertyValue::Number(1);
        assert_eq!(
            compare(None, Some(&present)),
            std::cmp::Ordering::Less,
            "an absent value sorts before a present one"
        );
        assert_eq!(
            compare(Some(&present), Some(&present)),
            std::cmp::Ordering::Equal,
            "equal values tie"
        );
    }
}
