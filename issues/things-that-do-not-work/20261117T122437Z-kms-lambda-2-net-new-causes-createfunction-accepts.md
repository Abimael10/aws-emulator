Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__lambda.create-function-code-source-kms-key-arn-naming-an-absent-key-rejected kms__lambda.create-function-with-an-asymmetric-kms-key-for-environment-variables-rejected
Issue-Key: kms__lambda.create-function-code-source-kms-key-arn-naming-an-absent-key-rejected|_error#12|AssertionError|raise AssertionError(f"expected a rejection, call succeeded instead: {result!r}")
Issue-Key: kms__lambda.create-function-with-an-asymmetric-kms-key-for-environment-variables-rejected|_error#17|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__lambda: 2 net-new causes — CreateFunction accepts a Code.SourceKMSKeyArn naming an absent key and accepts an asymmetric KMSKeyArn for environment variables

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunction

## Description
Two net-new root causes in Lambda's KMS validation at function creation. Related to but distinct from the open issue 20261105T111951Z-kms-lambda-environment-variable-kms-enforcement-and-kms.md.

Cause 1 of 2: Code.SourceKMSKeyArn is not resolved in KMS, so a deployment package 'encrypted' under a nonexistent key is accepted.
  AWS: recorded behavior only — CreateFunction with Code.SourceKMSKeyArn naming a key KMS reports not found is refused 400 InvalidParameterValueException whose message wraps KMS's own refusal: "Lambda can't find the KMS key. Check your KMS key settings. KMS exception: NotFoundException KMS message: Key '<sourceKmsKeyArn>' does not exist (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: <kms request id>; Proxy: null)" — the fixed text and the key ARN from the request, with only the KMS request id varying per call. No function is created.
  Emulator: the call returns 201 and creates the function (State 'Pending'), silently accepting the unusable key.
  Why: a client that validates its packaging key at create time gets a function whose source could never be decrypted, and the InvalidParameterValueException its error path handles never arrives.
  Fix: the CreateFunction handler's Code.SourceKMSKeyArn validation — resolve the key in KMS state and wrap KMS's NotFoundException into Lambda's InvalidParameterValueException before creating anything.
  Traces: kms__lambda.create-function-code-source-kms-key-arn-naming-an-absent-key-rejected.

Cause 2 of 2: an asymmetric KMS key supplied as KMSKeyArn for environment variables is accepted.
  AWS: recorded behavior only — Lambda encrypts environment variables with the key at create time, so an asymmetric (e.g. RSA_2048 SIGN_VERIFY) key fails the encryption and CreateFunction is refused with ServiceException and the exact message 'Lambda was unable to encrypt your environment variables. Please try again.' (fixed text, no variable parts). Nothing is created: GetFunction on the name afterwards is ResourceNotFoundException.
  Emulator: the call returns 201 with the function created and KMSKeyArn echoed as the asymmetric key.
  Why: the function exists with an encryption key that cannot encrypt; a client sees success where AWS fails, and its retry/fallback on ServiceException is never taken.
  Fix: the CreateFunction handler's environment-variable encryption path — it must actually resolve the key's spec/usage in KMS state and refuse an unusable key with ServiceException, leaving no function behind.
  Traces: kms__lambda.create-function-with-an-asymmetric-kms-key-for-environment-variables-rejected.

## Evidence
create-function-code-source-kms-key-arn-naming-an-absent-key-rejected — AssertionError: expected a rejection, call succeeded instead: {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'FunctionName': 'sim2real-xsvc-codekms-broken-fn-<id>', ... 'State': 'Pending'}. Expected code InvalidParameterValueException with the message prefix "Lambda can't find the KMS key. Check your KMS key settings. KMS exception: NotFoundException KMS message: Key '<arn>' does not exist (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: " and suffix '; Proxy: null)'.

create-function-with-an-asymmetric-kms-key-for-environment-variables-rejected — AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'FunctionName': 'sim2real-behav-kmsasym-bad-fn-<id>', 'Environment': {'Variables': {'PROBE': '1'}}, 'KMSKeyArn': 'arn:aws:kms:us-west-2:<acct>:key/<asymmetric key>' ...}. Expected ('ServiceException', 'Lambda was unable to encrypt your environment variables. Please try again.') and GetFunction -> ResourceNotFoundException.

## Steps to Reproduce
1. Cause 1: lambda.create_function(FunctionName=<n>, Runtime='python3.13', Role=<role>, Handler='index.lambda_handler', Code={'ZipFile': <zip>, 'SourceKMSKeyArn': 'arn:aws:kms:<region>:<acct>:key/<random uuid>'}) -> expect 400 InvalidParameterValueException with the wrapped KMS NotFoundException message and no function created.
2. Cause 2: create a KMS key with KeySpec=RSA_2048, KeyUsage=SIGN_VERIFY; lambda.create_function(..., Environment={'Variables': {'PROBE': '1'}}, KMSKeyArn=<that arn>) -> expect ServiceException 'Lambda was unable to encrypt your environment variables. Please try again.' and GetFunction -> ResourceNotFoundException.
