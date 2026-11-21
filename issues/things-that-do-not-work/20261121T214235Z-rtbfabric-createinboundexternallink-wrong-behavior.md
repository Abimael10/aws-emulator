Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 344bb9336e0011430569d037e3a195e35382835e

# CreateInboundExternalLink skips the is_external() gate the crate itself states for inbound external links, naming the link under an inbound endpoint the gateway does not have

_Filed: 2026-11-21T21:42:35Z_
_Operation: CreateInboundExternalLink | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `344bb9336e0011430569d037e3a195e35382835e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states in two places that only an `EXTERNAL` responder gateway takes links from outside the fabric, and that the public inbound name is a thing an `INTERNAL` gateway does not have. `CreateResponderGateway` honours that: it only assigns `external_inbound_endpoint` when `gateway.is_external()`, so `GetResponderGateway` reports no `externalInboundEndpoint` for an `INTERNAL` (or untyped) responder gateway. `AssociateCertificate`, the other surface gated on the same rule, reads the same `is_external()` helper and refuses with `CERTIFICATE_EXTERNAL_ONLY_MESSAGE`.

`CreateInboundExternalLink` does not call that helper at all. Its only precondition is that the id names SOME stored responder gateway, and it then assigns the new link a `domainName` built by `inbound_external_link_domain_name`, which is the link id prefixed onto `external_inbound_endpoint(gateway_id, region)` — the very name the crate documents an `INTERNAL` gateway as not having.

The result is two readings of one store that contradict each other: for the same `INTERNAL` responder gateway, `GetResponderGateway` answers with `externalInboundEndpoint` absent, while `GetInboundExternalLink` answers with a `domainName` sitting directly under that absent endpoint. Either the EXTERNAL-only rule the crate states twice holds — and this create must consult `is_external()` as its sibling `AssociateCertificate` does — or the two comments and the conditional in `CreateResponderGateway` are wrong; the crate cannot be right both ways at once.

## Evidence

The rule, stated by the crate, twice:

`services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_responder_gateway.rs:348-354`:
```
        // Only an EXTERNAL gateway accepts inbound external links, so only it has a
        // public inbound endpoint. The type is read off the record through the one
        // reading of it the whole crate shares.
        if gateway.is_external() {
            gateway.external_inbound_endpoint =
                Some(crate::state::external_inbound_endpoint(&gateway_id, &region));
        }
```

`services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:853-857`:
```
/// The public name an `EXTERNAL` responder gateway accepts inbound external
/// links on. An `INTERNAL` gateway has none.
pub fn external_inbound_endpoint(gateway_id: &str, region: &str) -> String {
    format!("{gateway_id}.inbound.rtbfabric.{region}.amazonaws.com")
}
```

The helper the crate shares for the check, `services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:231-237`:
```
    /// Whether this gateway is `EXTERNAL` — the type that takes links from outside
    /// the fabric, and the only one a certificate can be associated with. A gateway
    /// created without a `gatewayType` is not external: the member is optional, and
    /// nothing is assumed for a caller that left it out.
    pub fn is_external(&self) -> bool {
        self.gateway_type.as_deref() == Some(GATEWAY_TYPE_EXTERNAL)
    }
```

The sibling that DOES call it, `services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/associate_certificate.rs:192-202`:
```
        // A certificate is only served to callers outside the fabric, which is what
        // an EXTERNAL gateway is for; the precondition is read off the stored
        // gateway's own type.
        if !gateway.is_external() {
            return Err(AssociateCertificateErrorModeled::ValidationException(
                ValidationException {
                    message: crate::state::CERTIFICATE_EXTERNAL_ONLY_MESSAGE.to_string(),
                },
```

The path that skips it, `services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_inbound_external_link.rs:190-203` — the whole precondition:
```
        // An inbound external link arrives AT a responder gateway — the one the
        // `@http` URI addresses — so it is stored only where that gateway is. A
        // requester gateway of that id is no responder gateway: it opens outbound
        // external links instead.
        if !store.responder_gateways.contains_key(&input.gateway_id) {
            return Err(
                CreateInboundExternalLinkErrorModeled::ResourceNotFoundException(
```
and `services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_inbound_external_link.rs:287-296`, which then names the link under that endpoint regardless of type:
```
        let link = crate::state::InboundExternalLink {
            // The name callers outside the fabric reach THIS link on, assigned by
            // the service: ...
            domain_name: crate::state::inbound_external_link_domain_name(
                &input.gateway_id,
                &link_id,
                &region,
            ),
```
via `services/service-com-amazonaws-rtbfabric-rtbfabric/src/state/external_links.rs:45-54`:
```
pub fn inbound_external_link_domain_name(
    gateway_id: &str,
    link_id: &str,
    region: &str,
) -> String {
    format!(
        "{link_id}.{}",
        super::external_inbound_endpoint(gateway_id, region)
    )
}
```

The contradicting reader, `services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/get_responder_gateway.rs:229`:
```
            external_inbound_endpoint: gateway.external_inbound_endpoint.clone(),
```
(`None` for an `INTERNAL`/untyped gateway, per the conditional quoted above).

Test coverage: `services/service-com-amazonaws-rtbfabric-rtbfabric/tests/all/gateway_link_lifecycle.rs:2626-2667` drives this create from row 0 ("a domain-name gateway of no stated type, no attributes, no tags") and row 2 ("an INTERNAL gateway, error masking for two codes, three tags") and asserts 200 at line 2732. The justification the test states for the `domainName` assertion (lines 2752-2756) is about HOW the gateway is reached — "It is assigned whichever way the gateway itself is reached, so no row can answer it by echoing the gateway's own `domainName`" — and says nothing about gateway TYPE, so it does not reconcile those rows with `create_responder_gateway.rs:348`. By contrast `tests/all/certificate_association.rs:274-309` pins the EXTERNAL-only refusal for the certificate surface across "no gatewayType at all" and `INTERNAL`.

## Steps to Reproduce

1. POST /responder-gateway with a clientToken, vpcId, subnetIds, securityGroupIds, port, protocol, a domainName and "gatewayType": "INTERNAL" — note the minted gatewayId.
2. GET /responder-gateway/{gatewayId} and observe that externalInboundEndpoint is absent (create_responder_gateway.rs:351 only sets it for an EXTERNAL gateway).
3. POST /responder-gateway/{gatewayId}/inbound-external-link with a clientToken and logSettings. It answers 200 instead of consulting is_external() the way AssociateCertificate does on the same gateway.
4. Read the response's domainName: it is "{linkId}.{gatewayId}.inbound.rtbfabric.{region}.amazonaws.com", i.e. a name under the inbound endpoint state.rs:853-854 documents an INTERNAL gateway as not having and that GetResponderGateway just reported absent.
5. Repeat steps 1-3 with "gatewayType": "EXTERNAL" — the same create answers the same way, so the stored gateway's type changes nothing here.
6. For contrast, POST /responder-gateway/{gatewayId}/certificate on the INTERNAL gateway: that surface refuses with CERTIFICATE_EXTERNAL_ONLY_MESSAGE, which is the check this create omits.
