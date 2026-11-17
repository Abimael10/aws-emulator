Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__elbv2__s3.cfn-alb-access-logs-enabled-without-bucket-policy-grant-rolls-back-stack
Issue-Key: cloudformation__elbv2__s3.cfn-alb-access-logs-enabled-without-bucket-policy-grant-rolls-back-stack|run#10|AssertionError|assert status == "ROLLBACK_COMPLETE", f"the ungranted-bucket stack reached {status!r}"
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# cloudformation__elbv2__s3: the stack that enables ALB access logs to an ungranted bucket ends ROLLBACK_FAILED instead of ROLLBACK_COMPLETE

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a stack whose AWS::ElasticLoadBalancingV2::LoadBalancer enables access logging to an S3 bucket that does not grant the ELB log-delivery principal must fail the resource and roll back cleanly; the emulator's rollback itself fails.

AWS: recorded behavior only for the rollback outcome — CloudFormation's documented contract is that a resource whose create fails triggers rollback and, when every rollback step succeeds, the stack settles in ROLLBACK_COMPLETE (the reason being the relayed resource-handler envelope `Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)`). ROLLBACK_FAILED is the status AWS reserves for a rollback that could not undo a resource. The recording asserts the ungranted-bucket stack reaches ROLLBACK_COMPLETE, i.e. the failed access-logs load balancer is cleanly removed; its sibling with a bucket policy granting logdelivery.elasticloadbalancing.amazonaws.com succeeds.

Emulator: the stack reaches 'ROLLBACK_FAILED'. The create failure is produced (so the access-log permission check exists at some level) but the subsequent rollback cannot delete what it created, leaving the stack wedged in a non-recoverable status with resources behind it.

Why: a client's failed deploy becomes unrecoverable. ROLLBACK_FAILED is terminal-but-dirty: DeleteStack is required with RetainResources, CI pipelines that treat ROLLBACK_COMPLETE as "cleanly reverted, safe to retry" now stall, and the leaked load balancer/bucket keep costing and keep blocking the next deploy on name collisions.

Fix: the CloudFormation rollback path and the delete half of the ELBv2 load balancer resource handler — the resources created before the access-logs failure must be deletable during rollback so the stack settles ROLLBACK_COMPLETE.

Traces: cloudformation__elbv2__s3.cfn-alb-access-logs-enabled-without-bucket-policy-grant-rolls-back-stack.

Related but not covering: 20261109T153103Z-elbv2-s3-2-net-new-causes-modifyloadbalancerattributes-enables.md covers ModifyLoadBalancerAttributes not validating the bucket's delivery permission on the direct API; this cause is the CloudFormation rollback ending FAILED.

## Evidence
Expected: the stack whose load balancer sets access_logs.s3.enabled=true against a bucket with no policy statement for logdelivery.elasticloadbalancing.amazonaws.com fails the resource and the stack settles 'ROLLBACK_COMPLETE'.

Actual (replay):
  AssertionError: the ungranted-bucket stack reached 'ROLLBACK_FAILED'
  (check_cloudformation__elbv2__s3_cfn_alb_access_logs_enabled_without_bucket_policy_grant_rolls_back_stack.py line 209)

## Steps to Reproduce
1. CreateStack with an S3 bucket WITHOUT a bucket policy granting logdelivery.elasticloadbalancing.amazonaws.com s3:PutObject, a VPC with two subnets, and an AWS::ElasticLoadBalancingV2::LoadBalancer whose LoadBalancerAttributes set access_logs.s3.enabled=true and access_logs.s3.bucket to that bucket.
2. Poll DescribeStacks until a terminal status and read the failing resource's ResourceStatusReason.
Observed: StackStatus 'ROLLBACK_FAILED'. Expected: 'ROLLBACK_COMPLETE' with the relayed handler-failure reason.
3. Repeat with a bucket policy that grants the log-delivery principal: that stack must reach CREATE_COMPLETE.
