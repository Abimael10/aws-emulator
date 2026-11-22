Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle
Issue-Key: athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle|<lambda>#14|Timeout|lambda: athena.get_query_execution(QueryExecutionId=qid)["QueryExecution"],
Issue-Commit: 05866a96998b37f437bd411ae9396664f070e9d9

# athena__glue__s3: the seven-phase Athena-over-CSV estate lifecycle reaches no graded assertion — the CloudFormation deploy of the seeded CSV estate never settles inside the replay budget

> _Note: this issue was identified against the code base at commit `05866a96998b37f437bd411ae9396664f070e9d9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new cause, found by replaying the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout. The replay reported this check neither ALREADY FILED nor OWNED by an open issue.

Cause 1 of 1: the deploy-through-destroy Athena/Glue/S3 estate lifecycle never produces a single graded observation — the check is cut off at the per-check cap of 60 s.
AWS: recorded behavior (nothing here is in a Smithy model). The check's first blocking step is its deploy phase: CreateStack of the carved athena-csv-estate template, whose bucket is seeded through five s3deploy.BucketDeployment constructs (a Lambda-backed Custom:: resource plus a bootstrap asset), followed by a poll of DescribeStacks for a terminal StackStatus with a 300 s deadline and a 5 s interval, asserting CREATE_COMPLETE. Only then do the remaining six phases run: a fixed CREATE DATABASE / CREATE TABLE DDL sequence through Athena StartQueryExecution against the primary workgroup with an explicit OutputLocation, recursive per-prefix reads of the 21 seeded CSV objects with the header skipped per file, a convergent reset over the Glue databases whose tables are located in this bucket, and a destroy that must leave DescribeStackResources answering ValidationError for the deleted stack. Against AWS each of those phases reports a verdict.
Emulator: 'Check timed out after 60.0s' with no assertion evaluated — neither the CREATE_COMPLETE assertion of the deploy phase nor any later Athena/Glue assertion is ever reached, so the whole lifecycle is ungradeable. Hypothesis: the stack create does not reach a terminal StackStatus because the template's Lambda-backed Custom:: BucketDeployment resources never deliver a response — the same shape of defect open issues 20261108T043811Z-cloudformation-s3-a-lambda-backed-cloudformation-custom-resource-never-completes.md and 20261120T124018Z-athena-codebuild-dynamodb-ec2-ecs-kinesi-...-r2.md (a CDK AwsCustomResource never delivers its response) describe for other suites; neither was reported as owning this check. If the deploy does settle, the budget is then spent in the Athena DDL phase, where open issues 20261104T083348Z-athena-s3-athena-refuses-create-database-and-aliased.md and 20261111T040849Z-athena-s3-athena-s-sql-engine-parses-neither-ctas-with.md report that Athena's SQL engine refuses CREATE DATABASE and related statements.
Why: a client that provisions an Athena-over-S3 estate with CloudFormation and then queries it the documented way gets no terminal stack status and no query result inside any reasonable timeout — the deploy hangs rather than completing or failing, so neither the happy path nor a diagnosable failure is available, and the whole deploy/setup/query/reset/destroy workflow is unusable.
Fix: the component that settles a stack whose resources include a Lambda-backed Custom:: resource — the CloudFormation create/update worker and the custom-resource response path it waits on (the stack state machine behind CreateStack/DescribeStacks) — must drive the stack to a terminal StackStatus; and, behind it, the Athena StartQueryExecution/GetQueryResults path for the CREATE DATABASE / CREATE TABLE DDL sequence this lifecycle runs must reach a terminal query state.
Traces: athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle.

## Evidence
Behavioral suite replayed against the emulator built from this checkout: 2224/2681 checks passed (83.0%); athena__glue__s3 scored 50/56 (89%) with 6 failures, five of which the replay marked OWNED by open issues.

Failing check athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle:
  Error: Check timed out after 60.0s
No assertion was evaluated, so the emulator produced no field-level divergence to quote. Expected (AWS): the deploy phase's poll of DescribeStacks(StackName=<stack>) reaches a terminal StackStatus within its 300 s deadline and that status is CREATE_COMPLETE (the check's own assertion: 'stack <stack> reached <status>, not CREATE_COMPLETE'), after which the six remaining phases (setup, pre_invoke, solve, post_invoke, cleanup, destroy) each report a verdict — the DDL sequence lands six Glue databases, Athena reads the 21 seeded CSV objects recursively per prefix with the header skipped per file, the reset converges twice scoped to the databases holding a table located in this bucket, and after destroy DescribeStackResources answers ValidationError for the deleted stack.
Actual (emulator): the 60 s budget is consumed with no observation at all. Stack names, bucket names and query-execution ids in this scenario are per-run values; the requirement is that the stack reaches a terminal status and the Athena DDL/query phases reach terminal query states.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1) and re-run athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle; it ends with 'Check timed out after 60.0s' and no graded assertion.
3. To isolate the stall, CreateStack with the check's carved athena-csv-estate template (its bucket seeded through five s3deploy.BucketDeployment constructs, i.e. a Lambda-backed Custom:: resource plus a bootstrap asset) and poll DescribeStacks(StackName=<stack>) every 5 s: AWS reaches CREATE_COMPLETE, the emulator does not reach any terminal StackStatus.
4. If the stack does settle, run the lifecycle's DDL sequence directly — Athena StartQueryExecution of 'CREATE DATABASE IF NOT EXISTS <db>' and the CREATE EXTERNAL TABLE statements in the primary workgroup with an explicit ResultConfiguration.OutputLocation — and poll each QueryExecution to a terminal state to see which call fails to terminate.
