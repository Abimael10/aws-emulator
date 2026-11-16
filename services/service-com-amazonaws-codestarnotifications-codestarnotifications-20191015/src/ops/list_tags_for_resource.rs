// SPDX-License-Identifier: CC0-1.0

//! @op ListTagsForResource
//!
//! Reports the tags the notification rule [`crate::state`] holds for the ARN — the same
//! map `TagResource` left there and `DescribeNotificationRule` reports. A REPORTING
//! operation: it reads the store and writes nothing.
//!
//! The engine is held on `self` (constructor-injected); `ctx` carries request metadata
//! only — the request's SigV4 scope is the region whose rules this endpoint addresses.
//!
//! ---
//!
//! # Modeled contract for `ListTagsForResource`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/smithy-model/com-amazonaws-codestarnotifications-codestarnotifications-20191015.json")
//! ```
//!
//! Returns a list of the tags associated with a notification rule.
//!
//! ## Input
//!
//! * `Arn` (NotificationRuleArn, pattern
//!   ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$)
//!   **required** — The Amazon Resource Name (ARN) for the notification rule.
//!
//! ## Output
//!
//! * `Tags` (Tags) — The tags associated with the notification rule.
//!
//! ## Errors
//!
//! * `ResourceNotFoundException` (HTTP 404) — CodeStar Notifications can't find a resource
//!   that matches the provided ARN.
//! * `ValidationException` (HTTP 400) — One or more parameter values are not valid.
//! * `AccessDeniedException` (HTTP 403) — widened in through `extra_errors` (see below):
//!   the answer the resource-based authorization check gives an `Arn` that names no
//!   notification rule of this service.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListTagsForResourceError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-codestarnotifications-codestarnotifications-20191015.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListTagsForResourceError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
// `AccessDeniedException` is widened in through `extra_errors`: this SERVICE models the
// shape (`@httpError 403`, "you do not have sufficient permissions") and it is the answer
// the resource-based authorization check gives an `Arn` that names no notification rule,
// which every operation addressing a rule by ARN applies before it looks one up.
#[smithy_macros::operation_error(
    "com.amazonaws.codestarnotifications#ListTagsForResource",
    extra_errors = ["AccessDeniedException"]
)]
#[derive(Debug)]
pub enum ListTagsForResourceErrorModeled {
    AccessDeniedException(AccessDeniedException),
    ResourceNotFoundException(ResourceNotFoundException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_tags_for_resource::ListTagsForResourceError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListTagsForResourceError = crate::scaffolded::Scaffolded<ListTagsForResourceErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListTagsForResourceErrorModeled> for ListTagsForResourceError {
    fn from(modeled: ListTagsForResourceErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// HTTP 403 `AccessDeniedException`: the caller is not authorized for the resource the
/// request named, stated in this service's one text for the class.
fn not_authorized(arn: &str) -> ListTagsForResourceError {
    ListTagsForResourceErrorModeled::AccessDeniedException(AccessDeniedException {
        message: Some(crate::state::not_authorized_for(arn)),
    })
    .into()
}

/// HTTP 404 `ResourceNotFoundException`: no resource of this account and region matches
/// the ARN the request named. The TAGGING paths state it over the whole submitted ARN,
/// which is how they address the resource.
fn not_found(arn: &str) -> ListTagsForResourceError {
    ListTagsForResourceErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
        message: Some(crate::state::no_resource_with_arn(arn)),
    })
    .into()
}

/// The `ListTagsForResource` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListTagsForResource<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListTagsForResource<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListTagsForResource<Eng>
{
    type Input = ListTagsForResourceInput;
    type Output = ListTagsForResourceOutput;
    type Error = ListTagsForResourceError;

    const NAME: &'static str = "ListTagsForResource";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_TAGS_FOR_RESOURCE_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_TAGS_FOR_RESOURCE_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state as st;

        let region = st::request_region(ctx.parts());

        // Resource authorization comes FIRST, before any lookup: an `Arn` outside the
        // notification-rule form names no resource of this service, so nothing can
        // authorize the caller for it. A well-formed ARN no rule holds is the 404 below.
        if !st::is_notification_rule_arn(&input.arn) {
            return Err(not_authorized(&input.arn));
        }

        let Some(state) = st::service_state(&*self.engine) else {
            // This service's store is the shared `InMemoryEngine`. Mounted on any other
            // engine there is no notification-rule store to report from, which is a
            // "not written for this engine" condition, not a client error.
            return Err(crate::scaffolded::Scaffolded::not_implemented(
                "ListTagsForResource (no state store on this engine)",
            ));
        };
        let store = state.lock();

        let Some(rule) = store.rule_in_region(&region, &input.arn) else {
            return Err(not_found(&input.arn));
        };

        Ok(ListTagsForResourceOutput {
            // The rule's own tag map, read off the stored rule: a rule carrying no tags
            // holds zero entries, which is an EMPTY MAP on the wire, not an absent member.
            tags: Some(
                rule.tags
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ),
        })
    }
}
