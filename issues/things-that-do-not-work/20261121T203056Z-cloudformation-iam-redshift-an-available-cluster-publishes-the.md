Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__redshift.cfn-redshift-cluster-iam-roles-round-trip-and-endpoint-getatt
Issue-Key: cloudformation__iam__redshift.cfn-redshift-cluster-iam-roles-round-trip-and-endpoint-getatt|run#20|AssertionError|assert endpoint["Port"] == 5439, f"expected the cluster on port 5439, got {endpoint!r}"
Issue-Commit: 161daaf8224be3ae55f7d0bccd007967054680b8

# cloudformation__iam__redshift: an available cluster publishes the emulator's ephemeral listener port as Endpoint.Port (and in GetAtt Endpoint.Port) instead of the cluster's configured Redshift port (default 5439)

> _Note: this issue was identified against the code base at commit `161daaf8224be3ae55f7d0bccd007967054680b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeClusters

## Description
One net-new root cause, grounded in the single failing behavioral check of this replay that is not already filed.

Cause 1 of 1: the connection endpoint an available Redshift cluster publishes reports the emulator's own ephemeral listener port instead of the port the cluster accepts connections on.

AWS: Smithy (com.amazonaws.redshift): Cluster$Endpoint -> Endpoint{Address: String "The DNS address of the Cluster", Port: Integer "The port that the database engine is listening on", VpcEndpoints}. CreateClusterMessage$Port is documented as "The port number on which the cluster accepts incoming connections. ... Default: 5439. Valid Values: For clusters with RG or RA3 nodes - Select a port within the ranges 5431-5455 or 8191-8215. ... For clusters with dc2 nodes - Select a port within the range 1150-65535." So the port a cluster publishes is the port its creation request asked for, and when the request (or the AWS::Redshift::Cluster template) sets no Port it is the documented default 5439 — a modelled, request-derived value, not an allocation. CloudFormation's Fn::GetAtt <Cluster>.Endpoint.Port must return that same value as a string, so GetAtt and DescribeClusters agree.

Emulator: a cluster created from an AWS::Redshift::Cluster whose Properties carry no Port reaches ClusterStatus 'available' and DescribeClusters reports Endpoint {'Address': '127.0.0.1', 'Port': 33207, 'VpcEndpoints': [...]} — the port is whatever ephemeral port the core assigned when the cluster's listener was opened, never 5439 and never inside Redshift's valid RA3 ranges (5431-5455 / 8191-8215). The emulator does this on purpose: services/service-com-amazonaws-redshift-redshiftserviceversion20121201/src/ops/create_cluster.rs documents that "binding is asynchronous and the core owns the socket, the bind interface and the port. From here on the address and port this cluster publishes are that listener's own" (crate::state::attach_cluster_endpoint). The published port is therefore an environment artifact (33207 in this run) rather than the cluster's configured port, and Fn::GetAtt Endpoint.Port carries the same artifact.

Why: a client that reads Cluster.Endpoint — or a stack output built from GetAtt Endpoint.Port — to build a JDBC/ODBC/psql connection string, to record a warehouse's port in configuration, or to assert the documented 5439 default sees a value that changes on every run and is outside Redshift's documented port domain. Anything that compares the published port to the port it requested (or to 5439 when it requested none), that opens a security-group/ingress rule for the cluster's port, or that round-trips a CreateCluster/ModifyCluster Port through DescribeClusters is wrong against the emulator even though the control plane reports a usable warehouse. Note the Address is NOT the divergence here: the check only cross-checks Address against GetAtt, and a loopback address the emulator serves is legitimate; only the port is a requirement.

Fix: the cluster projection/state that builds Cluster.Endpoint — crate::state::attach_cluster_endpoint / StoredCluster::publish_endpoint and the shared cluster-endpoint derivation used by CreateCluster/DescribeClusters/ModifyCluster, plus the CloudFormation AWS::Redshift::Cluster resource handler that answers GetAtt Endpoint.Port from it. The published Endpoint.Port must be the cluster's CONFIGURED port (CreateCluster$Port, defaulting to 5439) and must stay independent of whatever port the emulator's own listener was bound to; if the endpoint must also remain dialable, the served listener has to be reached on the published port (or mapped to it) rather than the published port being rewritten to the listener's.

Traces: cloudformation__iam__redshift.cfn-redshift-cluster-iam-roles-round-trip-and-endpoint-getatt (the only net-new failing check in this replay). Related, NOT covering: open issue 20261109T125831Z-redshift-6-net-new-behavioral-causes-cluster-endpoint.md records the earlier state of this endpoint, where Address was an *.redshift.amazonaws.com host and Port was 5439 — its cause-1 replay keys are the native-SQL/connectivity checks and it does not carry this check or this port divergence; open issue 20261104T045922Z-redshift-publishes-a-cluster-endpoint-but-serves-no.md is about the absent PostgreSQL wire data plane, not the published port value. The analogous defect is already filed for other engines (20261121T073752Z-rds-db-cluster-envelopes-publish-the-emulator-s.md, 20261119T170405Z-neptune-2-net-new-port-causes-a-db.md), which is why this one is filed against the Redshift/CloudFormation surface separately.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator; service cloudformation__iam__redshift 0/1 passed.

[mismatch] cloudformation__iam__redshift.cfn-redshift-cluster-iam-roles-round-trip-and-endpoint-getatt
  File "check_cloudformation__iam__redshift_cfn_redshift_cluster_iam_roles_round_trip_and_endpoint_getatt.py", line 251, in run
    assert endpoint["Port"] == 5439, f"expected the cluster on port 5439, got {endpoint!r}"
  AssertionError: expected the cluster on port 5439, got {'Address': '127.0.0.1', 'Port': 33207, 'VpcEndpoints': [{'VpcEndpointId': 'vpce-7ac5e1339cf3f90f8', 'VpcId': 'vpc-000000000000000a1', 'NetworkInterfaces': [{'NetworkInterfaceId': 'eni-9b0d316b73fa48c18', 'SubnetId': 'subnet-44b0a134474a7c4a9', 'PrivateIpAddress': '10.124.120.42', 'AvailabilityZone': 'us-west-2c'}]}]}

Expected (AWS): DescribeClusters(ClusterIdentifier=<id>).Clusters[0].Endpoint.Port == 5439 for a single-node ra3.large cluster created from a template that sets no Port, and the stack output built from Fn::GetAtt ["Cluster", "Endpoint.Port"] == "5439", i.e. GetAtt and DescribeClusters agree on the cluster's configured port.
Actual (emulator): Endpoint.Port is the emulator's listener port (33207 in this run) and the GetAtt output carries the same value, so the assertion on the documented default fails before the GetAtt/DescribeClusters cross-check is even reached.

The literal 33207, the address 127.0.0.1, the vpce-/eni-/subnet- ids and the cluster/stack names are this run's environment values and are not part of the requirement. The requirement is the port VALUE: the port named by CreateCluster$Port / the AWS::Redshift::Cluster Port property, or 5439 when none is named — a request-derived value, never the emulator's bound listener port.

## Steps to Reproduce
1. CreateRole with a redshift.amazonaws.com trust policy (the cluster's IamRoles member).
2. CreateStack with a template containing one AWS::Redshift::Cluster whose Properties are ClusterType 'single-node', NodeType 'ra3.large', MasterUsername/MasterUserPassword, DBName 'dev', PubliclyAccessible false, IamRoles [<role arn>] — and NO Port property — plus Outputs EndpointAddress = Fn::GetAtt ["Cluster","Endpoint.Address"] and EndpointPort = Fn::GetAtt ["Cluster","Endpoint.Port"].
3. Wait for CREATE_COMPLETE, then poll redshift DescribeClusters(ClusterIdentifier=<id>) until ClusterStatus == 'available'.
4. Read Clusters[0].Endpoint: Port is the emulator's ephemeral listener port (e.g. 33207) instead of 5439, and DescribeStacks' EndpointPort output carries the same ephemeral value instead of "5439".
5. Repeat with the template setting Port explicitly (e.g. 5439 or 8191) — the published Endpoint.Port still does not echo the requested port.
6. Re-replay the check in trace_ids to confirm a fix.
