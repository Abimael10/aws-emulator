// SPDX-License-Identifier: CC0-1.0

//! Proving that the owner of an endpoint service owns the private DNS name assigned to it:
//! the verification a `StartVpcEndpointServicePrivateDnsVerification` begins, and the state
//! the clock owes the name once that verification has run.
//!
//! "Consumers of the endpoint service can use the private name only when the state is
//! `verified`" (`PrivateDnsNameConfiguration$State`), and the catalog that member is drawn
//! from carries `pendingVerification` beside it
//! (`smithy_query(query="shape",shape="DnsNameState")`), so a name waiting on its proof sits
//! in a NON-TERMINAL state: the verification is work the service takes on and reports the
//! outcome of later, not an answer the start call carries. The start therefore records the
//! instant the outcome falls due — one [`TRANSITION_DELAY`] out, the span every other move
//! this store owes uses ([`ComAmazonawsEc2Amazonec2State::vpc_endpoint_service_private_dns_verified_at`]) —
//! and [`ComAmazonawsEc2Amazonec2State::materialize_due_transitions`] applies it, so a caller
//! reading straight back still sees the name pending and a later one finds it verified, with
//! nothing slept for in between.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// Refuse a request that asks for the private DNS name of a service that has none: HTTP 400
/// [`INVALID_PARAMETER`], naming the service the request asked about.
///
/// The same class as [`private_dns_unsupported_service`] — a member's value cannot be honoured
/// given the state of the resource it names, rather than being wrong in itself: the
/// verification "prove[s] that the service provider owns the private DNS name domain for the
/// endpoint service" (`StartVpcEndpointServicePrivateDnsVerification`), so a service carrying
/// no such name ("(Interface endpoint configuration) The private DNS name to assign to the
/// endpoint service", `ModifyVpcEndpointServiceConfigurationRequest$PrivateDnsName`) has
/// nothing to prove ownership of and no record a proof could be published under.
pub fn private_dns_name_not_configured<E>(service_id: &str) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_PARAMETER,
        400,
        format!("The service {service_id} does not have a private DNS name configured."),
    )
}

impl ComAmazonawsEc2Amazonec2State {
    /// Begin the verification of the private DNS name of the endpoint service `service_id`, and
    /// answer `false` when that service has no private DNS name to verify.
    ///
    /// "Initiates the verification process to prove that the service provider owns the private
    /// DNS name domain for the endpoint service"
    /// (`StartVpcEndpointServicePrivateDnsVerification`): the name is put back into
    /// [`DNS_NAME_STATE_PENDING_VERIFICATION`] — where it already is for a name no verification
    /// has finished for, and where a name whose last verification ended in
    /// `failed` goes again — and the outcome falls due one [`TRANSITION_DELAY`] later
    /// ([`Self::materialize_due_private_dns_name_verifications`]). A name already
    /// [`DNS_NAME_STATE_VERIFIED`] is left verified and owes nothing: the proof the owner
    /// published is still published, so a repeat of the call cannot take the name away from the
    /// consumers already using it.
    ///
    /// The record the owner has to publish is the one the name was given when it was assigned
    /// ([`private_dns_name_verification`]), so a start neither mints a second record nor changes
    /// the one a describe has already reported.
    pub fn start_vpc_endpoint_service_private_dns_verification(
        &mut self,
        service_id: &str,
    ) -> bool {
        let Some(configuration) = self.vpc_endpoint_services.get_mut(service_id) else {
            return false;
        };
        let Some(private_dns_name) = configuration.private_dns_name.clone() else {
            return false;
        };
        // A stored name whose verification record somehow went missing is given the record it
        // was assigned with, derived from the service and the name, so what the owner must
        // publish is the same value every read of this service reports.
        let verification = configuration
            .private_dns_name_configuration
            .get_or_insert_with(|| private_dns_name_verification(service_id, &private_dns_name));
        if verification.state.as_deref() == Some(DNS_NAME_STATE_VERIFIED) {
            self.vpc_endpoint_service_private_dns_verified_at
                .remove(service_id);
            return true;
        }
        verification.state = Some(DNS_NAME_STATE_PENDING_VERIFICATION.to_string());
        self.vpc_endpoint_service_private_dns_verified_at
            .insert(service_id.to_string(), Instant::now() + TRANSITION_DELAY);
        true
    }

    /// Forget the verification the clock still owes the endpoint service `service_id`.
    ///
    /// The name a verification was started for is the name it is a proof of, so a call that
    /// REPLACES or REMOVES that name
    /// (`ModifyVpcEndpointServiceConfigurationRequest$PrivateDnsName`,
    /// `$RemovePrivateDnsName`), and a delete that takes the whole service, leave nothing for
    /// the outcome to be reported on — were it still owed, a name assigned afterwards would be
    /// verified by a proof nobody published for it.
    pub fn forget_vpc_endpoint_service_private_dns_verification(&mut self, service_id: &str) {
        self.vpc_endpoint_service_private_dns_verified_at
            .remove(service_id);
    }

    /// Apply the verification outcome the clock owes every endpoint service a start left
    /// pending, as of `now`.
    ///
    /// The outcome was reached at the instant the start recorded for it whether or not anybody
    /// called again, so it is applied from [`Self::materialize_due_transitions`], its one
    /// caller, before anything reads the store. This emulator verifies ownership of nothing, so
    /// what the verification of a published record reaches is
    /// [`DNS_NAME_STATE_VERIFIED`] — the state "consumers of the endpoint service can use the
    /// private name" in (`PrivateDnsNameConfiguration$State`).
    pub(super) fn materialize_due_private_dns_name_verifications(&mut self, now: Instant) {
        let services = &mut self.vpc_endpoint_services;
        self.vpc_endpoint_service_private_dns_verified_at
            .retain(|service_id, due_at| {
                if now < *due_at {
                    return true;
                }
                if let Some(verification) = services
                    .get_mut(service_id)
                    .and_then(|configuration| configuration.private_dns_name_configuration.as_mut())
                {
                    if verification.state.as_deref() == Some(DNS_NAME_STATE_PENDING_VERIFICATION) {
                        verification.state = Some(DNS_NAME_STATE_VERIFIED.to_string());
                    }
                }
                // The outcome is applied once; nothing is owed on this service any more.
                false
            });
    }
}
