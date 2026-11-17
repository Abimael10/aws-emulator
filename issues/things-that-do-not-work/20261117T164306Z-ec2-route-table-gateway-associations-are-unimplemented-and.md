Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found ec2-route-table-client-token-retry-tag-change-routes-to-gateway-and-interface-then-not-found
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|AssociateRouteTable|2xx->5xx|$.AssociateRouteTableResponse
Issue-Key: ec2|AssociateRouteTable|2xx->5xx|status
Issue-Key: ec2|DescribeRouteTables|2xx->2xx|$.DescribeRouteTablesResponse.routeTableSet.item.associationSet.item.associationState.state
Issue-Key: ec2|DescribeRouteTables|2xx->2xx|$.DescribeRouteTablesResponse.routeTableSet.item.associationSet.item
Issue-Key: ec2|DescribeRouteTables|2xx->2xx|$.DescribeRouteTablesResponse.routeTableSet.item.associationSet.item[*]
Issue-Key: ec2|DescribeRouteTables|2xx->2xx|$
Issue-Key: ec2|DescribeRouteTables|2xx->2xx|$.DescribeRouteTablesResponse.routeTableSet.item.routeSet.item[destinationCidrBlock="10.1.1.0/24"].state
Issue-Related: 20261104T062058Z-ec2-route-table-reads-lose-routes-associations-blackhole.md
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ec2 route table gateway associations are unimplemented and blackhole route state is never reported

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateRouteTable

## Description
6 causes in the EC2 route-table surface: the gateway (edge) association form of AssociateRouteTable is not served at all, so the whole association view of a route table is wrong, and separately a route whose target has been deleted never transitions to `blackhole`.

Cause 1 of 6: AssociateRouteTable with GatewayId answers 501
AWS: Smithy: AssociateRouteTable input carries `GatewayId` (target RouteGatewayId) alongside `SubnetId` and the required `RouteTableId`; output is `AssociationId` + `AssociationState` (RouteTableAssociationState). The operation models no errors, i.e. a well-formed edge association succeeds. Recorded response for this step: HTTP 200 with `associationId` and `associationState.state` = 'associating'.
Emulator: HTTP 501 and no `AssociateRouteTableResponse` body at all. (The replay reports no stub path for this operation, so it is not covered by an unimplemented-operation issue.)
Why: a client associating a route table with an internet gateway for edge routing gets a 501 that no SDK retry or error path expects; every subsequent read of that association is impossible, and ingress routing configuration cannot be built at all.
Fix: the AssociateRouteTable handler in the EC2 crate - accept the GatewayId form, allocate an rtbassoc- id, record the association against the route table with `gatewayId`, and return associationState.state='associating'.
Traces: ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found#task#6

Cause 2 of 6: DescribeRouteTables omits the gateway association right after it is made
AWS: Smithy: RouteTable.Associations -> xmlName `associationSet`, items RouteTableAssociation carrying routeTableAssociationId, routeTableId, gatewayId, main=false and associationState.state. Recorded response: HTTP 200 with an associationSet item for the gateway association in state 'associated'.
Emulator: HTTP 200 with `associationSet` serialized as an empty element, and the polled `associationSet.item.associationState.state` never appears (not in ['associated'] after 30 attempts).
Why: the association a client just created is invisible, so no edge-routing configuration can be verified or torn down.
Fix: the DescribeRouteTables handler / route-table association state in the EC2 crate.
Traces: ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found#task#7

Cause 3 of 6: DescribeRouteTables reports one association where AWS reports both the gateway and the subnet one
AWS: Recorded response: HTTP 200, associationSet with two items - the gateway association (gatewayId set) and the subnet association (subnetId set), both associationState.state='associated'.
Emulator: HTTP 200 with only the subnet association; polled for 30 attempts, the item count stays 1 instead of 2.
Why: a client enumerating the route table's associations cannot see that the table is also attached to the gateway, so edge associations are silently dropped from inventory.
Fix: the DescribeRouteTables handler / route-table association state.
Traces: ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found#task#9

Cause 4 of 6: DescribeRouteTables returns an empty associationSet after the subnet association is removed
AWS: Recorded response: HTTP 200 with the gateway association still listed, associationState.state='associated'.
Emulator: HTTP 200 with `associationSet` empty; polled 30 attempts, no item ever appears.
Why: removing one association appears to remove all of them, so a client cannot tell which attachments survive.
Fix: the DescribeRouteTables handler / route-table association state.
Traces: ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found#task#11

Cause 5 of 6: a disassociated gateway association is never reported with state 'disassociated'
AWS: Smithy: RouteTableAssociationState.State - after DisassociateRouteTable the association is reported with associationState.state='disassociated'. Recorded response: HTTP 200 with that association item present and state='disassociated'.
Emulator: HTTP 200 with an empty `associationSet`; the step's poll is capped at 60 s without the value ever being produced (state not in ['disassociated'] after 13 attempts).
Why: the documented terminal state of a disassociation is unobservable, so a client waiting for 'disassociated' waits forever.
Fix: the DisassociateRouteTable handler / route-table association state - keep the association and move it to 'disassociated'.
Traces: ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found#task#13

Cause 6 of 6: a route whose target is gone stays 'active' instead of 'blackhole'
AWS: Smithy: Route.State (RouteState) - "The `blackhole` state indicates that the route's target isn't available (for example, the specified gateway isn't attached to the VPC, or the specified NAT instance has been terminated)." Recorded response: HTTP 200 with the route for the destination CIDR reported state='blackhole' once its gateway/interface target has been deleted.
Emulator: HTTP 200 with that route still reported state='active'.
Why: a client cannot detect a broken route - routing audits and reconciliation treat a blackholed route as healthy.
Fix: the DescribeRouteTables route projection / the route-table state in the EC2 crate - derive route state from whether the target still exists.
Traces: ec2-route-table-client-token-retry-tag-change-routes-to-gateway-and-interface-then-not-found#task#17 (same cluster also at #task#19)

## Evidence
Replay of trace set simparity-wire-aws-v1 (service ec2), verdict=mismatch:
- AssociateRouteTable, fixture ec2-route-table-subnet-and-gateway-edge-associations-disassociated-then-association-not-found step task#6: status: expected 200, got 501; $.AssociateRouteTableResponse: missing (expected {'requestId': '<shaped-not-asserted>', 'associationId': '<shaped-not-asserted>', 'associationState': {'state': 'associating'}}, got None)
- DescribeRouteTables, same fixture task#7: $.DescribeRouteTablesResponse.routeTableSet.item.associationSet: type_mismatch (expected the gateway association item, got ''); associationSet.item.associationState.state: poll: not in ['associated'] after 30 attempt(s)
- same fixture task#9: associationSet.item: poll: 1 item(s), not 2 after 30 attempt(s) (gateway association missing; only the subnet association present)
- same fixture task#11: associationSet: type_mismatch (expected the gateway association item, got ''); associationSet.item: poll: 0 item(s), not 1 after 30 attempt(s)
- same fixture task#13: $: poll capped at 60s; associationSet: type_mismatch (expected the association with associationState.state='disassociated', got ''); state: poll: not in ['disassociated'] after 13 attempt(s)
- DescribeRouteTables, fixture ec2-route-table-client-token-retry-tag-change-routes-to-gateway-and-interface-then-not-found step task#17 (and task#19): routeSet.item[destinationCidrBlock="10.1.1.0/24"].state: value_mismatch (expected 'blackhole', got 'active')

## Steps to Reproduce
Gateway associations (causes 1-5):
1. CreateVpc, CreateSubnet, CreateInternetGateway + AttachInternetGateway, CreateRouteTable.
2. AssociateRouteTable with RouteTableId + GatewayId (edge association) -> emulator answers HTTP 501; AWS answers 200 with associationId and associationState.state='associating'.
3. DescribeRouteTables -> associationSet is empty; after also associating a subnet it lists only the subnet association.
4. DisassociateRouteTable the subnet association, then DescribeRouteTables -> associationSet is empty instead of still listing the gateway association, and after disassociating the gateway association it is never reported with state 'disassociated'.
Blackhole route (cause 6):
5. CreateRoute for 10.1.1.0/24 pointing at a gateway/network interface, then delete that target.
6. DescribeRouteTables -> the route is still state 'active'; AWS reports 'blackhole'.
Or replay the fixture steps listed under Traces.
