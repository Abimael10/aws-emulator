Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__codebuild__s3.deploy-cfn-codebuild-project-cache-s3-without-location-rejected
Issue-Key: cloudformation__codebuild__s3.deploy-cfn-codebuild-project-cache-s3-without-location-rejected|run#23|AssertionError|assert status == "CREATE_FAILED", f"expected stack status CREATE_FAILED, got {status!r}"
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# CodeBuild CreateProject does not validate an S3 cache without a location, so a CloudFormation AWS::CodeBuild::Project with Cache.Type=S3 and no Location reaches CREATE_COMPLETE and the project is minted

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateProject

## Description
ONE net-new root cause, grounded in the single failing check of this sweep for this service that the suite report does not mark ALREADY FILED or OWNED.

Cause 1 of 1: CodeBuild's CreateProject accepts a `cache` whose `type` is `S3` but which carries no `location`, instead of refusing the request; consequently a CloudFormation stack declaring AWS::CodeBuild::Project with `Cache: {"Type": "S3"}` and no `Location` deploys to CREATE_COMPLETE and the project is created, where AWS fails the resource and creates nothing.

AWS: Smithy (services/service-com-amazonaws-codebuild-codebuild-20161006/smithy-model/com-amazonaws-codebuild-codebuild-20161006.json) — CreateProject input member `cache` targets ProjectCache{type @required CacheType enum [NO_CACHE,S3,LOCAL], location String, modes, cacheNamespace}; ProjectCache.location is documented as "S3: This is the S3 bucket name/prefix" and is ignored only for NO_CACHE/LOCAL. CreateProject models InvalidInputException (errorKind client, httpError 400) for input that is not valid. Recorded behavior for this input class: CodeBuild refuses the call with HTTP 400 and the message "Invalid cache: location must be a valid S3 bucket, followed by slash and the prefix" — wholly fixed text with no request-derived part. Through CloudFormation the same refusal surfaces as the Project resource going CREATE_FAILED with exactly one CREATE_FAILED event whose ResourceStatusReason is the resource-handler wrapper: `Resource handler returned message: "<that CodeBuild message> (Service: CodeBuild, Status Code: 400, Request ID: <request id>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)` — the fixed parts being the wrapper itself, the service name `CodeBuild`, the status code `400` and HandlerErrorCode `InvalidRequest`; the request id and request token are per-run values. The project is never created, so BatchGetProjects(names=[<that name>]) answers projects=[] and projectsNotFound=[<that name>].

Emulator: the stack with the location-less S3 cache polls to terminal status CREATE_COMPLETE (DisableRollback=true, so an AWS failure would have landed as CREATE_FAILED), i.e. no CREATE_FAILED event and no resource-handler reason is produced at all, and the project is minted. The positive control in the same check — the identical template whose cache carries `Location: <bucket>/<prefix>` — reaches CREATE_COMPLETE and reports that cache location correctly, so the divergence is exactly the missing validation of a location-less S3 cache, not the cache feature as a whole.

Why: a client relying on AWS's behavior believes a mis-declared cache is caught at deploy time. Here the template applies cleanly and a project is created whose S3 cache can never resolve a bucket, so caching silently never happens and the misconfiguration is only discovered later (or never). Infrastructure-as-code tests that assert a bad template fails, and callers that branch on CREATE_FAILED plus the HandlerErrorCode InvalidRequest, take the wrong branch.

Fix: the cache validation in the CodeBuild CreateProject handler (reject cache.type == S3 with an absent/empty location with InvalidInputException and AWS's message text), and consequently the CloudFormation AWS::CodeBuild::Project resource handler must propagate that service refusal as a CREATE_FAILED event whose reason is the resource-handler wrapper with Service CodeBuild, Status Code 400 and HandlerErrorCode InvalidRequest. Note: 20261108T040031Z-codebuild-project-refusal-messages-diverge-from-aws-wording.md and 20261107T225801Z-codebuild-6-net-new-causes-builds-and-sandbox.md (its cause 5) cover CreateProject refusal MESSAGE wording for a service role and for an S3 SOURCE without a location; neither carries this check's key and neither covers the cache validation, which the emulator does not perform at all.

Traces: cloudformation__codebuild__s3.deploy-cfn-codebuild-project-cache-s3-without-location-rejected.

## Evidence
[mismatch] cloudformation__codebuild__s3.deploy-cfn-codebuild-project-cache-s3-without-location-rejected — AssertionError at check line 165: `assert status == "CREATE_FAILED", f"expected stack status CREATE_FAILED, got {status!r}"` -> "expected stack status CREATE_FAILED, got 'CREATE_COMPLETE'".

The check creates the stack with DisableRollback=True so a resource failure lands as CREATE_FAILED rather than ROLLBACK_COMPLETE, then polls describe_stacks for a terminal status within 180 s. The template is the minimal one: an IAM role trusting codebuild.amazonaws.com plus an AWS::CodeBuild::Project with Source {Type: NO_SOURCE, inline BuildSpec}, Artifacts {Type: NO_ARTIFACTS}, Environment {Type: LINUX_CONTAINER, ComputeType: BUILD_GENERAL1_SMALL, Image: aws/codebuild/standard:7.0} and Cache {"Type": "S3"} with no Location.

Because the stack completed, the assertions that follow were never reached: the single CREATE_FAILED event for logical id `Project` whose ResourceStatusReason must match `Resource handler returned message: "<message> (Service: CodeBuild, Status Code: 400, Request ID: ...)" (RequestToken: ..., HandlerErrorCode: InvalidRequest)` with message = "Invalid cache: location must be a valid S3 bucket, followed by slash and the prefix", and the follow-up BatchGetProjects showing the bad project in projectsNotFound with projects=[].

Positive control in the same check passed: the same template with Cache {"Type": "S3", "Location": "<bucket>/ci-cache"} reached CREATE_COMPLETE and BatchGetProjects reported cache.location == "<bucket>/ci-cache". Bucket/stack/project names in the run are per-run unique values, not requirements.

Smithy consulted: CreateProject (input member `cache` -> ProjectCache; errors AccountLimitExceededException / InvalidInputException / ResourceAlreadyExistsException, all client, httpError 400 defaulted), ProjectCache (type @required CacheType, location String documented as the S3 bucket name/prefix), CacheType enum [NO_CACHE, S3, LOCAL].

## Steps to Reproduce
1. git clone the repo and boot the emulator built from this checkout.
2. Direct API reproduction of the cause: iam CreateRole with a trust policy allowing codebuild.amazonaws.com; then codebuild CreateProject(name=<valid name>, source={type: NO_SOURCE, buildspec: <inline>}, artifacts={type: NO_ARTIFACTS}, environment={type: LINUX_CONTAINER, computeType: BUILD_GENERAL1_SMALL, image: aws/codebuild/standard:7.0}, serviceRole=<role arn>, cache={type: "S3"}) with NO location. Expect HTTP 400 InvalidInputException with message "Invalid cache: location must be a valid S3 bucket, followed by slash and the prefix"; observe the call succeed and the project be created.
3. CloudFormation reproduction (the failing check's path): cloudformation CreateStack with the template above (Capabilities=[CAPABILITY_IAM], DisableRollback=true) where the Project's Cache is {"Type": "S3"} with no Location; poll describe_stacks until terminal. Expect StackStatus CREATE_FAILED with exactly one CREATE_FAILED event for logical id `Project` whose ResourceStatusReason is `Resource handler returned message: "Invalid cache: location must be a valid S3 bucket, followed by slash and the prefix (Service: CodeBuild, Status Code: 400, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)`; observe CREATE_COMPLETE.
4. codebuild BatchGetProjects(names=[<the bad project name>]) — expect projects=[] and projectsNotFound=[<that name>]; observe the project present.
5. Control: repeat step 3 with Cache {"Type": "S3", "Location": "<existing bucket>/ci-cache"} — this must still reach CREATE_COMPLETE with cache.location reported, and already does.
