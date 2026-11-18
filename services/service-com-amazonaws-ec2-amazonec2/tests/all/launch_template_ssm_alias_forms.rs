// SPDX-License-Identifier: CC0-1.0

//! WHICH `ImageId` VALUES A LAUNCH TEMPLATE WRITER TAKES when the value names a Systems
//! Manager parameter instead of an AMI, driven over the wire the service speaks (`ec2Query`,
//! an `application/x-www-form-urlencoded` POST answered with XML — README.md / OPERATIONS.md
//! "Wire protocol") through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: "Alternatively, you can specify a Systems Manager parameter, using
//! one of the following formats. The Systems Manager parameter will resolve to an AMI ID on
//! launch" (`RequestLaunchTemplateData$ImageId`) — so a write of a launch template version
//! STORES such a value as submitted and reads nothing out of it: whether the parameter
//! exists, and which AMI it names, is settled by the launch that resolves it and by no
//! earlier call. Neither `CreateLaunchTemplate` nor `CreateLaunchTemplateVersion` refuses a
//! value for naming a parameter it cannot resolve.
//!
//! The rows are the alias forms that member documents — a public parameter, a parameter name
//! of the account's own, that name with a version number or with a label, and the parameter
//! ARN of a parameter shared from another account in each of those three shapes
//! (`smithy_query(query="doc",shape="RequestLaunchTemplateData",member="ImageId")`) — plus a
//! name in the published namespace that this account's catalog carries no image under, which
//! is the case a writer must not read as a reason to refuse. Each row is a distinct input
//! along what the rule generalises over (which form the value takes, and whether anything
//! resolves it), so no row is the specification; the published name every row is compared
//! against comes out of the store the handlers read rather than being written here.
//!
//! `launch_template_published_ssm_alias` owns the whole published catalog as rows of the
//! create's acceptance, and `launch_from_ssm_image_alias` owns what a LAUNCH does with each
//! value; this suite owns the forms a writer takes and the fact that it resolves none of
//! them.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, FIRST_LAUNCH_TEMPLATE_VERSION,
    SSM_IMAGE_ALIAS_PREFIX,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code};

/// The Region every call here is signed for. A launch template carries no Region member, so
/// the Region only selects the ec2Query envelope — and it is the Region of the parameter ARN
/// the shared-parameter rows carry.
const REGION: &str = "us-east-1";

/// The instance type every template here names, beside the `ImageId` under test.
const INSTANCE_TYPE: &str = "t3.micro";

/// A parameter name of the caller's OWN namespace: "to reference a parameter stored in the
/// same account: `resolve:ssm:parameter-name`". Nothing publishes it, so a writer that read
/// the value would find nothing behind it.
const OWN_PARAMETER: &str = "/team/golden/image-id";

/// A name in the PUBLISHED namespace (`Image$PublicSsmParameterName`: "under the
/// `aws/service/` namespace") that the catalog carries no image under.
const UNPUBLISHED_PARAMETER: &str = "/aws/service/ami-amazon-linux-latest/no-such-build";

/// The version number a `parameter-name:version-number` row pins.
const PARAMETER_VERSION: &str = "3";

/// The label a `parameter-name:label` row names.
const PARAMETER_LABEL: &str = "released";

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `LaunchTemplateData.<member>` parameter.
fn data(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// The `resolve:ssm:` value naming `parameter`.
fn alias(parameter: &str) -> String {
    format!("{SSM_IMAGE_ALIAS_PREFIX}{parameter}")
}

/// The ARN of a Systems Manager parameter, as the one canonical ARN builder of this
/// workspace writes it ([`engine::emulator_arn`]).
fn parameter_arn(parameter: &str) -> String {
    engine::emulator_arn("ssm", REGION, &format!("parameter{parameter}"))
}

/// The first public parameter name this account's published catalog resolves — read out of
/// the store the handlers read, so the row is the service's own catalog rather than a copy of
/// it — and the id of the AMI it names.
fn published_parameter(engine: &engine::InMemoryEngine) -> (String, String) {
    engine
        .state::<Ec2State>()
        .lock()
        .public_ssm_parameter_image_ids(engine::emulator_account_id())
        .into_iter()
        .next()
        .expect("the published image catalog must carry a parameter name")
}

/// Every alias form a launch template writer takes, as `(what the row names, the `ImageId`
/// the request carries)`.
///
/// `published` is the one name read out of the store, so the published-parameter row cannot
/// drift from the catalog.
fn alias_forms(published: &str) -> Vec<(String, String)> {
    let arn = parameter_arn(OWN_PARAMETER);
    vec![
        (
            "a public parameter the catalog resolves".to_string(),
            alias(published),
        ),
        (
            "a public parameter name the catalog resolves nothing for".to_string(),
            alias(UNPUBLISHED_PARAMETER),
        ),
        (
            "a parameter name of the account's own".to_string(),
            alias(OWN_PARAMETER),
        ),
        (
            "that name with a version number".to_string(),
            alias(&format!("{OWN_PARAMETER}:{PARAMETER_VERSION}")),
        ),
        (
            "that name with a label".to_string(),
            alias(&format!("{OWN_PARAMETER}:{PARAMETER_LABEL}")),
        ),
        (
            "the ARN of a parameter shared from another account".to_string(),
            alias(&arn),
        ),
        (
            "that ARN with a version number".to_string(),
            alias(&format!("{arn}:{PARAMETER_VERSION}")),
        ),
        (
            "that ARN with a label".to_string(),
            alias(&format!("{arn}:{PARAMETER_LABEL}")),
        ),
    ]
}

/// The `ImageId` the STORE holds for `version` of the template `launch_template_id` — the
/// value as the writer wrote it, read where every later reader of the template sees it.
fn stored_image_id(
    engine: &engine::InMemoryEngine,
    launch_template_id: &str,
    version: i64,
) -> Option<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .launch_templates
        .get(launch_template_id)
        .and_then(|record| record.versions.get(&version))
        .and_then(|stored| stored.data.image_id.clone())
}

/// The `imageId` `DescribeLaunchTemplateVersions` reports for every version of a template,
/// with no `ResolveAlias`: "if false, and if a Systems Manager parameter is specified for
/// `ImageId`, the parameter is displayed in the response for `imageId`. Default: false".
async fn described_image_ids(
    case: &str,
    router: &MultiServiceRouter,
    launch_template_id: &str,
) -> Vec<String> {
    let (status, body) = api(
        router,
        "DescribeLaunchTemplateVersions",
        &[p("LaunchTemplateId", launch_template_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeLaunchTemplateVersions, got {status} \
         with body {body}"
    );
    elements(&body, "imageId")
}

/// RULE: `CreateLaunchTemplate` ACCEPTS an `ImageId` in every documented `resolve:ssm:` form
/// — 200 with the launch template it stored, whose only version is both its default and its
/// latest — and the version keeps the value exactly as submitted, whether or not anything
/// this account can see resolves the parameter it names.
#[tokio::test]
async fn a_create_accepts_every_alias_form_and_stores_it_as_submitted() {
    let (router, engine) = super::ec2_query::emulator();
    let (published, _) = published_parameter(&engine);
    let first_version = FIRST_LAUNCH_TEMPLATE_VERSION.to_string();

    for (index, (case, submitted)) in alias_forms(&published).iter().enumerate() {
        let name = format!("alias.form.{index}");
        let (status, body) = api(
            &router,
            "CreateLaunchTemplate",
            &[
                p("LaunchTemplateName", name.clone()),
                data("ImageId", submitted.clone()),
                data("InstanceType", INSTANCE_TYPE),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?} ({submitted}): expected HTTP 200 from CreateLaunchTemplate, got \
             {status} with body {body}"
        );
        let launch_template_id = element(&body, "launchTemplateId").unwrap_or_else(|| {
            panic!("case {case:?}: expected a <launchTemplateId>, got {body}")
        });
        assert_eq!(
            (
                element(&body, "launchTemplateName"),
                element(&body, "defaultVersionNumber"),
                element(&body, "latestVersionNumber"),
            ),
            (
                Some(name.clone()),
                Some(first_version.clone()),
                Some(first_version.clone()),
            ),
            "case {case:?}: expected the stored template's name and its one version, got {body}"
        );

        let held = stored_image_id(&engine, &launch_template_id, FIRST_LAUNCH_TEMPLATE_VERSION);
        assert_eq!(
            held.as_deref(),
            Some(submitted.as_str()),
            "case {case:?}: expected the stored version to hold {submitted:?} unresolved, got \
             {held:?}"
        );
        let reported = described_image_ids(case, &router, &launch_template_id).await;
        assert_eq!(
            reported,
            vec![submitted.clone()],
            "case {case:?}: expected the version to report {submitted:?}, got {reported:?}"
        );
    }
}

/// The same rule for the OTHER writer: `CreateLaunchTemplateVersion` takes every form too and
/// stores it as submitted, so the two writers cannot disagree about which value a stored
/// version may carry.
#[tokio::test]
async fn a_version_create_accepts_every_alias_form_and_stores_it_as_submitted() {
    let (router, engine) = super::ec2_query::emulator();
    let (published, _) = published_parameter(&engine);

    // One template to hold the versions, created from a literal AMI id so the forms under
    // test are what the versions add.
    let (status, body) = api(
        &router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "alias.form.versions"),
            data("ImageId", "ami-0123456789abcdef0"),
            data("InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the template holding the versions must be created, got {status} with body {body}"
    );
    let launch_template_id =
        element(&body, "launchTemplateId").unwrap_or_else(|| panic!("no <launchTemplateId> in {body}"));

    let forms = alias_forms(&published);
    for (position, (case, submitted)) in forms.iter().enumerate() {
        let (status, body) = api(
            &router,
            "CreateLaunchTemplateVersion",
            &[
                p("LaunchTemplateId", launch_template_id.clone()),
                data("ImageId", submitted.clone()),
                data("InstanceType", INSTANCE_TYPE),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?} ({submitted}): expected HTTP 200 from \
             CreateLaunchTemplateVersion, got {status} with body {body}"
        );
        // The versions are "numbered in the order in which they are created"
        // (`CreateLaunchTemplateVersion`), so this one is the template's first plus the rows
        // created before it.
        let expected_number = FIRST_LAUNCH_TEMPLATE_VERSION + 1 + position as i64;
        assert_eq!(
            element(&body, "versionNumber"),
            Some(expected_number.to_string()),
            "case {case:?}: expected version {expected_number}, got {body}"
        );
        assert_eq!(
            element(&body, "imageId").as_deref(),
            Some(submitted.as_str()),
            "case {case:?}: expected the created version to report {submitted:?}, got {body}"
        );
        let held = stored_image_id(&engine, &launch_template_id, expected_number);
        assert_eq!(
            held.as_deref(),
            Some(submitted.as_str()),
            "case {case:?}: expected the stored version to hold {submitted:?} unresolved, got \
             {held:?}"
        );
    }

    // Every version the rows created reads back with its own value, none of them resolved.
    let reported = described_image_ids("every version", &router, &launch_template_id).await;
    for (case, submitted) in &forms {
        assert!(
            reported.contains(submitted),
            "case {case:?}: expected {submitted:?} among the versions described, got \
             {reported:?}"
        );
    }
}

/// RULE, second half: the parameter is read by the LAUNCH and by nothing earlier — a launch
/// from a template whose value the catalog resolves runs from that AMI, while one whose value
/// nothing resolves is refused there, the template itself staying exactly as it was stored.
#[tokio::test]
async fn the_launch_is_what_resolves_the_parameter_a_stored_version_carries() {
    let (router, engine) = super::ec2_query::emulator();
    let (published, published_image) = published_parameter(&engine);

    // (what the row names, the stored ImageId, the AMI a launch from it reports — `None` for
    // a value this account resolves nothing for)
    let rows: Vec<(String, String, Option<String>)> = vec![
        (
            "a public parameter the catalog resolves".to_string(),
            alias(&published),
            Some(published_image),
        ),
        (
            "a public parameter name the catalog resolves nothing for".to_string(),
            alias(UNPUBLISHED_PARAMETER),
            None,
        ),
        (
            "a parameter name of the account's own".to_string(),
            alias(OWN_PARAMETER),
            None,
        ),
        (
            "the ARN of a parameter shared from another account".to_string(),
            alias(&parameter_arn(OWN_PARAMETER)),
            None,
        ),
    ];

    for (index, (case, submitted, resolved)) in rows.iter().enumerate() {
        let name = format!("alias.launch.{index}");
        let (status, body) = api(
            &router,
            "CreateLaunchTemplate",
            &[
                p("LaunchTemplateName", name.clone()),
                data("ImageId", submitted.clone()),
                data("InstanceType", INSTANCE_TYPE),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create must be served, got {status} with body {body}"
        );
        let launch_template_id = element(&body, "launchTemplateId").unwrap_or_else(|| {
            panic!("case {case:?}: expected a <launchTemplateId>, got {body}")
        });

        let (status, launched) = api(
            &router,
            "RunInstances",
            &[
                p("LaunchTemplate.LaunchTemplateId", launch_template_id.clone()),
                p("MinCount", "1"),
                p("MaxCount", "1"),
            ],
        )
        .await;
        match resolved {
            Some(image_id) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from RunInstances, got {status} with \
                     body {launched}"
                );
                assert_eq!(
                    element(&launched, "imageId").as_deref(),
                    Some(image_id.as_str()),
                    "case {case:?}: expected the launch to run from {image_id}, got {launched}"
                );
            }
            // Which refusal class a launch answers for a value nothing resolves is
            // `launch_from_ssm_image_alias`'s own rule; what this row pins is WHERE the
            // value is read: the launch refuses it, the write did not.
            None => assert_eq!(
                status, 400,
                "case {case:?}: expected the launch to refuse a parameter nothing resolves, \
                 got {status} with body {launched} (code {:?})",
                error_code(&launched)
            ),
        }

        // The template is untouched by either outcome: the value it stored is still the one
        // submitted, read back through the describe.
        let reported = described_image_ids(case, &router, &launch_template_id).await;
        assert_eq!(
            reported,
            vec![submitted.clone()],
            "case {case:?}: expected the stored version to still report {submitted:?}, got \
             {reported:?}"
        );
    }
}
