Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: e2c5b0f48e2fab2584b6e4e1c711e4515b8cfc17

# RejectLink lets the gateway that opened the link decline its own request, recording "the peer declined" when the peer never answered

_Filed: 2026-11-23T09:02:49Z_
_Operation: RejectLink | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `e2c5b0f48e2fab2584b6e4e1c711e4515b8cfc17`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`RejectLink` resolves the link with `find_link_mut`, which matches a stored link from EITHER of its two ends, and then moves any link whose request is outstanding to `REJECTED`. It never asks which end the request was put to, although the record holds exactly that (`Link::gateway_id` is the opener, `Link::peer_gateway_id` is the end the request was put to, and the crate already has `Link::other_end` for reading the distinction).

The operation's own modelled documentation says who declines a request: "the responder can use this operation to decline the link request". The crate's own definition of the status the call writes says the same thing: `LINK_STATUS_REJECTED` is "terminal, the peer declined the link request". So after the opening gateway calls its own `/reject`, the store holds a link whose terminal status asserts that the peer declined a request the peer was never asked about, and every later reader (`GetLink`, `ListLinks`, the gateway's `linksRequestedCount`) reports that fiction. The gateway pair is also freed for a fresh `CreateLink` (`link_joining` skips terminal links), so one end can unilaterally clear a pending request out of the other end's way.

The handler words no refusal for this case even though `ConflictException` (409) is among `RejectLink`'s modelled errors and the crate already renders exactly such a refusal for a status that cannot be moved (`Link::status_conflict_message`, used three lines below).

This is the decline half of a defect whose accept half is already filed against `AcceptLink` (20261120T051427Z-rtbfabric-acceptlink-wrong-behavior); the code path, the doc sentence and the status constant quoted here are `RejectLink`'s own and are untouched by that issue.

## Evidence

Smithy, `smithy_query(query="doc", shape="RejectLink")` — verbatim:

> "Rejects a link request between gateways."
> "When a requester gateway requests to link with a responder gateway, the responder can use this operation to decline the link request."

The crate's own definition of the status this call writes, services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:148-149:
```
/// `LinkStatus` member: terminal, the peer declined the link request.
pub const LINK_STATUS_REJECTED: &str = "REJECTED";
```

The handler resolves from either end and asks nothing else, src/ops/reject_link.rs:185-200:
```
        let gateway_id = input.gateway_id.clone();
        let Some(link) = store.find_link_mut(&input.gateway_id, &input.link_id) else {
...
        if link.request_outstanding() {
            // Declining is the end of the link's life: terminal, so nothing
            // further is owed on it and the settle instant is dropped.
            link.status = crate::state::LINK_STATUS_REJECTED.to_string();
```
and `find_link_mut` is end-agnostic, src/state.rs:650-662 + 354-359:
```
    /// Whether `gateway_id` is one of this link's two ends — either end may read
    /// or delete it, so `GET /gateway/{gatewayId}/link/{linkId}` resolves from
    /// the requester's and the responder's id alike.
    pub fn has_end(&self, gateway_id: &str) -> bool {
        self.gateway_id == gateway_id || self.peer_gateway_id == gateway_id
    }
```
The distinction the handler does not use is on the record, src/state.rs:361-371:
```
    /// This link's OTHER end, seen from `gateway_id`: a link is one record with
    /// two ends, so a response addressed to one of them reports the other as its
    /// `peerGatewayId`.
    pub fn other_end(&self, gateway_id: &str) -> &str {
```
and the refusal the crate already words for a status this call cannot move, src/state.rs:408-413 (`status_conflict_message`), is used by this same handler at src/ops/reject_link.rs:204-206.

Test coverage: the existing suite pins the opener's decline — and its own comment concedes the documentation says otherwise. tests/all/link_rejection.rs:237-266:
```
struct Row {
    name: &'static str,
    /// `true` for the gateway the request was put to (the end the operation's
    /// documentation describes as declining it), `false` for the opener.
    from_peer: bool,
    settle_first: bool,
}

const ROWS: &[Row] = &[
...
    Row {
        name: "the opener withdraws an unsettled request",
        from_peer: false,
        settle_first: false,
    },
    Row {
        name: "the opener withdraws a settled request",
        from_peer: false,
        settle_first: true,
    },
];
```
The reason the test states for those two rows is the name "the opener withdraws a request" — it offers no model text, and the comment three lines above names the peer as "the end the operation's documentation describes as declining it", which is the sentence quoted above.

## Steps to Reproduce

1. POST /requester-gateway with {"clientToken":"t-req","vpcId":"vpc-0123456789abcdef0","subnetIds":["subnet-0123456789abcdef0"],"securityGroupIds":["sg-0123456789abcdef0"]} — note the minted gatewayId R.
2. POST /responder-gateway with the same vpc/subnet/security-group members plus {"clientToken":"t-resp","port":443,"protocol":"HTTPS","domainName":"responder.example.com"} — note the minted gatewayId P.
3. POST /gateway/R/create-link with {"peerGatewayId":"P","logSettings":{"applicationLogs":{"sampling":{"errorLog":0,"filterLog":0}}}} — note linkId L; the request is PENDING_REQUEST and then REQUESTED, i.e. outstanding against P.
4. POST /gateway/R/link/L/reject — i.e. the gateway that OPENED the request declines it. Observed: 200 with status REJECTED.
5. GET /gateway/P/link/L — observed: status REJECTED, so P is told that the peer declined a request P was never asked to answer (src/state.rs:148 defines REJECTED as "the peer declined the link request").
6. POST /gateway/R/create-link again with the same peerGatewayId — observed 200: the terminal record no longer occupies the pair, so the opener's self-decline has cleared the outstanding request unilaterally.
7. Expected: the decline is an answer only the end the request was put to can give (RejectLink's documentation: "the responder can use this operation to decline the link request"); a call from the opening end should not write the peer's decline into the record.
