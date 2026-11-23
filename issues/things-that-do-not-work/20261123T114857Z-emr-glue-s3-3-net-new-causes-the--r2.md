Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.emr-cluster-glue-catalog-disabled-configuration-negative emr__glue__s3.emr-spark-job-writes-result-via-glue-catalog-table-location-s3-positive emr__glue__s3.emr-cluster-glue-catalog-missing-permission-negative
Issue-Key: emr__glue__s3.emr-cluster-glue-catalog-disabled-configuration-negative|run#24|AssertionError|assert fixed_state == "COMPLETED", (
Issue-Key: emr__glue__s3.emr-spark-job-writes-result-via-glue-catalog-table-location-s3-positive|run#14|AssertionError|assert state == "COMPLETED", (
Issue-Key: emr__glue__s3.emr-cluster-glue-catalog-missing-permission-negative|run#15|AssertionError|assert "AccessDenied" in denied_log, f"expected an AccessDenied signal from Glue in the Hive log: {denied_log!r}"
Issue-Commit: cd8f73d475d74adb0ca8a470eb19f8c740cf481a

# emr__glue__s3: 3 net-new causes — the Hive CLI never appears after a ModifyInstanceGroups reconfiguration, spark-sql is absent from the step environment, and an explicit Deny on glue:* is not enforced for the Hive/Glue metastore

> _Note: this issue was identified against the code base at commit `cd8f73d475d74adb0ca8a470eb19f8c740cf481a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddJobFlowSteps

## Description
Three net-new root causes on the EMR-on-EC2 step/Glue-catalog path, all observed on checks the replay report marked neither ALREADY FILED nor OWNED. All three must be closed before this issue is submitted.

Related open issues exist for neighbouring symptoms and are named for context only — none of them carries the replay keys below, and the symptoms here are past the blanket "no application is ever run" failure those issues describe (steps now do execute a shell and a Hive query now does return rows): 20261108T104909Z-emr-glue-s3-2-net-new-causes-cluster.md (steps always end FAILED because no Hive/Spark/Hadoop application is run), 20261108T165124Z-emr-glue-s3-an-emr-step-is-reported.md (a step reported COMPLETED although the instance role is denied glue:*), 20261121T155504Z-emr-the-hadoop-hdfs-cli-is-absent-from.md (the hadoop/hdfs CLI is absent from the step environment), 20261119T185618Z-emr-5-net-new-root-causes-outside-the.md (cluster nodes carry no Hadoop/Spark binaries).

=== Cause 1 of 3: a hive-site Glue classification applied to a RUNNING instance group with ModifyInstanceGroups never takes effect — the Hive CLI is not even on the node's PATH ===
AWS:      Smithy (com.amazonaws.emr#ElasticMapReduce) InstanceGroupModifyConfig$Configurations — "A list of new or modified configurations to apply for an instance group" — with ReconfigurationType MERGE|OVERWRITE; after ModifyInstanceGroups the group is reconfigured and DescribeCluster/ListInstanceGroups report an incremented ConfigurationsVersion with the group back in RUNNING. Recorded behavior: on a cluster whose Applications include Hive, `hive -S -e "SELECT count(*) FROM <db>.<table>;"` run from a command-runner.jar shell step resolves the table through the reconfigured hive-site metastore and the step reaches StepState COMPLETED, printing the row count. The requirement is that the Hive installation is present on the node and that a reconfiguration of the primary group's hive-site classification changes which metastore it resolves against.
Emulator: after ModifyInstanceGroups(ClusterId, InstanceGroups=[{InstanceGroupId: <MASTER group>, Configurations: [hive-site with hive.metastore.client.factory.class=com.amazonaws.glue.catalog.metastore.AWSGlueDataCatalogHiveClientFactory}]) is accepted and the group is reported back in RUNNING, the identical step's output is the shell's own diagnostic `sh: hive: command not found` and the step does not COMPLETE. The fixed text of the message is the shell's, with `hive` being the command the step invoked. The emulator therefore has no Hive install for a reconfiguration to point anywhere, and the negative half of the same scenario (a Hive query that must FAIL without the Glue classification) passes only by accident, for the wrong reason.
Why:       a client cannot distinguish "the metastore is not configured for Glue" from "Hive is not installed": the positive control of every Glue-metastore configuration test fails, and any pipeline that reconfigures a running cluster's hive-site (the documented way to switch an EMR cluster onto the Glue Data Catalog without relaunching it) silently gets a cluster that can run no Hive at all.
Fix:       the node/step execution environment of the elasticmapreduce service crate (the step runner that executes a command-runner.jar shell step must provide the Hive CLI for a cluster whose Applications request Hive), plus the ModifyInstanceGroups handler's Configurations path, which must apply the reconfigured classification to the group's running nodes rather than only recording it.
Traces:    emr__glue__s3.emr-cluster-glue-catalog-disabled-configuration-negative — AssertionError: "hive query still failed after enabling the Glue Data Catalog metastore (positive control): sh: hive: command not found".

=== Cause 2 of 3: spark-sql is absent from the step environment, so no Spark SQL step can run against a Glue-catalog table ===
AWS:      recorded behavior only (the Smithy model declares Application/Configuration but says nothing about the installed binaries). On an EMR cluster whose Applications include Spark and Hive and whose spark-hive-site classification sets hive.metastore.client.factory.class=com.amazonaws.glue.catalog.metastore.AWSGlueDataCatalogHiveClientFactory, a command-runner.jar shell step running `spark-sql -e "INSERT INTO <db>.<table> SELECT 'row1','sparkval';"` executes, the step reaches StepState COMPLETED, and the inserted row is written as a non-empty object under the S3 prefix registered as the Glue table's StorageDescriptor.Location (so a ListObjectsV2 of that prefix returns a data object containing the inserted value).
Emulator: the step's captured output is `sh: spark-sql: command not found` and the step does not COMPLETE; nothing is written under the Glue table's registered S3 location. Fixed text is the shell's diagnostic, with `spark-sql` being the command invoked.
Why:       the Spark SQL CLI is the standard entry point for writing through a Glue-catalog-backed table on EMR; with it absent, a client sees a step failure that looks like its own SQL being wrong, and the data-plane invariant it depends on (an INSERT lands at the exact S3 location registered on the Glue table) can never hold.
Fix:       the same step execution environment in the elasticmapreduce service crate (install/provide the Spark SQL CLI for a cluster whose Applications request Spark, and honour spark-hive-site so the catalog it resolves is Glue), and the S3 write the executed application performs.
Traces:    emr__glue__s3.emr-spark-job-writes-result-via-glue-catalog-table-location-s3-positive — AssertionError: "spark-sql insert into the Glue-catalog-backed table failed: sh: spark-sql: command not found".

=== Cause 3 of 3: an explicit IAM Deny on glue:* on the cluster's EC2 instance role is not enforced when Hive resolves a table through the Glue Data Catalog ===
AWS:      an inline policy on the EMR EC2 instance role with Effect=Deny on glue:* makes every Glue Data Catalog call the node makes fail authorization; the Hive client's metastore lookup is refused by Glue with the AccessDeniedException error class (an explicit deny always wins over any Allow), the Hive driver surfaces that refusal in its output — the log carries an AccessDenied signal naming the denied glue action and the role principal — and the step ends FAILED with no row count printed. The variable parts of that refusal are the principal ARN, the action and the resource, taken from the request and the account's policies; the fixed part is the AccessDenied/AccessDeniedException classification and the "not authorized to perform" phrasing. Recorded behavior for the Hive-side log text; the deny-wins rule is IAM's documented evaluation order.
Emulator: the deny is not evaluated at all for in-cluster Glue access. The step's captured output is the successful row count `3\n` — the query resolved the Glue table and counted its rows — and no AccessDenied signal appears anywhere in the log, so the permission boundary is invisible. (The step's reported terminal state and its own output also disagree in this run: the state was FAILED while the output is the successful `3`, i.e. the terminal state is not derived from the executed command's exit status.) Hypothesis: the Glue catalog reads a step performs bypass the instance role's policy evaluation entirely.
Why:       a client cannot test or rely on the Glue authorization boundary of an EMR cluster: a cluster whose role is denied glue:* still reads the catalog, so a least-privilege configuration is unverifiable and a real deny would come as a surprise in production. Because no refusal is produced, the negative test cannot be told apart from a passing one, and the control-flow a client builds on AccessDenied never fires.
Fix:       the authorization path of the Glue Data Catalog reads performed on behalf of an EMR cluster (the step runner's catalog client in the elasticmapreduce service crate must assume the cluster's EC2 instance role and have the Glue handler evaluate that principal's policies, returning AccessDeniedException on an explicit deny), and the step runner's terminal-state derivation, which must take StepState from the executed command's exit status.
Traces:    emr__glue__s3.emr-cluster-glue-catalog-missing-permission-negative — AssertionError: "expected an AccessDenied signal from Glue in the Hive log: '3\n'".

## Evidence
Behavioral replay against the booted emulator, service heading `### emr__glue__s3` (suite simparity-behavioral-aws-v1).

Cause 1 — check emr__glue__s3.emr-cluster-glue-catalog-disabled-configuration-negative [mismatch]:
  expected (AWS): after ModifyInstanceGroups adds the hive-site Glue classification to the MASTER instance group of the SAME running cluster and the group returns to RUNNING with a bumped ConfigurationsVersion, the identical `hive -S -e "SELECT count(*) FROM <db>.<table>;"` step reaches Status.State COMPLETED and its captured stdout is the row count (3 for the 3-row CSV registered on the Glue table).
  actual (emulator): AssertionError: hive query still failed after enabling the Glue Data Catalog metastore (positive control): sh: hive: command not found. The database/table names, bucket and cluster id in the recording are per-run values; the requirement is that `hive` exists on the node and that the reconfiguration is applied.

Cause 2 — check emr__glue__s3.emr-spark-job-writes-result-via-glue-catalog-table-location-s3-positive [mismatch]:
  expected (AWS): the `spark-sql -e "INSERT INTO <db>.<table> SELECT 'row1','sparkval';"` step reaches Status.State COMPLETED, and ListObjectsV2 of the prefix registered as the Glue table location returns a non-empty object containing the inserted value.
  actual (emulator): AssertionError: spark-sql insert into the Glue-catalog-backed table failed: sh: spark-sql: command not found. No object is written under the table's registered prefix.

Cause 3 — check emr__glue__s3.emr-cluster-glue-catalog-missing-permission-negative [mismatch]:
  expected (AWS): with an inline `DenyGlueCatalog` policy (Effect=Deny, Action=glue:*) on the cluster's EC2 instance role, the Hive query step ends FAILED and the step log carries Glue's AccessDenied refusal (AccessDeniedException class, "... is not authorized to perform: glue:GetTable ...", principal/action/resource taken from the request and the account's policies); after the deny is removed the identical query COMPLETEs and prints 3.
  actual (emulator): AssertionError: expected an AccessDenied signal from Glue in the Hive log: '3\n' — the query succeeded under an explicit deny and no refusal of any kind was produced. The row count 3 is the check's own fixture data, not part of the requirement.

## Steps to Reproduce
Common setup: boot the emulator; create an S3 bucket; create the EMR service role and EC2 instance profile; create a Glue database and a CSV table whose StorageDescriptor.Location is an s3:// prefix in that bucket holding `1,alpha\n2,beta\n3,gamma\n`.

Cause 1:
1. RunJobFlow a keep-alive cluster with Applications Hive/Spark and NO hive-site classification; wait for WAITING/RUNNING.
2. AddJobFlowSteps a command-runner.jar shell step running `hive -S -e "SELECT count(*) FROM <db>.<table>;" > /tmp/out.txt 2>&1; aws s3 cp /tmp/out.txt s3://<bucket>/results/nocatalog.txt`. Observe the step does not COMPLETE (negative half).
3. ModifyInstanceGroups(ClusterId, InstanceGroups=[{InstanceGroupId: <MASTER group id>, Configurations: [{Classification: "hive-site", Properties: {hive.metastore.client.factory.class: com.amazonaws.glue.catalog.metastore.AWSGlueDataCatalogHiveClientFactory, hive.metastore.schema.verification: "false"}}]}]) and wait for the group to return to RUNNING with a bumped ConfigurationsVersion.
4. Re-run the identical Hive step and read the captured output from S3. Observed: `sh: hive: command not found` and a non-COMPLETED step; expected COMPLETED with the row count.

Cause 2:
5. RunJobFlow a keep-alive cluster with the spark-hive-site classification pointing the metastore factory at AWSGlueDataCatalogHiveClientFactory; wait for WAITING/RUNNING.
6. AddJobFlowSteps a command-runner.jar shell step running `spark-sql -e "INSERT INTO <db>.<table> SELECT 'row1','sparkval';" > /tmp/out.txt 2>&1; aws s3 cp /tmp/out.txt s3://<bucket>/results/spark_insert.txt`.
7. Read the captured output and ListObjectsV2 the Glue table's registered prefix. Observed: `sh: spark-sql: command not found`, step not COMPLETED, no data object; expected COMPLETED and an object containing `sparkval`.

Cause 3:
8. Create the EC2 instance role with an additional inline policy `DenyGlueCatalog` = {Effect: Deny, Action: "glue:*", Resource: "*"}; RunJobFlow a keep-alive cluster with the hive-site Glue classification on that instance profile.
9. AddJobFlowSteps the same Hive count step and read the captured log. Observed: the log is `3\n` (the query resolved the Glue table under an explicit deny) and no AccessDenied appears; expected a FAILED step whose log carries Glue's AccessDenied refusal.
10. DeleteRolePolicy the deny and re-run the identical step: it should then COMPLETE and print 3, which is the only half that currently behaves.
11. Re-replay the three checks in trace_ids to confirm the fix.
