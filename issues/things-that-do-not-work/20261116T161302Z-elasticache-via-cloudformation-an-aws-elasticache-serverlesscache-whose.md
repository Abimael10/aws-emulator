Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.deploy-serverless-cache-cfn-invalid-engine-version-rejected
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.deploy-serverless-cache-cfn-invalid-engine-version-rejected|run#15|AssertionError|assert status == "ROLLBACK_COMPLETE", f"expected rollback, stack ended in {status}"
Issue-Commit: 68169c86c7603a2902f84035dfc94fd02b30ddfc

# ElastiCache via CloudFormation: an AWS::ElastiCache::ServerlessCache whose MajorEngineVersion does not exist deploys to CREATE_COMPLETE instead of failing the resource and rolling the stack back

> _Note: this issue was identified against the code base at commit `68169c86c7603a2902f84035dfc94fd02b30ddfc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServerlessCache

## Description
Cause 1 of 1: the CloudFormation provisioning path for AWS::ElastiCache::ServerlessCache does not apply CreateServerlessCache's MajorEngineVersion validation, so a template CloudFormation must fail on is provisioned successfully. Failing check id: elasticache.deploy-serverless-cache-cfn-invalid-engine-version-rejected.

AWS: ElastiCache validates MajorEngineVersion against the versions the requested Engine supports. CreateServerlessCache (Smithy: services/service-com-amazonaws-elasticache-amazonelasticachev9 — input member MajorEngineVersion: String, optional; modeled error InvalidParameterCombinationException, awsQueryError code "InvalidParameterCombination", HTTP 400) refuses a MajorEngineVersion the engine does not offer with 400 / InvalidParameterCombination and the message "Specified engine does not support major engine version (<MajorEngineVersion>)." — the fixed text is "Specified engine does not support major engine version (" + ")" with a trailing period, and the only variable part is the MajorEngineVersion value echoed from the request. No cache is created. Recorded behavior for the CloudFormation leg: because the underlying create call is refused, the AWS::ElastiCache::ServerlessCache resource fails, and a stack created with OnFailure="ROLLBACK" settles in ROLLBACK_COMPLETE with no serverless cache left behind — a subsequent DescribeServerlessCaches for that name answers 404 ServerlessCacheNotFoundFault with "Serverless cache with name <ServerlessCacheName> not found."

Emulator: the direct CreateServerlessCache call IS refused correctly (the check's assertions on code InvalidParameterCombination and that message pass). But the same property delivered through CloudFormation is not validated: CreateStack with a template whose single AWS::ElastiCache::ServerlessCache carries MajorEngineVersion="999.999" (a value with no supporting engine) reaches terminal StackStatus "CREATE_COMPLETE" instead of "ROLLBACK_COMPLETE" (replay: AssertionError: expected rollback, stack ended in CREATE_COMPLETE). The CFN resource provider therefore either drops MajorEngineVersion or provisions the serverless cache without routing the request through the ElastiCache CreateServerlessCache validation that the direct API path performs.

Why: a client relying on AWS's behavior gets the opposite verdict on a deploy. Infrastructure pipelines use CloudFormation rollback as the gate on an invalid engine version: against AWS a bad MajorEngineVersion fails the resource and the stack unwinds, so the pipeline stops; against the emulator the stack reports CREATE_COMPLETE and the pipeline proceeds believing a cache on a supported engine version exists. Negative-path templates can never be exercised, and the stack's reported state disagrees with what the ElastiCache control plane would accept for the identical parameters.

Fix: the AWS::ElastiCache::ServerlessCache resource provider in the CloudFormation provisioning path must create the cache through the same CreateServerlessCache handler/validation that the direct API uses (passing MajorEngineVersion through), so its InvalidParameterCombination refusal surfaces as a resource CREATE_FAILED with the ElastiCache message as the resource status reason, and the stack transitions to ROLLBACK_COMPLETE with no serverless cache persisted in ElastiCache state.

Traces: elasticache.deploy-serverless-cache-cfn-invalid-engine-version-rejected (suite heading: ### elasticache).

## Evidence
Check: elasticache.deploy-serverless-cache-cfn-invalid-engine-version-rejected [mismatch]

Step that passes (direct API): ec.create_serverless_cache(ServerlessCacheName=<bad-name>, Engine="redis", MajorEngineVersion="999.999", SubnetIds=[...]) -> ClientError code "InvalidParameterCombination", message "Specified engine does not support major engine version (999.999)." (matches AWS).

Step that diverges (CloudFormation):
  cfn.create_stack(StackName=<bad-stack>, OnFailure="ROLLBACK", TemplateBody={"Resources":{"ServerlessCache":{"Type":"AWS::ElastiCache::ServerlessCache","Properties":{"ServerlessCacheName":<bad-name>,"Engine":"redis","SubnetIds":[...2 subnets...],"SecurityGroupIds":[<sg>],"MajorEngineVersion":"999.999"}}}})
  then poll DescribeStacks until terminal.

  Expected (AWS): StackStatus == "ROLLBACK_COMPLETE", and DescribeServerlessCaches(ServerlessCacheName=<bad-name>) -> 404 ServerlessCacheNotFoundFault "Serverless cache with name <bad-name> not found."
  Actual (emulator): StackStatus == "CREATE_COMPLETE".

  Replay output:
    File "check_elasticache_deploy_serverless_cache_cfn_invalid_engine_version_rejected.py", line 48, in run
      assert status == "ROLLBACK_COMPLETE", f"expected rollback, stack ended in {status}"
    AssertionError: expected rollback, stack ended in CREATE_COMPLETE

Model grounding: CreateServerlessCache declares MajorEngineVersion (String) and the error InvalidParameterCombinationException -> awsQueryError code "InvalidParameterCombination", HTTP 400.

(Names/ids above — stack names, cache names, subnet and security-group ids — are per-run values; the requirement is the status/refusal behavior, not these literals.)

## Steps to Reproduce
1. Boot the emulator and create (or reuse) a VPC with at least two subnets and one security group.
2. Confirm the direct refusal: CreateServerlessCache(ServerlessCacheName=<name>, Engine="redis", MajorEngineVersion="999.999", SubnetIds=[<2+ subnets>]) -> 400 InvalidParameterCombination, "Specified engine does not support major engine version (999.999)." (this already matches AWS).
3. CreateStack with OnFailure="ROLLBACK" and TemplateBody {"Resources":{"ServerlessCache":{"Type":"AWS::ElastiCache::ServerlessCache","Properties":{"ServerlessCacheName":"<same-name>","Engine":"redis","MajorEngineVersion":"999.999","SubnetIds":[<2 subnets>],"SecurityGroupIds":["<sg-id>"]}}}}.
4. Poll DescribeStacks(StackName=<stack>) until the StackStatus is terminal.
5. Observe StackStatus "CREATE_COMPLETE"; AWS reaches "ROLLBACK_COMPLETE" (the resource fails with the ElastiCache InvalidParameterCombination message as its status reason) and leaves no serverless cache, so DescribeServerlessCaches(ServerlessCacheName=<name>) answers 404 ServerlessCacheNotFoundFault.
