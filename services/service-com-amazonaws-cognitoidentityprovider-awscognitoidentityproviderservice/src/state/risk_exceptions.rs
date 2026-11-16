// SPDX-License-Identifier: CC0-1.0

//! The threat-protection exceptions a pool evaluates on an authentication
//! request: the always-block address ranges its stored
//! `RiskExceptionConfiguration` carries, and the address a request presents to
//! them.
//!
//! Every authentication operation asks the same question of the same stored
//! configuration, so the app-client-facing and the administrative paths cannot
//! disagree about which requests a tenant's blocklist stops.

use super::*;

/// The message an authentication request the pool's threat protection blocks is
/// refused with. Fixed text: it reports neither the address the request presented
/// nor the range that covers it, because the refusal says only that the request
/// was not allowed.
pub const AUTHENTICATION_BLOCKED_MESSAGE: &str = "Request not allowed due to security reasons.";

/// The message a request that sends a source address its app client may not
/// propagate is refused with ([`UserPool::forbids_propagated_address`]).
///
/// Fixed text: the member the value arrived in is the same one for every such
/// request, and the address it carried is not echoed back.
pub const PROPAGATED_ADDRESS_FORBIDDEN_MESSAGE: &str =
    "Cannot use IP Address passed into UserContextData";

/// What an authentication request presents of the caller's address to the pool's
/// threat protection.
#[derive(Debug, Clone, Copy)]
pub enum PresentedAddress<'a> {
    /// `ContextData.IpAddress`, "the source IP address of your user's device"
    /// (`ContextDataType` in this service's model): an administrative
    /// authentication operation is called by the application's own backend, which
    /// states the address of the device it is signing in, so the pool evaluates
    /// what the request states.
    Stated(Option<&'a str>),
    /// `UserContextData.IpAddress`, the same source address sent by the
    /// application itself: "when `true`, your application can include additional
    /// `UserContextData` in authentication requests. This data includes the IP
    /// address ... If you don't include this parameter, you can't send the source
    /// IP address to Amazon Cognito threat protection features"
    /// (`EnablePropagateAdditionalUserContextData`), so it reaches threat
    /// protection only from an app client configured to propagate it.
    Propagated(Option<&'a str>),
}

impl UserPool {
    /// Whether the app client `client_id` may NOT carry the source address a
    /// request's `UserContextData` presents.
    ///
    /// "When `true`, your application can include additional `UserContextData` in
    /// authentication requests. This data includes the IP address ... If you don't
    /// include this parameter, you can't send the source IP address to Amazon
    /// Cognito threat protection features"
    /// (`EnablePropagateAdditionalUserContextData` in this service's model): the
    /// address is propagated by the app clients configured for it, so a request
    /// that sends one to any other app client of this pool sends a value that app
    /// client may not use, and is refused with
    /// [`PROPAGATED_ADDRESS_FORBIDDEN_MESSAGE`] rather than evaluated against it.
    ///
    /// An app client this pool does not hold is left to the operation's own
    /// lookup, which words an unresolved client id its own way.
    #[must_use]
    pub fn forbids_propagated_address(&self, client_id: &str) -> bool {
        self.clients
            .get(client_id)
            .is_some_and(|client| !client.enable_propagate_additional_user_context_data)
    }

    /// Whether this pool's threat protection blocks an authentication request
    /// that the app client `client_id` carries and that presents `presented`.
    ///
    /// `BlockedIPRangeList` is "an always-block IP address list. Overrides the
    /// risk decision and always blocks authentication requests. This parameter is
    /// displayed and set in CIDR notation"
    /// (`RiskExceptionConfigurationType` in this service's model), so an
    /// authentication request from an address inside one of those ranges is
    /// refused however good the credential it presents is. Three things have to
    /// hold for a request to be blocked:
    ///
    /// * the pool takes automatic action on what its threat protection decides —
    ///   `UserPoolAddOns` is set to `ENFORCED`, to "configure automatic security
    ///   responses to potentially unwanted traffic to your user pool", while
    ///   `AUDIT` is the mode that "log[s] user security information but take[s] no
    ///   action" (`UserPoolAddOnsType`), and a pool with no add-ons evaluates
    ///   nothing at all;
    /// * the request presents an address threat protection receives
    ///   ([`PresentedAddress`]);
    /// * a range of the configuration that applies to this app client
    ///   ([`UserPool::blocked_ip_ranges`]) covers that address.
    #[must_use]
    pub fn blocks_authentication(
        &self,
        client_id: &str,
        presented: PresentedAddress<'_>,
    ) -> bool {
        if !self.enforces_threat_protection() {
            return false;
        }
        let address = match presented {
            PresentedAddress::Stated(address) => address,
            PresentedAddress::Propagated(address) => address.filter(|_| {
                self.clients
                    .get(client_id)
                    .is_some_and(|client| client.enable_propagate_additional_user_context_data)
            }),
        };
        let Some(address) = address.filter(|address| !address.is_empty()) else {
            return false;
        };
        self.blocked_ip_ranges(client_id)
            .iter()
            .any(|range| address_in_range(address, range))
    }

    /// Whether this pool ACTS on what its threat protection decides, rather than
    /// only recording it: `UserPoolAddOns` is set to
    /// [`ADVANCED_SECURITY_MODE_ENFORCED`], the mode that configures "automatic
    /// security responses to potentially unwanted traffic to your user pool",
    /// where [`ADVANCED_SECURITY_MODE_AUDIT`] "log[s] user security information
    /// but take[s] no action" and a pool storing no add-ons has threat protection
    /// off (`UserPoolAddOnsType` in this service's model).
    #[must_use]
    pub fn enforces_threat_protection(&self) -> bool {
        self.user_pool_add_ons.as_ref().is_some_and(|add_ons| {
            add_ons.advanced_security_mode == ADVANCED_SECURITY_MODE_ENFORCED
        })
    }

    /// The always-block address ranges that apply to the app client `client_id`.
    ///
    /// The exceptions are stored at the level a `SetRiskConfiguration` call named:
    /// `RiskConfigurationType.ClientId` is "the app client where this
    /// configuration is applied", and an app client that holds none of its own
    /// uses the pool's — "to reset the threat protection settings of an app client
    /// to be inherited from the user pool, send `UserPoolId` and `ClientId`
    /// parameters only" (`SetRiskConfiguration`), which stores a configuration
    /// carrying no exceptions at all. So the client's own exceptions apply where it
    /// stores some, and the pool's apply everywhere else.
    #[must_use]
    pub fn blocked_ip_ranges(&self, client_id: &str) -> &[String] {
        self.clients
            .get(client_id)
            .and_then(|client| client.risk_configuration.as_ref())
            .and_then(exception_configuration)
            .or_else(|| {
                self.risk_configuration
                    .as_ref()
                    .and_then(exception_configuration)
            })
            .and_then(|exceptions| exceptions.blocked_ip_range_list.as_deref())
            .unwrap_or_default()
    }
}

/// The address-range exceptions `configuration` carries, and `None` where it
/// carries none — a level whose stored configuration states no exceptions states
/// none, so the level above it is what applies.
fn exception_configuration(
    configuration: &RiskConfiguration,
) -> Option<&RiskExceptionConfigurationType> {
    configuration.risk_exception_configuration.as_ref()
}

impl ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState {
    /// Whether a request to the app client `client_id` may NOT present `address`
    /// as the source address of the user's device
    /// ([`UserPool::forbids_propagated_address`]).
    ///
    /// The ONE reading of that question, so every operation whose input carries
    /// `UserContextData` refuses the same requests with the same sentence. A
    /// request that presents no address at all presents nothing an app client
    /// needs to be configured for.
    #[must_use]
    pub fn propagated_address_forbidden(&self, client_id: &str, address: Option<&str>) -> bool {
        address.is_some_and(|address| !address.is_empty())
            && self
                .pool_of_client(client_id)
                .is_some_and(|pool| pool.forbids_propagated_address(client_id))
    }
}

/// Whether the address `address` names lies in the network `range` names.
///
/// Both sides are read as ADDRESSES rather than as text, by the tree's IP-network
/// parser: the list is "displayed and set in CIDR notation"
/// (`RiskExceptionConfigurationType`), so a range holds for every address of its
/// block and not only for one spelling of it, and a range naming a single address
/// is the host network that address is. An address or a range this emulator cannot
/// read is no match: a refusal rests on a block that covers the request.
fn address_in_range(address: &str, range: &str) -> bool {
    let Ok(address) = address.trim().parse::<::std::net::IpAddr>() else {
        return false;
    };
    let range = range.trim();
    match range.parse::<::ipnet::IpNet>() {
        Ok(network) => network.contains(&address),
        Err(_) => range
            .parse::<::std::net::IpAddr>()
            .is_ok_and(|single| single == address),
    }
}
