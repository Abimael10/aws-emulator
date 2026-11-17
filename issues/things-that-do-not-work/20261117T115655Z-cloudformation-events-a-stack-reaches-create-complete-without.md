Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__events.cfn-events-api-destination-update-nonexistent-connection-rejected-and-rolled-back cloudformation__events.cfn-events-connection-and-api-destination-update-in-place-and-replacement
Issue-Key: cloudformation__events|DescribeApiDestination|ResourceNotFoundException
Issue-Key: cloudformation__events|DescribeConnection|ResourceNotFoundException
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# cloudformation__events: a stack reaches CREATE_COMPLETE without materializing its AWS::Events::Connection / AWS::Events::ApiDestination, so DescribeConnection and DescribeApiDestination 404 the resources the stack claims to own

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1 — the CloudFormation providers for AWS::Events::Connection and AWS::Events::ApiDestination report success without creating anything in the EventBridge service.
AWS: when CreateStack settles at CREATE_COMPLETE, every resource in the template exists in its own service and is readable there under the Name the template gave it. A stack carrying an AWS::Events::Connection (AuthorizationType API_KEY with ApiKeyAuthParameters) and an AWS::Events::ApiDestination whose ConnectionArn is `Fn::GetAtt [Conn, Arn]` leaves `events describe-connection --name <conn>` and `events describe-api-destination --name <dest>` answering HTTP 200 with the stack's values — in particular the destination's ConnectionArn equals the connection's Arn output. ResourceNotFoundException (Smithy: errorKind client, httpError 400) is reserved for a name that does not exist. Only after that does the rest of the contract apply: an update pointing ConnectionArn at a connection that was never created must be refused and rolled back to the destination's prior ConnectionArn, and a Connection update must be in-place for a mutable property and a replacement (`Requested update requires the creation of a new physical resource; hence creating one.`) for an immutable one.
Emulator: the stack settles CREATE_COMPLETE (the checks' CREATE_COMPLETE assertion passes), but the very next read of the resource in the events service is refused: DescribeApiDestination answers ResourceNotFoundException `Failed to describe the api-destination(s). An api-destination '<the Name the template gave>' does not exist.` and DescribeConnection answers ResourceNotFoundException `Failed to describe the connection(s). Connection '<the Name the template gave>' does not exist.` The fixed text of both messages is AWS's; the name is the request's. So the stack owns resources that do not exist, and every downstream update/replacement/rollback assertion is unreachable.
Why: a client that deploys an EventBridge connection or API destination by stack and then uses it — resolving its ARN, authorizing it, attaching it to a rule target — gets a green stack and a 404 from EventBridge. Nothing in the deploy tells it the resource is missing, so the failure surfaces far from its cause.
Fix: the CloudFormation resource providers for AWS::Events::Connection and AWS::Events::ApiDestination — they must call the events service's create/update/delete operations and record the real physical resource id, rather than booking the resource as created in stack state alone. The open issue 20261109T171600Z-cloudformation-6-net-new-causes-aws-events-endpoint.md names other connection/api-destination deploy checks but does not carry these two checks' replay keys, so it does not cover them.
Traces: cloudformation__events.cfn-events-api-destination-update-nonexistent-connection-rejected-and-rolled-back, cloudformation__events.cfn-events-connection-and-api-destination-update-in-place-and-replacement.

## Evidence
Both checks assert CREATE_COMPLETE successfully and then fail on the first read of the resource in the events service:
- `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DescribeApiDestination operation: Failed to describe the api-destination(s). An api-destination 'sim2real-behav-evdest-<suffix>' does not exist.`
- `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DescribeConnection operation: Failed to describe the connection(s). Connection 'sim2real-behav-evconn-a-<suffix>' does not exist.`
Expected: HTTP 200 from both describes, with the destination's ConnectionArn equal to the stack's ConnAArn output.

## Steps to Reproduce
1. CreateStack with a template containing an AWS::Events::Connection (Name, AuthorizationType API_KEY, ApiKeyAuthParameters) and an AWS::Events::ApiDestination (Name, ConnectionArn = Fn::GetAtt [Conn, Arn], InvocationEndpoint, HttpMethod POST), plus an Output exporting the connection Arn.
2. Wait for CREATE_COMPLETE.
3. Call `events describe-connection --name <conn>` and `events describe-api-destination --name <dest>` — both are refused with ResourceNotFoundException instead of returning the deployed resource.
4. Re-run the two checks in trace_ids to confirm the subsequent update/replacement/rollback assertions become reachable.
