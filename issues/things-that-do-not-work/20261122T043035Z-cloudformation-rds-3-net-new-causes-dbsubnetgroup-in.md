Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__rds.cfn-rds-db-subnet-group-create-update-in-place-replace-retained-then-delete cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back cloudformation__rds.deploy-rds-db-cluster-endpoint-round-trip-and-delete
Issue-Key: cloudformation__rds.cfn-rds-db-subnet-group-create-update-in-place-replace-retained-then-delete|_update#20|AssertionError|assert stack["StackStatus"] == "UPDATE_COMPLETE", (
Issue-Key: cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back|_error#12|AssertionError|raise AssertionError(f"expected a ClientError but the call succeeded: {result!r}")
Issue-Key: cloudformation__rds.deploy-rds-db-cluster-endpoint-round-trip-and-delete|run#11|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Commit: 622c214bd0a84106899ec4445dc3396f8892f16d

# cloudformation__rds: 3 net-new causes — DBSubnetGroup in-place update answers NotImplemented, a rolled-back DBParameterGroup survives in RDS, and AWS::RDS::DBClusterEndpoint cannot be created

> _Note: this issue was identified against the code base at commit `622c214bd0a84106899ec4445dc3396f8892f16d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBClusterEndpoint

## Description
Three independent net-new root causes in CloudFormation's RDS resource handlers. Each must be closed separately.

=== Cause 1 of 3: an in-place update of AWS::RDS::DBSubnetGroup is refused as NotImplemented, so the stack rolls back ===
Failing check: cloudformation__rds.cfn-rds-db-subnet-group-create-update-in-place-replace-retained-then-delete
AWS:      Smithy (rds): ModifyDBSubnetGroup(DBSubnetGroupName required, DBSubnetGroupDescription, SubnetIds required) -> DBSubnetGroup, with modeled refusals DBSubnetGroupNotFoundFault (404), DBSubnetGroupDoesNotCoverEnoughAZs (400), InvalidSubnet (400), SubnetAlreadyInUse (400), DBSubnetQuotaExceededFault (400) — the group's description and subnet list ARE mutable. AWS::RDS::DBSubnetGroup therefore updates those properties with no interruption and no replacement: the update reaches UPDATE_COMPLETE, the group keeps its name and ARN, and DescribeDBSubnetGroups reflects the new values. Only a change to DBSubnetGroupName requires replacement.
Emulator: the update fails and the stack ends UPDATE_ROLLBACK_COMPLETE; the resource's UPDATE_FAILED reason is `NotImplemented: this emulator does not change the properties of an existing AWS::RDS::DBSubnetGroup in place; the resource was left as it is.` The stack-level reason is 'The following resource(s) failed to update: [SubnetGroup]. '.
Why:      a client cannot evolve a subnet group it provisioned — every stack update that touches the description or SubnetIds rolls back, and the emulator's reason is a self-describing NotImplemented sentence no AWS client can match or act on. Infrastructure-as-code flows that widen a subnet group's AZ coverage (a normal prerequisite for Multi-AZ) are blocked outright.
Fix:      the update path of the AWS::RDS::DBSubnetGroup resource handler in CloudFormation's resource provider — it must call the equivalent of ModifyDBSubnetGroup for description/SubnetIds changes and reach UPDATE_COMPLETE without replacement.
Traces:   cloudformation__rds.cfn-rds-db-subnet-group-create-update-in-place-replace-retained-then-delete (assertion at check line 270, from _update at line 80).

=== Cause 2 of 3: the rollback of a refused AWS::RDS::DBParameterGroup leaves the parameter group behind in RDS ===
Failing check: cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back
AWS:      Smithy (rds): DescribeDBParameterGroups models DBParameterGroupNotFoundFault with httpError 404 and awsQueryError code 'DBParameterGroupNotFound'; a name that does not exist is refused with Message `DBParameterGroup not found: <group name>` (fixed text `DBParameterGroup not found: `, variable part the DBParameterGroupName from the request). When a stack rolls back, every resource it created during the failed create is deleted, so a group whose create failed does not exist afterwards and the read is a 404.
Emulator: the refusal itself is correct — the stack reaches ROLLBACK_COMPLETE and the CREATE_FAILED reason carries RDS's exact 'Invalid parameter value: ... allowed values are: full,minimal,noblob' message with HandlerErrorCode InvalidRequest. But DescribeDBParameterGroups(DBParameterGroupName=<rolled-back group>) then answers HTTP 200 and returns the group (DBParameterGroupFamily mysql8.0, the template's Description, an ARN): the rollback created it and never removed it.
Why:      a rolled-back stack leaves an orphan parameter group in the account. The name is now taken, so a corrected redeploy under the same name collides; cleanup scripts that key on 'rollback means nothing was created' leak the resource; and 'does the group exist?' answers 200 where AWS answers 404 DBParameterGroupNotFound, so absence can never be observed.
Fix:      the rollback/delete path of the AWS::RDS::DBParameterGroup resource handler — a resource whose create failed must be deleted from RDS state during ROLLBACK, and the group must stop being returned by DescribeDBParameterGroups.
Traces:   cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back (assertion at check line 154). The open issue 20261117T174147Z-cloudformation-rds-3-net-new-causes-an-invalid.md covers an invalid MonitoringInterval reaching CREATE_COMPLETE and an uppercase binlog_row_image value rolling back; it does not carry this check's replay key and does not cover the orphan left by the rollback.

=== Cause 3 of 3: AWS::RDS::DBClusterEndpoint cannot be created, so the whole stack rolls back ===
Failing check: cloudformation__rds.deploy-rds-db-cluster-endpoint-round-trip-and-delete
AWS:      Smithy (rds): CreateDBClusterEndpoint(DBClusterIdentifier, DBClusterEndpointIdentifier, EndpointType required; StaticMembers, ExcludedMembers, Tags) -> DBClusterEndpoint carrying DBClusterEndpointArn, DBClusterIdentifier, DBClusterEndpointResourceIdentifier, Endpoint, EndpointType, CustomEndpointType, Status; its modeled refusals are DBClusterEndpointAlreadyExistsFault (400), DBClusterEndpointQuotaExceededFault (403), DBClusterNotFoundFault (404), DBInstanceNotFoundFault (404), InvalidDBClusterStateFault (400), InvalidDBInstanceStateFault (400) — none of which applies to a custom endpoint with CustomEndpointType ANY over a cluster the same stack just created successfully. AWS therefore reaches CREATE_COMPLETE with the endpoint Status 'available', Ref returning arn:<partition>:rds:<region>:<account>:cluster-endpoint:<endpoint id>, and the GetAtts Endpoint / EndpointType / DBClusterEndpointResourceIdentifier resolving to the values DescribeDBClusterEndpoints reports.
Emulator: the SubnetGroup and Cluster resources reach CREATE_COMPLETE, then the Endpoint resource goes CREATE_FAILED and the stack rolls back: event order ('SubnetGroup', CREATE_COMPLETE), ('Cluster', CREATE_COMPLETE), ('Endpoint', CREATE_FAILED), ('Endpoint', CREATE_FAILED), ('Endpoint', DELETE_COMPLETE), ('Cluster', DELETE_COMPLETE), ('SubnetGroup', DELETE_COMPLETE), (<stack>, ROLLBACK_COMPLETE). The replay's assertion captured the status chain only, so the ResourceStatusReason behind the CREATE_FAILED is not in the output; note also that TWO CREATE_FAILED rows are recorded for one resource where AWS records one.
Why:      custom DB cluster endpoints are unreachable through CloudFormation — any stack declaring one rolls back and takes the Aurora cluster with it, so read/write splitting and member-scoped endpoints cannot be provisioned as code at all.
Fix:      the AWS::RDS::DBClusterEndpoint create handler in CloudFormation's resource provider and the CreateDBClusterEndpoint path it calls (plus the duplicate CREATE_FAILED event row it emits).
Traces:   cloudformation__rds.deploy-rds-db-cluster-endpoint-round-trip-and-delete (assertion at check line 260). Related open issues 20261117T083457Z-rds-6-net-new-behavioral-causes-custom-db.md and 20261121T224824Z-rds-3-net-new-behavioral-causes-the-custom.md concern the custom DB-cluster-endpoint lifecycle on the rds service directly; neither carries this CloudFormation check's replay key.

## Evidence
Cause 1 — cloudformation__rds.cfn-rds-db-subnet-group-create-update-in-place-replace-retained-then-delete [mismatch]
  expected: UpdateStack changing DBSubnetGroupDescription/SubnetIds reaches UPDATE_COMPLETE with no replacement
  actual:   update ended 'UPDATE_ROLLBACK_COMPLETE': 'The following resource(s) failed to update: [SubnetGroup]. '; per-resource reason [('SubnetGroup', 'NotImplemented: this emulator does not change the properties of an existing AWS::RDS::DBSubnetGroup in place; the resource was left as it is.')]

Cause 2 — cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back [mismatch]
  The stack correctly reaches ROLLBACK_COMPLETE with the ParamGroup CREATE_FAILED reason 'Invalid parameter value: fully for: binlog_row_image allowed values are: full,minimal,noblob (Service: Rds, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' / InvalidRequest. Then:
  expected: DescribeDBParameterGroups(DBParameterGroupName=<bad group>) raises ('DBParameterGroupNotFound', 'DBParameterGroup not found: <bad group>')
  actual:   HTTP 200 {'DBParameterGroups': [{'DBParameterGroupName': '<bad group>', 'DBParameterGroupFamily': 'mysql8.0', 'Description': 'sim2real behavioral check', 'DBParameterGroupArn': 'arn:aws:rds:<region>:<account>:pg:<bad group>'}]}

Cause 3 — cloudformation__rds.deploy-rds-db-cluster-endpoint-round-trip-and-delete [mismatch]
  Template: AWS::RDS::DBSubnetGroup + Aurora AWS::RDS::DBCluster + AWS::RDS::DBClusterEndpoint { DBClusterEndpointIdentifier, DBClusterIdentifier: Ref Cluster, CustomEndpointType: 'ANY', Tags }.
  expected: stack CREATE_COMPLETE; DescribeDBClusterEndpoints filtered by the identifier returns exactly one endpoint with Status 'available', DBClusterEndpointArn == Ref(Endpoint) == arn:<partition>:rds:<region>:<account>:cluster-endpoint:<endpoint id>, CustomEndpointType 'ANY', and Endpoint/EndpointType/DBClusterEndpointResourceIdentifier equal to the stack's GetAtt outputs
  actual:   stack ended 'ROLLBACK_COMPLETE' with events [('SubnetGroup', 'CREATE_COMPLETE'), ('Cluster', 'CREATE_COMPLETE'), ('Endpoint', 'CREATE_FAILED'), ('Endpoint', 'CREATE_FAILED'), ('Endpoint', 'DELETE_COMPLETE'), ('Cluster', 'DELETE_COMPLETE'), ('SubnetGroup', 'DELETE_COMPLETE'), (<stack name>, 'ROLLBACK_COMPLETE')]

(Resource names, ARNs, request ids and timestamps in any single run are per-run values filling the templates above, not part of the requirement.)

## Steps to Reproduce
Cause 1: CreateStack with an AWS::RDS::DBSubnetGroup (description v1, two subnets); wait CREATE_COMPLETE; UpdateStack with the same logical id but a changed DBSubnetGroupDescription (and/or SubnetIds) and the same DBSubnetGroupName; poll DescribeStacks — the emulator ends UPDATE_ROLLBACK_COMPLETE with the NotImplemented reason on the SubnetGroup resource.

Cause 2: CreateStack with an AWS::RDS::DBParameterGroup of Family mysql8.0 whose Parameters set binlog_row_image to a value outside {full,minimal,noblob} (e.g. 'fully'); wait for ROLLBACK_COMPLETE; then call rds.DescribeDBParameterGroups(DBParameterGroupName=<that group>). The emulator returns the group with 200; AWS refuses with 404 DBParameterGroupNotFound 'DBParameterGroup not found: <name>'.

Cause 3: CreateStack with an AWS::RDS::DBSubnetGroup, an Aurora MySQL AWS::RDS::DBCluster, and an AWS::RDS::DBClusterEndpoint (DBClusterIdentifier: Ref of the cluster, CustomEndpointType 'ANY', its own identifier and tags). Poll DescribeStacks: the Endpoint resource goes CREATE_FAILED (twice) and the stack rolls back; read DescribeStackEvents for the Endpoint CREATE_FAILED ResourceStatusReason to see the handler's refusal.
