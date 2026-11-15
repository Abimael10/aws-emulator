// SPDX-License-Identifier: CC0-1.0

//! The precondition an assessment's `appVersion` has to satisfy, and how
//! `StartAppAssessment` words the refusal when it does not — end to end over HTTP.
//!
//! The rule under test: an assessment addresses an app version by one of two labels
//! — the application's draft or its current release — so a label that is neither,
//! and the release alias of an application that has published nothing, addresses no
//! version of that application and is refused with `ResourceNotFoundException` /
//! HTTP 404 whose `message` states the label asked for, the application (by the
//! `name` it is stored under and by the `appArn` the request carried) and the
//! precondition the label has to satisfy.
//!
//! The rows span the FORMS such a label takes — the alias before a publish, a
//! publish sequence number (minted and never minted), the two labels in another
//! case, a label merely containing the alias, and the shortest and longest label
//! `EntityVersion`'s pattern `^\S{1,50}$` admits — over more than one application,
//! so the name and the ARN in the sentence are read from the application the
//! request names rather than from anything the request itself carries.
//!
//! The harness (`emulator`, `post`, `Reply`) is the one `application_lifecycle`
//! drives every request of this crate's suites through: the real wire path,
//! `MultiServiceRouter::dispatch` with a restJson1 request.

use serde_json::json;

use super::application_lifecycle::{
    emulator, invalid_app_version_sentence as invalid_app_version, post, Reply, ACCOUNT, REGION,
};
use smithy_server_runtime::MultiServiceRouter;

/// Create an application named `name` and answer its ARN.
async fn create_an_app(router: &MultiServiceRouter, name: &str) -> String {
    let reply = post(router, "/create-app", &json!({"name": name})).await;
    reply.ok(name)["app"]["appArn"]
        .as_str()
        .expect("a minted appArn")
        .to_string()
}

/// Map one resource into `app_arn`'s draft, which is what makes the draft
/// publishable.
async fn map_a_resource(router: &MultiServiceRouter, app_arn: &str, identifier: &str) {
    let reply = post(
        router,
        "/create-app-version-resource",
        &json!({
            "appArn": app_arn,
            "resourceName": identifier,
            "logicalResourceId": {"identifier": identifier},
            "physicalResourceId": identifier,
            "resourceType": "AWS::S3::Bucket",
            "appComponents": ["appcommon"],
        }),
    )
    .await;
    reply.ok(identifier);
}

/// Publish `app_arn`'s draft as its new current release.
async fn publish(router: &MultiServiceRouter, app_arn: &str) {
    let reply = post(router, "/publish-app-version", &json!({"appArn": app_arn})).await;
    reply.ok("publish");
}

/// `StartAppAssessment` at one `appVersion` of one application.
async fn start_assessment(
    router: &MultiServiceRouter,
    app_arn: &str,
    app_version: &str,
    assessment_name: &str,
) -> Reply {
    post(
        router,
        "/start-app-assessment",
        &json!({
            "appArn": app_arn,
            "appVersion": app_version,
            "assessmentName": assessment_name,
        }),
    )
    .await
}

/// The longest label `EntityVersion`'s pattern `^\S{1,50}$` admits
/// (`smithy_query(query="operation",operation="StartAppAssessment")`: `appVersion`
/// targets `EntityVersion`), and the shortest.
const LONGEST_VERSION_LABEL: &str = "v9999999999999999999999999999999999999999999999999";
const SHORTEST_VERSION_LABEL: &str = "v";

/// Every form of `appVersion` that addresses no version of the application is
/// refused with the modelled 404 stating the label, the application and the
/// precondition — for an application that has published a release and for one that
/// has not, and with each application's OWN name in the sentence.
#[tokio::test]
async fn an_app_version_that_addresses_no_version_is_refused_with_the_precondition() {
    assert_eq!(
        LONGEST_VERSION_LABEL.len(),
        50,
        "the longest label must sit exactly on `EntityVersion`'s bound, got {}",
        LONGEST_VERSION_LABEL.len()
    );
    let router = emulator();

    // Two applications, one with a frozen release and one with nothing published,
    // so the sentence is asserted against two different stored names and two
    // different ARNs.
    let published_name = "precondition-published-app";
    let published = create_an_app(&router, published_name).await;
    map_a_resource(&router, &published, "first-bucket").await;
    publish(&router, &published).await;
    map_a_resource(&router, &published, "second-bucket").await;
    publish(&router, &published).await;

    let unpublished_name = "precondition-unpublished-app";
    let unpublished = create_an_app(&router, unpublished_name).await;

    // The rows differ along the FORM of the label. `applies_to_published` is false
    // for the release alias alone, which does address a version of the application
    // that published one.
    let rows: &[(&str, &str, bool)] = &[
        ("the release alias", "release", false),
        ("the first publish sequence number", "1", true),
        ("a publish sequence number nothing minted", "99", true),
        ("the alias in another case", "RELEASE", true),
        ("the draft label in another case", "Draft", true),
        ("a label merely containing the alias", "release-2", true),
        ("the shortest label the pattern admits", SHORTEST_VERSION_LABEL, true),
        ("the longest label the pattern admits", LONGEST_VERSION_LABEL, true),
    ];

    for (form, app_version, applies_to_published) in rows {
        let mut applications = vec![(unpublished_name, unpublished.as_str())];
        if *applies_to_published {
            applications.push((published_name, published.as_str()));
        }
        for (app_name, app_arn) in applications {
            let case = format!("{form} / {app_name}");
            let expected = invalid_app_version(app_version, app_name, app_arn);
            let refused = start_assessment(&router, app_arn, app_version, "a-refused-run").await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_code.as_str(),
                    refused.body["message"].as_str().unwrap_or_default(),
                    refused.body["resourceType"].as_str().unwrap_or_default(),
                    refused.body["resourceId"].as_str().unwrap_or_default(),
                ),
                (
                    404,
                    "ResourceNotFoundException",
                    expected.as_str(),
                    "AppVersion",
                    *app_version,
                ),
                "case {case:?}: expected HTTP 404 ResourceNotFoundException {expected:?}, \
                 got {} {:?} {:?}",
                refused.status,
                refused.error_code,
                refused.body
            );
        }
    }
}

/// The refusal above is about the LABEL: the release alias of an application that
/// published one is assessed, the draft addresses a version and is refused as
/// unassessable instead, and an application nothing was stored under is the
/// application-level refusal.
#[tokio::test]
async fn the_precondition_refusal_is_about_the_label_and_not_the_application() {
    let router = emulator();
    let app_name = "precondition-contrast-app";
    let app_arn = create_an_app(&router, app_name).await;
    map_a_resource(&router, &app_arn, "only-bucket").await;
    publish(&router, &app_arn).await;

    let started = start_assessment(&router, &app_arn, "release", "an-accepted-run").await;
    let assessment = started.ok("the release alias of an application that published one");
    assert_eq!(
        assessment["assessment"]["appArn"].as_str(),
        Some(app_arn.as_str()),
        "case \"the release alias of an application that published one\": the \
         assessment must be of that application, got {assessment:?}"
    );

    // The draft is a version the application HAS, so it is not refused with the
    // precondition sentence — it is refused as a version this operation does not
    // assess.
    let draft = start_assessment(&router, &app_arn, "draft", "a-draft-run").await;
    draft.refused("the draft", 400, "ValidationException");
    assert_ne!(
        draft.body["message"].as_str().unwrap_or_default(),
        invalid_app_version("draft", app_name, &app_arn).as_str(),
        "case \"the draft\": a version the application has must not be refused with \
         the precondition sentence, got {:?}",
        draft.body
    );

    let never_created =
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:app/00000000-0000-0000-0000-000000000000");
    let unknown = start_assessment(&router, &never_created, "release", "an-unknown-app-run").await;
    assert_eq!(
        (
            unknown.status,
            unknown.error_code.as_str(),
            unknown.body["message"].as_str().unwrap_or_default()
        ),
        (
            404,
            "ResourceNotFoundException",
            format!("App not found for appArn {never_created}").as_str()
        ),
        "case \"an application that was never created\": the refusal must name the \
         application, got {} {:?}",
        unknown.status,
        unknown.body
    );
}
