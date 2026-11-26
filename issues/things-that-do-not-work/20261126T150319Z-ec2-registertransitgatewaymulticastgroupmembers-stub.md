Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-deregister-transit-gateway-multicast-group-members ec2.op-contract-register-transit-gateway-multicast-group-members ec2.op-contract-search-transit-gateway-multicast-groups
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|RegisterTransitGatewayMulticastGroupMembers|NotImplemented
Issue-Commit: e1287cf409e72f27d41b5362b1ef9b2f278addc7

# Implement ec2 RegisterTransitGatewayMulticastGroupMembers: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `e1287cf409e72f27d41b5362b1ef9b2f278addc7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTransitGatewayMulticastGroupMembers

## Description
`RegisterTransitGatewayMulticastGroupMembers` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-ec2-amazonec2/src/ops/register_transit_gateway_multicast_group_members.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

3 recorded unit(s) fail on their `RegisterTransitGatewayMulticastGroupMembers` call, so each one unblocks only when `RegisterTransitGatewayMulticastGroupMembers` works: `ec2.op-contract-deregister-transit-gateway-multicast-group-members`, `ec2.op-contract-register-transit-gateway-multicast-group-members`, `ec2.op-contract-search-transit-gateway-multicast-groups`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-ec2-amazonec2/src/ops/register_transit_gateway_multicast_group_members.rs`. Replaying the corpus, the emulator refused the `RegisterTransitGatewayMulticastGroupMembers` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the RegisterTransitGatewayMulticastGroupMembers operation: operation RegisterTransitGatewayMulticastGroupMembers is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the ec2 emulator
Call RegisterTransitGatewayMulticastGroupMembers and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
