// SPDX-License-Identifier: CC0-1.0

//! The application user interfaces served ON a cluster — what
//! `GetOnClusterAppUIPresignedURL` answers for.
//!
//! One concern of this service's state. An on-cluster user interface is a web interface
//! an APPLICATION of the cluster serves from its primary node, so two facts about the
//! cluster decide what the operation can answer, and both are read off the cluster record
//! rather than off the request:
//!
//! * whether the cluster has a primary node at all ([`serves_app_ui_in_state`]) — a cluster
//!   that is shutting down or gone has none, so there is no interface to address and the
//!   request is refused rather than answered with a URL that opens nothing;
//! * whether the interface the request names has an application to serve it
//!   ([`ClusterRecord::serves_on_cluster_app_ui`]), read off the applications the launch
//!   installed ([`super::installed_applications`]), which is what
//!   `GetOnClusterAppUIPresignedURLOutput$PresignedURLReady` reports.
//!
//! The URL itself is composed by [`app_ui_presigned_url`], the ONE builder of an
//! application-UI address in this service: the persistent user interface's own URL
//! ([`super::PersistentAppUIRecord::presigned_url`]) is built by it too, so the two
//! surfaces cannot come to address an interface differently.

use super::*;

/// The interface a presigned-URL request that names no `OnClusterAppUIType` is answered
/// for: the Spark history server, the interface a client asks a cluster for by default —
/// the same reading [`super::DEFAULT_PERSISTENT_APP_UI_TYPE`] makes of a persistent user
/// interface's own type.
pub const DEFAULT_ON_CLUSTER_APP_UI_TYPE: &str = "SparkHistoryServer";

/// Every `OnClusterAppUIType` the model declares, each with the APPLICATION that serves
/// that interface on the cluster.
///
/// SOURCE: the types are `smithy_query(query="shape", shape="OnClusterAppUIType")` —
/// `SparkHistoryServer | YarnTimelineService | TezUI | ApplicationMaster |
/// JobHistoryServer | ResourceManager`. Which application serves each is the Amazon EMR
/// Management Guide's "View web interfaces hosted on Amazon EMR clusters": the Spark
/// history server is Spark's, the Tez UI is Tez's, and the YARN timeline service,
/// resource manager, application master and MapReduce job-history interfaces are all
/// served by Hadoop.
pub const ON_CLUSTER_APP_UI_SERVERS: &[(&str, &str)] = &[
    ("SparkHistoryServer", "Spark"),
    ("TezUI", "Tez"),
    ("YarnTimelineService", "Hadoop"),
    ("ResourceManager", "Hadoop"),
    ("ApplicationMaster", "Hadoop"),
    ("JobHistoryServer", "Hadoop"),
];

/// The application that serves the on-cluster interface `ui_type`, or `None` for a type
/// no application of the catalog above serves.
///
/// The modelled enum constraint already refuses any value outside
/// [`ON_CLUSTER_APP_UI_SERVERS`] before a handler runs; `None` is what keeps this reading
/// total rather than guessing an application for an interface the model does not declare.
pub fn on_cluster_app_ui_server(ui_type: &str) -> Option<&'static str> {
    ON_CLUSTER_APP_UI_SERVERS
        .iter()
        .find(|(declared, _)| declared.eq_ignore_ascii_case(ui_type))
        .map(|(_, application)| *application)
}

/// Whether a cluster in `state` serves its applications' web interfaces at all.
///
/// An on-cluster interface belongs to the cluster's primary node, so an ACTIVE cluster
/// has one ([`cluster_state::ACTIVE`]) — a cluster that is still coming up is answered the
/// address of the interface it is coming up to serve — while a cluster that is shutting
/// down or gone has none at all and nothing to address.
///
/// So the gate is the CAPACITY the cluster still holds and not its being up: `STARTING`,
/// `BOOTSTRAPPING`, `RUNNING` and `WAITING` are each served, and `TERMINATING`,
/// `TERMINATED` and `TERMINATED_WITH_ERRORS` are each refused with
/// [`APP_UI_INACTIVE_CLUSTER_MESSAGE`], whose own words are what this reading follows.
pub fn serves_app_ui_in_state(state: &str) -> bool {
    cluster_state::ACTIVE.contains(&state)
}

/// What a presigned-URL request for a cluster that is no longer active is refused with.
///
/// Recorded behavior for the text: ONE fixed sentence, carrying no value of the request or
/// of the cluster — neither the cluster the request named nor the state it is in is part of
/// it, so every inactive cluster is refused with the same words. It travels under the error
/// code a rejected input on this operation carries
/// ([`crate::scaffolded::Scaffolded::validation`]), the one the other unusable targets of
/// the application-UI surface are refused through.
pub const APP_UI_INACTIVE_CLUSTER_MESSAGE: &str =
    "A presigned URL can only be created for an active cluster.";

/// How long a presigned application-UI URL this service hands out stays valid.
///
/// A CHOSEN bound: Amazon EMR publishes no validity for an application-UI presigned URL,
/// so this is long enough for a caller to open the interface it addresses and short enough
/// that a URL which was cached or shared stops working, well inside the longest validity
/// the emulator's signer accepts.
pub const APP_UI_PRESIGNED_URL_VALIDITY: Duration = Duration::from_secs(60 * 60);

/// The address of the application user interface `ui_path` of the resource `host_id`, for
/// the application `application_id` when the caller named one, carrying the session token
/// `session_token` the answering call minted for it.
///
/// The ONE composition of an application-UI address in this service, and it is PRESIGNED:
/// the URL is query-signed by this emulator's own signer for
/// [`APP_UI_PRESIGNED_URL_VALIDITY`], so a caller that follows it reaches the emulator that
/// answered, and the URL carries the instant it was signed and how long it stays valid —
/// which is what makes it stop working rather than open an interface for ever.
///
/// Everything but the token is derived from the resource the interface belongs to — its own
/// id, with the interface as the path — so two calls for one interface address the same
/// one. The TOKEN is minted per call by the answering call rather than stored, so no two
/// calls hand back the same credential.
///
/// `None` when the request cannot be signed, which the answering call reports as its
/// modelled server error rather than an address that verifies nowhere.
pub fn app_ui_presigned_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    host_id: &str,
    ui_path: &str,
    application_id: Option<&str>,
    session_token: &str,
) -> Option<String> {
    let mut path = format!("/{host_id}/{}/", ui_path.to_lowercase());
    // The application id selects WHICH application's interface the URL opens, so it is
    // part of the path.
    if let Some(application_id) = application_id {
        path.push_str("history/");
        path.push_str(application_id);
        path.push('/');
    }
    let request = ::smithy_server_runtime::SignedUrlRequest::new(
        ::http::Method::GET,
        ctx.identity().signing_name(),
        &path,
    )
    .query(PRESIGNED_URL_TOKEN_PARAMETER, session_token);
    match ctx.url_signer().sign(request, APP_UI_PRESIGNED_URL_VALIDITY) {
        Ok(url) => Some(url.as_str().to_string()),
        Err(error) => {
            ::tracing::warn!(
                error = ?error,
                "an application user interface URL could not be presigned"
            );
            None
        }
    }
}

impl ClusterRecord {
    /// Whether this cluster runs the application `name`.
    ///
    /// Read off the applications the launch installed — "a case-insensitive list of
    /// applications for Amazon EMR to install and configure when launching the cluster"
    /// (`RunJobFlow$Applications` doc) — and compared without regard to case for the same
    /// reason that list is.
    pub fn runs_application(&self, name: &str) -> bool {
        self.applications.iter().any(|application| {
            application
                .name
                .as_deref()
                .is_some_and(|installed| installed.eq_ignore_ascii_case(name))
        })
    }

    /// Whether this cluster serves the on-cluster interface `ui_type`: it runs the
    /// application that serves it ([`on_cluster_app_ui_server`]).
    ///
    /// A cluster whose launch installed no such application has no interface of that kind
    /// to open, which is what `PresignedURLReady` reports — the fact is the cluster's, not
    /// the request's.
    pub fn serves_on_cluster_app_ui(&self, ui_type: &str) -> bool {
        on_cluster_app_ui_server(ui_type).is_some_and(|server| self.runs_application(server))
    }

    /// The address of this cluster's `ui_type` interface, presigned by the one builder
    /// above from the cluster's own id.
    pub fn on_cluster_app_ui_url(
        &self,
        ctx: &::smithy_server_runtime::OperationContext<'_>,
        ui_type: &str,
        application_id: Option<&str>,
        session_token: &str,
    ) -> Option<String> {
        app_ui_presigned_url(ctx, &self.id, ui_type, application_id, session_token)
    }
}
