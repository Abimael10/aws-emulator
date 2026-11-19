// SPDX-License-Identifier: CC0-1.0

//! The operations a DIRECTORY bucket does not serve, and the refusal a request addressing one
//! of them earns.
//!
//! S3 Express One Zone serves a NARROWER API than the general purpose surface: the model says
//! so operation by operation, in a note of that operation's own documentation — "this
//! operation is not supported for directory buckets" ([`UNSUPPORTED_NOTE`]) — which is the one
//! statement this module reads. A request that addresses a directory bucket and names such an
//! operation asks a bucket for something its KIND does not have, so S3 refuses it rather than
//! configuring the bucket: code written against S3 Express that appeared to work here would
//! fail in AWS, and a bucket left carrying a tag set, a versioning state, a notification
//! configuration or a replication configuration no directory bucket can have would then be
//! reported by every later read of it.
//!
//! The rule is applied to EVERY operation the model marks that way rather than per handler, so
//! the API surface a directory bucket serves is decided in one place and cannot drift from the
//! model: [`UNSUPPORTED_BY_A_DIRECTORY_BUCKET`] is the catalog, and the suite below pins it
//! against the model's own notes.
//!
//! The refusal is served as an OPERATION
//! ([`super::EmulatedService::override_operation`] routes to it), for the reason the sibling
//! refusals of this service are ([`super::invalid_bucket_name`]): that is the one seam a
//! service can answer a request from before the operation it addressed runs, and it is where
//! the addressed bucket and the addressed operation are both known.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::protocol::ServerProtocol;
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use super::wire::UnwrappedXmlRestXml;

/// The sentence the model states in the documentation of an operation S3 does not serve for a
/// directory bucket, as the model writes it (one `<p>` of a `<note>`).
///
/// This is the whole of the rule: an operation carrying this note is one a directory bucket
/// does not serve, and an operation without it is one it does. It is matched as a PREFIX of
/// the note's sentence, because a handful of operations continue it with the other bucket kind
/// they are not supported for either ("… or Amazon S3 on Outposts buckets"), which says the
/// same thing about a directory bucket. The suite at the bottom of this file reads it out of
/// the model and compares the result with [`UNSUPPORTED_BY_A_DIRECTORY_BUCKET`], so the catalog
/// cannot drift from the model it is taken from.
pub const UNSUPPORTED_NOTE: &str = "This operation is not supported for directory buckets";

/// Every operation of this model a DIRECTORY bucket does not serve, each with the routing key
/// this service holds its refusal under and the FEATURE its refusal names.
///
/// The membership test is [`UNSUPPORTED_NOTE`] in the operation's own documentation, read out
/// of the model by the suite below — so the operation list is the model's statement
/// transcribed, not a judgement of this crate's. The key beside each carries the `@` sigil the
/// sibling door keys carry ([`super::invalid_bucket_name::REFUSE_INVALID_BUCKET_NAME`]), which
/// is a character no modelled operation's name holds, so nothing but
/// [`super::EmulatedService::override_operation`] can ever look one up; there is one key per
/// operation because the refusal names what the bucket does not support and an entry of the
/// routing table has to be a `'static` key.
///
/// The FEATURE is what the refusal's sentence names ([`unsupported_message`]): the PLANE the
/// operation belongs to where the bucket lacks one (every call of the object tag-set plane
/// names `Object Tags`, every call of the versioning plane names `Versioning`), and the
/// operation itself where the whole call is what S3 Express does not serve rather than a
/// feature of the bucket (a listing form, the Object Lambda write-back). Operations of one
/// plane share a feature, so a client is told the same thing by the read and the write of it.
///
/// Two of them address no bucket at all (`ListBuckets` lists the account's general purpose
/// buckets, and the Object Lambda `WriteGetObjectResponse` writes back to a caller), so no
/// request can reach their refusal — the routing seam asks this catalog only once it has
/// resolved the addressed bucket and found a directory bucket. They are still here because the
/// operation list is the model's and nothing else.
pub const UNSUPPORTED_BY_A_DIRECTORY_BUCKET: [(&str, &str, &str); 59] = [
    ("DeleteBucketAnalyticsConfiguration", "@directory-bucket-unsupported-DeleteBucketAnalyticsConfiguration", ANALYTICS),
    ("DeleteBucketCors", "@directory-bucket-unsupported-DeleteBucketCors", CORS),
    ("DeleteBucketIntelligentTieringConfiguration", "@directory-bucket-unsupported-DeleteBucketIntelligentTieringConfiguration", INTELLIGENT_TIERING),
    ("DeleteBucketOwnershipControls", "@directory-bucket-unsupported-DeleteBucketOwnershipControls", OBJECT_OWNERSHIP),
    ("DeleteBucketReplication", "@directory-bucket-unsupported-DeleteBucketReplication", REPLICATION),
    ("DeleteBucketTagging", "@directory-bucket-unsupported-DeleteBucketTagging", BUCKET_TAGGING),
    ("DeleteBucketWebsite", "@directory-bucket-unsupported-DeleteBucketWebsite", WEBSITE),
    ("DeleteObjectTagging", "@directory-bucket-unsupported-DeleteObjectTagging", OBJECT_TAGS),
    ("DeletePublicAccessBlock", "@directory-bucket-unsupported-DeletePublicAccessBlock", PUBLIC_ACCESS_BLOCK),
    ("GetBucketAccelerateConfiguration", "@directory-bucket-unsupported-GetBucketAccelerateConfiguration", ACCELERATION),
    ("GetBucketAcl", "@directory-bucket-unsupported-GetBucketAcl", ACL),
    ("GetBucketAnalyticsConfiguration", "@directory-bucket-unsupported-GetBucketAnalyticsConfiguration", ANALYTICS),
    ("GetBucketCors", "@directory-bucket-unsupported-GetBucketCors", CORS),
    ("GetBucketIntelligentTieringConfiguration", "@directory-bucket-unsupported-GetBucketIntelligentTieringConfiguration", INTELLIGENT_TIERING),
    ("GetBucketLocation", "@directory-bucket-unsupported-GetBucketLocation", "GetBucketLocation"),
    ("GetBucketLogging", "@directory-bucket-unsupported-GetBucketLogging", LOGGING),
    ("GetBucketNotificationConfiguration", "@directory-bucket-unsupported-GetBucketNotificationConfiguration", NOTIFICATIONS),
    ("GetBucketOwnershipControls", "@directory-bucket-unsupported-GetBucketOwnershipControls", OBJECT_OWNERSHIP),
    ("GetBucketPolicyStatus", "@directory-bucket-unsupported-GetBucketPolicyStatus", "GetBucketPolicyStatus"),
    ("GetBucketReplication", "@directory-bucket-unsupported-GetBucketReplication", REPLICATION),
    ("GetBucketRequestPayment", "@directory-bucket-unsupported-GetBucketRequestPayment", REQUEST_PAYMENT),
    ("GetBucketTagging", "@directory-bucket-unsupported-GetBucketTagging", BUCKET_TAGGING),
    ("GetBucketVersioning", "@directory-bucket-unsupported-GetBucketVersioning", VERSIONING),
    ("GetBucketWebsite", "@directory-bucket-unsupported-GetBucketWebsite", WEBSITE),
    ("GetObjectAcl", "@directory-bucket-unsupported-GetObjectAcl", ACL),
    ("GetObjectLegalHold", "@directory-bucket-unsupported-GetObjectLegalHold", OBJECT_LOCK),
    ("GetObjectLockConfiguration", "@directory-bucket-unsupported-GetObjectLockConfiguration", OBJECT_LOCK),
    ("GetObjectRetention", "@directory-bucket-unsupported-GetObjectRetention", OBJECT_LOCK),
    ("GetObjectTagging", "@directory-bucket-unsupported-GetObjectTagging", OBJECT_TAGS),
    ("GetObjectTorrent", "@directory-bucket-unsupported-GetObjectTorrent", "GetObjectTorrent"),
    ("GetPublicAccessBlock", "@directory-bucket-unsupported-GetPublicAccessBlock", PUBLIC_ACCESS_BLOCK),
    ("ListBucketAnalyticsConfigurations", "@directory-bucket-unsupported-ListBucketAnalyticsConfigurations", ANALYTICS),
    ("ListBucketIntelligentTieringConfigurations", "@directory-bucket-unsupported-ListBucketIntelligentTieringConfigurations", INTELLIGENT_TIERING),
    ("ListBuckets", "@directory-bucket-unsupported-ListBuckets", "ListBuckets"),
    ("ListObjectVersions", "@directory-bucket-unsupported-ListObjectVersions", "ListObjectVersions"),
    ("ListObjects", "@directory-bucket-unsupported-ListObjects", "ListObjects"),
    ("PutBucketAccelerateConfiguration", "@directory-bucket-unsupported-PutBucketAccelerateConfiguration", ACCELERATION),
    ("PutBucketAcl", "@directory-bucket-unsupported-PutBucketAcl", ACL),
    ("PutBucketAnalyticsConfiguration", "@directory-bucket-unsupported-PutBucketAnalyticsConfiguration", ANALYTICS),
    ("PutBucketCors", "@directory-bucket-unsupported-PutBucketCors", CORS),
    ("PutBucketIntelligentTieringConfiguration", "@directory-bucket-unsupported-PutBucketIntelligentTieringConfiguration", INTELLIGENT_TIERING),
    ("PutBucketLogging", "@directory-bucket-unsupported-PutBucketLogging", LOGGING),
    ("PutBucketNotificationConfiguration", "@directory-bucket-unsupported-PutBucketNotificationConfiguration", NOTIFICATIONS),
    ("PutBucketOwnershipControls", "@directory-bucket-unsupported-PutBucketOwnershipControls", OBJECT_OWNERSHIP),
    ("PutBucketReplication", "@directory-bucket-unsupported-PutBucketReplication", REPLICATION),
    ("PutBucketRequestPayment", "@directory-bucket-unsupported-PutBucketRequestPayment", REQUEST_PAYMENT),
    ("PutBucketTagging", "@directory-bucket-unsupported-PutBucketTagging", BUCKET_TAGGING),
    ("PutBucketVersioning", "@directory-bucket-unsupported-PutBucketVersioning", VERSIONING),
    ("PutBucketWebsite", "@directory-bucket-unsupported-PutBucketWebsite", WEBSITE),
    ("PutObjectAcl", "@directory-bucket-unsupported-PutObjectAcl", ACL),
    ("PutObjectLegalHold", "@directory-bucket-unsupported-PutObjectLegalHold", OBJECT_LOCK),
    ("PutObjectLockConfiguration", "@directory-bucket-unsupported-PutObjectLockConfiguration", OBJECT_LOCK),
    ("PutObjectRetention", "@directory-bucket-unsupported-PutObjectRetention", OBJECT_LOCK),
    ("PutObjectTagging", "@directory-bucket-unsupported-PutObjectTagging", OBJECT_TAGS),
    ("PutPublicAccessBlock", "@directory-bucket-unsupported-PutPublicAccessBlock", PUBLIC_ACCESS_BLOCK),
    ("RestoreObject", "@directory-bucket-unsupported-RestoreObject", "RestoreObject"),
    ("SelectObjectContent", "@directory-bucket-unsupported-SelectObjectContent", "SelectObjectContent"),
    ("UpdateObjectEncryption", "@directory-bucket-unsupported-UpdateObjectEncryption", "UpdateObjectEncryption"),
    ("WriteGetObjectResponse", "@directory-bucket-unsupported-WriteGetObjectResponse", "WriteGetObjectResponse"),
];

/// The FEATURES the catalog above names, one per plane a directory bucket does not have.
///
/// Each is the name the plane is PUBLISHED under — the tag set of an object is "object tags"
/// (Amazon S3 User Guide, *Categorizing your storage using tags*), the bucket-level one is
/// "bucket tagging", and so on — which is what [`unsupported_message`] states; `Object Tags` is
/// the one recorded verbatim, in the refusal a tag write into a directory bucket earns.
pub const OBJECT_TAGS: &str = "Object Tags";
/// See [`OBJECT_TAGS`].
pub const BUCKET_TAGGING: &str = "Bucket Tagging";
/// See [`OBJECT_TAGS`].
pub const VERSIONING: &str = "Versioning";
/// See [`OBJECT_TAGS`].
pub const NOTIFICATIONS: &str = "Notifications";
/// See [`OBJECT_TAGS`].
pub const REPLICATION: &str = "Replication";
/// See [`OBJECT_TAGS`].
pub const ACL: &str = "ACL";
/// See [`OBJECT_TAGS`].
pub const WEBSITE: &str = "Website";
/// See [`OBJECT_TAGS`].
pub const CORS: &str = "CORS";
/// See [`OBJECT_TAGS`].
pub const LOGGING: &str = "Logging";
/// See [`OBJECT_TAGS`].
pub const REQUEST_PAYMENT: &str = "Request Payment";
/// See [`OBJECT_TAGS`].
pub const ACCELERATION: &str = "Transfer Acceleration";
/// See [`OBJECT_TAGS`].
pub const ANALYTICS: &str = "Analytics";
/// See [`OBJECT_TAGS`].
pub const INTELLIGENT_TIERING: &str = "Intelligent-Tiering";
/// See [`OBJECT_TAGS`].
pub const PUBLIC_ACCESS_BLOCK: &str = "Public Access Block";
/// See [`OBJECT_TAGS`].
pub const OBJECT_OWNERSHIP: &str = "Object Ownership";
/// See [`OBJECT_TAGS`].
pub const OBJECT_LOCK: &str = "Object Lock";

/// The routing key the refusal of `operation` against a directory bucket is served under, or
/// `None` for an operation a directory bucket DOES serve — which is every operation this
/// catalog does not name, so nothing of the Zonal data plane, the session calls or the
/// configurations S3 Express does support is touched.
pub fn refusal_key(operation: &str) -> Option<&'static str> {
    UNSUPPORTED_BY_A_DIRECTORY_BUCKET
        .into_iter()
        .find(|(named, _, _)| *named == operation)
        .map(|(_, key, _)| key)
}

/// The `<Message>` S3 refuses a call on a directory bucket with, naming the FEATURE that
/// bucket does not have.
///
/// FIXED text around the feature, with nothing of the request's values in it — recorded
/// behaviour: a tag write into a directory bucket is refused "This bucket does not support
/// Object Tags.", and the same sentence names whichever plane the call belongs to
/// ([`UNSUPPORTED_BY_A_DIRECTORY_BUCKET`] carries the feature per operation). It is the same
/// wording family S3 answers a PARAMETER a directory bucket does not serve with ("this bucket
/// does not support start-after query parameter for ListObjectsV2 API.",
/// `crate::ops::list_objects_v2`).
pub fn unsupported_message(feature: &str) -> String {
    format!("This bucket does not support {feature}.")
}

/// The operation that answers a request asking its directory bucket for a feature that kind of
/// bucket does not have.
///
/// One per catalogued operation ([`UNSUPPORTED_BY_A_DIRECTORY_BUCKET`]), because the sentence
/// names the feature and the routing seam can only hand back a borrow of an operation this
/// service already holds.
pub struct RefuseUnsupportedOperation {
    /// The feature the refusal names — the plane the refused operation belongs to.
    feature: &'static str,
    /// This service's own protocol, built ONCE: the envelope a refusal of this service goes
    /// out in — the bare `<Error>` root and the `x-amz-request-id` / `x-amz-id-2` pair S3 names
    /// every answer by.
    protocol: UnwrappedXmlRestXml,
}

impl RefuseUnsupportedOperation {
    /// Build the refusal naming `feature`. INFALLIBLE, and it holds no state: what it answers
    /// is one code and one sentence.
    pub fn new(feature: &'static str) -> Self {
        Self {
            feature,
            protocol: UnwrappedXmlRestXml::new(),
        }
    }

    /// The envelope: the code S3 answers a request whose address implies functionality it does
    /// not implement for the resource addressed ([`crate::state::NOT_IMPLEMENTED_CODE`], HTTP
    /// 501) and the sentence naming the feature.
    fn refusal(&self) -> Option<RtResponse> {
        let refusal = crate::scaffolded::UnmodeledMessage {
            message: unsupported_message(self.feature),
        };
        self.protocol
            .serialize_error(
                &refusal,
                &crate::scaffolded::UNMODELED_SCHEMA,
                crate::state::NOT_IMPLEMENTED_CODE,
                crate::state::NOT_IMPLEMENTED_STATUS,
            )
            .ok()
    }
}

impl ErasedOperation for RefuseUnsupportedOperation {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let _ = ctx;
            match self.refusal() {
                Some(response) => response,
                // The serializer cannot fail on a fixed code and one sentence; if it ever
                // did, the request keeps the framework's own answer rather than panicking in
                // the request path.
                None => svc.definition().routing_error_response(req),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model this service is generated from, read as the shape map it is.
    ///
    /// Read from the file at test time rather than embedded: the model is megabytes, and what
    /// this suite needs out of it is one trait per operation.
    fn model() -> ::serde_json::Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/smithy-model/com-amazonaws-s3-amazons3.json"
        );
        let text = ::std::fs::read_to_string(path).expect("the service's own model is readable");
        ::serde_json::from_str(&text).expect("the model is JSON")
    }

    /// Every operation shape of the model whose documentation carries [`UNSUPPORTED_NOTE`],
    /// by its short name.
    fn unsupported_in_the_model() -> Vec<String> {
        let model = model();
        let shapes = model["shapes"]
            .as_object()
            .expect("the model declares shapes")
            .clone();
        let mut named: Vec<String> = shapes
            .iter()
            .filter(|(_, shape)| shape["type"] == "operation")
            .filter(|(_, shape)| {
                shape["traits"]["smithy.api#documentation"]
                    .as_str()
                    .is_some_and(|documentation| documentation.contains(UNSUPPORTED_NOTE))
            })
            .filter_map(|(id, _)| id.rsplit('#').next().map(str::to_string))
            .collect();
        named.sort();
        named
    }

    /// RULE: the catalog is the model's own list of the operations a directory bucket does not
    /// serve — every operation whose documentation carries the note is in it, and nothing else
    /// is, so the surface this emulator refuses cannot drift from the model.
    #[test]
    fn the_catalog_is_the_models_own_list() {
        let expected = unsupported_in_the_model();
        let mut catalogued: Vec<String> = UNSUPPORTED_BY_A_DIRECTORY_BUCKET
            .into_iter()
            .map(|(operation, _, _)| operation.to_string())
            .collect();
        catalogued.sort();
        assert_eq!(
            catalogued, expected,
            "case \"the catalog against the model\": the model names {} operations the note \
             marks and the catalog {}; missing {:?}, over-stated {:?}",
            expected.len(),
            catalogued.len(),
            expected
                .iter()
                .filter(|named| !catalogued.contains(named))
                .collect::<Vec<_>>(),
            catalogued
                .iter()
                .filter(|named| !expected.contains(named))
                .collect::<Vec<_>>(),
        );
    }

    /// RULE: every catalogued operation is served under a key of its own that no modelled
    /// routing key can resolve, and the refusal names the feature the bucket does not have.
    #[test]
    fn every_entry_has_its_own_unresolvable_key() {
        let mut keys: Vec<&str> = Vec::new();
        for (operation, key, feature) in UNSUPPORTED_BY_A_DIRECTORY_BUCKET {
            assert!(
                key.starts_with('@'),
                "case {operation:?}: a door key must carry the sigil no modelled operation \
                 name holds, got {key:?}"
            );
            assert!(
                !keys.contains(&key),
                "case {operation:?}: each operation needs a key of its own, got {key:?} twice"
            );
            keys.push(key);
            assert_eq!(
                refusal_key(operation),
                Some(key),
                "case {operation:?}: the catalogued operation must resolve to its own key, got \
                 {:?}",
                refusal_key(operation)
            );
            let message = unsupported_message(feature);
            assert_eq!(
                message,
                format!("This bucket does not support {feature}."),
                "case {operation:?}: the refusal must name the feature the bucket does not \
                 have, got {message:?}"
            );
        }
    }

    /// RULE: the tag-set planes carry the feature name S3 states VERBATIM for them, and every
    /// call of one plane names the same feature — so the read and the write of it tell a client
    /// the same thing.
    #[test]
    fn every_call_of_one_plane_names_one_feature() {
        let feature_of = |operation: &str| {
            UNSUPPORTED_BY_A_DIRECTORY_BUCKET
                .into_iter()
                .find(|(named, _, _)| *named == operation)
                .map(|(_, _, feature)| feature)
        };
        // (case, the operations of one plane, the feature every one of them names)
        let planes: [(&str, &[&str], &str); 5] = [
            (
                "the object tag set",
                &["PutObjectTagging", "GetObjectTagging", "DeleteObjectTagging"],
                OBJECT_TAGS,
            ),
            (
                "the bucket tag set",
                &["PutBucketTagging", "GetBucketTagging", "DeleteBucketTagging"],
                BUCKET_TAGGING,
            ),
            (
                "versioning",
                &["PutBucketVersioning", "GetBucketVersioning"],
                VERSIONING,
            ),
            (
                "replication",
                &[
                    "PutBucketReplication",
                    "GetBucketReplication",
                    "DeleteBucketReplication",
                ],
                REPLICATION,
            ),
            (
                "Object Lock",
                &[
                    "PutObjectLockConfiguration",
                    "GetObjectLockConfiguration",
                    "PutObjectRetention",
                    "GetObjectRetention",
                    "PutObjectLegalHold",
                    "GetObjectLegalHold",
                ],
                OBJECT_LOCK,
            ),
        ];
        for (case, operations, feature) in planes {
            for operation in operations {
                assert_eq!(
                    feature_of(operation),
                    Some(feature),
                    "case {case:?} ({operation}): every call of one plane names that plane's \
                     feature, got {:?}",
                    feature_of(operation)
                );
            }
        }
        assert_eq!(
            unsupported_message(OBJECT_TAGS),
            "This bucket does not support Object Tags.",
            "the object tag-set refusal is recorded verbatim"
        );
    }

    /// RULE, the other half: an operation a directory bucket DOES serve resolves to no refusal
    /// — the Zonal data plane, the session calls and the configurations S3 Express supports.
    ///
    /// The rows are what the rule generalises over: a data-plane read, a write, the listing a
    /// directory bucket serves, the multipart calls, the session call and the two bucket
    /// configurations the model does NOT mark (a directory bucket has default encryption and a
    /// lifecycle configuration).
    #[test]
    fn a_supported_operation_resolves_to_no_refusal() {
        for operation in [
            "GetObject",
            "HeadObject",
            "PutObject",
            "DeleteObject",
            "ListObjectsV2",
            "CreateSession",
            "CreateMultipartUpload",
            "UploadPart",
            "CompleteMultipartUpload",
            "AbortMultipartUpload",
            "ListParts",
            "ListMultipartUploads",
            "CreateBucket",
            "DeleteBucket",
            "HeadBucket",
            "ListDirectoryBuckets",
            "GetBucketEncryption",
            "PutBucketEncryption",
            "GetBucketLifecycleConfiguration",
            "PutBucketLifecycleConfiguration",
            "GetBucketPolicy",
            "PutBucketPolicy",
            "RenameObject",
        ] {
            assert_eq!(
                refusal_key(operation),
                None,
                "case {operation:?}: a directory bucket serves this operation, so it must \
                 resolve to no refusal, got {:?}",
                refusal_key(operation)
            );
        }
    }
}
