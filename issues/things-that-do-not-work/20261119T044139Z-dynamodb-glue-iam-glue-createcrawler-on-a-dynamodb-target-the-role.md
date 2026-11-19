Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__glue__iam.dynamodb-crawler-missing-scan-permission-rejected dynamodb__glue__iam.glue-crawler-missing-table-scan-permission-fails-crawl
Issue-Key: dynamodb__glue__iam.dynamodb-crawler-missing-scan-permission-rejected|run#27|AssertionError|assert match, f"expected an embedded Request ID in the message, got {message!r}"
Issue-Key: dynamodb__glue__iam.glue-crawler-missing-table-scan-permission-fails-crawl|run#24|AssertionError|assert _without_request_id(message) == expected_message, (
Issue-Commit: 84137d6b6519f69d9c97ba0d49414e112206ddcf

# Glue CreateCrawler on a DynamoDB target: the role-denial refusal carries an emulator-authored message instead of the relayed DynamoDB AccessDeniedException

> _Note: this issue was identified against the code base at commit `84137d6b6519f69d9c97ba0d49414e112206ddcf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCrawler

## Description
Cause 1 of 1: Glue CreateCrawler refuses a DynamoDB target whose Role cannot read the table with the correct error class but the wrong message — the emulator writes its own summary of the problem instead of relaying the DynamoDB authorization failure it hit while validating the role.

Context: the open-then-fixed issue 20261117T135333Z-dynamodb-glue-iam-glue-crawlers-on-a-dynamodb-target-startcrawler-answers.md made CreateCrawler refuse this case at all (it previously returned 200). That part now holds: the call is refused, HTTP 400, Error Code InvalidInputException — the two checks below get past the code assertion and now fail only on the message. This is therefore NOT a case of a fix leaving a call refused; the refusal is correct and this is a net-new message-fidelity defect in the same handler path.

AWS: Smithy (glue CreateCrawler, errors AlreadyExistsException, InvalidInputException, OperationTimeoutException, ResourceNumberLimitExceededException; Targets required, Role required). The model states only the error class; the recorded behavior shows what AWS puts in InvalidInputException.Message: Glue assumes the supplied Role as the crawler principal and calls dynamodb:DescribeTable on the target table, and when that call is denied it surfaces the downstream DynamoDB error verbatim as the Glue message. Its template is:

  "User: <assumed-role-arn> is not authorized to perform: dynamodb:DescribeTable on resource: <table-arn> because no identity-based policy allows the dynamodb:DescribeTable action (Service: AmazonDynamoDBv2; Status Code: 400; Error Code: AccessDeniedException; Request ID: <request-id>; Proxy: null)"

Fixed text: "User: ", " is not authorized to perform: dynamodb:DescribeTable on resource: ", " because no identity-based policy allows the dynamodb:DescribeTable action (Service: AmazonDynamoDBv2; Status Code: 400; Error Code: AccessDeniedException; Request ID: ", "; Proxy: null)". Variable parts taken from the request and emulator state: <assumed-role-arn> is arn:aws:sts::<account-id>:assumed-role/<role-name-from-the-Role-ARN-in-the-request>/AWS-Crawler (AWS-Crawler is the fixed session name Glue uses); <table-arn> is the ARN of the DynamoDB table named by Targets.DynamoDBTargets[].Path; <request-id> is the DynamoDB request id of the denied DescribeTable, which both checks normalise away but which must be present and must be DynamoDB-shaped — one check matches it with /Request ID: ([A-Z0-9]+);/, i.e. uppercase alphanumeric with no hyphens, not a lowercase UUID. Note the embedded Service/Status Code/Error Code triple is DynamoDB's (AmazonDynamoDBv2 / 400 / AccessDeniedException) even though the outer error is Glue's InvalidInputException.

Emulator: status 400 and code InvalidInputException are right, but the message is the emulator's own text, which names neither the denied principal nor the resource ARN nor the denied action's own error class:

  "DynamoDB table <table-name> does not exist or the role provided does not have access to it. Please make sure the role has dynamodb:DescribeTable and dynamodb:Scan permissions on the table. (Service: glue; Status Code: 400; Error Code: InvalidInputException; Request ID: <uuid>; Proxy: null)"

Three sub-divergences inside the one message: (a) the prose is emulator-authored rather than the relayed DynamoDB denial; (b) the embedded triple says "Service: glue ... Error Code: InvalidInputException" where AWS embeds the downstream "Service: AmazonDynamoDBv2 ... Error Code: AccessDeniedException"; (c) the embedded Request ID is a lowercase hyphenated UUID, not a DynamoDB-style uppercase alphanumeric request id, so a client (and the check) matching AWS's request-id shape finds none.

Why: a client that parses this denial to tell an operator what to fix gets no principal and no resource. It cannot learn which assumed-role identity was denied, which table ARN it was denied on, or that the underlying failure was an IAM AccessDeniedException from DynamoDB rather than a Glue input-validation problem — so automated remediation (adding the one missing action to the one named resource) and log correlation by the downstream DynamoDB request id are both impossible. The message also conflates "table does not exist" with "not authorized", which AWS keeps distinct.

Fix: the CreateCrawler handler's role/target validation path for DynamoDBTargets — where it refuses the role, it must surface the downstream DynamoDB AccessDeniedException as the InvalidInputException message (assembling the assumed-role ARN with the AWS-Crawler session name, the target table's ARN, and a DynamoDB-shaped request id) instead of composing its own text, and must not stamp its own service/error-code into the embedded diagnostic triple.

Traces: dynamodb__glue__iam.dynamodb-crawler-missing-scan-permission-rejected (fails on the Request-ID shape assertion first, because the emulator's UUID does not match AWS's uppercase-alphanumeric request id), dynamodb__glue__iam.glue-crawler-missing-table-scan-permission-fails-crawl (fails on the full message comparison with the request id normalised away).

## Evidence
Check dynamodb__glue__iam.glue-crawler-missing-table-scan-permission-fails-crawl:
AssertionError: CreateCrawler denial message was 'DynamoDB table sim2real-xsvc-glue-ddb-deny-bbcccee764bc does not exist or the role provided does not have access to it. Please make sure the role has dynamodb:DescribeTable and dynamodb:Scan permissions on the table. (Service: glue; Status Code: 400; Error Code: InvalidInputException; Request ID: aa6810d0-04d8-4a23-b3e6-47f1846f0dac; Proxy: null)', expected (Request ID excluded) 'User: arn:aws:sts::<account-id>:assumed-role/<role-name>/AWS-Crawler is not authorized to perform: dynamodb:DescribeTable on resource: arn:aws:dynamodb:<region>:<account-id>:table/<table-name> because no identity-based policy allows the dynamodb:DescribeTable action (Service: AmazonDynamoDBv2; Status Code: 400; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)'

Check dynamodb__glue__iam.dynamodb-crawler-missing-scan-permission-rejected:
AssertionError: expected an embedded Request ID in the message, got 'DynamoDB table <table-name> does not exist or the role provided does not have access to it. Please make sure the role has dynamodb:DescribeTable and dynamodb:Scan permissions on the table. (Service: glue; Status Code: 400; Error Code: InvalidInputException; Request ID: 49b4601f-7c18-43e6-b4ea-7325155a5065; Proxy: null)'
(the check's request-id matcher is /Request ID: ([A-Z0-9]+);/ — AWS's embedded DynamoDB request id is uppercase alphanumeric; the emulator's lowercase hyphenated UUID does not match)

Expected: HTTP 400, Error.Code InvalidInputException (both correct in the emulator) with Error.Message = the relayed DynamoDB denial template above.
Actual: HTTP 400, Error.Code InvalidInputException with an emulator-authored message that embeds 'Service: glue' / 'Error Code: InvalidInputException' and a UUID request id.

## Steps to Reproduce
1. CreateTable a DynamoDB table and put one item in it; wait for ACTIVE and note its TableArn.
2. CreateRole with a trust policy allowing glue.amazonaws.com to sts:AssumeRole, and attach only arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole — grant NO dynamodb:DescribeTable / dynamodb:Scan on the table.
3. Glue CreateDatabase.
4. Glue CreateCrawler with Name=<crawler>, Role=<that role ARN>, DatabaseName=<that database>, Targets={"DynamoDBTargets":[{"Path":"<table name>"}]}.
5. Inspect the ClientError: Error.Code is InvalidInputException (correct) but Error.Message is the emulator's 'DynamoDB table ... does not exist or the role provided does not have access to it. Please make sure the role has dynamodb:DescribeTable and dynamodb:Scan permissions on the table. (Service: glue; ... Error Code: InvalidInputException; Request ID: <uuid>; Proxy: null)'.
6. AWS instead returns 'User: arn:aws:sts::<account>:assumed-role/<role>/AWS-Crawler is not authorized to perform: dynamodb:DescribeTable on resource: <table ARN> because no identity-based policy allows the dynamodb:DescribeTable action (Service: AmazonDynamoDBv2; Status Code: 400; Error Code: AccessDeniedException; Request ID: <uppercase-alphanumeric id>; Proxy: null)'.
