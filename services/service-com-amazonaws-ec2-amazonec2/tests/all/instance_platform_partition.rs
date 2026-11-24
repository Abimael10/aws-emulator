// SPDX-License-Identifier: CC0-1.0

//! Partitioning Windows instances from Linux ones server-side, end to end over the ec2Query
//! wire through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test, in two halves that have to agree:
//!
//! 1. THE WORD AN INSTANCE REPORTS: "the platform. This value is `windows` for Windows
//!    instances; otherwise, it is empty" (`Instance$Platform`). The member carries the
//!    LOWER-CASE word that sentence publishes for an instance launched from an AMI that
//!    carries a platform at all, and is left out entirely otherwise — which is what makes a
//!    `--query '…[InstanceType,Platform]' --output text` projection render the missing
//!    column as the CLI's own `None`. The billing member beside it, "the platform details
//!    associated with the billing code of the AMI" (`Instance$PlatformDetails`), is the AMI's
//!    own whichever operating system it holds.
//! 2. THE FILTERS THAT SELECT ON THEM: "`platform` - the platform. To list only Windows
//!    instances, use `windows`" and "`platform-details` - the platform (`Linux/UNIX` | … |
//!    `Windows` | `Windows BYOL` | …)" (`DescribeInstancesRequest$Filters`). Each selects on
//!    the value the operation REPORTS for its member, beside whatever other predicates the
//!    request carries, so a caller narrowing by tag and state can split its Windows
//!    instances from its Linux ones in one call.
//!
//! Every row is a distinct input along what the rule generalises over: which operating system
//! the AMI holds, whether it carries a platform member, and which of the billing platforms
//! the filter's own documentation publishes it bills as. The seeded platform-details values
//! are entries of that published list (`smithy_query(query="doc", shape=
//! "DescribeInstancesRequest", member="Filters")`); two rows are launched from the AMIs the
//! service's own catalog publishes instead, discovered through `DescribeImages`, so no row
//! depends on an id.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, IMAGE_PLATFORM_WINDOWS, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements, filters, indexed, tag_specification};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// The tag every launch of this suite carries, as a caller tags the estate it is about to
/// price: the filter rows narrow by it so nothing else the account holds is selected.
const PROBE_TAG: (&str, &str) = ("cost-probe", "platform-partition");

/// The word `Instance$Platform` publishes for a Windows instance — written out here, rather
/// than taken from the crate, because the LITERAL is the requirement: it is the same token
/// the `platform` filter takes and the same token a CLI or jq pipeline compares.
const REPORTED_WINDOWS: &str = "windows";

/// The resource type a launch assigns its instance tags to (`TagSpecification$ResourceType`).
const INSTANCE_RESOURCE: &str = "instance";

/// How long a poll here keeps looking: a large multiple of the service's own transition span,
/// so a loaded or slow host is never the reason an assertion fails. Nothing is waited out —
/// the CONDITION is looked for again until it holds.
fn poll_deadline() -> Duration {
    ec2::state::TRANSITION_DELAY * 40
}

/// One look inside that deadline, so the deadline costs many looks rather than one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The state a launch reaches once the clock owes it (`InstanceState$Name`).
const RUNNING: &str = "running";

/// One AMI this suite seeds: the case it is named by, the `Image$Platform` it carries (the
/// one value the modelled `PlatformValues` enum publishes, for an image that holds Windows)
/// and the `Image$PlatformDetails` it bills as.
///
/// The platform-details column is drawn from the catalog the `platform-details` filter's own
/// documentation publishes; the rows span both halves of it, the operating systems that carry
/// a platform member and those that carry none.
type Seed = (&'static str, Option<&'static str>, &'static str);

/// The seeded rows: three Windows billing platforms and four Linux ones.
const SEEDED: [Seed; 7] = [
    ("a Windows Server AMI", Some(IMAGE_PLATFORM_WINDOWS), "Windows"),
    (
        "a Windows AMI under a brought-in licence",
        Some(IMAGE_PLATFORM_WINDOWS),
        "Windows BYOL",
    ),
    (
        "a Windows AMI billed with SQL Server",
        Some(IMAGE_PLATFORM_WINDOWS),
        "Windows with SQL Server Standard",
    ),
    ("a plain Linux AMI", None, "Linux/UNIX"),
    ("a SUSE Linux AMI", None, "SUSE Linux"),
    ("a Red Hat Enterprise Linux AMI", None, "Red Hat Enterprise Linux"),
    ("an Ubuntu Pro AMI", None, "Ubuntu Pro"),
];

/// One row of the table: the AMI it launches from, what that AMI holds, and the instance the
/// launch minted.
#[derive(Debug, Clone)]
struct Row {
    case: String,
    image_id: String,
    /// Whether the AMI carries an `Image$Platform` at all — the only thing that decides
    /// whether the instance reports the word.
    holds_windows: bool,
    /// The `Image$PlatformDetails` the AMI bills as, which the instance reports as its own.
    platform_details: String,
    instance_id: String,
}

/// One call that must be served, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The published catalog's own rows, as `(image id, its platform member, its platform
/// details)` — read off `DescribeImages` so neither an id nor a platform word is written
/// into the table.
async fn published(router: &MultiServiceRouter, case: &str) -> Vec<(String, Option<String>, String)> {
    let body = ok(router, case, "DescribeImages", &[]).await;
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "imagesSet")
        .into_iter()
        .filter_map(|image| {
            let image_id = child_text(image, "imageId")?;
            let details = child_text(image, "platformDetails")?;
            Some((image_id, child_text(image, "platform"), details))
        })
        .collect()
}

/// Launch one instance from `image_id`, tagged with the probe tag, and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, image_id: &str) -> String {
    let mut params = vec![
        param("ImageId", image_id),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    params.extend(tag_specification(INSTANCE_RESOURCE, &[PROBE_TAG]));
    let body = ok(router, case, "RunInstances", &params).await;
    let launched = elements(&body, "instanceId");
    assert_eq!(
        launched.len(),
        1,
        "case {case:?}: expected one instance from RunInstances, got {launched:?} in {body}"
    );
    launched[0].clone()
}

/// The one instance a `DescribeInstances` body reports for a single id: the element carrying
/// both the id and the state, never a nested element of the same name.
fn described<'a, 'i>(
    case: &str,
    parsed: &'a roxmltree::Document<'i>,
    body: &str,
) -> roxmltree::Node<'a, 'i> {
    parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no instance in {body}"))
}

/// `DescribeInstances` for one instance, as its body.
async fn describe(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await
}

/// Look again until `instance_id` reports `running`, or a deadline far past the service's own
/// transition span passes: the launch owes that move to the clock, so what is polled for is
/// the state rather than a span of time.
async fn poll_until_running(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let body = describe(router, case, instance_id).await;
        let state = {
            let parsed = document(case, &body);
            let instance = described(case, &parsed, &body);
            child(instance, "instanceState")
                .and_then(|state| child_text(state, "name"))
                .unwrap_or_default()
        };
        if state == RUNNING {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} never reached {RUNNING}, last state {state:?} in {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What `DescribeInstances` reports for one instance's two platform members:
/// `(Instance$Platform, Instance$PlatformDetails)`, each `None` when the member is absent.
async fn reported_platform(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (Option<String>, Option<String>) {
    let body = describe(router, case, instance_id).await;
    let parsed = document(case, &body);
    let instance = described(case, &parsed, &body);
    (
        child_text(instance, "platform"),
        child_text(instance, "platformDetails"),
    )
}

/// The ids `DescribeInstances` reports for `params`, as a set — a selection is a set of
/// instances rather than an order.
async fn selected(
    router: &MultiServiceRouter,
    case: &str,
    params: &[Param],
) -> BTreeSet<String> {
    let body = ok(router, case, "DescribeInstances", params).await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .filter_map(|instance| child_text(instance, "instanceId"))
        .collect()
}

/// The tag and state predicates every selection below carries, as a cost client narrows its
/// own running estate, with `extra` appended — the platform predicate a row is about.
///
/// `tag:<key>` takes the tag key in the filter NAME, so the name is built from the probe key
/// rather than written out.
fn narrowed(extra: &[(&str, &[&str])]) -> Vec<Param> {
    let (key, value) = PROBE_TAG;
    let tag_name = format!("tag:{key}");
    let tag_values = [value];
    let state_values = [RUNNING];
    let mut entries: Vec<(&str, &[&str])> = vec![
        (tag_name.as_str(), &tag_values[..]),
        ("instance-state-name", &state_values[..]),
    ];
    entries.extend_from_slice(extra);
    filters(&entries)
}

/// Seed the table's AMIs, launch one instance from each and from two the catalog publishes,
/// and wait for every launch to be running.
async fn table() -> (MultiServiceRouter, Vec<Row>) {
    let case = "the platform table";
    let (router, engine) = super::ec2_query::emulator();

    // The AMIs this suite writes, one per billing platform of the published catalog of
    // `platform-details`. Only the members the rule reads are set; everything else about a
    // stored image is the store's own default.
    let mut rows: Vec<Row> = Vec::new();
    let mut seeded: Vec<(&str, bool, &str, String)> = Vec::new();
    {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        for (case, platform, platform_details) in SEEDED {
            let image_id = guard.insert_image(ImageSpec {
                name: format!("probe-{}", platform_details.replace(' ', "-").to_lowercase()),
                description: format!("A probe AMI billed as {platform_details}"),
                platform: platform.map(str::to_string),
                platform_details: platform_details.to_string(),
                ..ImageSpec::default()
            });
            seeded.push((case, platform.is_some(), platform_details, image_id));
        }
    }
    for (case, holds_windows, platform_details, image_id) in seeded {
        let instance_id = launch(&router, case, &image_id).await;
        rows.push(Row {
            case: case.to_string(),
            image_id,
            holds_windows,
            platform_details: platform_details.to_string(),
            instance_id,
        });
    }

    // Two rows from the catalog the service itself publishes: the Windows AMI a client
    // resolves by name, and an image published with no platform at all.
    let catalog = published(&router, case).await;
    for (wanted_windows, row_case) in [
        (true, "the published Windows AMI"),
        (false, "a published AMI carrying no platform"),
    ] {
        let (image_id, platform, platform_details) = catalog
            .iter()
            .find(|(_, platform, _)| platform.is_some() == wanted_windows)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {row_case:?}: the published catalog must hold such an image")
            });
        let instance_id = launch(&router, row_case, &image_id).await;
        rows.push(Row {
            case: row_case.to_string(),
            image_id,
            holds_windows: platform.is_some(),
            platform_details,
            instance_id,
        });
    }

    for row in &rows {
        poll_until_running(&router, &row.case, &row.instance_id).await;
    }
    (router, rows)
}

/// RULE 1: the platform word an instance reports is the lower-case literal for every AMI
/// that holds Windows, is absent for every AMI that holds none, and sits beside the billing
/// platform details of the AMI itself.
#[tokio::test]
async fn an_instance_reports_the_lower_case_platform_word_only_for_a_windows_ami() {
    let (router, rows) = table().await;
    for row in &rows {
        let case = &row.case;
        let expected = row.holds_windows.then_some(REPORTED_WINDOWS);
        let (platform, platform_details) = reported_platform(&router, case, &row.instance_id).await;
        assert_eq!(
            platform.as_deref(),
            expected,
            "case {case:?}: the instance launched from {} must report the platform \
             {expected:?}, got {platform:?}",
            row.image_id
        );
        assert_eq!(
            platform_details.as_deref(),
            Some(row.platform_details.as_str()),
            "case {case:?}: the instance must report the AMI's platform details {:?}, got \
             {platform_details:?}",
            row.platform_details
        );
    }
}

/// RULE 2: the `platform` filter selects exactly the instances that report the word, and the
/// `platform-details` filter exactly those reporting each billing platform — both beside the
/// tag and state predicates a caller narrows its estate with.
#[tokio::test]
async fn the_platform_filters_partition_the_estate_without_losing_an_instance() {
    let (router, rows) = table().await;
    let everything: BTreeSet<String> = rows.iter().map(|row| row.instance_id.clone()).collect();

    // The same tag and state predicates without any platform one: every launch, so an empty
    // answer below can never be the tag or the state filter's doing.
    let case = "the whole probed estate";
    let found = selected(&router, case, &narrowed(&[])).await;
    assert_eq!(
        found, everything,
        "case {case:?}: the tag and state predicates must select every probed instance, got \
         {found:?}"
    );

    // "To list only Windows instances, use `windows`": exactly the instances launched from an
    // AMI that holds Windows, and none of the Linux ones.
    let windows: BTreeSet<String> = rows
        .iter()
        .filter(|row| row.holds_windows)
        .map(|row| row.instance_id.clone())
        .collect();
    assert!(
        windows.len() > 1 && windows.len() < everything.len(),
        "the table must hold more than one Windows instance and some Linux ones, got \
         {windows:?} of {everything:?}"
    );
    let case = "platform=windows";
    let found = selected(
        &router,
        case,
        &narrowed(&[("platform", &[REPORTED_WINDOWS])]),
    )
    .await;
    assert_eq!(
        found, windows,
        "case {case:?}: expected the Windows instances {windows:?}, got {found:?}"
    );

    // Each billing platform selects the instances of the AMIs billed as it, however many
    // that is.
    let mut by_details: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for row in &rows {
        by_details
            .entry(row.platform_details.as_str())
            .or_default()
            .insert(row.instance_id.clone());
    }
    for (platform_details, expected) in &by_details {
        let case = format!("platform-details={platform_details}");
        let found = selected(
            &router,
            &case,
            &narrowed(&[("platform-details", &[platform_details])]),
        )
        .await;
        assert_eq!(
            &found, expected,
            "case {case:?}: expected {expected:?}, got {found:?}"
        );
    }

    // A platform no instance reports selects none of them — a normal answer rather than a
    // refusal — and so does a billing platform the published list holds that no AMI here
    // bills as.
    for value in ["linux", "Linux/UNIX"] {
        let case = format!("platform={value}");
        let found = selected(&router, &case, &narrowed(&[("platform", &[value])])).await;
        assert!(
            found.is_empty(),
            "case {case:?}: no instance reports that platform, so none is selected, got {found:?}"
        );
    }
    let case = "platform-details=Ubuntu Pro with SQL Server";
    let found = selected(
        &router,
        case,
        &narrowed(&[("platform-details", &["Ubuntu Pro with SQL Server"])]),
    )
    .await;
    assert!(
        found.is_empty(),
        "case {case:?}: no AMI here bills as that platform, so no instance is selected, got \
         {found:?}"
    );
}
