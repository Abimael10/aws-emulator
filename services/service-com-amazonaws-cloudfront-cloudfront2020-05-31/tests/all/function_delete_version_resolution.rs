// SPDX-License-Identifier: CC0-1.0

//! What `DeleteFunction` resolves its `If-Match` to, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! RULE: the delete resolves the submitted version against the versions THE FUNCTION ITSELF
//! has held — the current version of either of its stages, and every version a later update
//! superseded. A header that resolves to one of them, but not to the version the delete is
//! checked against, is a precondition that evaluated to false: `PreconditionFailed`, HTTP
//! 412. A header that resolves to NOTHING — it carries no version token at all, or a token
//! this function never stood at — is the other half of the error CloudFront words as "The
//! If-Match version is missing or not valid for the resource": `InvalidIfMatchVersion`, HTTP
//! 400. Either way the function stays exactly as it was, and the delete at its current
//! version then removes it.
//!
//! The rows differ along what the rule generalises over: whether the submitted token is one
//! the function held (the version an update superseded, the version a publish copied into the
//! other stage, the version two updates ago) or one it never held (a token minted for another
//! function, a well-formed token nothing holds, the current token with one character moved,
//! the current token's own prefix, a value that is no version token at all), how many updates
//! the function has been through, and whether it has been published.
//!
//! [`crate::function_delete`] tables the rest of the delete; this suite is only about which
//! of its two version refusals a header earns.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, Answer};
use crate::function_delete::delete_function;
use crate::function_lifecycle::{create_key_value_store, create_table, describe_function, Create};
use crate::function_publish::{create_one, publish_function};
use crate::function_update::update_function;

/// The refusal a header that resolves to a version the function HELD earns, and the one a
/// header that resolves to no version of it earns — the two halves of the rule, as the status
/// and the error code they answer with.
const STALE: (u16, &str) = (412, "PreconditionFailed");
const UNRESOLVED: (u16, &str) = (400, "InvalidIfMatchVersion");

/// The sentence each refusal carries, spelled out here so a wrong constant in the crate
/// cannot make these assertions pass. Both are fixed text: neither names the version the
/// header carried, the function, or anything else from the request.
const PRECONDITION_FAILED_MESSAGE: &str =
    "The request failed because it didn't meet the preconditions in one or more \
     request-header fields.";
const INVALID_IF_MATCH_VERSION_MESSAGE: &str =
    "The If-Match version is missing or not valid for the resource.";

/// A version token that is well formed — `E` + 13 characters of the shape this service mints
/// — but that no function in a fresh emulator has ever held.
const UNHELD_VERSION_TOKEN: &str = "E2ZYXWVUTSRQPO";

/// How the submitted header is built for one row.
enum Token {
    /// From the versions this function has held: the version its create handed back (index 0)
    /// and the one each later update handed back (1, 2, …), rendered as the row writes it.
    Held(usize, fn(&str) -> String),
    /// A value of its own, which no version of this function can be.
    Literal(String),
}

/// One row: what the header carries, how many updates the function has been through before
/// the delete, whether it has been published, and which refusal the delete must answer.
struct Row {
    case: &'static str,
    /// How many `UpdateFunction` calls run before the delete, each minting a version of its
    /// own.
    updates: usize,
    /// Whether the function is published before the delete, which gives it a second stage
    /// standing at the version the publish copied.
    published: bool,
    token: Token,
    refusal: (u16, &'static str),
}

/// The same create under another name, so a row runs against a function of its own.
fn renamed(create: &Create, name: &str) -> Create {
    let mut renamed = create.clone();
    renamed.name = name.to_string();
    renamed
}

/// The same function with a different comment, so an update of it really replaces what the
/// create stored and mints a version of its own.
fn edited(create: &Create, round: usize) -> Create {
    let mut edited = create.clone();
    edited.comment = format!("{} (round {round})", create.comment);
    edited
}

/// The version token the `DEVELOPMENT` stage of `name` stands at.
async fn current_version(router: &MultiServiceRouter, name: &str) -> String {
    let described = describe_function(router, name, None).await;
    assert_eq!(
        described.status, 200,
        "the describe of {name} must answer HTTP 200, got {} with {}",
        described.status, described.raw
    );
    described.header("etag").to_string()
}

/// The current token with its last character moved on, which is the shape a client gets by
/// editing a token it read: well formed, and a version nothing minted.
fn mutated(current: &str) -> String {
    let mut body: Vec<char> = current.chars().collect();
    let last = body.len() - 1;
    body[last] = if body[last] == '0' { '1' } else { '0' };
    body.into_iter().collect()
}

/// Assert `answer` is the refusal `(status, code)` with the sentence that refusal carries, and
/// nothing else.
fn assert_refused(answer: &Answer, case: &str, sent: &str, refusal: (u16, &str)) {
    let (status, code) = refusal;
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status} deleting with {sent:?}, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?}: expected {code} deleting with {sent:?}, got {}",
        answer.raw
    );
    let expected = if code == STALE.1 {
        PRECONDITION_FAILED_MESSAGE
    } else {
        INVALID_IF_MATCH_VERSION_MESSAGE
    };
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
}

/// Every row the rule is asserted over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the version an update superseded",
            updates: 1,
            published: false,
            token: Token::Held(0, |held| held.to_string()),
            refusal: STALE,
        },
        Row {
            case: "the version an update superseded, quoted as an entity tag",
            updates: 1,
            published: false,
            token: Token::Held(0, |held| format!("\"{held}\"")),
            refusal: STALE,
        },
        Row {
            case: "the version two updates ago",
            updates: 2,
            published: false,
            token: Token::Held(0, |held| held.to_string()),
            refusal: STALE,
        },
        Row {
            case: "the version in between two updates",
            updates: 2,
            published: false,
            token: Token::Held(1, |held| held.to_string()),
            refusal: STALE,
        },
        Row {
            case: "the version a publish copied into the other stage, superseded since",
            updates: 1,
            published: true,
            token: Token::Held(0, |held| held.to_string()),
            refusal: STALE,
        },
        Row {
            case: "a well-formed token no function has ever held",
            updates: 1,
            published: false,
            token: Token::Literal(UNHELD_VERSION_TOKEN.to_string()),
            refusal: UNRESOLVED,
        },
        Row {
            case: "the current token with one character moved",
            updates: 1,
            published: false,
            token: Token::Held(1, mutated),
            refusal: UNRESOLVED,
        },
        Row {
            case: "a superseded token with one character moved",
            updates: 1,
            published: false,
            token: Token::Held(0, mutated),
            refusal: UNRESOLVED,
        },
        Row {
            case: "the current token's own prefix",
            updates: 1,
            published: false,
            token: Token::Held(1, |held| held[..held.len() - 1].to_string()),
            refusal: UNRESOLVED,
        },
        Row {
            case: "a value that is no version token at all",
            updates: 1,
            published: false,
            token: Token::Literal("not-a-version".to_string()),
            refusal: UNRESOLVED,
        },
    ]
}

/// A header that resolves to a version the function HELD is a failed precondition; one that
/// resolves to no version of it is missing or not valid for the resource. Neither removes the
/// function, and the delete at its current version then does.
#[tokio::test]
async fn a_delete_tells_a_version_the_function_held_from_one_it_never_held() {
    let router = emulator();
    let kvs_arn = create_key_value_store(&router, "function_delete_version_store").await;
    let table = create_table(&kvs_arn);
    let seed = table
        .first()
        .cloned()
        .expect("the create table carries at least one row");

    for (index, row) in rows().into_iter().enumerate() {
        let create = renamed(&seed, &format!("delete_version_probe_{index}"));
        // Every version this function holds, in the order it held them: the create's, then
        // each update's.
        let mut held = vec![create_one(&router, &create).await];
        if row.published {
            let published = publish_function(&router, &create.name, &held[0]).await;
            assert_eq!(
                published.status, 200,
                "case {:?}: the seed publish must answer HTTP 200, got {} with {}",
                row.case, published.status, published.raw
            );
        }
        for round in 0..row.updates {
            let edit = edited(&create, round);
            let current = held.last().expect("the create's version is held").clone();
            let updated = update_function(&router, &create.name, &edit, Some(&current)).await;
            assert_eq!(
                updated.status, 200,
                "case {:?}: the seed update must answer HTTP 200, got {} with {}",
                row.case, updated.status, updated.raw
            );
            held.push(current_version(&router, &create.name).await);
        }
        let current = current_version(&router, &create.name).await;
        assert_eq!(
            Some(current.as_str()),
            held.last().map(String::as_str),
            "case {:?}: the function must stand at the version its last mutation handed back",
            row.case
        );

        let sent = match &row.token {
            Token::Held(position, render) => render(&held[*position]),
            Token::Literal(literal) => literal.clone(),
        };
        let refused = delete_function(&router, &create.name, &sent).await;
        assert_refused(&refused, row.case, &sent, row.refusal);

        // A refused delete removes nothing: the function still describes, at the version it
        // stood at — and the delete that carries that version removes it.
        let still = current_version(&router, &create.name).await;
        assert_eq!(
            still, current,
            "case {:?}: a refused delete must leave the function at its version, got {still:?}",
            row.case
        );
        let deleted = delete_function(&router, &create.name, &current).await;
        assert_eq!(
            deleted.status, 204,
            "case {:?}: expected HTTP 204 deleting at the current version, got {} with {}",
            row.case, deleted.status, deleted.raw
        );
        let gone = describe_function(&router, &create.name, None).await;
        assert_eq!(
            gone.status, 404,
            "case {:?}: the deleted function must be gone, got {} with {}",
            row.case, gone.status, gone.raw
        );
    }
}
