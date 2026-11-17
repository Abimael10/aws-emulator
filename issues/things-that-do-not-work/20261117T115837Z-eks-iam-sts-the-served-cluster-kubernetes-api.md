Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden
Issue-Key: eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden|run#31|KeyError|stale = first["metadata"]["resourceVersion"]
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# eks__iam__sts: the served cluster Kubernetes API returns created objects without metadata.resourceVersion, so optimistic concurrency (409 Conflict on a stale write) cannot be exercised

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
Cause 1 of 1: objects created through the cluster's served Kubernetes API carry no metadata.resourceVersion.
AWS: recorded behavior only (this is the Kubernetes API EKS serves for the cluster, not a Smithy-modeled EKS operation) — POST /api/v1/namespaces/<ns>/configmaps answers 201 with the created ConfigMap whose metadata includes a server-assigned resourceVersion; a PUT carrying the current resourceVersion is 200 and returns a NEW resourceVersion, and a PUT carrying the now-stale one is refused 409 with reason 'Conflict' and the API server's sentence 'Operation cannot be fulfilled on configmaps "<name>": the object has been modified; please apply your changes to the latest version and try again' (fixed text with the object name from the request substituted), leaving the stored object unchanged.
Emulator: the ConfigMap create is answered 201, but the returned object's metadata has no resourceVersion key at all (KeyError: 'resourceVersion' when reading created['metadata']['resourceVersion']), so no client can perform a versioned update and the whole optimistic-concurrency contract — the 200 at the current version, the 409 Conflict sentence at a stale version, and the unchanged object after the refusal — is unreachable.
Why: every controller, kubectl apply/replace and client-go update loop depends on metadata.resourceVersion for conflict detection; without it a client either cannot issue a versioned write or silently performs last-writer-wins updates that AWS would have refused 409.
Fix: the served cluster Kubernetes API object store / serializer for core v1 objects in the eks service crate's Kubernetes data-plane component — assign and return a monotonically changing metadata.resourceVersion on create and on every successful update, and enforce it on PUT with the 409 Conflict reason and message above. Related but distinct open issues: 20261113T151622Z-eks-10-net-new-root-causes-in-the.md, 20261114T131945Z-eks-2-net-new-root-causes-in-the--r3.md and 20261113T151622Z-ecr-eks-logs-2-net-new-root-causes.md cover other served-Kubernetes-API defects (anonymous access, /version, 405/415 on create bodies); none covers the missing resourceVersion on a created object.
Traces: eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden.

## Evidence
eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden:
  File check_eks__iam__sts_eks_configmap_stale_version_conflict_and_rbac_group_update_forbidden.py, line 127, in run
    stale = first['metadata']['resourceVersion']
  KeyError: 'resourceVersion'

The POST of the ConfigMap was answered 201 (the preceding assert status == 201 passed), so the emulator returned a created object whose metadata omits resourceVersion.
Expected (AWS): 201 with metadata.resourceVersion set; PUT at that version -> 200 with a different resourceVersion; PUT at the stale version -> 409, reason 'Conflict', message 'Operation cannot be fulfilled on configmaps "<name>": the object has been modified; please apply your changes to the latest version and try again', and a subsequent GET showing the data unchanged.

## Steps to Reproduce
1. eks.create_cluster(... accessConfig={'authenticationMode':'API','bootstrapClusterCreatorAdminPermissions':True} ...) and wait for ACTIVE.
2. Against the cluster's published Kubernetes endpoint, as the cluster creator: POST /api/v1/namespaces with a Namespace body (201), then POST /api/v1/namespaces/<ns>/configmaps with {'apiVersion':'v1','kind':'ConfigMap','metadata':{'name':'sim2real-settings'},'data':{'color':'blue'}}.
3. Read metadata.resourceVersion of the 201 response body — it is absent (expected: a server-assigned value).
4. PUT the ConfigMap with that resourceVersion (expect 200 and a new resourceVersion), then PUT again with the old one (expect 409 reason 'Conflict' with the message above) and GET to confirm the stored data did not change.
