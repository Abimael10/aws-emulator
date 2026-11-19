// SPDX-License-Identifier: CC0-1.0

//! Launching a cluster resolves its `ReleaseLabel` against the release catalog.
//!
//! THE RULE: a cluster launch NAMES the Amazon EMR release the cluster is to run — "release
//! labels are in the form `emr-x.x.x`, where x.x.x is an Amazon EMR release version such as
//! `emr-5.14.0` … the release label applies only to Amazon EMR releases version 4.0 and later.
//! Earlier versions use `AmiVersion`" (`RunJobFlow$ReleaseLabel` doc) — so `RunJobFlow` serves
//! EVERY label the release catalog publishes, which is the same set `ListReleaseLabels`
//! enumerates for a caller to choose from and `DescribeReleaseLabel` describes one entry of,
//! whatever family of it the label belongs to; and it refuses with HTTP 400
//! `ValidationException` carrying `The supplied release label is invalid: <label>.`, writing no
//! cluster, only a label that names none of those releases: a release the catalog does not
//! publish — a line it does not list, or a patch release of a line it lists that the line never
//! published — or a value not spelled as one of its labels at all. The member is optional ("for
//! Amazon EMR releases 4.0 and later … use `AmiVersion` for releases 3.x and 2.x"), so a launch
//! naming no release label names no release to resolve. The listing and the launch are two
//! readings of the one catalog and cannot disagree: a label a caller discovers is a label it can
//! launch on.
//!
//! The rows span what the rule generalises over: WHICH family and series a published label
//! belongs to, and HOW an unusable label names no release. The served rows read out of the
//! catalog itself are read through `ListReleaseLabels` rather than written here, so they follow
//! the catalog if it grows; the hand-written rows beside them come from the Amazon EMR Release
//! Guide's release-version tables, the source `state::RELEASE_LINES` records — among them
//! `emr-5.14.0`, the `ReleaseLabel` doc's own example, and the 4.0/4.1/4.7 lines Amazon EMR no
//! longer lists, which no part of this issue cited.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{at, Emr, INSTANCE_PROFILE, SERVICE_ROLE};

/// A `RunJobFlow` request naming `release_label` when there is one, under the cluster name
/// `name`.
fn launch(name: &str, release_label: Option<&str>) -> Value {
    let mut body = json!({
        "Name": name,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": { "InstanceCount": 1, "MasterInstanceType": "m5.xlarge" },
    });
    if let Some(label) = release_label {
        body["ReleaseLabel"] = json!(label);
    }
    body
}

/// Every release label the service publishes, read through `ListReleaseLabels` — the
/// catalog as a caller finds it, newest release first.
async fn published_labels(emr: &Emr) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({});
        if let Some(next) = token.as_ref() {
            body["NextToken"] = json!(next);
        }
        let payload = emr.ok("ListReleaseLabels", body).await;
        let page = at(&payload, &["ReleaseLabels"])
            .as_array()
            .cloned()
            .unwrap_or_else(|| {
                panic!("ListReleaseLabels must answer a ReleaseLabels list, got {payload}")
            });
        labels.extend(page.iter().filter_map(|label| {
            label.as_str().map(str::to_string)
        }));
        match at(&payload, &["NextToken"]).as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    assert!(
        labels.len() > 2,
        "the catalog must publish more than two releases, got {labels:?}"
    );
    labels
}

/// The cluster ids `ListClusters` reports.
async fn listed_clusters(emr: &Emr) -> Vec<String> {
    let payload = emr.ok("ListClusters", json!({})).await;
    at(&payload, &["Clusters"])
        .as_array()
        .map(|clusters| {
            clusters
                .iter()
                .filter_map(|cluster| cluster["Id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_launch_is_served_only_for_a_release_the_catalog_offers() {
    let emr = Emr::new();
    let published = published_labels(&emr).await;

    // ACCEPTED: the catalog's oldest and newest releases, and one from the middle of it,
    // taken out of the catalog rather than written here. A launch that names no release
    // label at all is accepted too: the release is then the one `AmiVersion` selects.
    let mut accepted: Vec<Option<String>> = vec![
        Some(published[0].clone()),
        Some(published[published.len() / 2].clone()),
        published.last().cloned(),
        None,
    ];
    accepted.dedup();
    for (index, label) in accepted.iter().enumerate() {
        let case = format!("accepted {label:?}");
        let before = listed_clusters(&emr).await;
        let name = format!("served-{index}");
        let (status, payload) = emr.call("RunJobFlow", launch(&name, label.as_deref())).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a release the listing publishes must be served, got {status} \
             with {payload}"
        );
        let cluster_id = payload["JobFlowId"].as_str().unwrap_or_default().to_string();
        assert!(
            !cluster_id.is_empty(),
            "case {case:?}: an accepted launch must answer a JobFlowId, got {payload}"
        );
        let after = listed_clusters(&emr).await;
        assert_eq!(
            after.len(),
            before.len() + 1,
            "case {case:?}: an accepted launch must leave one more cluster, got {after:?}"
        );

        // The cluster reports back the release it was launched with, so the accepted
        // value was persisted rather than only let through.
        let described = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            described["ReleaseLabel"].as_str().map(str::to_string),
            label.clone(),
            "case {case:?}: the cluster must report the release it was launched with, \
             got {described}"
        );
    }

    // REFUSED: every way a label can name no release at all.
    let unusable: Vec<String> = vec![
        // A patch release of a line the catalog records the patch releases of, which that
        // line never published.
        "emr-6.15.99".to_string(),
        // A major release no series was ever numbered under this label form.
        "emr-99.99.0".to_string(),
        // One patch past the newest release the catalog publishes, read off the catalog.
        next_patch(&published[0]),
        // The version without the prefix every label carries.
        published[0]
            .strip_prefix("emr-")
            .expect("a published label carries the emr- prefix")
            .to_string(),
        // The prefix in another case: a label is spelled exactly one way.
        published[0].to_uppercase(),
        // A line rather than a patch release.
        "emr-7.5".to_string(),
        // `XmlStringMaxLen256` admits the empty string, which names no release.
        String::new(),
    ];
    for label in &unusable {
        let case = format!("refused {label:?}");
        let before = listed_clusters(&emr).await;
        let (status, payload) = emr.call("RunJobFlow", launch("refused", Some(label))).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a label naming no release must be refused 400, \
             got {status} with {payload}"
        );
        let code = at(&payload, &["__type"]).as_str().unwrap_or_default();
        assert!(
            code.contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {code:?} in {payload}"
        );
        assert_eq!(
            at(&payload, &["message"])
                .as_str()
                .or_else(|| at(&payload, &["Message"]).as_str()),
            Some(format!("The supplied release label is invalid: {label}.").as_str()),
            "case {case:?}: expected the invalid-release-label sentence, got {payload}"
        );
        assert!(
            payload.get("JobFlowId").is_none(),
            "case {case:?}: a refused launch hands back no cluster id, got {payload}"
        );
        assert_eq!(
            listed_clusters(&emr).await,
            before,
            "case {case:?}: a refused launch must write no cluster"
        );
    }
}

/// The label one patch release past `label`, which the catalog therefore does not publish.
fn next_patch(label: &str) -> String {
    let version = label
        .strip_prefix("emr-")
        .expect("a published label carries the emr- prefix");
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().unwrap_or_default();
    let patch: u32 = parts
        .next()
        .and_then(|patch| patch.parse().ok())
        .expect("a published label carries a numeric patch component");
    format!("emr-{major}.{minor}.{}", patch + 1)
}

/// Labels the release catalog publishes, each one a launch is served for.
///
/// Hand-written rows from the Release Guide's release-version tables, differing along WHICH
/// family and series a label belongs to: the oldest series the catalog lists and the newest; a
/// line's first release and a maintenance patch of the same line; the `ReleaseLabel` doc's own
/// example release version; a PRE-GA label, which carries a qualifier after the version instead
/// of being an `emr-<major>.<minor>.<patch>` label at all; and the Spark family, which carries
/// its own prefix. Every label the catalog publishes is covered by
/// [`every_label_the_listing_publishes_is_served`]; these rows name the KINDS, so a family
/// dropped from the launch's resolution is reported as itself rather than as one of a hundred
/// listed labels.
const PUBLISHED_LABELS: &[&str] = &[
    // The `RunJobFlow$ReleaseLabel` doc's own example release version.
    "emr-5.14.0",
    // The 4.x series: the oldest label the catalog lists, and the newest line of the series.
    "emr-4.2.0",
    "emr-4.9.5",
    // The 5.x series: its first release, one from the middle of it, and a maintenance patch of
    // the same line.
    "emr-5.0.0",
    "emr-5.20.0",
    "emr-5.20.1",
    // The 6.x series at its last line, and the pre-GA labels of its first.
    "emr-6.15.0",
    "emr-6.0.0-beta",
    "emr-6.0.0-beta2",
    // The 7.x series, the one Amazon EMR still publishes to, at its newest lines.
    "emr-7.9.0",
    "emr-7.14.0",
    // The Spark release family, whose labels carry their own prefix.
    "emr-spark-8.0.0",
    "emr-spark-8.1.0",
];

/// Labels naming no release the catalog publishes, each refused.
///
/// The rows differ along the WAY the label names no release: a line Amazon EMR no longer lists,
/// so the catalog publishes neither it nor any patch of it (the 4.0, 4.1 and 4.7 lines); one
/// line past the newest line of each series; a series the label form never numbered, below the
/// 4.0 floor and above the last; a major release the numeric form never numbered (the Spark
/// family's is not one of them); a patch release of a line the catalog lists that the line
/// never published, both inside a gap in its patches and past its last; the Spark family past
/// its newest line; a pre-GA qualifier no release carries; and the spellings a label is never
/// written in — no prefix, another case, a padded component, a trailing space, a missing
/// component, an extra component and a non-numeric component.
const NO_PUBLISHED_RELEASE: &[&str] = &[
    "emr-4.0.0",
    "emr-4.1.0",
    "emr-4.7.0",
    "emr-4.10.0",
    "emr-5.37.0",
    "emr-6.16.0",
    "emr-7.15.0",
    "emr-7.90.0",
    "emr-3.11.0",
    "emr-8.0.0",
    "emr-6.15.1",
    "emr-5.0.1",
    "emr-5.36.3",
    "emr-spark-8.2.0",
    "emr-spark-9.0.0",
    "emr-6.0.0-beta3",
    "5.20.0",
    "EMR-5.20.0",
    "emr-05.20.0",
    "emr-5.20.0 ",
    "emr-5.20",
    "emr-5.20.0.1",
    "emr-5.x.0",
    "emr-spark-8.0",
];

/// An instance type the release `release_label` supports, read through
/// `ListSupportedInstanceTypes` — "the list of instance types that the release specified …
/// supports" — so a launch below names capacity its release can really be provisioned from and
/// the case is about the LABEL rather than about the instance type (an older release supports
/// none of the current instance families).
async fn supported_instance_type(emr: &Emr, release_label: &str) -> String {
    let payload = emr
        .ok(
            "ListSupportedInstanceTypes",
            json!({ "ReleaseLabel": release_label }),
        )
        .await;
    at(&payload, &["SupportedInstanceTypes"])
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["Type"].as_str())
        .unwrap_or_else(|| {
            panic!(
                "case {release_label:?}: a published release supports an instance type, \
                 got {payload}"
            )
        })
        .to_string()
}

/// Launch a cluster named `name` on `release_label`, asserting the launch is served and that
/// the cluster reads back through `DescribeCluster` reporting the release it was launched with.
async fn assert_served(emr: &Emr, case: &str, name: &str, release_label: &str) {
    let instance_type = supported_instance_type(emr, release_label).await;
    let mut request = launch(name, Some(release_label));
    request["Instances"]["MasterInstanceType"] = json!(instance_type);
    let (status, payload) = emr.call("RunJobFlow", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a published release must be served, got {status} with {payload}"
    );
    let cluster_id = payload["JobFlowId"].as_str().unwrap_or_default().to_string();
    assert!(
        !cluster_id.is_empty(),
        "case {case:?}: a served launch must answer a JobFlowId, got {payload}"
    );
    assert!(
        payload["ClusterArn"]
            .as_str()
            .is_some_and(|arn| !arn.is_empty()),
        "case {case:?}: a served launch must answer a ClusterArn, got {payload}"
    );

    let described = emr.describe_cluster(&cluster_id).await;
    assert_eq!(
        described["ReleaseLabel"].as_str(),
        Some(release_label),
        "case {case:?}: the cluster must report the release it was launched with, got {described}"
    );
}

/// Every KIND of label the catalog publishes is one a launch is served for, and the cluster the
/// launch leaves reports the release it was launched with.
#[tokio::test]
async fn a_launch_naming_a_published_release_is_served() {
    let emr = Emr::new();
    for (index, label) in PUBLISHED_LABELS.iter().enumerate() {
        assert_served(&emr, label, &format!("published-{index}"), label).await;
    }
}

#[tokio::test]
async fn a_launch_naming_no_published_release_is_refused() {
    let emr = Emr::new();
    for label in NO_PUBLISHED_RELEASE {
        let before = listed_clusters(&emr).await;
        let (status, payload) = emr
            .call("RunJobFlow", launch("unpublished", Some(label)))
            .await;
        assert_eq!(
            status, 400,
            "case {label:?}: a label naming no release must be refused 400, got {status} \
             with {payload}"
        );
        let code = at(&payload, &["__type"]).as_str().unwrap_or_default();
        assert!(
            code.contains("ValidationException"),
            "case {label:?}: expected ValidationException, got {code:?} in {payload}"
        );
        assert_eq!(
            at(&payload, &["message"])
                .as_str()
                .or_else(|| at(&payload, &["Message"]).as_str()),
            Some(format!("The supplied release label is invalid: {label}.").as_str()),
            "case {label:?}: expected the invalid-release-label sentence, got {payload}"
        );
        assert!(
            payload.get("JobFlowId").is_none(),
            "case {label:?}: a refused launch hands back no cluster id, got {payload}"
        );
        assert_eq!(
            listed_clusters(&emr).await,
            before,
            "case {label:?}: a refused launch must write no cluster"
        );
    }
}

/// A caller discovers the releases it may pin a cluster to through `ListReleaseLabels`, so
/// EVERY label that listing publishes is one a launch is served for: the two cannot disagree.
#[tokio::test]
async fn every_label_the_listing_publishes_is_served() {
    let emr = Emr::new();
    let published = published_labels(&emr).await;
    for (index, label) in published.iter().enumerate() {
        assert_served(&emr, label, &format!("listed-{index}"), label).await;
    }
}
