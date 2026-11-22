// SPDX-License-Identifier: CC0-1.0

//! @op ListOutgoingCertificates
//!
//! Lists the certificates this account has transferred out and the recipient has not
//! accepted yet.
//!
//! A row of this listing is exactly a certificate record carrying a pending transfer, so
//! the listing is a pure read of the certificate store filtered to those records and
//! projected by [`crate::state::RegisteredCertificate::outgoing_certificate`] — the same
//! projection every reader of a transfer uses, so what this listing reports and what
//! `DescribeCertificate` reports cannot drift. The order is the creation date in the
//! direction `ascendingOrder` names, walked by the shared pager every `@paginated` listing
//! of this service uses.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `ListOutgoingCertificates`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json")
//! ```
//!
//! Lists certificates that are being transferred but not yet accepted. Requires permission
//! to access the ListOutgoingCertificates action.
//!
//! ## Input
//!
//! * `ascendingOrder` (AscendingOrder, default false) **required** — Specifies the order for
//!   results. If True, the results are returned in ascending order, based on the creation
//!   date.
//! * `marker` (Marker, len 0..1024, pattern ^[A-Za-z0-9+/]+={0,2}$) — The marker for the next
//!   set of results.
//! * `pageSize` (PageSize, range 1..250) — The result page size.
//!
//! ## Output
//!
//! * `nextMarker` (Marker, len 0..1024, pattern ^[A-Za-z0-9+/]+={0,2}$) — The marker for the
//!   next set of results.
//! * `outgoingCertificates` (OutgoingCertificates) — The certificates that are being
//!   transferred but not yet accepted.
//!
//! ## Errors
//!
//! * `InternalFailureException` (HTTP 500) — An unexpected error has occurred.
//! * `InvalidRequestException` (HTTP 400) — The request is not valid.
//! * `ServiceUnavailableException` (HTTP 503) — The service is temporarily unavailable.
//! * `ThrottlingException` (HTTP 400) — The rate exceeds the limit.
//! * `UnauthorizedException` (HTTP 401) — You are not authorized to perform this operation.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListOutgoingCertificatesError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iot-awsiotservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListOutgoingCertificatesError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iot#ListOutgoingCertificates")]
#[derive(Debug)]
pub enum ListOutgoingCertificatesErrorModeled {
    InternalFailureException(InternalFailureException),
    InvalidRequestException(InvalidRequestException),
    ServiceUnavailableException(ServiceUnavailableException),
    ThrottlingException(ThrottlingException),
    UnauthorizedException(UnauthorizedException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_outgoing_certificates::ListOutgoingCertificatesError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListOutgoingCertificatesError = crate::scaffolded::Scaffolded<ListOutgoingCertificatesErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListOutgoingCertificatesErrorModeled> for ListOutgoingCertificatesError {
    fn from(modeled: ListOutgoingCertificatesErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListOutgoingCertificates` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListOutgoingCertificates<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListOutgoingCertificates<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListOutgoingCertificates<Eng>
{
    type Input = ListOutgoingCertificatesInput;
    type Output = ListOutgoingCertificatesOutput;
    type Error = ListOutgoingCertificatesError;

    const NAME: &'static str = "ListOutgoingCertificates";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_OUTGOING_CERTIFICATES_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_OUTGOING_CERTIFICATES_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(ListOutgoingCertificatesErrorModeled::InternalFailureException(
                InternalFailureException {
                    message: Some("the IoT emulator has no state store attached".to_string()),
                },
            )
            .into());
        };
        let _ = ctx;

        // A pure read of the certificate store: a row is a certificate the account has
        // transferred out and the recipient has not accepted yet, which is a record
        // carrying a pending transfer, so nothing here writes.
        let store = state.lock();
        let outgoing = store
            .certificates
            .values()
            .filter(|certificate| certificate.transfer.is_some());
        // The listing's order is the creation date, in the direction `ascendingOrder`
        // names — "If True, the results are returned in ascending order, based on the
        // creation date" — as the rows the shared pager walks
        // ([`crate::state::certificate_listing_rows`]), the same order every certificate
        // listing of this service runs in.
        let rows = crate::state::certificate_listing_rows(outgoing, input.ascending_order);

        // One page of that order, resumed after the position the caller's own marker names
        // ([`crate::state::page_of`]) — the same paging every other `@paginated` listing of
        // this service uses, so a token is read one way. The scope names THIS listing and
        // carries the direction, so a marker another listing or the other direction issued
        // does not resume this one from a position that means something else here.
        let scope = format!(
            "outgoing-certificates/{}",
            if input.ascending_order { "a" } else { "d" }
        );
        let resume_after = match input.marker.as_deref() {
            None => None,
            Some(marker) => {
                let Some(resume_after) = crate::state::decode_page_marker(marker, &scope) else {
                    return Err(ListOutgoingCertificatesErrorModeled::InvalidRequestException(
                        InvalidRequestException {
                            message: Some("marker is not a marker this listing issued".to_string()),
                        },
                    )
                    .into());
                };
                Some(resume_after)
            }
        };
        // A request naming no `pageSize` gets the page size a certificate listing of this
        // service answers with ([`crate::state::DEFAULT_CERTIFICATE_PAGE_SIZE`]). The clamp
        // keeps a named one inside the modelled `PageSize` range rather than depending on a
        // validation performed outside this module.
        let page_size =
            input
                .page_size
                .map_or(crate::state::DEFAULT_CERTIFICATE_PAGE_SIZE, |page_size| {
                    page_size.clamp(1, crate::state::MAX_LIST_PAGE_SIZE)
                }) as usize;
        let (page, next_marker) =
            crate::state::page_of(rows, &scope, resume_after.as_deref(), page_size);

        // Each entry is projected from the stored record and its stored transfer, so what
        // this listing reports and what a reader of one certificate reports cannot drift.
        Ok(ListOutgoingCertificatesOutput {
            outgoing_certificates: Some(
                page.iter()
                    .filter_map(|row| crate::state::certificate_listing_row_id(row))
                    .filter_map(|certificate_id| store.certificates.get(certificate_id))
                    .filter_map(crate::state::RegisteredCertificate::outgoing_certificate)
                    .collect(),
            ),
            next_marker,
        })
    }
}
