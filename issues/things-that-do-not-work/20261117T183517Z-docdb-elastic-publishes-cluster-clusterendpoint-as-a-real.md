Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: docdb-elastic.mongo-elastic-sharded-collection-round-trips-documents-by-their-shard-key docdb-elastic.mongo-elastic-shard-key-must-be-one-hashed-field-present-in-every-write docdb-elastic.mongo-elastic-refuses-unauthenticated-commands-an-unknown-user-and-a-wrong-password
Issue-Shape-Id: com.amazonaws.docdbelastic#ChimeraDbLionfishServiceLambda
Issue-Key: docdb-elastic.mongo-elastic-sharded-collection-round-trips-documents-by-their-shard-key|_mongo_connection#2|PublishedUrlRejected|host, port = ctx.endpoint_host(host, int(port))
Issue-Key: docdb-elastic.mongo-elastic-shard-key-must-be-one-hashed-field-present-in-every-write|_mongo_connection#2|PublishedUrlRejected|host, port = ctx.endpoint_host(host, int(port))
Issue-Key: docdb-elastic.mongo-elastic-refuses-unauthenticated-commands-an-unknown-user-and-a-wrong-password|_mongo_connection#3|PublishedUrlRejected|host, port = ctx.endpoint_host(host, int(port))
Issue-Commit: 85ee122b95e0a025d1600c3d9babe834193f0ff3

# DocDB Elastic publishes Cluster.clusterEndpoint as a real *.docdb-elastic.amazonaws.com DNS name the emulator does not serve, so no MongoDB data plane is reachable

> _Note: this issue was identified against the code base at commit `85ee122b95e0a025d1600c3d9babe834193f0ff3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
One net-new root cause for docdb-elastic.

Cause 1 of 1: the required `Cluster.clusterEndpoint` member is synthesized as a cosmetic, AWS-shaped DNS name instead of an address of the emulator, and nothing listens on the MongoDB wire port for an elastic cluster. Every native-protocol check dies before its first command, at the moment it tries to connect to the endpoint the emulator itself published. Failing checks: docdb-elastic.mongo-elastic-sharded-collection-round-trips-documents-by-their-shard-key, docdb-elastic.mongo-elastic-shard-key-must-be-one-hashed-field-present-in-every-write, docdb-elastic.mongo-elastic-refuses-unauthenticated-commands-an-unknown-user-and-a-wrong-password.

AWS: Smithy (com.amazonaws.docdbelastic#Cluster) models `clusterEndpoint` as a required String documented "The URL used to connect to the elastic cluster." It is not decoration: on AWS the value resolves and accepts a TLS MongoDB-wire-protocol connection from inside the cluster's VPC on port 27017, where the cluster serves `hello` (answering `ok: 1`, `isWritablePrimary: true`), `shardCollection`, and CRUD commands, and refuses unauthenticated or badly authenticated sessions with MongoDB error codes (13 AuthorizationFailed "Authorization failed.", 300 "Invalid auth mechanism SCRAM-SHA-256", 18 "Cascade Authentication failed. User does not exist" / "Authentication failed."). Recorded behavior for the data plane itself: the three checks above record exactly those replies against a reachable endpoint. The reachability requirement is structural — the handler must publish an address the emulator answers on, not an AWS production hostname.

Emulator: CreateCluster/GetCluster report an ACTIVE cluster whose `clusterEndpoint` is of the fixed form `<clusterName>-<hex>.<uuid>.<region>.docdb-elastic.amazonaws.com` — the fixed text is the `.<region>.docdb-elastic.amazonaws.com` suffix, the variable parts are the cluster name, a generated hex discriminator and a generated UUID. That host is not one of the emulator's own hosts and the emulator serves nothing at it, so the harness rejects the published target before connecting: `simparity.published.PublishedUrlRejected: endpoint_host was given <cluster>.<uuid>.us-west-2.docdb-elastic.amazonaws.com:27017 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address`. The specific hostnames in the three replays differ per run and are not part of the requirement; the pattern of announcing an off-emulator AWS production host is.

Why: a client that does what the API tells it to do — take `clusterEndpoint` and connect — is sent off the emulator to a name that either does not resolve or resolves to real AWS. No application, driver or test can reach an elastic cluster's data plane, and the whole MongoDB surface of docdb-elastic (authentication, shardCollection, inserts, finds, updates, deletes) is unexercisable even though the control plane reports the cluster ACTIVE. Secondarily, it is an egress attempt: the emulator directs its own client at an address outside itself.

Fix: the CreateCluster / RestoreClusterFromSnapshot cluster-projection code that builds `clusterEndpoint` (and whatever stores it in cluster state, so GetCluster/ListClusters return the same value) must publish a host[:port] the emulator actually answers on, and the docdb-elastic service must stand up a MongoDB-wire-protocol listener behind it that serves `hello`, SCRAM-SHA-1 PLAIN_TEXT auth for the cluster's adminUserName/adminUserPassword, `shardCollection` and CRUD, with the refusal codes above. Note the open issue 20261109T120625Z-docdb-cluster-instance-endpoints-are-published-as-real.md and 20261115T142542Z-docdb-dbinstance-endpoint-address-is-published-as-a.md: those are the same defect class in the `docdb` service and do not cover docdb-elastic's `clusterEndpoint`.

Traces: the three native-protocol checks listed above, each failing in the harness's endpoint_host() check on the value published by GetCluster.

## Evidence
Expected: GetCluster on an ACTIVE elastic cluster returns a `clusterEndpoint` host the emulator serves, and a TLS connection to it on port 27017 completes and speaks the MongoDB wire protocol — `hello` -> {ok: 1, isWritablePrimary: true}; an unauthenticated find/insert -> {ok: 0, code: 13, codeName: "AuthorizationFailed", errmsg: "Authorization failed."}; saslStart with SCRAM-SHA-256 -> {ok: 0, code: 300, errmsg: "Invalid auth mechanism SCRAM-SHA-256"}; SCRAM-SHA-1 for an unknown user -> {ok: 0, code: 18, errmsg: "Cascade Authentication failed. User does not exist"}; a wrong password -> {ok: 0, code: 18, errmsg: "Authentication failed."}.

Actual: `clusterEndpoint` is an AWS production DNS name of the form `<clusterName>-<hex>.<uuid>.<region>.docdb-elastic.amazonaws.com` that the emulator does not serve. The harness refuses it before any socket is opened:

  simparity.published.PublishedUrlRejected: endpoint_host was given sim2real-behav-docdbel-7c23388ca7e0.8d34fb57-535e-f3d3-bb7a-49b3e2e66557.us-west-2.docdb-elastic.amazonaws.com:27017 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address

The same rejection, with a different generated host per run, is the sole failure of all three native-protocol checks (…-8fbad3f9e50e.c61eb092-…, …-c6ae8008e3ac.2e587f11-…). No MongoDB reply was ever obtained, so no data-plane assertion was reached.

## Steps to Reproduce
1. CreateCluster(clusterName=<name>, authType="PLAIN_TEXT", adminUserName="sim2realadmin", adminUserPassword=<password>, shardCapacity=2, shardCount=1, shardInstanceCount=1, subnetIds=<private subnets>, vpcSecurityGroupIds=<group allowing 27017>).
2. Poll GetCluster(clusterArn=...) until status == "ACTIVE" and read cluster.clusterEndpoint.
3. Observe the value is `<clusterName>-<hex>.<uuid>.<region>.docdb-elastic.amazonaws.com` — a host outside the emulator.
4. Open a TLS connection to that host on port 27017 from inside the cluster's VPC and send an OP_MSG `{hello: 1, $db: "admin"}`. Nothing the emulator serves is reachable at that address; the behavioral harness rejects the published host with PublishedUrlRejected before connecting.
