Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent__cloudformation__iam__s3__s3vectors.deploy-data-source-cfn-rejects-unknown-knowledge-base
Issue-Key: bedrock-agent__cloudformation__iam__s3__s3vectors.deploy-data-source-cfn-rejects-unknown-knowledge-base|run#14|AssertionError|assert got == expected, f"expected {expected!r}, got {got!r}"
Issue-Commit: 632c4df423382c9e7bd6d219524f5d2a3f63c78e

# bedrock-agent__cloudformation__s3: AWS::Bedrock::DataSource CREATE_FAILED reason forwards the bedrock-agent message instead of CloudFormation's NotFound wording

> _Note: this issue was identified against the code base at commit `632c4df423382c9e7bd6d219524f5d2a3f63c78e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDataSource

## Description
Cause 1 of 1: the CloudFormation resource handler for AWS::Bedrock::DataSource, when the template's KnowledgeBaseId names a knowledge base that does not exist, reports the downstream bedrock-agent service message verbatim in the resource's CREATE_FAILED ResourceStatusReason instead of re-wording it as the CloudFormation resource-provider framework does. Failing check: bedrock-agent__cloudformation__iam__s3__s3vectors.deploy-data-source-cfn-rejects-unknown-knowledge-base.

AWS: Smithy com.amazonaws.bedrockagent#CreateDataSource — PUT /knowledgebases/{knowledgeBaseId}/datasources/, input knowledgeBaseId (@required, @httpLabel, pattern ^[0-9a-zA-Z]{10}$), modeled error ResourceNotFoundException (errorKind client, httpError 404). The model says nothing about CloudFormation stack-event text, so the reason wording is recorded behavior only: a stack whose AWS::Bedrock::DataSource names a KnowledgeBaseId Bedrock never issued reaches ROLLBACK_COMPLETE, and the DataSource resource's CREATE_FAILED ResourceStatusReason is the resource-provider envelope
  Resource handler returned message: "<message>" (RequestToken: <token>, HandlerErrorCode: NotFound)
whose <message> is the provider framework's standard not-found sentence:
  Resource of type '<CFN type name>' with identifier '<identifier>' was not found.
Its FIXED text is "Resource of type '", "' with identifier '", and "' was not found."; its variable parts are the CFN type name of the missing resource (here AWS::Bedrock::KnowledgeBase, i.e. the referenced type, not the type being created) and the identifier taken verbatim from the template's KnowledgeBaseId property. HandlerErrorCode is NotFound, and the RequestToken is a per-run value.

Emulator: the stack status (ROLLBACK_COMPLETE), the envelope and the HandlerErrorCode (NotFound) are all correct, but the message is the bedrock-agent service sentence rather than the provider's. The replay reports expected ("Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier '<kb-id>' was not found.", 'NotFound') and got ('KnowledgeBase with id <kb-id> is not found.', 'NotFound') — i.e. the handler maps bedrock-agent's ResourceNotFoundException onto HandlerErrorCode NotFound but passes that exception's own message straight through.

Why: the CREATE_FAILED reason is the only place a client learns WHICH resource a failed provisioning could not find. The provider wording names the CloudFormation type (AWS::Bedrock::KnowledgeBase) and the identifier in a shape that is uniform across every resource type, so deployment tooling, CDK/SAM error surfaces and CI log matchers parse it generically to tell "a referenced resource is missing" from "a property is invalid". A service-native sentence that names no CFN type breaks that parsing, and an operator reading the event cannot tell that the missing thing was a knowledge base resource type rather than the data source itself.

Fix: the AWS::Bedrock::DataSource create path of the CloudFormation resource provisioner — the code that catches the bedrock-agent CreateDataSource ResourceNotFoundException and classifies it as HandlerErrorCode NotFound — must synthesise the provider's standard "Resource of type '<Type>' with identifier '<id>' was not found." message, filling <Type> with AWS::Bedrock::KnowledgeBase and <id> with the template's KnowledgeBaseId property value, instead of forwarding the service message. The envelope assembly and HandlerErrorCode mapping already behave correctly and need no change.

Traces: bedrock-agent__cloudformation__iam__s3__s3vectors.deploy-data-source-cfn-rejects-unknown-knowledge-base (assertion in run(): `assert got == expected` on handler_reason(failed_reason(stack_events, "Ds"))). The check's positive control — the same template naming a real knowledge base deploying a DataSource that bedrock-agent lists and reads back, Ref(Ds) == '<kb id>|<data source id>', and the stack delete removing it — was never reached and must be confirmed by the same replay.

## Evidence
Failing check bedrock-agent__cloudformation__iam__s3__s3vectors.deploy-data-source-cfn-rejects-unknown-knowledge-base [mismatch]:

  AssertionError: expected ("Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier '4c9eac9f27' was not found.", 'NotFound'), got ('KnowledgeBase with id 4c9eac9f27 is not found.', 'NotFound')
  (check_bedrock_agent__cloudformation__iam__s3__s3vectors_deploy_data_source_cfn_rejects_unknown_knowledge_base.py, line 339)

The pair compared is (message, HandlerErrorCode) extracted from the DataSource resource's CREATE_FAILED ResourceStatusReason with
  ^Resource handler returned message: "(?P<message>.*)" \(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?P<code>[A-Za-z]+)\)$

Expected (real AWS), for a template resource
  "Ds": { "Type": "AWS::Bedrock::DataSource", "Properties": { "KnowledgeBaseId": "<10-char id Bedrock never issued>", "Name": "<name>", "DataSourceConfiguration": {"Type": "S3", "S3Configuration": {"BucketArn": <GetAtt DsBucket.Arn>}} } }
  StackStatus                        = ROLLBACK_COMPLETE
  ResourceStatusReason (Ds)          = Resource handler returned message: "Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier '<KnowledgeBaseId>' was not found." (RequestToken: <token>, HandlerErrorCode: NotFound)
  message fixed text                 = Resource of type '…' with identifier '…' was not found.
  message variable parts             = the CFN type name AWS::Bedrock::KnowledgeBase, and <KnowledgeBaseId> copied from the template property
  HandlerErrorCode                   = NotFound

Actual (emulator)
  StackStatus                        = ROLLBACK_COMPLETE (matches)
  HandlerErrorCode                   = NotFound (matches)
  message                            = KnowledgeBase with id <KnowledgeBaseId> is not found.  <-- bedrock-agent's own ResourceNotFoundException text, naming no CloudFormation type

The id shown in the replay output ('4c9eac9f27') is a per-run generated value and fills the template's <KnowledgeBaseId> slot; it is not part of the requirement.

Not asserted yet because the check aborts here: the rollback removing the stack's S3 bucket (HeadBucket refused), and the whole positive control (CREATE_COMPLETE, ListDataSources/GetDataSource read-back with status AVAILABLE and dataSourceConfiguration {"type":"S3","s3Configuration":{"bucketArn":…}}, Ref(Ds) == '<kb id>|<data source id>', and GetDataSource refused ResourceNotFoundException / "DataSource with id <ds id> is not found." after the stack delete).

## Steps to Reproduce
1. Deploy a CloudFormation stack containing only:
   - "DsBucket": AWS::S3::Bucket with a unique BucketName
   - "Ds": AWS::Bedrock::DataSource with Properties KnowledgeBaseId set to a syntactically valid 10-character id ([0-9a-zA-Z]{10}) that no knowledge base uses, Name set to a unique name, and DataSourceConfiguration {"Type": "S3", "S3Configuration": {"BucketArn": {"Fn::GetAtt": ["DsBucket", "Arn"]}}}
2. Poll DescribeStacks by StackId until a terminal status. Expected and observed: ROLLBACK_COMPLETE.
3. Call DescribeStackEvents for the stack and take the ResourceStatusReason of the "Ds" row whose ResourceStatus is CREATE_FAILED.
4. Match it against ^Resource handler returned message: "(?P<message>.*)" \(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?P<code>[A-Za-z]+)\)$
   Expected: code = NotFound and message = Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier '<the KnowledgeBaseId from step 1>' was not found.
   Actual:   code = NotFound and message = KnowledgeBase with id <the KnowledgeBaseId from step 1> is not found.
5. Confirm the rest of the check once the message is fixed: HeadBucket on the step-1 bucket must be refused (the rollback deleted it), then redeploy the same DataSource template against a KnowledgeBaseId produced by a real AWS::Bedrock::KnowledgeBase stack and verify CREATE_COMPLETE, the bedrock-agent ListDataSources/GetDataSource read-back, Ref(Ds) == '<kb id>|<data source id>', and removal on stack delete.
