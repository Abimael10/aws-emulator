Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 3db4d56b02e21f2a4281b49c781769e9a83f4fad

# The authorization gate ignores the group policies in force on an IAM user, on the stated premise that the IAM emulator does not model them — a premise the IAM crate contradicts

_Filed: 2026-11-26T04:49:33Z_
_Operation: AssociateEipToVlan | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `3db4d56b02e21f2a4281b49c781769e9a83f4fad`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::authorization::caller` is the gate every implemented operation of this crate runs first (`AssociateEipToVlan` at src/ops/associate_eip_to_vlan.rs:218, and the identical call in all 23 other implemented handlers). It gathers the identity-based documents in force on the caller from exactly two sources per principal kind: for an assumed-role session, the role's inline policies plus the default version of every managed policy attached to the role; for an IAM user, the user's inline policies plus the user's attached managed policies. The policies in force on the GROUPS the user belongs to are never read.

The module states its own justification for that omission: the rest of a principal's permission set, group policies first in the list, "is not modelled by the IAM emulator". That premise is false. The IAM crate this gate reads from holds group membership (`group_members`, written by `iam:AddUserToGroup`), group inline policies (`group_inline_policies`, written by `iam:PutGroupPolicy`) and group attached managed policies (`group_attached_policies`, written by `iam:AttachGroupPolicy`), and it publishes a helper whose own documentation says it is "where the group policies in force on the user are collected from". So the one IAM decision this gate says it can make soundly and that "is final whatever else the account holds" — an EXPLICIT `Deny` — is silently dropped when it lives in a policy attached to (or embedded in) a group the signing user belongs to. The identical document, attached to the user or to a role, is refused with `AccessDeniedException`; routed through a group it is ignored and every EVS operation is performed for a principal IAM says is denied.

This is the same defect shape as the already-fixed 20261025T163052Z-evs-associateeiptovlan-wrong-behavior ("reads managed policies attached to a role but not those attached to an IAM user"), one policy source further out: that fix added the user's attached policies to the user branch and left the group sources untouched. Nothing in this crate pins the group behaviour with an assertion — the only mention is the prose premise in the test header — and no handler needs to change: the user branch already has the `user_key` the IAM store keys group membership by, and already owns the `inline_documents` / `default_version_document` readers each group's two sources need.

## Evidence

The rule the gate states for itself — services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state/authorization.rs:37-38:
```
//! An EXPLICIT `Deny` about the action is the one IAM decision this emulator can make
//! soundly, and it is final whatever else the account holds, so it is what refuses here.
```

The premise the omission rests on — authorization.rs:39-43:
```
//! An ABSENT `Allow` is NOT evidence that the caller lacks the permission: the rest of a
//! principal's permission set (group policies, permissions boundaries, service control
//! policies, session policies) is not modelled by the IAM emulator, so refusing on its
//! absence would deny calls AWS permits. This is the reading AWS Transfer Family's gate
//! takes of the same store.
```
and the two-source claim it makes at authorization.rs:20-24: "its inline policies and the default versions of the managed policies attached to it, whichever kind of principal it is (a role or a user)".

The user branch, which reads only the user's own two sources — authorization.rs:179-187:
```
    } else if let Some(user_name) = iam_user_name(&caller.arn) {
        let user_key = iam_store::name_key(user_name);
        (
            inline_documents(&store.user_inline_policies, &user_key),
            store.user_attached_policies.get(&user_key),
        )
    } else {
        (Vec::new(), None)
    };
```

What the IAM crate the gate reads from actually holds — services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/state/account.rs:505:
```
    pub group_members: BTreeMap<String, BTreeSet<String>>,
```
account.rs:525: `pub group_inline_policies: BTreeMap<String, BTreeMap<String, InlinePolicy>>,`
account.rs:649: `pub group_attached_policies: BTreeMap<String, BTreeSet<String>>,`
and the IAM handlers that write them: src/ops/add_user_to_group.rs:229 (`.group_members`), src/ops/put_group_policy.rs:312 (`.group_inline_policies`), src/ops/attach_group_policy.rs:291 (`let attached = store.group_attached_policies.entry(group_key).or_default();`).

The helper the IAM store publishes for exactly this use — account.rs:1586-1590:
```
    /// ([`MAX_GROUPS_PER_USER`]), what tells a delete that the user
    /// still belongs to a group, what a report of the user's groups lists, and where
    /// the group policies in force on the user are collected from.
    pub fn group_keys_held_by<'a>(&'a self, user_key: &'a str) -> impl Iterator<Item = &'a String> {
```

That the same store CAN be read this way is shown by another emulator's gate over the same maps — services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/authorization.rs:501-519:
```
        NamedIdentity::User(user_name) => {
            let user_key = iam_store::name_key(user_name);
            collect_entity_policies(iam, &iam.user_inline_policies, &iam.user_attached_policies, &user_key, &mut caller);
            for group_key in iam.group_keys_held_by(&user_key) {
                collect_entity_policies(iam, &iam.group_inline_policies, &iam.group_attached_policies, group_key, &mut caller);
            }
        }
```

Test coverage: nothing in this crate exercises a group policy. The only occurrence of the word in the whole suite is the premise restated in prose — services/service-com-amazonaws-evs-amazonelasticvmwareservice/tests/all/authorization.rs:12-16:
```
//! What does NOT refuse here is as much of the rule as what does. The IAM emulator does
//! not model a principal's whole permission set (group policies, permissions boundaries,
//! service control policies), so an ABSENT `Allow` is not evidence that the caller lacks
//! the permission and only an EXPLICIT `Deny` refuses — the reading
//! `crate::state::authorization` documents.
```
Every row of that suite seeds a ROLE inline policy (tests/all/authorization.rs:66-80, `put_role_policy`); no row writes `group_members`, `group_inline_policies` or `group_attached_policies`, so no test pins the behaviour described here.

## Steps to Reproduce

1. In the IAM emulator: CreateGroup `evs-denied`, CreateUser `evs-user`, and AddUserToGroup to make the user a member (this writes `group_members`, src/ops/add_user_to_group.rs:229).
2. PutGroupPolicy on `evs-denied` with `{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Action":"evs:*","Resource":"*"}]}` (or CreatePolicy + AttachGroupPolicy, which writes `group_attached_policies`).
3. Sign an EVS request as `arn:aws:iam::<account>:user/evs-user` — the ARN form `iam_user_name` resolves (src/state/authorization.rs:218-222).
4. Call any implemented EVS operation, e.g. AssociateEipToVlan or GetEnvironment: the call is performed, because `caller.documents` holds only the user's own inline and attached policies (authorization.rs:179-187) and `explicit_deny` therefore returns `None` (authorization.rs:108-114).
5. Move the identical document to the user itself (PutUserPolicy or AttachUserPolicy) and repeat: the request is now refused with `AccessDeniedException`, showing one explicit Deny is final through one path and invisible through the other.
6. Cross-check that the group sources were populated and readable all along: GetAccountAuthorizationDetails reports them (services/.../iam/src/ops/get_account_authorization_details.rs:468-475), and OpenSearch's gate refuses the same caller for its own actions (services/.../opensearch/src/state/authorization.rs:510-518).
