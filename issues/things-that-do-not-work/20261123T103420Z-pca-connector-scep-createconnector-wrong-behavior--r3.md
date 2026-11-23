Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: bdbdda3693450031078273d1b3f99b664b354563

# VpcEndpointId is stored but never governs reachability: the announced Endpoint drops it on the emulator's own endpoint, and the SCEP plane serves every connector at both host forms

_Filed: 2026-11-23T10:34:20Z_
_Operation: CreateConnector | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `bdbdda3693450031078273d1b3f99b664b354563`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateConnector` reads `VpcEndpointId`, resolves it against EC2, stores it on the connector record and uses it once to build a published host — but nothing afterwards ever consults it, so the access path the create chose is not the access path the connector is reachable at. Two concrete symptoms of the one defect:

1. **The announced `Endpoint` loses the access path whenever the emulator is served on a port.** `announced_connector_endpoint` takes `vpc_endpoint_id` and forwards it only on the `connector_endpoint` branch; the `local_connector_endpoint` branch — the branch taken for every request that reached the emulator's own endpoint, or on any emulator served on a port, i.e. the normal deployment — drops the argument entirely. So in a served emulator a connector created with `VpcEndpointId` and one created without it report URLs identical but for the connector id, although `local_connector_endpoint`'s own doc claims it "carries what the published host carries" and `connector_endpoint`'s doc states "Two connectors that differ only in that choice therefore report different hosts." The crate's own test asserts that rule and only ever exercises the unserved router, so the published-host branch is the only one it reaches.

2. **The SCEP data plane serves a connector at an address this service never published for it.** `connector_addressed_by_host` extracts only the Region and the connector id: it accepts the public host form for a VPC-restricted connector, and accepts the PrivateLink form with ANY non-empty dotless label where the VPC endpoint id goes (`!vpc_endpoint_id.is_empty() && !vpc_endpoint_id.contains('.')`), discarding the label without comparing it to anything. `ScepDataPlane::serve` then looks the connector up by Region + id and serves it on the sole condition that it `is_active()`; `StoredConnector::vpc_endpoint_id` is never read (the field has exactly one write site and no read site in `src/`). So a connector created with `VpcEndpointId=vpce-aaa` is served its CA certificate at `https://<id>.scep.pca-connector-scep.<region>.amazonaws.com?operation=GetCACert`, and a connector created with no `VpcEndpointId` at all is served at `https://<id>.scep.vpce-anything.pca-connector-scep.<region>.vpce.amazonaws.com?operation=GetCACert` — an address no create ever announced.

## Evidence

Smithy, `CreateConnector$VpcEndpointId` (smithy_query query="operation" operation="CreateConnector"): "If you don't supply a value, by default Connector for SCEP creates a connector accessible over the public internet. If you provide a VPC endpoint ID, creates a connector accessible only through that specific VPC endpoint."

The crate states the same rule and then breaks it.

`services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/state.rs:1438-1440` (doc of `connector_endpoint`):
```
/// suffix; with one, the connector is "accessible only through that specific VPC endpoint",
/// so the host is qualified by that endpoint's id and sits under the PrivateLink suffix
/// instead. Two connectors that differ only in that choice therefore report different hosts.
```
`src/state.rs:1462` (doc of `local_connector_endpoint`): "It carries what the published host carries, with the Region and the connector's id moved out of the host and into the PATH" — yet `src/state.rs:1469-1474` takes no `vpc_endpoint_id` at all:
```
pub fn local_connector_endpoint(authority: &str, region: &str, connector_id: &str) -> String {
    format!(
        "https://{authority}/{}/{SCEP_HOST_LABEL}/{region}/{connector_id}",
```
and `src/state.rs:1501-1504` silently discards the parameter on that branch:
```
    match reached_emulator_endpoint(parts).or_else(|| served_endpoint_authority(served_base)) {
        Some(authority) => local_connector_endpoint(&authority, region, connector_id),
        None => connector_endpoint(region, connector_id, vpc_endpoint_id),
    }
```
The create is the caller: `src/ops/create_connector.rs:429-435` passes `input.vpc_endpoint_id.as_deref()` into `announced_connector_endpoint`, and `src/ops/create_connector.rs:464` stores `vpc_endpoint_id: input.vpc_endpoint_id`.

The field's own doc, `src/state.rs:291-292`:
```
    /// The VPC endpoint the connector is reachable through, when the create named one.
    pub vpc_endpoint_id: Option<String>,
```
A grep of `src/` for `vpc_endpoint_id` shows that record field written at `create_connector.rs:464` and read nowhere.

The plane contradicts its own module doc. `src/service/scep.rs:18-19`:
```
//! this plane declares no auth scheme and serves the request however it arrived. It still
//! claims only an address this service PUBLISHED, and answers only out of records the
//! control plane persisted.
```
But `src/state.rs:1627-1638` claims both host forms with any vpce label:
```
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
and `src/service/scep.rs:172-184` serves on status alone, consulting no access path:
```
            let Some(connector) = store.connector(&addressed.region, &connector_arn) else {
                return refused(NOT_FOUND_STATUS, "This URL names no SCEP connector.");
            };
            connector
                .is_active()
                .then(|| connector.certificate_authority_arn.clone())
```

Test coverage: `tests/all/connector_vpc_endpoint.rs:325-331` pins the rule that is broken on the local branch —
```
            assert_ne!(
                template, other,
                "case VpcEndpointId={path:?} vs {other_path:?}: two connectors whose creates \
                 named different access paths must report different SCEP hosts, but both \
                 reported {template:?} once the connector id is factored out"
            );
```
and it only drives `new_router()` (an unserved router addressed at AWS hosts), so it never reaches `local_connector_endpoint`. `tests/all/connector_endpoint_host_routing.rs:500-515` tables both published host forms but always dials each connector at *its own* published host (`reported_endpoint(...)`, line 535/584), so no row exercises a mismatched access path.

## Steps to Reproduce

1. Mount the service on an emulator that is served on a port (or send CreateConnector to the emulator's own endpoint rather than to an `amazonaws.com` host).
2. POST /connectors with {"CertificateAuthorityArn": "<active CA>", "VpcEndpointId": "vpce-0123abcd"} and, separately, the same body without `VpcEndpointId`.
3. GET /connectors/{arn} for both and compare `Connector.Endpoint`: the two URLs differ only in the connector id — `announced_connector_endpoint` took the `local_connector_endpoint` branch and dropped `vpc_endpoint_id`, so the documented access-path distinction is not observable.
4. On an unserved router (the published-host branch), create a connector WITH "VpcEndpointId": "vpce-0123abcd" and let it settle to ACTIVE.
5. GET https://<connector-id>.scep.pca-connector-scep.<region>.amazonaws.com?operation=GetCACert — the public host, which this create never announced: the plane answers 200 with the CA certificate, although the member documents the connector as "accessible only through that specific VPC endpoint".
6. Create a connector with NO `VpcEndpointId`, let it settle, then GET https://<connector-id>.scep.vpce-deadbeef.pca-connector-scep.<region>.vpce.amazonaws.com?operation=GetCACert — an address naming a VPC endpoint the connector has nothing to do with: the plane answers 200 with the CA certificate.
