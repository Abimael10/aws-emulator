// SPDX-License-Identifier: CC0-1.0

//! The wrong-scope sentence on every triple-addressed path, where the addressed scope is
//! POPULATED and holds a resource of the very same name.
//!
//! The RULE this adds a case to: the refusal of an id that resolves under another app
//! environment is decided by WHO OWNS the id, not by what the addressed app environment
//! happens to hold — so a scope that carries its own resource of the same kind and name
//! answers the one fixed sentence just like an empty one, on the read, the update and the
//! delete alike.
//!
//! The sibling [`super::wrong_scope_sentence`] owns the harness and the sentence, and spans
//! the kinds, the operations and which half of the (appId, environmentName) pair is wrong
//! against an EMPTY wrong scope; `read_and_delete_scope_classes`, `theme_scope_classes` and
//! `codegen_job_scope` cover a populated wrong scope by PHRASE. What is left to this suite is
//! the row none of them has: the addressed scope holds a resource of the same kind under the
//! SAME NAME as the one being addressed, answered with the whole sentence, which is the input
//! an implementation keyed on "the collection is empty" or "the name is free" would get
//! wrong; it also checks the named resource of the addressed scope is untouched afterwards.

use super::wrong_scope_sentence::{
    call, create, emulator, member_uri, Kind, APP_ID, ENVIRONMENT, WRONG_SCOPE_SENTENCE,
};

/// The name every row gives to both the owned resource and the one standing in the addressed
/// scope, so the two differ only in which app environment owns them.
const SHARED_NAME: &str = "Shared";

#[tokio::test]
async fn a_populated_wrong_scope_answers_the_same_sentence_on_every_path() {
    // The rows differ in the resource KIND addressed and in which half of the scope the
    // request gets wrong; in each, the addressed scope holds its own resource of that kind.
    let scopes = [
        ("another app, the same environment", "d9999zzzz", ENVIRONMENT),
        ("the same app, another environment", APP_ID, "integration"),
    ];

    for kind in Kind::ALL {
        for (scope, app_id, environment) in scopes {
            let router = emulator();
            let owned = create(&router, kind, APP_ID, ENVIRONMENT, SHARED_NAME).await;
            let native = create(&router, kind, app_id, environment, SHARED_NAME).await;
            assert_ne!(
                owned, native,
                "case {scope:?} on a {kind:?}: two resources of one name in two app \
                 environments must be two resources"
            );

            for (operation, method, body) in kind.member_operations() {
                let case = format!("{scope} holding a {kind:?} of its own, on {operation}");
                let answer = call(
                    &router,
                    method,
                    &member_uri(kind, app_id, environment, &owned),
                    body,
                )
                .await;

                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: a wrong-scope id must answer HTTP 400, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.message(),
                    WRONG_SCOPE_SENTENCE,
                    "case {case:?}: expected the message {WRONG_SCOPE_SENTENCE:?}, got {:?}",
                    answer.message()
                );
                assert!(
                    answer.error_code().is_empty(),
                    "case {case:?}: the refusal must name no error type, got {:?}",
                    answer.error_code()
                );
            }

            // Neither resource moved: the id's owner still serves it, and the resource the
            // addressed scope holds under the same name is still its own.
            for (owner, owner_app, owner_environment, id) in [
                ("the owning scope", APP_ID, ENVIRONMENT, &owned),
                ("the addressed scope", app_id, environment, &native),
            ] {
                let read = call(
                    &router,
                    "GET",
                    &member_uri(kind, owner_app, owner_environment, id),
                    None,
                )
                .await;
                let case = format!("{owner} of a {kind:?} after {scope}");
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: the resource must still read in its own scope, got {} with \
                     {:?}",
                    read.status, read.body
                );
                assert_eq!(
                    read.id(),
                    *id,
                    "case {case:?}: that read must report the same id, got {:?}",
                    read.body
                );
            }
        }
    }
}
