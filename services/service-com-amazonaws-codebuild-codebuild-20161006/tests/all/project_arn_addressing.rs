// SPDX-License-Identifier: CC0-1.0

//! Cover for how a project identifier that is an ARN is READ, driven over the wire the
//! router serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>` on
//! awsJson1_1.
//!
//! The rule under test: **a project identifier member the model types as a plain string
//! takes a project's NAME or its ARN, so a value written as a project ARN of this service
//! is PARSED before anything is looked up — its region field must name an AWS Region
//! (else the ARN is refused on that field) and must be the Region the request is signed
//! for (else `InvalidInputException` with CodeBuild's cross-Region sentence), its account
//! field must be the caller's (else `AccessDeniedException`, the authorization refusal, in
//! the common-error envelope's capital-`Message`), and anything else resolves on the
//! ARN's resource name exactly as the bare name does — and no refusal ever interpolates an
//! already-ARN identifier into an ARN template.**
//!
//! The tables span the FAMILY of operations that report on a project through such a member
//! — the batch read of projects and the three per-project listings, whose `names` /
//! `projectName` all target `NonEmptyString` — crossed with the forms an identifier takes:
//! a bare name, the project's own ARN, an ARN of another Region, of a value that is no
//! Region, of another account, of another partition, of another resource kind, and of a
//! project this account never created. Regions come from the published Region table, and
//! include ones no recording cites.

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state;
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{call, create_project, emulator, error_type, REGION};

/// The project every table below addresses, created once per router.
const HELD_PROJECT: &str = "arn-addressed-proj";

/// An account that is not the one this emulator serves.
const ANOTHER_ACCOUNT: &str = "210987654321";

/// The `message` an error body carries — the member every error shape this service's own
/// model declares states its sentence in.
///
/// `pub(crate)` here and on [`project_arn`]: the sibling `crate::list_builds_for_project_refusals`
/// reads the same two values of a refusal, so the readers are written once.
pub(crate) fn message(body: &Value) -> String {
    body["message"].as_str().unwrap_or_default().to_string()
}

/// The `Message` an error body carries — the member the AWS common-error envelope states
/// its sentence in, which is where an authorization refusal carries it.
fn capital_message(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// A project ARN naming `name` in `region` and `account`.
fn project_arn_in(partition: &str, region: &str, account: &str, name: &str) -> String {
    format!("arn:{partition}:codebuild:{region}:{account}:project/{name}")
}

/// A project ARN of this emulator's own account, in `region`.
pub(crate) fn project_arn(region: &str, name: &str) -> String {
    project_arn_in("aws", region, engine::EMULATOR_ACCOUNT_ID, name)
}

/// One operation that REPORTS on a project named by a plain-string identifier: how a
/// request of it is written, and how its success is read.
struct ProjectReader {
    /// The operation's name, as `X-Amz-Target` names it.
    name: &'static str,
    /// A request of this operation addressing `identifier`.
    request: fn(&str) -> Value,
}

/// Every operation of this service that reports on ONE project addressed by a
/// plain-string identifier: `BatchGetProjectsInput$names` ("The names or ARNs of the build
/// projects") and the `projectName` of the three per-project listings, all of which target
/// `NonEmptyString` rather than the patterned `ProjectName`
/// (`smithy_query(query="operation", operation="BatchGetProjects,ListBuildsForProject")`).
fn project_readers() -> Vec<ProjectReader> {
    vec![
        ProjectReader {
            name: "BatchGetProjects",
            request: |identifier| json!({"names": [identifier]}),
        },
        ProjectReader {
            name: "ListBuildsForProject",
            request: |identifier| json!({"projectName": identifier}),
        },
        ProjectReader {
            name: "ListBuildBatchesForProject",
            request: |identifier| json!({"projectName": identifier}),
        },
        ProjectReader {
            name: "ListSandboxesForProject",
            request: |identifier| json!({"projectName": identifier}),
        },
    ]
}

/// A router holding [`HELD_PROJECT`].
async fn emulator_holding_a_project() -> MultiServiceRouter {
    let router = emulator();
    create_project(&router, HELD_PROJECT, json!({})).await;
    router
}

/// Regions that ARE Regions but not the one these requests are signed for, from the AWS
/// General Reference's Region table: another commercial Region, a recently added one, one
/// of a partitioned Region's codes, and a two-group area — none of them the Region any
/// recording of this cluster cites.
fn other_regions() -> Vec<&'static str> {
    vec![
        "eu-west-3",
        "ap-southeast-7",
        "us-gov-west-1",
        "cn-northwest-1",
        "us-isob-east-1",
        "il-central-1",
    ]
}

/// Region fields that name no Region at all: a code shaped like one whose number no Region
/// carries, an Availability Zone, an area alone, the literal placeholder, and the empty
/// field.
fn no_regions() -> Vec<&'static str> {
    vec!["us-east-9", "eu-west-9", "us-west-2a", "us", "region", ""]
}

#[tokio::test]
async fn a_project_arn_of_another_region_is_refused_as_invalid_input() {
    let router = emulator_holding_a_project().await;

    for reader in project_readers() {
        for region in other_regions() {
            let arn = project_arn(region, HELD_PROJECT);
            let case = format!("{} addressed in {region}", reader.name);
            let (status, body) = call(&router, reader.name, (reader.request)(&arn)).await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_type(&body),
                "InvalidInputException",
                "case {case:?}: a cross-Region ARN is an input error, got {status} with {body}"
            );
            assert_eq!(
                message(&body),
                "Invalid project ARN: region does not match caller's region",
                "case {case:?}: expected CodeBuild's cross-Region sentence, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_project_arn_whose_region_field_names_no_region_is_refused_on_that_field() {
    let router = emulator_holding_a_project().await;

    for reader in project_readers() {
        for region in no_regions() {
            let arn = project_arn(region, HELD_PROJECT);
            let case = format!("{} with the region field {region:?}", reader.name);
            let (status, body) = call(&router, reader.name, (reader.request)(&arn)).await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_type(&body),
                "InvalidInputException",
                "case {case:?}: a malformed field is an input error, got {status} with {body}"
            );
            assert_eq!(
                message(&body),
                format!("Invalid ARN: region {region} is invalid"),
                "case {case:?}: expected the invalid-field sentence naming that field, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_project_arn_of_another_account_is_denied_rather_than_reported_absent() {
    let router = emulator_holding_a_project().await;
    // The principal this emulator's credential speaks for, read from the shared
    // definition of that identity rather than spelled here.
    let principal = service_common::caller_principal::ambient_user_arn();

    for reader in project_readers() {
        // Both an account that holds the same project name and one that holds nothing:
        // the answer cannot depend on what the other account has, since this account
        // cannot see it.
        for account in [ANOTHER_ACCOUNT, "111122223333"] {
            let arn = project_arn_in("aws", REGION, account, HELD_PROJECT);
            let case = format!("{} addressed in account {account}", reader.name);
            let (status, body) = call(&router, reader.name, (reader.request)(&arn)).await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_type(&body),
                "AccessDeniedException",
                "case {case:?}: another account's project is an authorization failure, got \
                 {status} with {body}"
            );
            assert_eq!(
                capital_message(&body),
                format!(
                    "User: {principal} is not authorized to perform: codebuild:{} on resource: \
                     {arn} because no resource-based policy allows the codebuild:{} action",
                    reader.name, reader.name
                ),
                "case {case:?}: expected the resource-policy refusal naming the principal, the \
                 action and the ARN asked for, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_held_project_is_reported_through_its_own_arn() {
    let router = emulator_holding_a_project().await;
    let arn = project_arn(REGION, HELD_PROJECT);

    // The batch read reports the project itself, and nothing in its absent half.
    let (status, body) = call(&router, "BatchGetProjects", json!({"names": [arn.clone()]})).await;
    assert_eq!(
        status, 200,
        "BatchGetProjects by ARN: expected 200, got {status} with {body}"
    );
    assert_eq!(
        body["projects"][0]["name"],
        json!(HELD_PROJECT),
        "BatchGetProjects by ARN: expected the project itself, got {body}"
    );
    assert_eq!(
        body["projectsNotFound"],
        json!([]),
        "BatchGetProjects by ARN: nothing was absent, got {body}"
    );

    // Each per-project listing reports its own empty listing for a project that holds
    // none of those records yet — present and empty, not a refusal.
    for reader in project_readers() {
        if reader.name == "BatchGetProjects" {
            continue;
        }
        let case = format!("{} by ARN", reader.name);
        let (status, body) = call(&router, reader.name, (reader.request)(&arn)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["ids"],
            json!([]),
            "case {case:?}: expected the empty listing, got {body}"
        );
    }
}

#[tokio::test]
async fn an_identifier_that_is_no_project_arn_of_this_account_resolves_to_nothing() {
    let router = emulator_holding_a_project().await;

    // Values that name no project this account holds, each still of the caller's own
    // Region and account where it has those fields at all — so none of them is refused on
    // a field. The third column is whether the BATCH READ answers it from the store: it
    // does for a value that carries a project NAME (its own, or the resource name of a
    // project ARN of this service), and refuses the ones that carry none, because an ARN
    // that is no project ARN of this service is the NAME it is and an ARN's separators are
    // not a project name's characters (`name_character_refusals`).
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "an ARN of another partition",
            project_arn_in("aws-us-gov", REGION, engine::EMULATOR_ACCOUNT_ID, HELD_PROJECT),
            true,
        ),
        (
            "an ARN of another resource kind",
            format!(
                "arn:aws:codebuild:{REGION}:{}:report-group/{HELD_PROJECT}",
                engine::EMULATOR_ACCOUNT_ID
            ),
            false,
        ),
        (
            "an ARN of another service",
            format!(
                "arn:aws:codepipeline:{REGION}:{}:{HELD_PROJECT}",
                engine::EMULATOR_ACCOUNT_ID
            ),
            false,
        ),
        (
            "an ARN no project is held under",
            project_arn(REGION, "never-created"),
            true,
        ),
        (
            "a name no project is held under",
            "never-created".to_string(),
            true,
        ),
    ];

    for (case, identifier, answered_from_the_store) in rows {
        let (status, body) = call(
            &router,
            "BatchGetProjects",
            json!({"names": [identifier.clone()]}),
        )
        .await;
        if answered_from_the_store {
            // The batch read reports it in its absent half, exactly as sent.
            assert_eq!(
                status, 200,
                "case {case:?}: BatchGetProjects expected 200, got {status} with {body}"
            );
            assert_eq!(
                body["projectsNotFound"],
                json!([identifier.clone()]),
                "case {case:?}: expected the identifier reported absent as sent, got {body}"
            );
            assert_eq!(
                body["projects"],
                json!([]),
                "case {case:?}: nothing resolves, got {body}"
            );
        } else {
            // The batch read refuses the name such a value carries, in the one sentence
            // CodeBuild states for a name it cannot be written with.
            assert_eq!(
                status, 400,
                "case {case:?}: BatchGetProjects expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_type(&body),
                "InvalidInputException",
                "case {case:?}: a value that is no project name is an input error, got {body}"
            );
            assert_eq!(
                message(&body),
                state::INVALID_PROJECT_NAME_MESSAGE,
                "case {case:?}: expected CodeBuild's invalid-name sentence, got {body}"
            );
        }

        // A per-project listing refuses it as a project it does not hold, quoting the ARN
        // the identifier names — ONCE: an identifier that is already an ARN is never
        // interpolated into an ARN template, which would state an ARN inside an ARN.
        for reader in project_readers() {
            if reader.name == "BatchGetProjects" {
                continue;
            }
            let case = format!("{reader_name} for {case}", reader_name = reader.name);
            let (status, body) = call(&router, reader.name, (reader.request)(&identifier)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_type(&body),
                "ResourceNotFoundException",
                "case {case:?}: a project this account does not hold is absent, got {body}"
            );
            let stated = message(&body);
            let expected = if identifier.starts_with("arn:") {
                format!("The provided project {identifier} does not exist.")
            } else {
                format!(
                    "The provided project {} does not exist.",
                    project_arn(REGION, &identifier)
                )
            };
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the project quoted by one ARN, got {stated:?}"
            );
            assert_eq!(
                stated.matches("arn:").count(),
                1,
                "case {case:?}: the sentence must state exactly one ARN, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_request_that_carries_no_project_identifier_states_what_the_member_takes() {
    let router = emulator_holding_a_project().await;

    // The listings whose `projectName` the model requires; `ListBuildBatchesForProject`
    // leaves it optional, so its absence is the member-specific refusal that operation
    // states for a listing it cannot scope.
    for operation in ["ListBuildsForProject", "ListSandboxesForProject"] {
        let (status, body) = call(&router, operation, json!({})).await;
        assert_eq!(
            status, 400,
            "case {operation:?}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "InvalidInputException",
            "case {operation:?}: a request naming no project is an input error, got {body}"
        );
        assert_eq!(
            message(&body),
            "Project name or ARN is required",
            "case {operation:?}: expected CodeBuild's sentence naming both forms, got {body}"
        );
    }
}
