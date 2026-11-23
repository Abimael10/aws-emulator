// SPDX-License-Identifier: CC0-1.0

//! The sentences EC2 refuses an absent launch-template version with, per the VALIDATOR that
//! named it, driven over the wire the service speaks (`ec2Query`) through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! RULE: `InvalidLaunchTemplateId.VersionNotFound` is one code answered by several
//! validators, and the message belongs to the validator rather than to the code — each
//! operation states the sentence AWS records for IT, with only the variable parts that
//! sentence carries:
//!
//! * `CreateLaunchTemplateVersion` refusing a `SourceVersion` the named template holds no
//!   version for states fixed text alone, naming neither the version nor the template;
//! * `ModifyLaunchTemplate` refusing a `DefaultVersion` the named template holds no version
//!   for names the version the request asked for, unquoted, and leaves the template as "the
//!   specified launch template";
//! * a request that RESOLVES a version to READ it or to LAUNCH from it names both the
//!   version and the template's id.
//!
//! The rows span what the rule generalises over: how far past the versions the template holds
//! the request reaches (one past the last minted, far past it, at and below the first version
//! a template can carry, and a negative number), which half of the reference names the
//! template (its id or its name), and how many versions the template holds. Every row also
//! checks the refusal moved nothing: the template still holds the versions it did and still
//! reports the default version it did.
//!
//! The versions each template DOES hold are served by the same two operations, so the
//! sentences above are about the version and not about the operation.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message};

/// The Region every call here is signed for: a launch template carries no Region member, so
/// the Region only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// AMI ids of the published form, so each version carries one launch parameter and the
/// versions can be told apart.
const IMAGES: [&str; 3] = [
    "ami-00000000000000001",
    "ami-00000000000000002",
    "ami-00000000000000003",
];

/// The code EC2 answers for a launch template version the named template does not hold,
/// whichever validator asked.
const VERSION_NOT_FOUND: &str = "InvalidLaunchTemplateId.VersionNotFound";

/// `CreateLaunchTemplateVersion`'s whole sentence for a `SourceVersion` that does not exist:
/// fixed text, no values.
const SOURCE_VERSION_SENTENCE: &str = "Launch template version does not exist.";

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

/// `ModifyLaunchTemplate`'s sentence for a `DefaultVersion` that does not exist: one variable
/// part, the version the request named.
fn default_version_sentence(version: &str) -> String {
    format!("The launch template version {version} is not found for the specified launch template.")
}

/// A reader's sentence for the same reference, which names both halves of it.
fn resolved_version_sentence(version: &str, launch_template_id: &str) -> String {
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
    versions: usize,
) -> String {
    let (status, body) = api(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", IMAGES[0]),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the template must be created, got {status}: {body}"
    );
    let launch_template_id =
        element(&body, "launchTemplateId").unwrap_or_else(|| panic!("case {case:?}: {body}"));
    for image in IMAGES.iter().take(versions).skip(1) {
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

/// The version numbers the template holds, and the version it reports as its default.
async fn template_counters(
    case: &str,
    router: &MultiServiceRouter,
    launch_template_id: &str,
) -> (Vec<String>, Option<String>) {
    let (status, body) = api(
        router,
        "DescribeLaunchTemplateVersions",
        &[p("LaunchTemplateId", launch_template_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the versions must be readable, got {status}: {body}"
    );
    let held = elements(&body, "versionNumber");
    let (status, described) = api(
        router,
        "DescribeLaunchTemplates",
        &[p("LaunchTemplateId.1", launch_template_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the template must be readable, got {status}: {described}"
    );
    (held, element(&described, "defaultVersionNumber"))
}

/// One row of the absent-version table: how many versions the template holds, the version the
/// request names, and whether it names the template by its id rather than by its name.
struct AbsentVersionRow {
    case: &'static str,
    versions: usize,
    version: &'static str,
    by_id: bool,
}

/// The rows both validators are driven over, so the two sentences are compared on the same
/// references.
const ABSENT_VERSION_ROWS: &[AbsentVersionRow] = &[
    AbsentVersionRow {
        case: "one past the only version a new template holds",
        versions: 1,
        version: "2",
        by_id: true,
    },
    AbsentVersionRow {
        case: "one past the last version of a template that holds three",
        versions: 3,
        version: "4",
        by_id: true,
    },
    AbsentVersionRow {
        case: "far past the last version minted",
        versions: 2,
        version: "999",
        by_id: true,
    },
    AbsentVersionRow {
        case: "below the first version a template can carry",
        versions: 1,
        version: "0",
        by_id: true,
    },
    AbsentVersionRow {
        case: "a negative version number",
        versions: 2,
        version: "-1",
        by_id: true,
    },
    AbsentVersionRow {
        case: "a template named by its NAME rather than its id",
        versions: 1,
        version: "9",
        by_id: false,
    },
];

/// The half of the request that names the template, as the row asks for it.
fn naming_half(row: &AbsentVersionRow, name: &str, launch_template_id: &str) -> (String, String) {
    if row.by_id {
        p("LaunchTemplateId", launch_template_id)
    } else {
        p("LaunchTemplateName", name)
    }
}

/// RULE: `CreateLaunchTemplateVersion` refusing a `SourceVersion` the template holds no
/// version for states EC2's fixed sentence for that validator — the same text whatever
/// version was asked for and whichever half of the reference named the template — and stores
/// no version.
#[tokio::test]
async fn an_absent_source_version_is_refused_with_the_writers_fixed_sentence() {
    for (index, row) in ABSENT_VERSION_ROWS.iter().enumerate() {
        let (router, _engine) = super::ec2_query::emulator();
        let name = format!("source.absent.{index}");
        let launch_template_id =
            template_with_versions(row.case, &router, &name, row.versions).await;
        let before = template_counters(row.case, &router, &launch_template_id).await;

        let (status, body) = api(
            &router,
            "CreateLaunchTemplateVersion",
            &[
                naming_half(row, &name, &launch_template_id),
                p("SourceVersion", row.version),
                p("LaunchTemplateData.KeyName", "a-key"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the create to be refused, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            VERSION_NOT_FOUND,
            "case {:?}: expected {VERSION_NOT_FOUND}, got {body}",
            row.case
        );
        let message = error_message(&body);
        assert_eq!(
            message, SOURCE_VERSION_SENTENCE,
            "case {:?}: expected {SOURCE_VERSION_SENTENCE:?}, got {message:?}",
            row.case
        );

        let after = template_counters(row.case, &router, &launch_template_id).await;
        assert_eq!(
            after, before,
            "case {:?}: a refused create leaves the template's versions and default version \
             alone, went from {before:?} to {after:?}",
            row.case
        );
    }
}

/// RULE: `ModifyLaunchTemplate` refusing a `DefaultVersion` the template holds no version for
/// names the version the request asked for in its own sentence, and moves the default version
/// nowhere.
#[tokio::test]
async fn an_absent_default_version_is_refused_naming_the_version_asked_for() {
    for (index, row) in ABSENT_VERSION_ROWS.iter().enumerate() {
        let (router, _engine) = super::ec2_query::emulator();
        let name = format!("default.absent.{index}");
        let launch_template_id =
            template_with_versions(row.case, &router, &name, row.versions).await;
        let before = template_counters(row.case, &router, &launch_template_id).await;

        let (status, body) = api(
            &router,
            "ModifyLaunchTemplate",
            &[
                naming_half(row, &name, &launch_template_id),
                p("SetDefaultVersion", row.version),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the modify to be refused, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            VERSION_NOT_FOUND,
            "case {:?}: expected {VERSION_NOT_FOUND}, got {body}",
            row.case
        );
        let expected = default_version_sentence(row.version);
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );

        let after = template_counters(row.case, &router, &launch_template_id).await;
        assert_eq!(
            after, before,
            "case {:?}: a refused modify leaves the template's versions and default version \
             alone, went from {before:?} to {after:?}",
            row.case
        );
    }
}

/// RULE: the three validators word the one code differently, so the same absent reference
/// earns three different sentences — the writer's fixed text, the default-version setter's
/// sentence naming the version, and the reader's naming both halves.
#[tokio::test]
async fn the_three_validators_of_one_code_state_three_different_sentences() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "one reference, three validators";
    let name = "sentences.one.reference";
    let launch_template_id = template_with_versions(case, &router, name, 2).await;
    let version = "7";

    let (create_status, created) = api(
        &router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", &launch_template_id),
            p("SourceVersion", version),
            p("LaunchTemplateData.KeyName", "a-key"),
        ],
    )
    .await;
    let (modify_status, modified) = api(
        &router,
        "ModifyLaunchTemplate",
        &[
            p("LaunchTemplateId", &launch_template_id),
            p("SetDefaultVersion", version),
        ],
    )
    .await;
    let (describe_status, described) = api(
        &router,
        "DescribeLaunchTemplateVersions",
        &[
            p("LaunchTemplateId", &launch_template_id),
            p("LaunchTemplateVersion.1", version),
        ],
    )
    .await;
    for (validator, status, body) in [
        ("the writer", create_status, &created),
        ("the default-version setter", modify_status, &modified),
        ("the reader", describe_status, &described),
    ] {
        assert_eq!(
            status, 400,
            "case {case:?}: {validator} must refuse the absent version, got {status}: {body}"
        );
        assert_eq!(
            error_code(body),
            VERSION_NOT_FOUND,
            "case {case:?}: {validator} must answer {VERSION_NOT_FOUND}, got {body}"
        );
    }

    let sentences = [
        ("the writer's", error_message(&created), SOURCE_VERSION_SENTENCE.to_string()),
        (
            "the default-version setter's",
            error_message(&modified),
            default_version_sentence(version),
        ),
        (
            "the reader's",
            error_message(&described),
            resolved_version_sentence(version, &launch_template_id),
        ),
    ];
    for (validator, actual, expected) in &sentences {
        assert_eq!(
            actual, expected,
            "case {case:?}: {validator} sentence expected {expected:?}, got {actual:?}"
        );
    }
    let distinct: std::collections::BTreeSet<&String> =
        sentences.iter().map(|(_, actual, _)| actual).collect();
    assert_eq!(
        distinct.len(),
        sentences.len(),
        "case {case:?}: each validator words the code its own way, got {distinct:?}"
    );
}

/// RULE: the versions a template DOES hold are served by both writers — a `SourceVersion` it
/// holds yields a new version carrying the source's parameters, and a `DefaultVersion` it
/// holds becomes the template's default — so the refusals above are about the version and not
/// about the operation.
#[tokio::test]
async fn every_version_the_template_holds_is_served_by_both_writers() {
    // (case, the version word the request names, the version it resolves to, that version's
    // ImageId) on a fresh template that holds two versions.
    let rows: &[(&str, &str, &str, &str)] = &[
        ("the first version by number", "1", "1", IMAGES[0]),
        ("the version minted after it", "2", "2", IMAGES[1]),
        (
            "the word that floats on the latest version",
            "$Latest",
            "2",
            IMAGES[1],
        ),
        (
            "the word that floats on the default version",
            "$Default",
            "1",
            IMAGES[0],
        ),
    ];
    for (index, (case, version, resolved, image)) in rows.iter().enumerate() {
        let (router, _engine) = super::ec2_query::emulator();
        let name = format!("sentences.held.{index}");
        let launch_template_id = template_with_versions(case, &router, &name, 2).await;

        // The default version is set FIRST, so the word each request resolves is resolved
        // against the versions the template held when it was created.
        let (status, body) = api(
            &router,
            "ModifyLaunchTemplate",
            &[
                p("LaunchTemplateId", &launch_template_id),
                p("SetDefaultVersion", version),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a default version the template holds must be served, got \
             {status}: {body}"
        );
        assert_eq!(
            element(&body, "defaultVersionNumber").as_deref(),
            Some(*resolved),
            "case {case:?}: expected the default version {resolved}, got {body}"
        );

        let (status, body) = api(
            &router,
            "CreateLaunchTemplateVersion",
            &[
                p("LaunchTemplateId", &launch_template_id),
                p("SourceVersion", version),
                p("LaunchTemplateData.KeyName", "a-key"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a source version the template holds must be served, got \
             {status}: {body}"
        );
        assert_eq!(
            element(&body, "imageId").as_deref(),
            Some(*image),
            "case {case:?}: the new version inherits the source's ImageId, got {body}"
        );
    }
}
