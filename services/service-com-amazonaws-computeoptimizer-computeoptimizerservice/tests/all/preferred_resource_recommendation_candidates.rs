// SPDX-License-Identifier: CC0-1.0

//! What a preference does to the recommendation candidates: the preferred-resource lists an
//! account STORES, and the CPU vendor architectures a REQUEST asks for.
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
//! The architecture preference a request carries is the other half of the same admission: one
//! rule decides which candidates an answer reports as options, so every surface of one analysis
//! — the recommendation reader, the projection of an option's metrics, the export file and the
//! Auto Scaling group reader — reports the same options under the same preference, which is
//! what the last test below drives over the model's whole architecture enum.
//!
//! Every call goes through the shared wire harness, i.e. the dispatch a client reaches the
//! emulator through; the analysis is made due by moving the stored enrollment instant back a
//! span, so nothing here depends on how fast the host runs.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    admits, analysed_emulator, call, cell, create_bucket, effective_for,
    exported_recommendation_rows_asking, group_recommendations, narrowed_group_options,
    preferred_resource_entry, put_preference, reported_group_options, store_for_each_group,
    strings, ACCOUNT,
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

/// The account-level scope an Amazon EC2 instance preference is written at: "the value must be
/// a 12-digit Amazon Web Services account ID" (`Scope$value`).
fn account_scope() -> Value {
    json!({"name": "AccountId", "value": ACCOUNT})
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
        if let Some(written) = preferred_resource_entry(included, excluded) {
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
            let stated = strings(
                &recommendation["effectiveRecommendationPreferences"]["preferredResources"][0]
                    ["effectiveIncludeList"],
            );
            let in_force = effective_for(&router, arn).await;
            let resolved = strings(&in_force["preferredResources"][0]["effectiveIncludeList"]);
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
        if let Some(written) = preferred_resource_entry(included, excluded) {
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

/// The option instance types the EXPORT of the account's instance recommendations holds, keyed
/// by instance ARN — the file the shared export-then-download workflow
/// ([`exported_recommendation_rows`]) leaves, read through the column naming one option's
/// instance type.
async fn exported_option_types(
    router: &MultiServiceRouter,
    case: &str,
) -> BTreeMap<String, Vec<String>> {
    exported_option_types_asking(router, case, None).await
}

/// The same export, asked for under the `cpuVendorArchitectures` preference `asked`.
async fn exported_option_types_asking(
    router: &MultiServiceRouter,
    case: &str,
    asked: Option<&Value>,
) -> BTreeMap<String, Vec<String>> {
    let rows = exported_recommendation_rows_asking(
        router,
        case,
        "ExportEC2InstanceRecommendations",
        BUCKET,
        &[ARN_COLUMN, OPTION_COLUMN],
        asked,
    )
    .await;
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
    let arns: Vec<String> = baseline.keys().cloned().collect();
    for (case, included, excluded) in rows {
        let router = analysed_emulator().await;
        if let Some(written) = preferred_resource_entry(included, excluded) {
            store_for_each_group(&router, &arns, &written).await;
        }

        let reported = reported_group_options(
            case,
            &group_recommendations(&router, case).await,
        );
        let expected = narrowed_group_options(&baseline, included, excluded);
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

/// One row of the architecture table: the case, and the `cpuVendorArchitectures` the request
/// asks for — `None` where it states no preference at all.
type ArchitectureRow<'a> = (&'a str, Option<&'a [&'a str]>);

/// Read every instance recommendation under the request preference `asked`, asserting the call
/// was served.
async fn instance_recommendations_asking(
    router: &MultiServiceRouter,
    case: &str,
    asked: Option<&Value>,
) -> Value {
    let mut request = json!({"maxResults": 100});
    if let Some(preference) = asked {
        request["recommendationPreferences"] = preference.clone();
    }
    let (status, body) = call(router, "GetEC2InstanceRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    body
}

/// Read every Auto Scaling group recommendation under the request preference `asked`.
async fn group_recommendations_asking(
    router: &MultiServiceRouter,
    case: &str,
    asked: Option<&Value>,
) -> Value {
    let mut request = json!({"maxResults": 100});
    if let Some(preference) = asked {
        request["recommendationPreferences"] = preference.clone();
    }
    let (status, body) = call(router, "GetAutoScalingGroupRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    body
}

/// The instance types the projection of `instance_arn`'s metrics covers, in the order it reports
/// them, asserting on the way that each option's `rank` is its position among the options
/// reported ("the top recommendation option is ranked as 1", and "the projected metric rank
/// correlates to the recommendation option rank").
async fn projected_option_types(
    router: &MultiServiceRouter,
    case: &str,
    instance_arn: &str,
    asked: Option<&Value>,
) -> Vec<String> {
    let mut request = json!({
        "instanceArn": instance_arn,
        "stat": "Maximum",
        "period": 3_600,
        "startTime": 1_761_000_000_i64,
        "endTime": 1_761_003_600_i64,
    });
    if let Some(preference) = asked {
        request["recommendationPreferences"] = preference.clone();
    }
    let (status, body) = call(
        router,
        "GetEC2RecommendationProjectedMetrics",
        request.clone(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    let options = body["recommendedOptionProjectedMetrics"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for (index, option) in options.iter().enumerate() {
        let position = i64::try_from(index).unwrap_or(i64::MAX) + 1;
        assert_eq!(
            option["rank"].as_i64(),
            Some(position),
            "case {case:?}: expected the option at position {position} of {instance_arn} to be \
             ranked {position}, got {option}"
        );
    }
    options
        .iter()
        .map(|option| {
            option["recommendedInstanceType"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Whether `part` holds the members of `whole` it holds in the order `whole` holds them: an
/// option a narrowing preference leaves keeps the position the analysis ranks it in.
fn is_subsequence<T: PartialEq>(part: &[T], whole: &[T]) -> bool {
    let mut held = whole.iter();
    part.iter()
        .all(|wanted| held.any(|candidate| candidate == wanted))
}

/// The rule one resource's options are judged against, over the rows of the architecture table:
/// a preference naming ONE architecture leaves a subsequence of the options reported under no
/// preference, the two single-architecture selections hold no option in common and together
/// cover that baseline, and naming both leaves it whole whichever order they are named in.
fn assert_architecture_partition<T: PartialEq + ::std::fmt::Debug>(
    case: &str,
    baseline: &[T],
    current: &[T],
    graviton: &[T],
    both: &[T],
    reversed: &[T],
) {
    for (asked, left) in [("CURRENT", current), ("AWS_ARM64", graviton)] {
        assert!(
            is_subsequence(left, baseline),
            "case {case:?}: asking for {asked} leaves options of the baseline {baseline:?} in \
             the order it ranks them, got {left:?}"
        );
    }
    assert!(
        !current.iter().any(|option| graviton.contains(option)),
        "case {case:?}: an option consists of ONE architecture, so the two selections share \
         none, got {current:?} and {graviton:?}"
    );
    assert_eq!(
        current.len() + graviton.len(),
        baseline.len(),
        "case {case:?}: the two architectures together admit every option of the baseline \
         {baseline:?}, got {current:?} and {graviton:?}"
    );
    assert_eq!(
        both, baseline,
        "case {case:?}: naming both architectures restricts nothing, got {both:?}"
    );
    assert_eq!(
        reversed, baseline,
        "case {case:?}: the order the architectures are named in does not reorder the options, \
         got {reversed:?}"
    );
}

/// Every surface of ONE analysis admits the options the request's `cpuVendorArchitectures`
/// preference leaves, and admits the same ones: "when you specify `AWS_ARM64` with a
/// `GetEC2InstanceRecommendations` or `GetAutoScalingGroupRecommendations` request, Compute
/// Optimizer returns recommendations that consist of Graviton instance types only", with a
/// `GetEC2RecommendationProjectedMetrics` request "projected utilization metrics for Graviton
/// instance type recommendations only", and with an `ExportEC2InstanceRecommendations` request
/// it "exports recommendations that consist of Graviton instance types only"
/// (`RecommendationPreferences$cpuVendorArchitectures`). A surface that admitted its own set
/// would let a client see an instance type recommended by one path and absent from the next.
///
/// The ORACLE is the rule, not a list of instance types: each surface is checked against the
/// reader of the same analysis, and the selections are checked against the baseline the same
/// request reports with no preference at all.
#[tokio::test]
async fn every_surface_of_the_analysis_admits_the_same_architecture_options() {
    /// The `CpuVendorArchitecture` the resource runs today, and the Graviton one — the model's
    /// whole value set (`smithy_query query="shape" shape="CpuVendorArchitecture"`).
    const CURRENT: &str = "CURRENT";
    /// See [`CURRENT`].
    const GRAVITON: &str = "AWS_ARM64";

    let rows: [ArchitectureRow; 5] = [
        ("no preference at all", None),
        ("the architecture the resource runs today", Some(&[CURRENT])),
        ("Graviton only", Some(&[GRAVITON])),
        ("both architectures", Some(&[CURRENT, GRAVITON])),
        (
            "both architectures, named the other way round",
            Some(&[GRAVITON, CURRENT]),
        ),
    ];

    // One entry per row, in the order the rows are listed.
    let mut instance_rows: Vec<BTreeMap<String, Vec<String>>> = Vec::new();
    let mut group_rows: Vec<BTreeMap<String, Vec<Vec<String>>>> = Vec::new();
    for (case, asked) in rows {
        // A fresh emulator per row: one instance export job runs per Region at a time, so each
        // row exports from its own store.
        let router = analysed_emulator().await;
        create_bucket(&router, BUCKET).await;
        let preference = asked.map(|values| json!({"cpuVendorArchitectures": values}));

        let read = reported_option_types(
            case,
            &instance_recommendations_asking(&router, case, preference.as_ref()).await,
        );
        // The projection of an option's metrics covers exactly the options the reader reports,
        // in the same order.
        for (arn, types) in &read {
            let projected = projected_option_types(&router, case, arn, preference.as_ref()).await;
            assert_eq!(
                &projected, types,
                "case {case:?}: expected the projection of {arn} to cover the options the \
                 reader reports, {types:?}, got {projected:?}"
            );
        }
        // The export of those recommendations holds one row per option the reader reports.
        let exported = exported_option_types_asking(&router, case, preference.as_ref()).await;
        assert_eq!(
            exported, read,
            "case {case:?}: expected the export to hold the options {read:?}, got {exported:?}"
        );

        let groups = reported_group_options(
            case,
            &group_recommendations_asking(&router, case, preference.as_ref()).await,
        );
        // A preference selects which OPTIONS an answer reports, never which resources are
        // analysed, so every row names the same resources as the baseline row.
        if let Some(baseline) = instance_rows.first() {
            assert_eq!(
                read.keys().collect::<Vec<_>>(),
                baseline.keys().collect::<Vec<_>>(),
                "case {case:?}: expected the instances of the baseline, got {read:?}"
            );
        }
        if let Some(baseline) = group_rows.first() {
            assert_eq!(
                groups.keys().collect::<Vec<_>>(),
                baseline.keys().collect::<Vec<_>>(),
                "case {case:?}: expected the groups of the baseline, got {groups:?}"
            );
        }
        instance_rows.push(read);
        group_rows.push(groups);
    }

    let empty_types: Vec<String> = Vec::new();
    let empty_configurations: Vec<Vec<String>> = Vec::new();
    let mut saw_split_instance = false;
    for (arn, baseline) in &instance_rows[0] {
        let left = |row: &BTreeMap<String, Vec<String>>| {
            row.get(arn).cloned().unwrap_or_else(|| empty_types.clone())
        };
        let current = left(&instance_rows[1]);
        let graviton = left(&instance_rows[2]);
        assert_architecture_partition(
            &format!("the options of {arn}"),
            baseline,
            &current,
            &graviton,
            &left(&instance_rows[3]),
            &left(&instance_rows[4]),
        );
        saw_split_instance |= !current.is_empty() && !graviton.is_empty();
    }
    assert!(
        saw_split_instance,
        "the analysis must hold an instance with a candidate of each architecture, or the table \
         proves nothing"
    );

    let mut saw_split_group = false;
    for (arn, baseline) in &group_rows[0] {
        let left = |row: &BTreeMap<String, Vec<Vec<String>>>| {
            row.get(arn)
                .cloned()
                .unwrap_or_else(|| empty_configurations.clone())
        };
        let current = left(&group_rows[1]);
        let graviton = left(&group_rows[2]);
        assert_architecture_partition(
            &format!("the configurations of {arn}"),
            baseline,
            &current,
            &graviton,
            &left(&group_rows[3]),
            &left(&group_rows[4]),
        );
        saw_split_group |= !current.is_empty() && !graviton.is_empty();
    }
    assert!(
        saw_split_group,
        "the analysis must hold a group with a candidate configuration of each architecture, or \
         the group half of the table proves nothing"
    );
}
