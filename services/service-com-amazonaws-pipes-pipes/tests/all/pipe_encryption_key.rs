// SPDX-License-Identifier: CC0-1.0

//! The customer managed key a pipe is encrypted under, end to end over the wire, with KMS
//! mounted beside this service on one shared engine so the key a pipe names is the very key
//! KMS's own `CreateKey`/`CreateAlias` minted.
//!
//! The rule under test: `KmsKeyIdentifier` names a KMS resource ("The identifier can be the
//! key Amazon Resource Name (ARN), KeyId, key alias, or key alias ARN",
//! `smithy_query(query="doc",shape="CreatePipeRequest",member="KmsKeyIdentifier")`) whose
//! `@length`/`@pattern` constraints say nothing about whether a key exists, so a write that
//! would bind a pipe to it resolves the identifier in KMS first and refuses an identifier KMS
//! holds no key for with the modelled `ValidationException` (HTTP 400) — "The provided KMS
//! Key does not exist." — writing nothing; an identifier KMS DOES resolve is stored and read
//! back. A lookup that says nothing about the key refuses nothing.
//!
//! The tables span what the rule generalises over rather than one recorded call: each form
//! the member takes (a key id, a key ARN, an alias name, an alias ARN), each write that
//! accepts the member (`CreatePipe` and `UpdatePipe`), and the answers that establish nothing
//! — no KMS beside this service, and a key ARN of another account, which is not this
//! emulator's to hold.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_lifecycle::{
    call, create, describe, list, listed_names, minimal_body, router, Reply, REGION,
};
use crate::pipe_update::update;

const SOURCE: &str = "arn:aws:sqs:us-west-2:000000000000:orders";
const TARGET: &str = "arn:aws:sqs:us-west-2:000000000000:results";

/// The exception every refusal in this suite carries, and the sentence it carries: FIXED
/// text for the class, with no part taken from the request.
const VALIDATION: &str = "ValidationException";
const KEY_NOT_FOUND: &str = "The provided KMS Key does not exist.";

/// How long a wait for the state a pipe's move settles into is given before it is a failure:
/// generous, because nothing here tests how fast a move lands — only that a settled pipe is
/// the one the next call is made against.
const SETTLE_BUDGET: Duration = Duration::from_secs(60);

/// How long between two reads while waiting for a move to land.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// Wait for the pipe `name` to report `want` as its `CurrentState`, LOOKING AGAIN until it
/// does or the budget lapses, and assert it got there — so what follows is made against a
/// pipe whose last move has landed rather than against the clock.
async fn settled_in(case: &str, router: &MultiServiceRouter, name: &str, want: &str) {
    let deadline = Instant::now() + SETTLE_BUDGET;
    let seen = loop {
        let seen = describe(router, name).await.string("CurrentState");
        if seen == want || Instant::now() >= deadline {
            break seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        seen, want,
        "case {case:?}: expected {name:?} to settle in {want:?}, it last reported {seen:?}"
    );
}

/// This service beside KMS on one shared engine, so a pipe's `KmsKeyIdentifier` is resolved
/// against the very keys KMS holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One KMS call, the way an SDK sends it: awsJson1_1 under KMS's own target prefix and
/// signing name. Answered as the status and the parsed body.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("kms.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDPIPESKMS/20260101/{REGION}/kms/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// A string at `pointer` of a KMS answer.
fn at(payload: &Value, pointer: &str) -> String {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A key of the account, answered as its key id and its key ARN.
async fn given_key(router: &MultiServiceRouter, description: &str) -> (String, String) {
    let (status, created) = kms(router, "CreateKey", &json!({ "Description": description })).await;
    assert_eq!(
        status, 200,
        "KMS CreateKey must answer 200, got {status} with {created}"
    );
    (
        at(&created, "/KeyMetadata/KeyId"),
        at(&created, "/KeyMetadata/Arn"),
    )
}

/// An alias of the account pointing at `key_id`, answered as its alias ARN — built the way
/// KMS addresses an alias, from the account the emulator serves.
async fn given_alias(router: &MultiServiceRouter, alias: &str, key_id: &str) -> String {
    let (status, answer) = kms(
        router,
        "CreateAlias",
        &json!({ "AliasName": alias, "TargetKeyId": key_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "KMS CreateAlias({alias}) must answer 200, got {status} with {answer}"
    );
    format!(
        "arn:aws:kms:{REGION}:{}:{alias}",
        engine::emulator_account_id()
    )
}

/// A `CreatePipe` body naming a key, in the state the recording creates a pipe in.
fn body_with_key(key: &str) -> Value {
    let mut body = minimal_body(SOURCE, TARGET);
    body["DesiredState"] = json!("STOPPED");
    body["KmsKeyIdentifier"] = json!(key);
    body
}

/// Every name `ListPipes` reports.
async fn all_names(router: &MultiServiceRouter) -> Vec<String> {
    let listed = list(router, "").await;
    assert_eq!(
        listed.status, 200,
        "ListPipes must answer 200, got {listed:?}"
    );
    listed_names(&listed)
}

/// Assert `reply` is the refusal a key that does not exist is answered with, and that it
/// wrote nothing: `name` is absent from `ListPipes` and `DescribePipe` does not find it.
async fn refused_and_wrote_nothing(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    reply: &Reply,
) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: a key that does not exist must be refused with HTTP 400, got {reply:?}"
    );
    assert_eq!(
        reply.error_code, VALIDATION,
        "case {case:?}: expected {VALIDATION}, got {:?} in {reply:?}",
        reply.error_code
    );
    assert_eq!(
        reply.message(),
        KEY_NOT_FOUND,
        "case {case:?}: expected the message {KEY_NOT_FOUND:?}, got {:?}",
        reply.message()
    );
    let names = all_names(router).await;
    assert!(
        !names.contains(&name.to_string()),
        "case {case:?}: ListPipes must not report {name:?}, got {names:?}"
    );
    let described = describe(router, name).await;
    assert_eq!(
        described.status, 404,
        "case {case:?}: DescribePipe must not find {name:?}, got {described:?}"
    );
}

/// The identifiers that name a key the account does NOT hold, one per form the member takes.
/// Source: the `KmsKeyIdentifier` member documentation (a key ARN, a KeyId, a key alias, a
/// key alias ARN).
fn unresolvable_identifiers() -> Vec<(&'static str, String)> {
    let account = engine::emulator_account_id();
    vec![
        (
            "key ARN of no key",
            format!("arn:aws:kms:{REGION}:{account}:key/11111111-2222-3333-4444-555555555555"),
        ),
        (
            "key id of no key",
            "66666666-7777-8888-9999-000000000000".to_string(),
        ),
        ("alias no alias holds", "alias/no-such-pipe-key".to_string()),
        (
            "alias ARN of no alias",
            format!("arn:aws:kms:{REGION}:{account}:alias/no-such-pipe-key"),
        ),
        (
            // The same uuid as a key this account DOES hold would still name no key at
            // another endpoint: a key ARN addresses one key in one Region.
            "key ARN of another Region",
            format!("arn:aws:kms:us-east-1:{account}:key/11111111-2222-3333-4444-555555555555"),
        ),
    ]
}

/// RULE: a write naming a `KmsKeyIdentifier` KMS resolves to a key is accepted and the
/// identifier is stored as sent, in every form the member takes.
#[tokio::test]
async fn create_pipe_accepts_every_form_of_a_key_that_exists() {
    let router = emulator();
    let (key_id, key_arn) = given_key(&router, "pipes encryption key").await;
    let alias = "alias/pipes-encryption";
    let alias_arn = given_alias(&router, alias, &key_id).await;

    // The rows differ along the FORM of the identifier, which is what the rule generalises
    // over; each resolves to the same key.
    let rows = [
        ("key id", key_id.clone()),
        ("key ARN", key_arn.clone()),
        ("alias name", alias.to_string()),
        ("alias ARN", alias_arn.clone()),
    ];

    for (index, (case, identifier)) in rows.iter().enumerate() {
        let name = format!("keyed{index}");
        let created = create(&router, &name, body_with_key(identifier)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: a key KMS resolves must be accepted, got {created:?}"
        );

        let described = describe(&router, &name).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribePipe must report the created pipe, got {described:?}"
        );
        assert_eq!(
            described.string("KmsKeyIdentifier"),
            *identifier,
            "case {case:?}: DescribePipe must report the identifier as sent, got {:?}",
            described.member("KmsKeyIdentifier")
        );
        let names = all_names(&router).await;
        assert!(
            names.contains(&name),
            "case {case:?}: ListPipes must report {name:?}, got {names:?}"
        );
    }
}

/// RULE: a write naming a `KmsKeyIdentifier` KMS holds no key for is refused with the
/// modelled `ValidationException` (HTTP 400) carrying AWS's sentence for the class, and
/// nothing is created.
#[tokio::test]
async fn create_pipe_refuses_a_key_that_does_not_exist() {
    let router = emulator();
    // A key the account DOES hold, so the refusals below are about the identifier and not
    // about an account with no keys at all.
    let (_, key_arn) = given_key(&router, "an unrelated key").await;

    for (index, (case, identifier)) in unresolvable_identifiers().into_iter().enumerate() {
        let name = format!("unresolvable{index}");
        let reply = create(&router, &name, body_with_key(&identifier)).await;
        refused_and_wrote_nothing(case, &router, &name, &reply).await;
    }

    // The control: the same request with a key that exists is accepted, so the refusals
    // above are the key's doing and not the fixture's.
    let accepted = create(&router, "control", body_with_key(&key_arn)).await;
    assert_eq!(
        accepted.status, 200,
        "the same create with a key that exists must be accepted, got {accepted:?}"
    );
}

/// RULE: the same holds for `UpdatePipe` — an update naming a key that does not exist is
/// refused and the pipe keeps the key it already carried, while a key that exists replaces
/// it and the empty string drops it.
#[tokio::test]
async fn update_pipe_refuses_a_key_that_does_not_exist_and_keeps_the_stored_one() {
    let router = emulator();
    let (first_id, first_arn) = given_key(&router, "the key the pipe is created with").await;
    let (_, second_arn) = given_key(&router, "the key the update moves to").await;
    let name = "updatekey";

    let created = create(&router, name, body_with_key(&first_arn)).await;
    assert_eq!(
        created.status, 200,
        "the fixture pipe must be created, got {created:?}"
    );
    // The pipe is created `CREATING` and reaches `STOPPED` a moment later; the updates below
    // are made against the settled pipe, so each is waited FOR rather than waited OUT.
    settled_in("the fixture pipe", &router, name, "STOPPED").await;

    for (case, identifier) in unresolvable_identifiers() {
        let reply = update(
            &router,
            name,
            json!({
                "RoleArn": "arn:aws:iam::000000000000:role/pipes-role",
                "KmsKeyIdentifier": identifier,
            }),
        )
        .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: UpdatePipe must refuse a key that does not exist, got {reply:?}"
        );
        assert_eq!(
            reply.error_code, VALIDATION,
            "case {case:?}: expected {VALIDATION}, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message(),
            KEY_NOT_FOUND,
            "case {case:?}: expected the message {KEY_NOT_FOUND:?}, got {:?}",
            reply.message()
        );
        let described = describe(&router, name).await;
        assert_eq!(
            described.string("KmsKeyIdentifier"),
            first_arn,
            "case {case:?}: the refused update must leave the stored key alone, got {:?}",
            described.member("KmsKeyIdentifier")
        );
    }

    // A key that exists replaces the stored one — by its id as well as by its ARN, so the
    // accept is about the ANSWER and not about the form.
    for (case, identifier) in [("key ARN", second_arn.clone()), ("key id", first_id.clone())] {
        let updated = update(
            &router,
            name,
            json!({
                "RoleArn": "arn:aws:iam::000000000000:role/pipes-role",
                "KmsKeyIdentifier": identifier,
            }),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: a key KMS resolves must be accepted, got {updated:?}"
        );
        settled_in(case, &router, name, "STOPPED").await;
        let described = describe(&router, name).await;
        assert_eq!(
            described.string("KmsKeyIdentifier"),
            identifier,
            "case {case:?}: DescribePipe must report the key the update stored, got {:?}",
            described.member("KmsKeyIdentifier")
        );
    }

    // The empty string names no key at all — it is how a caller returns the pipe to the
    // Amazon Web Services owned key — so it is never resolved and never refused.
    let cleared = update(
        &router,
        name,
        json!({
            "RoleArn": "arn:aws:iam::000000000000:role/pipes-role",
            "KmsKeyIdentifier": "",
        }),
    )
    .await;
    assert_eq!(
        cleared.status, 200,
        "an empty KmsKeyIdentifier must be accepted, got {cleared:?}"
    );
    settled_in("cleared", &router, name, "STOPPED").await;
    let described = describe(&router, name).await;
    assert_eq!(
        described.member("KmsKeyIdentifier"),
        &Value::Null,
        "an empty KmsKeyIdentifier must leave the pipe reporting no key, got {:?}",
        described.member("KmsKeyIdentifier")
    );
}

/// RULE: a refusal is grounded in KMS stating that the identifier resolves to no key, so a
/// lookup that says nothing about it refuses nothing — with no KMS beside this service, and
/// for a key of another account, which is not this emulator's to hold.
#[tokio::test]
async fn a_lookup_that_says_nothing_about_the_key_refuses_nothing() {
    let account = engine::emulator_account_id();
    // Rows differ along WHY nothing was established: no KMS to ask at all (rows 0..3, over
    // every form the member takes) and a key KMS would not answer about (row 4).
    let unknowable = [
        format!("arn:aws:kms:{REGION}:{account}:key/11111111-2222-3333-4444-555555555555"),
        "66666666-7777-8888-9999-000000000000".to_string(),
        "alias/no-such-pipe-key".to_string(),
        format!("arn:aws:kms:{REGION}:{account}:alias/no-such-pipe-key"),
    ];
    let without_kms = router();
    for (index, identifier) in unknowable.iter().enumerate() {
        let name = format!("nokms{index}");
        let created = create(&without_kms, &name, body_with_key(identifier)).await;
        assert_eq!(
            created.status, 200,
            "case {identifier:?}: with no KMS to ask, the create must not be refused, got \
             {created:?}"
        );
        let described = describe(&without_kms, &name).await;
        assert_eq!(
            described.string("KmsKeyIdentifier"),
            *identifier,
            "case {identifier:?}: the accepted create must be readable, got {described:?}"
        );
    }

    // A key ARN of ANOTHER account, with KMS mounted: this emulator does not hold that
    // account's keys, so their absence here says nothing about the key.
    let with_kms = emulator();
    let other_account =
        "arn:aws:kms:us-west-2:999988887777:key/11111111-2222-3333-4444-555555555555";
    let created = create(&with_kms, "otheraccount", body_with_key(other_account)).await;
    assert_eq!(
        created.status, 200,
        "a key of another account must not be refused, got {created:?}"
    );
    let described = describe(&with_kms, "otheraccount").await;
    assert_eq!(
        described.string("KmsKeyIdentifier"),
        other_account,
        "the accepted create must be readable, got {described:?}"
    );
}

/// The one call this suite makes that is neither a create nor an update: the refusal body a
/// client reads, asserted as the wire carries it — the modelled status, the error code and
/// the message, with no `fieldList` for a value that is well formed and names nothing.
#[tokio::test]
async fn the_refusal_body_carries_only_the_sentence_for_the_class() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let identifier = format!("arn:aws:kms:{REGION}:{account}:key/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    let reply = call(
        &router,
        "POST",
        "/v1/pipes/refusalbody",
        Some(body_with_key(&identifier)),
        REGION,
    )
    .await;
    assert_eq!(
        reply.status, 400,
        "the refusal must be HTTP 400, got {reply:?}"
    );
    assert_eq!(
        reply.error_code, VALIDATION,
        "the refusal must be a {VALIDATION}, got {:?}",
        reply.error_code
    );
    assert_eq!(
        reply.message(),
        KEY_NOT_FOUND,
        "the refusal must carry {KEY_NOT_FOUND:?}, got {:?}",
        reply.message()
    );
    assert_eq!(
        reply.member("fieldList"),
        &Value::Null,
        "the refusal must carry no fieldList, got {:?}",
        reply.member("fieldList")
    );
}
