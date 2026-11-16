// SPDX-License-Identifier: CC0-1.0

//! The application user interfaces served ON a cluster — what
//! `GetOnClusterAppUIPresignedURL` answers for.
//!
//! One concern of this service's state. An on-cluster user interface is a web interface
//! an APPLICATION of the cluster serves from its primary node, so two facts about the
//! cluster decide what the operation can answer, and both are read off the cluster record
//! rather than off the request:
//!
//! * whether the cluster is up at all ([`serves_app_ui_in_state`]) — a cluster whose
//!   primary node is still coming up, or already gone, serves no interface, and the
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
/// An on-cluster interface is served from the cluster's primary node, so a cluster that
/// has not finished coming up and one that is shutting down or gone serve none: the
/// cluster is up exactly in the two states this service already reads as up for the other
/// surface the primary node answers ([`super::accepts_sessions_in_state`]).
pub fn serves_app_ui_in_state(state: &str) -> bool {
    state == cluster_state::RUNNING || state == cluster_state::WAITING
}

/// How a presigned-URL request for a cluster that is not up is refused.
///
/// AWS publishes no wording for it, so the text is built the way this service words its
/// other cluster-state refusals ([`super::cluster_not_running_for_session_message`]): the
/// cluster the request named, the state it is actually in, and the states that do serve an
/// interface.
pub fn cluster_not_running_for_app_ui_message(cluster_id: &str, state: &str) -> String {
    format!(
        "Cluster '{cluster_id}' is in state {state}; an application user interface is only \
         served by a cluster in the {} or {} state.",
        cluster_state::RUNNING,
        cluster_state::WAITING
    )
}

/// The refusal that request is answered with: that sentence under the client error the
/// operation models.
///
/// No `ErrorCode`: Amazon EMR publishes no discriminator for this class, the same way the
/// session surface's own state refusal carries none.
pub fn cluster_not_running_for_app_ui(cluster_id: &str, state: &str) -> InvalidRequestException {
    InvalidRequestException {
        error_code: None,
        message: Some(cluster_not_running_for_app_ui_message(cluster_id, state)),
    }
}

/// The address of the application user interface `ui_path` of the resource `host_id`, in
/// `region`, for the application `application_id` when the caller named one, carrying the
/// session token `session_token` the answering call minted for it.
///
/// The ONE composition of an application-UI address in this service: everything but the
/// token is derived from the resource the interface belongs to — its own id under the
/// regional Amazon EMR application-UI domain, with the interface as the path — so two
/// calls for one interface open the same one. The TOKEN is what makes the URL a presigned
/// one, and the answering call mints it per call rather than storing it, so no two calls
/// hand back the same credential.
pub fn app_ui_presigned_url(
    host_id: &str,
    region: &str,
    ui_path: &str,
    application_id: Option<&str>,
    session_token: &str,
) -> String {
    let mut url = format!(
        "https://{host_id}.emrappui-prod.{region}.amazonaws.com/{}/",
        ui_path.to_lowercase()
    );
    // The application id selects WHICH application's interface the URL opens, so it is
    // part of the path.
    if let Some(application_id) = application_id {
        url.push_str("history/");
        url.push_str(application_id);
        url.push('/');
    }
    url.push('?');
    url.push_str(PRESIGNED_URL_TOKEN_PARAMETER);
    url.push('=');
    url.push_str(session_token);
    url
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

    /// The address of this cluster's `ui_type` interface, composed by the one builder
    /// above from the cluster's own id.
    pub fn on_cluster_app_ui_url(
        &self,
        region: &str,
        ui_type: &str,
        application_id: Option<&str>,
        session_token: &str,
    ) -> String {
        app_ui_presigned_url(&self.id, region, ui_type, application_id, session_token)
    }
}
