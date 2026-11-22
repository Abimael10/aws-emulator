Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-delete-instance-connect-endpoint
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-delete-instance-connect-endpoint|run#26|AssertionError|assert redelete_message == _ice_not_found(endpoint_id), redelete_message
Issue-Commit: 2689bdaf6d03055e1786ea65beae79bb03fe1728

# ec2: DeleteInstanceConnectEndpoint's NotFound message is re-worded — 'The instance-connect-endpoint ID '<id>' does not exist' instead of AWS's 'The Instance Connect Endpoint <id> does not exist. Add an existing Instance Connect Endpoint ID and retry your request.'

> _Note: this issue was identified against the code base at commit `2689bdaf6d03055e1786ea65beae79bb03fe1728`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteInstanceConnectEndpoint

## Description
One net-new root cause, grounded in the failing ec2 op-contract check for DeleteInstanceConnectEndpoint.

Cause 1 of 1: the InvalidInstanceConnectEndpointId.NotFound refusal of DeleteInstanceConnectEndpoint carries a generic, re-worded message instead of AWS's wording for that error class.

AWS: Smithy — DeleteInstanceConnectEndpoint takes a required InstanceConnectEndpointId and returns DeleteInstanceConnectEndpointResult.InstanceConnectEndpoint; the operation declares no modelled errors (EC2 uses the ec2Query generic error envelope), so the refusal is recorded behavior only. Recorded: deleting an EC2 Instance Connect Endpoint id that does not exist — whether it was never created, or was created and already deleted by this same caller — is refused with HTTP 400, Error.Code 'InvalidInstanceConnectEndpointId.NotFound' and Error.Message 'The Instance Connect Endpoint <id> does not exist. Add an existing Instance Connect Endpoint ID and retry your request.' The fixed text is 'The Instance Connect Endpoint ', ' does not exist. Add an existing Instance Connect Endpoint ID and retry your request.'; the only variable part is the endpoint id taken verbatim from the request's InstanceConnectEndpointId (unquoted, interpolated between the two fixed fragments). The same message is used for a never-created id and for the id of an endpoint that has reached delete-complete.

Emulator: the error Code is correct ('InvalidInstanceConnectEndpointId.NotFound' — the check's code assertion passes), but the message is a different template: 'The instance-connect-endpoint ID '<id>' does not exist'. It lower-cases and hyphenates the resource name ('instance-connect-endpoint' instead of 'Instance Connect Endpoint'), adds the words 'ID', single-quotes the id, and drops AWS's whole remediation sentence 'Add an existing Instance Connect Endpoint ID and retry your request.' This is the generic EC2 not-found template rather than the one AWS uses for this id class.

Why: an HTTP status, an error code and an error message are all part of the contract. Clients, SDK retry/error middleware, CLI users and log scrapers that match on AWS's message text (and support tooling that reproduces it) do not recognise this string, and the lost remediation sentence is the part that tells an operator what to do. Because the same wrong template is served for both the never-created id and the just-deleted id, no caller can distinguish the EC2 Instance Connect Endpoint refusal from any other EC2 not-found refusal by message.

Fix: the refusal path of the DeleteInstanceConnectEndpoint handler (and the shared not-found message helper it uses for the instance-connect-endpoint id class) must emit AWS's wording for this error class, with the request's InstanceConnectEndpointId interpolated unquoted, keeping the 400 status and the InvalidInstanceConnectEndpointId.NotFound code it already gets right.

Traces: ec2.op-contract-delete-instance-connect-endpoint.

## Evidence
Check ec2.op-contract-delete-instance-connect-endpoint, line 89 of the check:
  assert redelete_message == _ice_not_found(endpoint_id), redelete_message
  AssertionError: The instance-connect-endpoint ID 'eice-000000000000000ac' does not exist

Expected (AWS), per the check's _ice_not_found() helper: Error.Code 'InvalidInstanceConnectEndpointId.NotFound' with Error.Message 'The Instance Connect Endpoint <endpoint-id> does not exist. Add an existing Instance Connect Endpoint ID and retry your request.' (HTTP 400).
Actual (emulator): Error.Code 'InvalidInstanceConnectEndpointId.NotFound' (correct) with Error.Message 'The instance-connect-endpoint ID '<endpoint-id>' does not exist'.
The endpoint id in the recorded run (eice-000000000000000ac) is a per-run value filling the template's one variable slot; the requirement is the fixed text around it. The check asserts the identical message for the second delete of a now-dead endpoint and for a never-created, zero-filled id, so both paths must use it.

## Steps to Reproduce
1. CreateVpc(CidrBlock=10.72.0.0/16) and CreateSubnet in one availability zone.
2. CreateInstanceConnectEndpoint(SubnetId=<subnet>) and poll DescribeInstanceConnectEndpoints until State is 'create-complete'.
3. DeleteInstanceConnectEndpoint(InstanceConnectEndpointId=<id>) and poll until the endpoint reports 'delete-complete' or is gone.
4. Call DeleteInstanceConnectEndpoint(InstanceConnectEndpointId=<the same, now-dead id>) again: AWS answers 400 InvalidInstanceConnectEndpointId.NotFound with 'The Instance Connect Endpoint <id> does not exist. Add an existing Instance Connect Endpoint ID and retry your request.'; the emulator answers the same code with 'The instance-connect-endpoint ID '<id>' does not exist'.
5. Repeat with a never-created id of the same shape (the real id with its suffix replaced by zeros): the same message is expected and the same divergence appears.
