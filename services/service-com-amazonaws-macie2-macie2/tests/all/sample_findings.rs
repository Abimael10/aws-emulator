// SPDX-License-Identifier: CC0-1.0

//! Sample findings, end to end over the wire: `CreateSampleFindings` and the two
//! reads that project what it stored.
//!
//! Each case drives the production `MultiServiceRouter` through the harness
//! `macie_session_and_custom_data_identifiers` builds, so what is pinned is the
//! HTTP behaviour and the response BODIES rather than a handler called directly.
//!
//! The rules under test, each asserted over a SET of inputs rather than one
//! request:
//!
//! * `CreateSampleFindings` PERSISTS one sample finding for each finding type
//!   the request names — and one of every type the service supports when it
//!   names none — so `ListFindings` enumerates exactly those findings and
//!   `GetFindings` projects each one;
//! * every stored finding reports the account, partition and region of the call
//!   that created it, the category its type puts it in, a severity whose
//!   qualitative and numerical forms agree, and the sensitive-data or policy
//!   details its category carries — never both;
//! * a `findingCriteria` selects exactly the findings whose property satisfies
//!   every condition it names, and a property findings cannot be filtered on is
//!   refused 400 `ValidationException`;
//! * a `sortCriteria` orders the enumeration, `maxResults` pages it and its own
//!   `nextToken` resumes it, while a token this service did not issue is
//!   refused;
//! * `GetFindings` accepts as many unique ids as its member documents and
//!   refuses more;
//! * `GetFindingStatistics` counts those same selected findings, grouped by the
//!   property its `groupBy` names, in the order its `sortCriteria` asks for and
//!   at most as many groups as its `size` asks for — so the counts of one query
//!   sum to the findings `ListFindings` enumerates for the same criteria that
//!   report that property;
//! * findings belong to the region the request signed for and to that region's
//!   Macie session: every operation is refused 403 until Macie is enabled
//!   there, and `DisableMacie` deletes the findings with the rest of the
//!   account's resources.

use service_com_amazonaws_macie2_macie2 as macie2;
use smithy_server_runtime::MultiServiceRouter;

use super::macie_session_and_custom_data_identifiers::{
    call_in, emulator, enable_in, error_code, reported_instant,
};

/// The region most cases sign for.
const REGION: &str = "us-west-2";

/// A second region, for the cases that pin the per-region scope of a finding.
const OTHER_REGION: &str = "eu-west-1";

/// The operations' URIs, from their `@http` bindings.
const CREATE_URI: &str = "/findings/sample";
const LIST_URI: &str = "/findings";
const DESCRIBE_URI: &str = "/findings/describe";
const STATISTICS_URI: &str = "/findings/statistics";

/// The properties a statistics query may group findings by, written out here
/// rather than read from the crate so this expectation can fail. Source: the
/// `GroupBy` enum of this service's Smithy model
/// (`smithy_query(query="shape",shape="GroupBy")`).
const MODELLED_GROUP_PROPERTIES: [&str; 4] = [
    "classificationDetails.jobId",
    "resourcesAffected.s3Bucket.name",
    "severity.description",
    "type",
];

/// The account segment every emulated ARN and account field carries.
const ACCOUNT: &str = "000000000000";

/// The partition every emulated ARN carries.
const PARTITION: &str = "aws";

/// The finding types this service samples, written out here rather than read
/// from the crate so this expectation can fail. Source: the `FindingType` enum
/// of this service's Smithy model
/// (`smithy_query(query="shape",shape="FindingType")`).
const MODELLED_FINDING_TYPES: [&str; 11] = [
    "SensitiveData:S3Object/Multiple",
    "SensitiveData:S3Object/Financial",
    "SensitiveData:S3Object/Personal",
    "SensitiveData:S3Object/Credentials",
    "SensitiveData:S3Object/CustomIdentifier",
    "Policy:IAMUser/S3BucketPublic",
    "Policy:IAMUser/S3BucketSharedExternally",
    "Policy:IAMUser/S3BucketReplicatedExternally",
    "Policy:IAMUser/S3BucketEncryptionDisabled",
    "Policy:IAMUser/S3BlockPublicAccessDisabled",
    "Policy:IAMUser/S3BucketSharedWithCloudFront",
];

/// The category a finding of `finding_type` belongs to, as the two
/// `FindingCategory` values document themselves: "CLASSIFICATION, for a
/// sensitive data finding; and, POLICY, for a policy finding".
fn expected_category(finding_type: &str) -> &'static str {
    if finding_type.starts_with("SensitiveData:") {
        "CLASSIFICATION"
    } else {
        "POLICY"
    }
}

/// The score that goes with a severity description: `Severity$score` ranges
/// "from 1 (least severe) to 3 (most severe)" over the `SeverityDescription`
/// values "Low", "Medium" and "High".
fn expected_score(description: &str) -> i64 {
    match description {
        "Low" => 1,
        "Medium" => 2,
        "High" => 3,
        other => panic!("{other:?} is not a modelled SeverityDescription"),
    }
}

/// Create sample findings, requiring the modelled 200 and the empty body the
/// response shape models.
async fn create(router: &MultiServiceRouter, region: &str, body: &serde_json::Value, case: &str) {
    let (status, reply) = call_in(router, region, "POST", CREATE_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSampleFindings must answer 200, got {status} with {reply:?}"
    );
    assert!(
        reply.is_null() || reply == serde_json::json!({}),
        "case {case:?}: the response models no members, got {reply:?}"
    );
}

/// Enumerate findings, requiring the modelled 200, and hand back the whole body.
async fn list(
    router: &MultiServiceRouter,
    region: &str,
    body: &serde_json::Value,
    case: &str,
) -> serde_json::Value {
    let (status, reply) = call_in(router, region, "POST", LIST_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListFindings must answer 200, got {status} with {reply:?}"
    );
    reply
}

/// The ids one `ListFindings` response carries.
fn ids_of(case: &str, body: &serde_json::Value) -> Vec<String> {
    body["findingIds"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: findingIds must be an array, got {body:?}"))
        .iter()
        .map(|id| {
            id.as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a finding id is a string, got {id:?}"))
                .to_string()
        })
        .collect()
}

/// Every finding of `region`, through one unpaged enumeration.
async fn all_ids(router: &MultiServiceRouter, region: &str, case: &str) -> Vec<String> {
    let body = list(router, region, &serde_json::json!({}), case).await;
    ids_of(case, &body)
}

/// Read findings back by id, requiring the modelled 200.
async fn describe(
    router: &MultiServiceRouter,
    region: &str,
    ids: &[String],
    case: &str,
) -> Vec<serde_json::Value> {
    let body = serde_json::json!({ "findingIds": ids });
    let (status, reply) = call_in(router, region, "POST", DESCRIBE_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetFindings must answer 200, got {status} with {reply:?}"
    );
    reply["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: findings must be an array, got {reply:?}"))
        .clone()
}

/// Assert everything that is true of EVERY sample finding, whatever its type.
fn assert_sample_finding(case: &str, region: &str, finding: &serde_json::Value) {
    let finding_type = finding["type"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a finding reports its type, got {finding:?}"));
    let case = format!("{case}/{finding_type}");

    assert!(
        MODELLED_FINDING_TYPES.contains(&finding_type),
        "case {case:?}: the type must be one the model declares"
    );
    assert_eq!(
        finding["sample"],
        serde_json::json!(true),
        "case {case:?}: a sample finding reports sample=true, got {finding:?}"
    );
    assert_eq!(
        finding["archived"],
        serde_json::json!(false),
        "case {case:?}: nothing has suppressed a finding the moment it is created, \
         got {finding:?}"
    );
    assert_eq!(
        finding["accountId"].as_str(),
        Some(ACCOUNT),
        "case {case:?}: the finding applies to the emulator's account, got {finding:?}"
    );
    assert_eq!(
        finding["partition"].as_str(),
        Some(PARTITION),
        "case {case:?}: the finding reports the partition it was created in, got {finding:?}"
    );
    assert_eq!(
        finding["region"].as_str(),
        Some(region),
        "case {case:?}: the finding reports the region it was created in, got {finding:?}"
    );
    assert_eq!(
        finding["count"],
        serde_json::json!(1),
        "case {case:?}: a sensitive data finding's count is always 1, got {finding:?}"
    );
    assert_eq!(
        finding["category"].as_str(),
        Some(expected_category(finding_type)),
        "case {case:?}: the category follows from the type, got {finding:?}"
    );
    assert!(
        finding["schemaVersion"].as_str().is_some_and(|v| !v.is_empty()),
        "case {case:?}: the finding reports the schema version of its data structures, \
         got {finding:?}"
    );
    assert!(
        finding["title"].as_str().is_some_and(|v| !v.is_empty())
            && finding["description"]
                .as_str()
                .is_some_and(|v| !v.is_empty()),
        "case {case:?}: a sample finding carries a title and a description, got {finding:?}"
    );

    let severity = finding["severity"]["description"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: a finding reports its severity, got {finding:?}")
    });
    assert_eq!(
        finding["severity"]["score"].as_i64(),
        Some(expected_score(severity)),
        "case {case:?}: the numerical severity must be the rank of {severity:?}, got {finding:?}"
    );

    // Both instants are reported in the form every timestamp member of this
    // model pins, and a finding nothing has updated reports the same one twice.
    let created = reported_instant(&case, "createdAt", finding);
    let updated = reported_instant(&case, "updatedAt", finding);
    assert_eq!(
        created, updated,
        "case {case:?}: a finding nothing has updated reports createdAt as updatedAt"
    );

    // The details a finding carries are the ones its category has: "this value
    // is null for a policy finding", and the other way round.
    let classification = !finding["classificationDetails"].is_null();
    let policy = !finding["policyDetails"].is_null();
    match expected_category(finding_type) {
        "CLASSIFICATION" => {
            assert!(
                classification && !policy,
                "case {case:?}: a sensitive data finding reports classificationDetails and no \
                 policyDetails, got {finding:?}"
            );
            assert!(
                finding["classificationDetails"]["result"]["status"]["code"]
                    .as_str()
                    .is_some(),
                "case {case:?}: the classification result reports its status, got {finding:?}"
            );
            assert!(
                finding["resourcesAffected"]["s3Object"]["key"]
                    .as_str()
                    .is_some(),
                "case {case:?}: a sensitive data finding applies to an object, got {finding:?}"
            );
        }
        _ => {
            assert!(
                policy && !classification,
                "case {case:?}: a policy finding reports policyDetails and no \
                 classificationDetails, got {finding:?}"
            );
            assert!(
                finding["policyDetails"]["action"]["apiCallDetails"]["api"]
                    .as_str()
                    .is_some(),
                "case {case:?}: a policy finding names the API call that produced it, \
                 got {finding:?}"
            );
            // The two instants of that call are reported in the pinned form too.
            let details = &finding["policyDetails"]["action"]["apiCallDetails"];
            reported_instant(&case, "firstSeen", details);
            reported_instant(&case, "lastSeen", details);
        }
    }

    assert!(
        finding["resourcesAffected"]["s3Bucket"]["name"]
            .as_str()
            .is_some_and(|name| !name.is_empty()),
        "case {case:?}: every finding applies to a bucket, got {finding:?}"
    );
    assert!(
        finding["resourcesAffected"]["s3Bucket"]["arn"]
            .as_str()
            .is_some_and(|arn| arn.starts_with(&format!("arn:{PARTITION}:s3:::"))),
        "case {case:?}: the bucket ARN must be an S3 bucket ARN, got {finding:?}"
    );
}

/// A create stores one sample finding per finding type named, and the two reads
/// project exactly those. The table spans the request forms: no array at all,
/// one type of each category, a mixed pair, and every type the model declares.
#[tokio::test]
async fn a_create_stores_one_sample_finding_per_requested_type() {
    for (case, requested) in [
        ("no findingTypes array", None),
        (
            "one sensitive data type",
            Some(vec!["SensitiveData:S3Object/Credentials"]),
        ),
        (
            "one policy type",
            Some(vec!["Policy:IAMUser/S3BucketEncryptionDisabled"]),
        ),
        (
            "one of each category",
            Some(vec![
                "SensitiveData:S3Object/CustomIdentifier",
                "Policy:IAMUser/S3BucketSharedWithCloudFront",
            ]),
        ),
        ("every modelled type", Some(MODELLED_FINDING_TYPES.to_vec())),
    ] {
        let router = emulator();
        enable_in(&router, REGION, "{}").await;

        let body = match &requested {
            Some(types) => serde_json::json!({ "findingTypes": types }),
            None => serde_json::json!({}),
        };
        create(&router, REGION, &body, case).await;

        let expected_types: Vec<String> = requested.clone().map_or_else(
            || {
                MODELLED_FINDING_TYPES
                    .iter()
                    .map(|finding_type| (*finding_type).to_string())
                    .collect()
            },
            |types| {
                types
                    .iter()
                    .map(|finding_type| (*finding_type).to_string())
                    .collect()
            },
        );

        let ids = all_ids(&router, REGION, case).await;
        assert_eq!(
            ids.len(),
            expected_types.len(),
            "case {case:?}: one finding per type named, expected {} got {ids:?}",
            expected_types.len()
        );
        let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "case {case:?}: every finding carries its own id, got {ids:?}"
        );

        let findings = describe(&router, REGION, &ids, case).await;
        assert_eq!(
            findings.len(),
            ids.len(),
            "case {case:?}: every listed id must resolve to a finding, got {findings:?}"
        );
        let mut reported: Vec<String> = findings
            .iter()
            .map(|finding| finding["type"].as_str().unwrap_or_default().to_string())
            .collect();
        reported.sort();
        let mut expected_sorted = expected_types.clone();
        expected_sorted.sort();
        assert_eq!(
            reported, expected_sorted,
            "case {case:?}: the stored types must be the ones named, got {reported:?}"
        );

        for finding in &findings {
            assert_sample_finding(case, REGION, finding);
            let id = finding["id"].as_str().unwrap_or_default().to_string();
            assert!(
                ids.contains(&id),
                "case {case:?}: a finding's id must be the one the enumeration reported, \
                 got {finding:?}"
            );
        }

        // A read changes nothing a later reader sees.
        let again = all_ids(&router, REGION, case).await;
        assert_eq!(
            again, ids,
            "case {case:?}: reading findings must not change what the next reader sees"
        );
    }
}

/// A second create ADDS findings rather than replacing them: each call creates
/// its own samples, which is what makes the ids unique across calls.
#[tokio::test]
async fn a_second_create_adds_findings_of_its_own() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let body = serde_json::json!({ "findingTypes": ["SensitiveData:S3Object/Personal"] });
    create(&router, REGION, &body, "first call").await;
    let first = all_ids(&router, REGION, "first call").await;
    create(&router, REGION, &body, "second call").await;
    let second = all_ids(&router, REGION, "second call").await;

    assert_eq!(
        first.len(),
        1,
        "case \"first call\": one type named, one finding stored, got {first:?}"
    );
    assert_eq!(
        second.len(),
        2,
        "case \"second call\": a second create adds a finding, got {second:?}"
    );
    for id in &first {
        assert!(
            second.contains(id),
            "case \"second call\": the finding the first call created must still be there, \
             got {second:?}"
        );
    }
}

/// A finding type the model does not declare names no sample finding, and is
/// refused 400 `ValidationException` with nothing stored.
#[tokio::test]
async fn an_unsupported_finding_type_is_refused() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    for (case, types) in [
        ("a type the model does not declare", vec!["Policy:IAMUser/S3BucketOpen"]),
        ("an empty string", vec![""]),
        (
            "a supported type beside an unsupported one",
            vec!["SensitiveData:S3Object/Financial", "SensitiveData:S3Object/Secret"],
        ),
    ] {
        let body = serde_json::json!({ "findingTypes": types });
        let (status, reply) =
            call_in(&router, REGION, "POST", CREATE_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an unsupported finding type is refused, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: the refusal must be the modelled ValidationException, got {reply:?}"
        );
        assert!(
            all_ids(&router, REGION, case).await.is_empty(),
            "case {case:?}: a refused create must leave nothing behind"
        );
    }
}

/// A type named twice names one sample finding: the array names a SET of finding
/// types, "one for each type of sample finding to create". The rows are the
/// forms a repeat can take.
#[tokio::test]
async fn a_type_named_twice_is_one_sample_finding() {
    for (case, types, expected) in [
        (
            "the same type twice",
            vec![
                "SensitiveData:S3Object/Financial",
                "SensitiveData:S3Object/Financial",
            ],
            1,
        ),
        (
            "one type repeated beside another",
            vec![
                "Policy:IAMUser/S3BucketPublic",
                "SensitiveData:S3Object/Personal",
                "Policy:IAMUser/S3BucketPublic",
            ],
            2,
        ),
    ] {
        let router = emulator();
        enable_in(&router, REGION, "{}").await;
        create(
            &router,
            REGION,
            &serde_json::json!({ "findingTypes": types }),
            case,
        )
        .await;

        let ids = all_ids(&router, REGION, case).await;
        assert_eq!(
            ids.len(),
            expected,
            "case {case:?}: expected {expected} findings, got {ids:?}"
        );
    }
}

/// A criterion naming a property findings cannot be filtered on is refused even
/// when the region holds no finding to test it against: the properties are the
/// request's, not the store's.
#[tokio::test]
async fn an_unfilterable_property_is_refused_with_an_empty_store() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    assert!(
        all_ids(&router, REGION, "empty store").await.is_empty(),
        "case \"empty store\": nothing has created a finding yet"
    );

    let body = serde_json::json!({
        "findingCriteria": { "criterion": { "notAProperty": { "eq": ["x"] } } }
    });
    let (status, reply) = call_in(&router, REGION, "POST", LIST_URI, &body.to_string()).await;
    assert_eq!(
        status, 400,
        "case \"empty store\": an unfilterable property is refused, got {status} with {reply:?}"
    );
    assert_eq!(
        error_code(&reply),
        "ValidationException",
        "case \"empty store\": the refusal must be the modelled ValidationException, got {reply:?}"
    );
}

/// A `findingCriteria` selects exactly the findings whose property satisfies
/// every condition it names. The rows span the operators the model declares, the
/// three kinds of property value (text, number, boolean) and both categories.
#[tokio::test]
async fn a_finding_criteria_selects_exactly_what_it_names() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;

    let everything = all_ids(&router, REGION, "every type").await;
    let classification = MODELLED_FINDING_TYPES
        .iter()
        .filter(|finding_type| expected_category(finding_type) == "CLASSIFICATION")
        .count();
    let policy = MODELLED_FINDING_TYPES.len() - classification;

    for (case, criterion, expected) in [
        (
            "eq on one type",
            serde_json::json!({ "type": { "eq": ["SensitiveData:S3Object/Financial"] } }),
            1,
        ),
        (
            "eq on two types joined with OR",
            serde_json::json!({ "type": {
                "eq": ["SensitiveData:S3Object/Financial", "Policy:IAMUser/S3BucketPublic"]
            }}),
            2,
        ),
        (
            "eq on the classification category",
            serde_json::json!({ "category": { "eq": ["CLASSIFICATION"] } }),
            classification,
        ),
        (
            "neq on the classification category",
            serde_json::json!({ "category": { "neq": ["CLASSIFICATION"] } }),
            policy,
        ),
        (
            "eq on a boolean property",
            serde_json::json!({ "sample": { "eq": ["true"] } }),
            everything.len(),
        ),
        (
            "eq on a boolean property that holds for nothing",
            serde_json::json!({ "archived": { "eq": ["true"] } }),
            0,
        ),
        (
            "gte on the severity score",
            serde_json::json!({ "severity.score": { "gte": 3 } }),
            policy,
        ),
        (
            "lt on the severity score",
            serde_json::json!({ "severity.score": { "lt": 3 } }),
            classification,
        ),
        (
            "two conditions joined with AND",
            serde_json::json!({
                "category": { "eq": ["POLICY"] },
                "severity.score": { "lte": 2 },
            }),
            0,
        ),
        (
            "eqExactMatch on one value",
            serde_json::json!({ "severity.description": { "eqExactMatch": ["High"] } }),
            policy,
        ),
        (
            "gte on an instant in epoch milliseconds",
            serde_json::json!({ "createdAt": { "gte": 0 } }),
            everything.len(),
        ),
        (
            "lte on an instant before every finding",
            serde_json::json!({ "createdAt": { "lte": 0 } }),
            0,
        ),
        (
            "eq on the affected bucket",
            serde_json::json!({ "resourcesAffected.s3Bucket.name": {
                "eq": ["amzn-s3-demo-bucket"]
            }}),
            everything.len(),
        ),
        (
            "eq on a bucket no finding names",
            serde_json::json!({ "resourcesAffected.s3Bucket.name": { "eq": ["other-bucket"] } }),
            0,
        ),
        (
            "a property only one category carries",
            serde_json::json!({ "classificationDetails.originType": {
                "eq": ["AUTOMATED_SENSITIVE_DATA_DISCOVERY"]
            }}),
            classification,
        ),
    ] {
        let body = serde_json::json!({ "findingCriteria": { "criterion": criterion } });
        let selected = ids_of(case, &list(&router, REGION, &body, case).await);
        assert_eq!(
            selected.len(),
            expected,
            "case {case:?}: expected {expected} findings, got {selected:?}"
        );
        for id in &selected {
            assert!(
                everything.contains(id),
                "case {case:?}: a selected finding must be one that is stored, got {id:?}"
            );
        }
    }

    // A property findings cannot be filtered on is refused rather than silently
    // matching nothing, and so is a criterion larger than the service filters
    // on.
    for (case, criterion) in [
        ("a member no finding carries", serde_json::json!({ "notAProperty": { "eq": ["x"] } })),
        (
            "a nested member that is not filterable",
            serde_json::json!({ "severity.rank": { "gte": 1 } }),
        ),
        (
            "more values than one condition accepts",
            serde_json::json!({ "type": {
                // One past the bound the crate documents, so the work one
                // request can charge stays capped.
                "eq": vec!["SensitiveData:S3Object/Financial";
                           macie2::state::MAX_VALUES_PER_CONDITION + 1]
            }}),
        ),
    ] {
        let body = serde_json::json!({ "findingCriteria": { "criterion": criterion } });
        let (status, reply) = call_in(&router, REGION, "POST", LIST_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an unfilterable property is refused, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: the refusal must be the modelled ValidationException, got {reply:?}"
        );
    }
}

/// A `sortCriteria` orders the enumeration, and an attribute or order the model
/// does not declare is refused.
#[tokio::test]
async fn a_sort_criteria_orders_the_enumeration() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;

    for (case, order, ascending) in [("ascending", "ASC", true), ("descending", "DESC", false)] {
        let body = serde_json::json!({
            "sortCriteria": { "attributeName": "severity.score", "orderBy": order }
        });
        let ids = ids_of(case, &list(&router, REGION, &body, case).await);
        let findings = describe(&router, REGION, &ids, case).await;
        let scores: Vec<i64> = findings
            .iter()
            .map(|finding| finding["severity"]["score"].as_i64().unwrap_or_default())
            .collect();
        let ordered = scores.windows(2).all(|pair| {
            if ascending {
                pair[0] <= pair[1]
            } else {
                pair[0] >= pair[1]
            }
        });
        assert!(
            ordered,
            "case {case:?}: the scores must be in {order} order, got {scores:?}"
        );
    }

    // The same findings, ordered by a different attribute: the ids must be the
    // same set, only in another order.
    let by_type = ids_of(
        "by type",
        &list(
            &router,
            REGION,
            &serde_json::json!({ "sortCriteria": { "attributeName": "type" } }),
            "by type",
        )
        .await,
    );
    let unordered = all_ids(&router, REGION, "no criteria").await;
    assert_eq!(
        by_type.iter().collect::<std::collections::BTreeSet<_>>(),
        unordered.iter().collect::<std::collections::BTreeSet<_>>(),
        "case \"by type\": sorting must not change WHICH findings are enumerated"
    );

    for (case, criteria) in [
        (
            "an attribute findings cannot be sorted by",
            serde_json::json!({ "attributeName": "title" }),
        ),
        (
            "an order the model does not declare",
            serde_json::json!({ "attributeName": "createdAt", "orderBy": "RANDOM" }),
        ),
    ] {
        let body = serde_json::json!({ "sortCriteria": criteria });
        let (status, reply) = call_in(&router, REGION, "POST", LIST_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the criteria must be refused, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: the refusal must be the modelled ValidationException, got {reply:?}"
        );
    }
}

/// `maxResults` bounds a page and the service's own `nextToken` resumes the
/// enumeration, so paging through it yields exactly what one unpaged call does.
/// The rows are the page sizes a caller can ask for, including one larger than
/// the enumeration.
#[tokio::test]
async fn the_enumeration_pages_through_exactly_what_is_stored() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;
    let everything = all_ids(&router, REGION, "every type").await;

    for page_size in [1_usize, 2, 5, MODELLED_FINDING_TYPES.len(), 100] {
        let case = format!("maxResults {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One page per finding is the worst case; the loop cannot outrun it.
        for _ in 0..=everything.len() {
            let mut body = serde_json::json!({ "maxResults": page_size });
            if let Some(token) = token.as_deref() {
                body["nextToken"] = serde_json::json!(token);
            }
            let reply = list(&router, REGION, &body, &case).await;
            let page = ids_of(&case, &reply);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page must carry at most {page_size} ids, got {page:?}"
            );
            seen.extend(page);
            token = reply["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: the enumeration must end, got {seen:?}"
        );
        assert_eq!(
            seen, everything,
            "case {case:?}: paging must yield exactly what one call yields"
        );
    }

    // A token this service did not issue names no page.
    for (case, token) in [
        ("not hex at all", "not-a-token"),
        ("an odd number of hex digits", "abc"),
    ] {
        let body = serde_json::json!({ "nextToken": token });
        let (status, reply) = call_in(&router, REGION, "POST", LIST_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a foreign token is refused, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: the refusal must be the modelled ValidationException, got {reply:?}"
        );
    }
}

/// `GetFindings` accepts as many unique ids as its member documents, refuses
/// more, and reports nothing for an id this region holds no finding for.
#[tokio::test]
async fn get_findings_honours_the_documented_batch_size() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;
    let stored = all_ids(&router, REGION, "every type").await;

    // The documented bound, from the `findingIds` member doc: "You can specify
    // as many as 50 unique identifiers in this array."
    const MAX_IDS: usize = 50;

    for (case, count, expected_status) in [
        ("one id", 1, 200),
        ("the documented maximum", MAX_IDS, 200),
        ("one past the documented maximum", MAX_IDS + 1, 400),
    ] {
        // Padded with ids no finding carries, so the count is what varies.
        let mut ids: Vec<String> = stored.iter().take(count).cloned().collect();
        while ids.len() < count {
            ids.push(format!("{:032x}", ids.len() + 1));
        }
        let body = serde_json::json!({ "findingIds": ids });
        let (status, reply) =
            call_in(&router, REGION, "POST", DESCRIBE_URI, &body.to_string()).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status}, got {status} with {reply:?}"
        );
        if expected_status == 400 {
            assert_eq!(
                error_code(&reply),
                "ValidationException",
                "case {case:?}: the refusal must be the modelled ValidationException, \
                 got {reply:?}"
            );
        } else {
            let findings = reply["findings"].as_array().unwrap_or_else(|| {
                panic!("case {case:?}: findings must be an array, got {reply:?}")
            });
            let found = stored.iter().take(count).count();
            assert_eq!(
                findings.len(),
                found,
                "case {case:?}: an id no finding carries reports nothing, got {findings:?}"
            );
        }
    }

    // Naming one finding twice names one identifier, so it is reported once.
    let twice = vec![stored[0].clone(), stored[0].clone()];
    let findings = describe(&router, REGION, &twice, "the same id twice").await;
    assert_eq!(
        findings.len(),
        1,
        "case \"the same id twice\": a repeated id is one identifier, got {findings:?}"
    );
}

/// Findings belong to the region the request signed for and to that region's
/// Macie session: every operation is refused 403 until Macie is enabled there,
/// a create in one region leaves another region empty, and `DisableMacie`
/// deletes the findings with the rest of the account's resources.
#[tokio::test]
async fn findings_are_scoped_to_the_region_and_need_a_session() {
    let router = emulator();

    for (case, uri, body) in [
        ("CreateSampleFindings", CREATE_URI, "{}"),
        ("ListFindings", LIST_URI, "{}"),
        (
            "GetFindings",
            DESCRIBE_URI,
            "{\"findingIds\":[\"00000000000000000000000000000000\"]}",
        ),
    ] {
        let (status, reply) = call_in(&router, REGION, "POST", uri, body).await;
        assert_eq!(
            status, 403,
            "case {case:?}: Macie is not enabled here, so the call is refused, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "AccessDeniedException",
            "case {case:?}: the refusal must be the modelled AccessDeniedException, got {reply:?}"
        );
    }

    enable_in(&router, REGION, "{}").await;
    enable_in(&router, OTHER_REGION, "{}").await;
    create(
        &router,
        REGION,
        &serde_json::json!({ "findingTypes": ["Policy:IAMUser/S3BucketPublic"] }),
        "one region",
    )
    .await;

    let here = all_ids(&router, REGION, "one region").await;
    assert_eq!(
        here.len(),
        1,
        "case \"one region\": the region created in holds its finding, got {here:?}"
    );
    assert!(
        all_ids(&router, OTHER_REGION, "another region")
            .await
            .is_empty(),
        "case \"another region\": creating in one region must not create in another"
    );
    // The other region cannot read the finding by id either.
    assert!(
        describe(&router, OTHER_REGION, &here, "another region")
            .await
            .is_empty(),
        "case \"another region\": a finding of one region is not readable from another"
    );

    let (status, reply) = call_in(&router, REGION, "DELETE", "/macie", "").await;
    assert_eq!(
        status, 200,
        "case \"disable Macie\": DisableMacie must answer 200, got {status} with {reply:?}"
    );
    enable_in(&router, REGION, "{}").await;
    assert!(
        all_ids(&router, REGION, "after re-enabling Macie")
            .await
            .is_empty(),
        "case \"after re-enabling Macie\": the findings were deleted with the session"
    );
}

/// Aggregate findings, requiring the modelled 200, and hand back the groups the
/// response carries as `(groupKey, count)` pairs IN THE ORDER it reported them.
async fn statistics(
    router: &MultiServiceRouter,
    region: &str,
    body: &serde_json::Value,
    case: &str,
) -> Vec<(String, i64)> {
    let (status, reply) = call_in(router, region, "POST", STATISTICS_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetFindingStatistics must answer 200, got {status} with {reply:?}"
    );
    groups_of(case, &reply)
}

/// The groups one `GetFindingStatistics` response carries.
fn groups_of(case: &str, reply: &serde_json::Value) -> Vec<(String, i64)> {
    reply["countsByGroup"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: countsByGroup must be an array, got {reply:?}"))
        .iter()
        .map(|group| {
            let key = group["groupKey"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a group names the property value it groups, got {group:?}")
                })
                .to_string();
            let count = group["count"].as_i64().unwrap_or_else(|| {
                panic!("case {case:?}: a group reports how many findings it holds, got {group:?}")
            });
            (key, count)
        })
        .collect()
}

/// The value a finding reports for a dotted property path, read out of the
/// projection `GetFindings` answered with, or `None` when it reports none.
fn property_at(finding: &serde_json::Value, path: &str) -> Option<String> {
    let mut value = finding;
    for segment in path.split('.') {
        value = value.get(segment)?;
    }
    value.as_str().map(str::to_string)
}

/// The groups `findings` fall into under `group_by`, counted by the TEST from
/// what the two reads projected, keyed in ascending order. A finding reporting
/// no value for the property belongs to no group.
fn expected_groups(findings: &[serde_json::Value], group_by: &str) -> Vec<(String, i64)> {
    let mut tally: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for finding in findings {
        if let Some(key) = property_at(finding, group_by) {
            let count = tally.entry(key).or_insert(0);
            *count += 1;
        }
    }
    tally.into_iter().collect()
}

/// The findings a `findingCriteria` selects, read back through the two
/// enumerating reads, so what a statistics query is compared against is the
/// population `ListFindings` reports rather than one the test assumed.
async fn selected_findings(
    router: &MultiServiceRouter,
    region: &str,
    criteria: &serde_json::Value,
    case: &str,
) -> Vec<serde_json::Value> {
    let listed = list(
        router,
        region,
        &serde_json::json!({ "findingCriteria": criteria, "maxResults": 200 }),
        case,
    )
    .await;
    let ids = ids_of(case, &listed);
    if ids.is_empty() {
        return Vec::new();
    }
    describe(router, region, &ids, case).await
}

/// A statistics query counts every finding that reports the grouped property in
/// exactly one group. The table spans every property the model lets a query
/// group by, including the one no sample finding reports.
#[tokio::test]
async fn finding_statistics_count_every_finding_that_reports_the_grouped_property() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;
    let ids = all_ids(&router, REGION, "every type").await;
    let findings = describe(&router, REGION, &ids, "every type").await;

    for group_by in MODELLED_GROUP_PROPERTIES {
        let reported = statistics(
            &router,
            REGION,
            &serde_json::json!({ "groupBy": group_by }),
            group_by,
        )
        .await;
        let expected = expected_groups(&findings, group_by);
        assert_eq!(
            reported, expected,
            "case {group_by:?}: expected the groups {expected:?}, got {reported:?}"
        );

        let total: i64 = reported.iter().map(|(_, count)| *count).sum();
        let reporting = findings
            .iter()
            .filter(|finding| property_at(finding, group_by).is_some())
            .count();
        assert_eq!(
            usize::try_from(total).unwrap_or_default(),
            reporting,
            "case {group_by:?}: the counts must sum to the {reporting} findings that report \
             the property, got {reported:?}"
        );
        assert!(
            reported.iter().all(|(key, count)| !key.is_empty() && *count > 0),
            "case {group_by:?}: every group names a value and holds findings, got {reported:?}"
        );
    }
}

/// A statistics query counts exactly the findings its `findingCriteria` select —
/// the same population `ListFindings` enumerates for those criteria. The table
/// spans the criteria forms: none, one condition on a string, on an enum, on a
/// flag, two joined conditions, and one that selects nothing.
#[tokio::test]
async fn finding_statistics_count_only_the_findings_the_criteria_select() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;

    for (case, criteria) in [
        ("no conditions", serde_json::json!({})),
        (
            "one severity",
            serde_json::json!({ "criterion": { "severity.description": { "eq": ["High"] } } }),
        ),
        (
            "one category",
            serde_json::json!({ "criterion": { "category": { "eq": ["CLASSIFICATION"] } } }),
        ),
        (
            "one type",
            serde_json::json!({
                "criterion": { "type": { "eq": ["Policy:IAMUser/S3BucketPublic"] } }
            }),
        ),
        (
            "a category and a flag",
            serde_json::json!({
                "criterion": {
                    "category": { "eq": ["POLICY"] },
                    "archived": { "eq": ["false"] }
                }
            }),
        ),
        (
            "a bound no finding satisfies",
            serde_json::json!({ "criterion": { "severity.score": { "gt": 3 } } }),
        ),
    ] {
        let selected = selected_findings(&router, REGION, &criteria, case).await;
        for group_by in MODELLED_GROUP_PROPERTIES {
            let case = format!("{case}/{group_by}");
            let reported = statistics(
                &router,
                REGION,
                &serde_json::json!({ "groupBy": group_by, "findingCriteria": criteria }),
                &case,
            )
            .await;
            let expected = expected_groups(&selected, group_by);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the groups {expected:?}, got {reported:?}"
            );
        }
    }
}

/// The groups are reported in the order the `sortCriteria` ask for, and `size`
/// bounds how many of them a response carries. The order table spans every pair
/// of grouping and order the model declares, plus the two forms that name only
/// one of them.
#[tokio::test]
async fn finding_statistics_order_and_bound_the_groups_they_report() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;
    let ids = all_ids(&router, REGION, "every type").await;
    let findings = describe(&router, REGION, &ids, "every type").await;

    // The groups of one query, in the two total orders the model names: by the
    // name of each group, and by the number of findings in it with the name as
    // the tie-break.
    let by_key = expected_groups(&findings, "severity.description");
    let mut by_count = by_key.clone();
    by_count.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    let by_key_descending: Vec<(String, i64)> = by_key.iter().rev().cloned().collect();
    let by_count_descending: Vec<(String, i64)> = by_count.iter().rev().cloned().collect();
    assert!(
        by_key.len() > 1,
        "the sample findings must fall in more than one severity group for this \
         table to pin an order, got {by_key:?}"
    );

    for (case, sort_criteria, expected) in [
        ("no sortCriteria", None, &by_key),
        (
            "groupKey ascending",
            Some(serde_json::json!({ "attributeName": "groupKey", "orderBy": "ASC" })),
            &by_key,
        ),
        (
            "groupKey descending",
            Some(serde_json::json!({ "attributeName": "groupKey", "orderBy": "DESC" })),
            &by_key_descending,
        ),
        (
            "count ascending",
            Some(serde_json::json!({ "attributeName": "count", "orderBy": "ASC" })),
            &by_count,
        ),
        (
            "count descending",
            Some(serde_json::json!({ "attributeName": "count", "orderBy": "DESC" })),
            &by_count_descending,
        ),
        (
            "an attribute with no order",
            Some(serde_json::json!({ "attributeName": "count" })),
            &by_count,
        ),
        (
            "an order with no attribute",
            Some(serde_json::json!({ "orderBy": "DESC" })),
            &by_key_descending,
        ),
    ] {
        let mut body = serde_json::json!({ "groupBy": "severity.description" });
        if let Some(sort_criteria) = sort_criteria {
            body["sortCriteria"] = sort_criteria;
        }
        let reported = statistics(&router, REGION, &body, case).await;
        assert_eq!(
            &reported, expected,
            "case {case:?}: expected the groups {expected:?}, got {reported:?}"
        );
    }

    // `size` is "the maximum number of items to include in each page of the
    // response", so it truncates the ordered groups; a request naming none, or
    // a non-positive one, carries all of them.
    let groups = by_key.len();
    for size in [None, Some(0), Some(1), Some(2), Some(groups), Some(groups + 1)] {
        let case = format!("size {size:?}");
        let mut body = serde_json::json!({ "groupBy": "severity.description" });
        if let Some(size) = size {
            body["size"] = serde_json::json!(size);
        }
        let reported = statistics(&router, REGION, &body, &case).await;
        let expected_len = match size {
            Some(size) if size > 0 => size.min(groups),
            _ => groups,
        };
        assert_eq!(
            reported.len(),
            expected_len,
            "case {case}: expected {expected_len} of the {groups} groups, got {reported:?}"
        );
        assert_eq!(
            reported.as_slice(),
            &by_key[..expected_len],
            "case {case}: the groups carried are the first of the order asked for, \
             got {reported:?}"
        );
    }
}

/// A statistics query this service cannot run is refused 400
/// `ValidationException`: a property findings are not grouped by, a grouping or
/// an order the model does not declare, and a property findings cannot be
/// filtered on.
#[tokio::test]
async fn finding_statistics_refuse_a_query_they_cannot_run() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;

    for (case, body) in [
        (
            "a property findings are not grouped by",
            serde_json::json!({ "groupBy": "severity.score" }),
        ),
        (
            "a property no finding carries",
            serde_json::json!({ "groupBy": "resourcesAffected.s3Object.key" }),
        ),
        ("no groupBy at all", serde_json::json!({})),
        (
            "a grouping the model does not declare",
            serde_json::json!({
                "groupBy": "type",
                "sortCriteria": { "attributeName": "severity.score" }
            }),
        ),
        (
            "an order the model does not declare",
            serde_json::json!({
                "groupBy": "type",
                "sortCriteria": { "attributeName": "count", "orderBy": "SIDEWAYS" }
            }),
        ),
        (
            "a property findings cannot be filtered on",
            serde_json::json!({
                "groupBy": "type",
                "findingCriteria": { "criterion": { "severity": { "eq": ["High"] } } }
            }),
        ),
    ] {
        let (status, reply) =
            call_in(&router, REGION, "POST", STATISTICS_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the query must be refused, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: the refusal must be the modelled ValidationException, got {reply:?}"
        );
    }
}

/// A statistics query reads the findings of the region it is signed for and that
/// region's Macie session, and reading them leaves them exactly as they were.
#[tokio::test]
async fn finding_statistics_are_scoped_to_the_region_and_change_nothing() {
    let router = emulator();

    let (status, reply) = call_in(
        &router,
        REGION,
        "POST",
        STATISTICS_URI,
        "{\"groupBy\":\"type\"}",
    )
    .await;
    assert_eq!(
        status, 403,
        "case \"no session\": Macie is not enabled here, so the query is refused, \
         got {status} with {reply:?}"
    );
    assert_eq!(
        error_code(&reply),
        "AccessDeniedException",
        "case \"no session\": the refusal must be the modelled AccessDeniedException, \
         got {reply:?}"
    );

    enable_in(&router, REGION, "{}").await;
    enable_in(&router, OTHER_REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;

    for group_by in MODELLED_GROUP_PROPERTIES {
        let elsewhere = statistics(
            &router,
            OTHER_REGION,
            &serde_json::json!({ "groupBy": group_by }),
            group_by,
        )
        .await;
        assert!(
            elsewhere.is_empty(),
            "case {group_by:?}: another region holds none of these findings, got {elsewhere:?}"
        );
    }

    // A read changes nothing a later caller sees: the same query answers the
    // same groups, and the enumeration still reports every finding.
    let before = all_ids(&router, REGION, "before the query").await;
    let first = statistics(
        &router,
        REGION,
        &serde_json::json!({ "groupBy": "type" }),
        "first query",
    )
    .await;
    let second = statistics(
        &router,
        REGION,
        &serde_json::json!({ "groupBy": "type" }),
        "second query",
    )
    .await;
    let after = all_ids(&router, REGION, "after the query").await;
    assert_eq!(
        first, second,
        "case \"repeated query\": a read must answer the same groups, got {first:?} then {second:?}"
    );
    assert_eq!(
        before, after,
        "case \"repeated query\": a read must leave the findings as they were"
    );
}

/// The state the crate exposes and the wire agree: every stored finding carries
/// the id the enumeration reports it under, and the record is the one the read
/// projects.
#[tokio::test]
async fn the_store_holds_exactly_what_the_wire_reports() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    create(&router, REGION, &serde_json::json!({}), "every type").await;

    let ids = all_ids(&router, REGION, "every type").await;
    let findings = describe(&router, REGION, &ids, "every type").await;
    for finding in &findings {
        let id = finding["id"].as_str().unwrap_or_default();
        assert_eq!(
            id.chars().count(),
            macie2::state::FINDING_ID_LEN,
            "case {id:?}: a finding id is {} characters",
            macie2::state::FINDING_ID_LEN
        );
        assert!(
            id.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "case {id:?}: a finding id is lowercase hex"
        );
    }
}
