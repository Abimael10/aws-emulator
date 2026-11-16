// SPDX-License-Identifier: CC0-1.0

//! What a STORED preferred-resource preference does to the recommendation candidates.
//!
//! The rule under test: `PreferredResource` is "the preference to control which resource type
//! values are considered when generating rightsizing recommendations", whose exclude list
//! holds "the resource type values to exclude from the recommendation candidates" and whose
//! absent include list "includes all supported resources by default". So the lists an account
//! stores narrow the candidate instance types every reader of a rightsizing recommendation
//! reports as options — the reader of an Amazon EC2 instance recommendation, the export of
//! those recommendations and the projection of their metrics, and the reader of an Auto
//! Scaling group recommendation, "the Amazon EC2 instance and Auto Scaling group resource
//! types" this preference is available for. A candidate type the lists leave out is an option
//! of no answer, and the options an answer reports agree with the candidate set the same
//! preference RESOLVES into (`effectiveIncludeList`), so the service cannot report an instance
//! type as excluded from the candidates and recommend it at the same time.
//!
//! The ORACLE of every row is what the reader reports with nothing stored, narrowed by the
//! rule computed here from the written lists, so nothing below restates an instance type,
//! a rank or a figure the analysis happens to hold. The rows span the forms a preferred
//! resource value takes — an exact instance type, a family, a family with an explicit
//! wildcard, the catch-all — and the combinations of the two lists, up to a preference that
//! leaves no candidate at all.
//!
//! Every call goes through the shared wire harness, i.e. the dispatch a client reaches the
//! emulator through; the analysis is made due by moving the stored enrollment instant back a
//! span, so nothing here depends on how fast the host runs.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::state::{
    EXPORT_JOB_STEP, JOB_STATUS_COMPLETE, JOB_STATUS_FAILED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, cell, create_bucket, csv_rows, effective_for, emulator_with_engine, export_destination,
    get_object, make_analysis_due, opt_in, put_preference, ACCOUNT,
};

/// The destination bucket the export fixture writes to. Compute Optimizer does not create it,
/// so the fixture creates it through Amazon S3 first.
const BUCKET: &str = "preferred-resource-options";

/// The column an exported instance row names the instance in
/// (`ExportableInstanceField$InstanceArn`).
const ARN_COLUMN: &str = "InstanceArn";

/// The column naming the instance type ONE exported recommendation option would move the
/// instance to, i.e. the column that shows which candidates the preference left.
const OPTION_COLUMN: &str = "RecommendationOptionsInstanceType";

/// How many times the export fixture looks at an export job before giving up, one of the
/// job's own lifecycle steps apart: a deadline far past the two steps a job takes, because
/// what the fixture waits for is the job reaching its terminal status rather than a span of
/// the clock.
const JOB_POLLS: usize = 60;

/// The account-level scope an Amazon EC2 instance preference is written at: "the value must be
/// a 12-digit Amazon Web Services account ID" (`Scope$value`).
fn account_scope() -> Value {
    json!({"name": "AccountId", "value": ACCOUNT})
}

/// The prefix a preferred-resource value matches instance types by: "You can specify the exact
/// resource type value, such as m5.large, or use wild card expressions, such as m5"
/// (`PreferredResource$includeList`), so a value names every instance type its text is a
/// prefix of, and a trailing `*` is the same expression written out.
fn prefix_of(value: &str) -> &str {
    value.strip_suffix('*').unwrap_or(value)
}

/// Whether an include list of `included` and an exclude list of `excluded` leave
/// `instance_type` a recommendation candidate: it has to be named by the include list — or by
/// no include list at all, which includes every supported resource — and not by the exclude
/// list.
fn admits(included: &[&str], excluded: &[&str], instance_type: &str) -> bool {
    let names = |values: &[&str]| {
        values
            .iter()
            .any(|value| instance_type.starts_with(prefix_of(value)))
    };
    (included.is_empty() || names(included)) && !names(excluded)
}

/// The `preferredResources` entry an include list of `included` and an exclude list of
/// `excluded` are written as, or `None` where both are empty — which is no preference at all,
/// since "you must specify either an includeList or excludeList".
fn entry(included: &[&str], excluded: &[&str]) -> Option<Value> {
    if included.is_empty() && excluded.is_empty() {
        return None;
    }
    // "Compute Optimizer only supports the customization of Ec2InstanceTypes"
    // (`PreferredResource$name`).
    let mut written = json!({"name": "Ec2InstanceTypes"});
    if !included.is_empty() {
        written["includeList"] = json!(included);
    }
    if !excluded.is_empty() {
        written["excludeList"] = json!(excluded);
    }
    Some(written)
}

/// An emulator whose account is opted in and whose analysis is due, so every recommendation
/// reader reports the analysis this service holds.
async fn analysed_emulator() -> MultiServiceRouter {
    let (router, engine) = emulator_with_engine();
    opt_in(&router).await;
    make_analysis_due(&engine);
    router
}

/// The strings a reported list member holds.
fn reported_list(member: &Value) -> Vec<String> {
    member
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Read every instance recommendation, asserting the call was served.
async fn instance_recommendations(router: &MultiServiceRouter, case: &str) -> Value {
    let request = json!({"maxResults": 100});
    let (status, body) = call(router, "GetEC2InstanceRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    body
}

/// The instance type of every option a read reports, keyed by instance ARN, in the order the
/// options are reported.
fn reported_option_types(case: &str, body: &Value) -> BTreeMap<String, Vec<String>> {
    body["instanceRecommendations"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: instanceRecommendations must be a list, got {body}")
        })
        .iter()
        .map(|recommendation| {
            let arn = recommendation["instanceArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let types = recommendation["recommendationOptions"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            option["instanceType"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string()
                        })
                        .collect()
                })
                .unwrap_or_default();
            (arn, types)
        })
        .collect()
}

/// The one recommendation a response carries for `arn`.
fn recommendation_for<'a>(case: &str, body: &'a Value, arn: &str) -> &'a Value {
    body["instanceRecommendations"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|recommendation| recommendation["instanceArn"] == json!(arn))
        .unwrap_or_else(|| panic!("case {case:?}: no recommendation for {arn}, got {body}"))
}

/// The options a stored preference leaves of `baseline`: every candidate type the lists admit,
/// in the order the analysis ranks them.
fn narrowed(
    baseline: &BTreeMap<String, Vec<String>>,
    included: &[&str],
    excluded: &[&str],
) -> BTreeMap<String, Vec<String>> {
    baseline
        .iter()
        .map(|(arn, types)| {
            let left: Vec<String> = types
                .iter()
                .filter(|instance_type| admits(included, excluded, instance_type))
                .cloned()
                .collect();
            (arn.clone(), left)
        })
        .collect()
}

/// A stored preferred-resource preference narrows the options an instance recommendation
/// consists of to the candidate types its lists leave, and every reported option is one the
/// same preference resolves into a candidate — through the recommendation's own effective
/// preferences and through `GetEffectiveRecommendationPreferences` alike.
#[tokio::test]
async fn an_instance_recommendation_reports_only_the_candidates_the_preference_leaves() {
    // (case, includeList, excludeList)
    let rows: [(&str, &[&str], &[&str]); 9] = [
        ("no preference at all", &[], &[]),
        ("an exclude list naming one exact candidate type", &[], &["m5.xlarge"]),
        ("an exclude list naming a whole family", &[], &["m6g"]),
        (
            "an exclude list naming a family with an explicit wildcard",
            &[],
            &["m6g*"],
        ),
        ("an include list naming one family", &["m5"], &[]),
        (
            "an include list naming the exact candidate types",
            &["m5.xlarge", "m6g.xlarge"],
            &[],
        ),
        ("the catch-all include list", &["*"], &[]),
        ("an include list and an exclude list together", &["m5"], &["m5.xlarge"]),
        (
            "an exclude list naming every family the analysis offers",
            &[],
            &["m5", "m6g"],
        ),
    ];

    // The oracle: what the reader reports with nothing stored.
    let baseline_router = analysed_emulator().await;
    let baseline_body = instance_recommendations(&baseline_router, "the baseline").await;
    let baseline = reported_option_types("the baseline", &baseline_body);
    assert!(
        baseline.values().any(|types| types.len() > 1),
        "the analysis must hold an instance with several candidate types, or the table proves \
         nothing, got {baseline:?}"
    );

    let mut saw_narrowed = false;
    let mut saw_emptied = false;
    for (case, included, excluded) in rows {
        // A fresh emulator per row: what each row reads is its own write, not the last row's.
        let router = analysed_emulator().await;
        if let Some(written) = entry(included, excluded) {
            put_preference(
                &router,
                &json!({
                    "resourceType": "Ec2Instance",
                    "scope": account_scope(),
                    "preferredResources": [written],
                }),
            )
            .await;
        }

        let body = instance_recommendations(&router, case).await;
        let reported = reported_option_types(case, &body);
        let expected = narrowed(&baseline, included, excluded);
        assert_eq!(
            reported.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "case {case:?}: a preference narrows the OPTIONS, not which instances are \
             analysed, got {body}"
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the options {expected:?}, got {reported:?}"
        );
        saw_narrowed |= reported
            .iter()
            .any(|(arn, types)| types.len() < baseline[arn].len());
        saw_emptied |= reported
            .iter()
            .any(|(arn, types)| types.is_empty() && !baseline[arn].is_empty());

        // The answers about one resource agree: every option reported is a candidate the same
        // preference resolves to, both where the recommendation states the preferences it was
        // generated under and where the effective-preference reader resolves them.
        for (arn, types) in &reported {
            let recommendation = recommendation_for(case, &body, arn);
            let stated = reported_list(
                &recommendation["effectiveRecommendationPreferences"]["preferredResources"][0]
                    ["effectiveIncludeList"],
            );
            let in_force = effective_for(&router, arn).await;
            let resolved =
                reported_list(&in_force["preferredResources"][0]["effectiveIncludeList"]);
            assert_eq!(
                stated, resolved,
                "case {case:?}: expected {arn} to state the candidate set its effective \
                 preferences resolve to, got {stated:?} and {resolved:?}"
            );
            for instance_type in types {
                assert!(
                    stated.contains(instance_type),
                    "case {case:?}: {arn} recommends {instance_type:?}, which its own effective \
                     preferences leave out of the candidates: {recommendation}"
                );
            }
        }
    }

    assert!(
        saw_narrowed,
        "the table must hold a row that narrows the baseline options, or it proves nothing"
    );
    assert!(
        saw_emptied,
        "the table must hold a row that leaves an instance no candidate at all, or the empty \
         case is untested"
    );
}

/// The three readers of ONE instance analysis report the same narrowed options: the
/// recommendation reader, the projection of an option's metrics and the export file. A
/// preference that is honoured by one and not another would let a client see an instance type
/// recommended by one path and excluded by the next.
#[tokio::test]
async fn every_reader_of_the_analysis_honours_the_stored_preference() {
    // (case, includeList, excludeList)
    let rows: [(&str, &[&str], &[&str]); 3] = [
        ("no preference at all", &[], &[]),
        ("an exclude list naming a whole family", &[], &["m6g"]),
        ("an include list naming one exact candidate type", &["m5.xlarge"], &[]),
    ];

    let baseline_router = analysed_emulator().await;
    let baseline = reported_option_types(
        "the baseline",
        &instance_recommendations(&baseline_router, "the baseline").await,
    );

    for (case, included, excluded) in rows {
        // A fresh emulator per row: one instance export job runs per Region at a time, so each
        // row exports from its own store.
        let router = analysed_emulator().await;
        create_bucket(&router, BUCKET).await;
        if let Some(written) = entry(included, excluded) {
            put_preference(
                &router,
                &json!({
                    "resourceType": "Ec2Instance",
                    "scope": account_scope(),
                    "preferredResources": [written],
                }),
            )
            .await;
        }

        let expected = narrowed(&baseline, included, excluded);
        let reported = reported_option_types(
            case,
            &instance_recommendations(&router, case).await,
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: the reader reports the options the preference leaves, got \
             {reported:?}"
        );

        // The projection of an option's metrics covers exactly the options the reader reports.
        for (arn, types) in &expected {
            let request = json!({
                "instanceArn": arn,
                "stat": "Maximum",
                "period": 3_600,
                "startTime": 1_761_000_000_i64,
                "endTime": 1_761_003_600_i64,
            });
            let (status, body) =
                call(&router, "GetEC2RecommendationProjectedMetrics", request.clone()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from {request}, got {status}: {body}"
            );
            let projected: Vec<String> = body["recommendedOptionProjectedMetrics"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            option["recommendedInstanceType"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string()
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                &projected, types,
                "case {case:?}: expected the projection of {arn} to cover {types:?}, got {body}"
            );
        }

        // The export file holds one row per option the preference leaves, under the instance's
        // own ARN; an instance it leaves no option of still has a row, with the option column
        // empty.
        let exported = exported_option_types(&router, case).await;
        assert_eq!(
            exported, expected,
            "case {case:?}: expected the export to hold the options {expected:?}, got \
             {exported:?}"
        );
    }
}

/// Export the account's instance recommendations, poll the job to its terminal status — the
/// documented export-then-download workflow — and answer the option instance types the file
/// holds, keyed by instance ARN.
async fn exported_option_types(
    router: &MultiServiceRouter,
    case: &str,
) -> BTreeMap<String, Vec<String>> {
    let request = json!({
        "s3DestinationConfig": {"bucket": BUCKET, "keyPrefix": "options"},
        "fieldsToExport": [ARN_COLUMN, OPTION_COLUMN],
    });
    let (status, body) = call(router, "ExportEC2InstanceRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {request} to be accepted, got {status}: {body}"
    );
    let job_id = body["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an accepted export answers a job id, got {body}"))
        .to_string();
    let (bucket, key, _) = export_destination(case, &body);

    let mut status = String::new();
    for _ in 0..JOB_POLLS {
        let (code, reported) = call(
            router,
            "DescribeRecommendationExportJobs",
            json!({"jobIds": [job_id]}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: reading job {job_id}: got {code}: {reported}"
        );
        status = reported["recommendationExportJobs"][0]["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == JOB_STATUS_COMPLETE || status == JOB_STATUS_FAILED {
            break;
        }
        tokio::time::sleep(EXPORT_JOB_STEP).await;
    }
    assert_eq!(
        status, JOB_STATUS_COMPLETE,
        "case {case:?}: the export job never completed, last status {status:?}"
    );

    let (code, object) = get_object(router, &bucket, &key).await;
    assert_eq!(
        code, 200,
        "case {case:?}: downloading {key}: got {code}: {}",
        String::from_utf8_lossy(&object)
    );
    let document = String::from_utf8(object)
        .unwrap_or_else(|error| panic!("case {case:?}: the export file is text: {error}"));
    let rows = csv_rows(&document);
    let (header, data) = rows
        .split_first()
        .unwrap_or_else(|| panic!("case {case:?}: the export file carries a header row"));
    let mut held: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in data {
        let arn = cell(header, row, ARN_COLUMN).to_string();
        let option = cell(header, row, OPTION_COLUMN).to_string();
        let options = held.entry(arn).or_default();
        if !option.is_empty() {
            options.push(option);
        }
    }
    held
}

/// Read every Auto Scaling group recommendation, asserting the call was served.
async fn group_recommendations(router: &MultiServiceRouter, case: &str) -> Value {
    let request = json!({"maxResults": 100});
    let (status, body) = call(
        router,
        "GetAutoScalingGroupRecommendations",
        request.clone(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    body
}

/// The instance types of every option a group recommendation reports, keyed by group ARN: one
/// list per option, since a configuration runs one instance type or several
/// (`AutoScalingGroupConfiguration$instanceType` / `$mixedInstanceTypes`).
fn reported_group_options(case: &str, body: &Value) -> BTreeMap<String, Vec<Vec<String>>> {
    body["autoScalingGroupRecommendations"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: autoScalingGroupRecommendations must be a list, got {body}")
        })
        .iter()
        .map(|recommendation| {
            let arn = recommendation["autoScalingGroupArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let options = recommendation["recommendationOptions"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            let configuration = &option["configuration"];
                            let mixed = reported_list(&configuration["mixedInstanceTypes"]);
                            if mixed.is_empty() {
                                vec![configuration["instanceType"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string()]
                            } else {
                                mixed
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            (arn, options)
        })
        .collect()
}

/// The same narrowing applies to an Auto Scaling group's candidate configurations, the other
/// resource type the preference is available for: a configuration is a candidate only where
/// every instance type it runs is one, so excluding one type of a mixed configuration takes
/// that whole configuration out of the options.
#[tokio::test]
async fn a_group_recommendation_reports_only_the_configurations_the_preference_leaves() {
    // (case, includeList, excludeList)
    let rows: [(&str, &[&str], &[&str]); 5] = [
        ("no preference at all", &[], &[]),
        ("an exclude list naming one type of a mixed configuration", &[], &["m6gd"]),
        ("an exclude list naming a whole family", &[], &["m6g"]),
        ("an include list naming one family", &["m5"], &[]),
        ("the catch-all include list", &["*"], &[]),
    ];

    // The oracle: what the reader reports with nothing stored, and the group ARNs a preference
    // is written at — "you can create recommendation preferences for Auto Scaling groups only
    // at the resource level by specifying a scope name of ResourceArn and a scope value of the
    // Auto Scaling group Amazon Resource Name" (`PutRecommendationPreferences$scope`).
    let baseline_router = analysed_emulator().await;
    let baseline = reported_group_options(
        "the baseline",
        &group_recommendations(&baseline_router, "the baseline").await,
    );
    assert!(
        baseline
            .values()
            .any(|options| options.iter().any(|types| types.len() > 1)),
        "the analysis must hold a group with a mixed-instance-type candidate, or the mixed case \
         is untested, got {baseline:?}"
    );

    let mut saw_narrowed = false;
    for (case, included, excluded) in rows {
        let router = analysed_emulator().await;
        if let Some(written) = entry(included, excluded) {
            for arn in baseline.keys() {
                put_preference(
                    &router,
                    &json!({
                        "resourceType": "AutoScalingGroup",
                        "scope": {"name": "ResourceArn", "value": arn},
                        "preferredResources": [written.clone()],
                    }),
                )
                .await;
            }
        }

        let reported = reported_group_options(
            case,
            &group_recommendations(&router, case).await,
        );
        let expected: BTreeMap<String, Vec<Vec<String>>> = baseline
            .iter()
            .map(|(arn, options)| {
                let left: Vec<Vec<String>> = options
                    .iter()
                    .filter(|types| {
                        types
                            .iter()
                            .all(|instance_type| admits(included, excluded, instance_type))
                    })
                    .cloned()
                    .collect();
                (arn.clone(), left)
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the configurations {expected:?}, got {reported:?}"
        );
        saw_narrowed |= reported
            .iter()
            .any(|(arn, options)| options.len() < baseline[arn].len());
    }

    assert!(
        saw_narrowed,
        "the table must hold a row that narrows the baseline configurations, or it proves \
         nothing"
    );
}
