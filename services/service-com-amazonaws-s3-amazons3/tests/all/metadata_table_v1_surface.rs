// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what the V1 S3 Metadata TABLE surface does to the bucket it
//! addresses: nothing.
//!
//! Every assertion drives a real HTTP request through the router, so what is pinned is the
//! wire behaviour (the answer, and the V2 configuration afterwards) rather than any handler's
//! internals.
//!
//! The V1 metadata-table operations — `CreateBucketMetadataTableConfiguration`,
//! `GetBucketMetadataTableConfiguration` and `DeleteBucketMetadataTableConfiguration` — are
//! SUPERSEDED: "make sure that you update your processes to use the new V2 API operations
//! (CreateBucketMetadataConfiguration, GetBucketMetadataConfiguration, and
//! DeleteBucketMetadataConfiguration) instead of the V1 API operations" (each V1 operation's
//! own documentation), and AWS refuses a call on that surface rather than serving it. What
//! this suite pins is the part of that contract that does not depend on WHICH refusal it is:
//!
//! 1. no V1 call is SERVED — each is answered a client error, so no caller can believe a V1
//!    configuration was created, reported or removed;
//! 2. the three answer ALIKE — one surface, one answer, whichever of the three a client calls
//!    and whichever bucket it addresses;
//! 3. a V1 call leaves the bucket's V2 metadata configuration exactly as it was — the V2
//!    surface (`?metadataConfiguration`) is a different subresource and the V1 address must
//!    not touch it, whether the bucket carries a configuration or none.
//!
//! The ERROR CODE of that refusal is deliberately not pinned here: the recorded material for
//! this surface asks for `V1APIsNotAllowed`, and AWS's sentence for that code is in neither
//! the Smithy model (which declares no error shape for it) nor anything this emulator holds —
//! so pinning a code and a sentence here would pin a wording nothing sourced. The rows above
//! hold for every refusal the surface may be given.
//!
//! The rows differ along the operation (and so along the method and the payload), and along
//! the bucket the call addresses: one this emulator holds, one it holds nothing under, and one
//! carrying a V2 configuration of its own.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, Answer};
use super::object_and_tagging_roundtrip::{bucket, text};

/// The subresource every V1 metadata-table operation is bound at
/// (`smithy_query(query="operation",operation="CreateBucketMetadataTableConfiguration")`
/// reports `http {method: "POST", uri: "/{Bucket}?metadataTable"}`, and the read and the
/// delete the same uri under `GET` and `DELETE`).
const V1_SUBRESOURCE: &str = "metadataTable";

/// The subresource the V2 surface is bound at
/// (`smithy_query(query="operation",operation="GetBucketMetadataConfiguration")` reports
/// `GET /{Bucket}?metadataConfiguration`) — a different address, which is why a V1 call must
/// leave it alone.
const V2_SUBRESOURCE: &str = "metadataConfiguration";

/// The V1 create's payload: its `MetadataTableConfiguration` is a REQUIRED `@httpPayload`,
/// and its destination's two members are required too, so the document has to be complete
/// for the call to reach the surface at all rather than being refused for its body.
fn v1_create_payload() -> String {
    "<MetadataTableConfiguration>\
     <S3TablesDestination>\
     <TableBucketArn>arn:aws:s3tables:us-east-1:000000000000:bucket/metadata-destination\
     </TableBucketArn>\
     <TableName>ledger_metadata</TableName>\
     </S3TablesDestination>\
     </MetadataTableConfiguration>"
        .to_string()
}

/// The V2 create's payload: a journal-table configuration, the shape
/// `CreateBucketMetadataConfiguration` takes.
fn v2_create_payload() -> String {
    "<MetadataConfiguration>\
     <JournalTableConfiguration>\
     <RecordExpiration><Expiration>ENABLED</Expiration><Days>7</Days></RecordExpiration>\
     </JournalTableConfiguration>\
     </MetadataConfiguration>"
        .to_string()
}

/// One call on the V1 surface: the method the operation is bound to and the body it carries.
struct Call {
    operation: &'static str,
    method: &'static str,
    body: String,
}

/// The three operations of the surface, each as the model binds it.
fn calls() -> Vec<Call> {
    vec![
        Call {
            operation: "CreateBucketMetadataTableConfiguration",
            method: "POST",
            body: v1_create_payload(),
        },
        Call {
            operation: "GetBucketMetadataTableConfiguration",
            method: "GET",
            body: String::new(),
        },
        Call {
            operation: "DeleteBucketMetadataTableConfiguration",
            method: "DELETE",
            body: String::new(),
        },
    ]
}

/// The V1 call `call` addressed to `name`.
async fn v1_call(router: &MultiServiceRouter, call: &Call, name: &str) -> Answer {
    send(
        router,
        call.method,
        &format!("/{name}?{V1_SUBRESOURCE}"),
        &[],
        &call.body,
    )
    .await
}

/// `GET /<bucket>?metadataConfiguration` — the V2 read, which is what says whether a V1 call
/// changed anything. Answered as the (status, error code or reported state) pair, since every
/// answer of this service also carries the correlation ids it mints per request.
async fn v2_state(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    let read = send(
        router,
        "GET",
        &format!("/{name}?{V2_SUBRESOURCE}"),
        &[],
        "",
    )
    .await;
    let reported = match read.status {
        200 => read
            .element("MetadataConfigurationResult")
            .or_else(|| read.element("JournalTableConfigurationResult"))
            .unwrap_or_else(|| text(&read)),
        _ => read.error_code(),
    };
    (read.status, reported)
}

/// RULE: no call on the V1 surface is served, the three answer alike, and none of them
/// disturbs the bucket's V2 metadata configuration.
#[tokio::test]
async fn no_v1_metadata_table_call_is_served_and_none_touches_the_v2_configuration() {
    let router = router();
    let held = "metadata-table-v1-held";
    bucket(&router, held).await;
    // A bucket carrying a V2 configuration of its own, so the invariant is asserted both
    // where there is a configuration to disturb and where there is none.
    let configured = "metadata-table-v1-configured";
    bucket(&router, configured).await;
    let created = send(
        &router,
        "POST",
        &format!("/{configured}?{V2_SUBRESOURCE}"),
        &[],
        &v2_create_payload(),
    )
    .await;
    assert!(
        (200..300).contains(&created.status),
        "the V2 configuration of {configured:?} must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );

    let addresses: [(&str, &str); 3] = [
        ("a bucket this emulator holds", held),
        ("a bucket carrying a V2 configuration", configured),
        ("a name no bucket is held under", "metadata-table-v1-absent"),
    ];
    // The answer the FIRST V1 call carried, which every later one has to carry too.
    let mut surface_answer: Option<(u16, String)> = None;

    for call in calls() {
        for (addressed, name) in addresses {
            let case = format!("{} against {addressed}", call.operation);
            let before = v2_state(&router, name).await;
            let answered = v1_call(&router, &call, name).await;
            let body = text(&answered);

            // 1. Not served: a client error, so nothing can be read as a V1 configuration
            //    created, reported or removed.
            assert!(
                (400..600).contains(&answered.status),
                "case {case:?}: a V1 call must not be served, got {} with body {body:?}",
                answered.status
            );
            // 2. One surface, one answer.
            let answer = (answered.status, answered.error_code());
            assert_eq!(
                answer,
                surface_answer.get_or_insert_with(|| answer.clone()).clone(),
                "case {case:?}: expected the answer every other V1 call carries, got {answer:?} \
                 in body {body:?}"
            );
            // 3. The V2 configuration is exactly what it was.
            let after = v2_state(&router, name).await;
            assert_eq!(
                after, before,
                "case {case:?}: a V1 call must leave the V2 configuration alone, expected \
                 {before:?}, got {after:?}"
            );
        }
    }
}
