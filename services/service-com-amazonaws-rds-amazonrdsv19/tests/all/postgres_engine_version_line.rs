// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the BUILDS the RDS for PostgreSQL families publish, and for what a
//! request may pin a DB instance to.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answered with.
//!
//! The rules under test:
//!
//! * an RDS for PostgreSQL family is LISTED in every community minor release of its line,
//!   from the oldest build the catalogue publishes for it up to the build it has moved to —
//!   the community numbers a line's releases consecutively — and every build it reports as
//!   AVAILABLE is one a create may pin; a build the family never published is refused with
//!   `Cannot find version <version> for <engine>`;
//! * an engine version change is an UPGRADE: a request naming a build that comes before the
//!   one the instance runs is refused `InvalidParameterCombination`, and a later build is
//!   QUEUED as a pending value while the instance keeps reporting the version it is running —
//!   which a request naming the live version withdraws again.
//!
//! The tables below span what those rules generalise over: every `postgres` family the
//! report lists, builds drawn from the middle of three different families (none of them a
//! family's floor or its newest build, so a row cannot pass on a sample of two), both a minor
//! and a major version as the pinned value, a build past the newest the family publishes and
//! one between two published lines, and the version change in both directions.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::db_engine_versions::all_versions_of;
use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// The engine whose published builds are the community releases of each family's line.
const ENGINE: &str = "postgres";

/// The family whose builds are Amazon RDS EXTENDED SUPPORT builds (`11.22-rds.20241121`)
/// alone rather than a community line: it is long past community end of life, so its rows are
/// the builds this service still offers and not a consecutive numbering.
const EXTENDED_SUPPORT_FAMILY: &str = "11";

/// The minors the report LISTS per `MajorEngineVersion` family of [`ENGINE`], ascending,
/// taken off every version it reports — whatever status each carries, since the line is what
/// is published and the status is which of its builds the family is still offered in.
async fn published_minors(router: &MultiServiceRouter) -> BTreeMap<String, Vec<u64>> {
    listed_minors(router, None).await
}

/// The same, narrowed to the versions of one `Status` where one is named.
async fn listed_minors(
    router: &MultiServiceRouter,
    status: Option<&str>,
) -> BTreeMap<String, Vec<u64>> {
    let mut lines: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for version in all_versions_of(router, ENGINE).await {
        if version.major_engine_version == EXTENDED_SUPPORT_FAMILY {
            continue;
        }
        if status.is_some_and(|wanted| version.status != wanted) {
            continue;
        }
        let Some(minor) = minor_in_family(&version.engine_version, &version.major_engine_version)
        else {
            continue;
        };
        lines
            .entry(version.major_engine_version.clone())
            .or_default()
            .push(minor);
    }
    for minors in lines.values_mut() {
        minors.sort_unstable();
        // An Amazon RDS Extended Support build (`13.23-rds.20260224`) continues the
        // community release whose number it carries, so it is the same minor of the line
        // rather than another one.
        minors.dedup();
    }
    lines
}

/// The MINOR of `version` within the family `family`: the number right after the family's own
/// prefix, or `None` for a version of another family or a build whose minor is not a number.
fn minor_in_family(version: &str, family: &str) -> Option<u64> {
    let rest = version.strip_prefix(family)?.strip_prefix('.')?;
    rest.split(['.', '-']).next()?.parse().ok()
}

#[tokio::test]
async fn every_published_family_lists_the_whole_community_line_it_publishes() {
    let router = emulator();
    let lines = published_minors(&router).await;
    assert!(
        lines.len() >= 2,
        "the catalogue must publish several families of {ENGINE}, got {lines:?}"
    );
    let offered = listed_minors(&router, Some("available")).await;

    for (family, minors) in &lines {
        let case = format!("{ENGINE} {family}");
        assert!(
            !minors.is_empty(),
            "case {case:?}: a published family must list builds, got {minors:?}"
        );
        let floor = minors[0];
        let newest = *minors.last().expect("checked just above");
        // Consecutive from the oldest build the catalogue publishes to the newest: the
        // community numbers a line's releases one after another, so a family that listed a
        // sample of them would leave a gap a client could not place a release in.
        let expected: Vec<u64> = (floor..=newest).collect();
        assert_eq!(
            *minors, expected,
            "case {case:?}: expected every community minor from {floor} to {newest}, got \
             {minors:?}"
        );
        // And the family is OFFERED in builds of that line, the newest among them: a family
        // whose rows were all deprecated could not be created with at all.
        let available = offered
            .get(family)
            .unwrap_or_else(|| panic!("case {case:?}: the family must offer builds"));
        assert!(
            available.contains(&newest),
            "case {case:?}: the build the family has moved to must be offered, got \
             {available:?}"
        );
    }
}

/// One row of the create table: the version a create pins, and the version the instance must
/// report running.
struct PinnedVersion {
    case: &'static str,
    identifier: &'static str,
    /// The `EngineVersion` the create names.
    requested: &'static str,
    /// The version the instance must report, which is the one the request named for a minor
    /// and the family's own default for a major.
    provisioned: Option<&'static str>,
}

/// The builds the creates below pin.
///
/// Each is a build from the MIDDLE of its family's line — neither the oldest the family is
/// offered from nor the one it has moved to — so a catalogue listing a sample of two per
/// family could not serve any of them. The last row names only the major version, which
/// resolves to that family's own default build.
const PINNED: &[PinnedVersion] = &[
    PinnedVersion {
        case: "a build of the 14 line",
        identifier: "emu-pg-line-14",
        requested: "14.20",
        provisioned: Some("14.20"),
    },
    PinnedVersion {
        case: "a build of the 13 line",
        identifier: "emu-pg-line-13",
        requested: "13.19",
        provisioned: Some("13.19"),
    },
    PinnedVersion {
        case: "a build of the 16 line",
        identifier: "emu-pg-line-16",
        requested: "16.10",
        provisioned: Some("16.10"),
    },
    PinnedVersion {
        case: "a major version, which resolves to the family's own build",
        identifier: "emu-pg-line-major",
        requested: "15",
        provisioned: None,
    },
];

/// `CreateDBInstance` pinned to `version`, with the members the recorded flows carry.
async fn create_pinned(
    router: &MultiServiceRouter,
    identifier: &str,
    version: &str,
) -> (u16, String) {
    call(
        router,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier),
            ("Engine", ENGINE),
            ("EngineVersion", version),
            ("DBInstanceClass", "db.t3.micro"),
            ("AllocatedStorage", "20"),
            ("StorageType", "gp2"),
            ("MasterUsername", "emuadmin"),
            ("MasterUserPassword", "emu-password-1"),
        ],
    )
    .await
}

#[tokio::test]
async fn a_create_pinned_to_any_published_build_of_a_line_is_served() {
    let router = emulator();
    let lines = published_minors(&router).await;

    for row in PINNED {
        let created = expect_ok(row.case, create_pinned(&router, row.identifier, row.requested).await);
        let reported = text(&created, "EngineVersion");
        match row.provisioned {
            Some(expected) => assert_eq!(
                reported, expected,
                "case {:?}: expected the build the request pinned, got {reported:?}",
                row.case
            ),
            None => {
                // A major version resolves to that family's own build, which is one of the
                // builds the family publishes.
                let family_minors = lines
                    .get(row.requested)
                    .unwrap_or_else(|| panic!("case {:?}: the family is published", row.case));
                let minor = minor_in_family(&reported, row.requested).unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected a build of the {} line, got {reported:?}",
                        row.case, row.requested
                    )
                });
                assert!(
                    family_minors.contains(&minor),
                    "case {:?}: expected a published build of the line, got {reported:?}",
                    row.case
                );
            }
        }
        // And the report of the same catalogue lists it, so the create and the report cannot
        // disagree about which builds exist.
        let listed = expect_ok(
            row.case,
            call(
                &router,
                "DescribeDBEngineVersions",
                &[("Engine", ENGINE), ("EngineVersion", reported.as_str())],
            )
            .await,
        );
        assert_eq!(
            text(&listed, "EngineVersion"),
            reported,
            "case {:?}: the build an instance runs must be listed by the report, got {listed}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_build_the_line_never_published_is_refused() {
    let router = emulator();
    let lines = published_minors(&router).await;
    let fourteen = lines
        .get("14")
        .cloned()
        .expect("the 14 family is published");
    let past_the_newest = format!(
        "14.{}",
        fourteen.last().expect("the family lists builds") + 7
    );

    // (case, the version the create names)
    let rows: Vec<(&str, String)> = vec![
        ("a minor past the newest the family publishes", past_the_newest),
        ("a minor of a family this engine does not publish", "9.6.24".to_string()),
        ("a third component no community release carries", "14.20.1".to_string()),
        ("a major version this engine does not publish", "99".to_string()),
    ];

    for (case, version) in rows {
        let refused = expect_error(
            case,
            create_pinned(&router, "emu-pg-line-refused", &version).await,
            400,
            "InvalidParameterCombination",
        );
        assert_eq!(
            text(&refused, "Message"),
            format!("Cannot find version {version} for {ENGINE}"),
            "case {case:?}: expected the sentence naming the version and the engine, got \
             {refused}"
        );
    }
}

#[tokio::test]
async fn an_engine_version_change_is_an_upgrade_and_is_queued_until_it_is_applied() {
    let router = emulator();
    let identifier = "emu-pg-version-change";
    let created = expect_ok(
        "create the instance",
        create_pinned(&router, identifier, "14.20").await,
    );
    assert_eq!(
        text(&created, "EngineVersion"),
        "14.20",
        "the instance must be created on the build the request pinned, got {created}"
    );
    // A modification is accepted on an instance that is RUNNING, so the create's own
    // transition is waited out by polling the report — the CONDITION, not a fixed time.
    poll_until(
        "the created instance",
        "the instance reaches `available`",
        || read_instance_response(&router, identifier),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;

    // A build BEFORE the one the instance runs cannot be moved to, whichever line it belongs
    // to: the rows span an earlier major line and an earlier minor of the same line.
    for (case, earlier) in [
        ("an earlier major line", "13.23"),
        ("an earlier minor of the same line", "14.19"),
    ] {
        let refused = expect_error(
            case,
            call(
                &router,
                "ModifyDBInstance",
                &[
                    ("DBInstanceIdentifier", identifier),
                    ("EngineVersion", earlier),
                    ("AllowMajorVersionUpgrade", "true"),
                    ("ApplyImmediately", "true"),
                ],
            )
            .await,
            400,
            "InvalidParameterCombination",
        );
        assert_eq!(
            text(&refused, "Message"),
            format!("Cannot upgrade {ENGINE} from 14.20 to {earlier}"),
            "case {case:?}: expected the sentence naming both builds, got {refused}"
        );
    }
    // The refusal changed nothing: the instance still runs the build it was created on and
    // has no pending version.
    let described = read_instance(&router, identifier).await;
    assert_eq!(
        text(&described, "EngineVersion"),
        "14.20",
        "a refused change must leave the live version alone, got {described}"
    );
    assert_eq!(
        pending_version(&described),
        None,
        "a refused change must queue nothing, got {described}"
    );

    // A LATER build is queued as a pending value, and the instance keeps reporting the
    // version it is running until the change is applied.
    expect_ok(
        "queue a later major build",
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("EngineVersion", "16.10"),
                ("AllowMajorVersionUpgrade", "true"),
            ],
        )
        .await,
    );
    let queued = read_instance(&router, identifier).await;
    assert_eq!(
        pending_version(&queued),
        Some("16.10".to_string()),
        "the queued build must be reported as a pending value, got {queued}"
    );
    assert_eq!(
        text(&queued, "EngineVersion"),
        "14.20",
        "the live version must stay the one the instance runs, got {queued}"
    );

    // And naming the version the instance is RUNNING withdraws what the earlier request
    // queued: there is no change left to apply.
    expect_ok(
        "withdraw the queued build",
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("EngineVersion", "14.20"),
            ],
        )
        .await,
    );
    let withdrawn = read_instance(&router, identifier).await;
    assert_eq!(
        pending_version(&withdrawn),
        None,
        "naming the live version must withdraw the queued one, got {withdrawn}"
    );
    assert_eq!(
        text(&withdrawn, "EngineVersion"),
        "14.20",
        "the live version must stay the one the instance runs, got {withdrawn}"
    );
}

/// `DescribeDBInstances` for one instance.
async fn read_instance(router: &MultiServiceRouter, identifier: &str) -> String {
    expect_ok("read the instance", read_instance_response(router, identifier).await)
}

/// The same read as the HTTP status and the body, so a poll loop can call it again.
async fn read_instance_response(
    router: &MultiServiceRouter,
    identifier: &str,
) -> (u16, String) {
    call(
        router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await
}

/// The `EngineVersion` the instance's `PendingModifiedValues` carries, if any.
fn pending_version(body: &str) -> Option<String> {
    subtrees(body, "PendingModifiedValues")
        .into_iter()
        .find_map(|pending| optional_text(&pending, "EngineVersion"))
}
