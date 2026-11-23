Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden
Issue-Key: eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden|run#53|AssertionError|assert refused["message"] == (
Issue-Commit: c545ad358c0acc1c6d0e9e2d8dee3b8a2893a6bd

# eks: the served cluster Kubernetes API's 403 Forbidden message for a named object omits the quoted resource name, so it is not the apiserver's sentence

> _Note: this issue was identified against the code base at commit `c545ad358c0acc1c6d0e9e2d8dee3b8a2893a6bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster

## Description
ONE net-new root cause in the Kubernetes API the EKS emulator serves at cluster.endpoint.

Cause 1 of 1: an RBAC-denied write of a NAMED object is refused 403/Forbidden with a message that drops the object-name clause, so the emulator's sentence is not the Kubernetes apiserver's sentence.

AWS:      Smithy (services/service-com-amazonaws-eks-awswesleyfrontend) models only DescribeCluster -> Cluster.endpoint / Cluster.certificateAuthority; the behavior of the Kubernetes API served at that endpoint is recorded behavior only. On a real EKS cluster, a request for a verb the caller's bound Role does not grant on a resource identified BY NAME (e.g. PUT /api/v1/namespaces/<ns>/configmaps/<name>) is answered HTTP 403 with a v1 Status object whose reason is "Forbidden" and whose message is the apiserver's fixed template:
          <resource> "<name>" is forbidden: User "<username>" cannot <verb> resource "<resource>" in API group "<group>" in the namespace "<namespace>"
          Fixed text: ` is forbidden: User `, ` cannot `, ` resource `, ` in API group `, ` in the namespace `, and the quoting of each value. Variable parts, taken from the request, the access entry and the bound Role/RoleBinding: the plural resource token (configmaps), the object name, the authenticated username (an EKS access entry's `username`), the verb (update), the API group ("" for core/v1) and the namespace. The leading `<resource> "<name>"` clause is present exactly when the request names an object; only a collection-scoped (unnamed) request yields the shorter `<resource> is forbidden: ...` form.
Emulator: the refusal is 403 with reason "Forbidden" (correct), but the message is built WITHOUT the object-name clause — it reads `configmaps is forbidden: User "<username>" cannot update resource "configmaps" in API group "" in the namespace "<namespace>"` for a request that named the object. The name is known to the emulator: it populates details.name/details.kind in the same Status body, so only the message template is wrong.
Why:      the apiserver's Forbidden sentence is a stable, parsed contract. Clients, operators and CI assertions match on `<resource> "<name>" is forbidden:` to attribute a denial to a specific object; kubectl renders the message verbatim as `Error from server (Forbidden): ...`. With the name missing, a denial cannot be attributed to the object it was issued for, and a conformance check comparing the exact sentence fails even though the status code and reason are right.
Fix:      the authorization-refusal serializer of the served cluster Kubernetes API (the EKS cluster data-plane handler that renders the v1 Status body for an RBAC denial): include the `<resource> "<name>" ` prefix whenever the request path names an object, and keep the shorter form only for collection-scoped requests.
Note:     open issue 20261122T113249Z-eks-3-net-new-root-causes-in-the.md quotes this same sentence as the EXPECTED text of its Cause 2 (DELETE answered 405 MethodNotAllowed), but its defect is the unrouted DELETE verb, it carries only the eks.* replay keys it was filed from, and it does not cover this check — here the verb IS routed and authorized, and it is the rendered message that diverges.
Traces:   eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden.

## Evidence
Check eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden, at the assertion on the refused update's message.

Request: PUT /api/v1/namespaces/<ns>/configmaps/<name> with the current resourceVersion, signed as the principal that an EKS access entry maps to username <username> in group <group>, where that group is bound by a RoleBinding to a Role granting only {apiGroups: [""], resources: ["configmaps"], verbs: ["get"]}.

Actual (emulator), HTTP 403:
  {'kind': 'Status', 'apiVersion': 'v1', 'metadata': {}, 'status': 'Failure',
   'message': 'configmaps is forbidden: User "sim2real-cm-reader" cannot update resource "configmaps" in API group "" in the namespace "sim2real-xsvc-c8271862"',
   'reason': 'Forbidden', 'details': {'name': 'sim2real-settings', 'kind': 'configmaps'}, 'code': 403}

Expected (AWS), HTTP 403, reason Forbidden, message:
  configmaps "<name>" is forbidden: User "<username>" cannot update resource "configmaps" in API group "" in the namespace "<namespace>"

For the run above that is: configmaps "sim2real-settings" is forbidden: User "sim2real-cm-reader" cannot update resource "configmaps" in API group "" in the namespace "sim2real-xsvc-c8271862". The object name sim2real-settings, the username sim2real-cm-reader and the namespace sim2real-xsvc-c8271862 are values from this run's request/state and fill the template; the fixed text is ` is forbidden: User `, ` cannot update resource "configmaps" `, ` in API group "" `, ` in the namespace ` plus the quoting. The only divergence is the missing `"sim2real-settings" ` clause after the resource token — status code (403), reason (Forbidden) and the rest of the sentence match.

## Steps to Reproduce
1. CreateCluster with two subnets, a cluster service role and accessConfig {authenticationMode: API, bootstrapClusterCreatorAdminPermissions: true}; wait for ACTIVE. Build the Kubernetes client from DescribeCluster's cluster.endpoint + cluster.certificateAuthority.data.
2. CreateAccessEntry for a second IAM role principal with username "sim2real-cm-reader" and kubernetesGroups ["sim2real-cm-readers"].
3. As the cluster creator: POST /api/v1/namespaces (name <ns>), then POST /api/v1/namespaces/<ns>/configmaps with metadata.name "sim2real-settings" and data {color: blue}.
4. As the cluster creator: POST a Role in <ns> granting verbs ["get"] on core/v1 configmaps, and a RoleBinding of that Role to Group "sim2real-cm-readers".
5. Assume the second role and GET /api/v1/namespaces/<ns>/configmaps/sim2real-settings -> 200.
6. With the same credentials PUT that same path with the current resourceVersion -> 403 Forbidden, but message 'configmaps is forbidden: User "sim2real-cm-reader" cannot update resource "configmaps" in API group "" in the namespace "<ns>"' instead of 'configmaps "sim2real-settings" is forbidden: User "sim2real-cm-reader" cannot update resource "configmaps" in API group "" in the namespace "<ns>"'.
