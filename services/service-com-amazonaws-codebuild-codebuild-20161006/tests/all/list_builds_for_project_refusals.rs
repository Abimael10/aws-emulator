// SPDX-License-Identifier: CC0-1.0

//! Cover for how an operation that names ONE project REFUSES a request it cannot answer,
//! driven over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1.
//!
//! The rules under test:
//!
//! * a request that carries NO value in the member naming the one project it acts on is
//!   refused with 400 `InvalidInputException` stating the two FORMS that member takes —
//!   fixed text that interpolates no member name, no value and no count, so the caller is
//!   never handed the shared constraint envelope for that condition;
//! * a project identifier written as a project ARN of this service whose region field names
//!   a Region other than the one the request is signed for is an INPUT error, refused with
//!   400 `InvalidInputException` and fixed text naming neither the ARN nor either Region. It
//!   is decided from the value and the request's own Region alone — before anything is
//!   looked up, so it is stated whether or not a project of that name is held — and in
//!   either direction, so neither Region is privileged;
//! * a refusal that quotes the project asked for quotes the identifier the request SENT: an
//!   identifier that is already an ARN is never interpolated into an ARN template, which
//!   would state an ARN inside an ARN.
//!
//! The harness is the one the suites of this crate share
//! (`super::project_build_lifecycle`), reached through its per-Region `call_in`, with the two
//! readers of a project refusal the sibling `super::project_arn_addressing` states.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::project_arn_addressing::{message, project_arn};
use super::project_build_lifecycle::{
    call, call_in, create_project_body, emulator, error_type, listed_ids, REGION,
};

/// What CodeBuild states as the reason a request addressing ONE project carries no project
/// identifier. Fixed text in full — recorded behaviour, as the model states no message for
/// `InvalidInputException`.
const PROJECT_IDENTIFIER_REQUIRED: &str = "Project name or ARN is required";

/// What CodeBuild states as the reason a project ARN belongs to another Region. Fixed text
/// in full — recorded behaviour, as the model states no message for the error.
const CROSS_REGION_ARN_REFUSAL: &str = "Invalid project ARN: region does not match caller's region";

/// Every operation of this service that names ONE project through a member carrying the
/// project's NAME or its ARN — `projectName`, `@required` and targeting `NonEmptyString`
/// rather than the patterned `ProjectName`
/// (`smithy_query(query="operation", operation="StartBuild,StartBuildBatch,InvalidateProjectCache,ListBuildsForProject,ListSandboxesForProject")`).
///
/// Spans both halves of that set: the listings that REPORT on a project and the operations
/// that ACT on one, so the sentence is a fact about the member rather than about one
/// operation. Three of the five are named by no recording of this condition.
const PROJECT_IDENTIFIER_OPERATIONS: [&str; 5] = [
    "ListBuildsForProject",
    "ListSandboxesForProject",
    "StartBuild",
    "StartBuildBatch",
    "InvalidateProjectCache",
];

/// Regions these requests are signed for, from the AWS General Reference's Region table: the
/// Region the shared harness signs for, two other commercial Regions and a recently added
/// one.
const SIGNING_REGIONS: [&str; 4] = [REGION, "eu-west-3", "us-east-2", "il-central-1"];

/// Create a minimal NO_SOURCE project, signed for `region` — so the project is held in that
/// Region, which is the Region of the ARN it carries.
async fn create_project_in(router: &MultiServiceRouter, region: &str, name: &str) {
    let (status, body) = call_in(
        router,
        region,
        "CreateProject",
        create_project_body(name, json!({})),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateProject {name:?} in {region}: expected 200, got {status} with {body}"
    );
}

/// Start a build of `project`, signed for `region`, and hand back its id.
async fn start_build_in(router: &MultiServiceRouter, region: &str, project: &str) -> String {
    let (status, body) = call_in(
        router,
        region,
        "StartBuild",
        json!({"projectName": project}),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartBuild {project:?} in {region}: expected 200, got {status} with {body}"
    );
    body["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartBuild {project:?} in {region}: expected a build id, got {body}"))
        .to_string()
}

/// Assert the response refused the request as invalid input, and hand back the sentence it
/// stated.
fn stated_invalid_input(case: &str, status: u16, body: &Value) -> String {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_type(body),
        "InvalidInputException",
        "case {case:?}: expected the modelled input error, got {status} with {body}"
    );
    message(body)
}

/// RULE: a request that carries no value in the member naming the one project it acts on is
/// refused with CodeBuild's own fixed sentence, which states the two forms that member takes
/// and interpolates nothing from the request.
///
/// The table spans every operation whose `projectName` is required and takes a name or an
/// ARN, crossed with the Regions a request may be signed for, and then the other members a
/// listing's request may carry beside the missing one.
#[tokio::test]
async fn a_request_naming_no_project_states_what_the_identifier_member_takes() {
    let router = emulator();

    for operation in PROJECT_IDENTIFIER_OPERATIONS {
        for region in SIGNING_REGIONS {
            let case = format!("{operation} with no projectName, signed for {region}");
            let (status, response) = call_in(&router, region, operation, json!({})).await;
            let stated = stated_invalid_input(&case, status, &response);
            assert_eq!(
                stated, PROJECT_IDENTIFIER_REQUIRED,
                "case {case:?}: expected CodeBuild's sentence naming both forms, got {stated:?}"
            );
            // Fixed text in full: nothing of the request, of the member or of the constraint
            // walk appears in it.
            for leaked in ["projectName", "constraint", "null", operation, region] {
                assert!(
                    !stated.contains(leaked),
                    "case {case:?}: the sentence must not state {leaked:?}, got {stated:?}"
                );
            }
        }
    }

    // The same sentence whatever else the listing's request carries: the condition is the
    // missing identifier, not the shape of the rest of the body.
    for (form, body) in [
        ("a sort order beside it", json!({"sortOrder": "ASCENDING"})),
        ("a next token beside it", json!({"nextToken": "whatever"})),
        (
            "both beside it",
            json!({"sortOrder": "DESCENDING", "nextToken": "whatever"}),
        ),
    ] {
        let case = format!("ListBuildsForProject with {form}");
        let (status, response) = call(&router, "ListBuildsForProject", body).await;
        let stated = stated_invalid_input(&case, status, &response);
        assert_eq!(
            stated, PROJECT_IDENTIFIER_REQUIRED,
            "case {case:?}: expected CodeBuild's sentence naming both forms, got {stated:?}"
        );
    }
}

/// RULE: a project identifier that is a project ARN of this service naming a Region other
/// than the one the request is signed for is an input error, refused with CodeBuild's own
/// fixed cross-Region sentence — in either direction, and before any lookup, so a project
/// the account does not hold is refused on the same ground as one it does.
#[tokio::test]
async fn a_project_arn_of_another_region_is_an_input_error_whichever_region_is_signed() {
    let router = emulator();
    create_project_in(&router, REGION, "cross-region-proj").await;

    for signed in SIGNING_REGIONS {
        for arn_region in SIGNING_REGIONS {
            if arn_region == signed {
                continue;
            }
            // Both a project this account holds and one it never created: the answer is a
            // function of the value and the signed Region, so the store cannot change it.
            for project in ["cross-region-proj", "never-created-proj"] {
                let arn = project_arn(arn_region, project);
                let case =
                    format!("ListBuildsForProject for a {arn_region} ARN signed for {signed}");
                let (status, response) = call_in(
                    &router,
                    signed,
                    "ListBuildsForProject",
                    json!({"projectName": arn}),
                )
                .await;
                let stated = stated_invalid_input(&case, status, &response);
                assert_eq!(
                    stated, CROSS_REGION_ARN_REFUSAL,
                    "case {case:?}: expected CodeBuild's cross-Region sentence, got {stated:?}"
                );
                // Fixed text in full: it names neither Region, the ARN, nor the project.
                for leaked in ["arn:", signed, arn_region, project] {
                    assert!(
                        !stated.contains(leaked),
                        "case {case:?}: the sentence must not state {leaked:?}, got {stated:?}"
                    );
                }
            }
        }
    }
}

/// RULE: a project ARN of the signed Region resolves on its resource name, and an ARN of the
/// signed Region naming no project the account holds is the modelled missing resource,
/// quoting the identifier the request sent — ONCE, never spliced into an ARN template.
///
/// One project per signed Region, each read back both by its name and by its own ARN, so the
/// refusals above are about the identifier and not about the operation.
#[tokio::test]
async fn a_project_arn_of_the_signed_region_resolves_and_is_quoted_as_sent() {
    let router = emulator();

    for signed in SIGNING_REGIONS {
        // A project held in the Region the request is signed for, holding one build.
        let project = format!("own-arn-proj-{signed}");
        create_project_in(&router, signed, &project).await;
        let build = start_build_in(&router, signed, &project).await;

        for (form, identifier) in [
            ("its name", project.clone()),
            ("its own ARN", project_arn(signed, &project)),
        ] {
            let case = format!("ListBuildsForProject by {form} signed for {signed}");
            let (status, response) = call_in(
                &router,
                signed,
                "ListBuildsForProject",
                json!({"projectName": identifier}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {response}"
            );
            assert_eq!(
                listed_ids(&response),
                vec![build.clone()],
                "case {case:?}: expected the build the project holds, got {response}"
            );
        }

        // An ARN of the signed Region naming no project this account holds.
        let absent = project_arn(signed, "never-created-proj");
        let case = format!("ListBuildsForProject for an absent {signed} ARN");
        let (status, response) = call_in(
            &router,
            signed,
            "ListBuildsForProject",
            json!({"projectName": absent.clone()}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {response}"
        );
        assert_eq!(
            error_type(&response),
            "ResourceNotFoundException",
            "case {case:?}: a project the account does not hold is absent, got {response}"
        );
        let stated = message(&response);
        assert_eq!(
            stated,
            format!("The provided project {absent} does not exist."),
            "case {case:?}: expected the identifier quoted as sent, got {stated:?}"
        );
        assert_eq!(
            stated.matches("arn:").count(),
            1,
            "case {case:?}: the sentence must state exactly one ARN, got {stated:?}"
        );
    }
}
