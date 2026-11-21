Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 41754a0a77d48fd39f1ad4499b0c7fe266f2fb18

# The /tags/{resourceArn} surface cannot address an outbound external link, so the tags its create assigned can never be listed, added to or removed

_Filed: 2026-11-21T07:47:21Z_
_Operation: UntagResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `41754a0a77d48fd39f1ad4499b0c7fe266f2fb18`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateOutboundExternalLink` stores a caller's `tags` map on the `OutboundExternalLink` record it writes, and it even enforces the per-resource tag bound ON THE GROUND that this is the resource `TagResource` governs ("the same per-resource bound `TagResource` enforces is enforced here: a resource holds at most that many tags whichever call assigned them"). But the shared resolver behind the `/tags/{resourceArn}` surface — `crate::state::tagged_resource` + `resource_tags`/`resource_tags_mut`, which all three handlers of that surface call — can only reach `state.links` (links between two gateways), never `state.outbound_external_links`. The ARN grammar the surface accepts is the same for both kinds of link (`gateway/<gatewayId>/link/<linkId>`), so a caller addressing an outbound external link's own ARN gets the resolver's "this store holds no such record" answer: HTTP 500 `InternalServerErrorException` / "Internal server error.".

Result: a tag map that `CreateOutboundExternalLink` persisted and `GetOutboundExternalLink` reports back is write-once and unmanageable — `UntagResource` cannot remove a key from it, `TagResource` cannot add one, and `ListTagsForResource` cannot report it, even though the crate's own tagging module states the opposite rule for every taggable record it holds.

The finding is in the shared `state::tagging` resolver that `UntagResource`, `TagResource` and `ListTagsForResource` all call; it is named here under `UntagResource` because the already-filed `TagResource`/wrong-behavior (region scoping) and `ListTagsForResource`/wrong-behavior (the surface never settles the clock) issues are different symptoms of different causes.

## Evidence

The create ties its own tag bound to the TagResource surface — services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_outbound_external_link.rs:269-283:
```
        // The tags the create assigns are held in the record it writes, so the
        // same per-resource bound `TagResource` enforces is enforced here: a
        // resource holds at most that many tags whichever call assigned them.
        let tag_count = input.tags.as_ref().map_or(0, std::collections::HashMap::len);
        if tag_count > crate::state::TAGS_PER_RESOURCE_QUOTA {
```
and the record it writes carries the map — src/state/external_links.rs:32-52:
```
/// Holds the members `CreateOutboundExternalLink` takes but its response shape does
/// not carry (`publicEndpoint`, `attributes`, `logSettings`, `tags`):
...
    pub tags: HashMap<String, String>,
```

The tagging module's own rule — src/state/tagging.rs:12-17:
```
//! Tags are not a store of their own — every taggable record already carries its own
//! `tags` map, written by the create that minted it — so this module only addresses
//! them. That is also what keeps the surface bounded: a tag map lives and dies with
//! its resource, and the resources are bounded by
//! [`GATEWAY_QUOTA`](super::GATEWAY_QUOTA) and [`LINK_QUOTA`](super::LINK_QUOTA),
```
and `LINK_QUOTA` is the bound that covers BOTH link maps — src/state.rs:497-502:
```
    /// How many links this store holds in all: those between two gateways and those
    /// to an endpoint outside the fabric. One bound covers both, because a link of
    /// either kind is a link the store holds ([`LINK_QUOTA`]).
    pub fn links_held(&self) -> usize {
        self.links.len().saturating_add(self.outbound_external_links.len())
```

What the resolver can actually reach — src/state/tagging.rs:165-168 (and the identical write side at tagging.rs:200-205):
```
        TaggedResource::Link {
            gateway_id,
            link_id,
        } => state.find_link(gateway_id, link_id).map(|link| &link.tags),
```
where `find_link` scans only the gateway-to-gateway map — src/state.rs:521-525:
```
    pub fn find_link(&self, gateway_id: &str, link_id: &str) -> Option<&Link> {
        self.links
            .values()
            .find(|link| link.link_id == link_id && link.has_end(gateway_id))
    }
```
so the external link's ARN falls into the unresolved refusal — src/state/tagging.rs:55-72:
```
pub const TAGGING_UNRESOLVED_CODE: &str = "InternalServerErrorException";
...
pub const TAGGING_UNRESOLVED_MESSAGE: &str = "Internal server error.";
```
reached from the handler at src/ops/untag_resource.rs:155-158:
```
        let resource = crate::state::tagged_resource(&input.resource_arn, &region)
            .ok_or_else(crate::state::tagging_unresolved)?;
        let tags = crate::state::resource_tags_mut(&mut store, resource)
            .ok_or_else(crate::state::tagging_unresolved)?;
```

Test coverage: the external-link tag bound is pinned at tests/all/gateway_link_lifecycle.rs:2501-2529 with the same claim ("A create may assign at most as many tags as a resource holds — the same per-resource bound `TagResource` enforces"), and the link's stored tags are read back through `GetOutboundExternalLink` (tests/all/gateway_link_lifecycle.rs:2533-2540). No test in tests/all/resource_tagging.rs or anywhere else addresses an outbound external link through `/tags/{resourceArn}` (a grep of tests/ for `outbound-external-link` matches only gateway_link_lifecycle.rs), so nothing pins the current refusal.

## Steps to Reproduce

1. POST /requester-gateway with a clientToken, vpcId, subnetIds and securityGroupIds — note the requester gatewayId R.
2. POST /requester-gateway/R/outbound-external-link with {"clientToken":"t1","publicEndpoint":"https://bid.example.com","logSettings":{"applicationLogs":{"sampling":{"errorLog":0,"filterLog":0}}},"tags":{"tier":"gold"}} — note the linkId L.
3. GET /requester-gateway/R/outbound-external-link/L — HTTP 200 reporting tags {"tier":"gold"}, i.e. the record really holds the tag map.
4. GET /tags/arn:aws:rtbfabric:us-east-1:<emulator account>:gateway/R/link/L — observed HTTP 500 InternalServerErrorException "Internal server error." (the resolver's no-such-record refusal); expected the tags the create assigned, as for every other taggable record of this crate.
5. DELETE /tags/arn:aws:rtbfabric:us-east-1:<emulator account>:gateway/R/link/L?tagKeys=tier — observed the same HTTP 500, so the tag can never be removed; POST to the same path is refused identically, so it can never be changed either.
