Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected
Issue-Shape-Id: com.amazonaws.eksauth#EKSAuthFrontend
Issue-Key: eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected|run#17|AssertionError|assert message == expected, f"EXPECTED_MESSAGE={expected!r} GOT={message!r}"
Issue-Commit: 365042aa5aff1c761f3edc665c19518d88f112ee

# eks-auth: the AccessDenied refusal served to AssumeRoleForPodIdentity credentials omits AWS's trailing authorization-details pointer (1 cause)

> _Note: this issue was identified against the code base at commit `365042aa5aff1c761f3edc665c19518d88f112ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListRoles

## Description
One net-new root cause, grounded in the single eks-auth behavioral failure that the replay report does NOT mark ALREADY FILED or OWNED. The pod-identity credential scoping itself is correct (the granted action succeeds, the ungranted action is refused with the right error code and the right reason clause); the divergence is confined to the refusal message's text.

NOTE ON AN EXISTING ISSUE: open issue 20261117T112218Z-eks-auth-the-accessdenied-message-for-an-action.md describes this same omission on this same check, but the replay report did not mark this check as OWNED by it (it does not carry the check's replay key), so the check is still failing and unowned. If a fixer closes that issue with the same message-builder change, this issue is closed by the same fix — it is filed so the failing check is not left without an owning work item. Also related but NOT covering this failure (they are reached through the `iam`/`sts` suite sections, not the pod-identity credential path): 20261119T085411Z-iam-every-accessdenied-refusal-message-stops-at-action.md, 20261117T135333Z-iam-6-net-new-causes-denials-omit-the.md, 20261117T190411Z-sts-4-net-new-causes-session-policy-accessdenied.md.

Cause 1 of 1: a denial served to a principal minted by eks-auth AssumeRoleForPodIdentity stops at the reason clause and drops AWS's authorization-details pointer sentence.

AWS: when a request signed with credentials minted by AssumeRoleForPodIdentity attempts an action the pod role's identity-based policy does not allow, the call is refused with HTTP 403, error code `AccessDenied`, and the message is the standard IAM no-identity-policy denial followed by a pointer to a per-request, server-minted authorization id that appears twice:

  `User: <assumed-role session arn> is not authorized to perform: <action> on resource: <resource> because no identity-based policy allows the <action> action. Go to https://<console region>.console.aws.amazon.com/iam/home?region=<console region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`

Fixed text (the requirement): every character outside the angle brackets, including the literal `. Go to https://`, `.console.aws.amazon.com/iam/home?region=`, `#/authorization-details/`, ` for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: `. Variable parts, taken from the request, the caller's identity and server state: the assumed-role session ARN of the pod-identity principal, the action (`<service>:<Operation>`), the resource ARN the action was evaluated against, the console region, and the authorization id — the SAME id in both positions. The id, the session name, the role name and the account id are per-run values and are not part of the requirement.

Smithy grounding: smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json — iam#ListRoles models input PathPrefix/Marker/MaxItems, output Roles(required)/IsTruncated/Marker, and only error ServiceFailureException (server, httpError 500). No AccessDenied is modelled, so this denial's status, code and message are recorded behavior only; the status/code/message above are taken from the recorded response.

Emulator: the message ends immediately after the reason clause. Replay shows `User: arn:aws:sts::<account>:assumed-role/<pod-role>/<session-name> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action` with no `. Go to https://…#/authorization-details/<id> …` segment and no GetRequestAuthorizationDetails pointer at all. The error code `AccessDenied` and the reason clause are correct; only the trailer is absent. (The check reads the pointer's region and id back out of the emulator's own message with a regex, so those per-run values are explicitly ungraded; it fell back to the placeholder `. Go to <authorization-details pointer>` precisely because no such clause was emitted — this is a structural absence, not a per-run value difference.)

Why: a client or operator that extracts the authorization id from a refusal in order to call GetRequestAuthorizationDetails has nothing to extract, so AWS's documented diagnosis path for a pod-identity permission refusal is unavailable. Any client, test or log pipeline that matches AWS's denial text diverges, and the omission makes a correctly-scoped refusal indistinguishable from a differently-worded one.

Fix: the denial-message builder used by the authorization gate that evaluates requests signed with eks-auth pod-identity credentials — it must mint a per-request authorization id and append the pointer clause (with that id repeated in both positions) to the reason clause it already produces.

Traces: eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1) against the emulator built from this checkout. Overall 17/22 (77.3%); eks-auth 17/21 (81%), 4 failed; eks__eks-auth 0/1, 1 failed. Three of the eks-auth failures and the one eks__eks-auth failure are marked OWNED by 20261115T083258Z-eks-auth-assumeroleforpodidentity-resolves-clustername-before-the-token.md and are excluded from this issue.

Cause 1 — [mismatch] eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected:

AssertionError: EXPECTED_MESSAGE='User: arn:aws:sts::<account>:assumed-role/sim2real-eksa-podrole-<id>/eks-<cluster>-<pod>-<uuid> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action. Go to <authorization-details pointer>' GOT='User: arn:aws:sts::<account>:assumed-role/sim2real-eksa-podrole-<id>/eks-<cluster>-<pod>-<uuid> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action'

The check's expectation regex, which proves the region and id are ungraded:
  `\. Go to https://[a-z0-9-]+\.console\.aws\.amazon\.com/iam/home\?region=[a-z0-9-]+#/authorization-details/(\w+) for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: \1`

Everything preceding the trailer matched: the earlier assertions in the check passed, i.e. the error code was `AccessDenied` and the granted action (cloudwatch ListMetrics with the same borrowed credentials) succeeded, so pod-identity scoping is otherwise working.

Model grounding: iam#ListRoles declares only ServiceFailureException (500); AccessDenied is unmodelled, hence recorded behavior only.

## Steps to Reproduce
1. git_clone the repo; build and boot the emulator from the checkout; run the behavioral suite (simparity-behavioral-aws-v1) and read the `eks-auth` section.
2. Create an EKS cluster and a pod identity association whose role carries a narrow inline policy allowing only `cloudwatch:ListMetrics` on `*`.
3. Obtain the service-account token for that pod and call eks-auth AssumeRoleForPodIdentity(clusterName=<cluster>, token=<token>); take `credentials` and `assumedRoleUser.arn` from the response.
4. With those borrowed credentials, call cloudwatch ListMetrics — it succeeds (granted action).
5. With the same borrowed credentials, call iam ListRoles. Observe the refusal: error code `AccessDenied` (correct), but `Error.Message` ends at `... because no identity-based policy allows the iam:ListRoles action`.
6. Look for the trailing `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>` clause, with the same minted id in both positions — it is absent.
7. Replay eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected to confirm the fix.
