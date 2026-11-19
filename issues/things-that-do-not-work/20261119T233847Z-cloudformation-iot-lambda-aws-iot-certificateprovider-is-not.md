Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iot__lambda.cfn-iot-certificate-provider-create-and-delete-with-stack cloudformation__iot__lambda.cfn-iot-certificate-provider-duplicate-name-rejected
Issue-Key: cloudformation__iot__lambda|CreateStack|ValidationError
Issue-Commit: 4fb279e8268ff71ec62c1d9ac755c72465a1585e

# cloudformation__iot__lambda: AWS::IoT::CertificateProvider is not a registered CloudFormation resource type — CreateStack refuses the template with ValidationError, so no certificate-provider stack can be deployed

> _Note: this issue was identified against the code base at commit `4fb279e8268ff71ec62c1d9ac755c72465a1585e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause under the suite heading '### cloudformation__iot__lambda' (2/4 passed). Both failing checks of that heading die on the same call with the same message, so this is ONE cause.

Cause 1 of 1: the emulator's CloudFormation template-type registry does not contain AWS::IoT::CertificateProvider, so every CreateStack whose template declares one is refused outright at template validation and the resource is never provisioned.

AWS: recorded behavior only for CloudFormation's resource-type registry — the CloudFormation Smithy model (CreateStack -> input StackName/TemplateBody/Tags, output {StackId}, modeled errors LimitExceededException, AlreadyExistsException, TokenAlreadyExistsException, InsufficientCapabilitiesException) does not enumerate resource types; the set of recognized types is service state, not a modeled shape. Against real AWS, AWS::IoT::CertificateProvider is a supported resource type: a template declaring it is accepted by CreateStack (HTTP 200 with a StackId), the stack reaches CREATE_COMPLETE, the provider is readable through iot DescribeCertificateProvider / ListCertificateProviders, and DeleteStack removes it so the subsequent describe answers ResourceNotFoundException. The duplicate-name leg records the create failing at the RESOURCE level — CreateStack itself still returns 200 and the stack rolls back with a resource status reason carrying the iot ResourceAlreadyExistsException — never as a ValidationError on the template. The backing iot operations already exist in this emulator (CreateCertificateProvider is implemented; only UpdateCertificateProvider is an open stub), so what is missing is purely the CloudFormation type binding.

Emulator: CreateStack is refused with HTTP 400, error code ValidationError and the message "Template format error: Unrecognized resource types: [AWS::IoT::CertificateProvider]". Fixed text of that message is "Template format error: Unrecognized resource types: [" ... "]"; the bracketed list is the set of type names taken from the request's TemplateBody. No StackId is returned and nothing is written, so neither the happy-path round trip nor the duplicate-name refusal can be reached.

Why: a client that provisions IoT certificate providers declaratively cannot deploy at all — its stack is rejected before any resource is attempted, and the rejection arrives as a template-syntax error rather than as the resource-level outcome AWS would report. Code distinguishing "my template is malformed" from "my resource was refused" is told the wrong thing, and the duplicate-name negative path (a stack that rolls back because the provider name is taken) is unobservable.

Fix: the CloudFormation resource-type registry and its AWS::IoT::* provisioning handlers — register AWS::IoT::CertificateProvider with its Create/Read/Delete bindings onto the iot crate's CreateCertificateProvider / DescribeCertificateProvider / DeleteCertificateProvider operations (and the GetAtt/Ref attributes the type exposes), so CreateStack accepts the template and the resource lifecycle, including the duplicate-name rollback, is driven through the stack engine.

Traces: cloudformation__iot__lambda.cfn-iot-certificate-provider-create-and-delete-with-stack, cloudformation__iot__lambda.cfn-iot-certificate-provider-duplicate-name-rejected. Related but NOT covering: 20261109T164005Z-cloudformation-codeartifact-aws-codeartifact-packagegroup-is-not-a.md, 20261109T142012Z-cloudformation-mediapackage-vod-aws-mediapackage-packaginggroup-and-aws.md and 20261109T223435Z-cloudformation-mediapackagev2-cloudformation-does-not-register-any-aws.md describe the same registry gap for other resource types; 20261109T122953Z-cloudformation-iot-3-net-new-causes-every-aws.md covers other AWS::IoT::* types but carries neither of these checks' replay keys and does not name AWS::IoT::CertificateProvider.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout. Total 200/351 passed (57.0%); bootstrap_signal=false. Heading '### cloudformation__iot__lambda': 2/4 (50%), 2 failed — both failures are the two checks below, and neither is marked ALREADY FILED or OWNED in the report.

[error] cloudformation__iot__lambda.cfn-iot-certificate-provider-create-and-delete-with-stack:
  File "client.py", line 1094, in _make_api_call / raise error_class(parsed_response, operation_name)
  botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::IoT::CertificateProvider]
  Expected: CreateStack returns 200 with a StackId; the stack reaches CREATE_COMPLETE; the AWS::IoT::CertificateProvider resource is readable through iot DescribeCertificateProvider; DeleteStack drives the stack to DELETE_COMPLETE and the provider is gone afterwards.
  Actual: HTTP 400 ValidationError at CreateStack; no stack is created, so every later leg of the check is unreachable.

[error] cloudformation__iot__lambda.cfn-iot-certificate-provider-duplicate-name-rejected:
  Same call, same response: botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::IoT::CertificateProvider]
  Expected: CreateStack is ACCEPTED (200 + StackId) and the duplicate certificate-provider name surfaces as a resource-level CREATE_FAILED / rollback whose ResourceStatusReason carries the iot duplicate-name refusal — not as a template-format ValidationError.
  Actual: the template is refused before any resource is attempted, so the negative class under test is never exercised.

Requirement vs per-run values: the HTTP status (400), the error code (ValidationError) and the message are the divergence. The message's fixed text is "Template format error: Unrecognized resource types: [" and the closing "]"; the type name inside is taken from the request's TemplateBody. The generated stack names, the certificate-provider name, the Lambda function ARN used as the provider's lambdaFunctionArn, the StackId and request ids are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Boot the emulator built from this checkout and point botocore cloudformation, iot and lambda clients at it.
2. Create a Lambda function to act as the certificate provider's handler and note its ARN.
3. Call cloudformation CreateStack(StackName=<name>, TemplateBody=<template below>, Tags=[{Key: sim2real, Value: <name>}]) where the template body is {"AWSTemplateFormatVersion": "2010-09-09", "Resources": {"Provider": {"Type": "AWS::IoT::CertificateProvider", "Properties": {"CertificateProviderName": "<pname>", "LambdaFunctionArn": "<lambda arn>", "AccountDefaultForOperations": ["CreateCertificateFromCsr"]}}}}.
4. Observe HTTP 400 ValidationError "Template format error: Unrecognized resource types: [AWS::IoT::CertificateProvider]" instead of a 200 with a StackId.
5. Expected instead: the stack is accepted, reaches CREATE_COMPLETE, iot DescribeCertificateProvider(certificateProviderName=<pname>) returns the provider, and DeleteStack removes it.
6. Repeat with a second stack declaring the SAME CertificateProviderName: expected is an accepted CreateStack whose stack rolls back with a resource-level duplicate-name reason, not a template ValidationError.
7. Re-replay cloudformation__iot__lambda.cfn-iot-certificate-provider-create-and-delete-with-stack and cloudformation__iot__lambda.cfn-iot-certificate-provider-duplicate-name-rejected.
