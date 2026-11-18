// SPDX-License-Identifier: CC0-1.0

//! The aggregation a findings STATISTICS query runs over the findings plane:
//! which property a request groups findings by, how the groups it asks for are
//! counted, and the order and the number of groups a response reports.
//!
//! `GetFindingStatistics` is this service's only read that AGGREGATES findings
//! rather than projecting them, so the grouping lives here — beside the records
//! in [`super::findings`] and the query that selects them — rather than inside
//! that handler.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::ops::types::{
    Finding, FindingStatisticsSortCriteria, GroupCount,
    FINDING_STATISTICS_SORT_ATTRIBUTE_NAME_VALUES, GROUP_BY_VALUES,
};

use super::findings::{
    finding_property, unknown_order_message, PropertyValue, StoredFinding, ORDER_ASCENDING,
    ORDER_DESCENDING,
};

/// The one group property that is NOT also a property findings can be filtered
/// on: the classification job that produced the finding, an entry of
/// [`GROUP_BY_VALUES`].
pub const GROUP_BY_JOB_ID: &str = "classificationDetails.jobId";

/// Sort the groups by the number of findings in each: an entry of
/// [`FINDING_STATISTICS_SORT_ATTRIBUTE_NAME_VALUES`], documented as "count, sort
/// the results by the number of findings in each group of results".
pub const COUNT_ATTRIBUTE: &str = "count";

/// Sort the groups by their key: the other entry, documented as "groupKey, sort
/// the results by the name of each group of results".
pub const GROUP_KEY_ATTRIBUTE: &str = "groupKey";

/// The attribute the groups are ordered by when a request names no
/// `attributeName`. The group key is what names a group, and it is unique
/// across the groups of one query, so ordering by it is total — which is what
/// makes a response that names no sort criteria deterministic.
pub const DEFAULT_STATISTICS_SORT_ATTRIBUTE: &str = GROUP_KEY_ATTRIBUTE;

/// The groups one response carries when a request names no `size`. That member
/// is a bare `__integer` with neither a modelled range nor a documented default,
/// so this is the EMULATOR's choice, the same one its paginated reads make
/// ([`super::DEFAULT_PAGE_SIZE`]).
pub const DEFAULT_STATISTICS_GROUPS: usize = super::DEFAULT_PAGE_SIZE;

/// The most groups one response carries, however large a `size` a request asks
/// for: the EMULATOR's own bound on the amplification between a tiny request and
/// the response it produces, the same one its paginated reads apply.
pub const MAX_STATISTICS_GROUPS: usize = super::MAX_PAGE_SIZE;

/// How the service refuses a query that groups findings by a property it does
/// not aggregate them on. The only variable part is the property the request
/// named.
pub fn ungroupable_property_message(group_by: &str) -> String {
    format!("The groupBy names a property that findings cannot be grouped by: {group_by}")
}

/// How the service refuses a `sortCriteria` naming a grouping the results cannot
/// be sorted by.
pub fn unsortable_group_attribute_message(name: &str) -> String {
    format!("The sortCriteria names a grouping that the results cannot be sorted by: {name}")
}

/// Refuse a `groupBy` this service does not aggregate findings on.
///
/// Checked BEFORE the store is read, so a property the service cannot group by
/// is refused whether or not the region holds a finding to group.
pub fn validate_group_by(group_by: &str) -> Result<(), String> {
    if GROUP_BY_VALUES.contains(&group_by) {
        Ok(())
    } else {
        Err(ungroupable_property_message(group_by))
    }
}

/// The group `finding` falls in when findings are grouped by `group_by`, or
/// `None` when this finding carries no value for that property — a policy
/// finding was produced by no classification job, so it belongs to no group of
/// a query grouping by one.
///
/// Every group property but the classification job is also a property findings
/// can be filtered on, so those delegate to [`finding_property`]: one query
/// cannot read a property differently from the other.
pub fn finding_group_key(finding: &Finding, group_by: &str) -> Option<String> {
    if !GROUP_BY_VALUES.contains(&group_by) {
        return None;
    }
    match group_by {
        GROUP_BY_JOB_ID => finding
            .classification_details
            .as_ref()?
            .job_id
            .clone()
            .map(PropertyValue::Text),
        other => finding_property(finding, other),
    }
    .map(|value| value.as_text())
}

/// The grouping to order the groups by and whether to reverse that order, or the
/// refusal for a `sortCriteria` this service cannot serve.
///
/// A request naming no `sortCriteria`, or naming one without an
/// `attributeName`, is served in [`DEFAULT_STATISTICS_SORT_ATTRIBUTE`] order;
/// one naming no `orderBy` is served ascending, the first order the modelled
/// `OrderBy` declares.
fn statistics_sort_plan(
    criteria: Option<&FindingStatisticsSortCriteria>,
) -> Result<(&'static str, bool), String> {
    let Some(criteria) = criteria else {
        return Ok((DEFAULT_STATISTICS_SORT_ATTRIBUTE, false));
    };
    let descending = match criteria.order_by.as_deref() {
        None | Some(ORDER_ASCENDING) => false,
        Some(ORDER_DESCENDING) => true,
        Some(other) => return Err(unknown_order_message(other)),
    };
    let attribute = match criteria.attribute_name.as_deref() {
        None => DEFAULT_STATISTICS_SORT_ATTRIBUTE,
        Some(COUNT_ATTRIBUTE) => COUNT_ATTRIBUTE,
        Some(GROUP_KEY_ATTRIBUTE) => GROUP_KEY_ATTRIBUTE,
        Some(other) => return Err(unsortable_group_attribute_message(other)),
    };
    debug_assert!(FINDING_STATISTICS_SORT_ATTRIBUTE_NAME_VALUES.contains(&attribute));
    Ok((attribute, descending))
}

/// Refuse a `sortCriteria` this service cannot serve, without reading the store.
pub fn validate_statistics_sort_criteria(
    criteria: Option<&FindingStatisticsSortCriteria>,
) -> Result<(), String> {
    statistics_sort_plan(criteria).map(|_| ())
}

/// How many groups a response carries for a request asking for `size` of them:
/// an absent or non-positive `size` takes [`DEFAULT_STATISTICS_GROUPS`] and
/// anything larger than [`MAX_STATISTICS_GROUPS`] is capped there.
pub fn statistics_group_limit(size: Option<i32>) -> usize {
    match size {
        Some(requested) if requested > 0 => usize::try_from(requested)
            .unwrap_or(MAX_STATISTICS_GROUPS)
            .min(MAX_STATISTICS_GROUPS),
        _ => DEFAULT_STATISTICS_GROUPS,
    }
}

/// One `GroupCount` per group of `findings` under `group_by`, in the order
/// `criteria` asks for, at most `limit` of them.
///
/// Every finding carrying a value for the group property is counted in exactly
/// one group, so the counts sum to the number of findings the query selected
/// that report that property. A finding reporting no value for it belongs to no
/// group ([`finding_group_key`]), so it is counted in none.
pub fn group_counts<'a, I>(
    findings: I,
    group_by: &str,
    criteria: Option<&FindingStatisticsSortCriteria>,
    limit: usize,
) -> Result<Vec<GroupCount>, String>
where
    I: IntoIterator<Item = &'a StoredFinding>,
{
    let (attribute, descending) = statistics_sort_plan(criteria)?;

    let mut tally: BTreeMap<String, i64> = BTreeMap::new();
    for stored in findings {
        if let Some(key) = finding_group_key(&stored.finding, group_by) {
            let count = tally.entry(key).or_insert(0);
            *count = count.saturating_add(1);
        }
    }

    let mut groups: Vec<(String, i64)> = tally.into_iter().collect();
    // The group key is the final tie-break under either grouping, so two groups
    // holding the same number of findings keep one fixed relative order across
    // calls.
    groups.sort_by(|(left_key, left_count), (right_key, right_count)| {
        let ordering = match attribute {
            COUNT_ATTRIBUTE => left_count.cmp(right_count),
            _ => Ordering::Equal,
        }
        .then_with(|| left_key.cmp(right_key));
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    groups.truncate(limit);

    Ok(groups
        .into_iter()
        .map(|(group_key, count)| GroupCount {
            count: Some(count),
            group_key: Some(group_key),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        finding_group_key, group_counts, statistics_group_limit, statistics_sort_plan,
        validate_group_by, validate_statistics_sort_criteria, COUNT_ATTRIBUTE,
        DEFAULT_STATISTICS_GROUPS, DEFAULT_STATISTICS_SORT_ATTRIBUTE, GROUP_BY_JOB_ID,
        GROUP_KEY_ATTRIBUTE, MAX_STATISTICS_GROUPS,
    };
    use crate::ops::types::{
        ClassificationDetails, Finding, FindingStatisticsSortCriteria, ResourcesAffected, S3Bucket,
        Severity, GROUP_BY_VALUES, ORDER_BY_VALUES,
    };
    use crate::state::StoredFinding;
    use std::time::SystemTime;

    /// A finding carrying one value for each property a query may group by.
    fn finding(id: &str, severity: &str, finding_type: &str, bucket: &str) -> StoredFinding {
        StoredFinding {
            region: "us-west-2".to_string(),
            created_at: SystemTime::UNIX_EPOCH,
            finding: Finding {
                id: Some(id.to_string()),
                r#type: Some(finding_type.to_string()),
                severity: Some(Severity {
                    description: Some(severity.to_string()),
                    score: None,
                }),
                resources_affected: Some(ResourcesAffected {
                    s3_bucket: Some(S3Bucket {
                        name: Some(bucket.to_string()),
                        ..S3Bucket::default()
                    }),
                    s3_object: None,
                }),
                ..Finding::default()
            },
        }
    }

    /// Every property the model lets a query group by is one this service reads
    /// a group key for, and nothing else is a property it groups by.
    #[test]
    fn every_modelled_group_property_is_one_this_service_groups_by() {
        for group_by in GROUP_BY_VALUES {
            assert!(
                validate_group_by(group_by).is_ok(),
                "case {group_by:?}: a modelled GroupBy value must be accepted"
            );
        }
        for group_by in ["severity.score", "accountId", "", "type "] {
            let refused = validate_group_by(group_by);
            assert!(
                refused.is_err_and(|message| message.contains(group_by)),
                "case {group_by:?}: a property the model does not declare must be refused, \
                 naming it"
            );
            assert_eq!(
                finding_group_key(
                    &finding("a", "Low", "Policy:IAMUser/S3BucketPublic", "b").finding,
                    group_by
                ),
                None,
                "case {group_by:?}: a property findings are not grouped by reads no group key"
            );
        }
    }

    /// A finding falls in the group its value for the grouped property names,
    /// and in none when it reports no value for it.
    #[test]
    fn a_group_key_is_the_findings_value_for_the_grouped_property() {
        let mut sensitive = finding(
            "a",
            "High",
            "SensitiveData:S3Object/Personal",
            "amzn-s3-demo-bucket",
        );
        sensitive.finding.classification_details = Some(ClassificationDetails {
            job_id: Some("job-1".to_string()),
            ..ClassificationDetails::default()
        });
        let policy = finding("b", "Low", "Policy:IAMUser/S3BucketPublic", "other-bucket");

        for (case, stored, group_by, expected) in [
            (
                "severity of a sensitive data finding",
                &sensitive,
                "severity.description",
                Some("High"),
            ),
            (
                "type of a policy finding",
                &policy,
                "type",
                Some("Policy:IAMUser/S3BucketPublic"),
            ),
            (
                "bucket of a policy finding",
                &policy,
                "resourcesAffected.s3Bucket.name",
                Some("other-bucket"),
            ),
            (
                "job of a finding a job produced",
                &sensitive,
                GROUP_BY_JOB_ID,
                Some("job-1"),
            ),
            (
                "job of a finding no job produced",
                &policy,
                GROUP_BY_JOB_ID,
                None,
            ),
        ] {
            let key = finding_group_key(&stored.finding, group_by);
            assert_eq!(
                key.as_deref(),
                expected,
                "case {case:?}: grouping by {group_by:?} expected {expected:?}, got {key:?}"
            );
        }
    }

    /// The counts of a grouping partition the findings that report the grouped
    /// property: every group is non-empty and they sum to that population.
    #[test]
    fn the_counts_of_a_grouping_partition_the_findings() {
        let findings = vec![
            finding("a", "High", "SensitiveData:S3Object/Personal", "one"),
            finding("b", "High", "Policy:IAMUser/S3BucketPublic", "one"),
            finding("c", "Low", "Policy:IAMUser/S3BucketPublic", "two"),
        ];
        for (group_by, expected) in [
            ("severity.description", vec![("High", 2), ("Low", 1)]),
            (
                "type",
                vec![
                    ("Policy:IAMUser/S3BucketPublic", 2),
                    ("SensitiveData:S3Object/Personal", 1),
                ],
            ),
            (
                "resourcesAffected.s3Bucket.name",
                vec![("one", 2), ("two", 1)],
            ),
            (GROUP_BY_JOB_ID, vec![]),
        ] {
            let groups = group_counts(&findings, group_by, None, MAX_STATISTICS_GROUPS)
                .expect("no sort criteria is served");
            let reported: Vec<(String, i64)> = groups
                .iter()
                .map(|group| {
                    (
                        group.group_key.clone().unwrap_or_default(),
                        group.count.unwrap_or_default(),
                    )
                })
                .collect();
            let expected: Vec<(String, i64)> = expected
                .into_iter()
                .map(|(key, count)| (key.to_string(), count))
                .collect();
            assert_eq!(
                reported, expected,
                "case {group_by:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// The groups are reported in the order the sort criteria ask for, over
    /// every pair of grouping and order the model declares, and a criteria the
    /// model does not declare is refused.
    #[test]
    fn the_groups_are_reported_in_the_order_the_criteria_ask_for() {
        let findings = vec![
            finding("a", "High", "SensitiveData:S3Object/Personal", "one"),
            finding("b", "High", "Policy:IAMUser/S3BucketPublic", "one"),
            finding("c", "Low", "Policy:IAMUser/S3BucketPublic", "two"),
            finding("d", "Medium", "Policy:IAMUser/S3BucketPublic", "two"),
        ];
        for attribute in [None, Some(COUNT_ATTRIBUTE), Some(GROUP_KEY_ATTRIBUTE)] {
            for order in [None, Some("ASC"), Some("DESC")] {
                let criteria = FindingStatisticsSortCriteria {
                    attribute_name: attribute.map(str::to_string),
                    order_by: order.map(str::to_string),
                };
                let groups =
                    group_counts(&findings, "severity.description", Some(&criteria), MAX_STATISTICS_GROUPS)
                        .expect("a modelled sort criteria is served");
                let reported: Vec<(String, i64)> = groups
                    .iter()
                    .map(|group| {
                        (
                            group.group_key.clone().unwrap_or_default(),
                            group.count.unwrap_or_default(),
                        )
                    })
                    .collect();
                let case = format!("{attribute:?}/{order:?}");
                let mut expected = vec![
                    ("High".to_string(), 2_i64),
                    ("Low".to_string(), 1),
                    ("Medium".to_string(), 1),
                ];
                if attribute == Some(COUNT_ATTRIBUTE) {
                    expected.sort_by(|left, right| {
                        left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0))
                    });
                }
                if order == Some("DESC") {
                    expected.reverse();
                }
                assert_eq!(
                    reported, expected,
                    "case {case}: expected {expected:?}, got {reported:?}"
                );
            }
        }
        for order in ORDER_BY_VALUES {
            let criteria = FindingStatisticsSortCriteria {
                attribute_name: Some("severity".to_string()),
                order_by: Some((*order).to_string()),
            };
            assert!(
                validate_statistics_sort_criteria(Some(&criteria))
                    .is_err_and(|message| message.contains("severity")),
                "case {order:?}: a grouping the model does not declare must be refused"
            );
        }
        let sideways = FindingStatisticsSortCriteria {
            attribute_name: Some(COUNT_ATTRIBUTE.to_string()),
            order_by: Some("SIDEWAYS".to_string()),
        };
        assert!(
            validate_statistics_sort_criteria(Some(&sideways))
                .is_err_and(|message| message.contains("SIDEWAYS")),
            "an order the model does not declare must be refused"
        );
        assert_eq!(
            statistics_sort_plan(None).expect("no criteria is served"),
            (DEFAULT_STATISTICS_SORT_ATTRIBUTE, false),
            "a request naming no sortCriteria is served in the default order"
        );
    }

    /// `size` bounds the groups a response carries, and an absent or
    /// out-of-range one takes the service's own default and maximum.
    #[test]
    fn size_bounds_the_groups_a_response_carries() {
        for (size, expected) in [
            (None, DEFAULT_STATISTICS_GROUPS),
            (Some(0), DEFAULT_STATISTICS_GROUPS),
            (Some(-7), DEFAULT_STATISTICS_GROUPS),
            (Some(1), 1),
            (Some(3), 3),
            (Some(i32::MAX), MAX_STATISTICS_GROUPS),
        ] {
            let limit = statistics_group_limit(size);
            assert_eq!(
                limit, expected,
                "case {size:?}: expected a limit of {expected}, got {limit}"
            );
        }

        let findings = vec![
            finding("a", "High", "SensitiveData:S3Object/Personal", "one"),
            finding("b", "Medium", "Policy:IAMUser/S3BucketPublic", "two"),
            finding("c", "Low", "Policy:IAMUser/S3BucketPublic", "three"),
        ];
        for limit in 0..=4 {
            let groups = group_counts(&findings, "severity.description", None, limit)
                .expect("no sort criteria is served");
            assert_eq!(
                groups.len(),
                limit.min(3),
                "case {limit}: a response carries at most {limit} groups, got {groups:?}"
            );
        }
    }
}
