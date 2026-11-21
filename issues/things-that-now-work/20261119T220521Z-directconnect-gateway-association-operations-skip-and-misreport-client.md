Issue-Source: trace-sweep-divergence
Issue-Trace: direct-connect-validation-and-errors
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: direct-connect-validation-and-errors
Issue-Shape-Id: com.amazonaws.directconnect#OvertureService
Issue-Key: directconnect|DescribeDirectConnectGatewayAssociations|4xx->2xx|$.__type
Issue-Key: directconnect|DescribeDirectConnectGatewayAssociations|4xx->2xx|$.message
Issue-Key: directconnect|DescribeDirectConnectGatewayAssociations|4xx->2xx|status
Issue-Key: directconnect|CreateDirectConnectGatewayAssociation|4xx->4xx|$.message
Issue-Commit: ae04e9c7457c944b401f43b41441b2fef7a78bf2

# directconnect: gateway association operations skip and misreport client-fault validation

> _Note: this issue was identified against the code base at commit `ae04e9c7457c944b401f43b41441b2fef7a78bf2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDirectConnectGatewayAssociations

## Description
The emulator's Direct Connect *gateway-association* request validation diverges from AWS twice: one request AWS rejects as a client fault is accepted and answered 200, and one request AWS and the emulator both reject carries the wrong referent in its message because the inputs are validated in the wrong order. This bundle has 2 causes; BOTH must be fixed before it is closed.

Cause 1 of 2: DescribeDirectConnectGatewayAssociations accepts a request with no selector and answers 200 where AWS answers 400 DirectConnectClientException (fixture direct-connect-validation-and-errors, step task#6).
AWS: Smithy: every DescribeDirectConnectGatewayAssociations input member (associationId, associatedGatewayId, directConnectGatewayId, virtualGatewayId, maxResults, nextToken) is optional in the model, but the operation's own documentation states the requirement the model cannot express: "Lists the associations between your Direct Connect gateways and virtual private gateways and transit gateways. You must specify one of the following: A Direct Connect gateway / A virtual private gateway / A transit gateway / ...". So at least one selector is mandatory, and the operation's modeled client fault is DirectConnectClientException with httpError 400. The recording shows AWS answering status 400 with $.__type 'DirectConnectClientException' and the message 'Either Direct Connect Gateway ID or Associated Gateway ID or Association ID must be set'. That message is entirely FIXED text — it names the three accepted selectors and interpolates nothing from the request or from state.
Emulator: answers HTTP 200 with neither $.__type nor $.message, i.e. it treats the empty selector set as a valid unfiltered list request.
Why: a client that omits its selector (an unset variable, a mis-spelled parameter name, a filter built from an empty collection) gets a successful-looking response instead of the modeled rejection. Depending on what the 200 body carries, the caller either reads an empty list as 'no associations exist' or reads a cross-tenant unfiltered listing as if it had been filtered — in both cases a programming error AWS surfaces loudly is silently absorbed, and error-path tests that assert the 400 never fire.
Fix: DescribeDirectConnectGatewayAssociations' input validation, which must enforce the documented 'at least one selector' precondition that the Smithy model's optional members do not express, and raise DirectConnectClientException (400) with AWS's fixed message before any state lookup runs.
Traces: direct-connect-validation-and-errors#task#6.

Cause 2 of 2: CreateDirectConnectGatewayAssociation reports the Direct Connect gateway as missing when AWS reports the virtual gateway as missing (fixture direct-connect-validation-and-errors, step task#4).
AWS: Smithy: CreateDirectConnectGatewayAssociation takes a required directConnectGatewayId plus an optional virtualGatewayId ("The ID of the virtual private gateway.") / gatewayId, and models DirectConnectClientException (httpError 400) as its client fault; the message wording is recorded behavior only. The recording shows AWS answering 400 with 'The VirtualGateway <virtualGatewayId> for owner <accountId> does not exist' for a request naming a non-existent virtual gateway. Fixed text: 'The VirtualGateway ' ... ' for owner ' ... ' does not exist'; the variable parts are the virtualGatewayId copied from the request and the owning account id taken from the caller's credentials/state (the recorded 'vgw-00000000' and '000000000000' are that run's values, not part of the template). The recording therefore establishes AWS's resolution ORDER: the associated/virtual gateway is resolved and reported first, before the Direct Connect gateway id is checked.
Emulator: same 400 status, but the message is 'Direct Connect gateway ID <directConnectGatewayId> does not exist' — it validates directConnectGatewayId first and so names the wrong resource, never mentioning the virtual gateway the caller actually got wrong.
Why: DirectConnectClientException is a single shape behind a single 400 status, so the message is the only discriminator a caller has. A client (or a retry/repair routine) that parses the message to decide which identifier to fix is pointed at the Direct Connect gateway when the virtual gateway is the broken input, so it re-creates or re-resolves the wrong resource and the association never succeeds.
Fix: the identifier-resolution order in CreateDirectConnectGatewayAssociation's handler/validation — resolve and report the virtualGatewayId / gatewayId referent before the directConnectGatewayId, and emit AWS's VirtualGateway message template with the request's gateway id and the account id from state interpolated.
Traces: direct-connect-validation-and-errors#task#4.

Other open directconnect issues do NOT cover these two: '20261109T130740Z-directconnect-gateway-and-resiliency-group-error-responses-diverge.md' bundles DescribeDirectConnectGateways/CreateDirectConnectGateway/CreateResiliencyGroup error bodies (steps task#3, task#1, task#8) and carries no replay key for task#4 or task#6; '20261109T130740Z-directconnect-direct-connect-gateway-records-omit-arn-type.md' covers gateway record fields; '20261117T075510Z-directconnect-describelocations-catalog-diverges-from-the-published-location.md' covers the DescribeLocations catalog.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false).

Cause 1 - direct-connect-validation-and-errors#task#6, DescribeDirectConnectGatewayAssociations, expected 400 / actual 200:
  status: expected 400, got 200
  $.__type: missing (expected 'DirectConnectClientException', got None)
  $.message: missing (expected 'Either Direct Connect Gateway ID or Associated Gateway ID or Association ID must be set', got None)
  (the whole message is fixed text; nothing in it comes from the request)

Cause 2 - direct-connect-validation-and-errors#task#4, CreateDirectConnectGatewayAssociation, expected 400 / actual 400:
  $.message: value_mismatch
    expected: 'The VirtualGateway vgw-00000000 for owner 000000000000 does not exist'
    actual:   'Direct Connect gateway ID 00000000-0000-0000-0000-000000000000 does not exist'
  (fixed text: 'The VirtualGateway ... for owner ... does not exist'; 'vgw-00000000' is the request's virtualGatewayId and '000000000000' is the owning account id from state — the emulator instead names the directConnectGatewayId, showing it validates the inputs in the opposite order)

## Steps to Reproduce
1. Replay fixture direct-connect-validation-and-errors against the emulator built from this checkout.
2. Step task#6: call DescribeDirectConnectGatewayAssociations with none of associationId / associatedGatewayId / directConnectGatewayId / virtualGatewayId set, exactly as recorded. The emulator answers HTTP 200 with no $.__type and no $.message; AWS answers HTTP 400 with __type 'DirectConnectClientException' and message 'Either Direct Connect Gateway ID or Associated Gateway ID or Association ID must be set'.
3. Step task#4: call CreateDirectConnectGatewayAssociation naming a non-existent virtualGatewayId together with a non-existent directConnectGatewayId, as recorded. Status is 400 as expected, but $.message names the Direct Connect gateway ID instead of AWS's 'The VirtualGateway <virtualGatewayId> for owner <accountId> does not exist'.
4. Compare each response's status, $.__type and $.message against the fixture's expected values.
