Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__kms__redshift.cfn-redshift-cluster-invalid-kms-key-id-rejected cloudformation__kms__redshift.cfn-redshift-cluster-kms-key-id-round-trip-and-endpoint-getatt
Issue-Key: cloudformation__kms__redshift.cfn-redshift-cluster-invalid-kms-key-id-rejected|run#14|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", (
Issue-Key: cloudformation__kms__redshift.cfn-redshift-cluster-kms-key-id-round-trip-and-endpoint-getatt|run#15|AssertionError|assert got == want, f"DescribeClusters read {got!r}, expected {want!r}"
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# cloudformation__kms__redshift: 2 net-new causes — an AWS::Redshift::Cluster naming a nonexistent KmsKeyId CREATE_COMPLETEs instead of rolling back, and DescribeClusters.KmsKeyId echoes the bare key id instead of the key ARN

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two distinct root causes in the CloudFormation provisioning path for AWS::Redshift::Cluster's KMS encryption properties. Both are net-new: the two SnapshotCopyGrant failures in this service group are OWNED by 20261114T172233Z-cloudformation-kms-redshift-2-net-new-causes-aws.md and are not part of this issue.

Cause 1 of 2: an AWS::Redshift::Cluster whose KmsKeyId names a key KMS reports as nonexistent is provisioned anyway, so the stack reaches CREATE_COMPLETE instead of ROLLBACK_COMPLETE.
  AWS: Smithy (service-com-amazonaws-redshift-redshiftserviceversion20121201): CreateCluster takes KmsKeyId and the cluster resource must exist in KMS; CreateCluster's modelled client errors include InvalidParameterValue (httpError 400) for an encryption key that cannot be used. Recorded behavior: the check first proves the key is genuinely absent — KMS DescribeKey on that key id refuses with NotFoundException / "Key 'arn:<partition>:kms:<region>:<account>:key/<keyId>' does not exist" — and then requires the stack to end ROLLBACK_COMPLETE with the Cluster resource's CREATE_FAILED ResourceStatusReason being, verbatim apart from its variable parts: 'Resource handler returned message: "Unable to create encrypted cluster. The key '<kmsKeyId>' is inaccessible. (Service: Redshift, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: GeneralServiceException)'. Fixed text: 'Unable to create encrypted cluster. The key ', "' is inaccessible.", the '(Service: Redshift, Status Code: 400, ...)' SDK envelope and 'HandlerErrorCode: GeneralServiceException'. Variable parts: the KmsKeyId taken from the template, and the request id / attempt count / request token, which are per-run values and are NOT requirements. Afterwards DescribeClusters on the cluster identifier must refuse with ClusterNotFound / 'Cluster <clusterIdentifier> not found.'
  Emulator: the stack ends CREATE_COMPLETE with an empty failure-reason list, i.e. the cluster is created with an unresolvable KMS key and no CREATE_FAILED event is ever emitted.
  Why: a client that encodes its encryption-key wiring in a template gets a green stack and a cluster whose Encrypted/KmsKeyId values point at a key that does not exist. Every negative deployment test of KMS wiring passes vacuously, and rollback-driven cleanup never runs, so the caller believes an encrypted cluster was created against a key it never granted.
  Fix: the CreateCluster handler's validation — it must resolve KmsKeyId against KMS state and refuse with the inaccessible-key InvalidParameterValue before persisting the cluster; the CloudFormation AWS::Redshift::Cluster resource provider then surfaces that refusal as a CREATE_FAILED with the SDK-envelope reason and GeneralServiceException handler code, driving the stack to ROLLBACK_COMPLETE.
  Traces: cloudformation__kms__redshift.cfn-redshift-cluster-invalid-kms-key-id-rejected.

Cause 2 of 2: DescribeClusters reports Cluster.KmsKeyId as the bare KMS key id rather than the key's full ARN.
  AWS: Smithy: Cluster.KmsKeyId is a String with no further constraint, so the model does not fix the form — recorded behavior only. The recording shows that when the template supplies KmsKeyId from an AWS::KMS::Key Ref (which resolves to the bare key id), Redshift stores and reports the key's full ARN, so DescribeClusters reads back 'arn:<partition>:kms:<region>:<account>:key/<keyId>'. The check's expectation is the stack's own KeyArn output.
  Emulator: DescribeClusters returns the bare key UUID it received — read tuple ('ra3.large', '<admin>', True, '<keyId>') where ('ra3.large', '<admin>', True, 'arn:aws:kms:<region>:<account>:key/<keyId>') was required. Only the KmsKeyId element diverges; the node type, admin username and Encrypted flag all match.
  Why: a client that feeds Cluster.KmsKeyId straight into a KMS call, an IAM key-policy condition or a cross-account snapshot-copy configuration — all of which require a key ARN — gets a value KMS cannot resolve in those positions, and any comparison against a CloudFormation GetAtt Arn output or a kms:DescribeKey Arn fails.
  Fix: the CreateCluster handler's state (key-id normalisation on persist) and the DescribeClusters serializer for Cluster.KmsKeyId — normalise a supplied bare key id or alias to the key's ARN at creation time and report that ARN.
  Traces: cloudformation__kms__redshift.cfn-redshift-cluster-kms-key-id-round-trip-and-endpoint-getatt.

## Evidence
Cause 1 — cloudformation__kms__redshift.cfn-redshift-cluster-invalid-kms-key-id-rejected:
  Expected: stack status ROLLBACK_COMPLETE, with the Cluster resource CREATE_FAILED and ResourceStatusReason 'Resource handler returned message: "Unable to create encrypted cluster. The key '<kmsKeyId>' is inaccessible. (Service: Redshift, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: GeneralServiceException)'; then DescribeClusters -> ClusterNotFound / 'Cluster <clusterIdentifier> not found.'
  Actual: AssertionError: bad stack arn:aws:cloudformation:<region>:<account>:stack/<name>/<uuid> ended 'CREATE_COMPLETE': [] — no CREATE_FAILED event at all. The precondition that the key is absent did hold: KMS DescribeKey of it refused with NotFoundException / "Key 'arn:aws:kms:<region>:<account>:key/<keyId>' does not exist".

Cause 2 — cloudformation__kms__redshift.cfn-redshift-cluster-kms-key-id-round-trip-and-endpoint-getatt:
  Template property: "KmsKeyId": {"Ref": "Key"} on AWS::Redshift::Cluster (an AWS::KMS::Key Ref, i.e. the bare key id).
  Expected: DescribeClusters -> (NodeType, MasterUsername, Encrypted, KmsKeyId) == ('ra3.large', 's2rcfnadmin', True, 'arn:aws:kms:us-west-2:<account>:key/<keyId>') — the ARN equal to the stack's KeyArn output.
  Actual: ('ra3.large', 's2rcfnadmin', True, '<keyId>') — the bare key id, not the ARN.

## Steps to Reproduce
Cause 1:
1. CreateStack with an AWS::Redshift::Cluster whose Encrypted is true and whose KmsKeyId is a well-formed but nonexistent key id (e.g. 00000000-0000-0000-0000-000000000000).
2. Confirm KMS DescribeKey on that key id refuses with NotFoundException.
3. Poll DescribeStacks for a terminal status. Observe CREATE_COMPLETE with no CREATE_FAILED stack event; AWS reaches ROLLBACK_COMPLETE with the inaccessible-key handler reason, and DescribeClusters on the identifier then refuses with ClusterNotFound.

Cause 2:
1. CreateStack with an AWS::KMS::Key plus an AWS::Redshift::Cluster whose KmsKeyId is {"Ref": "<Key>"}, and a stack Output exporting GetAtt <Key>.Arn.
2. Wait for CREATE_COMPLETE, then call DescribeClusters on the cluster identifier.
3. Compare Clusters[0].KmsKeyId to the stack's KeyArn output. Observe the bare key id instead of the key ARN.
