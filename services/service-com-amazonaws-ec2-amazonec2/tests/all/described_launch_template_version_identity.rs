// SPDX-License-Identifier: CC0-1.0

//! What `DescribeLaunchTemplateVersions` answers ABOUT THE VERSION IT SELECTED, driven over
//! the wire the service speaks (`ec2Query`) through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rules under test:
//!
//! 1. THE REPORT IS THE SELECTED VERSION'S OWN IDENTITY. A version is named by number or by
//!    one of the aliases the launch template API documents, "$Latest, $Default, and numbers"
//!    (`DescribeLaunchTemplateVersionsRequest$Versions`), and whichever way it was named the
//!    item reported for it carries THAT version's number ("the version number",
//!    `LaunchTemplateVersion$VersionNumber`) and THAT version's stored launch parameters
//!    ("information about the launch template", `LaunchTemplateVersion$LaunchTemplateData`).
//!    So `$Latest` follows the versions the template holds right now: a template whose second
//!    version has just been created answers that second version and its launch parameters,
//!    not the first version's.
//! 2. `defaultVersion` IS COMPUTED AGAINST THE TEMPLATE'S CURRENT DEFAULT, never against the
//!    version's position: "indicates whether the version is the default version"
//!    (`LaunchTemplateVersion$DefaultVersion`) is true for exactly the version the template's
//!    `DefaultVersionNumber` names, so a newly created version is reported as NOT the default
//!    while the default still sits on an earlier one, and moving the default moves the flag.
//! 3. EVERY REPORTED VERSION CARRIES THE TEMPLATE'S `operator`. "The entity that manages the
//!    launch template" (`LaunchTemplateVersion$Operator`) is a fact of the template, so every
//!    version of it answers the same `OperatorResponse`: `managed` false and no principal for
//!    a template nobody manages — an answer, not something unknown, so the member is never
//!    left out — and `managed` true naming the provider for a template a provider created.
//!
//! Each table row is a distinct input along what the rules generalise over: how many versions
//! the template holds (one, two, three), how the request selects a version (by number, by
//! `$Latest`, by `$Default`, by two selectors at once, by naming none at all), where the
//! template's default sits (the first version, then a later one), and whether a service
//! provider manages the template — so no single call is the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, FIRST_LAUNCH_TEMPLATE_VERSION};

use super::ec2_query::{call, element, indexed};
use super::wire::{child, child_text, document};

/// The Region every call here is signed for. A launch template carries no Region member, so
/// the Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// The instance type each version of the template under test carries, version 1 first: the
/// launch parameters are what say WHICH version answered a selection, so no two versions
/// share one.
const VERSION_INSTANCE_TYPES: [&str; 3] = ["t3.micro", "t3.small", "m5.large"];

/// The service provider a managed template is created for — the value `Operator.Principal`
/// carries and `OperatorResponse$Principal` reports back.
const MANAGING_PRINCIPAL: &str = "autoscaling.amazonaws.com";

/// One version as this read reports it: the four members the rules assert over.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedVersion {
    /// `LaunchTemplateVersion$VersionNumber`.
    version_number: i64,
    /// `LaunchTemplateVersion$DefaultVersion`, as the wire wrote it.
    default_version: Option<String>,
    /// The `instanceType` of the version's own `launchTemplateData`.
    instance_type: Option<String>,
    /// The `(managed, principal)` of the version's `operator`, and `None` when the item
    /// carries no `operator` element at all — the absence a client reading the member cannot
    /// act on.
    operator: Option<Operator>,
}

/// The `(managed, principal)` an `OperatorResponse` answers, as the wire wrote the two
/// members: `managed` is answered for every resource, `principal` only where a provider
/// manages it.
type Operator = (Option<String>, Option<String>);

/// One row of the template table: what it covers, the `Operator.Principal` its create carries
/// (none at all for a template this account creates for itself), and the operator every
/// version of it must then report.
type TemplateRow = (&'static str, Option<&'static str>, Operator);

/// What a version must be reported as: its number, whether it is the template's default
/// version, and the index into [`VERSION_INSTANCE_TYPES`] its launch parameters carry.
type Expected = (i64, bool, usize);

/// A `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One `LaunchTemplateData.<member>` parameter.
fn data(member: &str, value: &str) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// One call that has to succeed, as the body it answered.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Create one launch template whose first version carries the first of
/// [`VERSION_INSTANCE_TYPES`], managed by `operator_principal` when the caller names one, and
/// answer the id it minted.
async fn create_template(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    operator_principal: Option<&str>,
) -> String {
    let mut params = vec![
        p("LaunchTemplateName", name),
        data("InstanceType", VERSION_INSTANCE_TYPES[0]),
    ];
    if let Some(principal) = operator_principal {
        params.push(p("Operator.Principal", principal));
    }
    let body = ok(case, router, "CreateLaunchTemplate", &params).await;
    element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must report the id it minted: {body}"))
}

/// Create the next version of `launch_template_id`, carrying the instance type
/// [`VERSION_INSTANCE_TYPES`] holds at `index`, and assert the writer numbered it `index + 1`
/// — "launch template versions are numbered in the order in which they are created"
/// (`CreateLaunchTemplateVersion`).
async fn create_version(
    case: &str,
    router: &MultiServiceRouter,
    launch_template_id: &str,
    index: usize,
) {
    let body = ok(
        case,
        router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", launch_template_id),
            data("InstanceType", VERSION_INSTANCE_TYPES[index]),
        ],
    )
    .await;
    let expected = FIRST_LAUNCH_TEMPLATE_VERSION + i64::try_from(index).unwrap_or_default();
    assert_eq!(
        element(&body, "versionNumber"),
        Some(expected.to_string()),
        "case {case:?}: expected the create to store version {expected}, got {body}"
    );
}

/// Every version a body reports, in the order it carried them — read through the XML DOCUMENT
/// so a member of the VERSION is never confused with one of the same name nested inside its
/// launch parameters.
fn reported_versions(case: &str, body: &str) -> Vec<ReportedVersion> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "versionNumber").is_some())
        .map(|node| ReportedVersion {
            version_number: child_text(node, "versionNumber")
                .and_then(|number| number.parse::<i64>().ok())
                .unwrap_or_default(),
            default_version: child_text(node, "defaultVersion"),
            instance_type: child(node, "launchTemplateData")
                .and_then(|data| child_text(data, "instanceType")),
            operator: child(node, "operator").map(|operator| {
                (
                    child_text(operator, "managed"),
                    child_text(operator, "principal"),
                )
            }),
        })
        .collect()
}

/// The `ReportedVersion` each `Expected` row stands for, under the `operator` every version of
/// the template answers.
fn wanted(expected: &[Expected], operator: Operator) -> Vec<ReportedVersion> {
    expected
        .iter()
        .map(|(version_number, default_version, instance_type)| ReportedVersion {
            version_number: *version_number,
            default_version: Some(default_version.to_string()),
            instance_type: Some(VERSION_INSTANCE_TYPES[*instance_type].to_string()),
            operator: Some(operator.clone()),
        })
        .collect()
}

/// RULES 1, 2 and 3 over one template: every selection answers the versions it names, each
/// with its own number, its own launch parameters, the default flag the template's current
/// default version gives it, and the template's operator.
///
/// `selectors` are the `Versions` entries, which reach the wire under the member's xmlName,
/// `LaunchTemplateVersion.N`; an empty list names no version at all, which describes every
/// version the template holds.
async fn check(
    case: &str,
    router: &MultiServiceRouter,
    launch_template_id: &str,
    selectors: &[&str],
    expected: &[Expected],
    operator: Operator,
) {
    let mut params = vec![p("LaunchTemplateId", launch_template_id)];
    params.extend(indexed("LaunchTemplateVersion", selectors));
    let body = ok(case, router, "DescribeLaunchTemplateVersions", &params).await;
    let reported = reported_versions(case, &body);
    let wanted = wanted(expected, operator);
    assert_eq!(
        reported, wanted,
        "case {case:?}: expected the versions {wanted:?}, got {reported:?} from body {body}"
    );
}

/// RULES 1, 2 and 3: the read answers the selected version's own identity, flags as the
/// default exactly the version the template defaults to, and carries the operator on every
/// version — for a template nobody manages and for one a service provider manages alike.
#[tokio::test]
async fn each_selection_answers_the_version_it_named_with_its_own_data_and_operator() {
    // The two kinds of template the operator rule spans: one this account created for itself,
    // and one created for a service provider, which the provider then manages.
    let templates: [TemplateRow; 2] = [
        (
            "a template nobody manages",
            None,
            (Some("false".to_string()), None),
        ),
        (
            "a template a service provider manages",
            Some(MANAGING_PRINCIPAL),
            (
                Some("true".to_string()),
                Some(MANAGING_PRINCIPAL.to_string()),
            ),
        ),
    ];

    for (template_case, principal, operator) in templates {
        let (router, _engine) = super::ec2_query::emulator();
        let launch_template_id =
            create_template(template_case, &router, "version.identity", principal).await;

        // One version held: it is the latest, the default, and the only answer to any
        // selection of it.
        for (case, selectors, expected) in [
            ("one version held, selected by $Latest", vec!["$Latest"], vec![(1, true, 0)]),
            ("one version held, selected by $Default", vec!["$Default"], vec![(1, true, 0)]),
            ("one version held, selected by number", vec!["1"], vec![(1, true, 0)]),
            ("one version held, naming no version", Vec::new(), vec![(1, true, 0)]),
        ] {
            let case = &format!("{template_case}: {case}");
            check(
                case,
                &router,
                &launch_template_id,
                &selectors,
                &expected,
                operator.clone(),
            )
            .await;
        }

        // A second version is created and the template's default is left where it was: the
        // template's $Latest is now the second version, its $Default still the first, and
        // neither answer carries the other's launch parameters.
        create_version(template_case, &router, &launch_template_id, 1).await;
        for (case, selectors, expected) in [
            (
                "two versions held, $Latest is the newer one",
                vec!["$Latest"],
                vec![(2, false, 1)],
            ),
            (
                "two versions held, $Default is still the first",
                vec!["$Default"],
                vec![(1, true, 0)],
            ),
            (
                "two versions held, the newer one by number",
                vec!["2"],
                vec![(2, false, 1)],
            ),
            (
                "two versions held, both aliases at once",
                vec!["$Latest", "$Default"],
                vec![(2, false, 1), (1, true, 0)],
            ),
            (
                "two versions held, naming no version",
                Vec::new(),
                vec![(2, false, 1), (1, true, 0)],
            ),
        ] {
            let case = &format!("{template_case}: {case}");
            check(
                case,
                &router,
                &launch_template_id,
                &selectors,
                &expected,
                operator.clone(),
            )
            .await;
        }

        // A third version, and then the default MOVED onto it: the flag follows the
        // template's default version rather than the version's position or its age.
        create_version(template_case, &router, &launch_template_id, 2).await;
        let before_move: Vec<(&str, Vec<&str>, Vec<Expected>)> = vec![
            (
                "three versions held, $Latest is the newest",
                vec!["$Latest"],
                vec![(3, false, 2)],
            ),
            (
                "three versions held, the middle one by number",
                vec!["2"],
                vec![(2, false, 1)],
            ),
            (
                "three versions held, naming no version",
                Vec::new(),
                vec![(3, false, 2), (2, false, 1), (1, true, 0)],
            ),
        ];
        for (case, selectors, expected) in before_move {
            let case = &format!("{template_case}: {case}");
            check(
                case,
                &router,
                &launch_template_id,
                &selectors,
                &expected,
                operator.clone(),
            )
            .await;
        }

        ok(
            template_case,
            &router,
            "ModifyLaunchTemplate",
            &[
                p("LaunchTemplateId", &launch_template_id),
                p("SetDefaultVersion", "3"),
            ],
        )
        .await;
        let after_move: Vec<(&str, Vec<&str>, Vec<Expected>)> = vec![
            (
                "the default moved to the newest, $Default follows it",
                vec!["$Default"],
                vec![(3, true, 2)],
            ),
            (
                "the default moved to the newest, $Latest is that same version",
                vec!["$Latest"],
                vec![(3, true, 2)],
            ),
            (
                "the default moved away from the first version",
                vec!["1"],
                vec![(1, false, 0)],
            ),
            (
                "the default moved to the newest, naming no version",
                Vec::new(),
                vec![(3, true, 2), (2, false, 1), (1, false, 0)],
            ),
        ];
        for (case, selectors, expected) in after_move {
            let case = &format!("{template_case}: {case}");
            check(
                case,
                &router,
                &launch_template_id,
                &selectors,
                &expected,
                operator.clone(),
            )
            .await;
        }
    }
}

/// RULE 3 where the record says nothing: a stored template holding no operator of its own
/// still answers one, because "if `true`, the resource is managed by a service provider"
/// (`OperatorResponse$Managed`) is an answer about every template — a template no provider
/// manages is managed by NOBODY, which is false, rather than unknown.
#[tokio::test]
async fn a_version_of_a_template_holding_no_operator_still_reports_one() {
    let case = "a stored template holding no operator";
    let (router, engine) = super::ec2_query::emulator();
    let launch_template_id = create_template(case, &router, "version.no.operator", None).await;
    create_version(case, &router, &launch_template_id, 1).await;

    // Take the operator off the stored record, which is the only thing the member is derived
    // from.
    {
        let state = engine.state::<Ec2State>();
        let mut store = state.lock();
        let record = store
            .launch_templates
            .get_mut(&launch_template_id)
            .expect("the store holds the template the create minted");
        record.template.operator = None;
    }

    check(
        case,
        &router,
        &launch_template_id,
        &["$Latest"],
        &[(2, false, 1)],
        (Some("false".to_string()), None),
    )
    .await;
}
