Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: db4cad1c26cf8d6e381b1c5a713894234123e1ec

# AcceptLink lets the gateway that opened the link accept its own request, recording "the peer has accepted" when the peer never answered

_Filed: 2026-11-20T05:14:27Z_
_Operation: AcceptLink | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `db4cad1c26cf8d6e381b1c5a713894234123e1ec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`AcceptLink` resolves the link with `store.find_link_mut(&input.gateway_id, &input.link_id)`, which matches on `Link::has_end` — i.e. EITHER of the link's two ends. The handler then moves any link whose request is still outstanding to `ACCEPTED` (and the clock on to `ACTIVE`) without asking whether the caller is the end the request was PUT TO. So the gateway that created the link can accept its own outstanding request: the link is driven to `ACTIVE` and reported to both ends as established, although the peer never answered.

That contradicts the crate's own definitions of exactly these two things. `Link::request_outstanding` — the predicate this handler uses as its gate — is documented as the set of statuses "from which the peer can still accept or decline the request", and the constant the handler writes, `LINK_STATUS_ACCEPTED`, is documented as "the peer has accepted the request and the link is coming up". After a self-accept the record holds that status while the peer has done nothing, so every later `GetLink`/`ListLinks` reader is told the peer accepted a link it never saw. The operation's own modelled documentation (reproduced in the handler header) likewise states that it is the responder that uses this operation to accept the request and establish the connection.

Note the contrast with `RejectLink`, where the opener addressing its own outstanding request has a coherent reading the crate's tests name — "the opener withdraws a settled request" (tests/all/link_rejection.rs:256-265). There is no corresponding reading of an opener ACCEPTING its own request: the accept is the peer's answer, and no other end can give it.

## Evidence

Smithy, `smithy_query(query="doc", shape="AcceptLink")` — verbatim: "Accepts a link request between gateways. When a requester gateway requests to link with a responder gateway, the responder can use this operation to accept the link request and establish the connection." (same sentence reproduced in the handler header, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/accept_link.rs:51-53).

The gate the handler uses, services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:300-305:
```
    /// Whether the link request is still outstanding — accepted by this service
    /// but not yet answered by the peer. These are the only statuses from which
    /// the peer can still accept or decline the request.
    pub fn request_outstanding(&self) -> bool {
        self.status == LINK_STATUS_PENDING_REQUEST || self.status == LINK_STATUS_REQUESTED
    }
```
The status it writes, src/state.rs:121-123:
```
/// `LinkStatus` member: the peer has accepted the request and the link is
/// coming up.
pub const LINK_STATUS_ACCEPTED: &str = "ACCEPTED";
```
The handler, src/ops/accept_link.rs:191 and 200-205 — resolution is from EITHER end and no caller check follows:
```
        let Some(link) = store.find_link_mut(&input.gateway_id, &input.link_id) else {
...
        if link.request_outstanding() {
            // The request is answered: the link is accepted now, and coming up
            // is what the clock still owes it.
            link.status = crate::state::LINK_STATUS_ACCEPTED.to_string();
```
and the resolver it relies on, src/state.rs:284-286:
```
    pub fn has_end(&self, gateway_id: &str) -> bool {
        self.gateway_id == gateway_id || self.peer_gateway_id == gateway_id
    }
```
Test coverage: tests/all/link_rejection.rs:535-542 pins the self-accept as a 200 (`name: "the opener accepts its own settled request, stating the timeout only", from_peer: false`), and the struct field it sets concedes the model says otherwise — tests/all/link_rejection.rs:495-497: "`true` for the gateway the request was put to (the end the operation's documentation describes as accepting it), `false` for the opener." No reason is stated for admitting the opener; the only justification offered anywhere is for the reject path's "the opener withdraws" rows (link_rejection.rs:256-265).

## Steps to Reproduce

1. POST /requester-gateway with a clientToken, vpcId, subnetIds, securityGroupIds — note the returned requester gatewayId R.
2. POST /responder-gateway with a clientToken and a domainName — note the returned responder gatewayId P.
3. POST /gateway/R/create-link with {"peerGatewayId": P, "logSettings": {"applicationLogs":{"sampling":{"errorLog":0,"filterLog":0}}}} — note linkId L; the link is PENDING_REQUEST, and the clock owes it REQUESTED.
4. POST /gateway/R/link/L/accept (the OPENER's own end) with the same logSettings body.
5. Observed: HTTP 200 with status ACCEPTED, and after the provisioning delay GET /gateway/R/link/L reports ACTIVE — a link established without the peer P ever answering, while the crate defines ACCEPTED as "the peer has accepted the request".
6. Expected per the operation's documentation and the crate's own definitions: only the end the request was put to (P) can accept it; R's accept of its own outstanding request is not the peer's answer.
