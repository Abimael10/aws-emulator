Issue-Source: trace-sweep-divergence
Issue-Trace: emr-run-job-flow-step-writes-s3-output
Issue-Step: teardown#36
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: emr-run-job-flow-step-writes-s3-output
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: ec2|DeleteRouteTable|2xx->4xx|status
Issue-Commit: 75e93429d3ea083e11be0578db4814848b1cd3ff

# ec2 DeleteRouteTable refuses a disassociated, non-main route table with 400 instead of 200

> _Note: this issue was identified against the code base at commit `75e93429d3ea083e11be0578db4814848b1cd3ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteRouteTable

## Description
One cause.

Cause 1 of 1: DeleteRouteTable rejects a route table whose only association was the subnet that has already been deleted.

AWS: Smithy: DeleteRouteTable input DeleteRouteTableRequest with RouteTableId @required (xmlName routeTableId), output Unit, errors [] (no modeled error). Its documentation states the only preconditions: "Deletes the specified route table. You must disassociate the route table from any subnets before you can delete it. You can't delete the main route table." A custom (non-main) route table whose subnet associations are gone therefore satisfies both preconditions, and AWS answers HTTP 200 with an ec2Query DeleteRouteTableResponse carrying return 'true'. In this fixture the route table's only association was the subnet deleted in the immediately preceding teardown step, so by the time DeleteRouteTable runs the table is unassociated and deletable.

Emulator: answers HTTP 400 for that same request (replay: status expected 200, got 400). The recorded AWS response for this step is a 200 success, so the recording carries no error code or message to compare the refusal body against — the divergence is the refusal itself, i.e. a status class AWS never returns here. Hypothesis: the route table's subnet-association bookkeeping is not cleared when the associated subnet is deleted, so a stale association survives its subnet and the deletability precondition is evaluated against a dependency that no longer exists. EC2 models no error for this operation at all, so whatever 400 class the emulator picks is additionally unmodeled for DeleteRouteTable.

Why: a client that provisions a VPC with a custom route table can never tear it down. The teardown sequence halts at this step, so every later step built on the route table being gone (DetachInternetGateway, DeleteInternetGateway, DeleteVpc in this same fixture) is attempted against an estate that is still referenced, and route tables plus their parent VPC leak across runs. A caller also cannot distinguish "still associated" from "does not exist", because AWS reserves this path for success.

Fix: the DeleteRouteTable handler's deletability check and the route-table association bookkeeping in EC2 state — a subnet deletion (and any explicit DisassociateRouteTable) must remove the association rows that reference it, so the subsequent DeleteRouteTable sees an unassociated, non-main table and returns 200 with return 'true'. Note the open issue 20261103T123451Z-ec2-refuses-valid-requests-with-400-or-501 describes the same stale-dependency fault for DeleteSubnet, DeleteVpc, DeleteNetworkAcl and DeleteSecurityGroup, but its Issue-Key list does not include DeleteRouteTable and the replay reports this cluster as unowned, so DeleteRouteTable is not covered by it and must be fixed and re-replayed on its own.

Traces: emr-run-job-flow-step-writes-s3-output#teardown#36.

## Evidence
Replay verdict (expected -> actual), cluster service=ec2|DeleteRouteTable|status=200->400|keys=status:
- fixture emr-run-job-flow-step-writes-s3-output, step teardown#36, operation DeleteRouteTable: status expected 200, got 400 (detail: "status: expected 200, got 400"). affected_count 1.
- The recorded AWS response for the step is a 200 success body (ec2Query DeleteRouteTableResponse / return 'true'); the emulator returns a 400 refusal instead, and EC2 models no error for DeleteRouteTable (errors: []).
- Preceding teardown steps in the same fixture delete the subnet this route table was associated with (teardown#35 DeleteSubnet), so at teardown#36 the table is non-main and unassociated.
- Context: the same fixture's later teardown steps (#37 DetachInternetGateway, #38 DeleteInternetGateway, #39 DeleteVpc) also fail 200->400, but those clusters are already owned by open issues; this DeleteRouteTable cluster reports owned_by [] and stub_path "".

## Steps to Reproduce
1. Replay the wire corpus simparity-wire-aws-v1 against the emulator.
2. Open fixture emr-run-job-flow-step-writes-s3-output and run it through its teardown phase (it creates a VPC with a custom route table, a subnet associated to it, and an internet gateway; teardown#35 deletes the subnet).
3. Inspect step teardown#36: EC2 DeleteRouteTable with the custom route table's RouteTableId.
4. Observe the emulator answers HTTP 400 where the recording and AWS answer HTTP 200 with DeleteRouteTableResponse return 'true'.
