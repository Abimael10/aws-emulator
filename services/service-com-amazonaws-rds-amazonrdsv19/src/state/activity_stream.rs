// SPDX-License-Identifier: CC0-1.0

//! The AUDIT POLICY STATE of a DB instance's database activity stream: the state
//! `ModifyActivityStream` writes, and the preconditions it holds a request to.
//!
//! `ModifyActivityStream` "changes the audit policy state of a database activity stream to
//! either locked (default) or unlocked", addressing the instance by `ResourceArn` — "the
//! Amazon Resource Name (ARN) of the RDS for Oracle or Microsoft SQL Server DB instance" —
//! and reports the state of that policy back as `ActivityStreamPolicyStatus`. So the policy
//! state is a property OF THE INSTANCE, stored on the instance's own record
//! ([`DbInstanceRecord::activity_stream_policy`]) rather than in a map beside it: it is the
//! same value `DBInstance$ActivityStreamPolicyStatus` reports, it goes when the instance
//! goes, and it follows the instance through a rename.
//!
//! The policy state does not change the instant it is asked for. `ActivityStreamPolicyStatus`
//! models two TRANSITIONAL members beside the two settled ones (`locked | unlocked |
//! locking-policy | unlocking-policy`), so a change of state is reported as the transition it
//! is until this service's clock has made it due ([`STATUS_TRANSITION_DELAY`]), and as the new
//! state afterwards — nothing sleeps for it: the due instant is stored on the record and
//! [`ActivityStreamPolicy::settle`] applies it from [`DbInstanceRecord::settle`].
//!
//! Asking for the state the policy is already in, or already moving to, is no transition at
//! all: the state is a SETTING rather than a step, so the call is taken and reports the policy
//! as it stands, the way every other idempotent write of this service does.
//!
//! A `ResourceArn` that names no DB instance of this service at all is refused on the MEMBER,
//! with the one sentence this front door gives every operation that addresses its resource by
//! ARN ([`tags::invalid_resource_name_message`]), so this operation cannot drift from the tag
//! writes and the Data API toggles in how it words an unusable reference.

use super::*;

/// The `locked` member of `AuditPolicyState` and of `ActivityStreamPolicyStatus`: "when it is
/// locked, it is read-only".
pub const AUDIT_POLICY_STATE_LOCKED: &str = "locked";

/// The `unlocked` member of both enums: "when a policy is unlocked, it is read/write".
pub const AUDIT_POLICY_STATE_UNLOCKED: &str = "unlocked";

/// The state an activity stream's audit policy is in until a modification moves it:
/// `ModifyActivityStream` changes it "to either locked (default) or unlocked", so `locked` is
/// the state a policy nothing has modified is reported in, and the state an absent
/// `AuditPolicyState` asks for.
pub const DEFAULT_AUDIT_POLICY_STATE: &str = AUDIT_POLICY_STATE_LOCKED;

/// The `locking-policy` member of `ActivityStreamPolicyStatus`: the policy is on its way to
/// [`AUDIT_POLICY_STATE_LOCKED`].
pub const ACTIVITY_STREAM_POLICY_LOCKING: &str = "locking-policy";

/// The `unlocking-policy` member of `ActivityStreamPolicyStatus`: the policy is on its way to
/// [`AUDIT_POLICY_STATE_UNLOCKED`].
pub const ACTIVITY_STREAM_POLICY_UNLOCKING: &str = "unlocking-policy";

/// The sentence `ModifyActivityStream` refuses a DB instance whose engine has no database
/// activity stream to modify with.
///
/// It is the operation's OWN published precondition — "This operation is supported for RDS
/// for Oracle and Microsoft SQL Server" — and has no variable part: the refusal states which
/// engines the operation serves, which is what tells a caller that no amount of waiting will
/// change the answer.
pub const ACTIVITY_STREAM_ENGINE_MESSAGE: &str =
    "This operation is supported for RDS for Oracle and Microsoft SQL Server.";

/// Whether a DB instance running `engine` has an audit policy this operation modifies.
///
/// The operation publishes the engines it serves — "This operation is supported for RDS for
/// Oracle and Microsoft SQL Server" — and its `ResourceArn` names "the RDS for Oracle or
/// Microsoft SQL Server DB instance", so the engine families are exactly those two, each
/// matched by the prefix every one of their engine names carries (`oracle-ee`,
/// `sqlserver-se`, …). The comparison is case-INSENSITIVE, the way every per-engine rule of
/// this service compares an engine name ([`switchover_is_supported_for`]), because `Engine`
/// is accepted case-insensitively on a create and stored as the request spelled it.
///
/// The RDS CUSTOM families (`custom-oracle-*`, `custom-sqlserver-*`) are deliberately NOT
/// among them: database activity streams are one of the features RDS Custom does not support
/// ("RDS Custom unsupported features", Amazon RDS User Guide), so an instance of one has no
/// activity stream and no audit policy for this call to change.
///
/// This is a different question from the one
/// `OrderableDBInstanceOption$SupportedActivityStreamModes` answers
/// ([`crate::ops::describe_orderable_db_instance_options`]), which is why the two rules do
/// not list the same engines: that member reports the MODES a stream can be STARTED in and
/// its own documentation names only Aurora PostgreSQL, Aurora MySQL and RDS for Oracle,
/// while the audit policy this operation writes is the one Amazon RDS keeps for the two
/// engines whose streams have an audit policy at all — "This operation is supported for RDS
/// for Oracle and Microsoft SQL Server". Each rule follows the published text of the member
/// or operation it belongs to.
pub fn engine_has_audit_policy(engine: &str) -> bool {
    let lowered = engine.to_ascii_lowercase();
    lowered.starts_with("oracle") || lowered.starts_with("sqlserver")
}

/// The audit policy state of one DB instance's database activity stream: the state it is in,
/// and the state a modification has asked for but the clock has not made due yet.
#[derive(Debug, Clone)]
pub struct ActivityStreamPolicy {
    /// The settled state: [`AUDIT_POLICY_STATE_LOCKED`] or [`AUDIT_POLICY_STATE_UNLOCKED`].
    state: String,
    /// Where the policy is headed and when it gets there, for a change still in flight;
    /// `None` for a policy that is where it was asked to be.
    pending: Option<PendingAuditPolicyState>,
}

/// A change of audit policy state this service owes the clock.
#[derive(Debug, Clone)]
struct PendingAuditPolicyState {
    /// The state the policy reaches when the change is due.
    state: String,
    /// When that is.
    due_at: Instant,
}

impl ActivityStreamPolicy {
    /// The policy of an instance nothing has modified: the documented default state, with
    /// nothing in flight.
    fn unmodified() -> Self {
        Self {
            state: DEFAULT_AUDIT_POLICY_STATE.to_string(),
            pending: None,
        }
    }

    /// The state the policy is in or is on its way to — what a request asking for the same
    /// state again is compared against.
    fn target(&self) -> &str {
        self.pending
            .as_ref()
            .map_or(self.state.as_str(), |pending| pending.state.as_str())
    }

    /// `ActivityStreamPolicyStatus` as it stands: the settled state, or the transitional
    /// member naming where a change in flight is going.
    pub fn status(&self) -> &str {
        match self.pending.as_ref() {
            Some(pending) if pending.state == AUDIT_POLICY_STATE_UNLOCKED => {
                ACTIVITY_STREAM_POLICY_UNLOCKING
            }
            Some(_) => ACTIVITY_STREAM_POLICY_LOCKING,
            None => self.state.as_str(),
        }
    }

    /// Ask the policy for `state`.
    ///
    /// A state the policy is already in, or already moving to, leaves it exactly as it is —
    /// the call is idempotent in the value — and any other starts the transition the clock
    /// then settles.
    fn request(&mut self, state: &str) {
        if self.target() == state {
            return;
        }
        self.pending = Some(PendingAuditPolicyState {
            state: state.to_string(),
            due_at: Instant::now() + STATUS_TRANSITION_DELAY,
        });
    }

    /// Apply a change of state whose due instant has passed; see
    /// [`DbInstanceRecord::settle`], which is what calls this.
    pub(super) fn settle(&mut self, now: Instant) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| now >= pending.due_at)
        {
            if let Some(pending) = self.pending.take() {
                self.state = pending.state;
            }
        }
    }
}

/// Why a modification of an activity stream's audit policy was refused, as the CLASS of
/// refusal rather than as one operation's error enum, so the handler lifts each into its own
/// modelled errors (`smithy_query(query="operation", operation="ModifyActivityStream")`).
#[derive(Debug, Clone)]
pub enum ActivityStreamRefusal {
    /// `ResourceArn` carries no Amazon RDS DB instance ARN at all — a value that is no ARN,
    /// or an ARN of another of this service's resource types. The value itself is unusable, so
    /// this is refused on the MEMBER that carried it rather than reported as a resource that
    /// might yet appear.
    UnusableArn(String),
    /// A DB instance ARN that names no DB instance this account holds in the request's
    /// region.
    NotFound(String),
    /// The instance is there, but is not one this operation can be performed on: its engine
    /// has no database activity stream, or it is not in a state a modification can be taken
    /// in.
    InvalidState(String),
}

/// What an accepted modification reports about the instance's activity stream: the status of
/// the stream itself, and the status of its audit policy.
#[derive(Debug, Clone)]
pub struct ActivityStreamReport {
    /// `ActivityStreamStatus` for the instance's stream, from the instance's own record —
    /// the same value `DBInstance$ActivityStreamStatus` reports.
    pub status: String,
    /// `ActivityStreamPolicyStatus` as the modification left it: the state asked for, or the
    /// transitional member while the change is in flight.
    pub policy_status: String,
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// Write the audit policy state of the activity stream of the DB instance `resource_arn`
    /// names in `region`, and report the stream and its policy as they now stand.
    ///
    /// Resolution is by the ARN each stored instance RENDERS ([`DbInstanceRecord::arn`]), so
    /// an ARN of another account, partition or region resolves to nothing here: RDS is
    /// regional, and the only instance this request can act on is one whose own reports carry
    /// the ARN it named.
    pub fn set_activity_stream_audit_policy(
        &mut self,
        region: &str,
        resource_arn: &str,
        audit_policy_state: Option<&str>,
    ) -> Result<ActivityStreamReport, ActivityStreamRefusal> {
        // `ResourceArn` names "the RDS for Oracle or Microsoft SQL Server DB INSTANCE", so a
        // value that is no DB instance ARN of this service — one that is no ARN, or an ARN
        // of another of its resource types — names no DB instance for this call to resolve
        // at all, and is refused on the member that carried it as a resource this service
        // does not know.
        let addressed = parse_rds_resource_arn(resource_arn).filter(|parsed| {
            tags::TaggedKind::from_arn_resource_type(parsed.resource_type)
                == Some(tags::TaggedKind::DbInstance)
        });
        let Some(parsed) = addressed else {
            return Err(ActivityStreamRefusal::UnusableArn(
                tags::invalid_resource_name_message(resource_arn),
            ));
        };

        // The DB instance the ARN names is its resource-name segment — the identifier —
        // which is also the handle the refusal names the instance by.
        let named = parsed.identifier.to_string();
        // The stored key of the instance the ARN names, folded the way every DB instance key
        // is ("stored as a lowercase string").
        let key = (region.to_string(), named.to_ascii_lowercase());
        // The wording this front door answers a `DBInstanceNotFoundFault` with on a call that
        // ACTS on the instance: the fixed phrase, then the instance the request named.
        let not_found =
            || ActivityStreamRefusal::NotFound(format!("DBInstance not found: {named}"));
        let Some(record) = self.db_instances.get_mut(&key) else {
            return Err(not_found());
        };
        // RDS is regional, and an account acts on its own resources: the only instance this
        // request can act on is one whose own reports carry the ARN it named, so an ARN of
        // another account, partition or region resolves to nothing here.
        if record.arn() != resource_arn {
            return Err(not_found());
        }
        // "This operation is supported for RDS for Oracle and Microsoft SQL Server": an
        // instance of any other engine has no audit policy for this call to change, which no
        // waiting will alter.
        if !engine_has_audit_policy(&record.engine) {
            return Err(ActivityStreamRefusal::InvalidState(
                ACTIVITY_STREAM_ENGINE_MESSAGE.to_string(),
            ));
        }
        // A modification of an instance's configuration needs the same available instance
        // every other modification of one does, and is refused with the sentence RDS words
        // for a modification an instance's status is not in a state for.
        if record.status != DB_INSTANCE_STATUS_AVAILABLE {
            return Err(ActivityStreamRefusal::InvalidState(
                modify_db_instance_invalid_state_message(&record.status),
            ));
        }

        // "to either locked (default) or unlocked": the state the request named, else the
        // default the operation documents for a call that names none.
        let requested = audit_policy_state.unwrap_or(DEFAULT_AUDIT_POLICY_STATE);
        let policy = record
            .activity_stream_policy
            .get_or_insert_with(ActivityStreamPolicy::unmodified);
        policy.request(requested);
        let policy_status = policy.status().to_string();
        // The instance's own log records what this service does to it, and a change of its
        // activity stream's audit policy is one of those things.
        record
            .engine_log
            .record(format!("database activity stream audit policy {policy_status}"));
        Ok(ActivityStreamReport {
            // The stream's own status, as the instance reports it
            // (`DBInstance$ActivityStreamStatus`): changing the audit policy neither starts
            // nor stops the stream — "restarting the activity stream isn't required".
            status: record.activity_stream_status().to_string(),
            policy_status,
        })
    }
}
