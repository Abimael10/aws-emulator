Issue-Source: trace-sweep-divergence
Issue-Trace: docdb-cluster-and-instance-endpoint-discovery
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: docdb-cluster-and-instance-endpoint-discovery
Issue-Shape-Id: com.amazonaws.docdb#AmazonRDSv19
Issue-Key: docdb|CreateDBInstance|2xx->2xx|$.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.Endpoint.Address
Issue-Key: docdb|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.Endpoint.Address
Issue-Commit: 5eee5a427c3106f612cccd386d0e828122ce4e41

# DocDB DBInstance Endpoint.Address is published as a docdb.amazonaws.com host the emulator does not serve

> _Note: this issue was identified against the code base at commit `5eee5a427c3106f612cccd386d0e828122ce4e41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBInstance

## Description
Two net-new wire causes, both the same defect in one place: every DocDB response that projects DBInstance.Endpoint publishes an address in AWS's public DNS namespace instead of the host:port the emulator itself serves, so the advertised instance endpoint is unreachable for any client of this emulator. The corpus asserts the published host belongs to the emulator (verdict `host_not_own`), not that it has a particular shape, so this is not a cosmetic difference in a generated name.

Cause 1 of 2: CreateDBInstance (fixture docdb-cluster-and-instance-endpoint-discovery, step task#8).
AWS: Smithy com.amazonaws.docdb#AmazonRDSv19 models DBInstance.Endpoint as the structure Endpoint, documented "Network information for accessing a cluster or instance. Client programs must specify a valid endpoint to access these Amazon DocumentDB resources.", with Endpoint.Address "Specifies the DNS address of the instance" and Endpoint.Port "Specifies the port that the database engine is listening on"; DBInstance.Endpoint itself is "Specifies the connection endpoint". The requirement is therefore that the emitted Address/Port is the address the engine actually answers on for the caller, not that some string is present. Status and body shape are otherwise correct here (HTTP 200, CreateDBClusterResult/CreateDBInstanceResult query-XML envelope); no error code or message is involved in this cause.
Emulator: the step returns HTTP 200 as expected, but $.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.Endpoint.Address is a templated public AWS hostname of the form <db-instance-identifier>.<resource-id>.<region>.docdb.amazonaws.com — a name the emulator neither serves nor resolves. The replay records `host_not_own (expected None, got 'sim2real-probe-docdbep-i-e7277c75.f0959wiye2mk.us-west-2.docdb.amazonaws.com')`; the identifier, resource id and region in that captured value are per-run environment values and are not part of the requirement.
Why: a client does exactly what the model documents — read DBInstance.Endpoint.Address/Port from the create response and connect — and gets a name-resolution failure against AWS's public namespace. The whole DocumentDB data plane is therefore unreachable, and the failure is misattributed by users to their own VPC DNS/route-table/security-group configuration rather than to the emulator.
Fix: the docdb instance endpoint state plus the DBInstance serializer that projects Endpoint.Address/Endpoint.Port (the same projection used by every DBInstance-returning operation) must emit the emulator's own serving host and the port its DocumentDB listener is bound to.
Traces: docdb-cluster-and-instance-endpoint-discovery#task#8.

Cause 2 of 2: DescribeDBInstances (fixture docdb-cluster-and-instance-endpoint-discovery, step task#9).
AWS: same model contract — DescribeDBInstances returns DBInstances.DBInstance[].Endpoint, whose Address/Port must be the connectable address of the instance; this is the read path a polling client uses after the instance reaches DBInstanceStatus 'available'.
Emulator: HTTP 200 as expected, but $.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.Endpoint.Address carries the same public *.docdb.amazonaws.com host instead of an emulator-served one: `host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-docdbep-i-e7277c75.f0959wiye2mk.us-west-2.docdb.amazonaws.com')`. Again the specific labels in that host are per-run values.
Why: endpoint discovery is the documented way to find an instance's connection endpoint, so a client that creates a cluster and instance and then discovers the endpoint still receives an unusable address; the defect survives any re-read and cannot be worked around client-side.
Fix: same component — DBInstance Endpoint projection/state must be host-correct on the describe path as well as the create path.
Traces: docdb-cluster-and-instance-endpoint-discovery#task#9.

Relation to already-filed issues (none of them carries either of these replay keys, so neither cause is covered): open 20261115T142542Z-docdb-dbinstance-endpoint-address-is-published-as-a.md describes the same family of defect for DBInstance.Endpoint.Address but is grounded in the BEHAVIORAL check docdb.cluster-mongo-insert-find-round-trip (key docdb.cluster-mongo-insert-find-round-trip|run#23) and so never replays these wire steps. Open 20261112T142502Z-docdb-cluster-endpoint-and-reader-endpoint-are-published.md owns the DBCluster Endpoint/ReaderEndpoint clusters of this same fixture (task#7/task#10), not the DBInstance ones. Open 20261109T120625Z-docdb-cluster-instance-endpoints-are-published-as-real.md is likewise behavioral and keyed on a probe-call rejection.

## Evidence
Replay of fixture docdb-cluster-and-instance-endpoint-discovery (trace set simparity-wire-aws-v1), both steps HTTP 200 expected and 200 actual, diverging only on the published endpoint host:

Step task#8, CreateDBInstance:
  Expected: $.CreateDBInstanceResponse.CreateDBInstanceResult.DBInstance.Endpoint.Address is a host the emulator serves (the corpus asserts ownership of the host, recorded as expected None for the public-AWS form).
  Actual:   host_not_own — 'sim2real-probe-docdbep-i-e7277c75.f0959wiye2mk.us-west-2.docdb.amazonaws.com'.

Step task#9, DescribeDBInstances:
  Expected: $.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.Endpoint.Address is a host the emulator serves ('<shaped-not-asserted>' — the hostname shape is explicitly not asserted).
  Actual:   host_not_own — 'sim2real-probe-docdbep-i-e7277c75.f0959wiye2mk.us-west-2.docdb.amazonaws.com'.

The instance identifier, resource id and region inside the observed hosts are per-run values; the requirement being violated is that Endpoint.Address names an address the emulator answers DocumentDB on, per Smithy Endpoint.Address ("Specifies the DNS address of the instance") / Endpoint.Port ("Specifies the port that the database engine is listening on") and the Endpoint doc ("Client programs must specify a valid endpoint to access these Amazon DocumentDB resources").

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it.
2. Replay the wire corpus simparity-wire-aws-v1, fixture docdb-cluster-and-instance-endpoint-discovery.
3. Step task#7 creates a DB cluster; step task#8 calls docdb CreateDBInstance (Engine='docdb', DBInstanceClass as recorded, DBClusterIdentifier=<the cluster>). Inspect CreateDBInstanceResult.DBInstance.Endpoint.Address: observed a <instance>.<resource-id>.<region>.docdb.amazonaws.com host; expected the emulator's own serving host and listener port.
4. Step task#9 calls docdb DescribeDBInstances for that instance. Inspect DescribeDBInstancesResult.DBInstances.DBInstance.Endpoint.Address: same public-AWS host instead of an emulator-served one.
5. Optionally resolve/connect to the advertised Address:Port from the replay environment — it does not resolve and nothing listens, confirming the published endpoint is not the emulator's.
