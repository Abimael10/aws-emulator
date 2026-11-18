// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DescribeInstanceImageMetadata`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=DescribeInstanceImageMetadata`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test is that the operation REPORTS THE STORE: for every instance the
//! account holds in the Region the request was signed for, the instance's own identity,
//! placement, state and tags beside the metadata of the AMI it was launched from — read off
//! the image store whatever has become of that image, because the operation "describes the
//! AMI that was used to launch an instance, even if the AMI is deprecated, deregistered,
//! made private (no longer public or shared with your account), or not allowed" — narrowed
//! by the `InstanceIds` and `Filters` the request names and cut into `MaxResults`-sized
//! pages.
//!
//! Every table row is a distinct input along what that generalises over: which AMI an
//! instance was launched from (the account's own, a public one published under an owner
//! alias, a deprecated one, another account's private one, and an id the store holds no
//! image under), which filter and which values, which ids, which page size and which
//! Region. The recorded call that asks for every instance is one row among them rather than
//! the specification.

use std::collections::{BTreeMap, BTreeSet};

use ec2::state::{AMAZON_IMAGE_OWNER_ID, ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{error_code, error_message, filters, unescaped};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call makes unless a row names another.
const REGION: &str = "us-east-1";

/// A second Region, which the regional scope of the report is read against.
const OTHER_REGION: &str = "eu-west-1";

/// The account another seeded image belongs to.
const PARTNER_ACCOUNT: &str = "111122223333";

/// A well-formed AMI id the image store holds no image under: what an instance launched
/// from an AMI this store does not hold names.
const ABSENT_IMAGE: &str = "ami-0123456789abcdef0";

/// The alias a published Amazon image carries (`Image$ImageOwnerAlias`).
const AMAZON_ALIAS: &str = "amazon";

/// A deprecation time that has passed, as an image carries one.
const PAST_DEPRECATION: &str = "2024-01-01T00:00:00.000Z";

/// A deprecation time still ahead.
const FUTURE_DEPRECATION: &str = "2999-12-31T23:59:59.000Z";

/// The images seeded beside the published catalog, one per class of AMI the report has to
/// describe: the account's own available image, a public image published under an owner
/// alias, an image whose deprecation time has passed, and another account's private image
/// (which this account cannot see through `DescribeImages` at all).
fn seeded_specs() -> Vec<(&'static str, ImageSpec)> {
    vec![
        (
            "own-available",
            ImageSpec {
                name: "app-own-golden".to_string(),
                description: "The account's own golden image".to_string(),
                ..ImageSpec::default()
            },
        ),
        (
            "amazon-public",
            ImageSpec {
                name: "al2026-ami-x86_64".to_string(),
                description: "A published Amazon image".to_string(),
                public: true,
                owner_id: AMAZON_IMAGE_OWNER_ID.to_string(),
                owner_alias: Some(AMAZON_ALIAS.to_string()),
                deprecation_time: Some(FUTURE_DEPRECATION.to_string()),
                ..ImageSpec::default()
            },
        ),
        (
            "deprecated",
            ImageSpec {
                name: "app-retired".to_string(),
                description: "An image whose deprecation time has passed".to_string(),
                deprecation_time: Some(PAST_DEPRECATION.to_string()),
                ..ImageSpec::default()
            },
        ),
        (
            "partner-private",
            ImageSpec {
                name: "partner-private".to_string(),
                description: "Another account's private image".to_string(),
                owner_id: PARTNER_ACCOUNT.to_string(),
                ..ImageSpec::default()
            },
        ),
    ]
}

/// One launch of the table below: the key its row is named by, the image it names, the
/// instance type it asks for and the tags it assigns.
type Launch = (&'static str, &'static str, &'static str, &'static [(&'static str, &'static str)]);

/// Which instance is launched from which image, with which instance type and which tags.
/// The image column names a seeded image, or an AMI id the store holds none under.
const LAUNCHES: [Launch; 5] = [
    ("own", "own-available", "t3.micro", &[("Name", "alpha"), ("Team", "core")]),
    ("amazon", "amazon-public", "m5.large", &[("Name", "beta")]),
    ("retired", "deprecated", "t3.small", &[]),
    ("private", "partner-private", "t3.medium", &[]),
    ("imageless", ABSENT_IMAGE, "c5.large", &[]),
];

/// One reported AMI, as the members the assertions read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ImageRow {
    image_id: String,
    name: String,
    owner_id: String,
    state: String,
    owner_alias: String,
    creation_date: String,
    deprecation_time: String,
    image_allowed: String,
    is_public: String,
}

/// One reported instance, as the members the assertions read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Row {
    instance_id: String,
    instance_type: String,
    launch_time: String,
    availability_zone: String,
    zone_id: String,
    state_name: String,
    state_code: String,
    owner_id: String,
    tags: Vec<(String, String)>,
    image: Option<ImageRow>,
    operator_managed: String,
}

/// What a seeded emulator holds: the router, the seeded image ids and the launched
/// instance ids, each by the key its table row names it with.
struct Fixture {
    router: MultiServiceRouter,
    specs: BTreeMap<&'static str, ImageSpec>,
    images: BTreeMap<&'static str, String>,
    instances: BTreeMap<&'static str, String>,
}

impl Fixture {
    /// The image id of a seeded row, or the AMI id a row names directly.
    fn image_id(&self, key: &str) -> String {
        self.images
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string())
    }

    /// The instance id of a launch row.
    fn instance_id(&self, key: &str) -> String {
        self.instances
            .get(key)
            .unwrap_or_else(|| panic!("{key} must be a launched instance"))
            .clone()
    }

    /// The ids of `keys`, in the order the service reports them: an instance id is the
    /// zero-padded hex of a counter that only grows, so sorting them is launch order.
    fn instance_ids(&self, keys: &[&str]) -> Vec<String> {
        let mut ids: Vec<String> = keys.iter().map(|key| self.instance_id(key)).collect();
        ids.sort();
        ids
    }
}

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = super::ec2_query::call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Seed the images and launch every instance of [`LAUNCHES`].
async fn fixture() -> Fixture {
    let (router, engine) = super::ec2_query::emulator();
    let specs = seeded_specs();
    let mut images: BTreeMap<&'static str, String> = BTreeMap::new();
    {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        for (key, spec) in &specs {
            images.insert(key, guard.insert_image(spec.clone()));
        }
    }
    let mut fixture = Fixture {
        router,
        specs: specs.into_iter().collect(),
        images,
        instances: BTreeMap::new(),
    };
    for (key, image, instance_type, tags) in LAUNCHES {
        let mut params = vec![
            param("ImageId", &fixture.image_id(image)),
            param("InstanceType", instance_type),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ];
        params.extend(super::ec2_query::tag_specification("instance", tags));
        let body = ok(key, &fixture.router, "RunInstances", &params).await;
        let launched = super::ec2_query::elements(&body, "instanceId");
        assert_eq!(
            launched.len(),
            1,
            "case {key:?}: expected one instance from RunInstances, got {launched:?}"
        );
        fixture.instances.insert(key, launched[0].clone());
    }
    fixture
}

/// One `DescribeInstanceImageMetadata` call in `region`, as its status and body.
async fn describe(
    router: &MultiServiceRouter,
    region: &str,
    params: &[Param],
) -> (u16, String) {
    super::ec2_query::call(router, region, "DescribeInstanceImageMetadata", params).await
}

/// The rows a successful response reports, in the order it reported them.
fn rows(case: &str, body: &str) -> Vec<Row> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "instanceImageMetadataSet")
        .into_iter()
        .map(|item| {
            let text = |name: &str| child_text(item, name).unwrap_or_default();
            let state = child(item, "instanceState");
            Row {
                instance_id: text("instanceId"),
                instance_type: text("instanceType"),
                launch_time: text("launchTime"),
                availability_zone: text("availabilityZone"),
                zone_id: text("zoneId"),
                state_name: state
                    .and_then(|state| child_text(state, "name"))
                    .unwrap_or_default(),
                state_code: state
                    .and_then(|state| child_text(state, "code"))
                    .unwrap_or_default(),
                owner_id: text("instanceOwnerId"),
                tags: set_items(item, "tagSet")
                    .into_iter()
                    .map(|tag| {
                        (
                            child_text(tag, "key").unwrap_or_default(),
                            child_text(tag, "value").unwrap_or_default(),
                        )
                    })
                    .collect(),
                image: child(item, "imageMetadata").map(|image| {
                    let text = |name: &str| child_text(image, name).unwrap_or_default();
                    ImageRow {
                        image_id: text("imageId"),
                        name: text("name"),
                        owner_id: text("imageOwnerId"),
                        state: text("imageState"),
                        owner_alias: text("imageOwnerAlias"),
                        creation_date: text("creationDate"),
                        deprecation_time: text("deprecationTime"),
                        image_allowed: text("imageAllowed"),
                        is_public: text("isPublic"),
                    }
                }),
                operator_managed: child(item, "operator")
                    .and_then(|operator| child_text(operator, "managed"))
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// The rows of a call that must succeed.
async fn described(case: &str, router: &MultiServiceRouter, params: &[Param]) -> Vec<Row> {
    let (status, body) = describe(router, REGION, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    rows(case, &body)
}

/// The instance ids a call reports, in order.
async fn described_ids(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
) -> Vec<String> {
    described(case, router, params)
        .await
        .into_iter()
        .map(|row| row.instance_id)
        .collect()
}

/// Every instance of the Region, with the metadata of the AMI it was launched from —
/// including an AMI the calling account could no longer describe or launch.
#[tokio::test]
async fn reports_every_instance_beside_the_ami_it_was_launched_from() {
    let fixture = fixture().await;
    let account = engine::emulator_account_id().to_string();
    let reported = described("every instance", &fixture.router, &[]).await;
    let by_id: BTreeMap<String, Row> = reported
        .iter()
        .map(|row| (row.instance_id.clone(), row.clone()))
        .collect();
    assert_eq!(
        reported.iter().map(|row| row.instance_id.clone()).collect::<Vec<_>>(),
        fixture.instance_ids(&LAUNCHES.map(|launch| launch.0)),
        "case \"every instance\": the report covers every launched instance, in id order"
    );

    for (key, image, instance_type, tags) in LAUNCHES {
        let case = format!("the instance launched from {image}");
        let id = fixture.instance_id(key);
        let row = by_id
            .get(&id)
            .unwrap_or_else(|| panic!("case {case:?}: {id} must be reported: {reported:?}"));
        assert_eq!(
            row.instance_type, instance_type,
            "case {case:?}: expected instanceType {instance_type}, got {row:?}"
        );
        assert_eq!(
            row.owner_id, account,
            "case {case:?}: the instance is owned by the calling account, got {row:?}"
        );
        assert_eq!(
            row.operator_managed, "false",
            "case {case:?}: no service provider manages the instance, got {row:?}"
        );
        // The zone, its id and the launch time are the instance's own facts, read against
        // the crate's one name -> id lookup and the report `DescribeInstances` gives for the
        // same instance, so the two reports cannot disagree.
        assert_eq!(
            row.zone_id,
            ec2::state::zone_id_for_zone_name(&row.availability_zone).unwrap_or_default(),
            "case {case:?}: zoneId must be the id of {}, got {row:?}",
            row.availability_zone
        );
        assert!(
            super::ec2_query::as_timestamp(&row.launch_time).is_some(),
            "case {case:?}: launchTime must be an ISO 8601 instant, got {row:?}"
        );
        assert_eq!(
            row.state_name,
            super::ec2_query::described_instance_state(&fixture.router, REGION, &id).await,
            "case {case:?}: the state must be the one DescribeInstances reports, got {row:?}"
        );
        let expected_tags: Vec<(String, String)> = tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            row.tags, expected_tags,
            "case {case:?}: expected the tags {expected_tags:?}, got {row:?}"
        );

        // The AMI's metadata, projected from the image the store holds under the id the
        // launch resolved — whatever this account's view of that image is now.
        let expected = match fixture.specs.get(image) {
            Some(spec) => ImageRow {
                image_id: fixture.image_id(image),
                name: spec.name.clone(),
                owner_id: spec.owner_id.clone(),
                state: spec.state.clone(),
                owner_alias: spec.owner_alias.clone().unwrap_or_default(),
                creation_date: spec.creation_date.clone(),
                deprecation_time: spec.deprecation_time.clone().unwrap_or_default(),
                // Every image satisfies the Allowed AMIs criteria of an account that
                // declares none, which is what a fresh account holds.
                image_allowed: "true".to_string(),
                is_public: spec.public.to_string(),
            },
            // An id the store holds no image under: the AMI is still named, since the
            // instance's own record is what knows which one launched it.
            None => ImageRow {
                image_id: image.to_string(),
                ..ImageRow::default()
            },
        };
        assert_eq!(
            row.image.as_ref(),
            Some(&expected),
            "case {case:?}: expected the image metadata {expected:?}, got {row:?}"
        );
    }
}

/// Each documented filter selects exactly the instances whose reported row carries a
/// matching value, with EC2's wildcards, its values joined with OR and several filters with
/// AND.
#[tokio::test]
async fn documented_filters_select_on_the_reported_row() {
    let fixture = fixture().await;
    let zone = described("the zone of the launches", &fixture.router, &[])
        .await
        .first()
        .map(|row| row.availability_zone.clone())
        .unwrap_or_default();
    let zone_id = ec2::state::zone_id_for_zone_name(&zone).unwrap_or_default();
    let own_id = fixture.instance_id("own");
    let amazon_id = fixture.instance_id("amazon");
    let every_key: Vec<&str> = LAUNCHES.iter().map(|launch| launch.0).collect();
    // The instances launched from an image the store holds: the four seeded ones.
    let with_image: Vec<&str> = vec!["own", "amazon", "retired", "private"];

    // Rows: one per documented filter name, with the values spanning an exact match, a
    // wildcard, several values (OR) and a value nothing carries.
    /// One filter row: the case, the `Filters` entries it sends, and the launch keys it
    /// must select.
    type FilterRow<'a> = (&'a str, Vec<(&'a str, Vec<&'a str>)>, Vec<&'a str>);
    let rows: Vec<FilterRow<'_>> = vec![
        ("instance-id, exact", vec![("instance-id", vec![own_id.as_str()])], vec!["own"]),
        (
            "instance-id, two values",
            vec![("instance-id", vec![own_id.as_str(), amazon_id.as_str()])],
            vec!["own", "amazon"],
        ),
        (
            "instance-id, a value nothing carries",
            vec![("instance-id", vec!["i-0abcdef1"])],
            vec![],
        ),
        ("instance-type, exact", vec![("instance-type", vec!["t3.micro"])], vec!["own"]),
        (
            "instance-type, a wildcard over a family",
            vec![("instance-type", vec!["t3.*"])],
            vec!["own", "retired", "private"],
        ),
        (
            "availability-zone",
            vec![("availability-zone", vec![zone.as_str()])],
            every_key.clone(),
        ),
        (
            "availability-zone, another zone",
            vec![("availability-zone", vec!["us-west-2a"])],
            vec![],
        ),
        ("zone-id", vec![("zone-id", vec![zone_id.as_str()])], every_key.clone()),
        (
            "owner-id, the image's publisher",
            vec![("owner-id", vec![AMAZON_IMAGE_OWNER_ID])],
            vec!["amazon"],
        ),
        (
            "owner-id, another account",
            vec![("owner-id", vec![PARTNER_ACCOUNT])],
            vec!["private"],
        ),
        ("owner-alias", vec![("owner-alias", vec![AMAZON_ALIAS])], vec!["amazon"]),
        (
            "owner-alias, an alias nothing publishes under",
            vec![("owner-alias", vec!["aws-marketplace"])],
            vec![],
        ),
        (
            "image-allowed, the images the account allows",
            vec![("image-allowed", vec!["true"])],
            with_image.clone(),
        ),
        ("image-allowed, false", vec![("image-allowed", vec!["false"])], vec![]),
        ("tag:<key>", vec![("tag:Name", vec!["alpha"])], vec!["own"]),
        (
            "tag:<key>, a wildcard",
            vec![("tag:Name", vec!["*a"])],
            vec!["own", "amazon"],
        ),
        ("tag-key", vec![("tag-key", vec!["Team"])], vec!["own"]),
        ("tag-key, shared by two", vec![("tag-key", vec!["Name"])], vec!["own", "amazon"]),
        (
            "two filters, joined with AND",
            vec![("tag-key", vec!["Name"]), ("instance-type", vec!["t3.*"])],
            vec!["own"],
        ),
        (
            "two filters, nothing satisfies both",
            vec![("tag-key", vec!["Team"]), ("owner-alias", vec![AMAZON_ALIAS])],
            vec![],
        ),
    ];

    for (case, entries, expected) in rows {
        let params = filters(
            &entries
                .iter()
                .map(|(name, values)| (*name, values.as_slice()))
                .collect::<Vec<_>>(),
        );
        let reported = described_ids(case, &fixture.router, &params).await;
        assert_eq!(
            reported,
            fixture.instance_ids(&expected),
            "case {case:?}: expected the instances {expected:?}, got {reported:?}"
        );
    }
}

/// The `launch-time` filter selects on the instant the instance reports, with the wildcard
/// its documentation gives ("`2023-09-29T*`, which matches an entire day").
#[tokio::test]
async fn the_launch_time_filter_selects_on_the_reported_instant() {
    let fixture = fixture().await;
    let reported = described("the launch times", &fixture.router, &[]).await;
    let exact = reported
        .first()
        .map(|row| row.launch_time.clone())
        .unwrap_or_default();
    let day = exact
        .split_once('T')
        .map(|(date, _)| format!("{date}T*"))
        .unwrap_or_default();
    let every = fixture.instance_ids(&LAUNCHES.map(|launch| launch.0));

    for (case, value, expected) in [
        ("the exact instant of one instance", exact.as_str(), vec![reported[0].instance_id.clone()]),
        ("a wildcard over the day they launched", day.as_str(), every.clone()),
        ("a wildcard over another day", "1999-01-01T*", Vec::new()),
        ("an instant nothing launched at", "2024-03-04T05:06:07.000Z", Vec::new()),
    ] {
        let selected = described_ids(
            case,
            &fixture.router,
            &filters(&[("launch-time", &[value])]),
        )
        .await;
        assert_eq!(
            selected, expected,
            "case {case:?}: launch-time={value} must select {expected:?}, got {selected:?}"
        );
    }
}

/// The `instance-state-name` filter selects the instances whose reported state it names,
/// whatever state the clock has taken them to by now.
#[tokio::test]
async fn the_instance_state_filter_selects_on_the_reported_state() {
    let fixture = fixture().await;
    let reported = described("the states", &fixture.router, &[]).await;
    let states: BTreeSet<String> = reported.iter().map(|row| row.state_name.clone()).collect();
    for state in &states {
        let case = format!("instance-state-name={state}");
        let mut expected: Vec<String> = reported
            .iter()
            .filter(|row| &row.state_name == state)
            .map(|row| row.instance_id.clone())
            .collect();
        expected.sort();
        let selected = described_ids(
            &case,
            &fixture.router,
            &filters(&[("instance-state-name", &[state.as_str()])]),
        )
        .await;
        assert_eq!(
            selected, expected,
            "case {case:?}: expected {expected:?}, got {selected:?}"
        );
    }
    // A state of the modelled enum no instance of the store is in selects nothing rather
    // than everything.
    let unused = ["pending", "running", "shutting-down", "terminated", "stopping", "stopped"]
        .into_iter()
        .find(|state| !states.contains(*state))
        .unwrap_or_default();
    let selected = described_ids(
        "a state no instance is in",
        &fixture.router,
        &filters(&[("instance-state-name", &[unused])]),
    )
    .await;
    assert!(
        selected.is_empty(),
        "case \"a state no instance is in\": instance-state-name={unused} must select \
         nothing, got {selected:?}"
    );
}

/// `MaxResults` caps the instances on a page and the token walks the whole selected set,
/// exactly once each, in id order.
#[tokio::test]
async fn max_results_caps_a_page_and_the_token_walks_the_whole_set() {
    let fixture = fixture().await;
    let every = fixture.instance_ids(&LAUNCHES.map(|launch| launch.0));

    let whole = described_ids("no page size", &fixture.router, &[]).await;
    assert_eq!(
        whole, every,
        "case \"no page size\": a request naming none reports every instance"
    );

    // Rows: the smallest page, sizes that divide and that do not divide the set, a size
    // larger than the set, and the largest page the service serves.
    for max_results in [1_u32, 2, 3, 5, 6, 1000] {
        let case = format!("MaxResults={max_results}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut params = vec![param("MaxResults", &max_results.to_string())];
            if let Some(token) = token.as_deref() {
                params.push(param("NextToken", token));
            }
            let (status, body) = describe(&fixture.router, REGION, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            pages += 1;
            let page: Vec<String> = rows(&case, &body)
                .into_iter()
                .map(|row| row.instance_id)
                .collect();
            assert!(
                page.len() <= max_results as usize,
                "case {case:?}: page {pages} reported {} instances, more than the \
                 {max_results} asked for: {page:?}",
                page.len()
            );
            walked.extend(page);
            let remaining = every.len().saturating_sub(walked.len());
            let next = super::ec2_query::member(&body, "nextToken");
            assert_eq!(
                next.is_some(),
                remaining > 0,
                "case {case:?}: page {pages} must carry a token exactly while instances \
                 remain ({remaining} left), got {next:?}"
            );
            match next {
                Some(next) => token = Some(next),
                None => break,
            }
            assert!(
                pages <= every.len() + 1,
                "case {case:?}: the walk must end, got {pages} pages for {} instances",
                every.len()
            );
        }
        assert_eq!(
            walked, every,
            "case {case:?}: the walk must cover every instance exactly once, in id order"
        );
        let expected_pages = every.len().div_ceil(max_results.max(1) as usize);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} page(s), got {pages}"
        );
    }
}

/// The report is scoped to the Region the request was signed for: EC2 is reached through a
/// regional endpoint, so an instance of another Region is no candidate and its id resolves
/// to nothing.
#[tokio::test]
async fn the_report_is_scoped_to_the_region_the_request_was_signed_for() {
    let fixture = fixture().await;
    let elsewhere = {
        let body = super::ec2_query::call(
            &fixture.router,
            OTHER_REGION,
            "RunInstances",
            &[
                param("ImageId", &fixture.image_id("own-available")),
                param("MinCount", "1"),
                param("MaxCount", "1"),
            ],
        )
        .await;
        assert_eq!(
            body.0, 200,
            "a launch in {OTHER_REGION} must succeed, got {} with body {}",
            body.0, body.1
        );
        super::ec2_query::elements(&body.1, "instanceId")
            .first()
            .cloned()
            .unwrap_or_default()
    };

    // Rows: each Region reports its own instances and none of the other's.
    for (region, expected) in [
        (REGION, fixture.instance_ids(&LAUNCHES.map(|launch| launch.0))),
        (OTHER_REGION, vec![elsewhere.clone()]),
    ] {
        let case = format!("every instance of {region}");
        let (status, body) = describe(&fixture.router, region, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let reported: Vec<String> = rows(&case, &body)
            .into_iter()
            .map(|row| row.instance_id)
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }

    // And an id of the other Region names no instance here.
    let case = "an instance of another Region, by id";
    let (status, body) = describe(
        &fixture.router,
        REGION,
        &[param("InstanceId.1", &elsewhere)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidInstanceID.NotFound",
        "case {case:?}: expected InvalidInstanceID.NotFound, got {body}"
    );
}

/// What the operation REFUSES: an id it cannot resolve, a filter it does not document, a
/// token it did not mint, and a `DryRun` of a request that would have succeeded.
#[tokio::test]
async fn refuses_an_unresolvable_id_an_unknown_filter_and_a_token_it_did_not_mint() {
    let fixture = fixture().await;

    // Rows: a well-formed id the account holds no instance under, a long-form id this
    // service never issued, and a value that is no instance id at all. The not-found
    // sentence is this operation's own, which ends with a full stop.
    for (case, instance_id, expected, message) in [
        (
            "an absent short-form id",
            "i-0abcdef1",
            "InvalidInstanceID.NotFound",
            "The instance ID 'i-0abcdef1' does not exist.",
        ),
        (
            "a long-form id this service never issued",
            "i-1234567890abcdef0",
            "InvalidInstanceID.Malformed",
            "Invalid id: \"i-1234567890abcdef0\"",
        ),
        (
            "a value that is no instance id",
            "instance-one",
            "InvalidInstanceID.Malformed",
            "Invalid id: \"instance-one\"",
        ),
    ] {
        let (status, body) = describe(
            &fixture.router,
            REGION,
            &[param("InstanceId.1", instance_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            message,
            "case {case:?}: expected the message {message:?}, got {}",
            unescaped(&body)
        );
    }

    // A filter name the operation does not document is refused rather than ignored, so it
    // can never read as "no results".
    for (case, name) in [
        ("a filter of another describe", "image-id"),
        ("a misspelled filter", "instance-state"),
        ("a tag filter with no key", "tag:"),
    ] {
        let (status, body) =
            describe(&fixture.router, REGION, &filters(&[(name, &["anything"])])).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The filter '{name}' is invalid"),
            "case {case:?}: expected the filter refusal naming {name}, got {}",
            unescaped(&body)
        );
    }

    // A token this operation did not mint is refused rather than read as an id.
    let case = "a token this operation did not mint";
    let (status, body) =
        describe(&fixture.router, REGION, &[param("NextToken", "not-a-token")]).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidPaginationToken",
        "case {case:?}: expected InvalidPaginationToken, got {body}"
    );

    // `DryRun` on a request that would have succeeded.
    let case = "DryRun";
    let (status, body) = describe(&fixture.router, REGION, &[param("DryRun", "true")]).await;
    assert_eq!(
        status, 412,
        "case {case:?}: expected HTTP 412, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );
}

/// The report is a READ: describing twice reports the same instances and the same AMI
/// metadata, and leaves what `DescribeInstances` reports for them alone.
#[tokio::test]
async fn describing_twice_reports_the_same_thing() {
    let fixture = fixture().await;
    /// The parts of a row no clock can move.
    type Stable = (String, String, Option<ImageRow>);
    let stable = |reported: Vec<Row>| -> Vec<Stable> {
        reported
            .into_iter()
            .map(|row| (row.instance_id, row.instance_type, row.image))
            .collect()
    };

    let first = stable(described("the first read", &fixture.router, &[]).await);
    let second = stable(described("the second read", &fixture.router, &[]).await);
    assert_eq!(
        first, second,
        "case \"two reads\": a read must not change what a later caller sees"
    );

    for (key, _, instance_type, _) in LAUNCHES {
        let id = fixture.instance_id(key);
        let body = ok(
            key,
            &fixture.router,
            "DescribeInstances",
            &[param("InstanceId.1", &id)],
        )
        .await;
        let reported = super::ec2_query::elements(&body, "instanceType");
        assert_eq!(
            reported,
            vec![instance_type.to_string()],
            "case {key:?}: DescribeInstances must still report {id} as {instance_type}, \
             got {reported:?}"
        );
    }
}
