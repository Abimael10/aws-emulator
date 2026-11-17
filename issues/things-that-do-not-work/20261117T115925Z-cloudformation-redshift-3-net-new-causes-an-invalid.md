Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__redshift.cfn-redshift-cluster-invalid-iam-role-arn-rejected cloudformation__redshift.cfn-redshift-cluster-nonexistent-clusterparametergroupname-rejected cloudformation__redshift.deploy-redshift-clusterparametergroup-invalid-name-rejected
Issue-Key: cloudformation__redshift.cfn-redshift-cluster-invalid-iam-role-arn-rejected|run#9|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", (
Issue-Key: cloudformation__redshift.cfn-redshift-cluster-nonexistent-clusterparametergroupname-rejected|run#12|AssertionError|assert got == want, f"handler reason {got!r}, expected {want!r}"
Issue-Key: cloudformation__redshift.deploy-redshift-clusterparametergroup-invalid-name-rejected|run#10|AssertionError|assert got == _REJECTED, f"handler reason {got!r}, expected {_REJECTED!r}"
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# cloudformation__redshift: 3 net-new causes — an invalid IamRoles value on AWS::Redshift::Cluster CREATE_COMPLETEs, resource-failure reasons omit the AWS SDK envelope and carry the wrong HandlerErrorCode, and the ClusterParameterGroup invalid-name refusal is emulator-authored

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three distinct root causes in CloudFormation provisioning of Redshift resources. All are net-new: the other failures under this service heading are OWNED by 20261109T125831Z-cloudformation-redshift-4-net-new-causes-aws-redshift.md and 20261113T040725Z-cloudformation-redshift-an-invalid-aws-redshift-clusterparametergroup-parametergroupfamily.md and are excluded here.

Cause 1 of 3: an AWS::Redshift::Cluster whose IamRoles holds a value that is not an IAM role ARN is provisioned anyway, so the stack reaches CREATE_COMPLETE instead of ROLLBACK_COMPLETE.
  AWS: Smithy: CreateCluster.IamRoles targets IamRoleArnList; ModifyClusterIamRoles/CreateCluster refuse a value that is not a Redshift-delegable IAM role ARN with InvalidParameterValue (httpError 400). Recorded behavior: the stack must end ROLLBACK_COMPLETE and the Cluster resource's CREATE_FAILED ResourceStatusReason must be 'Resource handler returned message: "The IAM role <iamRoleValue> is not valid. The IAM role must delegate access to an Amazon Redshift account. (Service: Redshift, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: GeneralServiceException)'. Fixed text: 'The IAM role ', ' is not valid. The IAM role must delegate access to an Amazon Redshift account.' and the SDK envelope; variable part: the offending IamRoles value copied from the request (the recording's literal 'not-an-arn-at-all' is one such value, not part of the template). DescribeClusters on the identifier must then refuse with ClusterNotFound / 'Cluster <clusterIdentifier> not found.'
  Emulator: the stack ends CREATE_COMPLETE with an empty failure-reason list — the cluster is created with a nonsense IAM role value and no CREATE_FAILED event is produced.
  Why: a client gets a green stack and a cluster whose IamRoles list cannot be assumed, so every COPY/UNLOAD or Lakehouse integration that relies on that association fails later at use time with no deployment-time signal. Negative deploy tests of IAM wiring pass vacuously. This is the CloudFormation-visible face of the acceptance gap also noted in 20261103T040731Z-redshift-createcluster-admits-invalid-requests-and-persists-the.md, which does not carry this check's replay key.
  Fix: the CreateCluster handler's validation of the IamRoles member (reject a value that is not an IAM role ARN delegating to Redshift before persisting the cluster), plus the AWS::Redshift::Cluster resource provider surfacing that refusal as a CREATE_FAILED that drives rollback.
  Traces: cloudformation__redshift.cfn-redshift-cluster-invalid-iam-role-arn-rejected.

Cause 2 of 3: CloudFormation resource-failure reasons for Redshift resources omit the AWS SDK envelope and report the wrong HandlerErrorCode.
  AWS: Recorded behavior only (CloudFormation reason text is not modelled in the Redshift Smithy model). A resource whose provider call was refused by Redshift carries a ResourceStatusReason containing the service refusal message followed by '(Service: Redshift, Status Code: <httpStatus>, Request ID: <id>) (SDK Attempt Count: <n>)', and the event's handler error code is the class CloudFormation maps that refusal to — 'InvalidRequest' for a not-found parameter-group reference (the Redshift refusal is a 404, per Smithy ClusterParameterGroupNotFoundFault httpError 404) and 'GeneralServiceException' for a malformed-identifier refusal (a 400). Fixed text: the SDK envelope's labels 'Service:', 'Status Code:', 'Request ID:', 'SDK Attempt Count:'. Variable parts: the service name, the HTTP status of the refusal, and the request id / attempt count, which are per-run values and are not themselves requirements.
  Emulator: the reason is the bare refusal message with no SDK envelope, and the handler error code is wrong in both directions — 'NotFound' where AWS reports 'InvalidRequest' (nonexistent ClusterParameterGroupName), and 'InvalidRequest' where AWS reports 'GeneralServiceException' (invalid parameter-group name). Observed: reason ('ClusterParameterGroup not found: <pgName>', 'NotFound') against required ('ClusterParameterGroup not found: <pgName> (Service: Redshift, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest').
  Why: clients and CDK/IaC tooling parse ResourceStatusReason and branch on HandlerErrorCode to decide whether a failure is retryable, a client mistake or a service fault. A missing envelope breaks the parse, and a handler code in the wrong class routes the failure down the wrong remediation path.
  Fix: the CloudFormation resource-provider error-mapping layer for Redshift resources — wrap the service refusal in the SDK envelope and map the refusal's HTTP status/error class to the HandlerErrorCode CloudFormation reports.
  Traces: cloudformation__redshift.cfn-redshift-cluster-nonexistent-clusterparametergroupname-rejected, cloudformation__redshift.deploy-redshift-clusterparametergroup-invalid-name-rejected.

Cause 3 of 3: the invalid-parameter-group-name refusal carries an emulator-authored message instead of AWS's identifier-rule wording.
  AWS: Smithy: CreateClusterParameterGroup.ParameterGroupName is a String the service validates as an identifier. Recorded behavior: a name containing characters outside the identifier rule is refused with the message 'The parameter DBParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.' — that whole sentence is fixed text naming the member as DBParameterGroupName, with no interpolation of the offending value at all.
  Emulator: refuses with its own wording that interpolates the value and states the rule differently: "Invalid value for ParameterGroupName: '<name>'. The identifier contains a character that is neither alphanumeric nor a hyphen. (allowed: 1 to 255 alphanumeric characters or hyphens, first character a letter, no trailing hyphen and no two consecutive hyphens)".
  Why: a client matching on AWS's documented sentence (or on the member name DBParameterGroupName it reports) does not recognise the refusal, so error-class detection and user-facing remediation text both break.
  Fix: the CreateClusterParameterGroup handler's ParameterGroupName validation message.
  Traces: cloudformation__redshift.deploy-redshift-clusterparametergroup-invalid-name-rejected.

## Evidence
Cause 1 — cloudformation__redshift.cfn-redshift-cluster-invalid-iam-role-arn-rejected:
  Expected: stack ROLLBACK_COMPLETE; Cluster CREATE_FAILED reason 'Resource handler returned message: "The IAM role <value> is not valid. The IAM role must delegate access to an Amazon Redshift account. (Service: Redshift, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: GeneralServiceException)'.
  Actual: AssertionError: bad stack arn:aws:cloudformation:<region>:<account>:stack/<name>/<uuid> ended 'CREATE_COMPLETE': [] — no failure events.

Cause 2 — cloudformation__redshift.cfn-redshift-cluster-nonexistent-clusterparametergroupname-rejected:
  Expected (reason, handlerErrorCode): ('ClusterParameterGroup not found: <pgName> (Service: Redshift, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')
  Actual: ('ClusterParameterGroup not found: <pgName>', 'NotFound')
  Also cloudformation__redshift.deploy-redshift-clusterparametergroup-invalid-name-rejected: handler error code 'InvalidRequest' where 'GeneralServiceException' is required, and the reason again lacks the '(Service: Redshift, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' envelope.

Cause 3 — cloudformation__redshift.deploy-redshift-clusterparametergroup-invalid-name-rejected:
  Expected message: 'The parameter DBParameterGroupName is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.'
  Actual message: "Invalid value for ParameterGroupName: '<name>'. The identifier contains a character that is neither alphanumeric nor a hyphen. (allowed: 1 to 255 alphanumeric characters or hyphens, first character a letter, no trailing hyphen and no two consecutive hyphens)"

## Steps to Reproduce
Cause 1:
1. CreateStack with an AWS::Redshift::Cluster whose IamRoles is a single value that is not an IAM role ARN (any non-ARN string).
2. Poll DescribeStacks for a terminal status. Observe CREATE_COMPLETE; AWS reaches ROLLBACK_COMPLETE and the Cluster CREATE_FAILED reason names the invalid IAM role with HandlerErrorCode GeneralServiceException.
3. DescribeClusters on the identifier: on AWS it refuses with ClusterNotFound; here the cluster exists.

Cause 2:
1. CreateStack with an AWS::Redshift::Cluster whose ClusterParameterGroupName names a group that does not exist.
2. Wait for the stack to roll back, read DescribeStackEvents and take the single CREATE_FAILED row for the Cluster resource.
3. Observe the ResourceStatusReason has no '(Service: Redshift, Status Code: ..., Request ID: ...) (SDK Attempt Count: ...)' envelope and the handler error code is 'NotFound' rather than 'InvalidRequest'. Repeat with an AWS::Redshift::ClusterParameterGroup whose ParameterGroupName is a malformed identifier and observe 'InvalidRequest' rather than 'GeneralServiceException'.

Cause 3:
1. Call CreateClusterParameterGroup (or deploy an AWS::Redshift::ClusterParameterGroup) with ParameterGroupName containing an underscore, e.g. a name of the form <Word>_<Word>_<Word>.
2. Read the refusal message. Observe the emulator's own wording instead of AWS's 'The parameter DBParameterGroupName is not a valid identifier. ...' sentence.
