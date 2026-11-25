Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 4d8b014dbcb3833e6a7aad4896bf06c6aff71e41

# GetLink reports the stored orientation instead of the addressed end, so it names the calling gateway as its own peerGatewayId

_Filed: 2026-11-22T14:19:14Z_
_Operation: GetLink | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `4d8b014dbcb3833e6a7aad4896bf06c6aff71e41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A link is one record with two ends and `GET /gateway/{gatewayId}/link/{linkId}` resolves from either end (`Link::has_end`). The crate states, in `state.rs`, the rule that a response addressed to one end reports the OTHER end as `peerGatewayId`, and it ships the helper `Link::other_end` to do exactly that. `AcceptLink` and `RejectLink` both call that helper and report `gatewayId` = the gateway in the URI. `GetLink` calls neither: it copies `link.gateway_id` and `link.peer_gateway_id` straight out of the record.

The consequence is that when the link is read from the end that did NOT open it, `GetLink` answers `gatewayId` = the gateway the caller did not address and `peerGatewayId` = the caller's own gateway id — i.e. the responder is reported as its own peer. The same link, addressed from the same gateway, is described with the opposite orientation by `AcceptLink`/`RejectLink` (and `GetLinkRoutingRule` likewise reports the gateway "as addressed"). Two reads of one resource therefore disagree about which gateway is the peer.

`ListLinks`'s `described()` helper copies the same stored orientation, so the listing of the responder's links has the same inversion; the single rule stated in `state.rs` is honoured by two handlers and skipped by two.

## Evidence

The rule, stated in this crate's state module — `services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:361-364`:

    /// This link's OTHER end, seen from `gateway_id`: a link is one record with
    /// two ends, so a response addressed to one of them reports the other as its
    /// `peerGatewayId`. An id that is neither end never reaches here, because the
    /// link is resolved by [`has_end`](Self::has_end) first.
    pub fn other_end(&self, gateway_id: &str) -> &str {

The siblings that honour it — `src/ops/accept_link.rs:226-230`:

        Ok(AcceptLinkOutput {
            // The response describes the link as addressed: the gateway in the
            // URI, and the link's other end as its peer.
            gateway_id,
            peer_gateway_id: link.other_end(&input.gateway_id).to_string(),

and `src/ops/reject_link.rs:210-214`:

        Ok(RejectLinkOutput {
            // The response describes the link as addressed: the gateway in the
            // URI, and the link's other end as its peer.
            gateway_id,
            peer_gateway_id: link.other_end(&input.gateway_id).to_string(),

The handler that skips it — `src/ops/get_link.rs:198-201` (no mention of `input.gateway_id` at all):

        Ok(GetLinkOutput {
            gateway_id: link.gateway_id.clone(),
            peer_gateway_id: link.peer_gateway_id.clone(),
            status: link.status.clone(),

and `src/ops/list_links.rs:264-268`:

    fn described(link: &crate::state::Link) -> ListLinksResponseStructure {
        ListLinksResponseStructure {
            gateway_id: link.gateway_id.clone(),
            peer_gateway_id: link.peer_gateway_id.clone(),

For contrast, the sibling read on the same URI family DOES report the addressed gateway — `src/ops/get_link_routing_rule.rs:191-195`:

        Ok(GetLinkRoutingRuleOutput {
            // The rule as addressed: the gateway in the URI, and the link the rule
            // routes for.
            gateway_id: input.gateway_id.clone(),

Test coverage, which asserts BOTH rules against the same resource: `tests/all/link_rejection.rs:325-336` pins the addressed-end rule for RejectLink —

        assert_eq!(
            text(&rejected, "gatewayId").as_str(),
            addressed.as_str(),
            "case {case:?}: expected the addressed gateway {addressed:?} as gatewayId, got {:?}",
...
            "case {case:?}: expected the other end {other:?} as peerGatewayId, got {:?}",

(and `tests/all/link_rejection.rs:607-610` the same for AcceptLink), while `tests/all/gateway_link_lifecycle.rs:712-716` pins the current, contradictory GetLink answer —

        assert_eq!(
            from_peer["peerGatewayId"], read_link["peerGatewayId"],
            "case {case:?}: expected the stored orientation from either end, got {:?}",

That test states no reason for preferring the stored orientation, and the state-module doc quoted above states the opposite rule for "a response addressed to one of them".

## Steps to Reproduce

1. POST /requester-gateway with a valid clientToken/vpcId/subnetIds/securityGroupIds -> note the requester gatewayId A
2. POST /responder-gateway likewise -> note the responder gatewayId B
3. POST /gateway/A/create-link with body {"peerGatewayId":"B","logSettings":{...}} -> note linkId L
4. GET /gateway/B/link/L — observe the response reports gatewayId=A and peerGatewayId=B, i.e. the gateway the caller addressed is named as its own peer
5. POST /gateway/B/link/L/accept with {"logSettings":{...}} — observe the response for the SAME link addressed from the SAME gateway reports gatewayId=B and peerGatewayId=A
6. Compare the two responses: they describe one link with opposite orientation; GET /gateway/B/links/ shows the same inversion in its entry for L
