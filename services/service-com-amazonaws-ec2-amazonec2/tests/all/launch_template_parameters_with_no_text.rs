// SPDX-License-Identifier: CC0-1.0

//! The launch parameters a launch-template write stores, as against the values its fields
//! carried.
//!
//! The rule under test: EC2 speaks `ec2Query`, where a request's parameters are
//! `application/x-www-form-urlencoded` fields. A launch template is the set of launch
//! parameters a client gave it, so each parameter is STORED AND REPORTED as the request sent
//! it — a field carrying no text included, which reads back as a parameter carrying no text.
//! A field with no text sets no VALUE, though, and that is what decides the two questions
//! about it:
//!
//! * "you must specify at least one parameter for the launch template data"
//!   (`smithy_query(query="shape",shape="RequestLaunchTemplateData")`) asks whether the
//!   request NAMED a parameter, which a pair list carries whether or not it holds text: a
//!   request sending one parameter with no text has named one, and only a request naming none
//!   at all is refused;
//! * "if you specify additional launch parameters for the new version, they overwrite any
//!   corresponding launch parameters inherited from the source version"
//!   (`CreateLaunchTemplateVersionRequest$SourceVersion`) asks what a new version overwrites
//!   its source with, and a field with no text carries nothing to overwrite one with, so the
//!   source's parameter stands.
//!
//! Both writes of launch template data are driven, because the rule is about the data rather
//! than about one operation, and every stored version is read back through
//! `DescribeLaunchTemplateVersions` — the projection a client reads — rather than off the
//! create's own body alone.
//!
//! The rows span the parameters the rule generalises over: every member
//! `RequestLaunchTemplateData` targets a string with, each driven with no text, with text, and
//! left out.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;

use super::ec2_query::{call, error_code};
use super::wire::{child, child_text, document};

/// The Region every call here is signed for: a launch template carries no Region member, so
/// the Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// One text launch parameter: the member a request spells it as, the element a reported
/// version spells it as, and two values that carry text.
struct TextParameter {
    /// The `LaunchTemplateData.<member>` field name.
    member: &'static str,
    /// The element a reported version carries the parameter in.
    element: &'static str,
    /// A value of the right form for the parameter, carrying text.
    value: &'static str,
    /// Another such value, for the row that OVERWRITES an inherited one.
    other: &'static str,
}

/// Every member `RequestLaunchTemplateData` targets a string with, taken from the shape
/// itself (`smithy_query(query="shape",shape="RequestLaunchTemplateData")`): `KernelId`,
/// `ImageId`, `InstanceType`, `KeyName`, `RamDiskId` and `UserData` — so the parameter one
/// recorded client sent with no text is one row of six rather than the subject.
///
/// `InstanceInitiatedShutdownBehavior` is the shape's remaining text member and is NOT a row:
/// it targets an enum, whose membership the request walk judges before the handler runs, so a
/// value for it never reaches the launch parameters this suite is about.
const TEXT_PARAMETERS: [TextParameter; 6] = [
    TextParameter {
        member: "KernelId",
        element: "kernelId",
        value: "aki-00000000000000001",
        other: "aki-00000000000000002",
    },
    TextParameter {
        member: "ImageId",
        element: "imageId",
        value: "ami-00000000000000001",
        other: "ami-00000000000000002",
    },
    TextParameter {
        member: "InstanceType",
        element: "instanceType",
        value: "t3.micro",
        other: "m5.large",
    },
    TextParameter {
        member: "KeyName",
        element: "keyName",
        value: "deployment-key",
        other: "rotated-key",
    },
    TextParameter {
        member: "RamDiskId",
        element: "ramDiskId",
        value: "ari-00000000000000001",
        other: "ari-00000000000000002",
    },
    TextParameter {
        // `@sensitive`, and base64 text as the member documents ("you must provide
        // base64-encoded text") — a launch parameter like any other here.
        member: "UserData",
        element: "userData",
        value: "ZWNobyBvbmU=",
        other: "ZWNobyB0d28=",
    },
];

/// A `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One `LaunchTemplateData.<member>` parameter.
fn data(member: &str, value: &str) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// A parameter of the data OTHER than `member`, so a row can carry a second parameter beside
/// the one it drives.
fn anchor(member: &str) -> (String, String) {
    if member == "InstanceType" {
        data("ImageId", "ami-0fffffffffffffff0")
    } else {
        data("InstanceType", "t3.nano")
    }
}

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call that has to succeed, as the body it answered.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The id a create reported for the template it minted.
fn minted_id(case: &str, body: &str) -> String {
    super::ec2_query::element(body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must report the id it minted: {body}"))
}

/// The text the one reported version carries in `element`, and `None` when it reports no such
/// element at all — the two cases this suite has to tell apart, which is why the body is read
/// through the XML parser rather than by substring.
fn reported(case: &str, body: &str, element: &str) -> Option<String> {
    let parsed = document(case, body);
    let version = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "versionNumber").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no reported version in {body}"));
    let launch_data = child(version, "launchTemplateData")
        .unwrap_or_else(|| panic!("case {case:?}: the version reports no launch data: {body}"));
    child_text(launch_data, element)
}

/// The same reading of ONE version of a template, taken through
/// `DescribeLaunchTemplateVersions` — the projection a client reads back. `selector` is the
/// `LaunchTemplateVersion` the request names, which may be a number or one of the two aliases
/// the operation documents (`$Latest`, `$Default`).
async fn described(
    case: &str,
    router: &MultiServiceRouter,
    launch_template_id: &str,
    selector: &str,
    element: &str,
) -> Option<String> {
    let body = ok(
        case,
        router,
        "DescribeLaunchTemplateVersions",
        &[
            p("LaunchTemplateId", launch_template_id),
            p("LaunchTemplateVersion.1", selector),
        ],
    )
    .await;
    reported(case, &body, element)
}

/// How many versions the store holds for `launch_template_id` — read off the state itself, so
/// "nothing was stored" is a claim about the store rather than about a report.
fn versions_held(engine: &engine::InMemoryEngine, launch_template_id: &str) -> usize {
    engine
        .state::<Ec2State>()
        .lock()
        .launch_templates
        .get(launch_template_id)
        .map_or(0, |record| record.versions.len())
}

/// How many launch templates the account holds, for the same reason.
fn templates_held(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().launch_templates.len()
}

/// Every way of naming ONE version of a template that holds exactly one: its number and both
/// documented aliases (`DescribeLaunchTemplateVersions$Versions`: "$Latest" and "$Default").
const VERSION_SELECTORS: [&str; 3] = ["1", "$Latest", "$Default"];

/// A parameter sent with no text is stored and reported AS SENT: both creates keep it, and
/// every read of the stored version reports it carrying no text — while the same parameter
/// sent WITH text is reported with that text, which is what makes the rows about the value the
/// field carried and not about the member.
#[tokio::test]
async fn a_parameter_with_no_text_is_stored_and_reported_as_sent() {
    let (router, _engine) = super::ec2_query::emulator();

    for parameter in &TEXT_PARAMETERS {
        let TextParameter {
            member,
            element,
            value,
            ..
        } = *parameter;

        // The template's FIRST version, created with the parameter carrying no text. The
        // create reports the TEMPLATE (`CreateLaunchTemplateResult$LaunchTemplate`), so the
        // version it stored is read through the versions report — by its number and by each
        // documented alias, since one version is the latest and the default one at once.
        let case = format!("{member} with no text on CreateLaunchTemplate");
        let name = format!("no-text.{member}");
        let body = ok(
            &case,
            &router,
            "CreateLaunchTemplate",
            &[
                p("LaunchTemplateName", &name),
                anchor(member),
                data(member, ""),
            ],
        )
        .await;
        let launch_template_id = minted_id(&case, &body);
        for selector in VERSION_SELECTORS {
            let case = format!("{case}, read back by {selector}");
            let actual = described(&case, &router, &launch_template_id, selector, element).await;
            assert_eq!(
                actual.as_deref(),
                Some(""),
                "case {case:?}: expected the read-back version to report <{element}> carrying \
                 no text, got {actual:?}"
            );
        }

        // The SAME parameter with no text on a new version of that template.
        let case = format!("{member} with no text on CreateLaunchTemplateVersion");
        let body = ok(
            &case,
            &router,
            "CreateLaunchTemplateVersion",
            &[
                p("LaunchTemplateId", &launch_template_id),
                anchor(member),
                data(member, ""),
            ],
        )
        .await;
        let actual = reported(&case, &body, element);
        assert_eq!(
            actual.as_deref(),
            Some(""),
            "case {case:?}: expected <{element}> carrying no text on the created version, got \
             {actual:?} in {body}"
        );
        let actual = described(&case, &router, &launch_template_id, "2", element).await;
        assert_eq!(
            actual.as_deref(),
            Some(""),
            "case {case:?}: expected the read-back version to report <{element}> carrying no \
             text, got {actual:?}"
        );
        // Version 2 is the template's LATEST, and version 1 is still its DEFAULT, so the two
        // aliases now name versions that disagree about the parameter's text.
        let case = format!("{member} with text on CreateLaunchTemplateVersion");
        let body = ok(
            &case,
            &router,
            "CreateLaunchTemplateVersion",
            &[
                p("LaunchTemplateId", &launch_template_id),
                anchor(member),
                data(member, value),
            ],
        )
        .await;
        let actual = reported(&case, &body, element);
        assert_eq!(
            actual.as_deref(),
            Some(value),
            "case {case:?}: expected <{element}> to carry {value:?}, got {actual:?} in {body}"
        );
        for (selector, expected) in [("3", value), ("$Latest", value), ("$Default", "")] {
            let case = format!("{case}, read back by {selector}");
            let actual = described(&case, &router, &launch_template_id, selector, element).await;
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected the read-back version to report {expected:?}, got \
                 {actual:?}"
            );
        }
    }
}

/// How one row sends its parameter beside the `SourceVersion` it inherits from.
enum Sent {
    /// The field is sent carrying no text.
    NoText,
    /// The field is not sent at all.
    LeftOut,
    /// The field is sent carrying other text.
    OtherText,
}

/// A parameter sent with no text does not overwrite the one a `SourceVersion` contributes:
/// the new version inherits the source's value for it, exactly as a request that left the
/// parameter out does — while a parameter sent WITH text overwrites it.
#[tokio::test]
async fn a_parameter_with_no_text_does_not_overwrite_the_one_it_inherits() {
    let (router, _engine) = super::ec2_query::emulator();

    // One template whose first version carries every text parameter, so each row has
    // something of its own to inherit.
    let case = "a source version carrying every text parameter";
    let mut first = vec![p("LaunchTemplateName", "inherit.no-text")];
    for parameter in &TEXT_PARAMETERS {
        first.push(data(parameter.member, parameter.value));
    }
    let body = ok(case, &router, "CreateLaunchTemplate", &first).await;
    let launch_template_id = minted_id(case, &body);

    let rows = [
        ("sent with no text", Sent::NoText),
        ("left out", Sent::LeftOut),
        ("sent with other text", Sent::OtherText),
    ];

    let mut version_number = 1;
    for (sent_case, sent) in &rows {
        for parameter in &TEXT_PARAMETERS {
            let TextParameter {
                member,
                element,
                value,
                other,
            } = *parameter;
            let case = format!("{member} {sent_case}, inheriting from version 1");
            // `LaunchTemplateData` is `required`, so every row carries a parameter of its
            // own that no row asserts on — the row that leaves its member OUT still has to
            // send launch template data.
            let mut params = vec![
                p("LaunchTemplateId", &launch_template_id),
                p("SourceVersion", "1"),
                data("EbsOptimized", "true"),
            ];
            let expected = match sent {
                Sent::NoText => {
                    params.push(data(member, ""));
                    value
                }
                Sent::LeftOut => value,
                Sent::OtherText => {
                    params.push(data(member, other));
                    other
                }
            };
            let body = ok(&case, &router, "CreateLaunchTemplateVersion", &params).await;
            version_number += 1;
            let actual = reported(&case, &body, element);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected <{element}> to carry {expected:?}, got {actual:?} \
                 in {body}"
            );
            let actual = described(
                &case,
                &router,
                &launch_template_id,
                &version_number.to_string(),
                element,
            )
            .await;
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected the read-back version to report {expected:?}, \
                 got {actual:?}"
            );
        }
    }
}

/// A request that NAMES a launch parameter has given launch template data, even when the
/// field it named carries no text: both creates are served and store the version. A request
/// that names NO launch parameter at all is refused with EC2's own code for a parameter the
/// request left out, and stores nothing.
#[tokio::test]
async fn a_request_that_names_no_parameter_at_all_is_refused() {
    let (router, engine) = super::ec2_query::emulator();

    // A template to create the versions of, carrying a key name of its own to inherit.
    let case = "a template to create versions of";
    let body = ok(
        case,
        &router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "no-text.names-one"),
            data("ImageId", "ami-00000000000000001"),
            data("KeyName", "deployment-key"),
        ],
    )
    .await;
    let launch_template_id = minted_id(case, &body);
    let mut held = templates_held(&engine);
    let mut versions = versions_held(&engine, &launch_template_id);

    for parameter in &TEXT_PARAMETERS {
        let member = parameter.member;

        // ONE parameter, carrying no text: the request named a launch parameter, so the
        // create is served and the template is stored.
        let case = format!("CreateLaunchTemplate naming only an empty {member}");
        let name = format!("names-one.{member}");
        let body = ok(
            &case,
            &router,
            "CreateLaunchTemplate",
            &[p("LaunchTemplateName", &name), data(member, "")],
        )
        .await;
        let minted = minted_id(&case, &body);
        held += 1;
        assert_eq!(
            templates_held(&engine),
            held,
            "case {case:?}: the served create must store the template"
        );
        let actual = described(&case, &router, &minted, "$Latest", parameter.element).await;
        assert_eq!(
            actual.as_deref(),
            Some(""),
            "case {case:?}: expected the stored version to report <{}> carrying no text, got \
             {actual:?}",
            parameter.element
        );

        let case = format!("CreateLaunchTemplateVersion naming only an empty {member}");
        let _ = ok(
            &case,
            &router,
            "CreateLaunchTemplateVersion",
            &[p("LaunchTemplateId", &launch_template_id), data(member, "")],
        )
        .await;
        versions += 1;
        assert_eq!(
            versions_held(&engine, &launch_template_id),
            versions,
            "case {case:?}: the served create must store the version"
        );
    }

    // A request whose launch template data names NO parameter at all: neither create has any
    // launch parameter to store, and each is refused with the code EC2 reports for a
    // parameter the request left out.
    let case = "CreateLaunchTemplate naming no launch parameter at all";
    let (status, body) = api(
        &router,
        "CreateLaunchTemplate",
        &[p("LaunchTemplateName", "names-none")],
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "MissingParameter"),
        "case {case:?}: expected a 400 MissingParameter, got {status} with body {body}"
    );
    assert_eq!(
        templates_held(&engine),
        held,
        "case {case:?}: a refused create stores no launch template"
    );

    let case = "CreateLaunchTemplateVersion naming no launch parameter at all";
    let (status, body) = api(
        &router,
        "CreateLaunchTemplateVersion",
        &[p("LaunchTemplateId", &launch_template_id)],
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "MissingParameter"),
        "case {case:?}: expected a 400 MissingParameter, got {status} with body {body}"
    );
    assert_eq!(
        versions_held(&engine, &launch_template_id),
        versions,
        "case {case:?}: a refused create stores no version"
    );

    // The same request naming a SOURCE version: the parameter it sends carries no text, so
    // the version it stores inherits the source's parameters whole.
    let case = "an empty parameter beside a source version";
    let body = ok(
        case,
        &router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", &launch_template_id),
            p("SourceVersion", "1"),
            data("KeyName", ""),
        ],
    )
    .await;
    let inherited = (
        reported(case, &body, "keyName"),
        reported(case, &body, "imageId"),
    );
    assert_eq!(
        inherited,
        (
            Some("deployment-key".to_string()),
            Some("ami-00000000000000001".to_string())
        ),
        "case {case:?}: expected the source's parameters to be inherited whole, got \
         {inherited:?} in {body}"
    );
    assert_eq!(
        versions_held(&engine, &launch_template_id),
        versions + 1,
        "case {case:?}: the inherited version is stored"
    );
}
