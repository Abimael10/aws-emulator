// SPDX-License-Identifier: CC0-1.0

//! @op GetCelebrityInfo
//!
//! ## What this handler does
//!
//! `GetCelebrityInfo` "gets the name and additional information about a celebrity based on
//! their Amazon Rekognition ID", so it is a READ of one celebrity this service holds, by the
//! identifier `RecognizeCelebrities` reported for it: "you get the celebrity ID from a call to
//! the RecognizeCelebrities operation" (the `Id` member's own documentation). The celebrities
//! are the seeded catalogue in [`crate::state::celebrity_catalogue`], which is the SAME
//! catalogue `RecognizeCelebrities` resolves a measured face against, so an `Id` that
//! operation handed out resolves here and the two operations cannot disagree about one
//! celebrity's name, gender or links.
//!
//! It answers the three members the model carries for a catalogue entry — its `Name`, its
//! `KnownGender` and its `Urls`, "an array of URLs pointing to additional celebrity
//! information" — and nothing else, since nothing else is known about one. An `Id` the
//! catalogue does not hold is a resource that cannot be found, which is the class this API
//! models for it; it reads the catalogue and writes nothing, so two identical calls answer
//! identically.
//!
//! ---
//!
//! # Modeled contract for `GetCelebrityInfo`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-rekognition-rekognitionservice/smithy-model/com-amazonaws-rekognition-rekognitionservice.json")
//! ```
//!
//! Gets the name and additional information about a celebrity based on their Amazon
//! Rekognition ID. The additional information is returned as an array of URLs. If there is
//! no additional information about the celebrity, this list is empty. For more information,
//! see Getting information about a celebrity in the Amazon Rekognition Developer Guide.
//! This operation requires permissions to perform the rekognition:GetCelebrityInfo action.
//!
//! ## Input
//!
//! * `Id` (RekognitionUniqueId, pattern ^[0-9A-Za-z]*$) **required** — The ID for the
//!   celebrity. You get the celebrity ID from a call to the RecognizeCelebrities operation,
//!   which recognizes celebrities in an image.
//!
//! ## Output
//!
//! * `KnownGender` (KnownGender) — Retrieves the known gender for the celebrity.
//! * `Name` (String) — The name of the celebrity.
//! * `Urls` (Urls, len 0..255) — An array of URLs pointing to additional celebrity
//!   information.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 400) — You are not authorized to perform the action.
//! * `InternalServerError` (HTTP 500) — Amazon Rekognition experienced a service issue. Try
//!   your call again.
//! * `InvalidParameterException` (HTTP 400) — Input parameter violated a constraint. Validate
//!   your parameter before calling the API operation again.
//! * `ProvisionedThroughputExceededException` (HTTP 400) — The number of requests exceeded
//!   your throughput limit. If you want to increase this limit, contact Amazon Rekognition.
//! * `ResourceNotFoundException` (HTTP 400) — The resource specified in the request cannot be
//!   found.
//! * `ThrottlingException` (HTTP 500) — Amazon Rekognition is temporarily unable to process
//!   the request. Try your call again.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetCelebrityInfoError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-rekognition-rekognitionservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetCelebrityInfoError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.rekognition#GetCelebrityInfo")]
#[derive(Debug)]
pub enum GetCelebrityInfoErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerError(InternalServerError),
    InvalidParameterException(InvalidParameterException),
    ProvisionedThroughputExceededException(ProvisionedThroughputExceededException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_celebrity_info::GetCelebrityInfoError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetCelebrityInfoError = crate::scaffolded::Scaffolded<GetCelebrityInfoErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetCelebrityInfoErrorModeled> for GetCelebrityInfoError {
    fn from(modeled: GetCelebrityInfoErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetCelebrityInfo` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetCelebrityInfo<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetCelebrityInfo<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetCelebrityInfo<Eng>
{
    type Input = GetCelebrityInfoInput;
    type Output = GetCelebrityInfoOutput;
    type Error = GetCelebrityInfoError;

    const NAME: &'static str = "GetCelebrityInfo";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_CELEBRITY_INFO_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_CELEBRITY_INFO_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // Nothing about the request beyond the `Id` it names is read: the catalogue is this
        // service's own, the same for every caller.
        let _ = ctx;
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            GetCelebrityInfoErrorModeled::InternalServerError(
                crate::state::modeled_error::store_unavailable(),
            )
        })?;
        let store = state.lock();
        // The modelled `pattern ^[0-9A-Za-z]*$` is refused before this handler runs, so an
        // `Id` that reaches here is well formed and the only question left is whether this
        // service holds a celebrity under it.
        let Some(entry) = store.celebrities.by_id(&input.id) else {
            return Err(GetCelebrityInfoErrorModeled::ResourceNotFoundException(
                crate::state::modeled_error::celebrity_not_found(),
            )
            .into());
        };
        Ok(GetCelebrityInfoOutput {
            // "An array of URLs pointing to additional celebrity information. If there is no
            // additional information about the celebrity, this list is empty" — so the member
            // is answered as the list the entry carries, empty or not.
            urls: Some(entry.urls()),
            name: Some(entry.name().to_string()),
            known_gender: Some(entry.known_gender()),
        })
    }
}
