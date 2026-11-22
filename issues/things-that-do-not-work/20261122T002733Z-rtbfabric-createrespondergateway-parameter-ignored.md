Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 05866a96998b37f437bd411ae9396664f070e9d9

# CreateResponderGateway stores no clientRoutingPolicy when the caller omits one, so the default the model documents (AVAILABILITY_ZONE_AFFINITY) is never stored or reported

_Filed: 2026-11-22T00:27:33Z_
_Operation: CreateResponderGateway | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `05866a96998b37f437bd411ae9396664f070e9d9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`clientRoutingPolicy` is optional on `CreateResponderGatewayRequest`, and the member's own modelled documentation states what the service does when it is absent: "If you don't specify a value, RTB Fabric uses AVAILABILITY_ZONE_AFFINITY." The member carries no Smithy `default` trait, so nothing fills it in before the handler runs (contrast `maxResults`, which the model does give `"default": 10` and which therefore reaches the handler as a plain `i32`).

The handler stores the caller's `Option<String>` verbatim and reports it back verbatim, so a gateway created without the member holds `None` forever. Both responses that carry the member — `CreateResponderGatewayResponse.clientRoutingPolicy` and `GetResponderGatewayResponse.clientRoutingPolicy`, documented as "The client routing policy of the gateway. This policy controls which Availability Zones RTB Fabric uses to reach the gateway..." — then omit it (JSON null), reporting that the gateway has no routing policy at all, where the model says the gateway is being reached under AVAILABILITY_ZONE_AFFINITY.

The crate already has the pattern for materialising a member the create did not carry: the same handler computes `external_inbound_endpoint` from the stored gateway's own type rather than leaving it absent (create_responder_gateway.rs:348-354). `clientRoutingPolicy` is the one documented substitution it does not make.

## Evidence

Smithy, `smithy_query(query="shape", shape="CreateResponderGatewayRequest")`, member `clientRoutingPolicy` — verbatim: "... <p>If you don't specify a value, RTB Fabric uses <code>AVAILABILITY_ZONE_AFFINITY</code>. To get the behavior of <code>ANY_AVAILABILITY_ZONE</code>, create the gateway with subnets in more than one Availability Zone. ..." The member shows no `default` trait (unlike `ListResponderGatewaysRequest$maxResults`, which the model gives `"default": 10`).

The member reaches the handler as an absent option, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/types/create_responder_gateway_types.rs:30 and :40:
```
    pub client_routing_policy: Option<String>,
```

What the handler stores and reports, src/ops/create_responder_gateway.rs:342 and :361:
```
            client_routing_policy: input.client_routing_policy,
```
```
            client_routing_policy: gateway.client_routing_policy.clone(),
```
and the read, src/ops/get_responder_gateway.rs:230:
```
            client_routing_policy: gateway.client_routing_policy.clone(),
```
Nothing in the crate ever substitutes a policy: `grep -n "client_routing" src` finds only these assignments plus the `ResponderGateway.client_routing_policy` field (src/state.rs:213) and the fingerprint rendering.

The sibling substitution the same handler DOES make for a member the request did not carry, src/ops/create_responder_gateway.rs:348-354:
```
        // Only an EXTERNAL gateway accepts inbound external links, so only it has a
        // public inbound endpoint. The type is read off the record through the one
        // reading of it the whole crate shares.
        if gateway.is_external() {
            gateway.external_inbound_endpoint =
                Some(crate::state::external_inbound_endpoint(&gateway_id, &region));
        }
```

Test coverage: tests/all/gateway_link_lifecycle.rs:137 is the row that omits the member (`client_routing_policy: None`), and tests/all/gateway_link_lifecycle.rs:420-426 asserts the response member is JSON null for it, with the stated reason "expected the persisted clientRoutingPolicy back" (repeated for the read at :571-575). That reason is exactly what the modelled sentence contradicts: the service does not persist nothing, it uses AVAILABILITY_ZONE_AFFINITY.

## Steps to Reproduce

1. POST /responder-gateway with the required members only (vpcId, subnetIds, securityGroupIds, port, protocol, clientToken and a domainName) and NO clientRoutingPolicy.
2. Observed: 200 whose body has no clientRoutingPolicy (null). Expected per the member's documented behaviour ("If you don't specify a value, RTB Fabric uses AVAILABILITY_ZONE_AFFINITY") the gateway's effective policy to be reported.
3. GET /responder-gateway/{gatewayId} — observed clientRoutingPolicy absent again, so no caller can learn which Availability Zone policy the gateway is reached under.
4. Repeat step 1 with "clientRoutingPolicy": "ANY_AVAILABILITY_ZONE" — that value is stored and reported, showing the member is only mishandled when omitted.
