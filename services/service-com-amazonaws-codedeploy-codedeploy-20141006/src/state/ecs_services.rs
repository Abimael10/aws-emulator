// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECS services a deployment group TARGETS, resolved against the
//! service that owns them.
//!
//! `ecsServices` is "the target Amazon ECS services in the deployment group […] A
//! target Amazon ECS service is specified as an Amazon ECS cluster and service
//! name pair"
//! (`smithy_query(query="doc",shape="CreateDeploymentGroupInput",member="ecsServices")`),
//! so whether a group may name a pair at all is a question about a resource of
//! ANOTHER service: the cluster and the service belong to Amazon ECS, and that
//! service is the one answer to "does this pair name a service of the account".
//! CodeDeploy therefore LOADS the pair when the group names it, and a pair that
//! cannot be loaded is refused with `InvalidECSServiceException` (HTTP 400)
//! before anything is stored, so a group never points at nothing.
//!
//! Loading the pair answers more than whether the service exists: it also reports
//! HOW that service is deployed. A deployment group drives its target through
//! CodeDeploy, which an Amazon ECS service only accepts when it was created with
//! the `CODE_DEPLOY` deployment controller — "the deployment controller is the
//! mechanism that determines how tasks are deployed for your service"
//! (`smithy_query(query="shape",shape="DeploymentController")` on that service's
//! model, whose other types, `ECS` and `EXTERNAL`, are deployed by the Amazon ECS
//! scheduler and by a third party outside CodeDeploy). A pair naming a service
//! deployed by anything else is therefore refused under the same
//! `InvalidECSServiceException`, with the sentence that states that precondition
//! ([`CODE_DEPLOY_CONTROLLER_REQUIRED`]), and nothing is stored for it either.
//!
//! The load is made the way any emulated service reaches another — over Amazon
//! ECS's own modelled `DescribeServices` through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]) — so the pair is
//! resolved against the very records `CreateCluster`/`CreateService` wrote and
//! nothing is cached: a service created a moment ago resolves at once, and one the
//! owner has stopped reporting stops resolving at once. What the owner still
//! reports it still holds, tombstone included ("you can't delete a service that
//! has existing tasks" leaves a service in `DRAINING`, and a deleted one is
//! reported `INACTIVE` for a period), so such a pair LOADS: this gate answers the
//! owner's verdict on the pair, not a status reading of its own. Nothing leaves
//! the process: the load goes over the in-process callback, never a socket.
//!
//! ## What silence means
//!
//! The reading `services/common/src/network.rs` states for a gate on another
//! service's records applies here: a hop that could not be made at all, or that
//! came back with something that is not about this pair (a server failure, an
//! emulator that mounts no Amazon ECS at all), is evidence of NOTHING, so it
//! refuses nothing and says so at `warn`. The refusals that ARE served are the
//! ones the owner's own answer grounds: a CLIENT refusal of the read
//! ([`ECS_READ_REFUSALS`]), or a read that succeeded and reported no such
//! service.

use ::smithy_server_runtime::{EmbeddedEmulator, OperationContext};

use crate::ops::types::ECSService;

/// Amazon ECS's endpoint prefix and ARN namespace, which is what the in-process
/// address of the load is built from.
const ECS_ARN_VENDOR: &str = "ecs";

/// The SERVICE a relayed refusal names, as the AWS SDK whose exception CodeDeploy
/// reports names Amazon ECS. It is neither the endpoint prefix above nor the
/// casing of the service's `sdkId`.
const ECS_SERVICE_NAME: &str = "AmazonECS";

/// The CLIENT refusals Amazon ECS's `DescribeServices` models — `AccessDenied`,
/// `Client`, `ClusterNotFound` and `InvalidParameter`
/// (`smithy_query(query="operation",operation="DescribeServices")` on that
/// service's model, whose fifth error, `ServerException`, is the server one).
///
/// Each of them is the owner's answer that the pair the group named could not be
/// loaded, which is the fact this gate refuses on; a server failure says nothing
/// about the pair, so it is not here.
const ECS_READ_REFUSALS: &[&str] = &[
    "AccessDeniedException",
    "ClientException",
    "ClusterNotFoundException",
    "InvalidParameterException",
];

/// The refusal Amazon ECS answers a caller it has not authorised, which is the one
/// of [`ECS_READ_REFUSALS`] modelled at a status of its own.
const ECS_ACCESS_DENIED: &str = "AccessDeniedException";

/// The HTTP status a relayed refusal names when the answer that carried it reached
/// this service without one to read: the status the owner's model declares for that
/// refusal — 403 for [`ECS_ACCESS_DENIED`], 400 for every other member of
/// [`ECS_READ_REFUSALS`]
/// (`smithy_query(query="operation",operation="DescribeServices")`).
fn modelled_refusal_status(code: &str) -> u16 {
    if code == ECS_ACCESS_DENIED {
        403
    } else {
        400
    }
}

/// The header AWS reports the id of the request a response answers in, read off
/// the owner's own answer so a relayed sentence names the request IT refused.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How many DISTINCT pairs one call resolves, which is also how many hops it
/// costs.
///
/// `ECSServiceList` carries no modelled length
/// (`smithy_query(query="shape",shape="ECSServiceList")`), so without this the
/// work one call costs would be bounded only by how large a body the caller
/// sends. It is THIS EMULATOR's own bound on that work, not an AWS quota, and it
/// is far above the one pair an Amazon ECS deployment group is wired with.
pub const MAX_ECS_SERVICES_RESOLVED: usize = 10;

/// How CodeDeploy states a pair it could not load: the cluster and the service the
/// REQUEST named, then the cause the owner of those resources gave.
///
/// `cause` is the owner's own sentence — its refusal as the SDK that relayed the
/// load renders it ([`relayed_ecs_refusal`]), or its sentence for a service it
/// holds none of ([`SERVICE_NOT_FOUND_CAUSE`]) — so the caller reads WHY the pair
/// did not load rather than only that it did not.
pub fn could_not_load_message(cluster_name: &str, service_name: &str, cause: &str) -> String {
    format!(
        "Could not load ECS service information for cluster: {cluster_name}, service: \
         {service_name}. Cause: {cause}"
    )
}

/// The cause a pair gets when the load SUCCEEDED and reported no such service:
/// the sentence the OWNER answers a reference it holds no service for, which is
/// what it states as its own `ServiceNotFoundException` message — this text, not
/// the prose the model documents that error with.
///
/// It carries no relayed-call clause, unlike [`relayed_ecs_refusal`]: the read was
/// answered rather than refused, so there is no exception of the owner's to render
/// — the fact is the absence of the service in an answer that named it.
const SERVICE_NOT_FOUND_CAUSE: &str = "Service not found.";

/// The deployment controller type an Amazon ECS service a deployment group targets
/// must have been created with: the one that hands its deployments to CodeDeploy
/// (`DeploymentControllerType`, whose other values are `ECS` and `EXTERNAL`).
const CODE_DEPLOY_CONTROLLER: &str = "CODE_DEPLOY";

/// How CodeDeploy states the controller precondition on a pair it DID load: a
/// group may only drive a service that is deployed by CodeDeploy.
///
/// Entirely fixed text — it names neither the pair nor the controller the service
/// does have, because the precondition is a property of the deployment group, not
/// a reading of one request's values.
pub const CODE_DEPLOY_CONTROLLER_REQUIRED: &str =
    "Deployment group's ECS service must be configured for a CODE_DEPLOY deployment controller.";

/// The owner's refusal as the SDK that relayed the load renders it: that service's
/// own sentence, then the clause naming who refused, with what status, under which
/// error code, and the id of the request that was refused.
///
/// A pair is loaded by CALLING Amazon ECS, so a load it refuses is reported as
/// that exception reads — down to the trailing `Proxy: null`, there being no proxy
/// in front of the call. Every variable part comes from the owner's own answer.
fn relayed_ecs_refusal(message: &str, status: u16, code: &str, request_id: &str) -> String {
    format!(
        "{message} (Service: {ECS_SERVICE_NAME}; Status Code: {status}; Error Code: {code}; \
         Request ID: {request_id}; Proxy: null)"
    )
}

/// `None` when this emulator has no reason to refuse the group's `ecsServices`;
/// otherwise the `InvalidECSServiceException` message that refuses the call.
///
/// Every pair the request names is resolved against the service that owns it, in
/// the order named, and the FIRST one that could not be loaded, or that named a
/// service CodeDeploy does not deploy, is what the refusal states: that is the
/// pair the caller has to change. Deduplicated, so naming one pair twice costs one
/// load and is one target, and bounded by [`MAX_ECS_SERVICES_RESOLVED`] rather
/// than by the request. An entry that names no cluster or no service at all
/// describes no pair to load, so it is left to the members' own gates.
///
/// Call it from a handler with no store guard held — the load reaches Amazon ECS's
/// own handler, which takes that service's lock — and apply the verdict once the
/// resource the request addresses has been resolved, the way a group's service
/// role is applied ([`super::service_role::unassumable_role_message`]).
pub async fn ecs_services_refusal_message(
    ctx: &OperationContext<'_>,
    region: &str,
    ecs_services: &[ECSService],
) -> Option<String> {
    let asked = distinct_pairs(ecs_services);
    if asked.is_empty() {
        return None;
    }
    let client = ecs_client(ctx.callback(), region);
    for (cluster_name, service_name) in asked {
        match load(&client, cluster_name, service_name).await {
            Load::Loaded => {}
            Load::Unloadable(cause) => {
                return Some(could_not_load_message(cluster_name, service_name, &cause));
            }
            // The pair LOADED — the service is there — but it is deployed by
            // something other than CodeDeploy, so a group driving it could never
            // deploy it.
            Load::NotCodeDeployControlled => {
                return Some(CODE_DEPLOY_CONTROLLER_REQUIRED.to_string());
            }
            // One unanswered load ends the question: a partial answer would refuse
            // a pair for not being in a read nobody made.
            Load::Unanswered => return None,
        }
    }
    None
}

/// What the owner answered about ONE pair.
enum Load {
    /// The pair names a service the owner reports, deployed by CodeDeploy.
    Loaded,
    /// The pair could not be loaded, for this cause.
    Unloadable(String),
    /// The pair names a service the owner reports, but one whose deployment
    /// controller is not [`CODE_DEPLOY_CONTROLLER`].
    NotCodeDeployControlled,
    /// The owner could not be asked at all, or answered something that is not
    /// about this pair — evidence of nothing.
    Unanswered,
}

/// The `(clusterName, serviceName)` pairs one call resolves: the ones the request
/// names, in order, without repeats, each naming both members, at most
/// [`MAX_ECS_SERVICES_RESOLVED`] of them.
fn distinct_pairs(ecs_services: &[ECSService]) -> Vec<(&str, &str)> {
    let mut asked: Vec<(&str, &str)> = Vec::new();
    for entry in ecs_services {
        let cluster_name = entry.cluster_name.as_deref().unwrap_or_default().trim();
        let service_name = entry.service_name.as_deref().unwrap_or_default().trim();
        if cluster_name.is_empty() || service_name.is_empty() {
            continue;
        }
        if asked.contains(&(cluster_name, service_name)) {
            continue;
        }
        asked.push((cluster_name, service_name));
        if asked.len() == MAX_ECS_SERVICES_RESOLVED {
            // At `warn` for the same reason an unanswered load is: past this bound
            // a pair is not resolved at all, so a group may be stored naming a
            // target nothing was asked about, and that is worth seeing.
            ::tracing::warn!(
                named = ecs_services.len(),
                resolved = MAX_ECS_SERVICES_RESOLVED,
                "a deployment group names more Amazon ECS services than one call resolves, so \
                 the rest are not loaded"
            );
            break;
        }
    }
    asked
}

/// Load ONE pair through the owner's own `DescribeServices`.
///
/// The service is asked for BY NAME within the cluster the pair names, which is
/// the read whose answer grounds a refusal: the owner itself decides that a
/// cluster reference names nothing (it refuses the read) and that a name within a
/// cluster it holds names no service (it answers the read and reports no service
/// for that name), so this service relays the owner's verdict instead of wording a
/// fact it did not resolve. The same answer reports the service's deployment
/// controller, which is what decides whether a deployment group may drive it.
async fn load(client: &::aws_sdk_ecs::Client, cluster_name: &str, service_name: &str) -> Load {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    let error = match client
        .describe_services()
        .cluster(cluster_name)
        .services(service_name)
        .send()
        .await
    {
        Ok(output) => {
            // The read named one service, so the answer carries it when the
            // cluster holds it and reports a failure for it when it does not. The
            // owner reports a service's NAME and its ARN, and resolves a reference
            // written either way (`DescribeServices$services`: "a list of services
            // to describe. You may specify up to 10 services to describe in a
            // single operation", each by name or ARN), so the pair is loaded when
            // what it named is either of them.
            let Some(held) = output.services().iter().find(|held| {
                held.service_name() == Some(service_name) || held.service_arn() == Some(service_name)
            }) else {
                return Load::Unloadable(SERVICE_NOT_FOUND_CAUSE.to_string());
            };
            // And the controller the owner reports for it is the one fact that
            // decides whether CodeDeploy deploys this service at all. An answer
            // that reported the service but no controller for it states nothing
            // about how it is deployed, so — as everywhere in this gate — it
            // refuses nothing and says so at `warn`.
            let Some(controller) = held
                .deployment_controller()
                .map(|controller| controller.r#type().as_str())
            else {
                ::tracing::warn!(
                    cluster = ?cluster_name,
                    service = ?service_name,
                    "the Amazon ECS service a deployment group names reports no deployment \
                     controller, so the controller precondition is not resolved"
                );
                return Load::Loaded;
            };
            return if controller == CODE_DEPLOY_CONTROLLER {
                Load::Loaded
            } else {
                Load::NotCodeDeployControlled
            };
        }
        Err(error) => error,
    };
    let refused = error.as_service_error();
    let code = refused
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default();
    if !ECS_READ_REFUSALS.contains(&code) {
        // At `warn`: this gate refuses nothing it did not resolve, so a deployment
        // where the load cannot be made admits a group whose target the account may
        // not have, and that is worth seeing.
        // The two names come off the request, so they are recorded QUOTED rather
        // than interpolated: a value carrying a newline then reads as one field of
        // one record instead of forging a line of its own in a plain-text log.
        ::tracing::warn!(
            cluster = ?cluster_name,
            service = ?service_name,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the Amazon ECS service a deployment group names could not be loaded"
        );
        return Load::Unanswered;
    }
    // The owner worded this refusal itself, so the sentence it stated is the one
    // relayed; a refusal that carried none is still that refusal, and the code it
    // came under is what names the cause then.
    let message = refused
        .and_then(ProvideErrorMetadata::message)
        .map_or_else(|| format!("{code}."), str::to_string);
    let status = error.raw_response().map_or_else(
        || modelled_refusal_status(code),
        |response| response.status().as_u16(),
    );
    // The id of the refused request is the owner's own, read off the answer it
    // refused with; an answer that carried none is still a refusal, so the relay
    // names that call by an id of its own instead.
    let request_id = error
        .raw_response()
        .and_then(|response| response.headers().get(REQUEST_ID_HEADER))
        .map(str::to_string)
        .unwrap_or_else(::service_common::query_error_envelope::new_request_id);
    Load::Unloadable(relayed_ecs_refusal(&message, status, code, &request_id))
}

/// Amazon ECS's client for this load: the IN-PROCESS callback into this same
/// emulator, so the pair is resolved against the records that service's own
/// handlers wrote.
///
/// The `endpoint_url` is an ADDRESS the in-process router reads, never a host any
/// resolver looks up, and `region` is the one the request being served was signed
/// for, so a caller cannot steer that authority. There is nothing to retry, time
/// out, cache an identity for or watch for a stalled stream over that transport —
/// each would need a sleep implementation this client is not given — and the
/// credentials the callback hands over are the emulator's own, which only sign
/// this operation's in-process calls and are never put in a response or a log.
fn ecs_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_ecs::Client {
    let config = ::aws_sdk_ecs::Config::builder()
        .behavior_version(::aws_sdk_ecs::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_ecs::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{ECS_ARN_VENDOR}.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_ecs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ecs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ecs::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ecs::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_ecs::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::{could_not_load_message, distinct_pairs, MAX_ECS_SERVICES_RESOLVED};
    use crate::ops::types::ECSService;

    /// One `ecsServices` entry, as a request names it.
    fn entry(cluster_name: Option<&str>, service_name: Option<&str>) -> ECSService {
        ECSService {
            cluster_name: cluster_name.map(str::to_string),
            service_name: service_name.map(str::to_string),
        }
    }

    /// One row of the table below: the case, the entries a request names, and the
    /// pairs those entries resolve to.
    type Row = (&'static str, Vec<ECSService>, Vec<(&'static str, &'static str)>);

    /// Which pairs a request's list resolves to: both members named, repeats
    /// collapsed, order kept, bounded.
    #[test]
    fn the_pairs_resolved_are_the_distinct_fully_named_ones_in_order() {
        let many: Vec<ECSService> = (0..MAX_ECS_SERVICES_RESOLVED + 5)
            .map(|index| entry(Some("prod"), Some(&format!("web-{index}"))))
            .collect();
        let rows: Vec<Row> = vec![
            ("no entry at all", vec![], vec![]),
            (
                "one pair",
                vec![entry(Some("prod"), Some("web"))],
                vec![("prod", "web")],
            ),
            (
                "the same pair twice",
                vec![
                    entry(Some("prod"), Some("web")),
                    entry(Some("prod"), Some("web")),
                ],
                vec![("prod", "web")],
            ),
            (
                "two services of one cluster, in the order named",
                vec![
                    entry(Some("prod"), Some("web")),
                    entry(Some("prod"), Some("api")),
                ],
                vec![("prod", "web"), ("prod", "api")],
            ),
            (
                "one service name in two clusters",
                vec![
                    entry(Some("prod"), Some("web")),
                    entry(Some("staging"), Some("web")),
                ],
                vec![("prod", "web"), ("staging", "web")],
            ),
            (
                "an entry naming no service",
                vec![entry(Some("prod"), None)],
                vec![],
            ),
            (
                "an entry naming no cluster",
                vec![entry(None, Some("web"))],
                vec![],
            ),
            (
                "an entry whose members are blank",
                vec![entry(Some("  "), Some(""))],
                vec![],
            ),
            (
                "a pair written with surrounding space",
                vec![entry(Some(" prod "), Some(" web "))],
                vec![("prod", "web")],
            ),
        ];
        for (case, ecs_services, expected) in rows {
            let resolved = distinct_pairs(&ecs_services);
            assert_eq!(
                resolved, expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
        let bounded = distinct_pairs(&many);
        assert_eq!(
            bounded.len(),
            MAX_ECS_SERVICES_RESOLVED,
            "case \"more pairs than one call resolves\": expected the bound \
             {MAX_ECS_SERVICES_RESOLVED}, got {}",
            bounded.len()
        );
    }

    /// The refusal states the pair the REQUEST named and the cause the owner gave.
    #[test]
    fn the_refusal_states_the_requested_pair_and_the_cause() {
        let rows = [
            ("prod", "web", "Cluster not found."),
            ("ghost-cluster", "ghost-service", "Service not found."),
        ];
        for (cluster_name, service_name, cause) in rows {
            let message = could_not_load_message(cluster_name, service_name, cause);
            let expected = format!(
                "Could not load ECS service information for cluster: {cluster_name}, service: \
                 {service_name}. Cause: {cause}"
            );
            assert_eq!(
                message, expected,
                "case {cluster_name:?}/{service_name:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
