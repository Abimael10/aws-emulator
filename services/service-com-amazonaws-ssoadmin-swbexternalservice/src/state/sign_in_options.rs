// SPDX-License-Identifier: CC0-1.0

//! The ONE validation of the portal `SignInOptions` a write carries, judged against the
//! application provider the application federates through.
//!
//! # The rules
//!
//! 1. `ApplicationUrl` is "a required parameter if the `Origin` parameter is
//!    `APPLICATION`" (`SignInOptions$ApplicationUrl`'s own documentation), so that origin
//!    without a URL is refused with the modelled `ValidationException` (HTTP 400);
//! 2. `Origin` `IDENTITY_CENTER` is the origin where "IAM Identity Center uses SAML
//!    identity-provider initiated authentication to sign the customer directly into a
//!    SAML-based application" (`SignInOptions$Origin`'s own documentation), so it is a
//!    configuration only an application whose provider federates over SAML 2.0 can carry.
//!    An application whose provider federates over OAuth 2.0 — an OIDC application — has
//!    no SAML sign-in to be navigated to, so that combination is refused with the same
//!    modelled `ValidationException` rather than stored.
//!
//! Neither rule is expressible in the model: `SignInOptions`
//! (`smithy_query(query="shape",shape="SignInOptions,SignInOrigin")`) declares
//! `ApplicationUrl` as an ordinary optional member and `Origin` as the plain enum
//! `IDENTITY_CENTER | APPLICATION`, with nothing tying either to the other member or to
//! the provider, so the boundary validator passes such a request and the handler is where
//! it is refused.
//!
//! WHERE IT IS ASKED: by both writes of this service that carry portal options —
//! `CreateApplication`, against the provider its `ApplicationProviderArn` names, and
//! `UpdateApplication`, against the provider the stored application was created under — so
//! one set of sign-in options is judged the same way whichever operation the client
//! called. Judged BEFORE anything is written, so a refused create mints no application and
//! a refused update leaves the stored options exactly as they were.

use crate::ops::types::SignInOptions;

use super::{BuiltInApplicationProvider, FEDERATION_OAUTH};

/// `SignInOrigin::APPLICATION` — the origin that redirects the user to the application's
/// own URL, which is why `SignInOptions$ApplicationUrl` is required with it.
pub const SIGN_IN_ORIGIN_APPLICATION: &str = "APPLICATION";

/// `SignInOrigin::IDENTITY_CENTER` — the origin that signs the user in through SAML
/// identity-provider initiated authentication, which only a SAML-based application has.
pub const SIGN_IN_ORIGIN_IDENTITY_CENTER: &str = "IDENTITY_CENTER";

/// The refusal a `SignInOptions` with `Origin` `APPLICATION` and no `ApplicationUrl`
/// earns, as the message of the modelled `ValidationException`.
///
/// The rule is rule 1 of this module: the URL the origin redirects to has to be there.
pub const APPLICATION_URL_REQUIRED_MESSAGE: &str =
    "PortalOptions.SignInOptions.ApplicationUrl is required when \
     PortalOptions.SignInOptions.Origin is APPLICATION.";

/// The refusal a `SignInOptions` with `Origin` `IDENTITY_CENTER` earns on an application
/// whose provider federates over OAuth 2.0, as the message of the modelled
/// `ValidationException`.
///
/// The whole sentence is FIXED text: the refusal states the one illegal combination and
/// carries nothing of the request or of the store, so there is nothing in it to build.
pub const IDENTITY_CENTER_ORIGIN_UNSUPPORTED_MESSAGE: &str =
    "SignInOrigin cannot be IDENTITY_CENTER for an OIDC application";

/// Whether an application of `provider` is an OIDC one — one whose provider federates over
/// OAuth 2.0 rather than SAML 2.0, and which therefore has no SAML identity-provider
/// initiated sign-in for `IDENTITY_CENTER` to navigate to.
#[must_use]
pub fn federates_over_oauth(provider: &BuiltInApplicationProvider) -> bool {
    provider.federation_protocol == FEDERATION_OAUTH
}

/// The refusal the `SignInOptions` of a write earns, or `None` when they are options the
/// service serves.
///
/// `options` is the sign-in options the REQUEST carries, absent when it carries none (a
/// write that configures no sign-in earns neither refusal), and `provider` the application
/// provider the application federates through — `None` when it resolves to no published
/// provider, which leaves rule 2 unanswerable, so only rule 1 is asked.
#[must_use]
pub fn sign_in_options_refusal(
    options: Option<&SignInOptions>,
    provider: Option<&BuiltInApplicationProvider>,
) -> Option<String> {
    let options = options?;
    if options.origin == SIGN_IN_ORIGIN_APPLICATION
        && options
            .application_url
            .as_ref()
            .is_none_or(|url| url.is_empty())
    {
        return Some(APPLICATION_URL_REQUIRED_MESSAGE.to_string());
    }
    if options.origin == SIGN_IN_ORIGIN_IDENTITY_CENTER
        && provider.is_some_and(federates_over_oauth)
    {
        return Some(IDENTITY_CENTER_ORIGIN_UNSUPPORTED_MESSAGE.to_string());
    }
    None
}
