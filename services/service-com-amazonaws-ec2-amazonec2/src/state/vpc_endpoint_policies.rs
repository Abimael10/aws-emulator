// SPDX-License-Identifier: CC0-1.0

//! The VPC ENDPOINT POLICIES in force on one instance's own traffic.
//!
//! # Why this lives here
//!
//! A VPC endpoint and the policy attached to it are this service's records
//! (`CreateVpcEndpoint`, whose `PolicyDocument` "controls access to the service that the
//! endpoint is for"), and so are the network facts that decide WHICH of them an instance's
//! traffic meets: which subnet the instance is in, which VPC that subnet belongs to and which
//! route table that subnet routes through. A component that runs code INSIDE the guest — the
//! agent Systems Manager delivers a command to — has to know what those documents say about the
//! calls that code makes, so it asks this store for them rather than keeping a second notion of
//! which endpoints a machine's traffic leaves through.
//!
//! What a document DECIDES is not decided here: that is the one reading of an endpoint policy
//! this workspace holds ([`::service_common::vpc_endpoint_calls`]), handed the documents
//! collected below. Nothing here writes, mints or resolves anything about the call itself.
//!
//! # Which endpoints an instance's traffic meets
//!
//! A GATEWAY endpoint — "you can use gateway endpoints to connect to Amazon S3 and DynamoDB"
//! (Amazon VPC User Guide, "Gateway endpoints") — serves the subnets whose ROUTE TABLE it was
//! created for: "you must specify the route tables … we add a route to each specified route
//! table that points to the endpoint" (`CreateVpcEndpointRequest$RouteTableIds`). So an endpoint
//! of the instance's own VPC is in force on that instance's calls when it was created for the
//! table the instance's subnet routes through ([`ComAmazonawsEc2Amazonec2State::subnet_route_table`]),
//! and an endpoint created for NO table narrows nothing about which subnets of the VPC it serves,
//! so it is in force on the whole VPC's.
//!
//! An endpoint whose state is not `available` yet serves nothing — the state is the one this
//! service OWES it at the instant asked, so an endpoint whose move to `available` has already
//! fallen due is available whether or not anybody has called since
//! ([`ComAmazonawsEc2Amazonec2State::vpc_endpoint_state_at`]), exactly as an instance whose move
//! to `running` has.
//!
//! INTERFACE endpoints are left out: such an endpoint is reached by the PRIVATE DNS NAME it
//! publishes rather than by a route of the subnet's table, so which calls travel through one is
//! a fact of what the guest RESOLVED, and the change that gives a guest's calls that resolution
//! is the one that can decide them.

use ::std::time::Instant;

use ::service_common::vpc_endpoint_calls::{endpoint_service_token, ServiceEndpointPolicy};

use super::{ComAmazonawsEc2Amazonec2State, VPC_ENDPOINT_STATE_AVAILABLE, VPC_ENDPOINT_TYPE_GATEWAY};

/// The endpoint policies in force on the calls made from inside the guest of the instance
/// `instance_id` — one per endpoint of its own VPC that serves it, with the service token that
/// endpoint is for and the document attached to it.
///
/// EMPTY where there is nothing in force: no store on this engine, an instance this store does
/// not hold, one it can place in no VPC, and a VPC whose endpoints serve no subnet of this
/// instance's. A caller reads this ONCE, before it runs anything, so no guard of this store is
/// held while the code it decides runs.
#[must_use]
pub fn instance_endpoint_policies<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
) -> Vec<ServiceEndpointPolicy> {
    let Some(state) = super::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    store.instance_endpoint_policies(instance_id, Instant::now())
}

impl ComAmazonawsEc2Amazonec2State {
    /// The state this service OWES the VPC endpoint `endpoint_id` at `settled`: the state it
    /// holds, or the one a pending move whose instant has already passed moves it to.
    ///
    /// Read rather than materialised, so a caller asking what an endpoint serves NOW writes
    /// nothing: the move itself is this store's own to make
    /// ([`Self::materialize_due_transitions`]).
    #[must_use]
    pub fn vpc_endpoint_state_at(&self, endpoint_id: &str, settled: Instant) -> Option<String> {
        let owed = self
            .vpc_endpoint_moves
            .get(endpoint_id)
            .filter(|pending| pending.due_at <= settled)
            .map(|pending| pending.to.clone());
        owed.or_else(|| self.vpc_endpoints.get(endpoint_id)?.state.clone())
    }

    /// The endpoint policies in force on the instance `instance_id`'s own traffic at `settled`
    /// (see the module docs), read off this store alone.
    #[must_use]
    pub fn instance_endpoint_policies(
        &self,
        instance_id: &str,
        settled: Instant,
    ) -> Vec<ServiceEndpointPolicy> {
        let Some(instance) = self.instances.get(instance_id) else {
            return Vec::new();
        };
        let Some(vpc_id) = self.instance_vpc_id(instance) else {
            return Vec::new();
        };
        let route_table = self
            .instance_subnet_id(instance)
            .and_then(|subnet_id| self.subnet_route_table(&subnet_id, &vpc_id));
        self.vpc_endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.vpc_id.as_deref() == Some(vpc_id.as_str()))
            .filter(|(_, endpoint)| {
                endpoint.vpc_endpoint_type.as_deref() == Some(VPC_ENDPOINT_TYPE_GATEWAY)
            })
            .filter(|(endpoint_id, _)| {
                self.vpc_endpoint_state_at(endpoint_id, settled).as_deref()
                    == Some(VPC_ENDPOINT_STATE_AVAILABLE)
            })
            .filter(|(_, endpoint)| serves_route_table(endpoint, route_table.as_deref()))
            .filter_map(|(endpoint_id, endpoint)| {
                Some(ServiceEndpointPolicy {
                    endpoint_id: endpoint_id.clone(),
                    service_token: endpoint_service_token(endpoint.service_name.as_deref()?)?
                        .to_string(),
                    // An endpoint created with no `PolicyDocument` of its own is stored with
                    // the full-access one this service gives it
                    // ([`super::default_vpc_endpoint_policy`]), so a document is what every
                    // endpoint record carries; one carrying none in force on nothing.
                    policy_document: endpoint.policy_document.clone()?,
                })
            })
            .collect()
    }
}

/// Whether `endpoint` serves the subnet whose table is `route_table`: it was created for that
/// table, or for no table at all (see the module docs).
fn serves_route_table(endpoint: &super::VpcEndpoint, route_table: Option<&str>) -> bool {
    let tables = endpoint.route_table_ids.as_deref().unwrap_or_default();
    tables.is_empty() || route_table.is_some_and(|table| tables.iter().any(|named| named == table))
}
