// SPDX-License-Identifier: CC0-1.0

//! The `deploymentTemplates` member a SaaS fulfillment option carries on the wire.
//!
//! AWS answers `ListFulfillmentOptions` with a `deploymentTemplates` member inside every
//! `saasFulfillmentOption` — present, and holding no element for an option the catalog holds
//! no deployment template for. The model this crate is generated from declares the member on
//! no shape (`smithy_query(query="shape",shape="SaasFulfillmentOption")` lists
//! `fulfillmentOptionId`, `fulfillmentOptionType`, `fulfillmentOptionDisplayName`,
//! `fulfillmentUrl`, `usageInstructions`, `availableFromTime`, `launchUrl` and `quickLaunch`
//! and nothing else), because a checked-in model is a SNAPSHOT of an API the live wire has
//! moved past. A member no shape declares can be neither a field of a generated shape nor
//! written by the schema serializer, so the only layer that can write it is the one holding
//! the body the serializer just rendered — which is what
//! [`service_common::success_body_members`] owns the plumbing of, and what this module takes
//! exactly one decision in: on a `ListFulfillmentOptions` success body, every SaaS option
//! that carries no `deploymentTemplates` member gains one, holding the templates the record
//! names — none, for every option this catalog holds, so an empty list.
//!
//! Why the distinction is worth making: an ABSENT member and a member holding an empty list
//! are different cases to a typed client. A client that reads the member to decide whether an
//! option offers deployment templates finds nothing to iterate where AWS hands it a list to
//! iterate zero times, and a strict reader faults on the missing member instead.
//!
//! Every other member, every other operation's body and every failure keep the bytes their
//! renderer wrote.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::serde_json::value::RawValue;
use ::service_common::success_body_members::{raw_value, with_rewritten_success_body, RawObject};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The output member holding the options a product is fulfilled through
/// (`ListFulfillmentOptionsOutput$fulfillmentOptions`).
const FULFILLMENT_OPTIONS_MEMBER: &str = "fulfillmentOptions";

/// The `FulfillmentOption` union member a SaaS option is served as.
const SAAS_MEMBER: &str = "saasFulfillmentOption";

/// The member of a SaaS option naming the deployment templates it offers.
const DEPLOYMENT_TEMPLATES_MEMBER: &str = "deploymentTemplates";

/// The value that member carries for an option the catalog holds no deployment template for:
/// a list, holding nothing. Rendered as its own bytes because the member is written into the
/// body rather than through a schema.
const NO_DEPLOYMENT_TEMPLATES: &[u8] = b"[]";

/// `body` with the `deploymentTemplates` member written into every SaaS option that carries
/// none, or `None` when there is nothing to write: the body is not a JSON object, it carries
/// no `fulfillmentOptions` list, or every SaaS option in it already names its templates.
///
/// Every value the renderer wrote is written back as the bytes it was read as — a
/// timestamp's own digits included — because the body is read and rewritten through
/// `serde_json`'s raw values.
fn with_deployment_templates(body: &[u8]) -> Option<Vec<u8>> {
    // The rewritten list first, so the bytes it is held as outlive the object that refers to
    // them.
    let listed = options_naming_their_templates(body)?;
    let mut object: RawObject<'_> = ::serde_json::from_slice(body).ok()?;
    object.insert(
        FULFILLMENT_OPTIONS_MEMBER.to_string(),
        raw_value(&listed)?,
    );
    ::serde_json::to_vec(&object).ok()
}

/// The rendered `fulfillmentOptions` list with every SaaS option naming its deployment
/// templates, or `None` when no option of the list needed the member written.
fn options_naming_their_templates(body: &[u8]) -> Option<Vec<u8>> {
    let object: RawObject<'_> = ::serde_json::from_slice(body).ok()?;
    let options: Vec<&RawValue> =
        ::serde_json::from_str(object.get(FULFILLMENT_OPTIONS_MEMBER)?.get()).ok()?;
    let written: Vec<Option<Vec<u8>>> = options
        .iter()
        .map(|option| option_naming_its_templates(option))
        .collect();
    if written.iter().all(Option::is_none) {
        return None;
    }
    let listed: Vec<&RawValue> = options
        .iter()
        .zip(&written)
        .map(|(option, written)| match written.as_deref().and_then(raw_value) {
            Some(rewritten) => rewritten,
            // An option this rewrite does not own keeps the bytes its renderer wrote.
            None => *option,
        })
        .collect();
    ::serde_json::to_vec(&listed).ok()
}

/// One rendered fulfillment option with its SaaS member naming its deployment templates, or
/// `None` for an option this rewrite leaves alone: one served as another union member, and
/// one whose SaaS member already names them.
fn option_naming_its_templates(option: &RawValue) -> Option<Vec<u8>> {
    // The SaaS member's own bytes first, for the same reason as above.
    let saas = saas_naming_its_templates(option)?;
    let mut object: RawObject<'_> = ::serde_json::from_str(option.get()).ok()?;
    object.insert(SAAS_MEMBER.to_string(), raw_value(&saas)?);
    ::serde_json::to_vec(&object).ok()
}

/// The SaaS member of one rendered option with `deploymentTemplates` written into it, or
/// `None` when the option carries no SaaS member or that member already names its templates.
fn saas_naming_its_templates(option: &RawValue) -> Option<Vec<u8>> {
    let object: RawObject<'_> = ::serde_json::from_str(option.get()).ok()?;
    let mut saas: RawObject<'_> = ::serde_json::from_str(object.get(SAAS_MEMBER)?.get()).ok()?;
    if saas.contains_key(DEPLOYMENT_TEMPLATES_MEMBER) {
        return None;
    }
    saas.insert(
        DEPLOYMENT_TEMPLATES_MEMBER.to_string(),
        raw_value(NO_DEPLOYMENT_TEMPLATES)?,
    );
    ::serde_json::to_vec(&saas).ok()
}

/// The response as this service answers it: a `ListFulfillmentOptions` success whose SaaS
/// options each name the deployment templates they offer.
#[must_use]
pub fn naming_deployment_templates(resp: RtResponse) -> RtResponse {
    with_rewritten_success_body(resp, with_deployment_templates)
}

/// `ListFulfillmentOptions` served through the envelope it is already served through — so
/// the framework still owns the whole deserialize → validate → handle → serialize sequence,
/// and this service still words a refused call itself — and then with the member above
/// written into every SaaS option of the answer.
pub struct DeploymentTemplatesOperation<O> {
    /// The envelope this wrapper delegates to, held in a field because
    /// [`ErasedOperation::run`] borrows the operation it runs for the whole call.
    envelope: O,
}

impl<O> DeploymentTemplatesOperation<O> {
    /// Wrap one already-constructed operation envelope.
    pub fn new(envelope: O) -> Self {
        Self { envelope }
    }
}

impl<O: ErasedOperation> ErasedOperation for DeploymentTemplatesOperation<O> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            naming_deployment_templates(self.envelope.run(svc, req, ctx).await)
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not change
    /// which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.envelope.auth_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a SaaS option of a rendered answer names its deployment templates, and nothing
    /// else about the body changes. The rows differ along what the body carries beside the
    /// member: another union member, an option that already names its templates, several
    /// options at once, and bodies this rewrite does not own.
    #[test]
    fn every_saas_option_names_its_deployment_templates() {
        // (case, the rendered body, whether the rewrite owns it)
        for (case, body, rewritten) in [
            (
                "one SaaS option",
                r#"{"fulfillmentOptions":[{"saasFulfillmentOption":{"fulfillmentOptionId":"fo-1","quickLaunch":"DISABLED"}}]}"#,
                true,
            ),
            (
                "a SaaS option beside an option of another type",
                r#"{"fulfillmentOptions":[{"apiFulfillmentOption":{"fulfillmentOptionId":"fo-2"}},{"saasFulfillmentOption":{"fulfillmentOptionId":"fo-3"}}],"nextToken":"t"}"#,
                true,
            ),
            (
                "two SaaS options",
                r#"{"fulfillmentOptions":[{"saasFulfillmentOption":{"fulfillmentOptionId":"fo-4"}},{"saasFulfillmentOption":{"fulfillmentOptionId":"fo-5"}}]}"#,
                true,
            ),
            (
                "an option that already names its templates",
                r#"{"fulfillmentOptions":[{"saasFulfillmentOption":{"deploymentTemplates":[]}}]}"#,
                false,
            ),
            (
                "no SaaS option at all",
                r#"{"fulfillmentOptions":[{"apiFulfillmentOption":{"fulfillmentOptionId":"fo-6"}}]}"#,
                false,
            ),
            ("an empty list", r#"{"fulfillmentOptions":[]}"#, false),
            ("another operation's body", r#"{"productId":"p-1"}"#, false),
            ("not an object", r#"[{"fulfillmentOptions":[]}]"#, false),
            ("not JSON at all", "<FulfillmentOptions/>", false),
        ] {
            let answered = with_deployment_templates(body.as_bytes());
            assert_eq!(
                answered.is_some(),
                rewritten,
                "case {case:?}: expected rewritten={rewritten}, got {answered:?}"
            );
            let Some(answered) = answered else {
                continue;
            };
            let rendered = String::from_utf8(answered).expect("the body is text");
            let options = rendered.matches(SAAS_MEMBER).count();
            let naming = rendered.matches(r#""deploymentTemplates":[]"#).count();
            assert_eq!(
                naming, options,
                "case {case:?}: every SaaS option names its templates, got {rendered}"
            );
        }
    }

    /// Every value the renderer wrote survives the rewrite, the digits of a timestamp
    /// included — the member is written into the body, so nothing beside it may be
    /// re-rendered.
    #[test]
    fn the_rest_of_the_answer_survives_the_rewrite() {
        let body = r#"{"locale":"en-US","fulfillmentOptions":[{"saasFulfillmentOption":{"fulfillmentOptionId":"fo-7","availableFromTime":1719446400.0,"fulfillmentUrl":"https://127.0.0.1:9297/_aws/x/fo-7/register","quickLaunch":"DISABLED"}}],"nextToken":"t"}"#;
        let rewritten = with_deployment_templates(body.as_bytes()).expect("the member is written");
        let rendered = String::from_utf8(rewritten).expect("the body is text");
        for kept in [
            r#""locale":"en-US""#,
            r#""availableFromTime":1719446400.0"#,
            r#""fulfillmentOptionId":"fo-7""#,
            r#""fulfillmentUrl":"https://127.0.0.1:9297/_aws/x/fo-7/register""#,
            r#""quickLaunch":"DISABLED""#,
            r#""nextToken":"t""#,
            r#""deploymentTemplates":[]"#,
        ] {
            assert!(
                rendered.contains(kept),
                "expected {kept:?} in the rewritten answer, got {rendered}"
            );
        }
    }
}
