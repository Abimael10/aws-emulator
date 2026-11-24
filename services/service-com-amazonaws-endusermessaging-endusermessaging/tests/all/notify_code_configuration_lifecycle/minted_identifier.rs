// SPDX-License-Identifier: CC0-1.0

//! The FORM of the identifier this service mints for a notify code configuration.
//!
//! RULE: a system-generated identifier is an OPAQUE token drawn from fresh entropy —
//! it carries the bare form this API's own examples write
//! (`ncc-abc12345678901234`, the `CreateNotifyCodeConfiguration` example;
//! `smithy_query(query="operation",operation="CreateNotifyCodeConfiguration")`), it is
//! unique, it is stable across every read of the resource, and nothing in it can be
//! predicted from another identifier the caller has already seen or from the order the
//! resources were created in. The ARN is that identifier under this service's
//! `notify-code-configuration` resource segment, so it follows the id.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch` (this suite's parent
//! harness), so the identifiers asserted on are the ones a real SDK client is handed.
//! The table spans what the rule generalises over: the create bodies differ in which
//! optional members they carry, in the name they take and in whether they carry an
//! idempotency token, and enough of them are minted in one run that a counted or
//! clock-seeded body would show up as a shared prefix or a one-digit step between
//! neighbours.

use serde_json::{json, Value};

use super::{Emulator, ACCOUNT_ID, CONFIGURATION_TYPE, SIGNING_NAME};

/// The resource prefix a bare `notifyCodeConfigurationId` carries, as the model's
/// `CreateNotifyCodeConfiguration` example writes one (`ncc-abc12345678901234`).
const ID_PREFIX: &str = "ncc-";

/// How many characters follow that prefix in the same example.
const ID_BODY_LENGTH: usize = 17;

/// `NotifyCodeConfigurationId`'s modelled bounds
/// (`smithy_query(query="shape",shape="NotifyCodeConfigurationId")`: `@length(1..64)`,
/// `@pattern ^[A-Za-z0-9_-]+$`).
const ID_MAX_LENGTH: usize = 64;

/// How many configurations one run mints, which is how many bodies the
/// unpredictability assertions compare.
const MINTED: usize = 12;

/// How many leading characters of two bodies must differ: a body that was a clock seed
/// plus a counter shares every character but its last few with every other body minted
/// in the same run, so comparing this many catches it. A uniformly drawn body of
/// `ID_BODY_LENGTH` characters collides here with vanishing probability.
const SHARED_PREFIX_LENGTH: usize = 9;

/// The create bodies this table spans: the rows differ in the name, in which optional
/// members they carry, and in whether the request carries an `@idempotencyToken`.
fn create_bodies() -> Vec<Value> {
    (0..MINTED)
        .map(|index| {
            let mut body = json!({
                "notifyCodeConfigurationName": format!("minted-{index}"),
            });
            if index % 2 == 0 {
                body["codeConfigurationParameters"] = json!({ "codeType": "NUMERIC" });
            }
            if index % 3 == 0 {
                body["channelParameters"] =
                    json!({ "text": { "inlineTemplateBody": "Your code is {{code}}" } });
            }
            if index % 4 == 0 {
                body["clientToken"] = json!(format!("token-{index}"));
            }
            body
        })
        .collect()
}

/// The body of a bare identifier: what follows the resource prefix.
fn body_of(case: &str, id: &str) -> String {
    id.strip_prefix(ID_PREFIX)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an id prefixed {ID_PREFIX:?}, got {id:?}")
        })
        .to_string()
}

/// Assert that `id` is written in the form this API's examples write a bare identifier
/// in, and inside the modelled shape's own bounds.
fn assert_identifier_form(case: &str, id: &str) {
    let body = body_of(case, id);
    assert_eq!(
        body.len(),
        ID_BODY_LENGTH,
        "case {case:?}: expected a body of {ID_BODY_LENGTH} characters, got {id:?} \
         ({} characters)",
        body.len()
    );
    assert!(
        body.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()),
        "case {case:?}: expected a lowercase alphanumeric body, got {id:?}"
    );
    assert!(
        !id.is_empty() && id.len() <= ID_MAX_LENGTH,
        "case {case:?}: expected an id inside the modelled 1..{ID_MAX_LENGTH} length, \
         got {id:?} ({} characters)",
        id.len()
    );
    assert!(
        id.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'),
        "case {case:?}: expected an id inside the modelled ^[A-Za-z0-9_-]+$ pattern, \
         got {id:?}"
    );
}

#[tokio::test]
async fn every_minted_identifier_is_opaque_unique_and_unpredictable() {
    let em = Emulator::new();
    let mut ids: Vec<String> = Vec::new();

    for body in create_bodies() {
        let case = body["notifyCodeConfigurationName"]
            .as_str()
            .expect("a named case")
            .to_string();
        let created = em.create(body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the create must be served, got {} / {:?}",
            created.status, created.body
        );
        let id = created.string("notifyCodeConfigurationId");
        assert_identifier_form(&case, &id);

        // The ARN is that identifier under this service's resource segment, for the
        // region the request was signed for.
        assert_eq!(
            created.string("notifyCodeConfigurationArn"),
            format!(
                "arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/{id}"
            ),
            "case {case:?}: the ARN must carry the minted id, got {:?}",
            created.body
        );

        // Stable: the id is a property of the resource, so every read of it reports the
        // same one — and the id reaches the resource it names.
        for read in [em.get(&id).await, em.get(&id).await] {
            assert_eq!(
                read.status, 200,
                "case {case:?}: the minted id must reach its configuration, got {} / {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.string("notifyCodeConfigurationId"),
                id,
                "case {case:?}: every read must report the one minted id, got {:?}",
                read.body
            );
        }

        ids.push(id);
    }

    for (index, id) in ids.iter().enumerate() {
        for other in ids.iter().skip(index + 1) {
            assert_ne!(
                id, other,
                "case ids minted in one run: two configurations must never share an \
                 identifier, got {id:?} twice"
            );
            let (body, other_body) = (
                body_of("minted", id),
                body_of("minted", other),
            );
            assert_ne!(
                &body[..SHARED_PREFIX_LENGTH],
                &other_body[..SHARED_PREFIX_LENGTH],
                "case ids minted in one run: a body drawn from fresh entropy must not \
                 share its first {SHARED_PREFIX_LENGTH} characters with another, got \
                 {id:?} and {other:?}"
            );
        }
    }
}
