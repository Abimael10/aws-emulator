// SPDX-License-Identifier: CC0-1.0

//! What a STORED preferred-resource preference does to EVERY reader of an Auto Scaling group
//! analysis.
//!
//! The rule under test: `PreferredResource` is "the preference to control which resource type
//! values are considered when generating rightsizing recommendations", available for "the
//! Amazon EC2 instance and Auto Scaling group resource types", whose exclude list holds "the
//! resource type values to exclude from the recommendation candidates" and whose absent
//! include list "includes all supported resources by default". So the lists stored for a group
//! narrow the candidate configurations EVERY reader of its rightsizing recommendation offers —
//! the recommendation reader and the export of those recommendations alike — and a
//! configuration survives only where every instance type it runs is still a candidate, since
//! one running an excluded type would consider that type. The answers about one group therefore
//! agree: no reader offers a configuration the candidate set the same preference RESOLVES into
//! (`effectiveIncludeList`, "the expanded version of your preferred resource's include list")
//! leaves out, whether that set is read off the recommendation's own
//! `effectiveRecommendationPreferences` or resolved by
//! `GetEffectiveRecommendationPreferences`.
//!
//! The ORACLE of every row is what the reader reports with nothing stored, narrowed by the rule
//! computed from the written lists ([`admits`]), so nothing below restates an instance type, a
//! rank or a figure this analysis happens to hold; the one row that names an exact instance
//! type takes it from that baseline rather than from a literal. The rows span the forms a
//! preferred-resource value takes — an exact type, a family, the catch-all — on both lists, up
//! to a preference that leaves a group no configuration at all.
//!
//! Every call goes through the shared wire harness, i.e. the dispatch a client reaches the
//! emulator through: the Compute Optimizer calls as `POST /` with `X-Amz-Target`, and an
//! exported file is downloaded over Amazon S3's own `GetObject`. The analysis is made due by
//! moving the stored enrollment instant back a span, and an export job is polled to its
//! terminal status, so nothing here depends on how fast the host runs.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    analysed_emulator, cell, create_bucket, effective_for, exported_recommendation_rows,
    group_recommendations, narrowed_group_options, preferred_resource_entry,
    reported_group_options, store_for_each_group, strings,
};

/// The destination bucket this suite exports to. Compute Optimizer does not create it, so each
/// row creates it through Amazon S3 first.
const BUCKET: &str = "asg-preferred-resource-options";

/// The column an exported row names the group in
/// (`ExportableAutoScalingGroupField$AutoScalingGroupArn`).
const ARN_COLUMN: &str = "AutoScalingGroupArn";

/// The columns naming the configuration ONE exported recommendation option would move the group
/// to: a single-instance-type option names one type and a mixed one names the types it mixes
/// (`AutoScalingGroupConfiguration$instanceType` / `$mixedInstanceTypes`), so both appear.
const OPTION_TYPE_COLUMN: &str = "RecommendationOptionsConfigurationInstanceType";
/// See [`OPTION_TYPE_COLUMN`].
const OPTION_MIXED_COLUMN: &str = "RecommendationOptionsConfigurationMixedInstanceTypes";

/// How a cell holding several values separates them, so one exportable field is still one
/// column.
const LIST_SEPARATOR: &str = "|";

/// The configurations the EXPORT of the account's Auto Scaling group recommendations offers each
/// group, keyed by group ARN — the file the shared export-then-download workflow
/// ([`exported_recommendation_rows`]) leaves, read through the columns naming one option's
/// configuration.
async fn exported_group_options(
    router: &MultiServiceRouter,
    case: &str,
) -> BTreeMap<String, Vec<Vec<String>>> {
    let rows = exported_recommendation_rows(
        router,
        case,
        "ExportAutoScalingGroupRecommendations",
        BUCKET,
        &[ARN_COLUMN, OPTION_TYPE_COLUMN, OPTION_MIXED_COLUMN],
    )
    .await;
    let (header, data) = rows
        .split_first()
        .unwrap_or_else(|| panic!("case {case:?}: the export file carries a header row"));
    let mut held: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for row in data {
        let arn = cell(header, row, ARN_COLUMN).to_string();
        let single = cell(header, row, OPTION_TYPE_COLUMN).to_string();
        let mixed = cell(header, row, OPTION_MIXED_COLUMN).to_string();
        // A group the preference leaves no configuration of still has its own row, with the
        // option columns empty.
        let options = held.entry(arn).or_default();
        if !mixed.is_empty() {
            options.push(
                mixed
                    .split(LIST_SEPARATOR)
                    .map(ToString::to_string)
                    .collect(),
            );
        } else if !single.is_empty() {
            options.push(vec![single]);
        }
    }
    held
}

/// The one recommendation a response carries for `arn`.
fn recommendation_for<'a>(case: &str, body: &'a Value, arn: &str) -> &'a Value {
    body["autoScalingGroupRecommendations"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|recommendation| recommendation["autoScalingGroupArn"] == json!(arn))
        .unwrap_or_else(|| panic!("case {case:?}: no recommendation for {arn}, got {body}"))
}

/// The candidate set a reported recommendation states its own effective preferences resolve to.
fn stated_candidates(recommendation: &Value) -> Vec<String> {
    strings(
        &recommendation["effectiveRecommendationPreferences"]["preferredResources"][0]
            ["effectiveIncludeList"],
    )
}

/// Both readers of an Auto Scaling group analysis — the recommendation reader and the export of
/// the same recommendations — offer only the configurations the stored preferred-resource lists
/// leave, and every configuration they offer runs instance types the same preference resolves
/// into candidates.
#[tokio::test]
async fn every_reader_of_a_group_analysis_honours_the_stored_preferred_resources() {
    // The oracle: what the reader reports with nothing stored.
    let baseline_router = analysed_emulator().await;
    let baseline_body = group_recommendations(&baseline_router, "the baseline").await;
    let baseline = reported_group_options("the baseline", &baseline_body);
    let arns: Vec<String> = baseline.keys().cloned().collect();
    assert!(
        baseline
            .values()
            .any(|options| options.iter().any(|types| types.len() > 1)),
        "the analysis must hold a group with a mixed-instance-type candidate, or the mixed case \
         is untested, got {baseline:?}"
    );
    // One exact instance type the analysis actually offers, taken from the baseline rather than
    // written down, so the exact-value row narrows something whatever the analysis holds.
    let offered = baseline
        .values()
        .flatten()
        .flatten()
        .next()
        .cloned()
        .unwrap_or_else(|| panic!("the analysis must offer a configuration, got {baseline:?}"));

    // (case, includeList, excludeList)
    let rows: [(&str, Vec<&str>, Vec<&str>); 7] = [
        ("no preference at all", vec![], vec![]),
        (
            "an exclude list naming one exact candidate type",
            vec![],
            vec![offered.as_str()],
        ),
        ("an exclude list naming a whole family", vec![], vec!["m6g"]),
        (
            "an exclude list naming a family with an explicit wildcard",
            vec![],
            vec!["m6g*"],
        ),
        ("an include list naming one family", vec!["m5"], vec![]),
        ("the catch-all include list", vec!["*"], vec![]),
        (
            "an exclude list naming every resource type",
            vec![],
            vec!["*"],
        ),
    ];

    let mut saw_narrowed = false;
    let mut saw_emptied = false;
    for (case, included, excluded) in rows {
        // A fresh emulator per row: what each row reads is its own write, and one Auto Scaling
        // group export job runs per Region at a time, so each row exports from its own store.
        let router = analysed_emulator().await;
        create_bucket(&router, BUCKET).await;
        if let Some(written) = preferred_resource_entry(&included, &excluded) {
            store_for_each_group(&router, &arns, &written).await;
        }

        let expected = narrowed_group_options(&baseline, &included, &excluded);
        let body = group_recommendations(&router, case).await;
        let reported = reported_group_options(case, &body);
        assert_eq!(
            reported.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "case {case:?}: a preference narrows the OPTIONS, not which groups are analysed, got \
             {body}"
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the reader to offer {expected:?}, got {reported:?}"
        );

        // The export of the same recommendations offers the same configurations: one row per
        // option the preference leaves, under the group's own ARN.
        let exported = exported_group_options(&router, case).await;
        assert_eq!(
            exported, expected,
            "case {case:?}: expected the export to hold {expected:?}, got {exported:?}"
        );

        saw_narrowed |= reported
            .iter()
            .any(|(arn, options)| options.len() < baseline[arn].len());
        saw_emptied |= reported
            .iter()
            .any(|(arn, options)| options.is_empty() && !baseline[arn].is_empty());

        // The answers about one group agree: every instance type an offered configuration runs
        // is a candidate the same preference resolves to, both where the recommendation states
        // the preferences it was generated under and where the effective-preference reader
        // resolves them.
        for (arn, options) in &reported {
            let recommendation = recommendation_for(case, &body, arn);
            let stated = stated_candidates(recommendation);
            let in_force = effective_for(&router, arn).await;
            let resolved = strings(&in_force["preferredResources"][0]["effectiveIncludeList"]);
            assert_eq!(
                stated, resolved,
                "case {case:?}: expected {arn} to state the candidate set its effective \
                 preferences resolve to, got {stated:?} and {resolved:?}"
            );
            for instance_type in options.iter().flatten() {
                assert!(
                    stated.contains(instance_type),
                    "case {case:?}: {arn} offers a configuration running {instance_type:?}, which \
                     its own effective preferences leave out of the candidates: {recommendation}"
                );
            }
        }
    }

    assert!(
        saw_narrowed,
        "the table must hold a row that narrows the baseline configurations, or it proves nothing"
    );
    assert!(
        saw_emptied,
        "the table must hold a row that leaves a group no configuration at all, or the empty case \
         is untested"
    );
}
