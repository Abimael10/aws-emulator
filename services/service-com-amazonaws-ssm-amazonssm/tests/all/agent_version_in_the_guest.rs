// SPDX-License-Identifier: CC0-1.0

//! The SSM Agent BUILD a managed node reports, and the package database in its guest that has
//! to agree with it — driven end to end with Systems Manager, Amazon EC2 and IAM on one
//! engine.
//!
//! RULE under test: the build a node reports (`InstanceInformation$AgentVersion`) is the one
//! the AMI its instance was launched from BUNDLES — the agent release that was current when
//! Amazon last published that image, which is the instant its standard support ended for an
//! image whose `Image$DeprecationTime` has passed and the present for one still being
//! published — and `IsLatestVersion` is whether that build is the newest release the service
//! publishes. The same build is what the node's `AWS:InstanceInformation` inventory reports and
//! what the package database inside its guest answers for the agent's own package.
//!
//! Every call goes through the front door: `DescribeImages` and `RunInstances` over EC2's
//! `ec2Query` wire, the role and instance profile over IAM's `awsQuery` wire, and
//! `DescribeInstanceInformation`, `GetInventory`, `SendCommand` and `GetCommandInvocation` over
//! Systems Manager's `awsJson1_1` wire.
//!
//! The rows differ along what the rule generalises over: whether the image carries a
//! deprecation instant at all, whether that instant has already passed, the platform of the
//! image and the architecture of the machine — plus, for the release series itself, the span
//! between an image's last publication and the read, on each side of a release boundary.
//!
//! Nothing here waits a fixed time: the registration span and the invocation's move to a
//! terminal status are stored due instants, so a case moves the stored instant or LOOKS AGAIN
//! until the condition holds.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_ssm_amazonssm::state::agent_releases::{
    is_latest, last_published_at, version_current_at, AGENT_RELEASES, AGENT_RELEASE_INTERVAL,
    LATEST_AGENT_VERSION,
};
use service_com_amazonaws_ssm_amazonssm::state::guest_packages::{
    AGENT_PACKAGE, AGENT_PACKAGE_RELEASE, RPM_ARM64,
};

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, REGION, SSM_CORE_POLICY_ARN,
};
use super::run_command_in_the_guest::{finish_due, finished, send};

/// The inventory type every managed node has by being registered, whose `AgentVersion`
/// attribute is the node's own reported build.
const INVENTORY_TYPE: &str = "AWS:InstanceInformation";

/// The status a shell answers for a command it could not run, which is what the guest answers
/// for a package query this service holds no answer for (POSIX, and
/// `service_common::posix_shell_session::Statuses::could_not_run`).
const COULD_NOT_RUN: i64 = 127;

/// One published image a row launches from, with the two instants that decide which agent
/// build it bundles.
#[derive(Debug, Clone)]
struct PublishedImage {
    /// The AMI id `DescribeImages` reported.
    id: String,
    /// Its `CreationDate`, as the instant the rendering names.
    created_at: Option<SystemTime>,
    /// Its `DeprecationTime`, where it carries one.
    deprecated_at: Option<SystemTime>,
}

/// One row of the image table.
struct ImageCase {
    case: &'static str,
    /// The name pattern a client resolves the image by.
    pattern: &'static str,
    /// Whether the image must carry a deprecation instant that has already passed — which is
    /// what makes its bundled build an earlier release.
    support_ended: bool,
    /// The instance type to launch, which is what decides the machine's architecture.
    instance_type: &'static str,
    /// The architecture `rpm` reports for the package installed in that image's guest.
    package_architecture: &'static str,
    /// Whether a shell script can be sent to the node: a Linux document names no step a
    /// Windows node runs, so the guest of such a node is read by the rows that can.
    runs_shell: bool,
}

/// The 64-bit x86 instance type the rows launch unless they are about another architecture.
const X86_INSTANCE_TYPE: &str = "t3.micro";

/// The 64-bit Arm instance type the Arm row launches, whose family carries AWS's own
/// `g` capability letter.
const ARM_INSTANCE_TYPE: &str = "t4g.micro";

/// The rows: the publication state of the image a node's instance was launched from.
fn image_rows() -> Vec<ImageCase> {
    vec![
        // A build of a family still in standard support: it carries a `DeprecationTime` that
        // has NOT passed, so Amazon is still publishing builds of it.
        ImageCase {
            case: "a build of a family still in standard support",
            pattern: "al2023-ami-2023*x86_64",
            support_ended: false,
            instance_type: X86_INSTANCE_TYPE,
            package_architecture: "x86_64",
            runs_shell: true,
        },
        // A build carrying no deprecation instant at all — nothing says it stopped being
        // published.
        ImageCase {
            case: "a build carrying no deprecation date at all",
            pattern: "amzn2-ami-hvm-2.0.*-x86_64-gp2",
            support_ended: false,
            instance_type: X86_INSTANCE_TYPE,
            package_architecture: "x86_64",
            runs_shell: true,
        },
        // A build of a family whose standard support has ENDED: no build of it has been
        // published since, so its guest holds the release that was current then.
        ImageCase {
            case: "a build whose family's standard support has ended",
            pattern: "amzn2-ami-hvm-2.0.*-x86_64-gp2",
            support_ended: true,
            instance_type: X86_INSTANCE_TYPE,
            package_architecture: "x86_64",
            runs_shell: true,
        },
        // The platform of the image is not what decides the build.
        ImageCase {
            case: "a Windows build still being published",
            pattern: "Windows_Server-2022-English-Full-Base*",
            support_ended: false,
            instance_type: X86_INSTANCE_TYPE,
            package_architecture: "x86_64",
            runs_shell: false,
        },
        // ... and neither is the machine's architecture, which only names the package build.
        ImageCase {
            case: "a 64-bit Arm build still in standard support",
            pattern: "al2023-ami-2023*arm64",
            support_ended: false,
            instance_type: ARM_INSTANCE_TYPE,
            package_architecture: RPM_ARM64,
            runs_shell: true,
        },
    ]
}

/// The instant an ISO 8601 rendering of the image catalog names, read the way the model's
/// timestamp form reads one.
fn instant(rendering: &str) -> Option<SystemTime> {
    let at = ::aws_smithy_types::DateTime::from_str(
        rendering,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .ok()?;
    SystemTime::try_from(at).ok()
}

/// The published image matching `pattern` whose support has — or has not — ended, resolved the
/// way a client resolves an AMI before a launch.
///
/// A deprecated image is reported only to a request that asks for one ("`IncludeDeprecated`"),
/// which is also what tells the two rows of one name pattern apart.
async fn published_image(emulator: &Emulator, row: &ImageCase) -> PublishedImage {
    let body = emulator
        .ec2(
            row.case,
            "DescribeImages",
            &[
                ("Owner.1", "amazon"),
                ("Filter.1.Name", "name"),
                ("Filter.1.Value.1", row.pattern),
                ("Filter.2.Name", "state"),
                ("Filter.2.Value.1", "available"),
                (
                    "IncludeDeprecated",
                    if row.support_ended { "true" } else { "false" },
                ),
            ],
        )
        .await;
    let document = ::roxmltree::Document::parse(&body)
        .unwrap_or_else(|_| panic!("case {:?}: DescribeImages answers XML: {body}", row.case));
    let now = SystemTime::now();
    for item in document.descendants().filter(|node| node.has_tag_name("item")) {
        let text = |name: &str| {
            item.children()
                .find(|child| child.has_tag_name(name))
                .and_then(|child| child.text())
                .map(str::to_string)
        };
        let Some(id) = text("imageId") else {
            continue;
        };
        let deprecated_at = text("deprecationTime").as_deref().and_then(instant);
        if deprecated_at.is_some_and(|at| at < now) != row.support_ended {
            continue;
        }
        return PublishedImage {
            id,
            created_at: text("creationDate").as_deref().and_then(instant),
            deprecated_at,
        };
    }
    panic!(
        "case {:?}: expected a published image matching {:?} whose support had{} ended, got \
         {body}",
        row.case,
        row.pattern,
        if row.support_ended { "" } else { " not" }
    )
}

/// Launch an instance from `image` whose profile lets its agent register, make the
/// registration due, and answer the `InstanceInformation` this service reports for it.
async fn registered_node(
    emulator: &Emulator,
    case: &str,
    role: &str,
    image: &PublishedImage,
    instance_type: &str,
) -> (String, Value) {
    let profile = emulator
        .profile(case, role, &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let body = emulator
        .ec2(
            case,
            "RunInstances",
            &[
                ("ImageId", image.id.as_str()),
                ("InstanceType", instance_type),
                ("MinCount", "1"),
                ("MaxCount", "1"),
                ("IamInstanceProfile.Name", profile.as_str()),
            ],
        )
        .await;
    let instance = Emulator::answered(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must mint an instance: {body}"));
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    let node = reported.get(&instance).cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: the launched instance must become a managed node, got {:?}",
            reported.keys().collect::<Vec<_>>()
        )
    });
    (instance, node)
}

/// The `AWS:InstanceInformation` content `GetInventory` reports for `instance`.
async fn inventory(emulator: &Emulator, case: &str, instance: &str) -> Value {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "GetInventory",
            &json!({
                "Filters": [{
                    "Key": format!("{INVENTORY_TYPE}.InstanceId"),
                    "Values": [instance],
                }],
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetInventory must be served, got {status}: {answered}"
    );
    answered["Entities"]
        .as_array()
        .and_then(|entities| entities.first())
        .and_then(|entity| entity["Data"][INVENTORY_TYPE]["Content"].as_array())
        .and_then(|content| content.first().cloned())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an inventory item for {instance}: {answered}")
        })
}

/// What the guest of `instance` wrote for one script, and the status its shell ended on.
async fn in_guest(emulator: &Emulator, case: &str, instance: &str, script: &str) -> Value {
    let command_id = send(emulator, case, instance, &[script]).await;
    finish_due(emulator, &command_id);
    finished(emulator, case, &command_id, instance).await
}

/// The build a node reports is the one its image bundles, and `IsLatestVersion` is whether that
/// build is the newest release the service publishes.
#[tokio::test]
async fn the_build_a_node_reports_is_the_one_its_image_bundles() {
    let emulator = Emulator::new();
    for (at, row) in image_rows().iter().enumerate() {
        let image = published_image(&emulator, row).await;
        let (instance, node) = registered_node(
            &emulator,
            row.case,
            &format!("agent-role-{at}"),
            &image,
            row.instance_type,
        )
        .await;
        let reported = node["AgentVersion"].as_str().unwrap_or_default().to_string();
        assert!(
            AGENT_RELEASES.contains(&reported.as_str()),
            "case {:?}: expected one of the published agent builds {AGENT_RELEASES:?}, got \
             {reported:?}",
            row.case
        );

        // The build is the release that was current when the image was last published: the
        // present for an image still being published, and the instant its support ended for
        // one whose deprecation has passed. The expectation is measured from the instants the
        // image reported on the wire.
        let now = SystemTime::now();
        let published_at = match image.deprecated_at.filter(|at| *at < now) {
            Some(ended_at) => ended_at.max(image.created_at.unwrap_or(ended_at)),
            None => now,
        };
        let releases_since = usize::try_from(
            now.duration_since(published_at)
                .unwrap_or_default()
                .as_secs()
                / AGENT_RELEASE_INTERVAL.as_secs(),
        )
        .unwrap_or(usize::MAX);
        let expected = AGENT_RELEASES[releases_since.min(AGENT_RELEASES.len() - 1)];
        assert_eq!(
            reported, expected,
            "case {:?}: expected the build current when the image was last published, got \
             {reported:?}",
            row.case
        );
        assert_eq!(
            node["IsLatestVersion"].as_bool(),
            Some(reported == LATEST_AGENT_VERSION),
            "case {:?}: IsLatestVersion must say whether {reported:?} is the current release \
             {LATEST_AGENT_VERSION:?}, got {node}",
            row.case
        );
        // A build frozen when a family's support ended is BEHIND the current release, which is
        // the divergence the member exists to report.
        if row.support_ended {
            assert!(
                !is_latest(&reported) && releases_since > 0,
                "case {:?}: an image that stopped being published before the current release \
                 must bundle an earlier build, got {reported:?}",
                row.case
            );
        }

        // One build, three readers: the describe, the node's own inventory, and the package
        // database in its guest.
        let item = inventory(&emulator, row.case, &instance).await;
        assert_eq!(
            item["AgentVersion"].as_str(),
            Some(reported.as_str()),
            "case {:?}: the inventory must report the build the describe reports, got {item}",
            row.case
        );
        if !row.runs_shell {
            continue;
        }
        let answered = in_guest(
            &emulator,
            row.case,
            &instance,
            &format!("rpm -q {AGENT_PACKAGE}"),
        )
        .await;
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some(
                format!(
                    "{AGENT_PACKAGE}-{reported}-{AGENT_PACKAGE_RELEASE}.{}\n",
                    row.package_architecture
                )
                .as_str()
            ),
            "case {:?}: the guest's package database must report the build the node reports, \
             got {answered}",
            row.case
        );
    }
}

/// The package query in the guest answers for the agent package the node holds, in the form the
/// invocation asked for, and for no other package.
#[tokio::test]
async fn the_guest_answers_the_agent_package_query_and_no_other() {
    let emulator = Emulator::new();
    let row = ImageCase {
        case: "the package query table",
        pattern: "al2023-ami-2023*x86_64",
        support_ended: false,
        instance_type: X86_INSTANCE_TYPE,
        package_architecture: "x86_64",
        runs_shell: true,
    };
    let image = published_image(&emulator, &row).await;
    let (instance, node) = registered_node(
        &emulator,
        row.case,
        "package-role",
        &image,
        row.instance_type,
    )
    .await;
    let version = node["AgentVersion"].as_str().unwrap_or_default().to_string();
    let nvra = format!("{AGENT_PACKAGE}-{version}-{AGENT_PACKAGE_RELEASE}.x86_64");

    // (case, the script, what the guest writes, the status its shell ends on)
    let rows: Vec<(&str, String, String, i64)> = vec![
        (
            "the package named exactly",
            format!("rpm -q {AGENT_PACKAGE}"),
            format!("{nvra}\n"),
            0,
        ),
        (
            "the long spelling of the query",
            format!("rpm --query {AGENT_PACKAGE}"),
            format!("{nvra}\n"),
            0,
        ),
        (
            "a query naming only the version tag",
            format!("rpm -q --queryformat '%{{VERSION}}' {AGENT_PACKAGE}"),
            format!("{version}\n"),
            0,
        ),
        (
            "the short spelling of that option, inline",
            format!("rpm -q --qf='%{{NAME}} %{{VERSION}}' {AGENT_PACKAGE}"),
            format!("{AGENT_PACKAGE} {version}\n"),
            0,
        ),
        (
            "a format naming every tag of the package",
            format!(
                "rpm -q --qf '%{{NVRA}}|%{{RELEASE}}|%{{ARCH}}|%{{EVR}}' {AGENT_PACKAGE}"
            ),
            format!(
                "{nvra}|{AGENT_PACKAGE_RELEASE}|x86_64|{version}-{AGENT_PACKAGE_RELEASE}\n"
            ),
            0,
        ),
        (
            "a glob the agent's package name satisfies",
            format!("rpm -qa '{AGENT_PACKAGE}*'"),
            format!("{nvra}\n"),
            0,
        ),
        (
            "a glob with the wildcard in the middle",
            "rpm -qa 'amazon*agent'".to_string(),
            format!("{nvra}\n"),
            0,
        ),
        // A package whose version this service does not hold is answered by nothing here, so
        // the guest reports what a shell reports for a program it could not run rather than
        // inventing a package database entry.
        (
            "a package this service holds no version for",
            "rpm -q httpd".to_string(),
            String::new(),
            COULD_NOT_RUN,
        ),
        (
            "a glob the agent's package name does not satisfy",
            "rpm -qa 'postgres*'".to_string(),
            String::new(),
            COULD_NOT_RUN,
        ),
        // An invocation that is not a query at all is a form this client performs nothing of.
        (
            "an invocation that installs rather than queries",
            format!("rpm -i {AGENT_PACKAGE}"),
            String::new(),
            COULD_NOT_RUN,
        ),
    ];

    for (case, script, output, status) in rows {
        let answered = in_guest(&emulator, case, &instance, &script).await;
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some(output.as_str()),
            "case {case:?}: expected the guest to write {output:?} for {script:?}, got {answered}"
        );
        assert_eq!(
            answered["ResponseCode"].as_i64(),
            Some(status),
            "case {case:?}: expected exit status {status} for {script:?}, got {answered}"
        );
    }
}

/// The release series is walked back ONE build per published interval, and an instant older
/// than the whole series answers its earliest build.
#[test]
fn the_build_current_at_an_instant_is_one_per_interval_back() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let interval = AGENT_RELEASE_INTERVAL;
    // (case, how long before the read the image was last published, the build index expected)
    let rows: Vec<(&str, Duration, usize)> = vec![
        ("published at the instant of the read", Duration::ZERO, 0),
        ("one second before the first release", interval - Duration::from_secs(1), 0),
        ("exactly one release ago", interval, 1),
        ("one second after that release", interval + Duration::from_secs(1), 1),
        ("two releases ago", 2 * interval, 2),
        ("just before the third release", 3 * interval - Duration::from_secs(1), 2),
        ("older than every build the series holds", 100 * interval, AGENT_RELEASES.len() - 1),
    ];
    for (case, ago, index) in rows {
        let answered = version_current_at(now - ago, now);
        assert_eq!(
            answered, AGENT_RELEASES[index],
            "case {case:?}: expected build {index} of {AGENT_RELEASES:?}, got {answered:?}"
        );
        assert_eq!(
            is_latest(answered),
            index == 0,
            "case {case:?}: only the first build of the series is the current release, got \
             {answered:?}"
        );
    }
    // The series is published newest first, so every entry after the first is an earlier build
    // than the one before it.
    for pair in AGENT_RELEASES.windows(2) {
        let segments = |version: &str| {
            version
                .split('.')
                .map(|segment| segment.parse::<u64>().unwrap_or_default())
                .collect::<Vec<u64>>()
        };
        assert!(
            segments(pair[0]) > segments(pair[1]),
            "case \"the series order\": {:?} must be a later build than {:?}",
            pair[0],
            pair[1]
        );
    }
}

/// An image was last published when its standard support ended, and is still being published
/// where that instant has not passed.
#[test]
fn an_image_is_last_published_when_its_support_ends() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let rendering = |at: SystemTime| {
        ::aws_smithy_types::DateTime::from(at)
            .fmt(::aws_smithy_types::date_time::Format::DateTime)
            .expect("a renderable instant")
    };
    let year = Duration::from_secs(365 * 24 * 60 * 60);
    let created = now - 2 * year;

    // (case, the CreationDate, the DeprecationTime, the instant it was last published at)
    let rows: Vec<(&str, Option<SystemTime>, Option<SystemTime>, SystemTime)> = vec![
        ("an image carrying no deprecation instant", Some(created), None, now),
        (
            "an image whose support has not ended yet",
            Some(created),
            Some(now + year),
            now,
        ),
        (
            "an image whose support ended a year ago",
            Some(created),
            Some(now - year),
            now - year,
        ),
        (
            "an image deprecated at the instant of the read",
            Some(created),
            Some(now),
            now,
        ),
        // An image is not published before it exists, however its deprecation instant was set.
        (
            "an image deprecated before it was created",
            Some(created),
            Some(created - year),
            created,
        ),
        // The instants this emulator holds nothing readable for leave the image still being
        // published: nothing says it stopped.
        ("an image whose instants are not held", None, None, now),
    ];
    for (case, created_at, deprecated_at, expected) in rows {
        let answered = last_published_at(
            created_at.map(rendering).as_deref(),
            deprecated_at.map(rendering).as_deref(),
            now,
        );
        assert_eq!(
            answered, expected,
            "case {case:?}: expected the image to have been last published at {expected:?}, \
             got {answered:?}"
        );
    }

    // A rendering that is no instant at all is read as no instant, rather than as the epoch.
    assert_eq!(
        last_published_at(Some("not-a-date"), Some("neither-is-this"), now),
        now,
        "case \"renderings that are no instants\": an image whose instants cannot be read is \
         still being published"
    );
}
