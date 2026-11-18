Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected
Issue-Shape-Id: com.amazonaws.eksauth#EKSAuthFrontend
Issue-Key: eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected|run#17|AssertionError|assert message == expected, f"EXPECTED_MESSAGE={expected!r} GOT={message!r}"
Issue-Commit: b9fa193222dbbf8f6227ac7f16352f3415fbd68d

# eks-auth: AccessDenied for an action outside the pod-identity role's permissions omits AWS's trailing authorization-details pointer (1 cause)

> _Note: this issue was identified against the code base at commit `b9fa193222dbbf8f6227ac7f16352f3415fbd68d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListRoles

## Description
One net-new root cause surviving the ALREADY-FILED/OWNED markers in the `eks-auth` suite section. The pod-identity credential scoping itself is correct — the ungranted action IS refused, with the right HTTP status and the right error code and reason clause — the divergence is confined to the refusal message's trailing clause.

DUPLICATE NOTICE: open issue 20261117T112218Z-eks-auth-the-accessdenied-message-for-an-action.md states this same cause and already lists this same check id under its Traces, but the replay report did NOT mark this check "OWNED by" it, so it is filed here per the sweep rules. A fixer should treat the two as one work item and close them together.

Cause 1 of 1: a denial served to a principal minted by AssumeRoleForPodIdentity stops at the reason clause and drops AWS's authorization-details pointer.

AWS: when a request signed with credentials minted by eks-auth AssumeRoleForPodIdentity attempts an action the pod role's identity-based policy does not allow, the call is refused with HTTP 403 and error code `AccessDenied`, and the message is the standard IAM no-identity-policy denial followed by a pointer to a per-request, server-minted authorization id that appears TWICE:

  `User: <assumed-role session arn> is not authorized to perform: <action> on resource: <resource arn> because no identity-based policy allows the <action> action. Go to https://<console region>.console.aws.amazon.com/iam/home?region=<console region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`

Fixed text (the requirement): every character outside the angle brackets, including the literal `. Go to https://`, `.console.aws.amazon.com/iam/home?region=`, `#/authorization-details/`, ` for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: `. Variable parts, taken from the request, the caller's identity and server state: the assumed-role session ARN of the pod-identity principal, the action as `<service>:<Operation>`, the resource ARN the action was evaluated against, the console region, and the authorization id — the SAME id in both positions. The authorization id and region are per-request and are explicitly ungraded by the check; they are never literals from one run.

Smithy grounding: iam#ListRoles (services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json) models input PathPrefix/Marker/MaxItems, output Roles (required)/IsTruncated/Marker, and exactly one error, ServiceFailureException (server, httpError 500, awsQueryError ServiceFailure). No AccessDenied is modelled, so this denial's status, code and message are recorded behavior only; the status/code/message above come from the recorded response.

Emulator: the message ends immediately after the reason clause. Replay shows `User: arn:aws:sts::<account>:assumed-role/<pod role>/<session name> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action` with no `. Go to https://…#/authorization-details/<id> …` segment and no GetRequestAuthorizationDetails pointer at all. The code (`AccessDenied`) and the reason clause are correct; only the trailer is absent.

Why: a client, operator or tool that extracts the authorization id out of a refusal in order to call GetRequestAuthorizationDetails has nothing to extract, so the documented diagnosis path for a pod-identity permission refusal does not exist on this emulator. Any client, test or log pipeline matching AWS's denial text diverges, and the omission makes a correctly-scoped refusal indistinguishable from a differently-worded one.

Fix: the denial-message builder in the authorization gate that evaluates requests signed with eks-auth pod-identity credentials — it must mint a per-request authorization id and append the pointer clause, with that id repeated in both positions, to the reason clause it already produces.

Traces: eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1) against the emulator built from this checkout. Overall 17/22 passed (77.3%); 5 mismatched, 0 errored, 0 skipped, 0 unloadable. Per-service: eks-auth 17/21 (81%) 4 failed; eks__eks-auth 0/1 (0%) 1 failed. Four of those five failures are marked OWNED by 20261115T083258Z-eks-auth-assumeroleforpodidentity-resolves-clustername-before-the-token.md and are excluded from this issue; the one below is the sole surviving failure.

Cause 1 — [mismatch] eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected, at check_eks_auth_pod_identity_credentials_scoped_to_role_permissions_rejected.py:67 (`assert message == expected`):

AssertionError: EXPECTED_MESSAGE='User: arn:aws:sts::<account>:assumed-role/sim2real-eksa-podrole-<id>/eks-sim2real-eksauth-<id>-sim2real-pod-<id>-<uuid> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action. Go to <authorization-details pointer>' GOT='User: arn:aws:sts::<account>:assumed-role/sim2real-eksa-podrole-<id>/eks-sim2real-eksauth-<id>-sim2real-pod-<id>-<uuid> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action'

The role name, session name, pod name/uid and account id in that text are per-run values and are NOT requirements (the account id is read back out of the response's own assumedRoleUser.arn). The requirement is the presence and the fixed text of the trailing pointer clause.

The check builds its expectation by reading the pointer's region and authorization id back out of the emulator's OWN message with the regex
  `\. Go to https://[a-z0-9-]+\.console\.aws\.amazon\.com/iam/home\?region=[a-z0-9-]+#/authorization-details/(\w+) for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: \1`
so the per-request id and the region are explicitly ungraded. It fell back to the literal placeholder `. Go to <authorization-details pointer>` only because the emulator emitted no such clause at all — the failure is the structural absence of the trailer, not a per-run value difference.

Everything preceding the trailer matched: the check's earlier assertions passed, i.e. `code == "AccessDenied"` held, and the granted action (cloudwatch ListMetrics with the same borrowed credentials) succeeded — so pod-identity credential scoping is otherwise working, and this is purely a message-text defect.

## Steps to Reproduce
1. git_clone the repo; build and boot the emulator from the checkout; run the behavioral suite (simparity-behavioral-aws-v1) and read the `eks-auth` section.
2. Create an EKS cluster, then a pod identity association for namespace `default` / service account `default` whose role carries a narrow inline policy allowing only `cloudwatch:ListMetrics` on `*`.
3. Obtain the bound service-account token for a pod in that namespace and call eks-auth AssumeRoleForPodIdentity(clusterName=<cluster>, token=<token>); take `credentials` and `assumedRoleUser.arn` from the response.
4. With those borrowed credentials call cloudwatch ListMetrics — it succeeds (the granted action).
5. With the same borrowed credentials call iam ListRoles. Observe the refusal: error code is `AccessDenied` (correct) but `Error.Message` ends at `... because no identity-based policy allows the iam:ListRoles action`.
6. Look for the trailing `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>` clause, with the same minted id in both positions — it is absent.
7. Replay eks-auth.pod-identity-credentials-scoped-to-role-permissions-rejected to confirm the fix.
