// SPDX-License-Identifier: CC0-1.0

//! The documents an `AWS::AppSync::*` declaration holds in AMAZON S3 rather than in the
//! template: read from the object the declaration addresses and submitted as the property
//! whose text they are.
//!
//! ## The rule this module implements
//!
//! Several of these types publish a property PAIR: the document itself, and a reference to an
//! Amazon S3 object holding it — `Code` beside `CodeS3Location`, `Definition` beside
//! `DefinitionS3Location`, a mapping template beside its own `…S3Location`. The two are the
//! same document submitted two ways: the reference says WHERE the text is, not what it means,
//! so a declaration that states the reference is created with the bytes that object holds,
//! carried to the owning call as the member the inline property travels as. That is how real
//! templates ship resolver and function code — `aws cloudformation package` and the CDK emit
//! the reference, never the inline text — so a reference nothing reads is a resource that
//! cannot be created at all.
//!
//! The object is read through the in-process callback into this same emulator
//! ([`crate::state::provisioner::s3_client`]), exactly as the rest of provisioning reaches
//! another service: the document a client staged with its own `PutObject` is the one the
//! resource is created from. Nothing here opens a socket or leaves the host, and nothing is
//! read from the reference but the bucket, the key and the object version it addresses —
//! through the one reading of a caller-addressed object this crate holds
//! ([`crate::state::template_url::addressed_object`]), so a reference may be written in the
//! `s3://<bucket>/<key>` form a template states or in either http(s) addressing style.
//!
//! The document travels to the member whose text it is, and that member's own modelled length
//! is what refuses one that is too long (`CreateFunction$code` is `@length(1, 32768)`). The
//! READ itself is bounded like every other read of a caller-addressed object this service makes
//! ([`MAX_DOCUMENT_BYTES`]), far past any document these members carry, so nothing unbounded is
//! held and nothing is submitted that the owning service's own validation has not judged.
//!
//! ## What a failed read reports
//!
//! The read is the resource provider's OWN step rather than a call of the service that owns
//! the type, so what it reports is that provider's diagnosis of it, nested: the document it
//! could not materialise, and inside that the reference it could not read, carrying the
//! exception an Amazon S3 client raises — that service's own message and its SDK's diagnostic
//! of the refused call ([`read_refusal`]).
//!
//! It is an exception the resource-provider plugin has no mapping for, so it is reported under
//! the plugin's class for a service that failed rather than as a refusal of the request
//! ([`READ_FAILED`]): a client reading the class sees a read to retry or an object to stage,
//! not a template to fix.

use serde_json::{Map, Value};

use super::Refusal;
use crate::state::provisioner::attribute_text;
use crate::state::template_url;

/// The `(reference, document)` pairs the types of a GraphQL API's own estate publish: each
/// Amazon S3 reference beside the property whose text that object holds.
///
/// Source: the properties those types publish, as the catalogue of them records
/// ([`super::appsync_resources`]) — `AWS::AppSync::Resolver` and
/// `AWS::AppSync::FunctionConfiguration` publish `CodeS3Location` beside `Code` and a
/// `…MappingTemplateS3Location` beside each mapping template, and
/// `AWS::AppSync::GraphQLSchema` publishes `DefinitionS3Location` beside `Definition`. A type
/// that publishes neither half of a pair states no such reference at all, so one table answers
/// for all six.
pub(super) const GRAPHQL_ESTATE_DOCUMENTS: &[(&str, &str)] = &[
    ("CodeS3Location", "Code"),
    ("DefinitionS3Location", "Definition"),
    ("RequestMappingTemplateS3Location", "RequestMappingTemplate"),
    (
        "ResponseMappingTemplateS3Location",
        "ResponseMappingTemplate",
    ),
];

/// The one pair an `AWS::AppSync::ChannelNamespace` publishes: that type's `CodeS3Location`
/// holds the text of its `CodeHandlers` — the handler code the namespace runs — rather than of
/// a `Code` property, which it does not publish ([`super::appsync_event_and_merged_apis`]).
pub(super) const CHANNEL_NAMESPACE_DOCUMENTS: &[(&str, &str)] =
    &[("CodeS3Location", "CodeHandlers")];

/// The code a refusal of one of these reads carries, which is the `HandlerErrorCode` class the
/// provider reports it under.
///
/// The provider classified the failure ITSELF: the read is its own step, so there is no error
/// code of the owning service's to pass on and nothing for the plugin to map — and an
/// exception it has no mapping for falls in the class for a service that failed, which is what
/// [`super::Owner::handler_error_class`] reads this code as.
pub(super) const READ_FAILED: &str = super::HANDLER_ERROR_SERVICE_FAILED;

/// What stands in as the inner diagnosis where Amazon S3 identified nothing: the read was not
/// served — or was served and carried no text — and nothing on the other end said why.
const UNIDENTIFIED_FAILURE: &str = "InternalFailure";

/// Amazon S3's own signing name, which routes the read in process and is what the diagnostic
/// looks the SDK's name for that service up under ([`super::sdk_message`]).
const S3_SIGNING_NAME: &str = "s3";

/// How many bytes of a referenced object are read.
///
/// Chosen (AWS publishes no ceiling on an object a resource handler reads): the object is
/// caller-staged, so the read of one is bounded like every other read of a caller-addressed
/// object this service makes ([`crate::ops::register_type`] bounds a handler package the same
/// way), and this is far past the largest document any of these members carries — `code` is
/// `@length(1, 32768)` and a mapping template `@length(1, 65536)`, and a schema document is a
/// type system written by hand. An object within it travels whole, so a document longer than
/// the member admits is refused by that member's own modelled length rather than here; one past
/// it is not read at all ([`too_large`]).
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// What the read reports for an object past [`MAX_DOCUMENT_BYTES`]: it was not read at all,
/// which is a different thing from a read Amazon S3 would not serve. Fixed text naming the
/// member the reference was carried on, with nothing of the request in it.
fn too_large(reference: &str) -> String {
    format!("{reference} exceeds the maximum document size.")
}

/// `properties` with every document its Amazon S3 references address READ and stated as the
/// property whose text it is, or `None` for a declaration that states no such reference — in
/// which case the declaration is already the document the create submits.
///
/// A declaration that states BOTH halves of a pair is created with the document it states
/// inline: that text is what it asked for, and the reference is what the provider resolves in
/// its absence.
pub(super) async fn fetched(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
    documents: &[(&str, &str)],
) -> Result<Option<Value>, Refusal> {
    let Value::Object(stated) = properties else {
        return Ok(None);
    };
    let mut fetched: Option<Map<String, Value>> = None;
    for (reference, document) in documents {
        let Some(location) = addressed(stated, reference) else {
            continue;
        };
        if addressed(stated, document).is_some() {
            continue;
        }
        let text = read(ctx, region, document, reference, &location).await?;
        let resolved = fetched.get_or_insert_with(|| stated.clone());
        resolved.insert((*document).to_string(), Value::String(text));
        // The reference itself is not a member of any owning call: what the resource is
        // created with is the document it addressed.
        resolved.remove(*reference);
    }
    Ok(fetched.map(Value::Object))
}

/// One stated property as the text it carries, or `None` where the declaration states none —
/// read through the one reading of a declared value this crate holds, so a value an intrinsic
/// resolved to is the text it resolved to.
fn addressed(stated: &Map<String, Value>, property: &str) -> Option<String> {
    stated
        .get(property)
        .filter(|value| !value.is_null())
        .map(attribute_text)
        .filter(|text| !text.is_empty())
}

/// The TEXT of the object `location` addresses, read from this emulator's own Amazon S3.
async fn read(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    document: &str,
    reference: &str,
    location: &str,
) -> Result<String, Refusal> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    let object = template_url::addressed_object(location).ok_or_else(|| {
        // Text that addresses no object was never a reference to one, which is the sentence
        // this service words about a member carrying such text.
        read_refusal(
            document,
            reference,
            &template_url::not_a_readable_object(reference),
        )
    })?;
    let answered = crate::state::provisioner::s3_client(ctx, region)
        .get_object()
        .bucket(&object.bucket)
        .key(&object.key)
        .set_version_id(object.version.clone())
        .send()
        .await
        .map_err(|error| {
            let code = error.code().unwrap_or(UNIDENTIFIED_FAILURE);
            let message = error.message().unwrap_or(code);
            // The exception an Amazon S3 client raises for the refused read: that service's
            // own message and its SDK's diagnostic of the call, which is what the provider
            // caught. An answer that carried no response at all is reported as it was worded,
            // with no diagnostic invented for it.
            let raised = match error.raw_response().map(|answer| answer.status().as_u16()) {
                Some(status) => super::sdk_message(S3_SIGNING_NAME, status, code, message),
                None => message.to_string(),
            };
            read_refusal(document, reference, &raised)
        })?;
    // The length the object REPORTS, before a single byte of it is held.
    if answered
        .content_length()
        .is_some_and(|length| length > MAX_DOCUMENT_BYTES as i64)
    {
        return Err(read_refusal(document, reference, &too_large(reference)));
    }
    let body = answered
        .body
        .collect()
        .await
        // The object was handed over and the body then stopped: nothing identified why.
        .map_err(|_| read_refusal(document, reference, UNIDENTIFIED_FAILURE))?
        .into_bytes();
    if body.len() > MAX_DOCUMENT_BYTES {
        return Err(read_refusal(document, reference, &too_large(reference)));
    }
    // What the reference addressed is the TEXT of a property; an object that is not valid
    // UTF-8 carries none, so the read served no document.
    String::from_utf8(body.to_vec())
        .map_err(|_| read_refusal(document, reference, UNIDENTIFIED_FAILURE))
}

/// The refusal the provider reports for a document it could not read: its own diagnosis of the
/// step it failed at, nested — the DOCUMENT it was materialising, and inside that the
/// REFERENCE it was reading and what the read answered.
///
/// Both halves are worded as the provider words a step it could not carry out
/// ([`crate::state::provisioner::handler_refusal`], whose no-message form is that sentence),
/// so the shape a client parses is the one every provider failure of this emulator carries.
fn read_refusal(document: &str, reference: &str, answered: &str) -> Refusal {
    let reading = Refusal {
        code: READ_FAILED.to_string(),
        message: String::new(),
    };
    let read_reference = crate::state::provisioner::handler_refusal(
        &format!("Error reading {reference} from S3. Error message: {answered}"),
        reading.clone(),
    );
    crate::state::provisioner::handler_refusal(
        &format!("Error reading {document} from S3.{}", read_reference.message),
        reading,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a reference's refusal is the provider's nested diagnosis — the document it was
    /// materialising around the reference it was reading and what that read answered — and it
    /// carries the class the provider reports such a failure under.
    ///
    /// The rows differ along which pair the reference belongs to, which is what the rule
    /// generalises over: the code of a resolver or a function, a schema's definition, a mapping
    /// template, and a channel namespace's handlers.
    #[test]
    fn a_failed_read_is_nested_in_the_document_it_was_materialising() {
        // (case, reference, document, what the read answered)
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "a code object the bucket holds nothing under",
                "CodeS3Location",
                "Code",
                "The specified key does not exist. (Service: S3, Status Code: 404, Request ID: \
                 A1, Extended Request ID: B2) (SDK Attempt Count: 1)",
            ),
            (
                "a schema definition in a bucket that is not there",
                "DefinitionS3Location",
                "Definition",
                "The specified bucket does not exist (Service: S3, Status Code: 404, Request \
                 ID: C3, Extended Request ID: D4) (SDK Attempt Count: 1)",
            ),
            (
                "a mapping template the read was not permitted to make",
                "RequestMappingTemplateS3Location",
                "RequestMappingTemplate",
                "Access Denied (Service: S3, Status Code: 403, Request ID: E5, Extended \
                 Request ID: F6) (SDK Attempt Count: 1)",
            ),
            (
                "a namespace's handlers, reported with nothing identified",
                "CodeS3Location",
                "CodeHandlers",
                UNIDENTIFIED_FAILURE,
            ),
        ];
        for (case, reference, document, answered) in rows {
            let refusal = read_refusal(document, reference, answered);
            let expected = format!(
                "Error occurred during operation 'Error reading {document} from S3.Error \
                 occurred during operation 'Error reading {reference} from S3. Error message: \
                 {answered}'.'."
            );
            assert_eq!(
                refusal.message, expected,
                "case {case:?}: expected the nested diagnosis, got {:?}",
                refusal.message
            );
            assert_eq!(
                refusal.code, READ_FAILED,
                "case {case:?}: expected the class the provider reports its own failed read \
                 under, got {:?}",
                refusal.code
            );
        }
    }

    /// RULE: every entry pairs an Amazon S3 REFERENCE with the document property whose text
    /// that object holds, and no table states one reference twice.
    #[test]
    fn each_reference_names_the_one_document_it_holds_the_text_of() {
        for (case, documents) in [
            ("a GraphQL API's estate", GRAPHQL_ESTATE_DOCUMENTS),
            ("a channel namespace", CHANNEL_NAMESPACE_DOCUMENTS),
        ] {
            for (reference, document) in documents {
                assert!(
                    reference.ends_with(S3_LOCATION_SUFFIX),
                    "case {case:?}: expected a reference property, got {reference:?}"
                );
                assert!(
                    !document.is_empty() && !document.ends_with(S3_LOCATION_SUFFIX),
                    "case {case:?}: expected {reference:?} to name a document property, got \
                     {document:?}"
                );
                let stated = documents
                    .iter()
                    .filter(|(named, _)| named == reference)
                    .count();
                assert_eq!(
                    stated, 1,
                    "case {case:?}: expected {reference:?} to be stated once, got {stated}"
                );
            }
        }
    }

    /// How every property that REFERENCES an Amazon S3 object rather than carrying a document
    /// is spelled: the published name of these types' reference properties.
    const S3_LOCATION_SUFFIX: &str = "S3Location";
}
