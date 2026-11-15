// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHERE a `BatchGetSecretValue` problem is reported, driven over the
//! real wire path (an awsJson1_1 request through the multi-service router into the mounted
//! service beside the IAM and STS services on one shared engine), so what is asserted is
//! what a client holding those credentials reads.
//!
//! THE RULE, in two halves:
//!
//! * A problem with ONE selected secret degrades THAT selection only: the call is a 200
//!   whose `SecretValues` carries every other selection's value and whose `Errors` carries
//!   exactly one entry for it — "If Secrets Manager encounters errors such as
//!   AccessDeniedException while attempting to retrieve any of the secrets, you can see the
//!   errors in Errors in the response" (`BatchGetSecretValue`, this service's Smithy model).
//!   That entry is the refusal the SINGLE read of that secret is refused with, because a
//!   batched read needs the authority a read of each secret needs — "you must have
//!   secretsmanager:GetSecretValue for each secret" (same place): its `SecretId` is the id
//!   the request supplied, its `ErrorCode` is that refusal's code and its `Message` is that
//!   refusal's sentence. So the first table asserts the ENTRY AGREES WITH `GetSecretValue`
//!   on the same secret, whatever refused it.
//!
//! * A problem with the REQUEST ITSELF fails the whole call, with nothing in either output
//!   list: the two ways of choosing the secrets cannot be carried together and neither may
//!   be absent — "You must include Filters or SecretIdList, but not both"
//!   (`BatchGetSecretValueRequest$SecretIdList`) — and a page size bounds only a selection
//!   the service made — "To use this parameter, you must also use the Filters parameter."
//!   (`BatchGetSecretValueRequest$MaxResults`). Both are `InvalidParameterException`, whose
//!   sentences are wholly fixed text carrying no part of the request.
//!
//! The tables span what the rule generalises over: what degrades the selection (an explicit
//! deny in the secret's own resource-based policy, no identity-based grant of the read, an
//! explicit deny in an identity-based policy, a secret inside its recovery window, an id
//! naming no secret, and a secret holding no current version — so the agreement is asserted
//! across all three per-entry error codes this operation can report), the form of the
//! `SecretId` each selection is named by, which selectors the request carries, and the page
//! size it asks for.
//!
//! `tests/all/secret_batch_get.rs` spans the batched read's selection and paging rules and
//! `tests/all/secret_batch_get_authorization.rs` the authority it needs;
//! `tests/all/identity_based_authorization.rs` owns the harness these cases reuse.

use serde_json::{json, Value};

use crate::identity_based_authorization::{
    allow_reads_of, allow_the_service, call_as, deny_read_of, error_code, error_message,
    errors_of, given_role_policy, given_secret, given_session, refusal_of_the_read,
    resource_policy_denying_reads, router_with_identity, ACCESS_DENIED, ACCESS_DENIED_STATUS,
    ACCOUNT_KEY_ID, CLIENT_ERROR_STATUS, IDENTITY_DENY_CLAUSE, NO_READ_GRANT_CLAUSE,
    RESOURCE_DENY_CLAUSE, SECRET_VALUE, SESSION_KEY_ID,
};

/// The two selections of every batch that stay readable, so a degraded one is asserted to
/// degrade nothing else.
const READABLE: [&str; 2] = ["batch-agree-first", "batch-agree-last"];

/// The selection each row of the first table degrades.
const DEGRADED: &str = "batch-agree-middle";

/// The error code a request this operation cannot read the selectors of carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The code an entry refused for the state of its secret carries.
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The code an entry whose secret or whose current version resolves to nothing carries.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// What this service answers a secret it cannot find with.
const SECRET_NOT_FOUND: &str = "Secrets Manager can't find the specified secret.";

/// What it answers a read of a secret inside its recovery window with: "If a secret is
/// scheduled for deletion, then its details, including the encrypted secret value, is not
/// accessible." (`GetSecretValue`, this service's Smithy model.)
const MARKED_FOR_DELETION: &str =
    "You can't perform this operation on the secret because it was marked for deletion.";

/// What it answers a read that resolves no value under the label it looked under with — the
/// label a read naming neither a version nor a stage resolves by: "If you don't specify
/// either a VersionStage or VersionId, Secrets Manager returns the AWSCURRENT version."
const NO_CURRENT_VERSION: &str =
    "Secrets Manager can't find the specified secret value for staging label: AWSCURRENT";

/// The shortest recovery window a delete may ask for: "The minimum value is 7 days"
/// (`DeleteSecretRequest$RecoveryWindowInDays`, this service's Smithy model).
const RECOVERY_WINDOW_DAYS: i64 = 7;

/// The sentences the batched read's request-form refusals carry, each wholly fixed text.
const BOTH_SELECTORS: &str = "You can't specify a secret ID list and filters in the same request.";
const NO_SELECTOR: &str = "You must specify a list of Secret IDs or a filter value.";
const MAX_RESULTS_WITHOUT_FILTERS: &str = "Max results can only be used with filters.";

/// What degrades ONE selection of the batch.
#[derive(Clone, Copy, Debug)]
enum Degradation {
    /// An explicit `Deny` of the single read in the secret's OWN resource-based policy.
    ResourcePolicyDeny,
    /// No identity-based statement allowing the single read of that secret, so it falls to
    /// IAM's default deny.
    NoIdentityGrant,
    /// An explicit `Deny` of the single read in the caller's identity-based policy.
    IdentityDeny,
    /// A secret inside its recovery window.
    MarkedForDeletion,
    /// An id naming no secret the account holds.
    NotHeld,
    /// A secret holding no version at all, so nothing carries `AWSCURRENT`.
    NoCurrentVersion,
}

/// The `SecretValues` of a batched read, as (ARN, SecretString, VersionId) triples in the
/// order they were answered.
///
/// The policy documents, the refusal-sentence builder and the `Errors` reader are the
/// harness's own ([`crate::identity_based_authorization`]), so one wording serves every suite;
/// only this reader is the suite's, because the agreement it asserts needs the version each
/// entry was answered from as well.
fn values_of(answer: &Value, case: &str) -> Vec<(String, String, String)> {
    answer
        .get("SecretValues")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no SecretValues in {answer}"))
        .iter()
        .map(|entry| {
            (
                entry["ARN"].as_str().unwrap_or_default().to_string(),
                entry["SecretString"].as_str().unwrap_or_default().to_string(),
                entry["VersionId"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Create one secret holding NO value, with the account's own credential, and report the ARN
/// the account holds it under: "If you include SecretString or SecretBinary then Secrets
/// Manager creates an initial secret version" (`CreateSecret`, this service's Smithy model),
/// so a create carrying neither leaves the secret with nothing under `AWSCURRENT`.
async fn given_valueless_secret(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> String {
    let (status, created) =
        call_as(router, ACCOUNT_KEY_ID, "CreateSecret", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {name:?}: a create carrying no value must be served, got {status} {created}"
    );
    created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no ARN in {created}"))
        .to_string()
}

/// THE RULE per entry: whatever refuses the read of ONE selected secret, the batched read is
/// a 200 that answers the others and reports that one in `Errors` with the id the request
/// supplied and the code and sentence the SINGLE read of it is refused with.
#[tokio::test]
async fn one_degraded_selection_is_reported_as_the_single_read_refuses_it() {
    // (what degrades the selection, the code its entry must carry)
    let rows: [(Degradation, &str); 6] = [
        (Degradation::ResourcePolicyDeny, ACCESS_DENIED),
        (Degradation::NoIdentityGrant, ACCESS_DENIED),
        (Degradation::IdentityDeny, ACCESS_DENIED),
        (Degradation::MarkedForDeletion, INVALID_REQUEST),
        (Degradation::NotHeld, NOT_FOUND),
        (Degradation::NoCurrentVersion, NOT_FOUND),
    ];
    // The form the request names each selection by. Both address the same secrets, and an
    // entry reports the id as the request spelled it.
    for by_arn in [false, true] {
        for (degradation, expected_code) in rows {
            let case = format!(
                "{degradation:?} addressed by {}",
                if by_arn { "ARN" } else { "name" }
            );
            let (router, engine) = router_with_identity();
            given_session(&engine);
            let mut readable: Vec<String> = Vec::new();
            for name in READABLE {
                readable.push(given_secret(&router, name).await);
            }
            // The degraded selection: the secret the row needs, and the id the request
            // names it by in each form.
            let (degraded_arn, degraded_id) = match degradation {
                Degradation::NotHeld => {
                    let arn = format!(
                        "arn:aws:secretsmanager:us-west-2:{}:secret:{DEGRADED}-AbCdEf",
                        ::engine::emulator_account_id()
                    );
                    let supplied = if by_arn { arn.clone() } else { DEGRADED.to_string() };
                    (arn, supplied)
                }
                Degradation::NoCurrentVersion => {
                    let arn = given_valueless_secret(&router, DEGRADED).await;
                    let supplied = if by_arn { arn.clone() } else { DEGRADED.to_string() };
                    (arn, supplied)
                }
                _ => {
                    let arn = given_secret(&router, DEGRADED).await;
                    let supplied = if by_arn { arn.clone() } else { DEGRADED.to_string() };
                    (arn, supplied)
                }
            };

            // The policies in force, and the state the row puts the degraded secret in.
            match degradation {
                Degradation::NoIdentityGrant => {
                    given_role_policy(&engine, Some(&allow_reads_of(&readable)));
                }
                Degradation::IdentityDeny => {
                    given_role_policy(&engine, Some(&deny_read_of(&degraded_arn)));
                }
                _ => given_role_policy(&engine, Some(&allow_the_service())),
            }
            if matches!(degradation, Degradation::ResourcePolicyDeny) {
                let (status, attached) = call_as(
                    &router,
                    ACCOUNT_KEY_ID,
                    "PutResourcePolicy",
                    &json!({
                        "SecretId": degraded_arn,
                        "ResourcePolicy": resource_policy_denying_reads().to_string(),
                    }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the policy must attach, got {status} {attached}"
                );
            }
            if matches!(degradation, Degradation::MarkedForDeletion) {
                let (status, deleted) = call_as(
                    &router,
                    ACCOUNT_KEY_ID,
                    "DeleteSecret",
                    &json!({
                        "SecretId": degraded_arn,
                        "RecoveryWindowInDays": RECOVERY_WINDOW_DAYS,
                    }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the delete must be served, got {status} {deleted}"
                );
            }

            // What the SINGLE read of the degraded secret is refused with — the refusal the
            // batch's entry has to carry.
            let (status, refused) = call_as(
                &router,
                SESSION_KEY_ID,
                "GetSecretValue",
                &json!({ "SecretId": degraded_id.clone() }),
            )
            .await;
            assert_eq!(
                status, CLIENT_ERROR_STATUS,
                "case {case:?}: expected the single read refused with HTTP \
                 {CLIENT_ERROR_STATUS}, got {status} {refused}"
            );
            let single_code = error_code(&refused);
            let single_message = error_message(&refused);
            assert_eq!(
                single_code, expected_code,
                "case {case:?}: expected the single read refused {expected_code}, got \
                 {single_code:?} with message {single_message:?}"
            );
            let expected_message = match degradation {
                Degradation::ResourcePolicyDeny => {
                    refusal_of_the_read(&degraded_id, RESOURCE_DENY_CLAUSE)
                }
                Degradation::NoIdentityGrant => {
                    refusal_of_the_read(&degraded_id, NO_READ_GRANT_CLAUSE)
                }
                Degradation::IdentityDeny => {
                    refusal_of_the_read(&degraded_id, IDENTITY_DENY_CLAUSE)
                }
                Degradation::MarkedForDeletion => MARKED_FOR_DELETION.to_string(),
                Degradation::NotHeld => SECRET_NOT_FOUND.to_string(),
                Degradation::NoCurrentVersion => NO_CURRENT_VERSION.to_string(),
            };
            assert_eq!(
                single_message, expected_message,
                "case {case:?}: expected {expected_message:?}, got {single_message:?}"
            );

            // The batch selects the two readable secrets AND the degraded one, in that
            // order, by the form this row names them with.
            let supplied: Vec<String> = readable
                .iter()
                .enumerate()
                .map(|(index, arn)| {
                    if by_arn {
                        arn.clone()
                    } else {
                        READABLE[index].to_string()
                    }
                })
                .chain([degraded_id.clone()])
                .collect();
            let (status, answer) = call_as(
                &router,
                SESSION_KEY_ID,
                "BatchGetSecretValue",
                &json!({ "SecretIdList": supplied.clone() }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: one degraded selection must not fail the call, got {status} \
                 {answer}"
            );

            // The readable selections are answered with their stored value, and the version
            // each is answered from is the one the single read of it resolves.
            let answered = values_of(&answer, &case);
            assert_eq!(
                answered.len(),
                READABLE.len(),
                "case {case:?}: expected {} value(s), got {answered:?}",
                READABLE.len()
            );
            for (index, arn) in readable.iter().enumerate() {
                let (served_status, served) = call_as(
                    &router,
                    SESSION_KEY_ID,
                    "GetSecretValue",
                    &json!({ "SecretId": supplied[index].clone() }),
                )
                .await;
                assert_eq!(
                    served_status, 200,
                    "case {case:?}: the single read of {arn:?} must be served, got \
                     {served_status} {served}"
                );
                let expected = (
                    arn.clone(),
                    SECRET_VALUE.to_string(),
                    served["VersionId"].as_str().unwrap_or_default().to_string(),
                );
                assert_eq!(
                    answered[index], expected,
                    "case {case:?}: expected the entry to agree with the single read \
                     ({expected:?}), got {:?}",
                    answered[index]
                );
            }

            // The degraded selection is exactly one `Errors` entry, carrying the id as
            // supplied and the refusal the single read answered with.
            let reported = errors_of(&answer, &case);
            let expected = vec![(degraded_id.clone(), single_code, single_message)];
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the entry to agree with the single read \
                 ({expected:?}), got {reported:?}"
            );
            // Every selection is accounted for exactly once, and the list form pages
            // nothing: it carries no selection the service made.
            assert_eq!(
                answered.len() + reported.len(),
                supplied.len(),
                "case {case:?}: expected every selection answered once, got {answer}"
            );
            assert!(
                answer["NextToken"].is_null(),
                "case {case:?}: the list form must answer no NextToken, got {answer}"
            );
        }
    }
}

/// THE RULE for the request itself: the selectors cannot be carried together and neither may
/// be absent, and a page size needs the form that selects — so a request that breaks either
/// is `InvalidParameterException` with fixed text and answers NEITHER list, even though every
/// secret it names is readable by this caller.
#[tokio::test]
async fn a_request_that_cannot_be_read_fails_the_whole_call() {
    let (router, engine) = router_with_identity();
    given_session(&engine);
    given_role_policy(&engine, Some(&allow_the_service()));
    let mut arns: Vec<String> = Vec::new();
    for name in READABLE {
        arns.push(given_secret(&router, name).await);
    }
    let names: Vec<String> = READABLE.iter().map(|name| name.to_string()).collect();
    let filter = json!([{ "Key": "name", "Values": ["batch-agree-"] }]);

    // (case, the members the request carries, the sentence it is refused with)
    let rows: Vec<(String, Value, &str)> = vec![
        (
            "both selectors".to_string(),
            json!({ "SecretIdList": names.clone(), "Filters": filter.clone() }),
            BOTH_SELECTORS,
        ),
        (
            "both selectors, the filter list empty".to_string(),
            json!({ "SecretIdList": names.clone(), "Filters": [] }),
            BOTH_SELECTORS,
        ),
        (
            "both selectors, the ids as ARNs and a page size".to_string(),
            json!({
                "SecretIdList": arns.clone(),
                "Filters": filter.clone(),
                "MaxResults": 2,
            }),
            BOTH_SELECTORS,
        ),
        ("neither selector".to_string(), json!({}), NO_SELECTOR),
        (
            "neither selector, with a page size".to_string(),
            json!({ "MaxResults": 1 }),
            NO_SELECTOR,
        ),
        // The page size rows run over each published bound of `MaxResultsBatchType`
        // (`@range` 1..20, this service's Smithy model) and a size between them.
        (
            "a page size of 1 beside the id list".to_string(),
            json!({ "SecretIdList": names.clone(), "MaxResults": 1 }),
            MAX_RESULTS_WITHOUT_FILTERS,
        ),
        (
            "a page size of 7 beside the id list".to_string(),
            json!({ "SecretIdList": arns.clone(), "MaxResults": 7 }),
            MAX_RESULTS_WITHOUT_FILTERS,
        ),
        (
            "a page size of 20 beside the id list".to_string(),
            json!({ "SecretIdList": names.clone(), "MaxResults": 20 }),
            MAX_RESULTS_WITHOUT_FILTERS,
        ),
        (
            "a page size and a cursor beside the id list".to_string(),
            json!({
                "SecretIdList": names.clone(),
                "MaxResults": 2,
                "NextToken": "not-a-cursor",
            }),
            MAX_RESULTS_WITHOUT_FILTERS,
        ),
    ];

    for (case, body, expected) in rows {
        let (status, refused) =
            call_as(&router, SESSION_KEY_ID, "BatchGetSecretValue", &body).await;
        assert_eq!(
            status, CLIENT_ERROR_STATUS,
            "case {case:?}: expected HTTP {CLIENT_ERROR_STATUS} for {body}, got {status} \
             {refused}"
        );
        let code = error_code(&refused);
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {code:?} for {refused}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // THE CONTROL: dropping the member that cannot hold serves the very same selection — the
    // id list alone, and the filter form with the page size the id list may not carry.
    let (status, served) = call_as(
        &router,
        SESSION_KEY_ID,
        "BatchGetSecretValue",
        &json!({ "SecretIdList": names.clone() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the id list alone\": expected 200, got {status} {served}"
    );
    assert_eq!(
        values_of(&served, "the id list alone").len(),
        READABLE.len(),
        "case \"the id list alone\": expected {} value(s), got {served}",
        READABLE.len()
    );
    assert!(
        errors_of(&served, "the id list alone").is_empty(),
        "case \"the id list alone\": expected no error entry, got {served}"
    );
    for page_size in [1usize, READABLE.len()] {
        let case = format!("the filter form with a page size of {page_size}");
        let (status, paged) = call_as(
            &router,
            SESSION_KEY_ID,
            "BatchGetSecretValue",
            &json!({ "Filters": filter.clone(), "MaxResults": page_size }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} {paged}");
        assert_eq!(
            values_of(&paged, &case).len(),
            page_size,
            "case {case:?}: expected a page of {page_size} value(s), got {paged}"
        );
    }

    // A denial of the caller's own authority, by contrast, fails the call with the denial —
    // the other half of the boundary the rows above pin.
    given_role_policy(&engine, None);
    let case = "no authority for the call at all";
    let (status, denied) = call_as(
        &router,
        SESSION_KEY_ID,
        "BatchGetSecretValue",
        &json!({ "SecretIdList": names }),
    )
    .await;
    assert_eq!(
        status, ACCESS_DENIED_STATUS,
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} {denied}"
    );
    assert_eq!(
        error_code(&denied),
        ACCESS_DENIED,
        "case {case:?}: expected an {ACCESS_DENIED}, got {denied}"
    );
}
