Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codecommit__codepipeline__glue__iam__lambda.e2e-diagnose-glue-pipeline-not-running-lifecycle
Issue-Key: codecommit__codepipeline__glue__iam__lambda.e2e-diagnose-glue-pipeline-not-running-lifecycle|phase#6|AssertionError|raise AssertionError(f'[phase {index}/{len(self.phases)} {name}] {exc}') from exc
Issue-Commit: 37e3f599a06cc415cb3d05dd7e4df6099b0b715c

# Lambda GetFunction publishes a Code.Location on an amazonaws.com host the emulator does not serve, so the code bytes cannot be fetched

> _Note: this issue was identified against the code base at commit `37e3f599a06cc415cb3d05dd7e4df6099b0b715c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFunction

## Description
Cause 1 of 1: GetFunction's Code.Location is serialized as a real-AWS presigned S3 URL host instead of an address on one of the emulator's own listeners.

AWS: GetFunction returns Code.Location, a presigned URL from which the caller can download the function's deployment package; the URL is resolvable and a GET against it returns the zip bytes. Smithy: lambda#GetFunction returns FunctionCodeLocation{RepositoryType, Location}, and Location is documented as a presigned URL valid for the caller to download the package — recorded behavior only for which host serves it; the requirement is that the announced URL is fetchable from the caller.

Emulator: Code.Location is announced as `https://awslambda-<region>-tasks.s3.<region>.amazonaws.com/snapshots/<account>/<function>-<uuid>` — a host that is not one of the emulator's own hosts. A customer of the emulator needs `http(s)://<emulator host>:<port>/<resource path>`. The caller therefore cannot fetch the package at all; the announced address is for a service the emulator is not reached on.

Why: any client that downloads a function's code to inspect, diff or re-deploy it (the exact diagnosis this trace performs: reading back the deployed launcher bytes) is sent to an address outside the emulator. It either fails to connect or, worse, reaches real AWS. The returned value is unusable even though the operation reported success.

Fix: the GetFunction serializer's Code.Location construction, which must mint a URL on the emulator's own host/port (or the per-resource listener) rather than the upstream S3 host template.

Traces: codecommit__codepipeline__glue__iam__lambda.e2e-diagnose-glue-pipeline-not-running-lifecycle.

## Evidence
Expected: GetFunction.Code.Location is an address served by the emulator, e.g. http(s)://<emulator host>:<port>/<path>.
Actual: AssertionError: [phase 2/4 solve] endpoint_url was given https://awslambda-us-west-2-tasks.s3.us-west-2.amazonaws.com/snapshots/000000000000/sim2real-refarch-glue-stack-ddac8-EtlLaunchFunction-…: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:33145/<resource path>

## Steps to Reproduce
1. Deploy a Lambda function (e.g. through a CloudFormation stack with Code.ZipFile).
2. Call GetFunction on it.
3. Read Code.Location and attempt to GET it; observe the host is an amazonaws.com S3 host the emulator does not serve.

## Resolution
Cause 1 is FIXED. By the time this report was picked up the code base had drifted past the commit it
was filed against: a download location is no longer formatted onto an upstream S3 hostname but
PRESIGNED against the base URL this emulator is reached on, with the object's own path-style address
(`/<per-region bucket>/snapshots/<account>/<key>`) carried in the path
(`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/download_location.rs`), and that
same path is read back by the crate's own download plane
(`services/service-com-amazonaws-lambda-awsgirapiservice/src/service.rs`), which serves the stored
deployment-package bytes under `content-type: application/zip`. The recorded unit this report was
derived from replays green.

What was missing was coverage of the route this report reproduces through: a function deployed by a
CloudFormation stack, whose `Code.ZipFile` is the template's SOURCE TEXT rather than an archive, so
the deployment archives it itself. The new suite
`services/service-com-amazonaws-cloudformation-cloudformation/tests/all/stack_function_package_download.rs`
deploys such a stack against an emulator reached at a loopback address with a port, reads the
function back through `GetFunction`, and follows the announced `Code.Location` unsigned: the address
has to be on that endpoint, the bytes served have to digest and measure to the `CodeSha256` and
`CodeSize` the same read reports, and the package has to carry the declared source under the module
file the declared `Handler` resolves. Its rows span the runtime families whose inline form
CloudFormation publishes (Python, Node.js, Ruby), a handler naming a module other than `index`, and
a package the client staged in Amazon S3 itself — which the download has to serve back byte for
byte.
