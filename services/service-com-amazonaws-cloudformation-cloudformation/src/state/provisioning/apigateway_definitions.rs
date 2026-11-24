// SPDX-License-Identifier: CC0-1.0

//! The OpenAPI definition an api DECLARATION holds in Amazon S3 rather than in the template:
//! read from the object the declaration addresses and imported as the document it is.
//!
//! ## The rule this module implements
//!
//! Both API Gateway control planes publish the same property PAIR on their api type: `Body`,
//! the definition itself, and `BodyS3Location`, a reference to an Amazon S3 object holding it
//! ("to import an HTTP API, you must specify a Body or BodyS3Location"). The two are the same
//! document submitted two ways — the reference says WHERE the text is, not what it means — so
//! a declaration that states the reference is imported from the bytes that object holds,
//! carried to the import as the member the inline property travels as. That is how real
//! templates ship an api definition (`aws cloudformation package` and the CDK emit the
//! reference, never the inline document), so a reference nothing reads is an api that cannot
//! be created at all.
//!
//! The object is read through the in-process callback into this same emulator
//! ([`crate::state::provisioner::s3_client`]), exactly as the rest of provisioning reaches
//! another service and through the one reading a resource PROVIDER of this crate makes of a
//! staged object ([`super::staged_object`], which the AppSync documents beside this one are
//! read through too): the document a client staged with its own `PutObject` is the one the api
//! is imported from. Nothing here opens a socket or leaves the host, and nothing is read from
//! the reference but the bucket, the key and the object version it addresses. The READ is
//! bounded like every other read of a caller-addressed object this service makes
//! ([`MAX_DEFINITION_BYTES`]), and the document itself is judged by the owning service's own
//! import, which is what refuses one that is not an api definition.
//!
//! ## What a failed read reports
//!
//! The read is the resource provider's OWN step rather than a call of the service that owns
//! the type, so what it reports is the exception THAT handler raises for a definition
//! reference it could not materialise: `Unable to parse BodyS3Location`, followed by the two
//! diagnostics it carries — the handler's own exception, which identified nothing, and the
//! one it was raised as, stating the status the read answered and the api control plane's own
//! bad-request exception class ([`unable_to_parse`]).
//!
//! It is an exception the resource-provider plugin has no mapping for, so it is reported under
//! the plugin's class for a service that failed rather than as a refusal of the request
//! ([`READ_FAILED`]), which is the class the recorded reason of such a resource carries: a
//! client reading the class sees a read to retry or an object to stage, not a template to fix.

use serde_json::Value;

use super::staged_object::{self, ReadFailure};
use super::Refusal;
use crate::state::provisioner::attribute_text;
use crate::state::template_url;

/// The property that carries an api's definition as an Amazon S3 OBJECT, under the one name
/// both api types publish it: `AWS::ApiGateway::RestApi.BodyS3Location` and
/// `AWS::ApiGatewayV2::Api.BodyS3Location`. Stated here because the sentence a failed read
/// reports names it.
pub(in crate::state) const BODY_S3_LOCATION: &str = "BodyS3Location";

/// The members that reference states, as both types publish them: the bucket holding the
/// object, its key, and the object version to read ("for versioning-enabled buckets").
const LOCATION_BUCKET: &str = "Bucket";
const LOCATION_KEY: &str = "Key";
const LOCATION_VERSION: &str = "Version";

/// The code a refusal of one of these reads carries, which is the `HandlerErrorCode` class the
/// provider reports it under.
///
/// The provider classified the failure ITSELF: the read is its own step, so there is no error
/// code of the owning service's to pass on and nothing for the plugin to map — and an
/// exception it has no mapping for falls in the class for a service that failed, which is what
/// [`super::Owner::handler_error_class`] reads this code as. Recorded behaviour: the reason of
/// an `AWS::ApiGatewayV2::Api` whose declared `BodyS3Location` names an object that is not
/// there closes `HandlerErrorCode: GeneralServiceException` (in the behavioral corpus
/// `simparity-behavioral-aws-v1`, check `apigatewayv2__cloudformation.deploy-apigatewayv2-\
/// cfn-bodys3location-missing-object-fails-stack`).
pub(in crate::state) const READ_FAILED: &str = super::HANDLER_ERROR_SERVICE_FAILED;

/// The sentence the handler of either api type words for a definition reference it could not
/// materialise, which is what the quoted message of the failed resource opens with.
///
/// Fixed text naming the property the reference was carried on, with nothing of the request in
/// it. Read from the same recorded reason [`READ_FAILED`] cites.
fn unable_to_parse_sentence() -> String {
    format!("Unable to parse {BODY_S3_LOCATION}")
}

/// The exception class the api control planes answer a request they will not take under
/// (`BadRequestException`, the modelled 400 of `ImportApi` and of `ImportRestApi`), which is
/// the class the handler raises its own refusal of a definition reference as.
const BAD_REQUEST_EXCEPTION: &str = "BadRequestException";

/// What each field of a diagnostic the handler raised ITSELF states: the handler built the
/// exception rather than receiving it from a called service, so the service it names, the
/// error code it carries and the call's id are all unset, and so is the proxy every such
/// diagnostic closes with ([`super::SdkDiagnostic::Proxied`] is the same shape for a call that
/// WAS answered).
const UNSET_FIELD: &str = "null";

/// The status such a diagnostic states for an exception no answer of any service reached:
/// nothing was answered, so there is no status to report.
const NO_ANSWERED_STATUS: u16 = 0;

/// One diagnostic in the shape the handler of an api type words, semicolon-separated: the
/// service, the HTTP status, the error code, the call's id and the proxy the call was made
/// through — every field unset but the status and the code, which are the two this handler
/// fills in.
fn diagnostic(status: u16, code: &str) -> String {
    format!(
        "(Service: {UNSET_FIELD}; Status Code: {status}; Error Code: {code}; \
         Request ID: {UNSET_FIELD}; Proxy: {UNSET_FIELD})"
    )
}

/// The refusal the handler reports for a definition reference it could not read: its own
/// sentence, the diagnostic of the exception it raised — which identified nothing — and the
/// diagnostic of the exception it raised that one AS, carrying the status the read answered
/// and that control plane's own bad-request class.
///
/// `status` is what the read ANSWERED, and `None` a read no answer came back for at all — a
/// reference addressing no object, or an object handed over and then not served — which states
/// the same unanswered status the inner diagnostic does.
fn unable_to_parse(status: Option<u16>) -> Refusal {
    Refusal {
        code: READ_FAILED.to_string(),
        message: format!(
            "{} {} {}",
            unable_to_parse_sentence(),
            diagnostic(NO_ANSWERED_STATUS, UNSET_FIELD),
            diagnostic(status.unwrap_or(NO_ANSWERED_STATUS), BAD_REQUEST_EXCEPTION),
        ),
    }
}

/// How many bytes of a referenced definition are read.
///
/// Chosen (AWS publishes no ceiling on an object a resource handler reads): the object is
/// caller-staged, so the read of one is bounded like every other read of a caller-addressed
/// object this service makes ([`super::appsync_s3_documents`] bounds its documents the same
/// way), and this is far past any api definition — the document travels whole, and one the
/// owning import will not take is refused by that import rather than here.
const MAX_DEFINITION_BYTES: usize = 1024 * 1024;

/// The TEXT of the definition `location` addresses, read from this emulator's own Amazon S3.
///
/// `location` is the reference as the declaration states it: the `{Bucket, Key, Version}`
/// members both types publish, or text addressing the same object, which is read through the
/// one reading of a caller-addressed object this crate holds
/// ([`template_url::addressed_object`]).
///
/// Every failure is the one refusal the handler words for a reference it could not materialise
/// ([`unable_to_parse`]), so a client parses one shape whether the object is not there, the
/// read was not permitted, the reference addresses no object at all or what it addresses
/// carries no document.
pub(in crate::state) async fn read_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    location: &Value,
) -> Result<String, Refusal> {
    let object = addressed(location).ok_or_else(|| unable_to_parse(None))?;
    staged_object::text(ctx, region, &object, MAX_DEFINITION_BYTES)
        .await
        // The one field of the handler's diagnostic a failed read fills in is the STATUS
        // Amazon S3 answered it with; a failure no answer of that service's came back for —
        // an object past the bound, a read that served no document — fills in none.
        .map_err(|failure| match failure {
            ReadFailure::Refused { status, .. } => unable_to_parse(status),
            _ => unable_to_parse(None),
        })
}

/// The Amazon S3 object one stated reference addresses, or `None` for a value that addresses
/// none.
///
/// The reference is the object stating the members both types publish; a value that resolved
/// to TEXT addresses the same object, and is read through the one reading of an addressed
/// object this crate holds.
fn addressed(location: &Value) -> Option<template_url::Object> {
    let Some(members) = location.as_object() else {
        return template_url::addressed_object(&attribute_text(location));
    };
    let stated = |member: &str| {
        members
            .get(member)
            .filter(|value| !value.is_null())
            .map(attribute_text)
            .filter(|text| !text.is_empty())
    };
    Some(template_url::Object {
        bucket: stated(LOCATION_BUCKET)?,
        key: stated(LOCATION_KEY)?,
        version: stated(LOCATION_VERSION),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// RULE: a definition reference the handler could not read is reported as that handler's
    /// own exception — its sentence naming the property, the diagnostic of the exception it
    /// raised, which identified nothing, and the diagnostic it was raised as, carrying the
    /// status the read answered and the api control plane's bad-request class — under the
    /// class the plugin reports an exception it has no mapping for.
    ///
    /// The rows differ along what the rule generalises over: WHAT the read answered — a status
    /// for an object no bucket holds, one for a read that was not permitted, one for a service
    /// that failed, and no answer at all for a reference addressing no object.
    #[test]
    fn a_failed_read_is_the_exception_the_handler_raised() {
        // (case, the status the read answered)
        let rows: &[(&str, Option<u16>)] = &[
            ("an object the bucket holds nothing under", Some(404)),
            ("a read that was not permitted", Some(403)),
            ("a read the owning service failed", Some(500)),
            ("a reference addressing no object at all", None),
        ];
        for (case, status) in rows {
            let refusal = unable_to_parse(*status);
            let expected = format!(
                "Unable to parse BodyS3Location (Service: null; Status Code: 0; Error Code: \
                 null; Request ID: null; Proxy: null) (Service: null; Status Code: {}; Error \
                 Code: BadRequestException; Request ID: null; Proxy: null)",
                status.unwrap_or(0),
            );
            assert_eq!(
                refusal.message, expected,
                "case {case:?}: expected the handler's own exception, got {:?}",
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

    /// The object a row expects a reference to address, written as the reading itself answers
    /// it.
    fn object(bucket: &str, key: &str, version: Option<&str>) -> template_url::Object {
        template_url::Object {
            bucket: bucket.to_string(),
            key: key.to_string(),
            version: version.map(ToString::to_string),
        }
    }

    /// RULE: a reference states the object by the members the type publishes, and a value that
    /// resolved to text addresses the same object; one that names no bucket and key addresses
    /// nothing.
    #[test]
    fn a_reference_addresses_the_object_it_states() {
        // (case, the stated reference, the object it addresses)
        let rows: &[(&str, Value, Option<template_url::Object>)] = &[
            (
                "the members the type publishes",
                json!({ "Bucket": "definitions", "Key": "openapi.json" }),
                Some(object("definitions", "openapi.json", None)),
            ),
            (
                "a reference naming an object version",
                json!({ "Bucket": "definitions", "Key": "api/v2.json", "Version": "v7" }),
                Some(object("definitions", "api/v2.json", Some("v7"))),
            ),
            (
                "a reference whose key is a nested path",
                json!({ "Bucket": "staging", "Key": "apis/http/openapi.yaml" }),
                Some(object("staging", "apis/http/openapi.yaml", None)),
            ),
            (
                "a reference that resolved to an s3 uri",
                json!("s3://definitions/openapi.json"),
                Some(object("definitions", "openapi.json", None)),
            ),
            (
                "a reference stating no key",
                json!({ "Bucket": "definitions" }),
                None,
            ),
            (
                "a reference whose bucket is empty",
                json!({ "Bucket": "", "Key": "openapi.json" }),
                None,
            ),
            (
                "a reference that addresses nothing",
                json!("openapi.json"),
                None,
            ),
        ];
        for (case, location, expected) in rows {
            let addressed = addressed(location);
            assert_eq!(
                addressed.as_ref(),
                expected.as_ref(),
                "case {case:?}: expected the object {expected:?} the reference states, got \
                 {addressed:?}"
            );
        }
    }
}
