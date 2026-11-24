// SPDX-License-Identifier: CC0-1.0

//! The launch parameters a launch-template write SPECIFIED, as against the form fields it
//! sent.
//!
//! The rule under test: EC2 speaks `ec2Query`, where a request's parameters are
//! `application/x-www-form-urlencoded` fields, so a field carrying no text cannot be told
//! from a field the request never sent. A launch parameter supplied with no text is therefore
//! a parameter the request DID NOT SPECIFY: the stored version carries nothing for it, every
//! read reports nothing for it, a version that names a `SourceVersion` inherits the source's
//! value for it rather than blanking it ("if you specify additional launch parameters for the
//! new version, they overwrite any corresponding launch parameters inherited from the source
//! version", `CreateLaunchTemplateVersionRequest$SourceVersion`), and a request whose every
//! parameter carries no text has specified none at all — "you must specify at least one
//! parameter for the launch template data"
//! (`smithy_query(query="shape",shape="RequestLaunchTemplateData")`).
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
/// `ImageId`, `InstanceType`, `KeyName`, `RamDiskId` and `UserData` — so the parameter a
/// client's key-pair fix spells is one row of six rather than the subject.
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

/// A parameter of the data OTHER than `member`, so a request that sends `member` with no text
/// still specifies something: the data has to carry at least one parameter.
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
/// `DescribeLaunchTemplateVersions` — the projection a client reads back.
async fn described(
    case: &str,
    router: &MultiServiceRouter,
    launch_template_id: &str,
    version_number: i64,
    element: &str,
) -> Option<String> {
    let body = ok(
        case,
        router,
        "DescribeLaunchTemplateVersions",
        &[
            p("LaunchTemplateId", launch_template_id),
            p("LaunchTemplateVersion.1", &version_number.to_string()),
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

/// A parameter sent with no text is not specified: neither create stores it, and neither the
/// create's own body nor the read-back reports it — while the same parameter sent WITH text is
/// carried, which is what makes the first half a rule about the text and not about the member.
#[tokio::test]
async fn a_parameter_with_no_text_is_not_specified_by_either_create() {
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
        // version it stored is read through the versions report.
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
        let actual = described(&case, &router, &launch_template_id, 1, element).await;
        assert_eq!(
            actual, None,
            "case {case:?}: expected the read-back version to report no <{element}>, \
             got {actual:?}"
        );

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
            actual, None,
            "case {case:?}: expected no <{element}> on the created version, got {actual:?} \
             in {body}"
        );
        let actual = described(&case, &router, &launch_template_id, 2, element).await;
        assert_eq!(
            actual, None,
            "case {case:?}: expected the read-back version to report no <{element}>, \
             got {actual:?}"
        );

        // The control: with TEXT the same parameter is stored and reported, so the rows above
        // are about the text the field carried and not about the member being dropped.
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
        let actual = described(&case, &router, &launch_template_id, 3, element).await;
        assert_eq!(
            actual.as_deref(),
            Some(value),
            "case {case:?}: expected the read-back version to report {value:?}, got {actual:?}"
        );
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
            let actual =
                described(&case, &router, &launch_template_id, version_number, element).await;
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected the read-back version to report {expected:?}, \
                 got {actual:?}"
            );
        }
    }
}

/// A request whose every launch parameter carries no text has specified NONE: both creates
/// refuse it with EC2's own code for a parameter the request left out and store nothing —
/// while the same request against a `SourceVersion` is a version OF THAT SOURCE's parameters,
/// which is a version that carries one.
#[tokio::test]
async fn a_request_whose_every_parameter_carries_no_text_specifies_none() {
    let (router, engine) = super::ec2_query::emulator();

    // A template to create the versions of, carrying a key name of its own to inherit.
    let case = "a template to refuse versions of";
    let body = ok(
        case,
        &router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "no-text.specifies-none"),
            data("ImageId", "ami-00000000000000001"),
            data("KeyName", "deployment-key"),
        ],
    )
    .await;
    let launch_template_id = minted_id(case, &body);
    let held = templates_held(&engine);

    for parameter in &TEXT_PARAMETERS {
        let member = parameter.member;

        let case = format!("CreateLaunchTemplate specifying only an empty {member}");
        let (status, body) = api(
            &router,
            "CreateLaunchTemplate",
            &[
                p("LaunchTemplateName", &format!("specifies-none.{member}")),
                data(member, ""),
            ],
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

        let case = format!("CreateLaunchTemplateVersion specifying only an empty {member}");
        let (status, body) = api(
            &router,
            "CreateLaunchTemplateVersion",
            &[p("LaunchTemplateId", &launch_template_id), data(member, "")],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "MissingParameter"),
            "case {case:?}: expected a 400 MissingParameter, got {status} with body {body}"
        );
        assert_eq!(
            versions_held(&engine, &launch_template_id),
            1,
            "case {case:?}: a refused create stores no version"
        );
    }

    // The same request naming a SOURCE version: the parameters it specifies are the source's,
    // so the version is created and carries them.
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
        2,
        "case {case:?}: the inherited version is stored"
    );
}
