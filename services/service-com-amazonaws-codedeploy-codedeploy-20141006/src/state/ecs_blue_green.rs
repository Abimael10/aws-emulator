// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECS BLUE/GREEN deployment: the replacement task set CodeDeploy installs in
//! the service, and the production traffic it then reroutes onto it.
//!
//! # The rule this module implements
//!
//! `CreateDeployment`'s own account of this deployment controller is what it does:
//! "CodeDeploy installs an updated version of the application as a new replacement task set
//! and reroutes production traffic from the original application task set to the replacement
//! task set. The original task set is terminated after a successful deployment"
//! (`smithy_query(query="doc",shape="CreateService")` on Amazon ECS's model states the same
//! for the controller). So a deployment of the `ECS` compute platform performs three writes,
//! none of them in CodeDeploy's own store:
//!
//! 1. the REPLACEMENT TASK SET is created in the Amazon ECS service the group targets
//!    (`ecsServices`: "A target Amazon ECS service is specified as an Amazon ECS cluster and
//!    service name pair"), running the revision its AppSpec declares
//!    ([`super::appspec::EcsTaskSetDeclaration`]) and bound to the OTHER target group of the
//!    group's pair ("One pair of target groups. One is associated with the original task
//!    set. The second is associated with the task set that serves traffic after the
//!    deployment is complete");
//! 2. once that task set has reached a steady state, the PRODUCTION TRAFFIC ROUTE is
//!    rewritten: `prodTrafficRoute` is "The path used by a load balancer to route production
//!    traffic when an Amazon ECS deployment is complete", so the listeners it names forward
//!    to the replacement group — in the increments the deployment's own
//!    `trafficRoutingConfig` describes ([`super::traffic_shift`]), which for a canary or a
//!    linear configuration is a weighted forward to both groups until the last of them, and
//!    the replacement task set is promoted to `PRIMARY` by the increment that completes the
//!    move;
//! 3. the ORIGINAL task set is deleted once the deployment's own termination wait has
//!    elapsed.
//!
//! # Where each write lands
//!
//! The task sets belong to Amazon ECS and the listeners to Elastic Load Balancing v2, so
//! each write is made in the store of the service that OWNS the resource, through the shared
//! engine — the same records `CreateTaskSet`, `DescribeTaskSets` and `DescribeListeners`
//! read — exactly as a Lambda deployment's alias move is written in the Lambda store
//! ([`super::lambda_alias`]). Nothing about a task set or a listener is copied into this
//! service's store: one resource has one home, and a client reads the shift where a client
//! reads the listener.
//!
//! Called with THIS store unlocked ([`apply_due_steps`], a stage of [`super::with_state`]),
//! so no two of the three stores are ever held at once, and before the clock may give the
//! deployment a successful outcome: a deployment that moved no traffic never reports success
//! for one.

use std::time::SystemTime;

use ::service_com_amazonaws_ecs_amazonec2containerservicev20141113 as ecs;
use ::service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10 as elbv2;

use crate::ops::types::{ErrorInformation, LoadBalancerInfo};

use super::appspec::EcsTaskSetDeclaration;
use super::lambda_traffic_shift::TrafficShiftProgress;
use super::traffic_shift::{self, Increment};
use super::{
    deployment_is_terminal, service_state, ComAmazonawsCodedeployCodedeploy20141006State,
    Deployment, DeploymentGroup, DEPLOYMENT_STATUS_FAILED, DEPLOYMENT_STATUS_IN_PROGRESS,
};

pub use super::appspec::COMPUTE_PLATFORM_ECS;

/// The `ErrorCode` a deployment fails with when the Amazon ECS service the group targets is
/// not one this account holds a task set can be created in
/// (`smithy_query(query="shape",shape="ErrorCode")` carries it).
pub const ERROR_CODE_INVALID_ECS_SERVICE: &str = "INVALID_ECS_SERVICE";

/// The `ErrorCode` a deployment fails with when the replacement task set could not be
/// created in a service that IS there, or the traffic route it names could not be rewritten;
/// see [`ERROR_CODE_INVALID_ECS_SERVICE`].
pub const ERROR_CODE_ECS_UPDATE_ERROR: &str = "ECS_UPDATE_ERROR";

/// The action type a traffic route forwards a request on.
const FORWARD_ACTION: &str = "forward";

/// The whole of a traffic route's weight, which the share of it on each group is taken out
/// of: an ELBv2 forward action weights its target groups, and
/// `TargetGroupTuple$Weight`'s range is 0..=999 with the SHARE each group takes being its
/// weight over the total — so two groups weighted to a percentage and its complement split
/// the route in exactly that proportion.
const WHOLE_ROUTE_WEIGHT: i32 = 100;

/// What one Amazon ECS deployment deploys, resolved ONCE when the deployment is recorded.
///
/// Everything the three writes need, and nothing that is read again later: the service the
/// task set belongs to, what the replacement runs, the pair of target groups the traffic
/// moves between, and the listeners that carry it. A deployment therefore performs the
/// deployment it was CREATED for, even if the group is retargeted afterwards.
#[derive(Debug, Clone)]
pub struct EcsDeployment {
    /// The Amazon ECS cluster and service name pair the group targets.
    pub cluster_name: String,
    /// See [`Self::cluster_name`].
    pub service_name: String,
    /// What the replacement task set runs, as the revision's AppSpec declares it.
    pub declaration: EcsTaskSetDeclaration,
    /// The NAMES of the group's one pair of target groups: the original task set's and the
    /// replacement's, in the order the group names them.
    pub target_group_names: Vec<String>,
    /// The listeners the production traffic route names.
    pub prod_listener_arns: Vec<String>,
}

/// How many pairs, target groups and listeners one deployment reads out of its group.
///
/// `targetGroupPairInfoList` is one pair ("One pair of target groups") and `listenerArns`
/// "is an array of strings with a maximum size of one" for the production route, so these
/// bounds sit at or just above what the members describe: the work one deployment costs is
/// fixed by this service rather than by how large a group a caller stored.
const MAX_TARGET_GROUPS: usize = 4;
/// See [`MAX_TARGET_GROUPS`].
const MAX_LISTENERS: usize = 4;

/// The Amazon ECS deployment `group` and `revision` describe, or `None` when they describe
/// none.
///
/// Read where the deployment is recorded ([`crate::ops::create_deployment`]), so a
/// deployment of another compute platform, one whose group targets no Amazon ECS service,
/// and one whose revision declares no replacement task set each carry none and perform
/// nothing here.
#[must_use]
pub fn declared(
    compute_platform: &str,
    group: &DeploymentGroup,
    declaration: Option<EcsTaskSetDeclaration>,
) -> Option<EcsDeployment> {
    if compute_platform != COMPUTE_PLATFORM_ECS {
        return None;
    }
    let declaration = declaration?;
    let target = group.ecs_services.first()?;
    let cluster_name = target.cluster_name.clone()?.trim().to_string();
    let service_name = target.service_name.clone()?.trim().to_string();
    if cluster_name.is_empty() || service_name.is_empty() {
        return None;
    }
    let pair = group
        .load_balancer_info
        .as_ref()
        .and_then(|info: &LoadBalancerInfo| info.target_group_pair_info_list.as_ref())
        .and_then(|pairs| pairs.first())?;
    let target_group_names: Vec<String> = pair
        .target_groups
        .iter()
        .flatten()
        .filter_map(|group| group.name.clone())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .take(MAX_TARGET_GROUPS)
        .collect();
    let prod_listener_arns: Vec<String> = pair
        .prod_traffic_route
        .iter()
        .flat_map(|route| route.listener_arns.iter().flatten())
        .map(|arn| arn.trim().to_string())
        .filter(|arn| !arn.is_empty())
        .take(MAX_LISTENERS)
        .collect();
    Some(EcsDeployment {
        cluster_name,
        service_name,
        declaration,
        target_group_names,
        prod_listener_arns,
    })
}

/// Whether `deployment` still owes work in Amazon ECS or Elastic Load Balancing v2, and so
/// may not be given its terminal status yet.
///
/// The counterpart of the gate a Lambda deployment's own shift is held to
/// ([`super::lambda_traffic_shift`]): a deployment reports a successful outcome only once
/// the replacement task set serves the production traffic and the original one has been
/// taken away.
#[must_use]
pub fn blocks_completion(deployment: &Deployment) -> bool {
    if deployment.ecs_blue_green.is_none() {
        return false;
    }
    match deployment.traffic_shift {
        // A deployment whose steps could not be carried out owes nothing further: it is
        // failed where the refusal is recorded, and the task sets stay as that refusal
        // found them.
        TrafficShiftProgress::Refused => false,
        // One that has moved the traffic still owes the original task set's deletion.
        TrafficShiftProgress::Applied => deployment.ecs_original_task_set.is_some(),
        // And one that has not moved it yet owes the move.
        TrafficShiftProgress::Declared | TrafficShiftProgress::InFlight => true,
    }
}

/// One step claimed by a pass, with everything the write needs.
#[derive(Debug, Clone)]
struct Claim {
    /// The deployment the step belongs to.
    deployment_id: String,
    /// What that deployment deploys.
    ecs: EcsDeployment,
    /// The Region its writes are addressed in, which is the Region it was created in.
    region: String,
    /// The replacement task set it has already created, when it has.
    replacement_task_set: Option<String>,
    /// The two target groups it decided to move the traffic between when it installed that
    /// task set ([`resolve_traffic_route`]), so every increment moves it the same way.
    original_target_group: Option<String>,
    /// See [`Self::original_target_group`].
    replacement_target_group: Option<String>,
    /// The original task set it is replacing, until that one has been taken away.
    original_task_set: Option<String>,
    /// The traffic increment the clock owes it now, and that increment's position in the
    /// schedule.
    due_increment: Option<(usize, Increment)>,
    /// Whether the last increment of the schedule is the one that is due.
    completes_the_move: bool,
    /// Whether the original task set's own deletion is due.
    terminates_the_original: bool,
}

/// What one claimed step came to.
#[derive(Debug, Clone)]
enum Step {
    /// Nothing was owed after all — the replacement task set has not reached a steady state
    /// yet, so the traffic stays where it is.
    Waiting,
    /// The replacement task set was created, under this id, replacing the task set that was
    /// serving production traffic (when the service had one), and moving the traffic between
    /// these two target groups.
    Installed {
        replacement: String,
        original: Option<String>,
        original_target_group: Option<String>,
        replacement_target_group: String,
    },
    /// The increment at this position was carried out on the production traffic route.
    Shifted(usize),
    /// The original task set was deleted.
    Terminated,
    /// The step could not be carried out, for this reason.
    Refused(Denial),
}

/// Why a step could not be carried out, as CodeDeploy reports it on
/// `DeploymentInfo.errorInformation`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Denial {
    /// An `ErrorCode`; see [`ERROR_CODE_INVALID_ECS_SERVICE`].
    code: &'static str,
    /// The accompanying message, naming the resource the deployment could not act on.
    message: String,
}

impl Denial {
    /// The denial for a cluster and service pair no task set could be created in.
    fn invalid_service(ecs: &EcsDeployment) -> Self {
        Self {
            code: ERROR_CODE_INVALID_ECS_SERVICE,
            message: format!(
                "The deployment failed because the ECS service {} of cluster {} could not be \
                 deployed to",
                ecs.service_name, ecs.cluster_name
            ),
        }
    }

    /// The denial for a replacement task set that could not be created in a service that is
    /// there — the revision it would run, or the target group it would register into, is not
    /// one this account holds.
    fn update_error(ecs: &EcsDeployment) -> Self {
        Self {
            code: ERROR_CODE_ECS_UPDATE_ERROR,
            message: format!(
                "The deployment failed because a replacement task set running {} could not be \
                 created in the ECS service {}",
                ecs.declaration.task_definition, ecs.service_name
            ),
        }
    }

    /// The denial for a production traffic route that could not be rewritten: the listeners
    /// the group names, or the pair of target groups the traffic moves between, are not ones
    /// Elastic Load Balancing v2 holds.
    fn traffic_route(ecs: &EcsDeployment) -> Self {
        Self {
            code: ERROR_CODE_ECS_UPDATE_ERROR,
            message: format!(
                "The deployment failed because the production traffic route of the ECS service \
                 {} could not be moved to the replacement target group",
                ecs.service_name
            ),
        }
    }
}

/// Carry out the Amazon ECS deployment step every deployment the clock owes one at this
/// instant owes, in the stores of the services that own the resources.
///
/// A store holding no Amazon ECS deployment in flight costs one read of this store and takes
/// no other service's lock at all.
///
/// `now_ms` is the instant of the whole pass this is one stage of, for the reason
/// [`super::lambda_traffic_shift::apply_due_shifts`] takes one: an increment is carried out
/// only for a deployment the earlier stages of the same pass found still running.
pub fn apply_due_steps<Eng: Send + Sync + 'static>(engine: &Eng, now_ms: i64) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let claimed = {
        let mut store = state.lock();
        claim_due_steps(&mut store, now_ms)
    };
    if claimed.is_empty() {
        return;
    }
    // The other services' stores are reached with THIS store unlocked, and what each
    // attempt came to is written back under it.
    let attempted: Vec<(Claim, Step)> = claimed
        .into_iter()
        .map(|claim| {
            let step = carry_out(engine, &claim);
            (claim, step)
        })
        .collect();
    let mut store = state.lock();
    for (claim, step) in &attempted {
        record(&mut store, claim, step, now_ms);
    }
}

/// The step every Amazon ECS deployment that is owed one at `now_ms` owes, CLAIMED as this
/// pass's to carry out: the deployment's shift is marked
/// [`TrafficShiftProgress::InFlight`] before any other store is touched, so two callers
/// racing through here cannot create two replacement task sets or move one listener twice,
/// and neither can settle the deployment until the pass that claimed it has written back
/// what the step came to.
fn claim_due_steps(
    state: &mut ComAmazonawsCodedeployCodedeploy20141006State,
    now_ms: i64,
) -> Vec<Claim> {
    let pending: Vec<String> = state.pending_deployments.clone();
    let mut claimed = Vec::new();
    for deployment_id in pending {
        let Some(deployment) = state.deployments.get(&deployment_id) else {
            continue;
        };
        let Some(claim) = due_claim(&deployment_id, deployment, now_ms) else {
            continue;
        };
        if let Some(deployment) = state.deployments.get_mut(&deployment_id) {
            deployment.traffic_shift = TrafficShiftProgress::InFlight;
        }
        claimed.push(claim);
    }
    claimed
}

/// The claim `deployment` owes at `now_ms`, or `None` when it owes none.
///
/// A deployment is owed a step once it has STARTED and while it has not reached an outcome:
/// one a caller stopped, or the alarms its group polls stopped, is owed nothing further. The
/// steps fall in order — the replacement task set exists before any traffic moves onto it,
/// and the original one is taken away only after the move is complete.
fn due_claim(deployment_id: &str, deployment: &Deployment, now_ms: i64) -> Option<Claim> {
    let ecs = deployment.ecs_blue_green.as_ref()?;
    if deployment.complete_time_ms.is_some()
        || deployment_is_terminal(&deployment.status)
        || deployment.starts_at_ms > now_ms
        || deployment.awaits_revision_fetch
    {
        return None;
    }
    let mut claim = Claim {
        deployment_id: deployment_id.to_string(),
        ecs: ecs.clone(),
        region: deployment.region.clone(),
        replacement_task_set: deployment.ecs_replacement_task_set.clone(),
        original_target_group: deployment.ecs_original_target_group.clone(),
        replacement_target_group: deployment.ecs_replacement_target_group.clone(),
        original_task_set: deployment.ecs_original_task_set.clone(),
        due_increment: None,
        completes_the_move: false,
        terminates_the_original: false,
    };
    match deployment.traffic_shift {
        // The replacement task set, then the increments that move the traffic onto it.
        TrafficShiftProgress::Declared => {
            if claim.replacement_task_set.is_none() {
                return Some(claim);
            }
            // Traffic does not move while a lifecycle event hook that runs BEFORE it has not
            // reported `Succeeded`: that is what such a hook is for (see
            // `super::lifecycle_hook`).
            if !super::lifecycle_hook::traffic_is_allowed(deployment) {
                return None;
            }
            let (index, increment) = traffic_shift::due_increment(deployment, now_ms)?;
            claim.due_increment = Some((index, increment));
            claim.completes_the_move = index + 1 == deployment.traffic_increments.len();
            Some(claim)
        }
        // "The original task set is terminated after a successful deployment": once the move
        // is complete, the deletion falls due with the termination wait the deployment was
        // given ([`Deployment::completes_at_ms`] carries it).
        TrafficShiftProgress::Applied => {
            claim.original_task_set.as_ref()?;
            if deployment.completes_at_ms > now_ms {
                return None;
            }
            claim.terminates_the_original = true;
            Some(claim)
        }
        TrafficShiftProgress::InFlight | TrafficShiftProgress::Refused => None,
    }
}

/// Carry ONE claimed step out, with this service's store unlocked.
fn carry_out<Eng: Send + Sync + 'static>(engine: &Eng, claim: &Claim) -> Step {
    if claim.terminates_the_original {
        return terminate_original_task_set(engine, claim);
    }
    if claim.replacement_task_set.is_none() {
        return install_replacement_task_set(engine, claim);
    }
    let Some((index, increment)) = claim.due_increment else {
        return Step::Waiting;
    };
    // "waits for its targets to become healthy": the traffic moves onto the replacement task
    // set once Amazon ECS reports it has reached a steady state — "The task runningCount is
    // equal to the computedDesiredCount" and "The pendingCount is 0"
    // (`TaskSet$stabilityStatus`) — read out of that service's own records.
    if !replacement_has_settled(engine, claim) {
        return Step::Waiting;
    }
    shift_production_traffic(engine, claim, index, increment)
}

/// Create the replacement task set in the Amazon ECS service the group targets, bound to the
/// target group of the pair that is not serving the production traffic.
fn install_replacement_task_set<Eng: Send + Sync + 'static>(engine: &Eng, claim: &Claim) -> Step {
    let Some(route) = resolve_traffic_route(engine, claim) else {
        return Step::Refused(Denial::traffic_route(&claim.ecs));
    };
    let Some(state) = ecs::state::service_state(engine) else {
        return Step::Refused(Denial::invalid_service(&claim.ecs));
    };
    let declaration = &claim.ecs.declaration;
    let requested = ecs::state::RequestedTaskSet {
        task_definition: declaration.task_definition.clone(),
        // "The second is associated with the task set that serves traffic after the
        // deployment is complete": the replacement is not serving production traffic yet, so
        // it is `ACTIVE` until the increment that completes the move promotes it.
        status: ecs::state::TASK_SET_ACTIVE.to_string(),
        load_balancers: vec![ecs::ops::types::LoadBalancer {
            target_group_arn: Some(route.replacement_target_group.clone()),
            load_balancer_name: None,
            container_name: declaration.container_name.clone(),
            container_port: declaration.container_port,
            advanced_configuration: None,
        }],
        network_configuration: awsvpc_configuration(declaration),
        launch_type: None,
        capacity_provider_strategy: Vec::new(),
        platform_version: declaration.platform_version.clone(),
        external_id: None,
    };
    let key = (claim.ecs.cluster_name.clone(), claim.ecs.service_name.clone());
    let now = SystemTime::now();
    let created = {
        let mut store = state.lock();
        // The transitions Amazon ECS's own clock already owed the service, so the task set is
        // added to the service a client would read.
        store.advance_clock(now);
        let original = store
            .service_task_sets(&key.0, &key.1)
            .into_iter()
            .find(|task_set| task_set.status == ecs::state::TASK_SET_PRIMARY)
            .map(|task_set| task_set.id.clone());
        let created = store.add_controller_task_set(&key, &requested, &claim.region, now);
        (created, original)
    };
    let (created, original) = created;
    match created {
        Ok(Some(replacement)) => {
            // The steps a task the placement made needs taken before its containers start,
            // and the target-group registration a running task of a bound task set earns, are
            // that service's own passes — run here with every guard dropped, exactly as its
            // own handlers run them after a placement.
            settle_ecs_placements(engine);
            Step::Installed {
                replacement,
                original,
                original_target_group: route.original_target_group,
                replacement_target_group: route.replacement_target_group,
            }
        }
        Ok(None) => Step::Refused(if service_is_deployable(engine, claim) {
            Denial::update_error(&claim.ecs)
        } else {
            Denial::invalid_service(&claim.ecs)
        }),
        Err(_) => Step::Refused(Denial::update_error(&claim.ecs)),
    }
}

/// Whether the Amazon ECS service the group targets is one a task set can be created in at
/// all, which is what tells a refused create of the task set apart from a pair naming no
/// such service.
fn service_is_deployable<Eng: Send + Sync + 'static>(engine: &Eng, claim: &Claim) -> bool {
    let Some(state) = ecs::state::service_state(engine) else {
        return false;
    };
    let store = state.lock();
    store
        .services
        .get(&(claim.ecs.cluster_name.clone(), claim.ecs.service_name.clone()))
        .is_some_and(|service| service.is_active() && !service.places_tasks())
}

/// The networking the declaration asks the replacement task set to run with, or `None` when
/// it asks for none — which leaves it on the service's own
/// ([`ecs::state::RequestedTaskSet::network_configuration`]).
fn awsvpc_configuration(
    declaration: &EcsTaskSetDeclaration,
) -> Option<ecs::ops::types::NetworkConfiguration> {
    if declaration.subnets.is_empty() {
        return None;
    }
    Some(ecs::ops::types::NetworkConfiguration {
        awsvpc_configuration: Some(ecs::ops::types::AwsVpcConfiguration {
            subnets: declaration.subnets.clone(),
            security_groups: Some(declaration.security_groups.clone()),
            assign_public_ip: declaration.assign_public_ip.clone(),
        }),
    })
}

/// Whether Amazon ECS reports the replacement task set as having reached a steady state.
///
/// The transitions that service's own clock already owed are materialised first, and the
/// passes its handlers run after a placement are run with them — the launch steps a placed
/// task needs taken, and the target-group registration a running task of a bound task set
/// earns — so what is read here is what a client polling `DescribeTaskSets` would read, and
/// the targets the replacement serves through are registered while this deployment waits for
/// them rather than only at the next call to that service.
fn replacement_has_settled<Eng: Send + Sync + 'static>(engine: &Eng, claim: &Claim) -> bool {
    let Some(replacement) = claim.replacement_task_set.as_ref() else {
        return false;
    };
    let Some(state) = ecs::state::service_state(engine) else {
        return false;
    };
    let key = (
        claim.ecs.cluster_name.clone(),
        claim.ecs.service_name.clone(),
        replacement.clone(),
    );
    let settled = {
        let mut store = state.lock();
        store.advance_clock(SystemTime::now());
        store
            .task_sets
            .get(&key)
            .cloned()
            .map(|task_set| {
                store
                    .task_set_view(&task_set, ecs::state::TaskSetFields::default())
                    .stability_status
            })
            .unwrap_or_default()
    };
    settle_ecs_placements(engine);
    settled.as_deref() == Some(ecs::state::STABILITY_STATUS_STEADY_STATE)
}

/// The passes Amazon ECS runs after a placement, each with every store guard dropped — the
/// discipline those passes themselves require, since they take the Amazon EC2, Amazon ECR,
/// IAM and Elastic Load Balancing v2 stores' locks.
fn settle_ecs_placements<Eng: Send + Sync + 'static>(engine: &Eng) {
    ecs::state::settle_task_launches(engine);
    ecs::state::settle_load_balancer_targets(engine);
}

/// The two target groups one deployment moves traffic between, and the listeners that carry
/// it.
#[derive(Debug, Clone)]
struct TrafficRoute {
    /// The target group the ORIGINAL task set serves traffic through, as the deployment
    /// resolved it when it installed the replacement; `None` where the production listeners
    /// forwarded to neither group of the pair.
    original_target_group: Option<String>,
    /// The other group of the pair, which the replacement task set serves through.
    replacement_target_group: String,
    /// The listeners the production traffic route names, with the load balancer each belongs
    /// to.
    listeners: Vec<(String, String)>,
}

/// The listeners the production traffic route names, with the load balancer each belongs to,
/// as Elastic Load Balancing v2 holds them; empty where it holds none of them.
fn production_listeners<Eng: Send + Sync + 'static>(
    engine: &Eng,
    claim: &Claim,
) -> Vec<(String, String)> {
    let Some(state) = elbv2::state::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    claim
        .ecs
        .prod_listener_arns
        .iter()
        .filter_map(|arn| {
            store
                .listeners
                .get(arn)
                .map(|listener| (arn.clone(), listener.load_balancer_arn.clone()))
        })
        .collect()
}

/// Which target group of the group's pair each task set serves through, decided ONCE — when
/// the replacement task set is installed — and then kept with the deployment.
///
/// "One is associated with the original task set. The second is associated with the task set
/// that serves traffic after the deployment is complete", and which is which is decided by
/// what the production listeners forward to BEFORE the deployment has moved anything: the
/// group their DEFAULT actions already forward to is the original task set's, and the other
/// one of the pair is the replacement's. Only the default actions answer that, because they
/// are what `prodTrafficRoute` moves; a rule a caller added to the same listener forwards
/// traffic of its own and says nothing about which task set serves production.
///
/// The direction is NOT re-derived per increment: from the first increment of a canary or a
/// linear configuration the route forwards to BOTH groups, so "the group the listeners
/// forward to" no longer tells them apart — re-reading it would let the order the group
/// happens to list the pair in decide which way the remaining increments move.
///
/// A pair naming one group, or naming a group that service holds none of, describes no route
/// to move.
fn resolve_traffic_route<Eng: Send + Sync + 'static>(
    engine: &Eng,
    claim: &Claim,
) -> Option<TrafficRoute> {
    let listeners = production_listeners(engine, claim);
    if listeners.is_empty() {
        return None;
    }
    let state = elbv2::state::service_state(engine)?;
    let store = state.lock();
    let forwarded: Vec<String> = listeners
        .iter()
        .filter_map(|(arn, _)| store.listeners.get(arn))
        .flat_map(|listener| {
            elbv2::state::action_target_group_arns(&listener.default_actions)
        })
        .collect();
    // The pair's names as the ARNs that service minted for them, in the order the group
    // names them.
    let pair: Vec<String> = claim
        .ecs
        .target_group_names
        .iter()
        .filter_map(|name| {
            store
                .target_groups
                .values()
                .find(|group| &group.name == name)
                .map(|group| group.arn.clone())
        })
        .collect();
    let original_target_group = pair.iter().find(|arn| forwarded.contains(arn)).cloned();
    let replacement_target_group = pair
        .iter()
        .find(|arn| Some(*arn) != original_target_group.as_ref())
        .cloned()?;
    Some(TrafficRoute {
        original_target_group,
        replacement_target_group,
        listeners,
    })
}

/// The route the deployment ALREADY resolved, with the listeners read again: the two groups
/// are the ones it decided on at the install ([`resolve_traffic_route`]), so every increment
/// of one deployment moves the traffic the same way.
fn stored_traffic_route<Eng: Send + Sync + 'static>(
    engine: &Eng,
    claim: &Claim,
) -> Option<TrafficRoute> {
    let replacement_target_group = claim.replacement_target_group.clone()?;
    let listeners = production_listeners(engine, claim);
    (!listeners.is_empty()).then_some(TrafficRoute {
        original_target_group: claim.original_target_group.clone(),
        replacement_target_group,
        listeners,
    })
}

/// Rewrite the production traffic route for one increment, and promote the replacement task
/// set when that increment completes the move.
fn shift_production_traffic<Eng: Send + Sync + 'static>(
    engine: &Eng,
    claim: &Claim,
    index: usize,
    increment: Increment,
) -> Step {
    let refused = Step::Refused(Denial::traffic_route(&claim.ecs));
    let (Some(route), Some(state)) = (
        stored_traffic_route(engine, claim),
        elbv2::state::service_state(engine),
    ) else {
        return refused;
    };
    let actions = forward_actions(&route, increment);
    {
        let mut store = state.lock();
        let now = SystemTime::now();
        for (listener_arn, load_balancer_arn) in &route.listeners {
            let Some(listener) = store.listeners.get_mut(listener_arn) else {
                continue;
            };
            listener.default_actions.clone_from(&actions);
            // A load balancer routes to a target group an action of one of its listeners
            // forwards to, so the association that write made (and the one it released) is
            // brought in line, exactly as `ModifyListener` brings it.
            store.refresh_target_group_associations(load_balancer_arn, now);
        }
    }
    if !increment.is_whole_move() {
        return Step::Shifted(index);
    }
    // The increment that completes the move is the one that makes the replacement task set
    // the one "serving production traffic", which is `TaskSet$status` PRIMARY — promoted
    // through the same write `UpdateServicePrimaryTaskSet` makes.
    let (Some(replacement), Some(ecs_state)) = (
        claim.replacement_task_set.clone(),
        ecs::state::service_state(engine),
    ) else {
        return refused;
    };
    let key = (
        claim.ecs.cluster_name.clone(),
        claim.ecs.service_name.clone(),
        replacement,
    );
    let _ = ecs_state
        .lock()
        .promote_primary_task_set(&key, SystemTime::now());
    Step::Shifted(index)
}

/// The default actions a traffic route carries after one increment.
///
/// The last increment of a schedule is the whole move, so the route forwards to the
/// replacement group alone. Every earlier one is a SPLIT: the share of the traffic the
/// increment moves sits on the replacement group and the rest stays on the original, which
/// is what a weighted forward action expresses — and with no original group to split from,
/// the move is the whole of the route.
fn forward_actions(route: &TrafficRoute, increment: Increment) -> Vec<elbv2::ops::types::Action> {
    let forward_to = |target_group_arn: &str| elbv2::ops::types::Action {
        r#type: FORWARD_ACTION.to_string(),
        target_group_arn: Some(target_group_arn.to_string()),
        ..Default::default()
    };
    let Some(original) = route.original_target_group.as_ref().filter(|_| {
        !increment.is_whole_move() && increment.percent < WHOLE_ROUTE_WEIGHT
    }) else {
        return vec![forward_to(&route.replacement_target_group)];
    };
    let weighted = |target_group_arn: &str, weight: i32| elbv2::ops::types::TargetGroupTuple {
        target_group_arn: Some(target_group_arn.to_string()),
        weight: Some(weight),
    };
    let moved = increment.percent.clamp(0, WHOLE_ROUTE_WEIGHT);
    vec![elbv2::ops::types::Action {
        r#type: FORWARD_ACTION.to_string(),
        forward_config: Some(elbv2::ops::types::ForwardActionConfig {
            target_groups: Some(vec![
                weighted(original, WHOLE_ROUTE_WEIGHT - moved),
                weighted(&route.replacement_target_group, moved),
            ]),
            target_group_stickiness_config: None,
        }),
        ..Default::default()
    }]
}

/// Delete the original task set, which is what "The original task set is terminated after a
/// successful deployment" is.
fn terminate_original_task_set<Eng: Send + Sync + 'static>(engine: &Eng, claim: &Claim) -> Step {
    let Some(original) = claim.original_task_set.clone() else {
        return Step::Terminated;
    };
    let Some(state) = ecs::state::service_state(engine) else {
        return Step::Terminated;
    };
    let key = (
        claim.ecs.cluster_name.clone(),
        claim.ecs.service_name.clone(),
        original,
    );
    let _ = state.lock().delete_task_set(&key, SystemTime::now());
    Step::Terminated
}

/// Write back what one claimed step came to.
///
/// This store was unlocked while the other services' were reached, so what the claim was
/// made from is checked again: a deployment another caller stopped in between keeps the
/// outcome it already has, and the claim's own progress is released either way — a step this
/// pass could not finish is one the next pass picks up.
fn record(
    state: &mut ComAmazonawsCodedeployCodedeploy20141006State,
    claim: &Claim,
    step: &Step,
    now_ms: i64,
) {
    let deployment_id = claim.deployment_id.as_str();
    let Some(deployment) = state.deployments.get_mut(deployment_id) else {
        return;
    };
    match step {
        Step::Waiting => deployment.traffic_shift = TrafficShiftProgress::Declared,
        Step::Installed {
            replacement,
            original,
            original_target_group,
            replacement_target_group,
        } => {
            deployment.traffic_shift = TrafficShiftProgress::Declared;
            deployment.ecs_replacement_task_set = Some(replacement.clone());
            deployment.ecs_original_task_set.clone_from(original);
            deployment
                .ecs_original_target_group
                .clone_from(original_target_group);
            deployment.ecs_replacement_target_group = Some(replacement_target_group.clone());
        }
        Step::Shifted(index) => {
            deployment.traffic_increments_applied = index + 1;
            deployment.traffic_shift = if claim.completes_the_move {
                TrafficShiftProgress::Applied
            } else {
                TrafficShiftProgress::Declared
            };
        }
        Step::Terminated => {
            deployment.traffic_shift = TrafficShiftProgress::Applied;
            deployment.ecs_original_task_set = None;
        }
        Step::Refused(denial) => {
            deployment.traffic_shift = TrafficShiftProgress::Refused;
            // The task sets stay as this refusal found them — the original one serving the
            // traffic it was serving, and the replacement one, where the refusal came after
            // it was installed, still recorded as this deployment's — so what a reader and a
            // rollback see is what the service holds.
            if deployment.complete_time_ms.is_some() || deployment_is_terminal(&deployment.status) {
                return;
            }
            if deployment.start_time_ms.is_none() {
                deployment.start_time_ms = Some(deployment.starts_at_ms);
                deployment.status = DEPLOYMENT_STATUS_IN_PROGRESS.to_string();
            }
            state.settle_deployment(
                deployment_id,
                DEPLOYMENT_STATUS_FAILED,
                Some(ErrorInformation {
                    code: Some(denial.code.to_string()),
                    message: Some(denial.message.clone()),
                }),
                now_ms,
            );
            // A deployment that failed is rolled back when its configuration asks for that
            // on a deployment failure, from the one place that is decided.
            super::rollback::roll_back_on_failure(state, deployment_id, now_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    //! What ONE increment makes of the production traffic route — the write
    //! [`shift_production_traffic`] performs, over the increments a configuration
    //! schedules. Asserted here rather than by watching a listener through an increment's
    //! own window, which is a question about the clock rather than about the rule: the same
    //! rule is asserted end to end, through the operations a client calls, in
    //! `core/router/tests/all/codedeploy_ecs_blue_green_moves_the_production_listener.rs`.

    use super::*;

    /// The route a deployment of a service behind one listener moves traffic over.
    fn route(original: Option<&str>) -> TrafficRoute {
        TrafficRoute {
            original_target_group: original.map(str::to_string),
            replacement_target_group: "arn:green".to_string(),
            listeners: vec![("arn:listener".to_string(), "arn:alb".to_string())],
        }
    }

    /// One increment of a schedule, at the share of the route it moves.
    fn increment(percent: i32) -> Increment {
        Increment {
            percent,
            offset_ms: 0,
        }
    }

    /// The target groups and weights one action list forwards to, as a read reports them:
    /// the whole route on a group an action names directly, and each tuple's own weight
    /// where the action weights several.
    fn forwarded(actions: &[elbv2::ops::types::Action]) -> Vec<(String, i32)> {
        let mut reported = Vec::new();
        for action in actions {
            if let Some(arn) = action.target_group_arn.as_ref() {
                reported.push((arn.clone(), WHOLE_ROUTE_WEIGHT));
            }
            for tuple in action
                .forward_config
                .iter()
                .flat_map(|config| config.target_groups.iter().flatten())
            {
                if let Some(arn) = tuple.target_group_arn.as_ref() {
                    reported.push((arn.clone(), tuple.weight.unwrap_or_default()));
                }
            }
        }
        reported
    }

    /// One row of the table below: the case, the route it moves, the share of that route the
    /// increment moves, and the weights the route must then carry.
    struct Row<'a> {
        case: &'a str,
        route: &'a TrafficRoute,
        percent: i32,
        expected: &'a [(&'a str, i32)],
    }

    /// RULE: an increment that completes the move forwards the route to the replacement
    /// group alone; every earlier one splits it, with the share that increment moves on the
    /// replacement group and the rest left on the original — the proportions the
    /// configuration's own increments describe, over the schedules it can describe.
    #[test]
    fn each_increment_leaves_the_route_split_in_the_share_it_moved() {
        let with_original = route(Some("arn:blue"));
        let rows = [
            Row {
                case: "a canary's first increment",
                route: &with_original,
                percent: 10,
                expected: &[("arn:blue", 90), ("arn:green", 10)],
            },
            Row {
                case: "a linear shift's first increment",
                route: &with_original,
                percent: 25,
                expected: &[("arn:blue", 75), ("arn:green", 25)],
            },
            Row {
                case: "a linear shift's last incomplete increment",
                route: &with_original,
                percent: 90,
                expected: &[("arn:blue", 10), ("arn:green", 90)],
            },
            Row {
                case: "the increment that completes the move",
                route: &with_original,
                percent: 100,
                expected: &[("arn:green", 100)],
            },
            Row {
                case: "an increment moving no traffic at all",
                route: &with_original,
                percent: 0,
                expected: &[("arn:blue", 100), ("arn:green", 0)],
            },
        ];
        for row in &rows {
            let actions = forward_actions(row.route, increment(row.percent));
            let reported = forwarded(&actions);
            let borrowed: Vec<(&str, i32)> = reported
                .iter()
                .map(|(arn, weight)| (arn.as_str(), *weight))
                .collect();
            assert_eq!(
                borrowed.as_slice(),
                row.expected,
                "case {:?}: expected the route {:?}, got {borrowed:?}",
                row.case,
                row.expected
            );
        }
    }

    /// RULE: with no original group to split from — a production route that forwarded to
    /// neither group of the pair — every increment is the whole of the route, since there is
    /// no share of it to leave behind.
    #[test]
    fn a_route_with_no_original_group_moves_whole() {
        let route = route(None);
        for percent in [10, 50, 100] {
            let reported = forwarded(&forward_actions(&route, increment(percent)));
            let borrowed: Vec<(&str, i32)> = reported
                .iter()
                .map(|(arn, weight)| (arn.as_str(), *weight))
                .collect();
            assert_eq!(
                borrowed.as_slice(),
                [("arn:green", WHOLE_ROUTE_WEIGHT)].as_slice(),
                "case {percent}%: expected the whole route on the replacement group, got \
                 {borrowed:?}"
            );
        }
    }
}
