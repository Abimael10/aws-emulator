// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `DeleteWebFunction` answers when the function the
//! request names holds no stored record: the operation states an END STATE — the
//! named function is not to exist — so a name nothing is stored under already
//! satisfies it and the call settles as the operation's modelled success (HTTP 204,
//! empty body, no error shape), which is what makes its modelled `@idempotent`
//! trait safe to retry on.
//!
//! Every assertion goes over the wire through the same [`Emulator`] harness the
//! `web_function_lifecycle` suite uses: a request is built, dispatched through the
//! production router, and only the HTTP status, the `x-amzn-errortype` header and
//! the JSON body are inspected. Nothing reaches into the service's state struct, so
//! a 204 that is not backed by a store the delete really left alone cannot pass.
//!
//! The tests are tables: the rows differ along HOW the named function misses (never
//! created, already deleted, differing only in case, extending or a prefix of a
//! stored name), which FORM named it (the bare name or the function ARN) and how
//! many times the call is repeated. The fixture's teardown delete of a function
//! that is already gone is one row.

use serde_json::{json, Value};

use super::web_function_lifecycle::{
    build_config, expected_arn, percent_encoded, role_arn, Emulator, Reply,
};

/// The region every request is signed for: the Region a function, and therefore its
/// ARN, is created in.
const REGION: &str = "us-east-1";

/// `DELETE /2025-03-07/web-functions/{functionName}`, addressing the function by
/// `identifier` — either form, percent-encoded as one URI segment.
async fn delete_function(emulator: &Emulator, identifier: &str) -> Reply {
    emulator
        .send(
            "DELETE",
            &format!("/web-functions/{}", percent_encoded(identifier)),
            REGION,
            None,
        )
        .await
}

/// `GET /2025-03-07/web-functions/{functionName}`.
async fn get_function(emulator: &Emulator, identifier: &str) -> Reply {
    emulator
        .send(
            "GET",
            &format!("/web-functions/{}", percent_encoded(identifier)),
            REGION,
            None,
        )
        .await
}

/// Create a web function with an initial revision and one tag, and return its
/// reported `functionArn`.
async fn create_function(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .send(
            "PUT",
            "/web-functions",
            REGION,
            Some(json!({
                "functionName": name,
                "tags": { "env": "prod" },
                "revisionConfig": {
                    "buildConfig": build_config("python3.13", "demo-bucket", "app.zip", None),
                    "serviceConfig": { "executionRoleArn": role_arn(name) },
                },
            })),
        )
        .await;
    assert_eq!(
        created.status, 202,
        "case {name:?}: CreateWebFunction expected 202, got {} with {:?}",
        created.status, created.body
    );
    created
        .string("functionArn")
        .unwrap_or_else(|| panic!("case {name:?}: the create reports a functionArn"))
}

/// Assert `reply` is this operation's modelled success: 204, an empty body and no
/// error shape at all.
fn assert_settled(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 204,
        "case {case}: expected HTTP 204, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.body,
        Value::Null,
        "case {case}: expected an empty body, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.error(),
        "",
        "case {case}: expected no error shape, got {:?}",
        reply.error()
    );
}

/// Assert the function `identifier` names is still stored and reports `name`.
async fn assert_still_stored(case: &str, emulator: &Emulator, identifier: &str, name: &str) {
    let reply = get_function(emulator, identifier).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: the untouched function expected 200, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.string("functionName").as_deref(),
        Some(name),
        "case {case}: the untouched function expected {name:?}, got {:?}",
        reply.body
    );
}

#[tokio::test]
async fn a_delete_of_a_function_that_holds_no_record_settles_as_the_modelled_success() {
    // RULE: `DeleteWebFunction` asks for the named function not to exist, so a name
    // this store holds no record under already satisfies the request and the call
    // settles as the operation's modelled success — HTTP 204 with an empty body and
    // no error shape — rather than reporting the function absent. The rows span how
    // the named function misses and which form named it.
    let emulator = Emulator::new();
    let held_name = "delete-idempotence-held";
    let held_arn = create_function(&emulator, held_name).await;

    let deleted_name = "delete-idempotence-gone";
    let deleted_arn = create_function(&emulator, deleted_name).await;
    assert_settled(
        "the first delete of a stored function",
        &delete_function(&emulator, deleted_name).await,
    );

    // Each row: what makes the named function miss, and the name it is named by.
    let rows: [(&str, &str); 5] = [
        ("a function that was never created", "never-created"),
        ("a function that was already deleted", deleted_name),
        (
            "a name that differs only in case",
            "Delete-Idempotence-Held",
        ),
        (
            "a name that extends the stored one",
            "delete-idempotence-held-2",
        ),
        ("a name that is a prefix of the stored one", "delete"),
    ];

    for (row, name) in rows {
        for (form, identifier) in [
            ("the name", name.to_string()),
            ("the ARN", expected_arn(REGION, name)),
        ] {
            let case = format!("{row}, addressed by {form} ({identifier})");
            assert_settled(&case, &delete_function(&emulator, &identifier).await);
            // The settled delete removed nothing: the function that IS stored is
            // still there, under both forms of its own identifier.
            assert_still_stored(&case, &emulator, held_name, held_name).await;
            assert_still_stored(&case, &emulator, &held_arn, held_name).await;
        }
    }

    // The control: the function that was deleted stays deleted, and its READER
    // still reports it absent — the delete's 204 is about the end state it asks
    // for, not about the function being there.
    let gone = get_function(&emulator, &deleted_arn).await;
    assert_eq!(
        gone.status, 404,
        "case the deleted function's reader: expected HTTP 404, got {} with {:?}",
        gone.status, gone.body
    );
    assert_eq!(
        gone.error(),
        "ResourceNotFoundException",
        "case the deleted function's reader: expected ResourceNotFoundException, got {:?}",
        gone.body
    );
}

#[tokio::test]
async fn repeating_a_delete_converges_and_a_name_can_be_created_again_after_it() {
    // RULE: the delete is a retry-safe statement of the end state, so repeating it
    // any number of times settles the same way and changes nothing else: the
    // function stays gone, every other function stays stored, and the name is free
    // for a create again — whose function a later delete removes exactly once more.
    // The rows span how many times the call is repeated and which form addresses it.
    let cases: [(&str, usize); 3] = [
        ("deleted once more", 1),
        ("deleted twice more", 2),
        ("deleted four times more", 4),
    ];

    for (case, repeats) in cases {
        let emulator = Emulator::new();
        let name = "delete-idempotence-repeat";
        let arn = create_function(&emulator, name).await;
        let neighbour = "delete-idempotence-neighbour";
        create_function(&emulator, neighbour).await;

        assert_settled(
            &format!("{case}: the delete that removes the record"),
            &delete_function(&emulator, name).await,
        );

        for repeat in 0..repeats {
            // Alternate the form the repeat addresses the function by, so both
            // resolutions reach the same settled answer.
            let identifier = if repeat % 2 == 0 {
                arn.clone()
            } else {
                name.to_string()
            };
            assert_settled(
                &format!("{case}: repeat {repeat} addressed by {identifier}"),
                &delete_function(&emulator, &identifier).await,
            );
        }

        // Nothing else moved: the neighbour is still stored, and the listing holds
        // only it.
        assert_still_stored(case, &emulator, neighbour, neighbour).await;
        let listed = emulator
            .send("POST", "/web-functions", REGION, Some(json!({})))
            .await;
        let names: Vec<String> = listed
            .field("functions")
            .and_then(Value::as_array)
            .map(|functions| {
                functions
                    .iter()
                    .filter_map(|function| {
                        function
                            .get("functionName")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names,
            vec![neighbour.to_string()],
            "case {case}: the repeats removed only the deleted function, got {:?}",
            listed.body
        );

        // The name is free again: a create stores a fresh record under it, which the
        // reader reports and which the next delete removes once.
        let recreated = create_function(&emulator, name).await;
        assert_eq!(
            recreated, arn,
            "case {case}: the function is created again under the same ARN, got {recreated:?}"
        );
        assert_still_stored(case, &emulator, name, name).await;
        assert_settled(
            &format!("{case}: the delete of the re-created function"),
            &delete_function(&emulator, name).await,
        );
        let gone = get_function(&emulator, name).await;
        assert_eq!(
            gone.status, 404,
            "case {case}: the re-created function is gone again, got {} with {:?}",
            gone.status, gone.body
        );
    }
}
