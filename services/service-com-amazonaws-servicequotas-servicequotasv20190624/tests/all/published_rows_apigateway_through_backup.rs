// SPDX-License-Identifier: CC0-1.0

//! What a default-quota row publishes under ITS OWN `QuotaCode`, for the five
//! `ServiceCode`s whose catalogs this change rebound.
//!
//! THE RULE: `QuotaCode` is the identity AWS publishes a quota under, so the row served
//! under a code carries the `QuotaName`, `Value`, `Unit`, `Adjustable`, `Period`,
//! `UsageMetric` and `Description` of the quota AWS publishes UNDER THAT CODE, never a
//! neighbouring quota's; and where a service publishes a class of rows, every row of the
//! class carries the class's text and metric worded around ITS OWN plane, so no row of a
//! class borrows another's.
//!
//! The first table pins the rebound rows field for field. The tests after it assert the
//! class rules over the WHOLE catalog of the service that publishes each class, so the
//! rule is asserted past the rows that motivated it:
//!
//! * the ACCOUNT planes of AWS Backup, whose name carries a per-Region-per-account suffix
//!   and whose sentence is the counted phrase of that same plane in the account's voice;
//! * the SCALABLE-TARGET planes of Application Auto Scaling, whose sentence names the one
//!   namespace that row's targets are registered under;
//! * the per-API RATE planes of Amazon Athena, where a replenishment row is metered by
//!   the `CallCount` of its own API and a burst row names its own API in each place of the
//!   multiplier sentence, and the two rows of an API are published as a pair;
//! * the metered RESOURCE planes of AWS AppConfig, where a plane counted for a parent
//!   resource is metered by that plane's own series and an account plane by the bare
//!   resource type.
//!
//! SOURCE for every expectation: AWS's published Service Quotas data for that service
//! code, and the recorded `ListAWSDefaultServiceQuotas` response of that code for which
//! `QuotaCode` publishes which quota and for the text, unit, limit and adjustability of
//! the rows it states. A member no source available here states pins nothing — a sentence
//! or a limit the catalog carries without a source is left unasserted, because a wrong
//! assertion would be worse than no assertion — and the catalog's PLACEHOLDER rows (the
//! rows it serves under a code whose quota no source names) are pinned nowhere, since they
//! are not AWS's behaviour.
//!
//! The harness (`client`, `emulator`, `default_quotas`) is the one `quota_workflow.rs`
//! owns.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::applied_quota_records::metric_resource;
use crate::quota_workflow::{client, default_quotas as defaults, emulator};

/// One published default-quota row, as the rule says it must be served.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    /// The published default, where a source available here states it; `None` asserts
    /// nothing about the member.
    value: Option<f64>,
    unit: &'static str,
    adjustable: bool,
    /// The published sentence, where a source available here states its text; `None`
    /// asserts nothing about the member.
    description: Option<&'static str>,
    /// The `Resource` dimension of the row's `AWS/Usage` metric, for a metered row;
    /// `None` asserts nothing about the metric.
    metric_resource: Option<&'static str>,
}

/// The rows this suite pins, one block per service code.
const ROWS: &[Row] = &[
    // apigateway — the Regional APIs of a Region (published as NOT adjustable), the
    // length of a method ARN (in bytes), and the authorizers of ONE developer portal:
    // three planes this catalog had bound to a neighbouring quota's code.
    Row {
        service_code: "apigateway",
        quota_code: "L-AA0FF27B",
        quota_name: "Regional APIs",
        value: Some(600.0),
        unit: "None",
        adjustable: false,
        description: None,
        metric_resource: None,
    },
    Row {
        service_code: "apigateway",
        quota_code: "L-5244589D",
        quota_name: "Method ARN Length",
        value: Some(1600.0),
        unit: "Bytes",
        adjustable: false,
        description: None,
        metric_resource: None,
    },
    Row {
        service_code: "apigateway",
        quota_code: "L-8F8AAAFE",
        quota_name: "Maximum Authorizers per Portal",
        value: Some(1.0),
        unit: "None",
        adjustable: false,
        description: None,
        metric_resource: None,
    },
    // The three sentences this service publishes for planes it had served with none: a
    // phrase of the quota's own, the counted-plane voice, and a sentence that names the
    // API types the limit may be raised for.
    Row {
        service_code: "apigateway",
        quota_code: "L-46624B39",
        quota_name: "API Payload Size",
        value: Some(10.0),
        unit: "Megabytes",
        adjustable: false,
        description: Some("Maximum payload size for non WebSocket API."),
        metric_resource: None,
    },
    Row {
        service_code: "apigateway",
        quota_code: "L-824C9E42",
        quota_name: "Client certificates",
        value: Some(60.0),
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of certificates that you can associate with this account \
             in the current region",
        ),
        metric_resource: None,
    },
    Row {
        service_code: "apigateway",
        quota_code: "L-E5AE38E3",
        quota_name: "Maximum integration timeout in milliseconds",
        value: Some(29_000.0),
        unit: "Milliseconds",
        adjustable: true,
        description: Some(
            "The maximum integration timeout (in milliseconds) allowed for your account \
             in the current region. This limit can only be increased for Regional and \
             private APIs.",
        ),
        metric_resource: None,
    },
    // appconfig — the two planes counted for a PARENT resource, each metered by that
    // plane's own series, and the size limit of one deployment.
    Row {
        service_code: "appconfig",
        quota_code: "L-FA210A1F",
        quota_name: "Maximum number of configuration profiles per application",
        value: Some(100.0),
        unit: "None",
        adjustable: true,
        description: Some(
            "A configuration profile contains metadata about a particular set of \
             configuration data used by your application. To reduce the number of \
             configuration profiles required per application, split configuration \
             profiles across multiple applications. Limit increase requests will be \
             automatically approved for values less than or equal to 500.",
        ),
        metric_resource: Some("ConfigurationProfilesPerApplication"),
    },
    Row {
        service_code: "appconfig",
        quota_code: "L-A52E46BE",
        quota_name: "Maximum number of environments per application",
        value: Some(20.0),
        unit: "None",
        adjustable: true,
        description: None,
        metric_resource: Some("EnvironmentsPerApplication"),
    },
    // The deployment size limit, whose NAME the recording states for this code and whose
    // published limit no source available here states — so only the members a source
    // states are pinned.
    Row {
        service_code: "appconfig",
        quota_code: "L-A5FC0339",
        quota_name: "Deployment size limit",
        value: None,
        unit: "Kilobytes",
        adjustable: true,
        description: None,
        metric_resource: None,
    },
    // application-autoscaling — the scalable-target planes this catalog had served one
    // namespace out of step, including the custom-resource plane AWS names in the plural.
    Row {
        service_code: "application-autoscaling",
        quota_code: "L-A2AD6458",
        quota_name: "Scalable targets for Lambda",
        value: Some(500.0),
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of scalable targets that you can register for the Lambda \
             namespace in this account in the current Region. A scalable target \
             identifies the resource that Application Auto Scaling can scale.",
        ),
        metric_resource: None,
    },
    Row {
        service_code: "application-autoscaling",
        quota_code: "L-1AAF0700",
        quota_name: "Scalable targets for SageMaker",
        value: Some(500.0),
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of scalable targets that you can register for the \
             SageMaker namespace in this account in the current Region. A scalable target \
             identifies the resource that Application Auto Scaling can scale.",
        ),
        metric_resource: None,
    },
    Row {
        service_code: "application-autoscaling",
        quota_code: "L-4A11ECEB",
        quota_name: "Scalable targets for custom resources",
        value: Some(500.0),
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of scalable targets that you can register for the custom \
             resource namespace in this account in the current Region. A scalable target \
             identifies the resource that Application Auto Scaling can scale.",
        ),
        metric_resource: None,
    },
    // athena — the two burst multipliers the recording publishes at FOUR rather than at
    // the class's two, under the codes it states for them.
    Row {
        service_code: "athena",
        quota_code: "L-136E993A",
        quota_name: "Burst multiplier quota for DeleteNamedQuery API",
        value: Some(4.0),
        unit: "None",
        adjustable: false,
        description: Some(
            "Multiplier factor to scale burst capacity for DeleteNamedQuery API based on \
             the rate quota (Burst = Rate * BurstMultiplierQuota). DeleteNamedQuery Burst \
             is the maximum number of DeleteNamedQuery API tokens you can accumulate in \
             the account in the region",
        ),
        metric_resource: None,
    },
    Row {
        service_code: "athena",
        quota_code: "L-CC64F7CE",
        quota_name: "Burst multiplier quota for GetNamedQuery API",
        value: Some(4.0),
        unit: "None",
        adjustable: false,
        description: Some(
            "Multiplier factor to scale burst capacity for GetNamedQuery API based on the \
             rate quota (Burst = Rate * BurstMultiplierQuota). GetNamedQuery Burst is the \
             maximum number of GetNamedQuery API tokens you can accumulate in the account \
             in the region",
        ),
        metric_resource: None,
    },
    // backup — the account planes, whose sentence this catalog had worded in the name's
    // per-Region-per-account voice instead of the account's, and the two planes it had
    // bound to another code's quota.
    Row {
        service_code: "backup",
        quota_code: "L-7705D2CB",
        quota_name: "Backup vaults per Region per account",
        value: Some(300.0),
        unit: "None",
        adjustable: true,
        description: Some("Number of backup vaults in this account in the current Region"),
        metric_resource: None,
    },
    Row {
        service_code: "backup",
        quota_code: "L-FFD6444F",
        quota_name: "Concurrent backup copies per supported service per account",
        value: Some(5.0),
        unit: "None",
        adjustable: false,
        description: Some("Number of concurrent backup copies per supported service per account"),
        metric_resource: None,
    },
    Row {
        service_code: "backup",
        quota_code: "L-BD69F607",
        quota_name: "Backup plans per Region per account",
        value: Some(300.0),
        unit: "None",
        adjustable: true,
        description: Some("Number of backup plans in this account in the current Region"),
        metric_resource: None,
    },
    Row {
        service_code: "backup",
        quota_code: "L-C296F1F5",
        quota_name: "Report plans per Region per account",
        value: Some(20.0),
        unit: "None",
        adjustable: true,
        description: Some("Number of report plans in this account in the current Region"),
        metric_resource: None,
    },
];

/// The counted phrase of a plane named `<Plane> per Region per account`: the plane's own
/// words, in the lower case AWS publishes them in mid-sentence.
fn counted_phrase(plane: &str) -> String {
    let mut characters = plane.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// A resource type as a metric dimension names it: the word with its first letter in the
/// upper case the `AWS/Usage` series are published in.
fn dimension_word(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

// The `Resource` dimension of a row's `AWS/Usage` metric is read through the one helper
// `applied_quota_records.rs` owns, imported above.

/// Every pinned code reports the quota published under it — name, limit, unit,
/// adjustability, sentence and usage series — through the listing and through the single
/// default read alike.
#[tokio::test]
async fn a_rebound_row_carries_the_facts_of_its_own_quota() {
    let engine = emulator();
    let client = client(&engine, "us-east-1");

    let mut listings: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for row in ROWS {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let pair = json!({"ServiceCode": row.service_code, "QuotaCode": row.quota_code});
        let quota = client.ok("GetAWSDefaultServiceQuota", pair.clone()).await["Quota"].clone();

        assert_eq!(
            (
                quota["QuotaName"].clone(),
                quota["Unit"].clone(),
                quota["Adjustable"].clone(),
            ),
            (
                json!(row.quota_name),
                json!(row.unit),
                json!(row.adjustable),
            ),
            "case {case}: expected the quota AWS publishes under this code, got {quota}"
        );
        if let Some(value) = row.value {
            assert_eq!(
                quota["Value"],
                json!(value),
                "case {case}: expected the default this quota is published at, got {quota}"
            );
        }
        if let Some(description) = row.description {
            assert_eq!(
                quota["Description"],
                json!(description),
                "case {case}: expected the published sentence, got {quota}"
            );
        }
        if let Some(resource) = row.metric_resource {
            assert_eq!(
                metric_resource(&quota),
                Some(resource),
                "case {case}: expected the series this plane is metered by, got {quota}"
            );
        }

        // One walk of each service's catalog is shared by every row of that service.
        if !listings.contains_key(row.service_code) {
            let walked = defaults(&client, row.service_code).await;
            listings.insert(row.service_code, walked);
        }
        let listed = &listings[row.service_code];
        assert!(
            listed.contains(&quota),
            "case {case}: the listing must carry the record the single read returned, got \
             {listed:?}"
        );

        // The applied plane is keyed on the same code, so a client that read the code out
        // of the default catalog resolves the same quota's record in force.
        let applied = client.ok("GetServiceQuota", pair).await["Quota"].clone();
        assert_eq!(
            (applied["QuotaName"].clone(), applied["Value"].clone()),
            (json!(row.quota_name), quota["Value"].clone()),
            "case {case}: the applied read must resolve the same quota, got {applied}"
        );
    }
}

/// Over the WHOLE AWS Backup catalog: an ACCOUNT plane's sentence counts that plane's own
/// phrase in the account's voice, every other row counts its own phrase in the plain
/// voice, and no two rows share a sentence.
#[tokio::test]
async fn a_backup_account_plane_counts_its_own_phrase_in_the_account_voice() {
    let engine = emulator();
    let client = client(&engine, "eu-west-1");

    let listed = defaults(&client, "backup").await;
    assert!(
        !listed.is_empty(),
        "case backup: the service must publish its catalog, got {listed:?}"
    );

    let mut sentences: BTreeSet<String> = BTreeSet::new();
    let mut account_planes = 0_usize;
    for quota in &listed {
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        let case = format!("backup/{}", quota["QuotaCode"].as_str().unwrap_or_default());
        let description = quota["Description"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: this service describes every row: {quota}"));

        if let Some(plane) = name.strip_suffix(" per Region per account") {
            account_planes += 1;
            assert_eq!(
                description,
                format!(
                    "Number of {} in this account in the current Region",
                    counted_phrase(plane)
                ),
                "case {case}: an account plane counts its own phrase in the account's \
                 voice, got {description:?}"
            );
        } else {
            assert!(
                description.starts_with("Number of ") || description.starts_with("Maximum number "),
                "case {case}: a counted plane states its count, got {description:?}"
            );
            assert!(
                !description.ends_with(" in this account in the current Region"),
                "case {case}: the account voice belongs to the per-Region-per-account \
                 planes alone, got {description:?}"
            );
        }
        assert!(
            sentences.insert(description.to_string()),
            "case {case}: a sentence belongs to one plane, got {description:?} twice in \
             {listed:?}"
        );
    }
    assert!(
        account_planes >= 5,
        "case backup: the account planes are a class of rows, got {account_planes}"
    );
}

/// Over the WHOLE Application Auto Scaling catalog: every scalable-target row's sentence
/// is the class form around the ONE namespace that row's targets are registered under, and
/// two rows never name one namespace.
#[tokio::test]
async fn a_scalable_target_row_names_its_own_namespace() {
    let engine = emulator();
    let client = client(&engine, "ap-south-1");

    let listed = defaults(&client, "application-autoscaling").await;
    let mut namespaces: BTreeSet<String> = BTreeSet::new();
    let mut targets = 0_usize;
    for quota in &listed {
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        let case = format!(
            "application-autoscaling/{}",
            quota["QuotaCode"].as_str().unwrap_or_default()
        );
        let Some(scaled) = name.strip_prefix("Scalable targets for ") else {
            continue;
        };
        targets += 1;
        let description = quota["Description"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: this class carries a sentence: {quota}"));

        let namespace = description
            .strip_prefix("The maximum number of scalable targets that you can register for the ")
            .and_then(|rest| rest.split_once(" namespace in this account in the current Region."))
            .map(|(namespace, _)| namespace.to_string())
            .unwrap_or_else(|| {
                panic!("case {case}: expected the class sentence, got {description:?}")
            });
        assert_eq!(
            description,
            format!(
                "The maximum number of scalable targets that you can register for the \
                 {namespace} namespace in this account in the current Region. A scalable \
                 target identifies the resource that Application Auto Scaling can scale."
            ),
            "case {case}: the sentence is the class form around this row's own namespace"
        );
        assert!(
            namespaces.insert(namespace.clone()),
            "case {case}: a namespace belongs to one row, got {namespace:?} twice for \
             {scaled:?} in {listed:?}"
        );
    }
    assert!(
        targets >= 13,
        "case application-autoscaling: the scalable-target planes are a class of rows, got \
         {targets}"
    );
}

/// Over the WHOLE Amazon Athena catalog: a replenishment row is metered by the `CallCount`
/// of its OWN API over a one-second window, a burst row names its own API in each place of
/// the multiplier sentence, and the two rows of an API are published as a pair.
#[tokio::test]
async fn an_athena_rate_row_meters_its_own_api() {
    let engine = emulator();
    let client = client(&engine, "us-west-2");

    let listed = defaults(&client, "athena").await;
    let mut replenished: BTreeSet<String> = BTreeSet::new();
    let mut burst: BTreeSet<String> = BTreeSet::new();
    for quota in &listed {
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        let case = format!("athena/{}", quota["QuotaCode"].as_str().unwrap_or_default());

        if let Some(api) = name
            .strip_prefix("Replenishment rate of ")
            .and_then(|rest| rest.strip_suffix(" API calls"))
        {
            assert!(
                replenished.insert(api.to_string()),
                "case {case}: one row replenishes one API, got {api:?} twice in {listed:?}"
            );
            assert_eq!(
                (
                    quota["UsageMetric"]["MetricName"].as_str(),
                    metric_resource(quota),
                    quota["Period"].clone(),
                ),
                (
                    Some("CallCount"),
                    Some(api),
                    json!({"PeriodValue": 1, "PeriodUnit": "SECOND"}),
                ),
                "case {case}: a rate is metered by its own API's calls over its own \
                 window, got {quota}"
            );
        } else if let Some(api) = name
            .strip_prefix("Burst multiplier quota for ")
            .and_then(|rest| rest.strip_suffix(" API"))
        {
            assert!(
                burst.insert(api.to_string()),
                "case {case}: one row multiplies one API's rate, got {api:?} twice in \
                 {listed:?}"
            );
            let description = quota["Description"].as_str().unwrap_or_else(|| {
                panic!("case {case}: this class carries the multiplier sentence: {quota}")
            });
            assert_eq!(
                description,
                format!(
                    "Multiplier factor to scale burst capacity for {api} API based on \
                     the rate quota (Burst = Rate * BurstMultiplierQuota). {api} Burst \
                     is the maximum number of {api} API tokens you can accumulate in \
                     the account in the region"
                ),
                "case {case}: the burst sentence names this row's own API in each of its \
                 places"
            );
            assert!(
                quota["UsageMetric"].is_null(),
                "case {case}: a multiplier is metered by no series of its own, got {quota}"
            );
        }
    }

    assert_eq!(
        replenished, burst,
        "case athena: a throttled API publishes a rate and the multiplier on it, got \
         {replenished:?} replenished and {burst:?} multiplied"
    );
    assert!(
        replenished.len() >= 11,
        "case athena: the per-API rates are a class of rows, got {}",
        replenished.len()
    );
}

/// Over the WHOLE AWS AppConfig catalog: a plane counted for a PARENT resource is metered
/// by that plane's own series, and an account plane by the bare resource type.
#[tokio::test]
async fn an_appconfig_plane_is_metered_by_its_own_series() {
    let engine = emulator();
    let client = client(&engine, "eu-central-1");

    let listed = defaults(&client, "appconfig").await;
    let mut metered = 0_usize;
    for quota in &listed {
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        let case = format!("appconfig/{}", quota["QuotaCode"].as_str().unwrap_or_default());
        let Some(resource) = metric_resource(quota) else {
            continue;
        };
        metered += 1;

        if let Some((_, parent)) = name.split_once(" per ") {
            let suffix = format!("Per{}", dimension_word(parent));
            assert!(
                resource.ends_with(&suffix),
                "case {case}: a plane counted per {parent} is metered by its own series, \
                 which ends {suffix:?}, got {resource:?}"
            );
        } else {
            assert!(
                !resource.contains("Per"),
                "case {case}: an account plane is metered by the bare resource type, got \
                 {resource:?}"
            );
        }
        assert_eq!(
            quota["UsageMetric"]["MetricDimensions"]["Service"].as_str(),
            Some("AWS AppConfig"),
            "case {case}: the series is published under the service's display name, got \
             {quota}"
        );
    }
    assert!(
        metered >= 4,
        "case appconfig: the metered resource planes are a class of rows, got {metered}"
    );
}
