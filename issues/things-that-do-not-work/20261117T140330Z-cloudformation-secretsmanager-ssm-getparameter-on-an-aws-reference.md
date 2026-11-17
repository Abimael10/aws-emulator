Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__secretsmanager__ssm.deploy-cfn-secret-value-ssm-reference-and-post-delete-refusals
Issue-Key: cloudformation__secretsmanager__ssm.deploy-cfn-secret-value-ssm-reference-and-post-delete-refusals|run#20|AssertionError|assert not ok, (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# cloudformation__secretsmanager__ssm: GetParameter on an /aws/reference/secretsmanager/ reference with WithDecryption=False returns the parameter instead of refusing it

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetParameter

## Description
Cause 1 of 1: the Parameter Store Secrets Manager reference path accepts a read with decryption disabled.
AWS: Smithy com.amazonaws.ssm#GetParameter has an optional WithDecryption flag; the /aws/reference/secretsmanager/<secret-id> name form is Parameter Store's read-through to Secrets Manager. Recorded behavior: a secret can only be read through that reference WITH decryption — GetParameter(Name="/aws/reference/secretsmanager/<secret>", WithDecryption=False) is refused HTTP 400 with Error.Code ValidationException and the message "WithDecryption flag must be True for retrieving a Secret Manager secret." (entirely fixed text, nothing interpolated). The same read with WithDecryption=True succeeds and returns the secret's current value.
Emulator: the WithDecryption=False read succeeds with HTTP 200 and returns a Parameter element (Type SecureString, the Secrets Manager ARN, DataType text) — the read-through is served regardless of the decryption flag.
Why: a client that deliberately reads parameters without decryption (to enumerate or audit without touching secret material) believes the undecrypted read is legal for secret references, and AWS will refuse the identical call in production. The emulator's response also reports Version 0, which no real parameter version is.
Fix: the GetParameter handler's /aws/reference/secretsmanager/ read-through path in the ssm service crate — require WithDecryption=True and otherwise refuse with the ValidationException above.
Traces: cloudformation__secretsmanager__ssm.deploy-cfn-secret-value-ssm-reference-and-post-delete-refusals (the rest of the scenario — the generated secret's length/alphabet and the decrypted reference matching GetSecretValue — passes before this assertion).

## Evidence
Behavioral suite, service heading `### cloudformation__secretsmanager__ssm`, check deploy-cfn-secret-value-ssm-reference-and-post-delete-refusals, line 104:

AssertionError: GetParameter with WithDecryption=False on a Secrets Manager reference must be refused, not return {'Parameter': {'Name': '/aws/reference/secretsmanager/<secret-name>', 'Type': 'SecureString', 'Version': 0, 'LastModifiedDate': <ts>, 'ARN': 'arn:aws:secretsmanager:<region>:<account>:secret:<secret-name>-AIoBMu', 'DataType': 'text'}, 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}

Expected (AWS): HTTP 400, Error.Code ValidationException, Message "WithDecryption flag must be True for retrieving a Secret Manager secret."

The secret name, ARN suffix and timestamp above are per-run values; the requirement is the refusal, its code and its message.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Create a Secrets Manager secret (directly, or via a CloudFormation AWS::SecretsManager::Secret with GenerateSecretString).
3. Call ssm GetParameter with Name="/aws/reference/secretsmanager/<secret-name>" and WithDecryption=True — it returns the secret value (this works).
4. Call the same GetParameter with WithDecryption=False. Observe HTTP 200 with a Parameter element; AWS answers HTTP 400 ValidationException "WithDecryption flag must be True for retrieving a Secret Manager secret.".
5. Re-replay the check listed in trace_ids.
