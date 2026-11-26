// SPDX-License-Identifier: CC0-1.0

//! How `DeleteDeploymentStrategy` WORDS its two refusals, over the set of ids a caller can
//! name.
//!
//! `BadRequestException` covers every AppConfig validation refusal, so the MESSAGE is the
//! only field that says which refusal a delete hit. The rule under test is that each
//! refusal carries that class's own AppConfig wording, byte for byte:
//!
//! * a published catalogue entry is immutable — 400 `BadRequestException` whose message is
//!   the fixed prefix `Cannot delete predefined Deployment Strategy `, the submitted
//!   `DeploymentStrategyId`, and the terminating full stop this operation's sentence ends
//!   with;
//! * an id nothing answers to — 404 `ResourceNotFoundException` with the deployment
//!   strategy kind's own not-found template, which names no account and spells the kind as
//!   one word.
//!
//! The tables span what those rules generalise over: every entry of the published
//! catalogue (not only the one a caller happens to name), and — for the second rule — both
//! branches of `DeploymentStrategyId`'s pattern
//! (`^(^[a-z0-9]{4,7}$|^AppConfig\.[A-Za-z0-9]{9,40}$)$`, from
//! `smithy_query(query="operation", operation="DeleteDeploymentStrategy")`), including a
//! strategy this account created and then deleted.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::state::{
    deployment_strategy_not_found, PREDEFINED_DEPLOYMENT_STRATEGIES,
};

use super::appconfig_workflow::{instant_deployment_strategy, json_call, router, Answer};

/// The collection segment the model binds the delete to — AppConfig's own misspelling
/// (`DELETE /deployementstrategies/{DeploymentStrategyId}`).
const STRATEGY_DELETES: &str = "/deployementstrategies";

/// `DELETE /deployementstrategies/{Id}`, with no request body at all.
async fn delete_strategy(router: &MultiServiceRouter, id: &str) -> Answer {
    json_call(
        router,
        "DELETE",
        &format!("{STRATEGY_DELETES}/{id}"),
        &Value::Null,
    )
    .await
}

/// `GET /deploymentstrategies/{Id}`.
async fn get_strategy(router: &MultiServiceRouter, id: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/deploymentstrategies/{id}"),
        &Value::Null,
    )
    .await
}

/// The short error code of a refusal, with any shape namespace stripped.
fn code(answer: &Answer) -> String {
    answer
        .error_code()
        .split('#')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// Deleting any entry of AppConfig's published catalogue is refused with the immutability
/// sentence exactly as AppConfig words it — the fixed prefix, the id the request submitted
/// and the terminating full stop — and the entry still reads back untouched.
#[tokio::test]
async fn deleting_a_predefined_strategy_is_refused_with_appconfigs_sentence() {
    let router = router();

    // Rows are the whole predefined catalogue (`state::PREDEFINED_DEPLOYMENT_STRATEGIES`,
    // the AWS AppConfig User Guide's "Predefined deployment strategies" table), so every
    // entry is asserted, including the ones no recorded request names.
    for predefined in PREDEFINED_DEPLOYMENT_STRATEGIES {
        let case = predefined.id;
        let before = get_strategy(&router, predefined.id).await;
        assert_eq!(
            before.status,
            200,
            "case {case:?}: the catalogue entry must be readable first, got {} / {}",
            before.status,
            String::from_utf8_lossy(&before.body)
        );

        let refused = delete_strategy(&router, predefined.id).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected 400, got {} / {}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            code(&refused),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {:?}",
            refused.error_code()
        );

        let expected = format!("Cannot delete predefined Deployment Strategy {case}.");
        let message = refused.string("Message").unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );

        // The refusal changed nothing: a caller that retries the read sees exactly the
        // record it saw before asking for the delete.
        let after = get_strategy(&router, predefined.id).await;
        assert_eq!(
            after.json(),
            before.json(),
            "case {case:?}: a refused delete must change nothing, expected {}, got {}",
            before.json(),
            after.json()
        );
    }
}

/// One id a delete can name, and the answer its class carries.
struct Row {
    case: &'static str,
    /// `None` names `id` as given; `Some(name)` creates a strategy under that name first
    /// and deletes the id the create minted.
    create_as: Option<&'static str>,
    id: &'static str,
}

/// The immutability sentence and the not-found template are different refusals, and a
/// delete that really removes a record is neither: each id form a caller can name gets its
/// own class's wording, so the message discriminates them.
#[tokio::test]
async fn each_delete_outcome_carries_its_own_classs_wording() {
    let router = router();

    // Rows span both branches of the member's pattern: a minted id, an unminted id of the
    // `^[a-z0-9]{4,7}$` form, a published `AppConfig.*` entry and an `AppConfig.*` id the
    // catalogue does not publish.
    let rows = [
        Row {
            case: "a strategy this account created",
            create_as: Some("delete-wording-own-strategy"),
            id: "",
        },
        Row {
            case: "an id of the minted form that nothing minted",
            create_as: None,
            id: "zzz9999",
        },
        Row {
            case: "a published catalogue entry",
            create_as: None,
            id: "AppConfig.AllAtOnce",
        },
        Row {
            case: "a predefined-looking id AppConfig does not publish",
            create_as: None,
            id: "AppConfig.NotPublished1",
        },
    ];

    for row in rows {
        let case = row.case;
        let id = match row.create_as {
            Some(name) => instant_deployment_strategy(&router, name).await,
            None => row.id.to_string(),
        };
        let is_predefined = PREDEFINED_DEPLOYMENT_STRATEGIES
            .iter()
            .any(|predefined| predefined.id == id);

        let answer = delete_strategy(&router, &id).await;
        if row.create_as.is_some() {
            assert_eq!(
                answer.status,
                204,
                "case {case:?}: a created strategy's delete must answer 204, got {} / {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert!(
                answer.body.is_empty(),
                "case {case:?}: a 204 carries no body, got {:?}",
                String::from_utf8_lossy(&answer.body)
            );

            // Once removed the same id is in the not-found class, so the SECOND delete is
            // worded as a miss rather than as an immutability refusal.
            let again = delete_strategy(&router, &id).await;
            let expected = deployment_strategy_not_found(&id);
            let message = again.string("Message").unwrap_or_default();
            assert_eq!(
                again.status,
                404,
                "case {case:?}: deleting it twice must answer 404, got {} / {}",
                again.status,
                String::from_utf8_lossy(&again.body)
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
            continue;
        }

        let (expected_status, expected_code, expected_message) = if is_predefined {
            (
                400,
                "BadRequestException",
                format!("Cannot delete predefined Deployment Strategy {id}."),
            )
        } else {
            (
                404,
                "ResourceNotFoundException",
                deployment_strategy_not_found(&id),
            )
        };
        assert_eq!(
            answer.status, expected_status,
            "case {case:?}: expected {expected_status}, got {} / {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            code(&answer),
            expected_code,
            "case {case:?}: expected {expected_code}, got {:?}",
            answer.error_code()
        );
        let message = answer.string("Message").unwrap_or_default();
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected message {expected_message:?}, got {message:?}"
        );
    }
}
