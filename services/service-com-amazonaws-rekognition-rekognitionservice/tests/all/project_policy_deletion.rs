// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of `DeleteProjectPolicy` over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: RekognitionService.<Operation>`), dispatched through the same
//! [`MultiServiceRouter`] the production emulator mounts.
//!
//! THE RULE UNDER TEST: deleting a named child of a resource resolves the parent, then
//! refuses with the modelled 400 `ResourceNotFoundException` when the parent holds no child
//! of that name — decided BEFORE any precondition on the child is consulted — refuses with
//! the modelled 400 `InvalidPolicyRevisionIdException` when the request names a revision
//! that is not the one the child currently carries, and otherwise removes the child and
//! answers the empty 200. Every refusal leaves the parent holding exactly the children it
//! held.
//!
//! Each test asserts that rule over a TABLE whose rows differ along what it generalises
//! over: which `PolicyName` is asked for (one the project never held, both published
//! `ProjectPolicyName` length bounds, a name carrying the punctuation its pattern admits),
//! what the request says about the revision (none, one this service never minted, a stale
//! one, another policy's current one, both published `ProjectPolicyRevisionId` bounds, the
//! current one in either case of its hexadecimal digits), and which project ARN is named.
//! The never-attached name with no revision is the row the issue's recorded exchange
//! carries; the rest are the same rule at other inputs.
//!
//! The helpers that create a project, record the model version a policy document grants on
//! and read the policies back come from the attach suite beside this one
//! ([`crate::project_policies`]) rather than being built a second time here.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::face_collections::{call, Reply};
use crate::project_policies::{
    create_project, harness_with_engine, list_policies, listed_policy, member, policy_document,
    put_policy, record_model_version, REGION,
};

/// `PolicyName` is `ProjectPolicyName`, `len 1..128, pattern ^[a-zA-Z0-9_.\-]+$`
/// (`smithy_query(query="operation",operation="DeleteProjectPolicy")`).
const POLICY_NAME_MAX: usize = 128;

/// `PolicyRevisionId` is `ProjectPolicyRevisionId`, `len 0..64, pattern ^[0-9A-Fa-f]+$`
/// (same model query).
const POLICY_REVISION_ID_MAX: usize = 64;

/// `DeleteProjectPolicy` for `policy_name` on `project_arn`, naming `revision_id` when the
/// delete asks for a particular revision.
async fn delete_policy(
    router: &MultiServiceRouter,
    project_arn: &str,
    policy_name: &str,
    revision_id: Option<&str>,
) -> Reply {
    let mut body = json!({
        "ProjectArn": project_arn,
        "PolicyName": policy_name,
    });
    if let Some(revision) = revision_id {
        body["PolicyRevisionId"] = json!(revision);
    }
    call(router, "DeleteProjectPolicy", body).await
}

/// The project-name segment of a `ProjectArn`: the part after `project/` and before the
/// trailing version segment the service assigned (`ProjectArn`'s pattern ends
/// `project\/[a-zA-Z0-9_.\-]{1,255}\/[0-9]+$`).
fn project_name_of(project_arn: &str) -> String {
    let (_, resource) = project_arn
        .split_once(":project/")
        .unwrap_or_else(|| panic!("a ProjectArn names a project: {project_arn:?}"));
    let (project_name, _version) = resource
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("a ProjectArn ends in its version segment: {project_arn:?}"));
    project_name.to_string()
}

/// The wording a name the project has no policy under is refused with.
fn expected_not_found_message(policy_name: &str, project_name: &str) -> String {
    format!("The specified policy {policy_name} does not exist for project {project_name}.")
}

/// The wording a delete naming a revision other than the policy's current one is refused
/// with.
fn expected_stale_revision_message(policy_name: &str) -> String {
    format!(
        "The policy cannot be deleted. Please enter the latest policy revision ID associated \
         with the policy {policy_name} and try again."
    )
}

/// Assert that `reply` is the modelled refusal of class `code`, worded `message`.
fn assert_refused(case: &str, reply: &Reply, code: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (400, code),
        "case {case:?}: the delete must be refused as {code}, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("Message"),
        Some(message.to_string()),
        "case {case:?}: expected the wording {message:?}, got {:?}",
        reply.body
    );
}

/// Assert that `reply` is the successful delete: HTTP 200 with a body carrying no member,
/// since `DeleteProjectPolicyResponse` models none.
fn assert_deleted(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the delete must succeed, got {:?}",
        reply.body
    );
    let members = reply.body.as_object().map_or(0, serde_json::Map::len);
    assert_eq!(
        members, 0,
        "case {case:?}: a successful delete answers an empty body, got {:?}",
        reply.body
    );
}

/// The names of the policies the project holds, in order.
async fn policy_names(router: &MultiServiceRouter, project_arn: &str) -> Vec<String> {
    let mut names: Vec<String> = list_policies(router, project_arn, None)
        .await
        .iter()
        .filter_map(|policy| member(policy, "PolicyName"))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_policy_the_project_never_attached_cannot_be_deleted() {
    let (router, engine) = harness_with_engine();
    let project_arn = create_project(&router, "delete-missing-policy-project").await;
    let version = record_model_version(&engine, "delete-missing-policy-project", "version-one");
    let document = policy_document(&[version.as_str()]);

    // One policy IS attached, so every refusal below is about the NAME asked for rather than
    // about the project holding no policy at all, and its revision is one this service really
    // minted.
    let attached = put_policy(&router, &project_arn, "attached", &document, None).await;
    assert_eq!(
        attached.status, 200,
        "the attach the rest of this test leans on must succeed, got {:?}",
        attached.body
    );
    let attached_revision = attached
        .string("PolicyRevisionId")
        .expect("the attach answers a revision");

    let project_name = project_name_of(&project_arn);
    let long_name = "p".repeat(POLICY_NAME_MAX);
    let longest_revision = "f".repeat(POLICY_REVISION_ID_MAX);

    // Each row asks to delete a policy the project does not hold, differing in the name it
    // asks for and in what it says about a revision — which is never consulted, because
    // there is no policy whose revision it could disagree with.
    for (case, policy_name, revision) in [
        // The row the recorded exchange carries.
        ("no revision at all", "never-attached", None),
        (
            "a well-formed revision this service never minted",
            "never-attached",
            Some("deadbeef".to_string()),
        ),
        (
            "the revision another policy of this project carries",
            "never-attached",
            Some(attached_revision.clone()),
        ),
        (
            "the shortest well-formed revision",
            "never-attached",
            Some("a".to_string()),
        ),
        (
            "the longest well-formed revision",
            "never-attached",
            Some(longest_revision.clone()),
        ),
        ("the shortest policy name", "p", None),
        (
            "the longest policy name",
            long_name.as_str(),
            Some("DEADBEEF".to_string()),
        ),
        (
            "a name carrying the punctuation the pattern admits",
            "a.b-c_d",
            None,
        ),
    ] {
        let reply = delete_policy(&router, &project_arn, policy_name, revision.as_deref()).await;
        assert_refused(
            case,
            &reply,
            "ResourceNotFoundException",
            &expected_not_found_message(policy_name, &project_name),
        );
    }

    // Every refusal left the project exactly as it was.
    assert_eq!(
        policy_names(&router, &project_arn).await,
        vec!["attached".to_string()],
        "a refused delete must leave the project's policies as they were"
    );
}

#[tokio::test]
async fn a_delete_that_does_not_name_the_latest_revision_is_refused() {
    let (router, engine) = harness_with_engine();
    let project_arn = create_project(&router, "delete-revision-project").await;
    let first_version = record_model_version(&engine, "delete-revision-project", "version-one");
    let second_version = record_model_version(&engine, "delete-revision-project", "version-two");
    let original = policy_document(&[first_version.as_str()]);
    let replacement = policy_document(&[second_version.as_str()]);

    // A second policy, so "a revision this service minted" and "THIS policy's revision" can
    // be told apart, and so a refusal can be seen to leave its neighbour alone.
    let other = put_policy(&router, &project_arn, "other", &original, None).await;
    assert_eq!(
        other.status, 200,
        "the neighbouring attach must succeed, got {:?}",
        other.body
    );
    let other_revision = other
        .string("PolicyRevisionId")
        .expect("the attach answers a revision");

    let attached = put_policy(&router, &project_arn, "revisioned", &original, None).await;
    assert_eq!(
        attached.status, 200,
        "the attach must succeed, got {:?}",
        attached.body
    );
    let stale_revision = attached
        .string("PolicyRevisionId")
        .expect("the attach answers a revision");

    // A rewrite moves the policy on, which is what makes the first revision stale.
    let rewritten = put_policy(
        &router,
        &project_arn,
        "revisioned",
        &replacement,
        Some(&stale_revision),
    )
    .await;
    assert_eq!(
        rewritten.status, 200,
        "the rewrite must succeed, got {:?}",
        rewritten.body
    );
    let current_revision = rewritten
        .string("PolicyRevisionId")
        .expect("the rewrite answers a revision");

    // Each row names a revision that is not the one the policy currently carries.
    let longest_revision = "f".repeat(POLICY_REVISION_ID_MAX);
    for (case, revision, code, message) in [
        (
            "a revision this service never minted",
            "deadbeef".to_string(),
            "InvalidPolicyRevisionIdException",
            expected_stale_revision_message("revisioned"),
        ),
        (
            "the revision the rewrite replaced",
            stale_revision.clone(),
            "InvalidPolicyRevisionIdException",
            expected_stale_revision_message("revisioned"),
        ),
        (
            "another policy's current revision",
            other_revision.clone(),
            "InvalidPolicyRevisionIdException",
            expected_stale_revision_message("revisioned"),
        ),
        (
            "the shortest well-formed revision",
            "a".to_string(),
            "InvalidPolicyRevisionIdException",
            expected_stale_revision_message("revisioned"),
        ),
        (
            "the longest well-formed revision",
            longest_revision.clone(),
            "InvalidPolicyRevisionIdException",
            expected_stale_revision_message("revisioned"),
        ),
        (
            // `ProjectPolicyRevisionId` is `len 0..64` while its pattern needs at least one
            // hexadecimal digit, so the empty revision is outside the model and is refused at
            // the boundary rather than read as naming no revision.
            "the empty revision",
            String::new(),
            "ValidationException",
            String::new(),
        ),
    ] {
        let reply = delete_policy(&router, &project_arn, "revisioned", Some(&revision)).await;
        if code == "ValidationException" {
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, code),
                "case {case:?}: a revision outside the model must be refused, got {:?}",
                reply.body
            );
        } else {
            assert_refused(case, &reply, code, &message);
        }

        // The refusal deleted nothing: the policy is still the record the rewrite wrote.
        let listed = listed_policy(&router, &project_arn, "revisioned")
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the policy must survive the refusal"));
        assert_eq!(
            (
                member(&listed, "PolicyDocument"),
                member(&listed, "PolicyRevisionId"),
            ),
            (
                Some(replacement.clone()),
                Some(current_revision.clone())
            ),
            "case {case:?}: a refused delete must leave the policy as it was, got {listed:?}"
        );
    }

    // A revision is hexadecimal, and hexadecimal is case-insensitive, so the current revision
    // names the policy in either case — and naming it deletes exactly that policy.
    let deleted = delete_policy(
        &router,
        &project_arn,
        "revisioned",
        Some(&current_revision.to_ascii_uppercase()),
    )
    .await;
    assert_deleted("the current revision in upper case", &deleted);
    assert_eq!(
        policy_names(&router, &project_arn).await,
        vec!["other".to_string()],
        "the delete must remove exactly the policy it named"
    );
}

#[tokio::test]
async fn deleting_a_policy_removes_exactly_it() {
    let (router, engine) = harness_with_engine();
    let project_arn = create_project(&router, "delete-policy-project").await;
    let version = record_model_version(&engine, "delete-policy-project", "version-one");
    let document = policy_document(&[version.as_str()]);

    // Each row is a policy to delete, differing in the name it is attached under and in
    // whether the delete names the revision the policy carries.
    let long_name = "p".repeat(POLICY_NAME_MAX);
    let cases: Vec<(&str, String, bool)> = vec![
        ("no revision named", "copy-one".to_string(), false),
        ("the current revision named", "copy-two".to_string(), true),
        ("the shortest policy name", "p".to_string(), false),
        ("the longest policy name", long_name.clone(), true),
        (
            "a name carrying the punctuation the pattern admits",
            "a.b-c_d".to_string(),
            true,
        ),
    ];

    for (case, policy_name, _) in &cases {
        let reply = put_policy(&router, &project_arn, policy_name, &document, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the attach must succeed, got {:?}",
            reply.body
        );
    }

    let mut remaining = policy_names(&router, &project_arn).await;
    let mut expected: Vec<String> = cases.iter().map(|(_, name, _)| name.clone()).collect();
    expected.sort();
    assert_eq!(
        remaining, expected,
        "every row's policy must be attached before any of them is deleted"
    );

    for (case, policy_name, names_revision) in &cases {
        let revision = listed_policy(&router, &project_arn, policy_name)
            .await
            .and_then(|policy| member(&policy, "PolicyRevisionId"));
        let named = if *names_revision {
            Some(
                revision
                    .clone()
                    .unwrap_or_else(|| panic!("case {case:?}: a listed policy carries a revision")),
            )
        } else {
            None
        };
        let reply = delete_policy(&router, &project_arn, policy_name, named.as_deref()).await;
        assert_deleted(case, &reply);

        // The read-back: the deleted policy is gone and every other one is still there.
        remaining.retain(|name| name != policy_name);
        assert_eq!(
            policy_names(&router, &project_arn).await,
            remaining,
            "case {case:?}: the delete must remove exactly the policy it named"
        );

        // A delete is not idempotent: the name now names no policy, so repeating it is the
        // not-found refusal rather than a second success.
        let repeated = delete_policy(&router, &project_arn, policy_name, None).await;
        assert_refused(
            case,
            &repeated,
            "ResourceNotFoundException",
            &expected_not_found_message(policy_name, &project_name_of(&project_arn)),
        );
    }

    // The names are free again: a deleted policy's name can be attached anew, which naming
    // no revision asks for.
    for (case, policy_name, _) in &cases {
        let reply = put_policy(&router, &project_arn, policy_name, &document, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a deleted policy's name must be free to attach again, got {:?}",
            reply.body
        );
    }
    assert_eq!(
        policy_names(&router, &project_arn).await,
        expected,
        "the re-attached policies must be the ones the project holds"
    );
}

#[tokio::test]
async fn a_project_arn_naming_no_project_names_nothing_to_delete_from() {
    let (router, engine) = harness_with_engine();
    let project_arn = create_project(&router, "delete-live-project").await;
    let version = record_model_version(&engine, "delete-live-project", "version-one");
    let document = policy_document(&[version.as_str()]);
    let attached = put_policy(&router, &project_arn, "survivor", &document, None).await;
    assert_eq!(
        attached.status, 200,
        "the attach must succeed, got {:?}",
        attached.body
    );
    let revision = attached
        .string("PolicyRevisionId")
        .expect("the attach answers a revision");

    // Each row is an ARN inside `ProjectArn`'s pattern that names no project of this store,
    // so there is no project to look a policy up in.
    for (case, arn) in [
        (
            "a project name nothing was created under",
            format!(
                "arn:aws:rekognition:{REGION}:{}:project/ghost-project/1700000000000",
                engine::emulator_account_id()
            ),
        ),
        (
            "a live project's name under another account",
            project_arn.replace(engine::emulator_account_id(), "123456789012"),
        ),
        (
            "a live project's name in another region",
            project_arn.replace(REGION, "eu-west-1"),
        ),
    ] {
        for named in [None, Some(revision.as_str())] {
            let reply = delete_policy(&router, &arn, "survivor", named).await;
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "ResourceNotFoundException"),
                "case {case:?}/revision={named:?}: an ARN naming no project must be refused, \
                 got {:?}",
                reply.body
            );
        }
    }

    // The live project kept its policy through every refusal on another ARN.
    let listed = listed_policy(&router, &project_arn, "survivor")
        .await
        .expect("the policy of the live project must survive");
    assert_eq!(
        (
            member(&listed, "PolicyDocument"),
            member(&listed, "PolicyRevisionId"),
        ),
        (Some(document.clone()), Some(revision.clone())),
        "a refusal on another ARN must not change this project's policy, got {listed:?}"
    );
    assert_eq!(
        list_policies(&router, &project_arn, None).await.len(),
        1,
        "the live project must still hold exactly its one policy"
    );
}
