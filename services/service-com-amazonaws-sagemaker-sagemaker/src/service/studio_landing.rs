// SPDX-License-Identifier: CC0-1.0

//! Serves the STUDIO LANDING addresses a Studio domain publishes — its own `Url`, and the
//! `Url` of every space in it — at the addresses `CreateDomain` and `CreateSpace` minted for
//! them.
//!
//! A domain's `Url` is "the URL to the created domain" — an address whose whole point is
//! that the caller FOLLOWS it — so it has to lead back to THIS emulator: a Studio DNS name
//! of AWS's own (`d-<id>.studio.<region>.sagemaker.aws`) resolves to nothing a client of
//! the emulator can reach, so the Studio flow would fail with a name or network error
//! instead of landing anywhere. `CreateDomain` therefore addresses it on the endpoint the
//! request arrived on ([`crate::state::studio_domain_url`]) and this module serves it, and a
//! space's own address — derived from its domain's ([`crate::state::studio_space_url`]) — is
//! served the same way:
//!
//! * a `GET` of a live domain's landing address reports where that domain lands a user —
//!   its id, its name, the status it is in, how its members authenticate and the default
//!   experience its resolved settings name;
//! * a `GET` of a space's address reports that space under its domain;
//! * a domain or space this account does not hold is `404`, a domain whose resolved settings
//!   do not admit Studio at all is `403` ("If this value is set to `DISABLED`, the user
//!   cannot access Studio"), and a resource the service has not finished provisioning is
//!   `503`, because Studio is not serving for it yet.
//!
//! The model binds no operation to that path — a landing page is not an API call, it is
//! what the API's URL points at — so it is claimed through
//! [`smithy_server_runtime::Service::override_operation`] rather than through the generated
//! routing table, and it is served however it arrives ([`ErasedOperation::auth_schemes`]
//! answers an empty list): the address is opened by a browser, which signs nothing.
//!
//! What it answers is read off the stored domain, so a landing fetch and a `DescribeDomain`
//! or `DescribeSpace` of the same resource cannot disagree. The one write is the transition
//! the clock already owed that resource, which was due at its stored instant whether or not
//! anybody fetched the address.

use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use super::published_address::{
    answer, decoded_segments, FETCH_METHOD, JSON_MEDIA_TYPE, PLAIN_MEDIA_TYPE,
};

/// The method a landing address is opened on: a browser following the domain's `Url`.
const LANDING_METHOD: &str = FETCH_METHOD;

/// The media type the landing report is served in.
const LANDING_MEDIA_TYPE: &str = JSON_MEDIA_TYPE;

/// What a fetch addressing a domain this account does not hold is answered with. The
/// address is opened by a browser rather than by an AWS SDK, so the answer is the status
/// itself with a one-line reason and no API error envelope.
const NO_DOMAIN: &str = "no Studio domain is served at this address\n";

/// What a fetch addressing a space the domain does not hold is answered with.
const NO_SPACE: &str = "no Studio space is served at this address\n";

/// What a fetch of a domain whose resolved settings do not admit Studio is answered with.
const NO_STUDIO: &str = "this domain does not admit Studio\n";

/// The Smithy auth scheme an empty override is spelled with: see
/// [`ErasedOperation::auth_schemes`] below.
const ANONYMOUS: &[&str] = &[];

/// Serves the Studio landing addresses of this service.
///
/// Owns the shared engine, as an operation does, because what it serves is the domain the
/// create wrote: nothing is held beside those records, so a landing fetch and a describe
/// report the same domain.
pub struct StudioLandingRoute<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> StudioLandingRoute<Eng> {
    /// Build the route from the shared engine, as this service's operations are built.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// Whether this route serves `req`, which is what the service's `can_handle` and
    /// `override_operation` seams both ask.
    pub fn claims(&self, method: &str, uri: &str) -> bool {
        addressed(method, uri).is_some()
    }

    /// Serve one fetch of a landing address.
    fn serve(&self, req: &RtRequest) -> RtResponse {
        let Some(addressed) = addressed(req.method(), req.uri()) else {
            return answer(404, PLAIN_MEDIA_TYPE, NO_DOMAIN);
        };
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return answer(500, PLAIN_MEDIA_TYPE, NO_DOMAIN);
        };
        let now = std::time::SystemTime::now();
        let mut store = state.lock();
        // Resolved through the crate's one domain lookup, so a domain whose removal the
        // clock already completed is gone here exactly as it is for `DescribeDomain`.
        let Some(domain) = store.domain_mut(addressed.domain_id(), now) else {
            drop(store);
            return answer(404, PLAIN_MEDIA_TYPE, NO_DOMAIN);
        };
        // The only write: the transition the clock already owed this domain, which a
        // reader would have materialised too, so what this address reports and what the
        // control plane reports cannot disagree.
        domain.settle_status(now);
        let reported = Landing {
            domain_id: domain.id.clone(),
            domain_name: domain.name.clone(),
            status: domain.status.clone(),
            auth_mode: domain.auth_mode.clone(),
            landing_uri: domain.default_user_settings.default_landing_uri.clone(),
            studio_web_portal: domain.default_user_settings.studio_web_portal.clone(),
        };
        // A space is reached THROUGH its domain, so its own status is what decides whether
        // the space is serving; it is settled and read the same way, through the crate's one
        // space lookup.
        let space = match &addressed {
            Addressed::Domain(_) => None,
            Addressed::Space {
                domain_id,
                space_name,
            } => match store.space_mut(domain_id, space_name, now) {
                Some(space) => {
                    space.settle_status(now);
                    Some((space.name.clone(), space.status.clone()))
                }
                None => {
                    drop(store);
                    return answer(404, PLAIN_MEDIA_TYPE, NO_SPACE);
                }
            },
        };
        drop(store);

        // A domain whose resolved settings do not admit Studio serves no Studio to land
        // on, whatever the default experience of the domain is — and neither does anything
        // in it.
        if reported
            .studio_web_portal
            .as_deref()
            .is_some_and(|portal| portal != crate::state::DEFAULT_STUDIO_WEB_PORTAL)
        {
            return answer(403, PLAIN_MEDIA_TYPE, NO_STUDIO);
        }
        // Studio is reached once the service has finished provisioning the resource the
        // address names: before that — and for one whose provisioning failed or that is
        // being torn down — there is nothing serving at this address yet, and the status
        // says which.
        let (subject, status) = match &space {
            Some((_, space_status)) => ("space", space_status.clone()),
            None => ("domain", reported.status.clone()),
        };
        let serving = match &space {
            Some((_, space_status)) => space_status == crate::state::SPACE_STATUS_IN_SERVICE,
            None => reported.status == crate::state::DOMAIN_STATUS_IN_SERVICE,
        };
        if !serving {
            return answer(
                503,
                PLAIN_MEDIA_TYPE,
                &format!("this {subject} serves no Studio while it is in {status} status\n"),
            );
        }
        answer(200, LANDING_MEDIA_TYPE, &reported.rendered(space.as_ref()))
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for StudioLandingRoute<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        let answer = self.serve(req);
        Box::pin(async move { answer })
    }

    /// Who may reach this address: whoever holds the URL. A domain's `Url` is opened by a
    /// browser, which presents no SigV4 credential.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        Some(ANONYMOUS)
    }
}

/// Where one domain lands a user, as the stored domain holds it.
struct Landing {
    domain_id: String,
    domain_name: String,
    status: String,
    auth_mode: String,
    /// The default experience the domain's resolved settings name, as
    /// `DefaultUserSettings.DefaultLandingUri` holds it.
    landing_uri: Option<String>,
    /// Whether those settings admit Studio, as `DefaultUserSettings.StudioWebPortal`
    /// holds it.
    studio_web_portal: Option<String>,
}

impl Landing {
    /// The landing report, every member read off the stored domain — and off the stored
    /// space where the address named one.
    fn rendered(&self, space: Option<&(String, String)>) -> String {
        let mut reported = ::serde_json::json!({
            "DomainId": self.domain_id,
            "DomainName": self.domain_name,
            "Status": self.status,
            "AuthMode": self.auth_mode,
            "DefaultLandingUri": self.landing_uri,
            "StudioWebPortal": self.studio_web_portal,
        });
        if let Some((space_name, space_status)) = space {
            reported["SpaceName"] = ::serde_json::Value::from(space_name.as_str());
            reported["SpaceStatus"] = ::serde_json::Value::from(space_status.as_str());
        }
        reported.to_string()
    }
}

/// One landing address, as its path names it: a domain's own, or a space's under it.
#[derive(Debug, PartialEq, Eq)]
enum Addressed {
    /// The domain's own landing address, `/_aws/sagemaker/domain/<DomainId>/studio`.
    Domain(String),
    /// A space's address under it, `…/studio/space/<SpaceName>`.
    Space {
        domain_id: String,
        space_name: String,
    },
}

impl Addressed {
    /// The `DomainId` the address names, which both forms carry: a space is reached through
    /// its domain.
    fn domain_id(&self) -> &str {
        match self {
            Self::Domain(domain_id) => domain_id,
            Self::Space { domain_id, .. } => domain_id,
        }
    }
}

/// The landing address `uri` names on a `GET`, or `None` when the request is not one this
/// route serves.
///
/// The path is read the way the mint wrote it — its segments percent-decoded — so the
/// identifiers resolved here are the ones the records are held under.
fn addressed(method: &str, uri: &str) -> Option<Addressed> {
    if method != LANDING_METHOD {
        return None;
    }
    let segments = decoded_segments(uri);
    let (addressing, under) = segments.split_at_checked(5)?;
    let [namespace, plane, kind, domain_id, landing] = addressing else {
        return None;
    };
    if namespace != ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
        || plane != crate::state::PRESIGNED_SESSION_PLANE
        || kind != crate::state::DOMAIN_ARN_KIND
        || landing != crate::state::STUDIO_LANDING_SEGMENT
    {
        return None;
    }
    match under {
        [] => Some(Addressed::Domain(domain_id.clone())),
        [space, space_name] if space == crate::state::SPACE_PATH_SEGMENT => {
            Some(Addressed::Space {
                domain_id: domain_id.clone(),
                space_name: space_name.clone(),
            })
        }
        _ => None,
    }
}
