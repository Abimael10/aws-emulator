Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 818e3d78c9e19e331f1b7d6a4463059a9c746980

# The "every not-found path" guard and the constant's own doc both exclude the external-link surface, so five implemented handlers' 404 wording is unguarded

_Filed: 2026-11-23T17:53:54Z_
_Operation: GetOutboundExternalLink | Class: error-response_

> _Note: this issue was identified against the code base at commit `818e3d78c9e19e331f1b7d6a4463059a9c746980`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states the not-found rule in exactly one place and guards it with exactly one suite, and BOTH of those statements now exclude a whole surface the crate serves.

`RESOURCE_NOT_FOUND_MESSAGE`'s own doc enumerates the paths it covers — "a gateway of either kind, a link, a routing rule, a certificate association" — and the end-to-end suite that pins the wording says its table spans "every operation of this service that can answer not-found". Neither enumeration mentions the two external-link surfaces (`/requester-gateway/{gatewayId}/outbound-external-link`, `/responder-gateway/{gatewayId}/inbound-external-link`), although five implemented handlers on them construct `ResourceNotFoundException` with that very constant: `GetOutboundExternalLink`, `GetInboundExternalLink`, `DeleteOutboundExternalLink`, `CreateOutboundExternalLink` and `CreateInboundExternalLink`. Five further implemented handlers that answer the same refusal are also absent from the table (`ListLinks`, `UpdateLink`, `DeleteLinkRoutingRule`, `UpdateRequesterGateway`, `UpdateResponderGateway`).

So the one regression guard for "the refusal says the same thing whichever resource was addressed" asserts nothing about ten of the operations that give it, and the constant's doc — the crate's only statement of which paths are in scope — is false as written: it lists four kinds of resource where the store now holds six. A later change to any external-link handler's refusal (a message that echoes the gatewayId or linkId, a different modelled error, a 500 from the tagging channel) would pass the whole suite, which is exactly what the suite exists to prevent.

The fix is confined to the two statements of the rule: extend the constant's doc to the resources the store actually holds, and add the omitted calls to `calls(...)` in the suite. No handler response changes and no existing assertion changes — every omitted handler already answers 404 `ResourceNotFoundException` with `RESOURCE_NOT_FOUND_MESSAGE`, so the added rows pass as written.

## Evidence

The rule, and its enumeration of the paths it covers — services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:122-129:
```
/// The message this service's `ResourceNotFoundException` carries: fixed text
/// naming neither the resource asked for nor its id.
///
/// Every not-found path of this service answers with it — a gateway of either
/// kind, a link, a routing rule, a certificate association — so the refusal says
/// the same thing whichever resource was addressed, and no path echoes an id the
/// caller supplied back at them.
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "The requested resource was not found.";
```

The guard that claims to span every such call — tests/all/not_found_message_wording.rs:11-14:
```
//! The table spans what the rule generalises over: every operation of this service
//! that can answer not-found (gateway reads, deletes, the link surface, the
//! routing-rule surface and the certificate surface), each addressed with ids that
//! satisfy the modelled patterns so the existence check is what refuses, and with ids
```
and the table's own doc, tests/all/not_found_message_wording.rs:115-118:
```
/// Every call of this service that can answer not-found, for one (gateway, link,
/// rule) triple nothing minted: the operation's name, its method, its path and its
/// body.
fn calls(gateway_id: &str, link_id: &str, rule_id: &str) -> Vec<(String, &'static str, String, Option<Value>)> {
```
The table it introduces (tests/all/not_found_message_wording.rs:123-207) holds exactly 14 rows: GetRequesterGateway, DeleteRequesterGateway, GetResponderGateway, DeleteResponderGateway, GetLink, DeleteLink, RejectLink, AcceptLink, CreateLink, GetLinkRoutingRule, CreateLinkRoutingRule, AssociateCertificate, GetCertificateAssociation, DisassociateCertificate — and nothing else; the assertions it then makes are the only ones that pin this wording, tests/all/not_found_message_wording.rs:221-230 (`status, 404` / `code, "ResourceNotFoundException"` / `message`).

The implemented handlers that answer the same refusal and appear in neither enumeration:

src/ops/get_outbound_external_link.rs:185-193:
```
        let Some(link) = store.find_outbound_external_link(&input.gateway_id, &input.link_id)
        else {
            return Err(GetOutboundExternalLinkErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string(),
                },
            )
            .into());
        };
```
src/ops/get_inbound_external_link.rs:185-193 (the same block for the other direction), src/ops/delete_outbound_external_link.rs:171-182, src/ops/create_outbound_external_link.rs:193-202 ("A responder gateway of that id is no requester gateway"), src/ops/create_inbound_external_link.rs:194-203 ("A requester gateway of that id is no responder gateway"), src/ops/list_links.rs:180-187, src/ops/update_link.rs:176-183, src/ops/delete_link_routing_rule.rs:167-176, src/ops/update_requester_gateway.rs:195-204 and src/ops/update_responder_gateway.rs:245-252 — ten handlers in all, each constructing `ResourceNotFoundException { message: crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string() }`.

Test coverage: the external-link surfaces' own suite asserts the not-found wording only for the pair-resolution rows it already drives (tests/all/gateway_link_lifecycle.rs:2498-2506 and 3358-3365 compare `text(&refused, "message")` with `RESOURCE_NOT_FOUND_MESSAGE` for the read path), and nothing anywhere pins it for `DeleteOutboundExternalLink`, `CreateOutboundExternalLink`, `CreateInboundExternalLink`, `ListLinks`, `UpdateLink`, `DeleteLinkRoutingRule` or the two gateway updates.

## Steps to Reproduce

1. Read src/state.rs:122-129: the constant's doc lists the resources whose not-found paths it covers — a gateway of either kind, a link, a routing rule, a certificate association.
2. Read tests/all/not_found_message_wording.rs:11-14 and 115-118: the suite states its table spans "every operation of this service that can answer not-found".
3. Enumerate the table's rows at tests/all/not_found_message_wording.rs:123-207 — 14 calls, none of them on an external-link surface.
4. grep -n 'RESOURCE_NOT_FOUND_MESSAGE' src/ops/*.rs: fifteen implemented handlers answer with it, including src/ops/get_outbound_external_link.rs:187-192, src/ops/get_inbound_external_link.rs:187-192, src/ops/delete_outbound_external_link.rs:174-181, src/ops/create_outbound_external_link.rs:194-201, src/ops/create_inbound_external_link.rs:195-202, src/ops/list_links.rs:181-186, src/ops/update_link.rs:177-182, src/ops/delete_link_routing_rule.rs:170-175, src/ops/update_requester_gateway.rs:196-203 and src/ops/update_responder_gateway.rs:246-251 — ten calls the table omits.
5. Confirm the omission is not a modelling artefact: GET /requester-gateway/rtb-gw-a/outbound-external-link/link-a against an empty store answers 404 ResourceNotFoundException with "The requested resource was not found.", i.e. it is a not-found path of this service that both statements of the rule leave out.
6. Expected: the constant's doc names the resources the store actually holds (the external links included), and the suite's table carries a row for every operation that can answer not-found, so the one fixed wording stays guarded for all of them.
