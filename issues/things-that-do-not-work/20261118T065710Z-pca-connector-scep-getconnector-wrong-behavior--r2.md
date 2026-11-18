Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 85d805405eee386b5837c9b46d90be46bccf5f7f

# The SCEP plane serves a connector at a host it never published: the access-path form and the VPC endpoint label are discarded, not matched

_Filed: 2026-11-18T06:57:10Z_
_Operation: GetConnector | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `85d805405eee386b5837c9b46d90be46bccf5f7f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`Connector$Endpoint` is minted from the access path the create chose: a connector created without `VpcEndpointId` is published at `https://<id>.scep.pca-connector-scep.<region>.amazonaws.com`, and one created with a `VpcEndpointId` at `https://<id>.scep.<vpce-id>.pca-connector-scep.<region>.vpce.amazonaws.com` (`crate::state::connector_endpoint`). The reverse reading that CLAIMS and SERVES that plane (`crate::state::connector_addressed_by_host`, used by `service::scep::claims_at` and `service::scep::addressed_connector`) documents itself as the inverse of that function, but it is not: it accepts EITHER suffix for ANY connector and, in the PrivateLink form, accepts any non-empty dotless label in the VPC-endpoint position without comparing it to anything. What it hands back is only `AddressedConnector { region, connector_id }`, and `ScepDataPlane::serve` then resolves the connector by ARN and checks only `is_active()` — the record's stored `vpc_endpoint_id` is never consulted on the serving path.

Consequences, all of them answered 200 with the private CA's DER certificate:

* a connector created with `VpcEndpointId` — which the model says is "accessible only through that specific VPC endpoint" — serves `GetCACert` at the PUBLIC host form, an address this service never published for it;
* the same connector serves at a PrivateLink host carrying a DIFFERENT (or entirely made-up) endpoint id, e.g. `<id>.scep.bogus.pca-connector-scep.<region>.vpce.amazonaws.com`;
* a public connector serves at a PrivateLink host it was never published at.

So the access-path choice the create stores has no effect on which addresses the data plane answers at, even though the crate already owns the function that computes the one host it published for that connector. This is NOT the already-fixed "VpcEndpointId has no observable effect" gap about the REPORTED `Endpoint` value: the reported value does differ, and must keep differing — the gap is on the claim/serve side, which ignores it.

## Evidence

Model, CreateConnectorRequest$VpcEndpointId (smithy_query query="doc" shape="CreateConnectorRequest" member="VpcEndpointId"):
> "If you don't supply a value, by default Connector for SCEP creates a connector accessible over the public internet. If you provide a VPC endpoint ID, creates a connector accessible only through that specific VPC endpoint."

The crate's own statement of that rule on the MINTING side — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/state.rs:1344-1349:
```
/// The access path the create chose is part of that derivation, because it is what makes the
/// URL reachable: with no VPC endpoint id the connector is "accessible over the public
/// internet" (the `VpcEndpointId` member's own doc) and the host sits under the public DNS
/// suffix; with one, the connector is "accessible only through that specific VPC endpoint",
/// so the host is qualified by that endpoint's id and sits under the PrivateLink suffix
/// instead. Two connectors that differ only in that choice therefore report different hosts.
```
and the two forms it builds — state.rs:1353-1365:
```
pub fn connector_endpoint(region, connector_id, vpc_endpoint_id) -> String {
    match vpc_endpoint_id {
        Some(vpc_endpoint_id) => format!("https://{connector_id}.{SCEP_HOST_LABEL}.{vpc_endpoint_id}.{ARN_SERVICE}.{region}.{PRIVATE_DNS_SUFFIX}"),
        None => format!("https://{connector_id}.{SCEP_HOST_LABEL}.{ARN_SERVICE}.{region}.{PUBLIC_DNS_SUFFIX}"),
```

The CLAIMING side, which says it is the inverse — state.rs:1515-1521:
```
/// The connector a published SCEP HOST names, or `None` when the host is not one
/// [`connector_endpoint`] builds.
///
/// The inverse of that function — both of its forms, public and PrivateLink — so a client
/// that DOES resolve AWS's own hostnames reaches the same plane at the same address this
/// service published to it.
```
but the code under that comment accepts either suffix for any connector and never checks WHICH endpoint id (if any) the connector was published under — state.rs:1526-1547:
```
    let body = host
        .strip_suffix(&format!(".{PRIVATE_DNS_SUFFIX}"))
        .or_else(|| host.strip_suffix(&format!(".{PUBLIC_DNS_SUFFIX}")))?;
...
    let (connector_id, labels) = head.split_once('.')?;
    let on_this_plane = labels == SCEP_HOST_LABEL
        || labels
            .strip_prefix(SCEP_HOST_LABEL)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|vpc_endpoint_id| {
                !vpc_endpoint_id.is_empty() && !vpc_endpoint_id.contains('.')
            });
    (on_this_plane && !connector_id.is_empty()).then(|| AddressedConnector {
        region: region.to_string(),
        connector_id: connector_id.to_string(),
    })
```

The serving path never consults the record's access path — src/service/scep.rs:172-184:
```
            let Some(connector) = store.connector(&addressed.region, &connector_arn) else {
                return refused(NOT_FOUND_STATUS, "This URL names no SCEP connector.");
            };
            connector
                .is_active()
                .then(|| connector.certificate_authority_arn.clone())
```
even though the record carries it — state.rs:288-289:
```
    /// The VPC endpoint the connector is reachable through, when the create named one.
    pub vpc_endpoint_id: Option<String>,
```

Test coverage: tests/all/connector_endpoint_host_routing.rs:500-515 `host_rows()` has exactly two rows ("the public SCEP host of a connector reachable over the internet", vpc_endpoint_id: None; "the endpoint-qualified host of a connector reachable through a VPC endpoint", vpc_endpoint_id: Some("vpce-0123abcd456789abc")), and each dials the endpoint that connector itself published (`reported_endpoint`, :400-423). No case in the crate dials the OTHER form, or a mismatched vpce label, so nothing pins the current cross-form behaviour.

## Steps to Reproduce

1. Build a router with this service and Amazon Web Services Private CA on one shared engine, as tests/all/connector_endpoint_host_routing.rs:91-102 does, and drive a private CA to ACTIVE.
2. POST /connectors with {"CertificateAuthorityArn": <that CA>, "VpcEndpointId": "vpce-0123abcd456789abc"} and release the pending transition so the connector is ACTIVE.
3. GET /connectors/{arn} and note the published Endpoint: https://<id>.scep.vpce-0123abcd456789abc.pca-connector-scep.<region>.vpce.amazonaws.com.
4. Dispatch GET https://<id>.scep.pca-connector-scep.<region>.amazonaws.com?operation=GetCACert — the PUBLIC host form, which this connector was never published at — and observe 200 with content-type application/x-x509-ca-cert and the CA's DER certificate.
5. Dispatch GET https://<id>.scep.vpce-ffffffffffffffff.pca-connector-scep.<region>.vpce.amazonaws.com?operation=GetCACert (a different VPC endpoint id) and observe the same 200 + certificate.
6. Create a second connector with NO VpcEndpointId and dispatch GET https://<id2>.scep.bogus.pca-connector-scep.<region>.vpce.amazonaws.com?operation=GetCACert: again 200, at a PrivateLink address that connector was never published at.
