// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHO owns an already-exists sentence: the resource KIND, not the
//! operation that ran into the taken name.
//!
//! The rule under test: a write that asks for a name the account already holds is refused
//! `AlreadyExistsException` (400) with the one sentence this service words that resource
//! kind's already-exists condition in — fixed text naming the kind, with the requested name
//! withheld — whichever write ran into it. A create that takes a name and an update that
//! RENAMES a resource onto a name already held are the same condition on the same kind, so
//! they answer one sentence rather than two, and the refused write leaves the stored
//! resource as it was.
//!
//! The axis this suite spans is therefore the OPERATION, which the per-surface suite
//! ([`crate::catalog_duplicate_create_wording::each_already_exists_surface_keeps_its_own_sentence`],
//! where the five resource kinds and their sentences live) does not: it drives the creates.
//! The name table and the sentence itself are that suite's; nothing of them is restated here.
//!
//! Sources: `smithy_query operation=CreateDatabase,UpdateDatabase` for the two writes, their
//! shared `AlreadyExistsException` ("a resource to be created or added already exists") and
//! `DatabaseInput$Name` — "the name of the database … For Hive compatibility, this is folded
//! to lowercase when it is stored", which is why a row renames onto a taken name in another
//! casing and is refused just the same.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::error_messages::{assert_refusal, assert_withholds};
use crate::wire::{emulator, error_message, ok};

/// The modelled error both writes answer for a name already taken.
const ALREADY_EXISTS: &str = "AlreadyExistsException";

/// The whole `Message` the DATABASE kind words that condition with, written out here as a
/// literal so this suite pins the wording rather than restating the service's constant.
const DATABASE_ALREADY_EXISTS: &str = "Database already exists.";

/// Create the database `name`, with a description the row can tell from another's.
async fn given_database(router: &MultiServiceRouter, name: &str, description: &str) {
    ok(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": name, "Description": description } }),
    )
    .await;
}

/// The `Description` a `GetDatabase` reports for `name`.
async fn description_of(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let read = ok(router, "GetDatabase", &json!({ "Name": name })).await;
    read["Database"]["Description"].as_str().map(str::to_string)
}

/// One row of the table: the write that ran into the taken name, and how the name it asks
/// for is spelled against the one already held.
struct Row {
    case: &'static str,
    /// The name the write asks for, built from the name already held.
    asked_for: fn(&str) -> String,
    /// Whether the write is the CREATE of that name; `false` is the rename-update onto it.
    creating: bool,
}

/// RULE: the create that takes a taken name and the update that RENAMES onto one are refused
/// with the one sentence that resource kind words the condition in, the requested name
/// withheld, and neither write changes what is stored.
///
/// The rows differ along the write that ran into the taken name and along how the second name
/// is spelled against the first (the same spelling, and the casing the store folds), so the
/// rule is driven over the condition rather than over one request.
#[tokio::test]
async fn one_sentence_answers_a_taken_name_whichever_write_asked_for_it() {
    // The rows: the write that asks for the taken name, and how that name is spelled.
    let rows = [
        Row {
            case: "a create of the taken name",
            asked_for: |taken| taken.to_string(),
            creating: true,
        },
        Row {
            case: "a create of the taken name in another casing",
            asked_for: |taken| taken.to_uppercase(),
            creating: true,
        },
        Row {
            case: "a rename onto the taken name",
            asked_for: |taken| taken.to_string(),
            creating: false,
        },
        Row {
            case: "a rename onto the taken name in another casing",
            asked_for: |taken| taken.to_uppercase(),
            creating: false,
        },
    ];

    for Row {
        case,
        asked_for,
        creating,
    } in rows
    {
        // A fresh emulator per row: the taken name, and a second database for the rename
        // rows to move.
        let router = emulator();
        let taken = "taken_database_2026";
        let mover = "mover_database_2026";
        given_database(&router, taken, "the definition the taken name holds").await;
        given_database(&router, mover, "the definition the mover holds").await;

        let name = asked_for(taken);
        let request = if creating {
            json!({ "DatabaseInput": { "Name": &name, "Description": "a refused definition" } })
        } else {
            json!({
                "Name": mover,
                "DatabaseInput": { "Name": &name, "Description": "a refused definition" },
            })
        };
        let operation = if creating {
            "CreateDatabase"
        } else {
            "UpdateDatabase"
        };
        let answered = assert_refusal(
            &router,
            case,
            operation,
            &request,
            ALREADY_EXISTS,
            DATABASE_ALREADY_EXISTS,
        )
        .await;
        assert_withholds(case, &error_message(&answered), &name);

        // Neither write reached the store: the taken name still holds its own definition and
        // the database a refused rename addressed is still where it was, with its own.
        for (resource, expected) in [
            (taken, "the definition the taken name holds"),
            (mover, "the definition the mover holds"),
        ] {
            let stored = description_of(&router, resource).await;
            assert_eq!(
                stored.as_deref(),
                Some(expected),
                "case {case:?}: a refused write must leave {resource:?} as it was, got {stored:?}"
            );
        }
    }
}
