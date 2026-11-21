Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: d3d6261999f9a11a24fded61323b830533f5b9d0

# A ListLinks nextToken is not bound to the gateway whose walk issued it, so replaying one on another gateway's listing silently skips links instead of being refused

_Filed: 2026-11-19T07:24:23Z_
_Operation: ListLinks | Class: pagination_

> _Note: this issue was identified against the code base at commit `d3d6261999f9a11a24fded61323b830533f5b9d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

ListLinks walks one gateway's links (the records `has_end(gatewayId)` is true for), ordered by `linkId`, and names the page boundary with a cursor that is the last entry's `linkId`. The token it hands out carries only the OPERATION name and that cursor — never the `gatewayId` the walk was scoped to — and `decode_next_token` only checks that the operation inside the token matches. A token issued by `GET /gateway/{A}/links/` is therefore accepted unchanged by `GET /gateway/{B}/links/`, where it is used as a cursor into B's own enumeration: every link of B whose `linkId` sorts at or below A's cursor is silently dropped from the answer, and when all of B's links sort below that cursor the call answers an empty `links` list and no `nextToken` — i.e. "gateway B has no links" — with HTTP 200.

The crate already refuses every other token that the addressed walk did not issue: a token from another listing of this service, a bare cursor, non-hex, an over-long token and an expired one are all 400. The one case left accepted is the one this operation itself can hand out, because the cursor's scope (`Self::NAME`) does not identify the walk when the walk is per-gateway. ListLinks is the only parent-scoped listing of this service that is implemented (ListLinkRoutingRules and ListCertificateAssociations are still scaffolds), so it is the only place the hole is reachable today.

## Evidence

Token issued with the operation name and the link id only — services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/list_links.rs:232-239:
```
        // A token only when links remain after this page, so a caller stops paging
        // exactly when the enumeration is exhausted.
        let next_token = (end < mine.len())
            .then(|| {
                page.last()
                    .map(|last| crate::state::encode_next_token(Self::NAME, &last.link_id, now))
            })
            .flatten();
```
The walk the cursor indexes into is per-GATEWAY — list_links.rs:214-227:
```
        // Ordered by `linkId`. An id is unique, so that is a total order over the
        // records this gateway is an end of: a page boundary is stable across
        // calls, and a walk neither repeats a link nor skips one.
        let mut mine: Vec<&crate::state::Link> = store
            .links
            .values()
            .filter(|link| link.has_end(&input.gateway_id))
            .collect();
        mine.sort_unstable_by(|left, right| left.link_id.cmp(&right.link_id));

        let start = match resume_after.as_deref() {
            Some(cursor) => mine.partition_point(|link| link.link_id.as_str() <= cursor),
            None => 0,
        };
```
The only scope check the codec makes is the operation name — src/state/page_tokens.rs:96-98:
```
    if scope != operation || cursor.is_empty() {
        return Err(TokenRefusal::Unrecognised);
    }
```
and its stated intent, page_tokens.rs:56-61: "Two things travel with it: the operation that issued it, so a token minted by one listing is not resumed from by another, and the issuing instant, because the token's usable span is counted from when it was handed out."

Test coverage — the rule is pinned only for tokens from a DIFFERENT operation: tests/all/gateway_link_lifecycle.rs:1782-1790 `a_link_listing_token_this_operation_never_issued_is_refused`, whose comment reads "A token another listing of this service issued is not this walk's to resume from: the issuing operation travels inside the token." Its six rows (list at lines 1816-1827: "not hex at all", "an odd number of hex digits", "hex carrying no cursor", "hex of a bare link id", "far longer than any issued token", "another listing's token") all assert 400, and the closing case at 1842-1847 pins that "Its own token still resumes the walk". No row replays a token issued by ANOTHER GATEWAY's ListLinks walk, so nothing pins the current silent-skip answer.

## Steps to Reproduce

1. Create a requester gateway R and two responder gateways S1 and S2 (POST /requester-gateway, POST /responder-gateway).
2. Open two links from R: POST /gateway/{R}/create-link with peerGatewayId S1, then with peerGatewayId S2. Note the two minted link ids; the second is the higher one (ids are minted from a monotonic counter), call them L1 (on S1) and L2 (on S2).
3. GET /gateway/{S2}/links/?maxResults=1 — answers 200 with the single entry L2 and, because nothing remains, no nextToken. To obtain a token whose cursor is L2, instead list from R: GET /gateway/{R}/links/?maxResults=2 pages R's two links; take the nextToken from GET /gateway/{R}/links/?maxResults=1 after the SECOND link was created so the cursor is L1... simplest form: GET /gateway/{R}/links/?maxResults=2 is exhausted, so use GET /gateway/{R}/links/?maxResults=1 twice to get a token whose cursor is L1.
4. Replay that token on the OTHER gateway's listing: GET /gateway/{S1}/links/?nextToken=<token>.
5. Observed: HTTP 200 with "links": [] and no nextToken — S1's only link L1 is silently skipped, because the cursor L1 was compared against S1's own enumeration. Expected: the same 400 the handler already gives a token this walk never issued (tests/all/gateway_link_lifecycle.rs:1816-1840), since the token belongs to gateway R's walk, not S1's.
