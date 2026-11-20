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
/// The ENCRYPTION form a directory bucket does not serve, named in the refusal a write asking
/// one to store an object under a customer-provided key earns
/// (`crate::service::object_write::directory_bucket_customer_key_refusal`).
///
/// "This functionality is not supported for directory buckets"
/// (`PutObjectRequest$SSECustomerAlgorithm`) — such a bucket serves server-side encryption with
/// Amazon S3 managed keys and with KMS keys, and no key the caller supplies. Recorded verbatim,
/// like [`OBJECT_TAGS`]: it is a feature of the BUCKET, so it is stated in that sentence rather
/// than as a member this emulator has not written.
pub const SSE_CUSTOMER_KEY: &str = "server side encryption with a customer provided key";

/// The RESTRICTION a copy into a directory bucket of a source object that carries a tag set
/// is refused by, named in the same sentence family as the features above
/// ([`unsupported_message`]).
///
/// It is not the object tag-set PLANE: what such a copy asked for is the carrying over of the
/// SOURCE's tags ("for directory buckets in a `CopyObject` operation, only the empty tag-set
/// is supported. Any requests that attempt to write non-empty tags into directory buckets will
/// receive a `501 Not Implemented` status code", `CopyObjectRequest$TaggingDirective`), and the
/// sentence is the only thing that tells a caller which of the two restrictions fired — the
/// remedy for this one is `TaggingDirective=REPLACE` over an empty tag set, not giving up the
/// tagging API. Recorded verbatim, like [`OBJECT_TAGS`] beside it.
pub const COPYING_SOURCE_OBJECT_TAGS: &str = "copying source objects with Object Tags";

/// The one bucket-configuration document a DIRECTORY bucket's endpoint parses.
///
/// A directory bucket is configured by the create that places it in its zone — "the location
/// type is Availability Zone or Local Zone" (`LocationInfo`) — so `CreateBucketConfiguration`
/// is the only configuration document that endpoint knows, which is what
/// [`not_a_create_bucket_configuration_message`] states.
pub const CREATE_BUCKET_CONFIGURATION_ELEMENT: &str = "CreateBucketConfiguration";

/// The catalogued operations that submit a bucket-configuration DOCUMENT, each with the root
/// element the model binds as its REQUIRED `@httpPayload` member — that member's `xmlName`
/// (`smithy_query(query="operation",operation="PutBucketVersioning,PutBucketReplication,…")`).
///
/// These are refused for the DOCUMENT they carry rather than for the feature they ask for
/// ([`not_a_create_bucket_configuration_message`]): a directory bucket's endpoint parses one
/// bucket-configuration document ([`CREATE_BUCKET_CONFIGURATION_ELEMENT`]), so a PUT that
/// submits any other is a request-shape fault, which is a different thing — and a different
/// class of answer — from an operation nothing can carry out on that bucket at all.
///
/// Every entry is in [`UNSUPPORTED_BY_A_DIRECTORY_BUCKET`] too — the model is what says a
/// directory bucket does not serve it — and the two together decide which refusal it earns.
/// `PutBucketAcl` is deliberately NOT here even though it is catalogued: its
/// `AccessControlPolicy` payload is optional, so a canned-ACL request submits no document for
/// such a refusal to name.
pub const CONFIGURATION_DOCUMENT_PUTS: [(&str, &str); 13] = [
    ("PutBucketAccelerateConfiguration", "AccelerateConfiguration"),
    ("PutBucketAnalyticsConfiguration", "AnalyticsConfiguration"),
    ("PutBucketCors", "CORSConfiguration"),
    (
        "PutBucketIntelligentTieringConfiguration",
        "IntelligentTieringConfiguration",
    ),
    ("PutBucketLogging", "BucketLoggingStatus"),
    (
        "PutBucketNotificationConfiguration",
        "NotificationConfiguration",
    ),
    ("PutBucketOwnershipControls", "OwnershipControls"),
    ("PutBucketReplication", "ReplicationConfiguration"),
    ("PutBucketRequestPayment", "RequestPaymentConfiguration"),
    ("PutBucketTagging", "Tagging"),
    ("PutBucketVersioning", "VersioningConfiguration"),
    ("PutBucketWebsite", "WebsiteConfiguration"),
    ("PutPublicAccessBlock", "PublicAccessBlockConfiguration"),
];

/// The root element the model binds for `operation`'s configuration document, or `None` for an
/// operation that submits none ([`CONFIGURATION_DOCUMENT_PUTS`]).
///
/// What the refusal NAMES is the element the request carried
/// ([`submitted_document_element`]); this is the model's own binding for the call, which is
/// what the suite below pins the table against.
pub fn configuration_document(operation: &str) -> Option<&'static str> {
    CONFIGURATION_DOCUMENT_PUTS
        .into_iter()
        .find(|(named, _)| *named == operation)
        .map(|(_, element)| element)
}

/// The `<Message>` S3 refuses a bucket-configuration document a directory bucket's endpoint
/// does not parse with, naming the document that was submitted.
///
/// FIXED text around the submitted element: the variable part is the root element of the
/// document the request carried, and the fixed part names the one document that endpoint does
/// parse ([`CREATE_BUCKET_CONFIGURATION_ELEMENT`]). This is a REQUEST-SHAPE refusal
/// ([`crate::state::INVALID_ARGUMENT_CODE`], HTTP 400) rather than [`unsupported_message`]'s
/// unimplemented-feature one, because what it reports is the document the caller sent, which a
/// caller can act on.
pub fn not_a_create_bucket_configuration_message(element: &str) -> String {
    format!("{element} is not valid, expected {CREATE_BUCKET_CONFIGURATION_ELEMENT}")
}

/// The root element of the document `req` submitted, when its body is markup at all.
///
/// Read off the REQUEST, because that is what the refusal above names: a document whose root is
/// not the one the model binds for the call is still the document the caller sent. `None` for a
/// body no XML reader can walk (an empty one included) — such a request submits no document for
/// that refusal to name, and is answered for the feature instead
/// ([`RefuseUnsupportedOperation::refusal`]).
fn submitted_document_element(req: &RtRequest) -> Option<String> {
    let mut reader = ::quick_xml::Reader::from_reader(req.body().bytes()?);
    loop {
        let element = match reader.read_event() {
            Ok(::quick_xml::events::Event::Start(start)) => {
                start.local_name().into_inner().to_vec()
            }
            Ok(::quick_xml::events::Event::Empty(empty)) => {
                empty.local_name().into_inner().to_vec()
            }
            Ok(::quick_xml::events::Event::Eof) | Err(_) => return None,
            Ok(_) => continue,
        };
        return ::std::string::String::from_utf8(element).ok();
    }
}

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

/// The operation that answers a request asking its directory bucket for something that kind of
/// bucket does not have.
///
/// One per catalogued operation ([`UNSUPPORTED_BY_A_DIRECTORY_BUCKET`]), because the answer
/// names what the request asked for and the routing seam can only hand back a borrow of an
/// operation this service already holds.
pub struct RefuseUnsupportedOperation {
    /// The operation this refusal is held for — what decides WHICH of the two refusals it is
    /// ([`CONFIGURATION_DOCUMENT_PUTS`]).
    operation: &'static str,
    /// The feature the unimplemented-feature refusal names — the plane the refused operation
    /// belongs to.
    feature: &'static str,
    /// This service's own protocol, built ONCE: the envelope a refusal of this service goes
    /// out in — the bare `<Error>` root and the `x-amz-request-id` / `x-amz-id-2` pair S3 names
    /// every answer by.
    protocol: UnwrappedXmlRestXml,
}

impl RefuseUnsupportedOperation {
    /// Build the refusal `operation` earns, naming `feature` where it is the feature that is
    /// refused. INFALLIBLE, and it holds no state: what it answers is one code and one
    /// sentence, read off the request.
    pub fn new(operation: &'static str, feature: &'static str) -> Self {
        Self {
            operation,
            feature,
            protocol: UnwrappedXmlRestXml::new(),
        }
    }

    /// The envelope, which is one of two documents:
    ///
    /// * a PUT that SUBMITS a bucket-configuration document ([`CONFIGURATION_DOCUMENT_PUTS`]
    ///   names the calls, [`submitted_document_element`] reads what the request carried) is
    ///   refused for that DOCUMENT — the request-shape refusal
    ///   ([`crate::state::INVALID_ARGUMENT_CODE`], HTTP 400) naming the element the request
    ///   carried beside the one document a directory bucket's endpoint parses
    ///   ([`not_a_create_bucket_configuration_message`]);
    /// * every other catalogued call is refused for the FEATURE the bucket does not have: the
    ///   code S3 answers a request whose address implies functionality it does not implement
    ///   for the resource addressed ([`crate::state::NOT_IMPLEMENTED_CODE`], HTTP 501) and the
    ///   sentence naming that feature ([`unsupported_message`]). A request that carries no
    ///   document at all submits none for the first refusal to name, whichever call it
    ///   addressed, so it is answered here too.
    fn refusal(&self, req: &RtRequest) -> Option<RtResponse> {
        let submitted = configuration_document(self.operation)
            .and_then(|_| submitted_document_element(req));
        let (code, status, message) = match submitted {
            Some(element) => (
                crate::state::INVALID_ARGUMENT_CODE,
                crate::state::INVALID_ARGUMENT_STATUS,
                not_a_create_bucket_configuration_message(&element),
            ),
            None => (
                crate::state::NOT_IMPLEMENTED_CODE,
                crate::state::NOT_IMPLEMENTED_STATUS,
                unsupported_message(self.feature),
            ),
        };
        let refusal = crate::scaffolded::UnmodeledMessage { message };
        self.protocol
            .serialize_error(&refusal, &crate::scaffolded::UNMODELED_SCHEMA, code, status)
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
            match self.refusal(req) {
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

    /// The ROOT ELEMENT of `operation`'s REQUIRED XML document payload, as the model binds it
    /// — the `xmlName` of the member marked `@httpPayload` and `@required`, else that member's
    /// own name — or `None` for an operation whose payload is optional or which has none.
    fn required_document_payload_in_the_model(operation: &str) -> Option<String> {
        let model = model();
        let target = model["shapes"][format!("com.amazonaws.s3#{operation}")]["input"]["target"]
            .as_str()?
            .to_string();
        let members = model["shapes"][&target]["members"].as_object()?.clone();
        members.iter().find_map(|(name, member)| {
            let traits = &member["traits"];
            let bound = traits.get("smithy.api#httpPayload").is_some();
            let required = traits.get("smithy.api#required").is_some();
            (bound && required).then(|| {
                traits["smithy.api#xmlName"]
                    .as_str()
                    .unwrap_or(name)
                    .to_string()
            })
        })
    }

    /// RULE: every PUT refused for the DOCUMENT it submits is one the model marks unsupported
    /// for a directory bucket AND binds a required XML document payload for, and the element
    /// the refusal names is the one the model binds — so neither the list nor the wording can
    /// drift from the model.
    #[test]
    fn every_configuration_put_names_the_document_the_model_binds() {
        for (operation, element) in CONFIGURATION_DOCUMENT_PUTS {
            assert!(
                refusal_key(operation).is_some(),
                "case {operation:?}: a document refusal only ever answers an operation a \
                 directory bucket does not serve, and this one is not catalogued"
            );
            let modelled = required_document_payload_in_the_model(operation);
            assert_eq!(
                modelled.as_deref(),
                Some(element),
                "case {operation:?}: the refusal must name the document the model binds, got \
                 {modelled:?}"
            );
            assert_eq!(
                configuration_document(operation),
                Some(element),
                "case {operation:?}: the catalogued operation must resolve to its own document, \
                 got {:?}",
                configuration_document(operation)
            );
            let message = not_a_create_bucket_configuration_message(element);
            assert_eq!(
                message,
                format!("{element} is not valid, expected CreateBucketConfiguration"),
                "case {operation:?}: the refusal must name the submitted document beside the \
                 one a directory bucket's endpoint parses, got {message:?}"
            );
        }
    }

    /// RULE, the other half: a catalogued call that submits NO required document — a read, a
    /// delete, an object call, and the one PUT whose document is optional — resolves to no
    /// document refusal, so it keeps the unimplemented-feature sentence.
    #[test]
    fn a_call_without_a_required_document_resolves_to_no_document_refusal() {
        for operation in [
            "GetBucketVersioning",
            "GetBucketReplication",
            "DeleteBucketReplication",
            "DeleteBucketTagging",
            "PutObjectTagging",
            "PutObjectAcl",
            "PutBucketAcl",
            "ListObjects",
            "RestoreObject",
        ] {
            assert_eq!(
                configuration_document(operation),
                None,
                "case {operation:?}: this call submits no bucket-configuration document, so it \
                 must resolve to none, got {:?}",
                configuration_document(operation)
            );
        }
        // And the one that is catalogued but excluded is excluded because the model makes its
        // document OPTIONAL, which is the reason stated beside the table.
        assert_eq!(
            required_document_payload_in_the_model("PutBucketAcl"),
            None,
            "case \"PutBucketAcl\": its document payload is optional, so the model binds no \
             required one, got {:?}",
            required_document_payload_in_the_model("PutBucketAcl")
        );
    }

    /// RULE: the copy of a tagged SOURCE into a directory bucket names its own restriction,
    /// distinct from the object tag-set plane's sentence, so the two are told apart by the only
    /// thing that differs between them.
    #[test]
    fn copying_a_tagged_source_names_its_own_restriction() {
        assert_eq!(
            unsupported_message(COPYING_SOURCE_OBJECT_TAGS),
            "This bucket does not support copying source objects with Object Tags.",
            "the copy restriction is recorded verbatim"
        );
        assert_ne!(
            unsupported_message(COPYING_SOURCE_OBJECT_TAGS),
            unsupported_message(OBJECT_TAGS),
            "the two restrictions must not be told to a caller in the same sentence"
        );
    }

    /// The `@http` uri `operation` is bound at, as the model binds it.
    fn http_uri_in_the_model(operation: &str) -> Option<String> {
        let model = model();
        model["shapes"][format!("com.amazonaws.s3#{operation}")]["traits"]["smithy.api#http"]
            ["uri"]
            .as_str()
            .map(str::to_string)
    }

    /// RULE, the converse: EVERY catalogued call the model binds a required XML document payload
    /// for is refused for that document, unless the document belongs to an OBJECT rather than to
    /// the bucket — so a bucket-configuration PUT cannot be left on the feature refusal by being
    /// forgotten here.
    ///
    /// Which calls those are is read out of the model (the member marked `@httpPayload` and
    /// `@required`, and the `@http` uri that says whether the call addresses an object), so the
    /// table's membership is the model's statement rather than a list kept by hand.
    #[test]
    fn every_catalogued_bucket_put_that_submits_a_document_is_in_the_table() {
        for (operation, _, _) in UNSUPPORTED_BY_A_DIRECTORY_BUCKET {
            let Some(element) = required_document_payload_in_the_model(operation) else {
                // No required document: a read, a delete, or a PUT whose document is optional
                // (`PutBucketAcl`, `PutObjectLockConfiguration`) — such a request may submit
                // none, so there is nothing for that refusal to name.
                assert_eq!(
                    configuration_document(operation),
                    None,
                    "case {operation:?}: the model binds no required document for this call, so \
                     it must resolve to none, got {:?}",
                    configuration_document(operation)
                );
                continue;
            };
            // An OBJECT's subresource is not a bucket configuration: the uri carries the key
            // label, and such a call is refused for the PLANE it asks for — recorded verbatim
            // for the object tag set.
            let addresses_an_object = http_uri_in_the_model(operation)
                .unwrap_or_default()
                .contains("{Key+}");
            match addresses_an_object {
                true => assert_eq!(
                    configuration_document(operation),
                    None,
                    "case {operation:?}: this call submits an OBJECT's document, so it keeps the \
                     feature refusal, got {:?}",
                    configuration_document(operation)
                ),
                false => assert_eq!(
                    configuration_document(operation),
                    Some(element.as_str()),
                    "case {operation:?}: the model binds the required bucket-configuration \
                     document {element:?} for this call, so it must be refused for that \
                     document, got {:?}",
                    configuration_document(operation)
                ),
            }
        }
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
