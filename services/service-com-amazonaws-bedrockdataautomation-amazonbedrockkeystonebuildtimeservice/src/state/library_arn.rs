// SPDX-License-Identifier: CC0-1.0

//! Which `DataAutomationLibraryArn` this service ADMITS: the check every operation that
//! resolves one takes on its input, before it looks the library up.
//!
//! RULE. A `libraryArn` addresses a library of the endpoint the request reached, so an ARN
//! that is well formed (the modelled `DataAutomationLibraryArn` `@pattern` admits it) but
//! names a Region OTHER than the one the request was signed for addresses nothing this
//! endpoint can serve. That is a fact about the INPUT rather than about the store, so it is
//! refused with the modelled `ValidationException` (HTTP 400,
//! `smithy_query(query="operation",operation="GetDataAutomationLibrary")` lists it beside
//! `ResourceNotFoundException` at 404) and [`WRONG_REGION_MESSAGE`], decided BEFORE the
//! lookup — never answered as a resource that happens to be missing, which a client reads as
//! "create it, or treat it as deleted" and retries.
//!
//! The check is on the REGION alone. The partition and the account field are read where this
//! service already reads them (`super::names_account_owned_resource` tells an ARN of this
//! account's resources from an AWS-owned one), and the resource id is the modelled pattern's
//! business; nothing else about the value is re-judged here.
//!
//! It covers the `libraryArn` of every operation that resolves one — the library's own read,
//! update and delete, and the ingestion-job surface underneath it
//! (`/data-automation-libraries/{libraryArn}/…`) — so one input member has one admission
//! rule wherever it is bound. The other resource families of this service each word their
//! own refusal for an ARN that resolves to nothing
//! ([`super::BLUEPRINT_ARN_NOT_FOUND_MESSAGE`], [`super::PROJECT_ARN_NOT_FOUND_MESSAGE`],
//! the tagging surface's `<Kind> <resourceARN> does not exist`), each as its own recording
//! words it, so none of them is reworded from here.

use crate::ops::types::ValidationException;

/// What this service answers for a `libraryArn` naming another Region.
///
/// RECORDED behaviour, and entirely FIXED text: the sentence names no member, interpolates
/// neither the ARN nor either Region, and reads nothing from state — so it is spelled here
/// once and never formatted.
pub const WRONG_REGION_MESSAGE: &str = "The provided ARN is invalid for the service region";

/// The Region label a value is compared under: the grammar every ARN this service MINTS
/// carries in that field (`super::mint_data_automation_library_arn` reduces the request's
/// Region through the same shared reader), so an ARN this service handed out is always
/// admitted by the ARN it was minted for.
fn region_label(region: &str) -> &str {
    ::service_common::arn::region_label(region).unwrap_or(super::DEFAULT_REGION)
}

/// Whether `library_arn` names a library of a Region OTHER than the one `ctx`'s request was
/// signed for.
///
/// `false` for a value that is no ARN of this service's namespace at all: such a value is
/// the modelled `@pattern`'s business, and the framework's constraint walk has already
/// refused it before any handler ran.
#[must_use]
pub fn names_another_region(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    library_arn: &str,
) -> bool {
    let Some(parsed) = ::service_common::arn::parse_of(library_arn, super::ARN_SERVICE) else {
        return false;
    };
    parsed.region != region_label(&super::request_region(ctx))
}

/// The refusal an operation answers for a `libraryArn` this endpoint does not admit, or
/// `None` for one it does.
///
/// Returned as the modelled shape rather than as a response, so each operation lifts it into
/// its OWN error channel — the channel is per operation, the rule and the words are not.
/// The refusal carries no `fieldList`: the sentence names no member, so an entry invented
/// here would report one the service does not.
#[must_use]
pub fn region_refusal(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    library_arn: &str,
) -> Option<ValidationException> {
    names_another_region(ctx, library_arn).then(|| ValidationException {
        message: Some(WRONG_REGION_MESSAGE.to_string()),
        field_list: None,
    })
}
