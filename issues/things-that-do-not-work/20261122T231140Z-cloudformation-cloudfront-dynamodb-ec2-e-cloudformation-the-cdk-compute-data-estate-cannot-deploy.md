Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudfront__dynamodb__ec2__eks__kinesis__lambda__redshift-data__s3.cdk-compute-data-estate-serves-and-destroys
Issue-Key: cloudformation__cloudfront__dynamodb__ec2__eks__kinesis__lambda__redshift-data__s3.cdk-compute-data-estate-serves-and-destroys|_run_cli#14|AssertionError|assert result.returncode == 0, (
Issue-Commit: a9b1f91879b3ed1957cfd9e0361182ba83602a4e

# cloudformation: the CDK compute-data estate cannot deploy — the Warehouse stack's template is refused "Template format error: Unrecognized resource types"

> _Note: this issue was identified against the code base at commit `a9b1f91879b3ed1957cfd9e0361182ba83602a4e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: `cdk deploy --all` of the compute-data estate fails because CloudFormation refuses one of the estate's stack templates — the Warehouse stack, the one carrying the Redshift Serverless warehouse — with a ValidationError whose message is "Template format error: Unrecognized resource types: [...]". Nothing in the estate is created, so every downstream assertion of the check (the Kinesis stream round trip, the Redshift Data API query through the warehouse's workgroup/namespace, the deployed S3 objects, and the destroy) is unreachable.

AWS: recorded behavior only for the estate as a whole — the check asserts that `cdk deploy --all` of this app exits 0 and that every named stack (including "Warehouse") reaches CREATE_COMPLETE, after which `redshift-serverless:GetWorkgroup` / `GetNamespace` describe the warehouse the stack created and the Redshift Data API answers a query against it. AWS CloudFormation reserves ValidationError / "Template format error: Unrecognized resource types: [<Type>, ...]" for a template that declares a resource type CloudFormation does not know; for a type AWS does support, CreateStack accepts the template and provisions the resource instead. The check's own teardown reads the Warehouse stack's resources back as `AWS::RedshiftServerless::Namespace`, and its outputs are RedshiftNamespaceName / RedshiftWorkgroupName / RedshiftDatabaseName, so the stack that is refused declares the Redshift Serverless namespace and workgroup types that AWS does support.

Emulator: `cdk deploy --all` exits 1. The CLI output tail ends, immediately after the construct annotations for the stack `sim2real-cad-<token>-Warehouse`, with "Template format error: Unrecognized res…" — i.e. the stack create is refused ValidationError "Template format error: Unrecognized resource types: [...]" rather than accepted, because the resource types the Warehouse stack declares are not registered in the emulator's CloudFormation resource-type registry.

Why: a client relying on AWS's behavior cannot deploy a multi-stack estate at all. The refusal is at template validation, before any resource is created, so the whole `cdk deploy --all` aborts with a non-zero exit and no stack events to diagnose; any app whose template mentions one unregistered type is undeployable even though every other resource in it is supported, and the usual recovery (watch the stack roll back and fix the one resource) is unavailable because no stack is ever created.

Fix: the CloudFormation service's template-validation / resource-type registry and the provisioning handlers for the Redshift Serverless resource types the Warehouse stack declares (AWS::RedshiftServerless::Namespace and AWS::RedshiftServerless::Workgroup) — they must be recognized by CreateStack and provisioned so the created namespace/workgroup are the ones redshift-serverless and redshift-data then answer for.

Related open issues that do NOT cover this check (named here so the fixer can tell them apart; neither carries this check's replay key): 20261109T124343Z-cloudformation-redshift-serverless-aws-redshiftserverless-namespace-is-not.md and 20261109T123922Z-appflow-cloudformation-aws-appflow-connectorprofile-and-aws-redshiftserverless.md report the same unregistered-type class for single-stack namespace/workgroup templates.

Traces: cloudformation__cloudfront__dynamodb__ec2__eks__kinesis__lambda__redshift-data__s3.cdk-compute-data-estate-serves-and-destroys.

## Evidence
Failing check: [mismatch] cloudformation__cloudfront__dynamodb__ec2__eks__kinesis__lambda__redshift-data__s3.cdk-compute-data-estate-serves-and-destroys

Expected: `cdk deploy --all` exits 0 and the estate's stacks — including "Warehouse" — reach CREATE_COMPLETE, after which the check reads outputs["Warehouse"]["RedshiftWorkgroupName"] / ["RedshiftNamespaceName"] through redshift-serverless and runs a Data API statement against them.

Actual: AssertionError: `cdk deploy --all` exited 1, expected 0; output tail: '... (Construct Annotations)\n   sim2real-cad-<token>-Warehouse\n   Acknowledge with \'Annotation::@aws-cdk/core:crossStackReferencesDefaultStrong\'\nTemplate format error: Unrecognized res' (the tail is cut by the report at the type list).

The refused status/code/message class: HTTP 400 ValidationError with the message "Template format error: Unrecognized resource types: [<Type>, ...]" — the fixed text is "Template format error: Unrecognized resource types: " and the bracketed list is the set of types taken from the submitted template; here the submitted stack is the Warehouse stack, whose resources the check reads back as AWS::RedshiftServerless::Namespace (and whose outputs name a workgroup, namespace and database).

## Steps to Reproduce
1. Point the CDK at the emulator and deploy the compute-data estate app used by the check (`cdk bootstrap`, then `cdk deploy --all --ci --no-version-reporting --no-notices`), region us-east-1.
2. Observe the CLI exits 1 and the output ends with "Template format error: Unrecognized res..." right after the annotations for the `...-Warehouse` stack.
3. Minimal equivalent: CreateStack with a template declaring AWS::RedshiftServerless::Namespace and AWS::RedshiftServerless::Workgroup — AWS accepts it and creates the namespace/workgroup; the emulator answers 400 ValidationError "Template format error: Unrecognized resource types: [...]".
