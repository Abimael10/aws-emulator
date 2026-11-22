// SPDX-License-Identifier: CC0-1.0

//! What Config's READ paths report, and how it draws the identifiers it assigns.
//!
//! Four rules are pinned here, each over a table of inputs rather than one case:
//!
//! 1. A reporting read of the configuration inventory answers from what the inventory
//!    holds, whatever recorders the account has: an account that has recorded nothing
//!    has discovered nothing, which is an empty result, never a refusal.
//! 2. A grouped aggregate conformance-pack compliance summary reports one row per
//!    source group the aggregator takes in and the request's filters admit, with zero
//!    counts for a group that holds no conformance pack — the absence of packs is a
//!    zero count, not an absent row.
//! 3. An aggregation source's sync status describes a relationship data has already
//!    moved across, so an aggregator reports none until the aggregation of its current
//!    sources has completed, which is work the clock owes.
//! 4. Every identifier Config assigns is drawn from randomness in the form AWS
//!    publishes for it, so it carries neither the creation order nor a hint of the next
//!    id.
//! 5. An identifier Config spells as a uuid is written in the canonical hyphenated
//!    36-character form, and that exact text is what every read of the resource echoes
//!    and what the ARN built around it carries.
//!
//! Every case drives the emulator the way a client does — an `awsJson1_1` `POST /`
//! through the same `MultiServiceRouter` the production router builds — reusing the
//! `Emulator` harness `config_setup_and_rule_loop` defines.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::state::{
    service_state, AGGREGATION_COMPLETES_AFTER, SERVICE_LINKED_PRINCIPALS,
};

use crate::config_setup_and_rule_loop::{managed_rule, recorder_request, Emulator, ROLE_ARN};

/// Resource types the rows use: entries of
/// `com.amazonaws.configservice#ResourceType`, from `smithy_query(query="shape",
/// shape="ResourceType")`.
const EC2_VOLUME: &str = "AWS::EC2::Volume";
/// See [`EC2_VOLUME`].
const DYNAMODB_TABLE: &str = "AWS::DynamoDB::Table";
/// See [`EC2_VOLUME`].
const S3_BUCKET: &str = "AWS::S3::Bucket";

/// The aggregator every aggregate row below reads through.
const AGGREGATOR: &str = "read-paths-aggregator";

/// The `FailureReason` Config answers for a proactive execution the account has no
/// proactive rule for: a fixed sentence that carries no part of the request.
const NO_PROACTIVE_RULE_REASON: &str =
    "No Config rule is enabled in proactive mode for the given resource type.";

/// Past the span an execution's terminal status falls due after
/// (`crate::state::EXECUTION_SETTLES_AFTER`), so the summary a row reads is the
/// terminal one.
const PAST_THE_EXECUTION_SPAN: Duration = Duration::from_millis(700);

/// How many ids a row mints when it checks that an assigned id is drawn rather than
/// counted.
const IDS_PER_MINT: usize = 8;

/// The widths of the five hyphen-separated groups a uuid's canonical form is written
/// in, `8-4-4-4-12` hexadecimal digits — the one spelling AWS publishes a uuid in. The
/// 36 characters they come to are also the modelled maximum of `StoredQuery$QueryId`
/// (`smithy_query(query="shape", shape="QueryId")`: length 1..36, pattern `^\S+$`).
const UUID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

/// The length of a canonical uuid: its five groups and the four hyphens between them.
const UUID_LENGTH: usize = 36;

/// Refuse `id` unless it is one uuid in the canonical hyphenated form, naming `case`
/// and the `member` it was read from.
fn assert_canonical_uuid(case: &str, member: &str, id: &str) {
    let widths: Vec<usize> = id.split('-').map(|group| group.chars().count()).collect();
    assert_eq!(
        widths.as_slice(),
        UUID_GROUPS.as_slice(),
        "case {case:?}: expected {member} in the uuid groups {UUID_GROUPS:?}, got {id:?}"
    );
    assert_eq!(
        id.chars().count(),
        UUID_LENGTH,
        "case {case:?}: expected {member} to be {UUID_LENGTH} characters, got {id:?}"
    );
    assert!(
        id.chars().all(|character| character == '-'
            || (character.is_ascii_hexdigit() && !character.is_ascii_uppercase())),
        "case {case:?}: expected {member} in lowercase hexadecimal and hyphens, got {id:?}"
    );
}

/// The recorder states a read of the inventory is served under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorders {
    /// The account has no configuration recorder at all.
    None,
    /// Only a service-linked recorder, which records for the service it is linked to.
    ServiceLinked,
    /// The account's own customer managed recorder.
    CustomerManaged,
}

/// Bring `emulator`'s account to `recorders`.
async fn with_recorders(emulator: &Emulator, recorders: Recorders) {
    match recorders {
        Recorders::None => {}
        Recorders::ServiceLinked => {
            emulator
                .ok(
                    "PutServiceLinkedConfigurationRecorder",
                    &json!({ "ServicePrincipal": SERVICE_LINKED_PRINCIPALS[0] }),
                )
                .await;
        }
        Recorders::CustomerManaged => {
            emulator
                .ok(
                    "PutConfigurationRecorder",
                    &recorder_request(json!({ "roleARN": ROLE_ARN })),
                )
                .await;
        }
    }
}

#[tokio::test]
async fn an_inventory_read_reports_what_the_inventory_holds_whatever_recorders_exist() {
    // Rows differ along the read and the shape of the request it is asked with; each is
    // asked of an account under each recorder state, where the inventory is empty, so
    // the answer under test is "nothing discovered" rather than a refusal.
    let reads: [(&str, Value, &str); 5] = [
        (
            "a bare discovery listing",
            json!({ "resourceType": EC2_VOLUME }),
            "resourceIdentifiers",
        ),
        (
            "a discovery listing narrowed by resource id",
            json!({ "resourceType": DYNAMODB_TABLE, "resourceIds": ["orders"] }),
            "resourceIdentifiers",
        ),
        (
            "a discovery listing including deleted resources",
            json!({ "resourceType": S3_BUCKET, "includeDeletedResources": true }),
            "resourceIdentifiers",
        ),
        (
            "a batched configuration-item read of one key",
            json!({ "resourceKeys": [{ "resourceType": EC2_VOLUME, "resourceId": "vol-0001" }] }),
            "baseConfigurationItems",
        ),
        (
            "a batched configuration-item read of several keys",
            json!({
                "resourceKeys": [
                    { "resourceType": EC2_VOLUME, "resourceId": "vol-0001" },
                    { "resourceType": S3_BUCKET, "resourceId": "config-bucket" },
                ],
            }),
            "baseConfigurationItems",
        ),
    ];

    for (case, request, reported) in reads {
        let operation = if reported == "resourceIdentifiers" {
            "ListDiscoveredResources"
        } else {
            "BatchGetResourceConfig"
        };
        for recorders in [
            Recorders::None,
            Recorders::ServiceLinked,
            Recorders::CustomerManaged,
        ] {
            let case = format!("{case} with {recorders:?}");
            let emulator = Emulator::new();
            with_recorders(&emulator, recorders).await;

            let (status, body) = emulator.call(operation, &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a report of the inventory is served, got {status} with {body}"
            );
            assert_eq!(
                body[reported].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: expected an empty {reported} list, got {body}"
            );
            // The batched read also reports that it processed every key it was handed.
            if operation == "BatchGetResourceConfig" {
                assert_eq!(
                    body["unprocessedResourceKeys"].as_array().map(Vec::len),
                    Some(0),
                    "case {case:?}: every key was processed, so none is unprocessed, got {body}"
                );
            }
        }
    }
}

/// An account aggregation source over this account, aggregating `regions` or every
/// Region when `regions` is empty.
fn account_source(account_id: &str, regions: &[&str]) -> Value {
    let mut source = json!({ "AccountIds": [account_id] });
    if regions.is_empty() {
        source["AllAwsRegions"] = json!(true);
    } else {
        source["AwsRegions"] = json!(regions);
    }
    source
}

/// Create [`AGGREGATOR`] over this account in every Region.
async fn put_whole_account_aggregator(emulator: &Emulator) {
    let account_id = emulator_server::emulator_account_id();
    emulator
        .ok(
            "PutConfigurationAggregator",
            &json!({
                "ConfigurationAggregatorName": AGGREGATOR,
                "AccountAggregationSources": [account_source(account_id, &[])],
            }),
        )
        .await;
}

/// Move [`AGGREGATOR`]'s sources far enough into the past that the clock already owes
/// it the aggregation of those sources, the way the recording pump's own clock-owed
/// work is waited out — without sleeping through the span.
fn finish_aggregating(emulator: &Emulator) {
    let state = service_state(&*emulator.engine).expect("the test engine is the shared engine");
    let mut store = state.lock();
    let aggregator = store
        .configuration_aggregators
        .get_mut(AGGREGATOR)
        .expect("the aggregator under test is in state");
    aggregator.last_updated_at -= AGGREGATION_COMPLETES_AFTER;
}

/// The groups one aggregate summary response reports, as `(GroupName, the summary's
/// counts in the order the members are named)`.
fn groups(case: &str, body: &Value, list: &str, counts: [&str; 2]) -> Vec<(String, [i64; 2])> {
    body[list]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {list} list, got {body}"))
        .iter()
        .map(|entry| {
            let name = entry["GroupName"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a GroupName, got {entry}"))
                .to_string();
            let summary = &entry["ComplianceSummary"];
            // A count the response leaves out is the modelled default of 0.
            let read = |member: &str| {
                summary
                    .get(member)
                    .and_then(Value::as_i64)
                    .or_else(|| summary[member]["CappedCount"].as_i64())
                    .unwrap_or_default()
            };
            (name, [read(counts[0]), read(counts[1])])
        })
        .collect()
}

#[tokio::test]
async fn a_conformance_pack_summary_reports_a_zero_valued_row_for_a_group_that_holds_no_pack() {
    // Rows differ along what the aggregated account holds and along the request's own
    // Filters: a group the aggregator takes in and the filters admit is REPORTED
    // whatever it holds, with zero counts when it holds no conformance pack, while a
    // group the filters exclude is no group at all.
    struct Case {
        case: &'static str,
        /// Whether the account holds one Config rule — a resource the summary does not
        /// count, so the row stays zero-valued.
        holds_a_rule: bool,
        /// The `Filters` the request carries, if any.
        filters: Option<Value>,
        /// Whether a row is expected for this account's group.
        reported: bool,
    }

    let account_id = emulator_server::emulator_account_id();
    let cases = [
        Case {
            case: "nothing in the account",
            holds_a_rule: false,
            filters: None,
            reported: true,
        },
        Case {
            case: "a Config rule but no conformance pack",
            holds_a_rule: true,
            filters: None,
            reported: true,
        },
        Case {
            case: "a filter naming this account",
            holds_a_rule: false,
            filters: Some(json!({ "AccountId": account_id })),
            reported: true,
        },
        Case {
            case: "a filter naming another account",
            holds_a_rule: false,
            filters: Some(json!({ "AccountId": "111122223333" })),
            reported: false,
        },
        Case {
            case: "a filter naming another Region",
            holds_a_rule: false,
            filters: Some(json!({ "AwsRegion": "eu-west-3" })),
            reported: false,
        },
    ];

    const LIST: &str = "AggregateConformancePackComplianceSummaries";
    const COUNTS: [&str; 2] = [
        "CompliantConformancePackCount",
        "NonCompliantConformancePackCount",
    ];

    for case in cases {
        let emulator = Emulator::new();
        if case.holds_a_rule {
            emulator
                .ok(
                    "PutConfigurationRecorder",
                    &recorder_request(json!({ "roleARN": ROLE_ARN })),
                )
                .await;
            emulator
                .ok("PutConfigRule", &managed_rule("read-paths-rule"))
                .await;
        }
        put_whole_account_aggregator(&emulator).await;

        let mut request = json!({ "ConfigurationAggregatorName": AGGREGATOR });
        if let Some(filters) = case.filters.clone() {
            request["Filters"] = filters;
        }
        let body = emulator
            .ok("GetAggregateConformancePackComplianceSummary", &request)
            .await;
        let reported = groups(case.case, &body, LIST, COUNTS);
        let expected = if case.reported {
            vec![(account_id.to_string(), [0, 0])]
        } else {
            Vec::new()
        };
        assert_eq!(
            reported, expected,
            "case {:?}: expected {expected:?}, got {body}",
            case.case
        );

        // The read is a report: a second read sees exactly the same rows.
        let again = emulator
            .ok("GetAggregateConformancePackComplianceSummary", &request)
            .await;
        assert_eq!(
            again, body,
            "case {:?}: reading a summary must not change what a later read sees, got {again}",
            case.case
        );
    }
}

#[tokio::test]
async fn an_aggregator_reports_no_source_status_until_it_has_aggregated() {
    // Rows differ along the FORM of the aggregation sources, and so along how many
    // (source, Region) pairs the aggregator holds: none of them has a status before the
    // aggregation completes, and every one of them has one after.
    let account_id = emulator_server::emulator_account_id();
    let cases: [(&str, Value, usize); 4] = [
        (
            "this account in one Region",
            json!({ "AccountAggregationSources": [account_source(account_id, &["us-east-1"])] }),
            1,
        ),
        (
            "this account in three Regions",
            json!({
                "AccountAggregationSources": [
                    account_source(account_id, &["us-east-1", "eu-west-1", "ap-south-1"]),
                ],
            }),
            3,
        ),
        (
            "this account in every Region",
            json!({ "AccountAggregationSources": [account_source(account_id, &[])] }),
            1,
        ),
        (
            "an organization over every Region",
            json!({
                "OrganizationAggregationSource": { "RoleArn": ROLE_ARN, "AllAwsRegions": true },
            }),
            1,
        ),
    ];

    for (case, sources, pairs) in cases {
        let emulator = Emulator::new();
        let mut request = sources.clone();
        request["ConfigurationAggregatorName"] = json!(AGGREGATOR);
        emulator.ok("PutConfigurationAggregator", &request).await;

        // Straight after the sources are written nothing has moved across them yet.
        let before = emulator
            .ok(
                "DescribeConfigurationAggregatorSourcesStatus",
                &json!({ "ConfigurationAggregatorName": AGGREGATOR }),
            )
            .await;
        assert_eq!(
            before["AggregatedSourceStatusList"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: an aggregator that has not aggregated yet reports no source \
             status, got {before}"
        );

        // Reading the status must not bring the aggregation forward.
        let again = emulator
            .ok(
                "DescribeConfigurationAggregatorSourcesStatus",
                &json!({ "ConfigurationAggregatorName": AGGREGATOR }),
            )
            .await;
        assert_eq!(
            again, before,
            "case {case:?}: reading the statuses must not change what a later read sees, \
             got {again}"
        );

        // Once the clock owes the aggregation, every pair the sources enumerate has a
        // status, and it reports the instant that aggregation completed.
        finish_aggregating(&emulator);
        let after = emulator
            .ok(
                "DescribeConfigurationAggregatorSourcesStatus",
                &json!({ "ConfigurationAggregatorName": AGGREGATOR }),
            )
            .await;
        let page = after["AggregatedSourceStatusList"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a status list, got {after}"))
            .clone();
        assert_eq!(
            page.len(),
            pairs,
            "case {case:?}: expected one status per (source, Region) pair, got {after}"
        );
        for status in &page {
            assert_eq!(
                status["LastUpdateStatus"].as_str(),
                Some("SUCCEEDED"),
                "case {case:?}: an aggregated source reports a completed sync, got {status}"
            );
            assert!(
                status["LastUpdateTime"].as_f64().is_some(),
                "case {case:?}: an aggregated source reports when it was last verified, \
                 got {status}"
            );
        }
    }
}

/// Mint `IDS_PER_MINT` identifiers of one kind through the API, each from the same
/// account, and answer them in the order they were minted.
async fn minted(emulator: &Emulator, kind: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for ordinal in 0..IDS_PER_MINT {
        let id = match kind {
            "ResourceEvaluationId" => {
                let started = emulator
                    .ok(
                        "StartResourceEvaluation",
                        &json!({
                            "EvaluationMode": "PROACTIVE",
                            "ResourceDetails": {
                                "ResourceId": format!("bucket-{ordinal}"),
                                "ResourceType": S3_BUCKET,
                                "ResourceConfiguration": "{}",
                            },
                        }),
                    )
                    .await;
                started["ResourceEvaluationId"].as_str().map(str::to_string)
            }
            "QueryId" => {
                let name = format!("query-{ordinal}");
                emulator
                    .ok(
                        "PutStoredQuery",
                        &json!({
                            "StoredQuery": {
                                "QueryName": name,
                                "Expression": "SELECT resourceId WHERE resourceType = \
                                               'AWS::S3::Bucket'",
                            },
                        }),
                    )
                    .await;
                let stored = emulator
                    .ok("GetStoredQuery", &json!({ "QueryName": name }))
                    .await;
                stored["StoredQuery"]["QueryId"].as_str().map(str::to_string)
            }
            "ConfigRuleId" => {
                let name = format!("rule-{ordinal}");
                emulator.ok("PutConfigRule", &managed_rule(&name)).await;
                let described = emulator
                    .ok("DescribeConfigRules", &json!({ "ConfigRuleNames": [name] }))
                    .await;
                described["ConfigRules"][0]["ConfigRuleId"]
                    .as_str()
                    .map(str::to_string)
            }
            "ConfigurationAggregatorId" => {
                let account_id = emulator_server::emulator_account_id();
                let created = emulator
                    .ok(
                        "PutConfigurationAggregator",
                        &json!({
                            "ConfigurationAggregatorName": format!("aggregator-{ordinal}"),
                            "AccountAggregationSources": [account_source(account_id, &[])],
                        }),
                    )
                    .await;
                // The id Config assigned is the resource segment of the ARN.
                created["ConfigurationAggregator"]["ConfigurationAggregatorArn"]
                    .as_str()
                    .and_then(|arn| arn.rsplit('/').next())
                    .map(str::to_string)
            }
            other => panic!("the suite mints no {other}"),
        };
        ids.push(
            id.unwrap_or_else(|| panic!("case {kind:?}: the service must assign an id, ordinal {ordinal}")),
        );
    }
    ids
}

#[tokio::test]
async fn an_assigned_identifier_is_drawn_rather_than_counted() {
    // Rows differ along WHICH identifier the service assigns, and so along the form it
    // takes: the ids written as an unbroken run of hexadecimal characters and the ids
    // written as a prefix and a run of base-36 characters. Every row asserts the same
    // rule — distinct, inside the published form, and not a sequence a caller could
    // continue.
    struct Case {
        kind: &'static str,
        /// The prefix the published form gives the id, if any.
        prefix: &'static str,
        /// The widths of the hyphen-separated groups the id carries after its prefix:
        /// one group for a flat run of characters, five for a canonical uuid.
        groups: &'static [usize],
        /// Whether those characters are hexadecimal; otherwise they are base-36.
        hexadecimal: bool,
        /// The modelled maximum length of the id.
        max_length: usize,
        /// Whether the account needs a configuration recorder before the id's resource
        /// can be created.
        needs_recorder: bool,
    }

    // The bounds come from this service's model: `ResourceEvaluationId` is 1..128;
    // `StoredQuery$QueryId` is 1..36 with pattern `^\S+$`; `ConfigRule$ConfigRuleId` is
    // 1..64; `ConfigurationAggregator$ConfigurationAggregatorArn`'s resource segment is
    // `config-aggregator-[a-z\d]+`.
    let cases = [
        Case {
            kind: "ResourceEvaluationId",
            prefix: "",
            groups: &UUID_GROUPS,
            hexadecimal: true,
            max_length: 128,
            needs_recorder: false,
        },
        Case {
            kind: "QueryId",
            prefix: "",
            groups: &UUID_GROUPS,
            hexadecimal: true,
            max_length: 36,
            needs_recorder: false,
        },
        Case {
            kind: "ConfigRuleId",
            prefix: "config-rule-",
            groups: &[6],
            hexadecimal: false,
            max_length: 64,
            needs_recorder: true,
        },
        Case {
            kind: "ConfigurationAggregatorId",
            prefix: "config-aggregator-",
            groups: &[8],
            hexadecimal: false,
            max_length: 256,
            needs_recorder: false,
        },
    ];

    for case in cases {
        let kind = case.kind;
        let emulator = Emulator::new();
        if case.needs_recorder {
            emulator
                .ok(
                    "PutConfigurationRecorder",
                    &recorder_request(json!({ "roleARN": ROLE_ARN })),
                )
                .await;
        }
        let ids = minted(&emulator, kind).await;

        for id in &ids {
            assert!(
                id.starts_with(case.prefix) && (1..=case.max_length).contains(&id.chars().count()),
                "case {kind:?}: expected an id of the published form inside 1..{} \
                 characters, got {id:?}",
                case.max_length
            );
            let body = &id[case.prefix.len()..];
            let widths: Vec<usize> = body.split('-').map(|group| group.chars().count()).collect();
            assert_eq!(
                widths.as_slice(),
                case.groups,
                "case {kind:?}: expected the groups {:?} after the prefix, got {id:?}",
                case.groups
            );
            let well_formed = body.chars().all(|character| {
                character == '-'
                    || if case.hexadecimal {
                        character.is_ascii_hexdigit() && !character.is_ascii_uppercase()
                    } else {
                        character.is_ascii_lowercase() || character.is_ascii_digit()
                    }
            });
            assert!(
                well_formed,
                "case {kind:?}: expected the id's characters to be inside its alphabet, \
                 got {id:?}"
            );
        }

        let distinct: BTreeSet<&String> = ids.iter().collect();
        assert_eq!(
            distinct.len(),
            ids.len(),
            "case {kind:?}: every assigned id must be distinct, got {ids:?}"
        );
        // A counter-derived id walks one ordering as it is minted; a drawn one does
        // not, so the ids as minted are not the ids in sorted order.
        let mut sorted: Vec<&String> = ids.iter().collect();
        sorted.sort();
        let as_minted: Vec<&String> = ids.iter().collect();
        assert_ne!(
            sorted, as_minted,
            "case {kind:?}: the ids were handed out in increasing order, so a caller can \
             guess the next one, got {ids:?}"
        );
    }
}

/// A `ClientToken` at the modelled minimum width (`ClientToken` is 64..256 characters,
/// from `smithy_query(query="shape", shape="ClientToken")`), seeded so no two rows
/// share one token.
fn client_token(seed: &str) -> String {
    format!("{seed:0<64}")
}

#[tokio::test]
async fn a_minted_uuid_identifier_is_canonical_and_is_the_text_every_read_echoes() {
    // Rows differ along the shape of the START request — the deployment context, the
    // timeout, the client token and the resource type being evaluated — because the
    // form of the id Config mints is a property of the service, not of the request.
    let evaluations: [(&str, Value); 5] = [
        (
            "a bare resource definition",
            json!({
                "EvaluationMode": "PROACTIVE",
                "ResourceDetails": {
                    "ResourceId": "config-emulator-bucket",
                    "ResourceType": S3_BUCKET,
                    "ResourceConfiguration": "{\"BucketName\":\"config-emulator-bucket\"}",
                },
            }),
        ),
        (
            "an evaluation grouped by a deployment context",
            json!({
                "EvaluationMode": "PROACTIVE",
                "EvaluationContext": { "EvaluationContextIdentifier": "stack/uuid-form/0001" },
                "ResourceDetails": {
                    "ResourceId": "vol-0001",
                    "ResourceType": EC2_VOLUME,
                    "ResourceConfiguration": "{\"Size\":8}",
                },
            }),
        ),
        (
            "an evaluation with an explicit timeout",
            json!({
                "EvaluationMode": "PROACTIVE",
                "EvaluationTimeout": 60,
                "ResourceDetails": {
                    "ResourceId": "config-emulator-table",
                    "ResourceType": DYNAMODB_TABLE,
                    "ResourceConfiguration": "{\"TableName\":\"config-emulator-table\"}",
                },
            }),
        ),
        (
            "an idempotent evaluation carrying a client token",
            json!({
                "EvaluationMode": "PROACTIVE",
                "ClientToken": client_token("uuid-form-token"),
                "ResourceDetails": {
                    "ResourceId": "config-emulator-bucket",
                    "ResourceType": S3_BUCKET,
                    "ResourceConfiguration": "{}",
                },
            }),
        ),
        (
            "an evaluation of a third-party resource type",
            json!({
                "EvaluationMode": "PROACTIVE",
                "ResourceDetails": {
                    "ResourceId": "extension-resource",
                    "ResourceType": "MyOrg::MyService::MyResource",
                    "ResourceConfiguration": "{}",
                },
            }),
        ),
    ];

    for (case, request) in evaluations {
        let emulator = Emulator::new();
        let started = emulator.ok("StartResourceEvaluation", &request).await;
        let id = started["ResourceEvaluationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {started}"))
            .to_string();
        assert_canonical_uuid(case, "ResourceEvaluationId", &id);

        // The evaluation is KEYED by that exact text: the reads that echo the id back
        // report the same characters, hyphens included.
        let summary = emulator
            .ok(
                "GetResourceEvaluationSummary",
                &json!({ "ResourceEvaluationId": id }),
            )
            .await;
        assert_eq!(
            summary["ResourceEvaluationId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the summary to echo the minted id, got {summary}"
        );
        let listed = emulator.ok("ListResourceEvaluations", &json!({})).await;
        let ids: Vec<&str> = listed["ResourceEvaluations"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a listing, got {listed}"))
            .iter()
            .filter_map(|entry| entry["ResourceEvaluationId"].as_str())
            .collect();
        assert_eq!(
            ids,
            vec![id.as_str()],
            "case {case:?}: expected the listing to report the minted id, got {listed}"
        );
    }

    // Rows differ along the stored query's description and the text of its expression,
    // Unicode included, because none of that reaches the id Config assigns.
    let queries: [(&str, &str, Option<&str>, &str); 4] = [
        (
            "a query with no description",
            "uuid-form-plain",
            None,
            "SELECT resourceId WHERE resourceType = 'AWS::S3::Bucket'",
        ),
        (
            "a query with a description",
            "uuid-form-described",
            Some("every bucket this account records"),
            "SELECT resourceId, resourceType WHERE resourceType = 'AWS::S3::Bucket'",
        ),
        (
            "a query whose expression carries non-ASCII text",
            "uuid-form-unicode",
            Some("ressources chiffrées"),
            "SELECT resourceId WHERE resourceType = 'AWS::S3::Bucket' /* chiffré 🚀 */",
        ),
        (
            "a query whose name is spelled with every character its pattern allows",
            "UUID_form-0123456789",
            None,
            "SELECT resourceId, resourceType",
        ),
    ];

    for (case, name, description, expression) in queries {
        let emulator = Emulator::new();
        let mut stored_query = json!({ "QueryName": name, "Expression": expression });
        if let Some(description) = description {
            stored_query["Description"] = json!(description);
        }
        let created = emulator
            .ok("PutStoredQuery", &json!({ "StoredQuery": stored_query }))
            .await;
        let created_arn = created["QueryArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueryArn, got {created}"))
            .to_string();

        let read = emulator
            .ok("GetStoredQuery", &json!({ "QueryName": name }))
            .await;
        let stored = &read["StoredQuery"];
        let id = stored["QueryId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueryId, got {read}"))
            .to_string();
        assert_canonical_uuid(case, "StoredQuery.QueryId", &id);

        // `QueryArn`'s pattern carries the id as its final `query-<id>` segment, so the
        // ARN and the id must agree character for character.
        let arn = stored["QueryArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueryArn, got {read}"));
        assert_eq!(
            arn, created_arn,
            "case {case:?}: expected the read to report the ARN the create answered \
             with, got {arn:?}"
        );
        assert!(
            arn.ends_with(&format!(":stored-query/{name}/query-{id}")),
            "case {case:?}: expected the ARN to carry the id as its query-<id> segment, \
             got {arn:?} beside {id:?}"
        );

        // The listing reports the same identity as the read.
        let listed = emulator.ok("ListStoredQueries", &json!({})).await;
        let metadata: Vec<(&str, &str)> = listed["StoredQueryMetadata"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a listing, got {listed}"))
            .iter()
            .filter_map(|entry| Some((entry["QueryId"].as_str()?, entry["QueryArn"].as_str()?)))
            .collect();
        assert_eq!(
            metadata,
            vec![(id.as_str(), arn)],
            "case {case:?}: expected the listing to report the minted identity, got {listed}"
        );
    }
}

#[tokio::test]
async fn a_proactive_execution_with_no_proactive_rule_reports_the_published_failure_reason() {
    // Rows differ along the resource the execution is handed — its type, its id and the
    // configuration document — because the reason an account with no proactive rule
    // answers with carries no part of the request.
    let cases: [(&str, &str, &str, Value); 4] = [
        (
            "a bucket",
            S3_BUCKET,
            "config-emulator-bucket",
            json!({ "BucketName": "config-emulator-bucket" }),
        ),
        (
            "a table",
            DYNAMODB_TABLE,
            "config-emulator-table",
            json!({ "TableName": "config-emulator-table" }),
        ),
        ("a volume", EC2_VOLUME, "vol-0001", json!({ "Size": 8 })),
        (
            "a resource type of a third-party extension",
            "MyOrg::MyService::MyResource",
            "extension-resource",
            json!({}),
        ),
    ];

    for (case, resource_type, resource_id, configuration) in cases {
        let emulator = Emulator::new();
        let started = emulator
            .ok(
                "StartResourceEvaluation",
                &json!({
                    "EvaluationMode": "PROACTIVE",
                    "ResourceDetails": {
                        "ResourceId": resource_id,
                        "ResourceType": resource_type,
                        "ResourceConfiguration": configuration.to_string(),
                    },
                }),
            )
            .await;
        let id = started["ResourceEvaluationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {started}"))
            .to_string();

        tokio::time::sleep(PAST_THE_EXECUTION_SPAN).await;
        let summary = emulator
            .ok(
                "GetResourceEvaluationSummary",
                &json!({ "ResourceEvaluationId": id }),
            )
            .await;
        assert_eq!(
            summary["EvaluationStatus"]["Status"].as_str(),
            Some("FAILED"),
            "case {case:?}: with no proactive rule nothing ran, got {summary}"
        );
        assert_eq!(
            summary["EvaluationStatus"]["FailureReason"].as_str(),
            Some(NO_PROACTIVE_RULE_REASON),
            "case {case:?}: expected the published failure reason, got {summary}"
        );
    }
}
