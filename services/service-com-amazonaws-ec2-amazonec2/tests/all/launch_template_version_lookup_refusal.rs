// SPDX-License-Identifier: CC0-1.0

//! The sentence EC2 refuses a launch-template VERSION it cannot resolve with, driven over the
//! wire the service speaks (`ec2Query`) through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! RULE: a request that RESOLVES a launch-template version against a template it named — a
//! read of that version, or a launch from it — is refused `InvalidLaunchTemplateId.VersionNotFound`
//! with EC2's own sentence for a version the template does not hold: fixed text around two
//! values taken from the request, the version it named and the id of the template it named it
//! on, closed by a period. The error code alone does not say which half of the reference is
//! wrong; the sentence does, and a client that classifies the failure reads it.
//!
//! The rows span what the rule generalises over: how many versions the template holds, how far
//! past them the request reaches (one past the last minted, far past it, and below the first a
//! template can carry), which half of the reference names the template (its id or its name),
//! which operation resolves the version (a read of it and a launch from it), and the versions
//! the same template DOES resolve — the numbers it minted and the two words that float on the
//! version in force, which must be served rather than refused.
//!
//! The `Versions` list reaches the wire under its xmlName, `LaunchTemplateVersion.N`.

use service_com_amazonaws_ec2_amazonec2::state as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message};

/// The Region every call here is signed for: a launch template carries no Region member, so
/// the Region only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// An AMI id of the published form, so a version the rows create carries one launch parameter
/// and the versions can be told apart.
const FIRST_IMAGE: &str = "ami-00000000000000001";
const SECOND_IMAGE: &str = "ami-00000000000000002";

/// A `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// The sentence EC2 states for a version the named template does not hold.
fn expected_sentence(version: &str, launch_template_id: &str) -> String {
    format!(
        "Could not find the specified version {version} for the launch template with ID \
         {launch_template_id}."
    )
}

/// A launch template of `versions` versions, each carrying its own `ImageId`, created over
/// EC2's own wire; answers the id it was minted under.
async fn template_with_versions(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    images: &[&str],
) -> String {
    let (first, rest) = images.split_first().expect("a template holds one version");
    let (status, body) = api(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", first),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the template must be created, got {status}: {body}"
    );
    let launch_template_id =
        element(&body, "launchTemplateId").unwrap_or_else(|| panic!("case {case:?}: {body}"));
    for image in rest {
        let (status, body) = api(
            router,
            "CreateLaunchTemplateVersion",
            &[
                p("LaunchTemplateId", &launch_template_id),
                p("LaunchTemplateData.ImageId", image),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the extra version must be created, got {status}: {body}"
        );
    }
    launch_template_id
}

/// One row of the absent-version table: the versions the template holds, the version the
/// request names, and whether it names the template by its id.
struct AbsentVersionRow {
    case: &'static str,
    images: &'static [&'static str],
    version: &'static str,
    by_id: bool,
}

/// RULE: a read of a launch-template version the template does not hold is refused
/// `InvalidLaunchTemplateId.VersionNotFound` with EC2's sentence naming that version and the
/// template's id.
#[tokio::test]
async fn a_read_of_an_absent_launch_template_version_states_the_version_and_the_template() {
    let rows: &[AbsentVersionRow] = &[
        AbsentVersionRow {
            case: "one past the only version a new template holds",
            images: &[FIRST_IMAGE],
            version: "2",
            by_id: true,
        },
        AbsentVersionRow {
            case: "one past the last version of a template that holds two",
            images: &[FIRST_IMAGE, SECOND_IMAGE],
            version: "3",
            by_id: true,
        },
        AbsentVersionRow {
            case: "far past the last version",
            images: &[FIRST_IMAGE, SECOND_IMAGE],
            version: "99",
            by_id: true,
        },
        AbsentVersionRow {
            case: "below the first version a template can carry",
            images: &[FIRST_IMAGE],
            version: "0",
            by_id: true,
        },
        AbsentVersionRow {
            case: "a template named by its NAME rather than its id",
            images: &[FIRST_IMAGE],
            version: "4",
            by_id: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, _engine) = super::ec2_query::emulator();
        let name = format!("lookup.absent.{index}");
        let launch_template_id = template_with_versions(row.case, &router, &name, row.images).await;
        let half = if row.by_id {
            p("LaunchTemplateId", &launch_template_id)
        } else {
            p("LaunchTemplateName", &name)
        };
        let (status, body) = api(
            &router,
            "DescribeLaunchTemplateVersions",
            &[half, p("LaunchTemplateVersion.1", row.version)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the read to be refused, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "InvalidLaunchTemplateId.VersionNotFound",
            "case {:?}: expected EC2's version-not-found code, got {body}",
            row.case
        );
        assert_eq!(
            error_message(&body),
            expected_sentence(row.version, &launch_template_id),
            "case {:?}: expected EC2's sentence for a version the template does not hold",
            row.case
        );
    }
}

/// RULE: the versions the same template DOES resolve are served — every number it minted, and
/// the two words that float on the version in force — so the refusal above is about the
/// version and not about the operation.
#[tokio::test]
async fn every_version_the_template_holds_is_read_rather_than_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    let launch_template_id = template_with_versions(
        "held versions",
        &router,
        "lookup.held",
        &[FIRST_IMAGE, SECOND_IMAGE],
    )
    .await;

    // (case, the version word, the version number it resolves to)
    let rows: &[(&str, &str, &str)] = &[
        ("the first version", "1", "1"),
        ("the version minted after it", "2", "2"),
        (
            "the word that floats on the latest version",
            ec2::LAUNCH_TEMPLATE_VERSION_LATEST,
            "2",
        ),
        (
            "the word that floats on the default version",
            ec2::LAUNCH_TEMPLATE_VERSION_DEFAULT,
            "1",
        ),
    ];
    for (case, version, resolved) in rows {
        let (status, body) = api(
            &router,
            "DescribeLaunchTemplateVersions",
            &[
                p("LaunchTemplateId", &launch_template_id),
                p("LaunchTemplateVersion.1", version),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a version the template holds must be read, got {status}: {body}"
        );
        assert_eq!(
            elements(&body, "versionNumber"),
            vec![(*resolved).to_string()],
            "case {case:?}: expected exactly the version the word names, got {body}"
        );
    }
}

/// RULE: a LAUNCH from a version the template does not hold is refused the same code and the
/// same sentence, and launches nothing.
#[tokio::test]
async fn a_launch_from_an_absent_launch_template_version_states_the_same_sentence() {
    let (router, _engine) = super::ec2_query::emulator();
    let launch_template_id = template_with_versions(
        "launch",
        &router,
        "lookup.launch",
        &[FIRST_IMAGE, SECOND_IMAGE],
    )
    .await;

    for version in ["3", "40"] {
        let (status, body) = api(
            &router,
            "RunInstances",
            &[
                p("LaunchTemplate.LaunchTemplateId", &launch_template_id),
                p("LaunchTemplate.Version", version),
                p("MinCount", "1"),
                p("MaxCount", "1"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {version:?}: expected the launch to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidLaunchTemplateId.VersionNotFound",
            "case {version:?}: expected EC2's version-not-found code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_sentence(version, &launch_template_id),
            "case {version:?}: expected EC2's sentence for a version the template does not hold"
        );

        let (status, described) = api(&router, "DescribeInstances", &[]).await;
        assert_eq!(
            status, 200,
            "case {version:?}: DescribeInstances expected 200, got {status}: {described}"
        );
        assert!(
            elements(&described, "instanceId").is_empty(),
            "case {version:?}: a refused launch leaves no instance, got {described}"
        );
    }
}
