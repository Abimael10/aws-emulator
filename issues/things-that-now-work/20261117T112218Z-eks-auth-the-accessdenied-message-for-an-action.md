Issue-Severity: MEDIUM
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# eks-auth: the AccessDenied message for an action outside the pod-identity role's permissions omits AWS's trailing authorization-details pointer (1 cause)

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListRoles

## Description
One net-new root cause, reached by the failing check in the `eks-auth` suite section. The pod-identity credential scoping itself is correct here — the ungranted action IS refused, with the right error code and the right reason clause — the divergence is confined to the refusal's message text.

Cause 1 of 1: a denial served to a principal created by AssumeRoleForPodIdentity stops at the reason clause and drops AWS's authorization-details pointer.

AWS: when a request made with credentials minted by eks-auth AssumeRoleForPodIdentity attempts an action the pod role's identity-based policy does not allow, the call is refused with HTTP 403 and error code `AccessDenied`, and the message is the standard IAM no-policy denial followed by a pointer to a per-request, server-minted authorization id, which appears twice:

  `User: <assumed-role session arn> is not authorized to perform: <action> on resource: <resource> because no identity-based policy allows the <action> action. Go to https://<console region>.console.aws.amazon.com/iam/home?region=<console region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`

Fixed text: everything outside the angle brackets, including the literal `. Go to https://`, `.console.aws.amazon.com/iam/home?region=`, `#/authorization-details/`, ` for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: `. Variable parts, taken from the request, the caller's identity and server state: the assumed-role session ARN of the pod-identity principal, the action (`<service>:<Operation>`), the resource ARN the action was evaluated against, the console region (IAM's global partition region), and the authorization id — the same id in both positions. The authorization id is per-request and the only part a client reads back; it is never a literal from one run.

Smithy grounding: iam#ListRoles models only ServiceFailureException (500) — the AccessDenied denial class and its message are recorded behavior only; the status/code/message come from the recorded response.

Emulator: the message ends immediately after the reason clause. Replay shows `User: arn:aws:sts::<account>:assumed-role/<pod role>/<session name> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action` with no `. Go to https://…#/authorization-details/<id> …` segment and no GetRequestAuthorizationDetails pointer at all. The code (`AccessDenied`) and the reason clause are correct; only the trailer is missing.

Why: an operator or tool that extracts the authorization id from a refusal in order to call GetRequestAuthorizationDetails has nothing to extract, so the documented diagnosis path for a pod-identity permission refusal is unavailable on this emulator. Any client, test or log pipeline that matches AWS's denial text diverges, and the omission makes a correctly-scoped refusal indistinguishable from a differently-worded one.

Fix: the denial-message builder used by the authorization gate that serves requests signed with eks-auth pod-identity credentials — it must mint a per-request authorization id and append the pointer clause (with the id repeated in both positions) to the reason clause it already produces.

Related (does NOT cover this failure): open issues 20261107T153110Z-iam-the-session-policy-accessdenied-message-omits-aws.md and 20261114T024343Z-iam-5-net-new-causes-createservicelinkedrole-accepts-an.md (its Cause 2) state the same omission for denials reached through the `iam` suite section's own checks. Neither carries the replay key of the eks-auth check listed below, and neither covers the pod-identity credential path, so this failure is not closed by them; they are named here so a fixer can share one message-builder fix across both.

Traces: eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1) against the emulator built from this checkout. Overall 17/22; eks-auth section 17/21 (81%), 4 failed — the other three eks-auth failures and the one eks__eks-auth failure are marked OWNED by 20261115T083258Z-eks-auth-assumeroleforpodidentity-resolves-clustername-before-the-token.md and are not part of this issue.

Cause 1 — [mismatch] eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected:

AssertionError: EXPECTED_MESSAGE='User: arn:aws:sts::000000000000:assumed-role/sim2real-eksa-podrole-<id>/eks-sim2real-eksauth-<id>-sim2real-pod-<id>-<uuid> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::000000000000:role/ because no identity-based policy allows the iam:ListRoles action. Go to <authorization-details pointer>' GOT='User: arn:aws:sts::000000000000:assumed-role/sim2real-eksa-podrole-<id>/eks-sim2real-eksauth-<id>-sim2real-pod-<id>-<uuid> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::000000000000:role/ because no identity-based policy allows the iam:ListRoles action'

(The role name, session name and account id above are per-run values and are NOT requirements; the account id is read back from the response's own assumedRoleUser.arn. The requirement is the presence and fixed text of the trailing pointer clause.)

The check builds its expectation by reading the pointer's region and authorization id back out of the emulator's OWN message with the regex
  `\. Go to https://[a-z0-9-]+\.console\.aws\.amazon\.com/iam/home\?region=[a-z0-9-]+#/authorization-details/(\w+) for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: \1`
so the per-request id and the region are explicitly ungraded. It fell back to the literal placeholder `. Go to <authorization-details pointer>` precisely because the emulator emitted no such clause — the failure is the structural absence of the trailer, not a per-run value difference.

Everything preceding the trailer matched: the check's earlier assertions passed, i.e. error code was `AccessDenied` and the granted action (cloudwatch:ListMetrics with the same borrowed credentials) succeeded, so pod-identity scoping is otherwise working.

Model grounding (smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json): ListRoles input PathPrefix/Marker/MaxItems, output Roles (required)/IsTruncated/Marker, errors: ServiceFailureException only (server, httpError 500). No AccessDenied is modelled, so the denial status, code and message are recorded behavior only.

## Steps to Reproduce
1. git_clone the repo; build and boot the emulator from the checkout; run the behavioral suite (simparity-behavioral-aws-v1) and read the `eks-auth` section.
2. Create an EKS cluster and a pod identity association whose role carries a narrow inline policy allowing only `cloudwatch:ListMetrics` on `*`.
3. Obtain the service-account token for that pod and call eks-auth AssumeRoleForPodIdentity(clusterName=<cluster>, token=<token>); take `credentials` and `assumedRoleUser.arn` from the response.
4. With those borrowed credentials, call cloudwatch ListMetrics — it succeeds (granted action).
5. With the same borrowed credentials, call iam ListRoles. Observe the refusal: the error code is `AccessDenied` (correct) but `Error.Message` ends at `... because no identity-based policy allows the iam:ListRoles action`.
6. Look for the trailing `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>` clause, with the same minted id in both positions — it is absent.
7. Replay eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected to confirm the fix.
