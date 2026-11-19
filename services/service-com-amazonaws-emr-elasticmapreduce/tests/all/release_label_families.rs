// SPDX-License-Identifier: CC0-1.0

//! The release-label CATALOG this service publishes, family by family.
//!
//! THE RULE these cases assert: `ListReleaseLabels` publishes EVERY release label Amazon
//! EMR offers — every release line of every family, at the patch level each is published
//! at, including the families whose labels are not of the `emr-<major>.<minor>.<patch>`
//! form (a pre-GA qualifier, a prefix of their own) — newest release first within a
//! family, paged by `MaxResults`/`NextToken`; and the catalog is ONE catalog, so every
//! label it publishes also describes through `DescribeReleaseLabel` and launches through
//! `RunJobFlow`, while a label it does not publish is refused by both.
//!
//! `release_labels.rs` holds the filter and paging contract of the same operation, and
//! `release_label_catalog.rs` what one release reports; the tables below are about WHICH
//! releases the catalog holds, which is what a client reads this operation to learn.
//!
//! Every case drives the real HTTP path through the router's `dispatch` (the shared
//! [`Emr`] harness), so these are the bytes an SDK client sends.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{assert_refused, at, minimal_request, Emr};

/// One release FAMILY of the catalog: the `Prefix` filter that selects it, labels it must
/// publish, and labels of the same shape it must NOT publish.
#[derive(Debug)]
struct FamilyRow {
    /// The `ReleaseLabelFilter$Prefix` that selects the family.
    prefix: &'static str,
    /// Labels the family publishes: the head of the series, its tail, and releases in
    /// between — including the patch revisions and the pre-GA labels, which are the
    /// entries a catalog captured at one point in time loses.
    published: &'static [&'static str],
    /// Labels just outside the published set: a patch Amazon EMR never published on a
    /// line it did publish, a line below the oldest the family publishes, and one above
    /// its head.
    unpublished: &'static [&'static str],
}

/// The families, with the labels each must and must not publish.
///
/// SOURCE: the Amazon EMR Release Guide's release-version table, family by family — the
/// same source `state::RELEASE_LINES`, `state::PRE_RELEASE_LABELS` and
/// `state::SPARK_RELEASE_LINES` record. Rows differ along the family (the four
/// `emr-<major>.<minor>.<patch>` series and the prefixed Spark family), the label form
/// (a plain release, a patch revision, a pre-GA qualifier, a prefixed family), and the
/// position in the family (head, middle, tail) — and name releases no recorded unit of
/// this catalog cites (`emr-4.4.0`, `emr-5.11.4`, `emr-5.2.3`, `emr-7.10.0`) beside the
/// ones it does.
const FAMILY_ROWS: &[FamilyRow] = &[
    FamilyRow {
        prefix: "emr-4",
        published: &["emr-4.9.6", "emr-4.8.5", "emr-4.6.0", "emr-4.4.0", "emr-4.2.0"],
        // The 4.8 and 4.9 lines are published from patch 2 and patch 1 on, and the catalog
        // publishes no 4.x release below 4.2.0 and none above 4.9.6.
        unpublished: &[
            "emr-4.8.0",
            "emr-4.8.1",
            "emr-4.9.0",
            "emr-4.1.0",
            "emr-4.0.0",
            "emr-4.9.7",
        ],
    },
    FamilyRow {
        prefix: "emr-5",
        published: &[
            "emr-5.36.2",
            "emr-5.33.1",
            "emr-5.32.1",
            "emr-5.31.1",
            "emr-5.29.0",
            "emr-5.20.1",
            "emr-5.11.4",
            "emr-5.2.3",
            "emr-5.0.3",
            "emr-5.0.0",
        ],
        // A patch above the newest the line publishes, a line above the newest the series
        // publishes, and the patches of the 5.0 line Amazon EMR does not list.
        unpublished: &["emr-5.37.0", "emr-5.20.2", "emr-5.0.1", "emr-5.0.2"],
    },
    FamilyRow {
        prefix: "emr-6",
        published: &[
            "emr-6.15.0",
            "emr-6.9.1",
            "emr-6.8.1",
            "emr-6.0.0",
            "emr-6.0.0-beta",
            "emr-6.0.0-beta2",
        ],
        unpublished: &["emr-6.16.0", "emr-6.15.1", "emr-6.7.1", "emr-6.0.0-beta3"],
    },
    FamilyRow {
        prefix: "emr-7",
        published: &["emr-7.14.0", "emr-7.10.0", "emr-7.6.0", "emr-7.5.0", "emr-7.0.0"],
        unpublished: &["emr-7.15.0", "emr-7.14.1"],
    },
    FamilyRow {
        // A family whose labels are NOT of the `emr-<major>.<minor>.<patch>` form: it
        // carries a prefix of its own, so a client filtering for it must find it.
        prefix: "emr-spark",
        published: &["emr-spark-8.1.0", "emr-spark-8.0.0"],
        unpublished: &["emr-spark-8.2.0", "emr-spark-8.0.1", "emr-spark-7.0.0"],
    },
];

/// The instance type every launch below names: a family the WHOLE catalog supports, from
/// its oldest release to its newest, so a launch is refused for the release it names and
/// never for its capacity.
const CATALOG_WIDE_INSTANCE_TYPE: &str = "m4.xlarge";

/// The labels one `ListReleaseLabels` call reports, with the token it answered with.
async fn page(emr: &Emr, case: &str, request: Value) -> (Vec<String>, Option<String>) {
    let out = emr.ok("ListReleaseLabels", request).await;
    let labels = at(&out, &["ReleaseLabels"])
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: ListReleaseLabels must answer a ReleaseLabels list, got {out}")
        })
        .iter()
        .map(|label| {
            label
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every reported release label is a string, got {out}")
                })
                .to_string()
        })
        .collect();
    let token = at(&out, &["NextToken"]).as_str().map(str::to_string);
    (labels, token)
}

/// Every label a request reports, following its tokens to the end of the listing, and how
/// many pages it took.
async fn walk(emr: &Emr, case: &str, request: Value) -> (Vec<String>, usize) {
    let mut labels: Vec<String> = Vec::new();
    let mut pages = 0usize;
    let mut token: Option<String> = None;
    loop {
        let mut request = request.clone();
        if let Some(token) = token.as_deref() {
            request["NextToken"] = json!(token);
        }
        let (served, next) = page(emr, case, request).await;
        labels.extend(served);
        pages += 1;
        assert!(
            pages <= 1_000,
            "case {case:?}: the listing must terminate, got {pages} pages"
        );
        match next {
            Some(next) => token = Some(next),
            None => return (labels, pages),
        }
    }
}

/// The `(major, minor, patch)` version of a label of the `emr-<major>.<minor>.<patch>`
/// series, or `None` for a label of another form (a pre-GA qualifier, a family prefix).
fn series_version(label: &str) -> Option<(u32, u32, u32)> {
    let mut parts = label.strip_prefix("emr-")?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// Every label the catalog publishes, in the order the listing reports it.
async fn whole_catalog(emr: &Emr) -> Vec<String> {
    let (labels, _) = walk(emr, "no filters", json!({})).await;
    assert!(
        labels.len() > 10,
        "the Region publishes its release catalog, got {labels:?}"
    );
    labels
}

/// Each family publishes the labels it is published with, at the patch level each is
/// published at, newest release first — and publishes nothing beside them.
#[tokio::test]
async fn every_family_publishes_its_whole_series() {
    let emr = Emr::new();
    let catalog = whole_catalog(&emr).await;

    for row in FAMILY_ROWS {
        let case = format!("Prefix={:?}", row.prefix);
        let (reported, _) = walk(&emr, &case, json!({ "Filters": { "Prefix": row.prefix } })).await;

        for label in row.published {
            assert!(
                reported.contains(&label.to_string()),
                "case {case}: the family publishes {label:?}, got {reported:?}"
            );
        }
        for label in row.unpublished {
            assert!(
                !reported.contains(&label.to_string()),
                "case {case}: {label:?} names no release of the family, got {reported:?}"
            );
        }
        assert!(
            reported.iter().all(|label| label.starts_with(row.prefix)),
            "case {case}: the filter keeps the labels that start with it, got {reported:?}"
        );

        // The family is reported newest release first, and it is the same order the
        // unfiltered listing reports those labels in.
        let versions: Vec<(u32, u32, u32)> =
            reported.iter().filter_map(|label| series_version(label)).collect();
        for pair in versions.windows(2) {
            assert!(
                pair[0] >= pair[1],
                "case {case}: the family is reported newest release first, got {reported:?}"
            );
        }
        let in_catalog_order: Vec<String> = catalog
            .iter()
            .filter(|label| reported.contains(label))
            .cloned()
            .collect();
        assert_eq!(
            reported, in_catalog_order,
            "case {case}: a filtered listing is the catalog with releases dropped, got \
             {reported:?}"
        );
    }
}

/// The catalog is ONE catalog: a label the listing publishes describes and launches, and
/// a label it does not publish is refused by both — whatever form the label carries.
#[tokio::test]
async fn a_published_label_describes_and_launches_and_an_unpublished_one_is_refused() {
    let emr = Emr::new();

    for row in FAMILY_ROWS {
        for label in row.published {
            let described = emr
                .ok("DescribeReleaseLabel", json!({ "ReleaseLabel": label }))
                .await;
            assert_eq!(
                at(&described, &["ReleaseLabel"]).as_str(),
                Some(*label),
                "case {label:?}: the catalog describes the release it publishes, got {described}"
            );
            assert!(
                at(&described, &["Applications"])
                    .as_array()
                    .is_some_and(|applications| !applications.is_empty()),
                "case {label:?}: a published release bundles applications, got {described}"
            );

            let mut request = minimal_request(&format!("catalog-{label}"));
            request["ReleaseLabel"] = json!(label);
            request["Instances"] = json!({
                "InstanceCount": 1,
                "MasterInstanceType": CATALOG_WIDE_INSTANCE_TYPE,
            });
            let (status, payload) = emr.call("RunJobFlow", request).await;
            assert_eq!(
                status, 200,
                "case {label:?}: a published release launches a cluster, got {status} with \
                 {payload}"
            );
            let cluster_id = at(&payload, &["JobFlowId"])
                .as_str()
                .unwrap_or_else(|| panic!("case {label:?}: an accepted launch answers an id"))
                .to_string();
            let cluster = emr.describe_cluster(&cluster_id).await;
            assert_eq!(
                at(&cluster, &["ReleaseLabel"]).as_str(),
                Some(*label),
                "case {label:?}: the cluster runs the release it was launched on, got {cluster}"
            );
        }

        for label in row.unpublished {
            let (status, payload) = emr
                .call("DescribeReleaseLabel", json!({ "ReleaseLabel": label }))
                .await;
            assert_refused(label, status, &payload, "InvalidRequestException");

            let mut request = minimal_request(&format!("unpublished-{label}"));
            request["ReleaseLabel"] = json!(label);
            request["Instances"] = json!({
                "InstanceCount": 1,
                "MasterInstanceType": CATALOG_WIDE_INSTANCE_TYPE,
            });
            let (status, payload) = emr.call("RunJobFlow", request).await;
            assert_refused(label, status, &payload, "ValidationException");
        }
    }
}

/// Every label the listing publishes is a release of the catalog: the listing and
/// `DescribeReleaseLabel` answer from the one table, and no label is reported twice.
#[tokio::test]
async fn every_published_label_is_a_release_of_the_catalog() {
    let emr = Emr::new();
    let catalog = whole_catalog(&emr).await;

    let mut seen: Vec<&String> = catalog.iter().collect();
    seen.sort();
    let before = seen.len();
    seen.dedup();
    assert_eq!(
        seen.len(),
        before,
        "the catalog reports each release once, got {catalog:?}"
    );

    for label in &catalog {
        let described = emr
            .ok("DescribeReleaseLabel", json!({ "ReleaseLabel": label }))
            .await;
        assert_eq!(
            at(&described, &["ReleaseLabel"]).as_str(),
            Some(label.as_str()),
            "case {label:?}: the catalog describes every release it publishes, got {described}"
        );
    }
}

/// A label of a form other than `emr-<major>.<minor>.<patch>` keeps its place in the
/// listing: a pre-GA label follows the release of the version it preceded, oldest
/// qualifier first, and a family carrying a prefix of its own follows the series.
#[tokio::test]
async fn a_label_of_another_form_keeps_its_place_in_the_listing() {
    let emr = Emr::new();
    let catalog = whole_catalog(&emr).await;

    let position = |label: &str| {
        catalog
            .iter()
            .position(|published| published == label)
            .unwrap_or_else(|| panic!("case {label:?}: the catalog publishes it, got {catalog:?}"))
    };

    // The 6.0 line was published as two beta releases before its general-availability
    // release, so both labels follow it, in the order they were published.
    let general_availability = position("emr-6.0.0");
    let beta = position("emr-6.0.0-beta");
    let beta2 = position("emr-6.0.0-beta2");
    assert_eq!(
        (beta, beta2),
        (general_availability + 1, general_availability + 2),
        "case \"emr-6.0.0\": its pre-GA labels follow it in publication order, got \
         {catalog:?}"
    );

    // The family with a prefix of its own is listed after the series, so a label of it
    // never interleaves with the `emr-<major>.<minor>.<patch>` releases.
    let last_of_series = catalog
        .iter()
        .rposition(|label| series_version(label).is_some())
        .expect("the catalog publishes the emr-x.x.x series");
    for label in ["emr-spark-8.1.0", "emr-spark-8.0.0"] {
        assert!(
            position(label) > last_of_series,
            "case {label:?}: a family with its own prefix follows the series, got {catalog:?}"
        );
    }
    assert!(
        position("emr-spark-8.1.0") < position("emr-spark-8.0.0"),
        "case \"emr-spark\": the family is published newest release first, got {catalog:?}"
    );
}

/// The catalog is larger than one page, so the listing hands it over in pages that rejoin
/// into it whatever page size the caller asks for.
#[tokio::test]
async fn the_catalog_is_paged_and_the_pages_rejoin_into_it() {
    let emr = Emr::new();
    let catalog = whole_catalog(&emr).await;

    // "Defines the maximum number of release labels to return in a single response. The
    // default is 100." (`ListReleaseLabelsInput$MaxResults` doc): the catalog holds more
    // releases than that, so the first page is full and a token is due.
    let (first, token) = page(&emr, "no filters", json!({})).await;
    assert_eq!(
        first.len(),
        100,
        "case \"no filters\": the default page is 100 releases of a larger catalog, got {}",
        first.len()
    );
    assert!(
        token.is_some(),
        "case \"no filters\": releases are left to report, so a token is due"
    );

    for size in [1, 7, 25, 100] {
        let case = format!("MaxResults={size}");
        let (paged, pages) = walk(&emr, &case, json!({ "MaxResults": size })).await;
        assert_eq!(
            paged, catalog,
            "case {case}: the pages rejoin into the catalog, got {paged:?}"
        );
        assert_eq!(
            pages,
            catalog.len().div_ceil(size),
            "case {case}: expected {} pages for {} releases, got {pages}",
            catalog.len().div_ceil(size),
            catalog.len()
        );
    }
}

/// The application the Spark family bundles is the one such a release runs, read back
/// through the operation that reports one release's applications.
#[tokio::test]
async fn a_release_of_the_spark_family_bundles_spark() {
    let emr = Emr::new();

    for label in ["emr-spark-8.0.0", "emr-spark-8.1.0"] {
        let described = emr
            .ok("DescribeReleaseLabel", json!({ "ReleaseLabel": label }))
            .await;
        let applications: Vec<String> = at(&described, &["Applications"])
            .as_array()
            .unwrap_or_else(|| panic!("case {label:?}: Applications is a list, got {described}"))
            .iter()
            .map(|application| {
                at(application, &["Name"])
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            applications,
            vec!["Spark".to_string()],
            "case {label:?}: an Amazon EMR Spark release bundles Spark, got {described}"
        );
    }

    // The application filter reaches the family too: filtering for Spark keeps its
    // releases, and filtering for an application it does not bundle drops them.
    let (ships_spark, _) = walk(
        &emr,
        "Application=spark",
        json!({ "Filters": { "Application": "spark" } }),
    )
    .await;
    let (ships_hadoop, _) = walk(
        &emr,
        "Application=hadoop",
        json!({ "Filters": { "Application": "hadoop" } }),
    )
    .await;
    for label in ["emr-spark-8.0.0", "emr-spark-8.1.0"] {
        assert!(
            ships_spark.contains(&label.to_string()),
            "case {label:?}: the Spark filter keeps the Spark family, got {ships_spark:?}"
        );
        assert!(
            !ships_hadoop.contains(&label.to_string()),
            "case {label:?}: a release of the family bundles no Hadoop, got {ships_hadoop:?}"
        );
    }
}
