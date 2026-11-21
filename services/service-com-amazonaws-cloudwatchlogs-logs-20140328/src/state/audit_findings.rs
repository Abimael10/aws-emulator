// SPDX-License-Identifier: CC0-1.0

//! The AUDIT half of a log group's data protection policy: recording each detection of
//! sensitive data as a FINDING in the log group the policy names as its findings destination.
//!
//! A data protection policy carries two blocks over the same `DataIdentifier` array
//! ([`super::check_data_protection_policy_document`]): an `Audit` action, which "must contain
//! a `FindingsDestination` object", and a `Deidentify` action, which masks the data. The two
//! act on ONE ingestion and neither replaces the other — the stored event is masked
//! ([`super::data_protection::masked_where`]) AND the detection is recorded here — so a
//! client that alarms on the findings stream sees every ingestion the masking touched.
//!
//! A findings destination is "`FindingsDestination.CloudWatchLogs.LogGroup`", one of the
//! destination kinds the Audit action admits, and the only one this service OWNS: a Firehose
//! stream or an S3 bucket named there belongs to another service and is not written from here
//! ([`super::DataProtectionPolicyShape::findings_destination_log_groups`]). Delivery is into
//! the destination group's own state, so a finding is read back through this service's own
//! event reads — `FilterLogEvents` over the destination group, and `GetLogEvents` over the
//! stream it arrives in — like any other log event, and a destination log group the account
//! does not hold takes nothing.
//!
//! ## What a finding says, and what it does not
//!
//! A finding is one log event whose message is a JSON document naming the instant the audit
//! was taken, the log group the data was found in, and the data identifiers that were found
//! with how many terms of each. It NEVER carries the matched value: the detection it is built
//! from does not hold one ([`super::data_protection::Detected`]), which is the point of the
//! audit stream — "which data identifiers were seen in which log group" is reportable to a
//! reader who may not see the data itself.
//!
//! The spans are not written either: a finding states the COUNT of what each identifier
//! matched, which is what a detection record is about, and the positions of sensitive data in
//! a message are of no use to a reader that cannot read the message.

use ::serde_json::json;

/// The member of a finding naming the instant the audit was taken.
const AUDIT_TIMESTAMP_MEMBER: &str = "auditTimestamp";

/// The member of a finding naming the log group the data was found in.
const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The member of a finding listing the data identifiers that were found.
const DATA_IDENTIFIERS_MEMBER: &str = "dataIdentifiers";

/// The member of one listed identifier carrying its published name.
const IDENTIFIER_NAME_MEMBER: &str = "name";

/// The member of one listed identifier carrying how many terms of its data were found.
const IDENTIFIER_COUNT_MEMBER: &str = "count";

impl super::ComAmazonawsCloudwatchlogsLogs20140328State {
    /// The log groups the data protection policies in force for `group` send audit findings
    /// to: the ones the group's OWN policy names together with the ones the account-level data
    /// protection policy that applies to the group names.
    ///
    /// Both, on the same rule the identifiers resolve under: "the two policies are cumulative"
    /// (`PutAccountPolicy`), so an ingestion a group-level and an account-level policy both
    /// cover is audited to each of their destinations. A destination named twice is delivered
    /// to once.
    #[must_use]
    pub fn audit_findings_destinations_for(&self, group: &super::LogGroupRecord) -> Vec<String> {
        let mut destinations = group
            .data_protection_policy
            .as_ref()
            .map(|record| destinations_in(&record.policy_document))
            .unwrap_or_default();
        let account_level = super::applicable_account_policy(
            &self.account_policies,
            super::POLICY_TYPE_DATA_PROTECTION,
            group,
        );
        if let Some(policy) = account_level {
            for destination in destinations_in(&policy.policy_document) {
                if !destinations.contains(&destination) {
                    destinations.push(destination);
                }
            }
        }
        destinations
    }
}

/// The findings destination log groups one accepted policy document names.
fn destinations_in(document: &str) -> Vec<String> {
    super::check_data_protection_policy_document(document)
        .map(|policy| policy.findings_destination_log_groups)
        .unwrap_or_default()
}

/// The finding one audited message owes: the JSON document a destination log group receives
/// as the message of one log event.
///
/// `resource_arn` is the ARN of the log group the data was found in, and `audit_time_ms` the
/// instant the ingestion was audited at — the ingestion's own instant, since the audit is
/// taken as the event arrives. `found` is the detection the ingestion already took, so the
/// finding reports exactly what was masked.
///
/// `None` where nothing was found: an ingestion that carried none of the named data is audited
/// to nothing, which is what makes the findings stream a record of detections rather than of
/// ingestions.
#[must_use]
pub fn finding(
    resource_arn: &str,
    audit_time_ms: i64,
    found: &[super::data_protection::Detected],
) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let identifiers: Vec<::serde_json::Value> = found
        .iter()
        .map(|detected| {
            json!({
                IDENTIFIER_NAME_MEMBER: detected.name,
                IDENTIFIER_COUNT_MEMBER: detected.spans.len(),
            })
        })
        .collect();
    Some(
        json!({
            AUDIT_TIMESTAMP_MEMBER: audit_instant(audit_time_ms),
            RESOURCE_ARN_MEMBER: resource_arn,
            DATA_IDENTIFIERS_MEMBER: identifiers,
        })
        .to_string(),
    )
}

/// The instant a finding states its audit was taken at: the UTC calendar instant, to the
/// second, which is the precision an audit finding's `auditTimestamp` is written to.
fn audit_instant(audit_time_ms: i64) -> String {
    let whole_second = audit_time_ms - audit_time_ms.rem_euclid(1_000);
    ::aws_smithy_types::DateTime::from_millis(whole_second)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        // Every instant this type can hold renders; one that does not is stated as the epoch
        // milliseconds the ingestion carried rather than dropping the finding.
        .unwrap_or_else(|_| whole_second.to_string())
}

/// The log stream of a destination group that the findings about `source_group_name` arrive
/// in.
///
/// One stream per audited log group, named by that group, so a destination collecting the
/// findings of several groups keeps each group's findings in a stream of its own and a reader
/// can tell them apart by the stream they arrived in. Created on the first finding delivered
/// to it, the way any other stream of this service is created.
#[must_use]
pub fn findings_stream_name(source_group_name: &str) -> String {
    source_group_name.to_string()
}

/// Deliver `findings` — the documents one ingestion's audited events owe — into each of
/// `destinations`, as log events of the stream that destination holds the findings of
/// `source_group_name` in.
///
/// Written through the store's own records, so a finding is an ordinary log event of the
/// destination group: it counts towards the volume that group holds, moves its stream's event
/// times and is subject to the same storage ceiling as any ingested event. A destination the
/// account holds no log group under takes nothing: this service delivers only to a group that
/// exists, and reports no finding it did not store.
///
/// A finding is NOT itself audited: it carries no sensitive data to find, and auditing one
/// would make a policy whose destination is its own group ingest forever.
pub fn deliver(
    store: &mut super::ComAmazonawsCloudwatchlogsLogs20140328State,
    region: &str,
    source_group_name: &str,
    destinations: &[String],
    findings: &[String],
    audit_time_ms: i64,
) {
    if findings.is_empty() {
        return;
    }
    let stream_name = findings_stream_name(source_group_name);
    for destination in destinations {
        if !store.log_groups.contains_key(destination) {
            continue;
        }
        // A finding is written INTO the destination group, so it is encrypted under the key
        // that group is associated with — the destination's own, never the audited group's.
        let destination_kms_key_id = store
            .log_groups
            .get(destination)
            .and_then(|group| group.kms_key_id.clone());
        // The sequence numbers come from the store's one monotonic counter, so a finding is
        // ordered against every other event the store holds; they are taken before the
        // destination group is borrowed to write them.
        //
        // A finding is an ordinary log event of the destination group, so the transformer in
        // force for THAT group transforms it as it is stored, exactly as it transforms
        // anything else ingested there ([`super::active_transformer`]).
        let pipeline = store
            .log_groups
            .get(destination)
            .map(|group| super::active_transformer::pipeline(store, group))
            .unwrap_or_default();
        let events: Vec<super::StoredEvent> = findings
            .iter()
            .map(|document| super::StoredEvent {
                timestamp: audit_time_ms,
                message: document.clone(),
                masked_message: None,
                transformed_message: pipeline.transform(document),
                ingestion_time: audit_time_ms,
                ingestion_seq: store.take_ingestion_seq(),
                ingest_kms_key_id: destination_kms_key_id.clone(),
            })
            .collect();
        let Some(group) = store.log_groups.get_mut(destination) else {
            continue;
        };
        let stream = group
            .streams
            .entry(stream_name.clone())
            .or_insert_with(|| {
                super::LogStreamRecord::new(region, destination, &stream_name, audit_time_ms)
            });
        let appends_in_order = stream
            .events
            .last()
            .zip(events.first())
            .is_none_or(|(newest, first)| first.key() > newest.key());
        stream.events.extend(events);
        if !appends_in_order {
            stream.events.sort_by_key(super::StoredEvent::key);
        }
        stream.enforce_storage_ceiling();
        stream.refresh_event_times();
        stream.last_ingestion_time = Some(audit_time_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::data_protection::Detected;

    /// One row of the finding table: what a failing assertion calls it, the detection taken,
    /// and the `(identifier, count)` pairs the finding must state.
    type FindingRow = (&'static str, Vec<Detected>, &'static [(&'static str, usize)]);

    /// RULE: a finding names the audit instant, the log group the data was found in and each
    /// identifier found with the count of its terms — and carries no value of the message.
    #[test]
    fn a_finding_names_the_identifiers_found_and_no_value() {
        let resource = "arn:aws:logs:eu-west-2:000000000000:log-group:/protected";
        // The rows differ along what the rule generalises over: how many identifiers were
        // found, and how many terms of one of them.
        let rows: &[FindingRow] = &[
            (
                "one identifier found once",
                vec![Detected {
                    name: "EmailAddress",
                    spans: vec![(12, 27)],
                }],
                &[("EmailAddress", 1)],
            ),
            (
                "one identifier found twice",
                vec![Detected {
                    name: "EmailAddress",
                    spans: vec![(0, 15), (23, 39)],
                }],
                &[("EmailAddress", 2)],
            ),
            (
                "two identifiers found",
                vec![
                    Detected {
                        name: "EmailAddress",
                        spans: vec![(0, 15)],
                    },
                    Detected {
                        name: "IpAddress",
                        spans: vec![(21, 32)],
                    },
                ],
                &[("EmailAddress", 1), ("IpAddress", 1)],
            ),
        ];
        for (case, found, expected) in rows {
            let document = finding(resource, 1_767_225_600_123, found)
                .unwrap_or_else(|| panic!("case {case:?}: a detection owes a finding"));
            let parsed: ::serde_json::Value = ::serde_json::from_str(&document)
                .unwrap_or_else(|_| panic!("case {case:?}: a finding is JSON, got {document}"));
            assert_eq!(
                parsed[AUDIT_TIMESTAMP_MEMBER].as_str(),
                Some("2026-01-01T00:00:00Z"),
                "case {case:?}: expected the audit instant to the second, got {document}"
            );
            assert_eq!(
                parsed[RESOURCE_ARN_MEMBER].as_str(),
                Some(resource),
                "case {case:?}: expected the audited log group's ARN, got {document}"
            );
            let reported: Vec<(String, u64)> = parsed[DATA_IDENTIFIERS_MEMBER]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a finding lists identifiers, got {document}"))
                .iter()
                .map(|entry| {
                    (
                        entry[IDENTIFIER_NAME_MEMBER]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        entry[IDENTIFIER_COUNT_MEMBER].as_u64().unwrap_or_default(),
                    )
                })
                .collect();
            let wanted: Vec<(String, u64)> = expected
                .iter()
                .map(|(name, count)| ((*name).to_string(), *count as u64))
                .collect();
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected {wanted:?}, got {reported:?}"
            );
            // A finding states those three members and nothing else, and one listed
            // identifier its name and its count and nothing else: no span of the message, and
            // no value from it, can ride along.
            let mut members: Vec<&str> = parsed
                .as_object()
                .unwrap_or_else(|| panic!("case {case:?}: a finding is a JSON object, got {document}"))
                .keys()
                .map(String::as_str)
                .collect();
            members.sort_unstable();
            let mut expected_members = vec![
                AUDIT_TIMESTAMP_MEMBER,
                DATA_IDENTIFIERS_MEMBER,
                RESOURCE_ARN_MEMBER,
            ];
            expected_members.sort_unstable();
            assert_eq!(
                members, expected_members,
                "case {case:?}: expected only the finding's own members, got {document}"
            );
            let listed_identifiers = parsed[DATA_IDENTIFIERS_MEMBER].as_array().cloned();
            for entry in listed_identifiers.unwrap_or_default() {
                let mut listed: Vec<&str> = entry
                    .as_object()
                    .unwrap_or_else(|| panic!("case {case:?}: an identifier is an object, got {entry}"))
                    .keys()
                    .map(String::as_str)
                    .collect();
                listed.sort_unstable();
                let mut expected_listed = vec![IDENTIFIER_COUNT_MEMBER, IDENTIFIER_NAME_MEMBER];
                expected_listed.sort_unstable();
                assert_eq!(
                    listed, expected_listed,
                    "case {case:?}: expected only the identifier's name and count, got {entry}"
                );
            }
        }
    }

    /// RULE: an ingestion that carried none of the named data owes no finding.
    #[test]
    fn nothing_found_owes_no_finding() {
        let document = finding("arn:aws:logs:eu-west-2:000000000000:log-group:/protected", 0, &[]);
        assert_eq!(
            document, None,
            "case \"a message carrying none of the named data\": got {document:?}"
        );
    }
}
