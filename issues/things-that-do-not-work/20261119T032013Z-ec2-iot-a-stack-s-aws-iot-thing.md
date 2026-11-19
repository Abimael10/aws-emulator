Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iot.iot-thing-stack-delete-fails-while-a-principal-is-attached
Issue-Key: iot|AttachThingPrincipal|ResourceNotFoundException
Issue-Commit: 8470bd15600894c04d6297f04472e87ed5582e16

# ec2__iot: a stack's AWS::IoT::Thing reaches CREATE_COMPLETE but the thing is absent from the IoT registry, so AttachThingPrincipal is refused 404

> _Note: this issue was identified against the code base at commit `8470bd15600894c04d6297f04472e87ed5582e16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachThingPrincipal

## Description
REOPENS 20261118T162135Z-iot-refusals-diverge-on-6-causes-not-found.md: its fix left the call refused. That issue's Cause 1 is this same AttachThingPrincipal 404; its wording fix has landed (the emulator now emits AWS's sentence verbatim), but the call is still refused where AWS succeeds, because the thing the request names does not exist in the emulator's IoT registry even though CloudFormation reported it created.

Cause 1 of 1: a CloudFormation stack declaring an AWS::IoT::Thing settles CREATE_COMPLETE and answers Ref with the thing's name, yet the thing was never written to the IoT registry, so the first control-plane call that resolves it is refused.
AWS: recorded behavior only (no Smithy statement covers the CloudFormation resource type). CloudFormation provisions AWS::IoT::Thing by calling iot:CreateThing, so once the stack is CREATE_COMPLETE the thing is a registry resource like any other: AttachThingPrincipal (Smithy: PUT /things/{thingName}/principals, principal bound to header x-amzn-principal; errors InvalidRequestException 400, ResourceNotFoundException 404) returns HTTP 200 with an empty body, and ListThingPrincipals then reports the certificate ARN. The 404 wording the emulator now uses — fixed text "Failed to attach the principal ", " to thing ", " because the thing does not exist in your account", with the principal taken from the request header and the thing name from the {thingName} URI label — is correct for a thing that really is absent; it is simply not the answer AWS gives for a thing a completed stack created.
Emulator: the stack reaches CREATE_COMPLETE and its Outputs carry ThingName == the requested name (both asserted and passed in the check), but AttachThingPrincipal for that name answers HTTP 404 ResourceNotFoundException "Failed to attach the principal <cert-arn> to thing <thing-name> because the thing does not exist in your account". Nothing in the stack's status or events reports a failure. Hypothesis: the AWS::IoT::Thing resource provider records the physical id without performing the underlying CreateThing against the IoT service state.
Why: a client that provisions devices with CloudFormation believes the registry holds its things; every follow-on call (AttachThingPrincipal, DescribeThing, UpdateThing, DeleteStack's own delete-order guard) then fails with a 404 for a resource the stack says it owns. The whole delete-ordering contract this check exercises — DeleteStack must end DELETE_FAILED while a principal is attached, leaving the thing and its dependency VPC intact — is unreachable, so the emulator also cannot show the refusal CloudFormation is supposed to surface.
Fix: CloudFormation's AWS::IoT::Thing resource provider (the create path must write the thing into the IoT service's registry state, and must fail the resource when it cannot) and the delete path that is expected to report DELETE_FAILED when the thing still has a principal attached. The AttachThingPrincipal handler itself is correct for a genuinely absent thing — do not change its wording.
Traces: ec2__iot.iot-thing-stack-delete-fails-while-a-principal-is-attached.

Related open issues that do NOT carry this check: 20261029T211043Z-iot-lambda-iot-a-cloudformation-custom-resource-that-creates-a.md (a Lambda-backed CUSTOM resource that calls iot:CreateThing, not the native AWS::IoT::Thing type) and 20261109T122953Z-cloudformation-iot-3-net-new-causes-every-aws.md (AWS::IoT::* types unrecognized by CreateStack — here the type IS recognized and the stack completes).

## Evidence
Replayed check ec2__iot.iot-thing-stack-delete-fails-while-a-principal-is-attached.
The stack asserts passed: status == CREATE_COMPLETE and Outputs[ThingName] == the requested thing name (template: AWS::EC2::VPC plus AWS::IoT::Thing with ThingName from a parameter, Thing DependsOn Vpc).
Expected: AttachThingPrincipal(thingName=<stack's thing>, principal=<cert ARN from CreateKeysAndCertificate>) -> HTTP 200, then ListThingPrincipals(thingName) == [cert ARN].
Actual: botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the AttachThingPrincipal operation: Failed to attach the principal arn:aws:iot:<region>:<account>:cert/<64-hex> to thing <thing-name> because the thing does not exist in your account.
(The ARN, account and thing name above are one run's values filling the message template; the requirement is the 200, not that message.)

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. replay ec2__iot.iot-thing-stack-delete-fails-while-a-principal-is-attached.
Direct repro against the booted emulator: (a) cloudformation:CreateStack with a template containing an AWS::EC2::VPC and an AWS::IoT::Thing whose ThingName is a parameter, and an Output {"Value": {"Ref": "Thing"}}; (b) poll DescribeStacks until CREATE_COMPLETE and read the ThingName output; (c) iot:DescribeThing(thingName=<that output>) — expect the thing's description, observe ResourceNotFoundException; (d) iot:CreateKeysAndCertificate(setAsActive=False) then iot:AttachThingPrincipal(thingName=<that output>, principal=<certificateArn>) — expect HTTP 200, observe 404 "Failed to attach the principal ... because the thing does not exist in your account"; (e) DeleteStack with the principal attached — expect the stack to end DELETE_FAILED with Thing DELETE_FAILED and Vpc untouched.
