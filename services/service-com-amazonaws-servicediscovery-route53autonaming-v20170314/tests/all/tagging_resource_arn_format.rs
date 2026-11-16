// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the shape a tagging call's `ResourceARN` must have — the rule
//! that decides, before any lookup, whether a tagging call is refused as bad INPUT or as
//! an ABSENT resource.
//!
//! The rule under test: `ListTagsForResource`, `TagResource` and `UntagResource` each
//! declare `InvalidInput` beside `ResourceNotFoundException`
//! (`smithy_query(query="operation", operation="ListTagsForResource,TagResource,UntagResource")`),
//! and the two answer different questions. An `AmazonResourceName` whose resource portion
//! is not one of this service's two taggable forms — `namespace/` followed by a namespace
//! id, or `service/` followed by a service id, each the kind prefix plus the sixteen
//! lowercase-alphanumeric characters every Cloud Map identifier carries — names no
//! resource at any point in time, so it is refused as `InvalidInput` with the service's
//! one fixed resource-format sentence, and nothing is read or written. An ARN of that form
//! that resolves to nothing is the other question: the resource is absent
//! (`ResourceNotFoundException`). Both classes answer HTTP 400, as every client-class
//! refusal of this service does (`client_error_wire_shape.rs`). What an ARN itself is —
//! its six fields, its non-empty partition and vendor — is the tree's one shared parse, so
//! an ARN of another vendor is the same bad request: it addresses nothing in this
//! service's namespace.
//!
//! The rows below span what the rule generalises over: the resource kind, the identifier's
//! length either side of the minted width, its character repertoire, the kind prefix and
//! the id prefix disagreeing, a resource kind this service does not tag, an ARN with no
//! resource portion at all, another vendor's ARN, an ARN with no partition, and a
//! reference that is not an ARN — on all three operations.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloud_map_core::{emulator, namespace, ok, refusal, service, REGION};

/// The sentence a malformed `ResourceARN` is refused with. Fixed text: it states the two
/// resource prefixes and one example identifier of each, and echoes nothing of the
/// request — so it is written out here rather than read back from the service.
const INVALID_RESOURCE_FORMAT: &str = "Invalid resource format in ARN. Resource must start \
     with 'service/' or 'namespace/' followed by a valid identifier. Example: \
     'namespace/ns-a2b3c4d5e6f7g2h3' or 'service/srv-b2c3d4e5f6g7h2i3'";

/// An identifier of the minted width — sixteen lowercase alphanumerics — that nothing
/// created, for the rows that must reach the store and miss.
const UNUSED_ID_BODY: &str = "0000000000000000";

/// An ARN of this service's own form naming `resource`.
fn arn(resource: &str) -> String {
    format!(
        "arn:aws:servicediscovery:{REGION}:{}:{resource}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The three tagging calls against `resource_arn`, each as (operation, request body).
fn tagging_calls(resource_arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "TagResource",
            json!({ "ResourceARN": resource_arn, "Tags": [{ "Key": "k", "Value": "v" }] }),
        ),
        (
            "UntagResource",
            json!({ "ResourceARN": resource_arn, "TagKeys": ["k"] }),
        ),
        (
            "ListTagsForResource",
            json!({ "ResourceARN": resource_arn }),
        ),
    ]
}

/// The status, wire error code and message text of one refused call, through the shared
/// harness's reader of a refusal ([`super::cloud_map_core::refusal`]), with an absent
/// message read as the empty text so a row can state the sentence it expects.
async fn refused(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, String, String) {
    let (status, code, message) = refusal(router, operation, body).await;
    (status, code, message.unwrap_or_default())
}

/// Every resource portion that is not one of this service's two taggable identifier forms
/// is refused as `InvalidInput`, with the one fixed sentence, on all three tagging
/// operations.
#[tokio::test]
async fn a_resource_portion_that_is_not_a_taggable_identifier_is_invalid_input() {
    let router = emulator();

    // (case, the ResourceARN the call names)
    let malformed: Vec<(&str, String)> = vec![
        (
            "a namespace id one character short of the minted width",
            arn("namespace/ns-a2b3c4d5e6f7g2h"),
        ),
        (
            "a namespace id one character past the minted width",
            arn("namespace/ns-a2b3c4d5e6f7g2h3x"),
        ),
        (
            "a word where the namespace identifier belongs",
            arn("namespace/ns-doesnotexist000"),
        ),
        (
            "a service id one character short of the minted width",
            arn("service/srv-b2c3d4e5f6g7h2i"),
        ),
        (
            "a service id one character past the minted width",
            arn("service/srv-b2c3d4e5f6g7h2i3x"),
        ),
        ("no identifier behind the kind prefix", arn("namespace/")),
        (
            "an identifier with no kind prefix",
            arn(&format!("ns-{UNUSED_ID_BODY}")),
        ),
        (
            "the service id prefix under the namespace kind",
            arn(&format!("namespace/srv-{UNUSED_ID_BODY}")),
        ),
        (
            "the namespace id prefix under the service kind",
            arn(&format!("service/ns-{UNUSED_ID_BODY}")),
        ),
        (
            "a resource kind this service does not tag",
            arn(&format!("instance/i-{UNUSED_ID_BODY}")),
        ),
        (
            "uppercase in the identifier",
            arn("namespace/ns-A2B3C4D5E6F7G2H3"),
        ),
        (
            "punctuation in the identifier",
            arn("namespace/ns-a2b3c4d5e6f7g2h."),
        ),
        (
            "a path segment behind the identifier",
            arn(&format!("namespace/ns-{UNUSED_ID_BODY}/alias")),
        ),
        (
            "a colon where the resource separator belongs",
            arn(&format!("namespace:ns-{UNUSED_ID_BODY}")),
        ),
        (
            "an ARN with no resource portion at all",
            format!(
                "arn:aws:servicediscovery:{REGION}:{}",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
        (
            "a bare identifier, not an ARN",
            format!("ns-{UNUSED_ID_BODY}"),
        ),
        ("a resource portion with no ARN around it", "namespace/ns-a2b3c4d5e6f7g2h3".to_string()),
        (
            // Another vendor's ARN addresses nothing in this service's namespace, which is
            // the same bad request as a malformed resource.
            "a taggable resource form under another vendor",
            format!(
                "arn:aws:s3:{REGION}:{}:namespace/ns-{UNUSED_ID_BODY}",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
        (
            "an ARN with no partition",
            format!(
                "arn::servicediscovery:{REGION}:{}:namespace/ns-{UNUSED_ID_BODY}",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
    ];

    for (case, resource_arn) in malformed {
        for (operation, body) in tagging_calls(&resource_arn) {
            let (status, code, message) = refused(&router, operation, body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidInput"),
                "case {case:?} ({resource_arn}) on {operation}: expected 400 InvalidInput, \
                 got {status} {code} {message:?}"
            );
            assert_eq!(
                message, INVALID_RESOURCE_FORMAT,
                "case {case:?} on {operation}: expected the fixed resource-format sentence, \
                 got {message:?}"
            );
        }
    }
}

/// An ARN of the taggable form that resolves to nothing is the OTHER question: the
/// resource is absent, on both taggable kinds and all three operations.
#[tokio::test]
async fn a_taggable_identifier_nothing_created_is_not_found() {
    let router = emulator();

    for (kind, resource_arn) in [
        ("namespace", arn(&format!("namespace/ns-{UNUSED_ID_BODY}"))),
        ("service", arn(&format!("service/srv-{UNUSED_ID_BODY}"))),
    ] {
        for (operation, body) in tagging_calls(&resource_arn) {
            let (status, code, message) = refused(&router, operation, body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "ResourceNotFoundException"),
                "case {kind:?} ({resource_arn}) on {operation}: expected 400 \
                 ResourceNotFoundException, got {status} {code} {message:?}"
            );
        }
    }
}

/// The format rule is checked before anything is read or written: a refused call changes
/// no stored tag, and the stored resources keep answering their own tagging calls.
#[tokio::test]
async fn a_malformed_resource_arn_changes_nothing_the_store_holds() {
    let router = emulator();

    let (_, namespace_id) = namespace(&router, "arn-format-ns").await;
    let namespace_arn = ok(&router, "GetNamespace", json!({ "Id": namespace_id }))
        .await["Namespace"]["Arn"]
        .as_str()
        .expect("a stored namespace carries its ARN")
        .to_string();
    let service_id = service(&router, &namespace_id, "arn-format-svc").await;
    let service_arn = ok(&router, "GetService", json!({ "Id": service_id }))
        .await["Service"]["Arn"]
        .as_str()
        .expect("a stored service carries its ARN")
        .to_string();

    for (kind, resource_arn) in [("namespace", namespace_arn), ("service", service_arn)] {
        ok(
            &router,
            "TagResource",
            json!({
                "ResourceARN": resource_arn,
                "Tags": [{ "Key": "stage", "Value": "prod" }],
            }),
        )
        .await;
        let stored = ok(
            &router,
            "ListTagsForResource",
            json!({ "ResourceARN": resource_arn }),
        )
        .await;
        assert_eq!(
            stored["Tags"],
            json!([{ "Key": "stage", "Value": "prod" }]),
            "case {kind:?}: the stored tag is what the read reports, got {stored}"
        );

        // The same ARN with its identifier mangled — the one difference the rule turns
        // on — is refused, and the stored tags are untouched by it.
        let mangled = format!("{resource_arn}x");
        for (operation, body) in tagging_calls(&mangled) {
            let (status, code, _) = refused(&router, operation, body).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidInput"),
                "case {kind:?} on {operation} with a mangled identifier: expected 400 \
                 InvalidInput, got {status} {code}"
            );
        }
        let after = ok(
            &router,
            "ListTagsForResource",
            json!({ "ResourceARN": resource_arn }),
        )
        .await;
        assert_eq!(
            after, stored,
            "case {kind:?}: a refused tagging call changes no stored tag, got {after}"
        );
    }
}
