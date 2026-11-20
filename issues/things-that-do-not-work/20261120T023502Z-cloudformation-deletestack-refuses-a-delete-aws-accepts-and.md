Issue-Source: trace-sweep-divergence
Issue-Trace: cloudformation-deletestack-retainresources-non-empty-bucket
Issue-Step: teardown#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudformation-deletestack-retainresources-non-empty-bucket stack-policy-input-validation cloudformation-cfns3cnt-create-failed-stack-keeps-live-bucket
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation|DeleteStack|2xx->4xx|status
Issue-Key: cloudformation|ListStackResources|2xx->2xx|$.ListStackResourcesResponse.ListStackResourcesResult.StackResourceSummaries.member[LogicalResourceId="Vec"].ResourceStatusReason
Issue-Commit: 060e8ea86e1a5a163ad053fe72c03956e6c60bea

# cloudformation: DeleteStack refuses a delete AWS accepts, and a failed resource's ResourceStatusReason diverges

> _Note: this issue was identified against the code base at commit `060e8ea86e1a5a163ad053fe72c03956e6c60bea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStack

## Description
Two net-new root causes on the CloudFormation wire surface, both reached while tearing down / reading back a stack whose delete or whose create partially failed. A fixer must close BOTH before submitting.

Cause 1 of 2: DeleteStack answers HTTP 400 for a delete AWS accepts with HTTP 200.
AWS: DeleteStack is a fire-and-forget call — it returns 200 with an empty DeleteStackResult (Smithy: DeleteStack output is Unit, and the ONLY modeled operation error is TokenAlreadyExistsException @httpError 400; every other refusal would be the service-common ValidationError). Re-driving a delete on a stack that is already DELETE_FAILED, and naming that stack's own live logical ids in RetainResources, are both accepted: Smithy documents RetainResources as 'For stacks in the DELETE_FAILED state, a list of resource logical IDs that are associated with the resources you want to retain. During deletion, CloudFormation deletes the stack but doesn't delete the retained resources' and calls out the non-empty S3 bucket as the motivating case. So a DeleteStack that names resources of the stack, in a state where retention is meaningful, must come back 200 and move the stack toward DELETE_COMPLETE.
Emulator: the same DeleteStack request answers HTTP 400 — the step asserted status 200 and got 400. It reproduces on two unrelated fixtures' teardown DeleteStack (a retain-resources-over-a-non-empty-bucket stack, and a stack-policy stack), so the guard is on DeleteStack itself and not on one template. Hypothesis: the RetainResources / stack-state validation added for the opposite defect (open issue 20261107T085452Z-cloudformation-8-net-new-behavioral-causes-stack-outputs.md, its Cause 3, reports DeleteStack ACCEPTING an INVALID RetainResources list) now over-refuses valid retain lists or a repeat delete; that issue is behavioral-only and does NOT carry this replay key, so it does not cover this.
Why: a client cannot delete the stack at all. The call that is supposed to be idempotent and non-failing becomes a hard client error, so teardown scripts, CI cleanups and the documented 'delete the stack but retain the non-empty bucket' remediation all abort, and the stack and its resources leak.
Fix: DeleteStack's request validation / state guard in the CloudFormation operation handler — the retain-list and stack-state checks must accept logical ids that belong to the stack and a re-driven delete of a DELETE_FAILED stack, and refuse only what AWS refuses (ValidationError for a retain list naming resources that are not in a valid state).
Traces: cloudformation-deletestack-retainresources-non-empty-bucket#teardown#9; stack-policy-input-validation#teardown#11.

Cause 2 of 2: the ResourceStatusReason ListStackResources reports for the resource whose create failed does not reproduce AWS's reason.
AWS: for a resource provisioned through a resource-type handler, the CREATE_FAILED reason CloudFormation records (and serves back on StackResourceSummary.ResourceStatusReason) is the handler envelope wrapping the RELAYED service exception: fixed text 'Resource handler returned message: "' + the service's own error message INCLUDING its SDK suffix '(Service: <service>, Status Code: <status>, Request ID: <id>)' + '" (RequestToken: ' + the operation's request token + ', HandlerErrorCode: ' + the handler error code + ')'. The fixed text is the envelope and the service's message template; the request token, request id and the generated bucket/index names are per-run values. Recorded behavior only for the exact inner text: the recorded reason is shape-masked in the fixture ('<shaped-not-asserted>'), and the replay still reports value_mismatch on that member, so the emulator's reason differs from the recorded one in more than its per-run ids.
Emulator: ListStackResources serves ResourceStatusReason 'Resource handler returned message: "Invalid vector bucket name" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)' for the failed AWS::S3Vectors::VectorBucket resource — a bare, emulator-authored handler message with no relayed service-exception suffix.
Why: a client or deploy tool that reads ResourceStatusReason to classify WHY a resource failed (matching the service, the status code or the service's own message out of the relayed suffix) cannot: it sees a message AWS never words that way, so the failure is unclassifiable and operator-facing output is wrong.
Fix: the CloudFormation resource-handler failure-reason propagation (the reason recorded on the resource and serialized by ListStackResources / DescribeStackResources / DescribeStackEvents) must carry the provisioning service's real rejection message with its SDK envelope, and the handler error code AWS uses for that rejection class. Same defect family as open issues 20261117T143604Z-cloudformation-secretsmanager-2-net-new-causes-the-colliding.md and 20261119T173202Z-cloudformation-ecs-a-stack-event-resourcestatusreason-carrying-an.md, neither of which carries this replay key; 20261117T124526Z-cloudformation-s3vectors-2-net-new-causes-an-aws.md is the S3 Vectors index/dimension behavioral issue and does not cover this reason either.
Traces: cloudformation-cfns3cnt-create-failed-stack-keeps-live-bucket#task#5.

## Evidence
Cause 1 — fixture cloudformation-deletestack-retainresources-non-empty-bucket, step teardown#9, operation DeleteStack: expected HTTP 200 (empty DeleteStackResult), actual HTTP 400. Same cluster: stack-policy-input-validation#teardown#11 (expected 200, got 400). Divergence key service=cloudformation|DeleteStack|status=200->400|keys=status.
Cause 2 — fixture cloudformation-cfns3cnt-create-failed-stack-keeps-live-bucket, step task#5, operation ListStackResources, HTTP 200 both sides: $.ListStackResourcesResponse.ListStackResourcesResult.StackResourceSummaries.member[LogicalResourceId="Vec"].ResourceStatusReason: value_mismatch — expected the recorded (shape-masked) reason '<shaped-not-asserted>', actual 'Resource handler returned message: "Invalid vector bucket name" (RequestToken: 6bade315-1537-d743-f5af-3f987f4f94b2, HandlerErrorCode: InvalidRequest)'. The RequestToken uuid is a per-run value and is not the finding; the missing relayed service-exception suffix inside the quoted handler message is.

## Steps to Reproduce
Cause 1: replay fixture cloudformation-deletestack-retainresources-non-empty-bucket and run step teardown#9 (also stack-policy-input-validation#teardown#11). Minimal repro: CreateStack a template with an AWS::S3::Bucket, put an object in the bucket, DeleteStack and let it reach DELETE_FAILED, then call DeleteStack again with RetainResources naming that bucket's logical id — AWS answers 200 and the stack goes DELETE_COMPLETE with the bucket retained; the emulator answers 400.
Cause 2: replay fixture cloudformation-cfns3cnt-create-failed-stack-keeps-live-bucket step task#5. Minimal repro: CreateStack a template with an AWS::S3::Bucket plus an AWS::S3Vectors::VectorBucket whose name is invalid, wait for the create to fail, then call ListStackResources on the stack and read the failed resource's ResourceStatusReason.
