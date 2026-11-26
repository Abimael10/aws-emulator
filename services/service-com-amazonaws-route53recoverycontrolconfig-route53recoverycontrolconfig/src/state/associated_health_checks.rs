// SPDX-License-Identifier: CC0-1.0

//! Which Amazon Route 53 health checks FOLLOW a routing control of this service.
//!
//! A health check is a resource of Route 53, not of this service: a client creates one
//! with `CreateHealthCheck` and makes it follow a routing control by naming that control
//! in `HealthCheckConfig$RoutingControlArn` ("The Amazon Resource Name (ARN) for the
//! Route 53 Application Recovery Controller routing control"). So the association is a
//! fact of THAT service's store, and this module reads it there — through Route 53's own
//! state ([`route53::service_state`]) — rather than keeping a second copy here that a
//! health check created or deleted on the other side would leave stale.
//!
//! Read with NO guard of this service's own store held: the two stores are never locked at
//! once.

use ::service_com_amazonaws_route53_awsdnsv20130401::state as route53;

/// The ids of the account's Route 53 health checks whose configuration names
/// `routing_control_arn`, in ascending id order.
///
/// The order is the one Route 53's own store keys health checks in, so it is a stable
/// total order and a pagination cursor built from an id always resumes at the same place.
///
/// An engine that vends no Route 53 store holds no health check at all, so nothing
/// follows any routing control on it: the answer is then the empty list, which is what a
/// routing control nobody has associated a health check with is answered with as well.
pub fn following_health_check_ids<Eng: Send + Sync + 'static>(
    engine: &Eng,
    routing_control_arn: &str,
) -> Vec<String> {
    let Some(state) = route53::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    store
        .health_checks
        .values()
        .filter(|check| check.config.routing_control_arn.as_deref() == Some(routing_control_arn))
        .map(|check| check.id.clone())
        .collect()
}
