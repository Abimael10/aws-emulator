// SPDX-License-Identifier: CC0-1.0

//! The Traffic Mirror filter store: the ids, bounds, byte charge and error channel the
//! Traffic Mirror filter operations share, with the one write and the one removal path a
//! filter enters and leaves the store by.
//!
//! The filters themselves live on the service's one state struct
//! ([`ComAmazonawsEc2Amazonec2State::traffic_mirror_filters`]) as the MODELLED
//! [`TrafficMirrorFilter`] shape, so the create and the describe cannot report one filter
//! differently; only the primitives over that store live here.

use super::{tag_list_bytes, ComAmazonawsEc2Amazonec2State};
use crate::ops::types::TrafficMirrorFilter;
use crate::scaffolded::Scaffolded;

/// The id prefix a Traffic Mirror filter is minted under.
///
/// EC2 names the resource's ids `tmf-<hex>`, the same `<prefix>-<hex>` form every other
/// EC2 resource id takes, so [`ComAmazonawsEc2Amazonec2State::next_id`] mints them.
pub const TRAFFIC_MIRROR_FILTER_ID_PREFIX: &str = "tmf";

/// The one `ResourceType` a `CreateTrafficMirrorFilter` tag specification may name: "The
/// tags to assign to a Traffic Mirror filter"
/// (`CreateTrafficMirrorFilterRequest$TagSpecifications`), spelled as the modelled
/// `ResourceType` enum spells it (`smithy_query(query="shape",shape="ResourceType")`).
pub const TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE: &str = "traffic-mirror-filter";

/// How many Traffic Mirror filters this emulator stores before
/// `CreateTrafficMirrorFilter` is refused.
///
/// CHOSEN, like `MAX_VERIFIED_ACCESS_TRUST_PROVIDERS` and for the same reason: nothing in
/// this emulator reclaims a stored filter, so the store needs a ceiling that is not the
/// process's memory. A count ceiling is not a memory bound on its own — see
/// `MAX_STORED_BYTES` — so the caller-chosen bytes of each record are charged against that
/// budget too ([`traffic_mirror_filter_payload_bytes`]).
pub const MAX_TRAFFIC_MIRROR_FILTERS: usize = 1_000;

/// The largest page `DescribeTrafficMirrorFilters` serves, and the page it serves when the
/// request names no size.
///
/// The top of the modelled `MaxResults` range, `TrafficMirroringMaxResults`'s
/// `range 5..1000`
/// (`smithy_query(query="operation",operation="DescribeTrafficMirrorFilters")`), so a
/// request that names no page size is answered with the largest page the operation can be
/// ASKED for and the `NextToken` path behaves the same either way.
pub const MAX_TRAFFIC_MIRROR_FILTERS_PAGE_SIZE: usize = 1_000;

/// EC2's code for a Traffic Mirror filter id that names no filter of the account.
///
/// EC2's model declares no error shape at all (see [`crate::scaffolded::ServiceError`]),
/// and the service answers `Invalid<Resource>Id.NotFound` for an id of a resource it
/// cannot resolve — `InvalidVolume.NotFound`, `InvalidLaunchTemplateId.NotFound` — so that
/// is the code this resource's id gets.
pub const INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND: &str =
    "InvalidTrafficMirrorFilterId.NotFound";

/// EC2's quota code for one Traffic Mirror filter too many, named the way every other
/// per-resource EC2 quota code is (`VolumeLimitExceeded`, `LaunchTemplateLimitExceeded`).
pub const TRAFFIC_MIRROR_FILTER_LIMIT_EXCEEDED: &str = "TrafficMirrorFilterLimitExceeded";

/// The caller-chosen bytes one stored Traffic Mirror filter holds for the process's
/// lifetime: its tags, its description and the client token the token map keeps for it.
///
/// Everything else on the record — the id, and the rule and network-service sets other
/// operations fill — is derived by this crate or drawn from a modelled enum, so it is
/// bounded already and not counted here. The ONE definition of that charge, so a writer
/// cannot store bytes the store's budget never saw.
pub fn traffic_mirror_filter_payload_bytes(
    filter: &TrafficMirrorFilter,
    client_token: Option<&str>,
) -> usize {
    tag_list_bytes(filter.tags.as_deref().unwrap_or_default())
        .saturating_add(filter.description.as_ref().map_or(0, String::len))
        .saturating_add(client_token.map_or(0, str::len))
}

/// Refuse a request that named a Traffic Mirror filter id this account holds no filter
/// for: HTTP 400 [`INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND`].
///
/// `TrafficMirrorFilterId` publishes no pattern
/// (`smithy_query(query="shape",shape="TrafficMirrorFilterId")`), so there is no spelling
/// to call an id malformed by: one code covers an id of any shape, because an id this
/// service never minted names no filter of the account whether or not it reads like one.
/// The message names the id the REQUEST gave, so a client reading the text sees which id
/// it was.
pub fn invalid_traffic_mirror_filter_id_not_found<E>(
    traffic_mirror_filter_id: &str,
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND,
        400,
        format!("{traffic_mirror_filter_id} does not exist"),
    )
}

/// Refuse a create that would take the store past [`MAX_TRAFFIC_MIRROR_FILTERS`] or past
/// the store's byte budget: HTTP 400 [`TRAFFIC_MIRROR_FILTER_LIMIT_EXCEEDED`].
pub fn traffic_mirror_filter_limit_exceeded<E>(message: impl Into<String>) -> Scaffolded<E> {
    Scaffolded::service_error(TRAFFIC_MIRROR_FILTER_LIMIT_EXCEEDED, 400, message)
}

/// Is `allowed` the `TrafficMirrorNetworkService` catalog — the value set the model declares
/// for the network services a Traffic Mirror filter can mirror
/// (`crate::ops::types::TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES`)?
///
/// Keyed to the TYPE's value set, the way every catalog-keyed refusal of this crate is
/// ([`super::is_modelled_catalog`] records that rule), so every member of this model that names
/// a mirrored network service is refused an unpublished one in the same words.
pub fn is_traffic_mirror_network_service_catalog(allowed: &[String]) -> bool {
    super::is_modelled_catalog(
        allowed,
        crate::ops::types::TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES,
    )
}

/// How EC2 states a network service the catalog does not hold: HTTP 400
/// `InvalidParameterValue`, "'`<value>`' is not a valid network service".
///
/// RECORDED: EC2 does not state this refusal as the generic validation clause, and it names
/// neither the parameter nor the catalog — only the value it could not read, which is the one
/// part of the sentence built from the request. That echo is bounded the way every refusal of
/// this crate bounds a value the caller chose the length of, so echoing it cannot amplify a
/// request into an arbitrarily long response.
pub fn traffic_mirror_network_service_refusal_message(value: &str) -> String {
    format!(
        "'{value}' is not a valid network service",
        value = super::echoed_resource_id(value),
    )
}

impl ComAmazonawsEc2Amazonec2State {
    /// Whether another Traffic Mirror filter fits inside the store's ceiling
    /// ([`MAX_TRAFFIC_MIRROR_FILTERS`]).
    ///
    /// The same contract as [`Self::has_vpc_capacity`]: a create asks first and refuses
    /// with [`traffic_mirror_filter_limit_exceeded`] when it answers `false`.
    pub fn has_traffic_mirror_filter_capacity(&self) -> bool {
        self.traffic_mirror_filters.len() < MAX_TRAFFIC_MIRROR_FILTERS
    }

    /// Store `filter` under a freshly minted `TrafficMirrorFilterId`, charge the
    /// caller-chosen bytes it and `client_token` retain against the store's byte budget,
    /// and answer the id it was given.
    ///
    /// The id is minted HERE rather than by the writer, so a filter never shares an id
    /// with another resource of this store, and the record is written with that id on it so
    /// no reader has to learn the key twice. The charge happens at the insert the way
    /// [`Self::insert_vpc`] charges a VPC's tags, so no path can store those bytes
    /// uncharged; a create still asks [`Self::has_traffic_mirror_filter_capacity`] and
    /// [`Self::would_exceed_stored_bytes`] first so it can refuse instead of overshooting.
    pub fn insert_traffic_mirror_filter(
        &mut self,
        mut filter: TrafficMirrorFilter,
        client_token: Option<&str>,
    ) -> String {
        let traffic_mirror_filter_id = self.next_id(TRAFFIC_MIRROR_FILTER_ID_PREFIX);
        filter.traffic_mirror_filter_id = Some(traffic_mirror_filter_id.clone());
        self.add_stored_bytes(traffic_mirror_filter_payload_bytes(&filter, client_token));
        if let Some(token) = client_token {
            self.traffic_mirror_filter_client_tokens
                .insert(token.to_string(), traffic_mirror_filter_id.clone());
        }
        self.traffic_mirror_filters
            .insert(traffic_mirror_filter_id.clone(), filter);
        traffic_mirror_filter_id
    }

    /// Remove the Traffic Mirror filter `traffic_mirror_filter_id` names and answer it as it
    /// was last reported, or `None` when the store holds no such filter.
    ///
    /// The ONE way a filter leaves the store, so a deletion is observable through every path
    /// that resolves a filter id: [`TrafficMirrorFilter`] carries no state member
    /// (`smithy_query(query="shape",shape="TrafficMirrorFilter")`), so there is no terminal
    /// state for a deleted filter to be reported in and the id simply stops resolving.
    ///
    /// The answer is the record the removal took OUT of the store, so what a delete reports
    /// is what the readers reported — never a second copy built beside it. The token that
    /// named this filter stops naming it, so a later create carrying it mints a new filter
    /// rather than reporting one nothing holds, and the bytes the record held go back to the
    /// store's budget on exactly the terms [`Self::insert_traffic_mirror_filter`] took them
    /// on ([`traffic_mirror_filter_payload_bytes`]).
    pub fn remove_traffic_mirror_filter(
        &mut self,
        traffic_mirror_filter_id: &str,
    ) -> Option<TrafficMirrorFilter> {
        let filter = self.traffic_mirror_filters.remove(traffic_mirror_filter_id)?;
        let client_token = self
            .traffic_mirror_filter_client_tokens
            .iter()
            .find(|(_, named)| named.as_str() == traffic_mirror_filter_id)
            .map(|(token, _)| token.clone());
        if let Some(token) = client_token.as_deref() {
            self.traffic_mirror_filter_client_tokens.remove(token);
        }
        self.release_stored_bytes(traffic_mirror_filter_payload_bytes(
            &filter,
            client_token.as_deref(),
        ));
        // A rule lives on the filter it belongs to, so the filter's removal takes its rules
        // with it: their bytes and their client tokens go back on the same terms as the
        // filter's own ([`Self::release_traffic_mirror_filter_rules`]).
        self.release_traffic_mirror_filter_rules(&filter);
        Some(filter)
    }

    /// Add `add` to and remove `remove` from the network services of the filter
    /// `traffic_mirror_filter_id` names, and answer the record as it now stands — `None` when
    /// the store holds no such filter.
    ///
    /// The ONE way a filter's network services change, so the set a reader reports is the set
    /// a write left behind. The member is a SET of modelled enum members
    /// (`TrafficMirrorFilter$NetworkServices`), so naming a service twice, or naming one the
    /// filter already mirrors, leaves one entry and no second copy, and the set is held in the
    /// order the modelled `TrafficMirrorNetworkService` enum declares its members
    /// ([`crate::ops::types::TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES`]) rather than in the order
    /// one request happened to list them. Removals are applied before additions, so a service
    /// a single request both adds and removes is mirrored afterwards: "Use AddNetworkServices
    /// to add network services to a Traffic Mirror filter … use RemoveNetworkServices to
    /// remove the network services" (`ModifyTrafficMirrorFilterNetworkServices`) leaves the
    /// combination undefined, and this is the one reading of it every caller gets.
    ///
    /// Nothing is charged against the store's byte budget: every member of this set is drawn
    /// from a modelled enum, which is bounded already and so uncounted by
    /// [`traffic_mirror_filter_payload_bytes`].
    pub fn modify_traffic_mirror_filter_network_services(
        &mut self,
        traffic_mirror_filter_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Option<TrafficMirrorFilter> {
        let filter = self.traffic_mirror_filters.get_mut(traffic_mirror_filter_id)?;
        let mut network_services: std::collections::BTreeSet<String> = filter
            .network_services
            .take()
            .unwrap_or_default()
            .into_iter()
            .collect();
        for network_service in remove {
            network_services.remove(network_service);
        }
        for network_service in add {
            network_services.insert(network_service.clone());
        }
        filter.network_services = Some(
            crate::ops::types::TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES
                .iter()
                .filter(|declared| network_services.contains(**declared))
                .map(|declared| (*declared).to_string())
                .collect(),
        );
        Some(filter.clone())
    }

    /// The ids of the Traffic Mirror sessions that mirror through the filter
    /// `traffic_mirror_filter_id`, in store order.
    ///
    /// "You cannot delete a Traffic Mirror filter that is in use by a Traffic Mirror session"
    /// (`DeleteTrafficMirrorFilter`): the delete asks this and refuses while the answer is
    /// not empty, so the filter a stored session names is still there for that session to
    /// mirror by.
    pub fn traffic_mirror_sessions_using_filter(
        &self,
        traffic_mirror_filter_id: &str,
    ) -> Vec<String> {
        self.traffic_mirror_sessions
            .iter()
            .filter(|(_, session)| {
                session.traffic_mirror_filter_id.as_deref() == Some(traffic_mirror_filter_id)
            })
            .map(|(session_id, _)| session_id.clone())
            .collect()
    }
}
