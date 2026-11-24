Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: c6dd973eb6cd65a31ae8eee55c5b01fc505934bd

# An update that states no member still advances DateModified, so a read reports a last-modified instant at which nothing changed

_Filed: 2026-11-24T21:09:20Z_
_Operation: UpdateLifecyclePolicy | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `c6dd973eb6cd65a31ae8eee55c5b01fc505934bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateLifecyclePolicy` declares exactly one required member, the `@httpLabel` `PolicyId`; every member that could change the policy is optional. The handler applies each of them under its own `if let Some(..)`, and then calls `policy.mark_modified(now)` unconditionally, outside all of them.

So `PATCH /policies/{PolicyId}` with a body that states no member at all — a request the model admits — writes a new `date_modified` onto the record even though not one byte of the policy's projection changed. A subsequent `GetLifecyclePolicy` reports a `DateModified` that is "the local date and time when the lifecycle policy was last modified" at an instant at which the policy was not modified, and a client that polls `DateModified` to detect change sees a change that did not happen.

That contradicts the rule the crate writes on the helper itself: `mark_modified` is documented as the stamp of an accepted write "whichever member it changes", ending "so a policy nothing changed keeps the instant it had". The handler reaches it on a request that changed nothing.

## Evidence

The rule the crate states, services/service-com-amazonaws-dlm-dlm-20180112/src/state.rs:813-826:
```
    /// Stamp the instant this policy was last written, which
    /// `LifecyclePolicy$DateModified` reports as "the local date and time when
    /// the lifecycle policy was last modified".
    ///
    /// EVERY accepted write of this record goes through here, whichever member
    /// it changes — the configuration, the activation state, the tags — because
    /// a read projects the whole record ([`LifecyclePolicyRecord::to_policy`])
    /// and the instant it reports is therefore the instant of the last change
    /// to ANY of it, not of the last change to one chosen member. A refused
    /// request never reaches it: every refusal is decided before the first
    /// write, so a policy nothing changed keeps the instant it had.
    pub fn mark_modified(&mut self, now: SystemTime) {
        self.date_modified = now;
    }
```

The code under it, services/service-com-amazonaws-dlm-dlm-20180112/src/ops/update_lifecycle_policy.rs:375-448 — every mutation is conditional, the stamp is not:
```
        // A partial update: only the members the request carries overwrite the
        // stored policy, and every absent member keeps the value the create or an
        // earlier update stored.
        if let Some(execution_role_arn) = input.execution_role_arn {
...
        if let Some(exclusions) = input.exclusions {
            policy.exclusions = Some(exclusions);
        }
        // "The local date and time when the lifecycle policy was last modified."
        policy.mark_modified(now);
```

That such a request is well formed — `smithy_query(query="operation", operation="UpdateLifecyclePolicy")`: `PolicyId` is the only member marked `"required": true`; `ExecutionRoleArn`, `State`, `Description`, `PolicyDetails`, `CreateInterval`, `RetainInterval`, `CopyTags`, `ExtendDeletion`, `CrossRegionCopyTargets` and `Exclusions` are all optional. The crate's own OPERATIONS.md:78 lists the same: only `PolicyId` is `required`.

Test coverage: tests/all/lifecycle_policy_crud.rs:1110-1142 (`an_update_advances_date_modified_and_leaves_date_created`) sends `update(&router, &id, json!({"State": "DISABLED"}))` — a real change, so it is unaffected by fixing this. No test sends an update that states no member, and tests/all/tag_write_date_modified.rs:6-10 states the same rule this breaks: "EVERY accepted write of a policy record stamps it with the instant of that write — whichever member of the projection a read reports the write changed ... and a REFUSED request stamps nothing." The sibling symptom on the tag path is already filed (20261029T180848Z-dlm-untagresource-wrong-behavior); this is the update path, which no issue covers.

## Steps to Reproduce

1. POST /policies with a valid `ExecutionRoleArn`, `Description`, `State: "ENABLED"` and a custom snapshot `PolicyDetails`. Note the PolicyId.
2. GET /policies/{PolicyId} and record `/Policy/DateModified`.
3. Wait long enough for the `date-time` rendering to differ (> 1s).
4. PATCH /policies/{PolicyId} with the body `{}` — no member but the URI `PolicyId`.
5. Observe HTTP 200.
6. GET /policies/{PolicyId} again: `/Policy/DateModified` has advanced to the instant of the empty PATCH, while every other member of the projection is byte-for-byte what step 2 reported — a "last modified" instant at which nothing was modified.
