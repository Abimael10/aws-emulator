// SPDX-License-Identifier: CC0-1.0

//! WHO a request speaks for, and whether the policies in force let it act on what it
//! addresses — one table, or the account's whole table namespace.
//!
//! A table's resource-based policy is evaluated on every call that reads or writes an item
//! of that table, at the instant of the call — nothing is cached, so a policy just put
//! refuses immediately and one just deleted stops refusing immediately.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the SAME
//!    place `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the
//!    one the caller reads back from STS for the credentials it signed with. Resolving it
//!    materialises the session expiries the clock has already made due, a write owed at
//!    that instant whether or not anybody called.
//! 2. **what the TABLE's own document says** — the revision of [`super::AttachedPolicy`]
//!    in force at the instant of the call ([`super::ResourcePolicySlot::visible`]): a
//!    `Deny` about the request refuses it whatever any grant says, and an `Allow` that
//!    names the caller itself grants it.
//! 3. **what the CALLER is allowed to do at all** — the identity-based documents the IAM
//!    store holds for that principal. Both halves are judged by
//!    [`::service_common::iam_authorization`], the tree's one evaluation of policy
//!    documents against `(action, resource, principal, context)`, so this service and
//!    every other one handed the same documents decide alike — a resource policy's
//!    statement `Condition` included.
//! 4. **what the CREDENTIAL itself was scoped to** — the session policies the mint of the
//!    credential passed, which STS holds on the session the access key id names
//!    ([`sts_store::session_scoping_documents`]) and which are read by the one reading of a
//!    resolved scope ([`sts_store::ScopingPolicies::decision`]). They SCOPE the permissions
//!    of the
//!    identity down and grant nothing of their own: "the resulting session's permissions
//!    are the intersection of the role's identity-based policies and the session policies"
//!    (`AssumeRoleRequest$Policy`). A long-term access key is scoped by none.
//!
//! ## The rule
//!
//! In IAM's own order: an explicit `Deny` about the request — in the table's policy, in an
//! identity-based document or in a session policy — refuses, whatever any grant says.
//! Otherwise the request needs a grant: an `Allow` about it in the caller's identity-based
//! policies, or one in the table's policy that names the CALLER. With neither the request is
//! refused, which is IAM's implicit deny, and the operation has no effect. A grant still has
//! to pass the scope the credential was minted with: a request the session policies do not
//! allow is refused even where the documents in force on the identity allow it, and one only
//! they allow is refused as if they had allowed nothing — each refusal naming the document
//! that withheld the grant, because they need different fixes.
//!
//! A call that addresses NO single table — a listing of the account's table resources, or a
//! report of the account's own quotas — acts on the table namespace the account's tables live
//! in ([`account_resource`]) instead. No table is addressed, so no resource-based policy is
//! in force and the same rule is decided on the identity-based documents and the credential's
//! scope alone ([`account_denial`]): IAM's implicit deny still refuses a principal that
//! nothing grants the action to.
//!
//! A call this service makes to ANOTHER service's resource FOR the caller — the object write
//! a table export performs, the object read a table import performs — is made with the
//! CALLER's permissions, so it is decided by the same rule over that service's own action and
//! ARN, against the caller's identity-based documents and the resource policy that resource
//! carries ([`performed_for_caller_denial`]).
//!
//! A statement that names the ACCOUNT rather than the caller (the account's root ARN, or
//! the bare account id that stands for it) delegates to the account rather than granting:
//! "you must also attach an identity-based policy" to the principal. So the account
//! spellings decide a `Deny` — which covers every principal of the account — and never
//! make a grant on their own.
//!
//! ## The two callers that are not IAM principals
//!
//! * **The account's own published credential**, which is the position AWS account root
//!   credentials are in: the IAM store holds no policies for it and the table's owner has
//!   full control of its tables, so such a request is allowed — while an explicit `Deny`
//!   in the table's policy still refuses it.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed
//!   policy is owned by AWS, not by the account, so the account's store holds no document
//!   for it; refusing on evidence this emulator does not have would deny access AWS
//!   permits, so such a principal is allowed where the readable documents neither grant
//!   nor deny. A credential scoped by a MANAGED session policy whose document is likewise
//!   unheld is in that same position: the scope it was minted with cannot withhold a grant
//!   this emulator cannot read, while a `Deny` a document it CAN read still refuses.
//!
//! An expired session credential speaks for nobody: no statement names it, so every action
//! it asks for is refused.
//!
//! ## What the request itself contributes to the decision
//!
//! A statement may be CONDITIONED, and a condition is decided against the keys the request
//! carries values for. A data-plane call of this service ANSWERS for the keys below — it
//! publishes each of them whether or not it carries a value, so a clause over one is
//! decided by IAM's own rules for a key the request carries no value for (`ForAllValues:`
//! holds over the empty set, `ForAnyValue:` does not, and an unqualified operator holds
//! only where it is negated or spelled `…IfExists`) rather than being left undecided:
//!
//! * the partition this call addresses — [`AddressedKeys`], the value(s) of the table's
//!   `HASH` key attribute the `Key`, the `Item`, the key condition or the statement names,
//!   one per partition the call touches. A call that addresses no partition of its own (a
//!   table or index walk) carries NO value for the key, which it answers for: that is IAM's
//!   empty set and not an unknown. Only a call whose addressed partition cannot be read at
//!   all ([`AddressedKeys::unknown`]) answers for nothing, because the value a clause would
//!   be decided against is not established.
//! * the attributes this call names — [`RequestKeys`], the top-level attribute names of the
//!   projection a read asks for or of the item a write stores, which is what a fine-grained
//!   statement admits a caller to some of an item's attributes with. A call that names none
//!   carries no value for the key; a call whose named set this cannot ENUMERATE — the
//!   attributes an update expression or a PartiQL statement writes — answers for the key
//!   with nothing at all, so a clause over it stays undecided rather than holding over an
//!   empty set the request did not have.
//! * the selection a read asks for — the `Select` a `Query` or `Scan` states or implies,
//!   and the one a key-addressed read implies: the attributes it listed
//!   (`SPECIFIC_ATTRIBUTES`) or every attribute of the item (`ALL_ATTRIBUTES`). A call that
//!   selects nothing — every write — carries no value for the key.
//! * the operation this call's action is performed INSIDE — [`EnclosingOperation`]: the
//!   multi-item operation one of whose entries asked for the action, which is what a
//!   statement that admits an action only as part of a transaction tests. A call that
//!   performs an action of its OWN carries no value for the key, which it answers for, so a
//!   grant conditioned on an enclosing operation does not reach it.
//! * the session tags the calling principal carries, which is where a policy written with
//!   `${aws:PrincipalTag/<key>}` takes the text it stands for. They are known for a
//!   session this emulator's STS registry minted and for nobody else, so only such a
//!   caller declares that an absent tag key is a tag it does not carry.
//! * the federated identity the calling credential was minted by exchanging, which a
//!   session of a role assumed with a web identity token carries: the
//!   `<provider>:<claim>` keys that token publishes, among them the subject it names. They
//!   are what a policy written with `${<provider>:<claim>}` takes the text it stands for,
//!   so a statement conditioned on the partitions a call addresses can pin them to the
//!   caller's OWN federated subject.
//!
//! Every one of them is passed through [`::service_common::iam_authorization::Request`] — for
//! the table's own document as much as for the identity's — so the clause is decided by the
//! tree's one evaluation of policy documents rather than here. A fine-grained statement
//! over the partitions a call addresses therefore decides that call: it applies to the
//! partitions its condition selects and to no others.

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use super::*;

/// The condition key whose value is the partition a data-plane call addresses: the first
/// key attribute of the table, which is what a fine-grained identity policy scopes a
/// caller to one partition with.
const LEADING_KEYS_CONDITION_KEY: &str = "dynamodb:LeadingKeys";

/// The condition-key FAMILY the calling principal's session tags are named in, each member
/// after the tag key it carries: `aws:PrincipalTag/tenant` holds the value of the session
/// tag `tenant`.
const PRINCIPAL_TAG_CONDITION_KEY_FAMILY: &str = "aws:PrincipalTag/";

/// The condition key whose values are the attributes a data-plane call names: one entry per
/// top-level attribute name the call's projection or its item carries, which is what a
/// fine-grained statement scopes a caller to some of an item's attributes with.
const ATTRIBUTES_CONDITION_KEY: &str = "dynamodb:Attributes";

/// The condition key whose value is WHICH attributes a read asks for, as the `Select` member
/// of a read names them ([`Selection`]).
const SELECT_CONDITION_KEY: &str = "dynamodb:Select";

/// The condition key whose value is the operation an action is performed INSIDE, named by
/// [`EnclosingOperation`] — what a statement admitting an action only as one entry of a
/// transaction, or only outside one, tests.
const ENCLOSING_OPERATION_CONDITION_KEY: &str = "dynamodb:EnclosingOperation";

/// An operation that performs the actions ITS OWN ENTRIES ask for, as the condition key
/// reporting it holds that operation's name.
///
/// Such an operation has no IAM action of its own: each entry of it is authorized as the
/// action that entry performs — a statement of a PartiQL transaction or batch as that
/// statement's own PartiQL action, an item of a transactional read or write as the
/// single-item action it is the transactional form of — so the only thing that tells those
/// calls apart from the single-entry operation performing the same action is this key. A
/// call that performs an action of its own (every single-item operation, `BatchGetItem` and
/// `BatchWriteItem`, each of which is authorized as ITSELF) encloses nothing and carries no
/// value for the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnclosingOperation {
    /// A PartiQL statement run as one statement of a transaction.
    ExecuteTransaction,
    /// A PartiQL statement run as one statement of a batch.
    BatchExecuteStatement,
    /// An item read as one item of a transactional read.
    TransactGetItems,
    /// An item written, or checked, as one item of a transactional write.
    TransactWriteItems,
}

impl EnclosingOperation {
    /// The operation's name, which is the text the condition key holds for a call it
    /// encloses.
    #[must_use]
    fn name(self) -> &'static str {
        match self {
            Self::ExecuteTransaction => "ExecuteTransaction",
            Self::BatchExecuteStatement => "BatchExecuteStatement",
            Self::TransactGetItems => "TransactGetItems",
            Self::TransactWriteItems => "TransactWriteItems",
        }
    }
}

/// The principal a request speaks for, and everything the decision is made from.
///
/// Resolved once per request, BEFORE this service's own store is locked, so the STS and
/// IAM stores are never reached while this service's state guard is held.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    pub arn: String,
    /// Every spelling of the principal a policy may name IT by: the ARN STS reports, and
    /// the ARN of the IAM role behind an assumed-role session. These decide a GRANT.
    pub principals: Vec<String>,
    /// The spellings that name the ACCOUNT the principal belongs to rather than the
    /// principal: they decide a deny and never grant (see the module docs).
    pub account_principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    pub documents: Vec<String>,
    /// The session policy documents the mint of this credential scoped it with, as they
    /// read at the instant of this request, and whether a managed one among them names a
    /// document the IAM store does not hold (see the module docs). Empty for a credential
    /// no mint scoped.
    pub scope: sts_store::ScopingPolicies,
    /// Set when this request presents the account's own published credential, which holds
    /// no IAM policies (see the module docs).
    pub account_credential: bool,
    /// Set when the principal carries a policy whose document the IAM store does not hold
    /// — an AWS-managed policy (see the module docs).
    pub unreadable_attachment: bool,
    /// The session tags the principal carries, as STS attached them when the session was
    /// minted: the values `aws:PrincipalTag/<key>` holds for a request this credential
    /// signs.
    pub tags: Vec<(String, String)>,
    /// Set when the principal's session tags are KNOWN — a session this emulator's STS
    /// registry holds — which is what lets a clause over a tag key the principal does NOT
    /// carry be decided rather than left undecided. A long-term credential's tags live in
    /// IAM and are not read here, so such a caller declares nothing about them.
    pub tags_known: bool,
    /// The condition keys the web identity this credential was minted by exchanging
    /// publishes, as STS recorded them on the session: the values a
    /// `<provider>:<claim>` clause, and a policy variable naming one, hold for a request
    /// this credential signs. Empty for a credential no such exchange minted.
    pub federated_claims: Vec<(String, String)>,
    /// The UNIQUE IDS the identities behind this caller's spellings carry now, as IAM holds
    /// them. A resource-based policy holds an identity principal by its unique id rather than
    /// by the ARN its document spells it with (see [`super::policy_principals`]), so these
    /// are what decide which of that document's principals name THIS caller
    /// ([`AttachedPolicy::spellings_decided`]). Empty for a caller no identity of this
    /// account stands behind.
    pub unique_ids: Vec<String>,
}

impl Caller {
    /// Every spelling a DENY is decided against: the principal's own and its account's,
    /// because a statement denying the account denies each of its principals.
    fn denied_spellings(&self) -> Vec<String> {
        let mut named = self.principals.clone();
        named.extend(self.account_principals.iter().cloned());
        named
    }
}

/// Who this request speaks for, resolved from the access key id it signed with.
///
/// Call it before locking this service's store (see [`Caller`]).
pub fn request_caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let iam = sts_store::iam_state(engine);
    // A long-term access key speaks for the IAM user that holds it, which is the identity
    // STS itself reports for it; a key no store holds is the account's own published
    // credential.
    let long_term = match &iam {
        Some(iam) => sts_store::long_term_identity(Some(&iam.lock()), &access_key_id),
        None => sts_store::ambient_identity(),
    };
    let resolved = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            long_term.clone(),
        )
        .map(|caller| {
            (
                caller.identity.arn.clone(),
                caller.identity.account.clone(),
                caller.role_arn.clone(),
                // WHOSE identity-based documents are in force for this credential, which is
                // not always the principal it speaks for: STS's one answer for it
                // (`sts_store::Caller::identity_policies_arn`).
                caller.identity_policies_arn().to_string(),
                // The tags are this registry's record of the session, so they are known
                // exactly for a credential it minted.
                caller.kind.is_some(),
                caller.tags.clone(),
                caller.session_policies.clone(),
                caller.federated_claims.clone(),
            )
        }),
        // No STS store on this engine: the credential names no session, so it speaks for
        // the long-term identity it was resolved to, and is judged on that identity's
        // documents.
        None => Ok((
            long_term.arn.clone(),
            long_term.account.clone(),
            None,
            long_term.arn.clone(),
            false,
            Vec::new(),
            sts_store::SessionPolicies::default(),
            Vec::new(),
        )),
    };
    // An expired session credential speaks for nobody: it carries no documents and no
    // principal spelling. The refusal still names what was presented, so a caller reading
    // it can tell an expired credential from an unauthorized one.
    let Ok((
        arn,
        account,
        role_arn,
        policies_arn,
        tags_known,
        tags,
        session_policies,
        federated_claims,
    )) = resolved
    else {
        return Caller {
            arn: format!("an expired session credential ({access_key_id})"),
            ..Caller::default()
        };
    };
    let mut caller = Caller {
        account_credential: arn == sts_store::ambient_identity().arn,
        principals: vec![arn.clone()],
        account_principals: iam::account_principals(&arn),
        arn,
        tags,
        tags_known,
        federated_claims,
        ..Caller::default()
    };
    // An assumed-role session is also named by the ARN of the role it is a session of,
    // which is the spelling a policy uses to name the role itself, and the identity whose
    // policies are in force for it.
    if let Some(role_arn) = &role_arn {
        if !caller.principals.contains(role_arn) {
            caller.principals.push(role_arn.clone());
        }
    }
    if caller.account_principals.is_empty() {
        caller.account_principals = vec![account_root_arn(&account), account];
    }
    // The IAM store of the same engine, read ONCE for both halves of what the caller may do:
    // the documents in force on its identity, and the documents its credential was scoped
    // with. An engine carrying no IAM store holds neither. The documents are the ones the
    // IDENTITY behind the credential carries (`policies_arn`), which for a federated-user
    // session is the IAM user the mint stood on rather than the federated principal, who
    // carries none of its own.
    let held = iam.as_ref().map(|iam| iam.lock());
    if let Some(iam) = held.as_deref() {
        if let Some(policies) = iam.principal_policies(&policies_arn) {
            caller.documents = policies.documents;
            caller.unreadable_attachment = !policies.uncarried.is_empty();
        }
        // WHICH identity of the account each spelling names, read off the same store while
        // it is open: a resource-based policy holds an identity principal by that identity's
        // unique id, so this is what decides which of its principals name this caller.
        caller.unique_ids = caller
            .principals
            .iter()
            .filter_map(|spelling| identity_unique_id(iam, spelling).cloned())
            .collect();
    }
    // What the credential itself was scoped to, read at the instant of the request so a
    // managed scoping policy edited since the mint scopes by what it says NOW; the inline
    // document the mint passed scopes it whether or not a store is there to read.
    let scoping = sts_store::session_scoping_documents(held.as_deref(), &session_policies);
    caller.scope = scoping;
    caller
}

/// The ARN of an account's root user, the spelling that stands for the whole account in a
/// policy's `Principal` element.
fn account_root_arn(account: &str) -> String {
    format!("arn:aws:iam::{account}:root")
}

/// The partitions a data-plane call addresses, as the condition key that reports them
/// holds their values: one entry per partition the `Key`, the `Item`, the key condition or
/// the PartiQL statement names, in the order the request named them.
///
/// A call that addresses no partition of its own — a table or index walk
/// ([`Self::whole_table`]) — carries NO value for the key and answers for it: that is
/// IAM's empty set, which a `ForAllValues:` clause holds over and a `ForAnyValue:` one does
/// not. A call whose addressed partition cannot be read at all ([`Self::unknown`]: a key map
/// without the table's own `HASH` attribute, a value of another type than the one the table
/// declared for it) answers for nothing, because the value the clause would be decided
/// against is not established — and such a request is refused for what is wrong with it
/// once the decision is past.
#[derive(Debug, Clone, Default)]
pub struct AddressedKeys {
    /// The partitions the call addresses, empty for a call that addresses none of its own;
    /// `None` when they cannot be read at all (see the type docs).
    values: Option<Vec<String>>,
}

impl AddressedKeys {
    /// A call that addresses no partition of its own: a table or index walk, which carries
    /// no value for the key and answers that it carries none.
    #[must_use]
    pub fn whole_table() -> Self {
        Self {
            values: Some(Vec::new()),
        }
    }

    /// A call whose addressed partitions cannot be read at all (see the type docs).
    #[must_use]
    pub fn unknown() -> Self {
        Self { values: None }
    }

    /// The partitions the key or item maps `maps` address, read against `table`'s own
    /// partition-key attribute.
    #[must_use]
    pub fn of_maps<'a, I: IntoIterator<Item = &'a Item>>(table: &Table, maps: I) -> Self {
        let partition = table.partition_key_name();
        Self::of_values(maps.into_iter().map(|map| map.get(partition)))
    }

    /// The partitions the attribute values `values` address: a `Query`'s partition-key
    /// equality operand, or the value a key map carries for the partition attribute. A
    /// value that is absent, or carries no text a condition key can hold, leaves the
    /// addressed partitions unknown (see the type docs).
    #[must_use]
    pub fn of_values<'a, I: IntoIterator<Item = Option<&'a AttributeValue>>>(values: I) -> Self {
        let mut addressed = Vec::new();
        for value in values {
            match value.and_then(condition_key_text) {
                Some(text) => addressed.push(text),
                None => return Self::unknown(),
            }
        }
        // A call that named no partition at all addresses the whole table, whatever the
        // member it would have named one in.
        Self {
            values: Some(addressed),
        }
    }

    /// The partition one PartiQL statement addresses: the item an `INSERT` adds, or the
    /// partition-key equality the `WHERE` clause of a `SELECT`, `UPDATE` or `DELETE` pins.
    /// A `SELECT` that pins none walks the table.
    #[must_use]
    pub fn of_statement(table: &Table, statement: &PartiqlStatement) -> Self {
        let pinned = |predicate: &Predicate| {
            Self::of_values([pinned_equalities(predicate).get(table.partition_key_name())])
        };
        match statement {
            PartiqlStatement::Insert(insert) => Self::of_maps(table, [&insert.item]),
            // A `SELECT` whose `WHERE` clause pins no partition of its own walks the
            // table, exactly as one carrying no clause at all does.
            PartiqlStatement::Select(select) => match &select.filter {
                Some(filter) => {
                    let equalities = pinned_equalities(filter);
                    match equalities.get(table.partition_key_name()) {
                        Some(value) => Self::of_values([Some(value)]),
                        None => Self::whole_table(),
                    }
                }
                None => Self::whole_table(),
            },
            // A check reads the ONE item its inner `SELECT` addresses, so the partition
            // that clause pins is the partition it is about.
            PartiqlStatement::Exists(select) => match &select.filter {
                Some(filter) => pinned(filter),
                None => Self::whole_table(),
            },
            PartiqlStatement::Update(update) => pinned(&update.filter),
            PartiqlStatement::Delete(delete) => pinned(&delete.filter),
        }
    }
}

/// The text a condition key holds for one partition-key value: the string itself for an
/// `S` value, the number in the canonical form this store holds it in for an `N` one (so
/// two spellings of one figure address one partition and decide alike), and the base64 the
/// wire carries for a `B` one — the three types a key attribute may be declared as.
/// Anything else is no key value at all, so it names no text.
///
/// This is the store's ONE rendering of a key attribute's value as the text something outside
/// the item addresses it by, so the partition a policy clause is decided against and the
/// contributor a key activity is attributed to ([`super::contributor_insights`]) are the same
/// string for the same value.
pub(crate) fn condition_key_text(value: &AttributeValue) -> Option<String> {
    match value {
        AttributeValue::S(text) => Some(text.clone()),
        AttributeValue::N(text) => canonical_number_text(text).ok(),
        AttributeValue::B(bytes) => Some(::aws_smithy_types::base64::encode(bytes)),
        _ => None,
    }
}

/// The condition keys a data-plane call of this service answers for, with the values it
/// carries for each: the partitions it addresses, the attributes it names, and the
/// selection a read asks for.
///
/// A key is published only where this call can answer for it COMPLETELY (see the module
/// docs): an empty value list means "the request names none", so a call that names
/// attributes this does not enumerate must answer for that key with nothing at all rather
/// than with the empty set, exactly as [`AddressedKeys::unknown`] does for the partitions.
#[derive(Debug, Clone)]
pub struct RequestKeys {
    /// The partitions the call addresses.
    addressed: AddressedKeys,
    /// The top-level attribute names the call names, in name order — empty for a call that
    /// names none — or `None` where the call cannot answer for the whole set it names, or
    /// names more of them than [`MAX_PUBLISHED_ATTRIBUTES`] (see the type docs).
    attributes: Option<Vec<String>>,
    /// The selection a read asks for, or `None` for a call that selects nothing. Every
    /// data-plane call answers for this key: only a `Query` or a `Scan` carries the member,
    /// and a read addressed by key implies one, so a call with neither really carries no
    /// value for it.
    select: Option<&'static str>,
    /// The operation this call's action is performed inside, or `None` for a call that
    /// performs an action of its own. Every data-plane call answers for this key: it either
    /// is one entry of such an operation or it is not ([`EnclosingOperation`]).
    enclosing: Option<EnclosingOperation>,
}

impl RequestKeys {
    /// A call that answers for the partitions it addresses alone: it selects nothing, and
    /// the attributes it names are not enumerated here, so a clause over them stays
    /// undecided rather than being read as naming none.
    #[must_use]
    pub fn addressing(addressed: AddressedKeys) -> Self {
        Self {
            addressed,
            attributes: None,
            select: None,
            enclosing: None,
        }
    }

    /// The same keys, answered for a call performed as ONE ENTRY of `enclosing` rather than
    /// as an operation of its own: the condition key reporting the enclosing operation then
    /// holds that operation's name ([`EnclosingOperation`]).
    #[must_use]
    pub fn within(mut self, enclosing: EnclosingOperation) -> Self {
        self.enclosing = Some(enclosing);
        self
    }

    /// A CONTROL-PLANE call on the table: it addresses no partition, names no attribute of
    /// an item and asks for no selection, and it performs an action of its own rather than
    /// one an enclosing operation asked for, so it carries NO value for any of the
    /// data-plane keys and answers that it carries none — IAM's empty set. A statement
    /// conditioned on one of those keys therefore does not apply to such a call, whichever
    /// effect it states, which is what AWS decides for a request whose context carries no
    /// value for the key a clause names.
    #[must_use]
    pub fn control_plane() -> Self {
        Self {
            addressed: AddressedKeys::whole_table(),
            attributes: Some(Vec::new()),
            select: None,
            enclosing: None,
        }
    }

    /// A read addressed by key, which has no `Select` member and implies one: the
    /// attributes its projection listed, or every attribute of the item when it listed
    /// none ("If no attribute names are specified, then all attributes are returned",
    /// `GetItem$ProjectionExpression`).
    #[must_use]
    pub fn projected(addressed: AddressedKeys, projection: Option<&[DocumentPath]>) -> Self {
        Self {
            addressed,
            attributes: named_attributes(projection.unwrap_or_default()),
            select: Some(match projection {
                Some(_) => SPECIFIC_ATTRIBUTES_SELECT,
                None => ALL_ATTRIBUTES_SELECT,
            }),
            enclosing: None,
        }
    }

    /// A read that states its own selection: the `Select` a `Query` or `Scan` carried, or
    /// the one its projection and the schema it reads imply
    /// ([`super::resolve_selection`]).
    #[must_use]
    pub fn selected(addressed: AddressedKeys, selection: &Selection) -> Self {
        let (select, paths): (&'static str, &[DocumentPath]) = match selection {
            Selection::Specific(paths) => (SPECIFIC_ATTRIBUTES_SELECT, paths),
            Selection::AllAttributes => (ALL_ATTRIBUTES_SELECT, &[]),
            Selection::AllProjected => (ALL_PROJECTED_ATTRIBUTES_SELECT, &[]),
            Selection::Count => (COUNT_SELECT, &[]),
        };
        Self {
            addressed,
            attributes: named_attributes(paths),
            select: Some(select),
            enclosing: None,
        }
    }

    /// A write that stores `item`, which names every attribute it carries and selects
    /// nothing.
    #[must_use]
    pub fn storing(addressed: AddressedKeys, item: &Item) -> Self {
        Self::storing_maps(addressed, [item])
    }

    /// A write whose named attributes are those of the item or key maps `maps` — the puts
    /// and deletes one batch sends to one table, each naming the attributes of the item it
    /// stores or of the key it removes, and nothing else.
    #[must_use]
    pub fn storing_maps<'a, I: IntoIterator<Item = &'a Item>>(
        addressed: AddressedKeys,
        maps: I,
    ) -> Self {
        Self {
            addressed,
            attributes: published_attributes(
                maps.into_iter()
                    .flat_map(|map| map.keys().map(String::as_str)),
            ),
            select: None,
            enclosing: None,
        }
    }
}

/// The attributes a projection names, as the condition key that reports them holds their
/// values: the TOP-LEVEL attribute name each document path starts at — a path that reaches
/// inside an attribute names that attribute.
fn named_attributes(paths: &[DocumentPath]) -> Option<Vec<String>> {
    published_attributes(paths.iter().map(DocumentPath::attribute))
}

/// The attribute names a call names, as the condition key holds them: each name once, in
/// name order, or `None` once the call names more than [`MAX_PUBLISHED_ATTRIBUTES`] of them
/// — a set that large is one no statement enumerates, so the key is then answered with
/// nothing at all rather than with a value list whose size the caller chooses (which is the
/// "cannot enumerate" case [`RequestKeys`] already models).
///
/// The names are deduplicated through an ordered SET and the walk stops at the ceiling, so
/// the work one call asks for here is linear in what it sent and bounded per name by this
/// service's own constant rather than quadratic in the attribute count.
fn published_attributes<'a, I: IntoIterator<Item = &'a str>>(names: I) -> Option<Vec<String>> {
    let mut named: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();
    for name in names {
        named.insert(name);
        if named.len() > MAX_PUBLISHED_ATTRIBUTES {
            return None;
        }
    }
    Some(named.into_iter().map(str::to_string).collect())
}

/// The most attribute names one call publishes for the condition key that reports them: the
/// ceiling a PROJECTION is already held to ([`super::MAX_PROJECTED_PATHS`]), so the item a
/// write stores is read against the same bound as the paths a read lists.
///
/// A ceiling of this emulator's own rather than one AWS states, so that the context one
/// request builds cannot grow with an attribute count the caller chooses; past it the key is
/// answered with nothing, which leaves a clause over it undecided and so refuses the call
/// where AWS, which publishes every name, might decide it either way.
const MAX_PUBLISHED_ATTRIBUTES: usize = super::MAX_PROJECTED_PATHS;

/// The `Select` a read of the attributes it listed asks for.
const SPECIFIC_ATTRIBUTES_SELECT: &str = "SPECIFIC_ATTRIBUTES";

/// The `Select` a read of every attribute of the item asks for.
const ALL_ATTRIBUTES_SELECT: &str = "ALL_ATTRIBUTES";

/// The `Select` a read of everything an index projects asks for.
const ALL_PROJECTED_ATTRIBUTES_SELECT: &str = "ALL_PROJECTED_ATTRIBUTES";

/// The `Select` a read that reports only how many items matched asks for.
const COUNT_SELECT: &str = "COUNT";

/// The condition keys this request carries values for, as `(key, value)` pairs: one entry
/// per partition it addresses, one per attribute it names, one for the selection it asks
/// for, one per session tag the calling principal carries, and one per claim of the web
/// identity its credential was minted by exchanging.
fn condition_context(caller: &Caller, keys: &RequestKeys) -> Vec<(String, String)> {
    let mut context: Vec<(String, String)> = keys
        .addressed
        .values
        .iter()
        .flatten()
        .map(|value| (LEADING_KEYS_CONDITION_KEY.to_string(), value.clone()))
        .collect();
    context.extend(
        keys.attributes
            .iter()
            .flatten()
            .map(|name| (ATTRIBUTES_CONDITION_KEY.to_string(), name.clone())),
    );
    if let Some(select) = keys.select {
        context.push((SELECT_CONDITION_KEY.to_string(), select.to_string()));
    }
    if let Some(enclosing) = keys.enclosing {
        context.push((
            ENCLOSING_OPERATION_CONDITION_KEY.to_string(),
            enclosing.name().to_string(),
        ));
    }
    context.extend(principal_context(caller));
    context
}

/// The condition keys the CALLING PRINCIPAL carries values for, whatever the call asks for:
/// one entry per session tag it carries, and one per claim of the web identity its
/// credential was minted by exchanging.
///
/// Held apart from [`condition_context`] because they are a property of the CREDENTIAL
/// rather than of the request, so a call that addresses no table of its own
/// ([`account_denial`]) answers for exactly these.
fn principal_context(caller: &Caller) -> Vec<(String, String)> {
    let mut context: Vec<(String, String)> = caller
        .tags
        .iter()
        .map(|(key, value)| {
            (
                format!("{PRINCIPAL_TAG_CONDITION_KEY_FAMILY}{key}"),
                value.clone(),
            )
        })
        .collect();
    context.extend(caller.federated_claims.iter().cloned());
    context
}

/// The keys [`principal_context`] answers for whether or not it carries a value for each —
/// the caller's own half of [`published_condition_keys`].
fn principal_published_keys(caller: &Caller) -> Vec<&str> {
    let mut published = Vec::new();
    if caller.tags_known {
        published.push(PRINCIPAL_TAG_CONDITION_KEY_FAMILY);
    }
    published.extend(sts_store::federated_claim_keys(&caller.federated_claims));
    published
}

/// The condition keys this request MODELS, whether or not it carries a value for each: the
/// keys whose absence from the context means the request carries no value for them rather
/// than that this emulator cannot answer for them.
///
/// A data-plane call answers for the selection it asks for however few values it carries
/// for it, and for the operation its action is performed inside whether or not it is
/// performed inside one ([`EnclosingOperation`]), for every partition it addresses unless
/// they cannot be read at all
/// ([`AddressedKeys::unknown`]), and for the attributes it names only where the whole set it
/// names is enumerated ([`RequestKeys`]) — a call that names attributes this cannot
/// enumerate publishes nothing for that key, so a clause over it stays undecided instead of
/// holding over an empty set the request did not really have. A caller whose session tags
/// are known knows every tag it carries — so the whole tag FAMILY is published, which is
/// what lets a clause over a tag key the principal does not carry be decided. A federated
/// credential answers for each key its own exchange published, so those are published one
/// by one ([`sts_store::federated_claim_keys`]).
fn published_condition_keys<'a>(caller: &'a Caller, keys: &RequestKeys) -> Vec<&'a str> {
    let mut published = vec![SELECT_CONDITION_KEY, ENCLOSING_OPERATION_CONDITION_KEY];
    if keys.attributes.is_some() {
        published.push(ATTRIBUTES_CONDITION_KEY);
    }
    if keys.addressed.values.is_some() {
        published.push(LEADING_KEYS_CONDITION_KEY);
    }
    published.extend(principal_published_keys(caller));
    published
}

/// The sentence AWS refuses an unauthorized request with: who was refused, which action on
/// which resource, and the reason the refusal rests on.
///
/// ONE statement of that wording for this crate, so the data-plane's policy decision
/// ([`denial_addressing`]) and every other refusal naming an action the caller may not
/// perform cannot word it differently.
pub fn not_authorized(caller_arn: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {caller_arn} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// The `AccessDeniedException` message a request that addresses no partition of its own
/// earns when the policies in force do not permit it, or `None` when they do. See
/// [`denial_keys`], which this is the table-wide case of.
pub fn denial(caller: &Caller, table: &Table, action: &str, now: SystemTime) -> Option<String> {
    denial_addressing(caller, table, action, now, &AddressedKeys::whole_table())
}

/// The resource a call that addresses no single table of its own acts on: the account's
/// whole table namespace in the Region the request reached
/// (`arn:aws:dynamodb:<region>:<account>:table/*`), or the table a listing NARROWS itself to
/// when the request names one.
///
/// A listing of this account's table resources is authorized against the namespace that
/// holds them, in the form [`super::table_arn`] mints every table ARN of this service in, so
/// a statement naming `table/*` — or one naming the one table a listing filters to — decides
/// it.
pub fn account_resource(region: &str, table: Option<&str>) -> String {
    super::table_arn(region, table.unwrap_or(ACCOUNT_TABLES_WILDCARD))
}

/// The table-name position of [`account_resource`] for a call that narrows itself to no
/// table: the namespace wildcard, which is what stands for "every table of this account in
/// this Region".
const ACCOUNT_TABLES_WILDCARD: &str = "*";

/// The `AccessDeniedException` message a request that addresses the account's table
/// namespace rather than one table earns when the policies in force do not permit it, or
/// `None` when they do.
///
/// `action` is the IAM action the operation performs (`dynamodb:ListTables`) and `resource`
/// what it acts on ([`account_resource`]). The decision itself is
/// [`identity_only_denial`]'s, which is the case this is one of.
pub fn account_denial(caller: &Caller, action: &str, resource: &str) -> Option<String> {
    identity_only_denial(caller, action, resource)
}

/// The `AccessDeniedException` message a request earns when the policies in force on the
/// CALLER do not permit `action` on `resource`, or `None` when they do — the decision for a
/// call whose addressed resource carries no policy of its own to grant or deny it: the
/// account's table namespace ([`account_resource`]), a table that does not exist yet, or a
/// backup, export or import job of one.
///
/// The rule is the module docs' rule with the two halves such a call has: no resource-based
/// policy is in force, so the decision rests on the caller's identity-based documents and the
/// scope its credential was minted with. An explicit `Deny` in either refuses; otherwise the
/// call needs an `Allow` in the identity-based documents, and with none it is refused — IAM's
/// implicit deny, naming the document that withheld the grant — and the operation has no
/// effect.
///
/// The condition keys such a call answers for are the CALLER's own
/// ([`principal_context`]): it addresses no partition, names no attribute and asks for no
/// selection, so it publishes no value for the keys a data-plane call answers for and a
/// clause over one of those stays undecided rather than holding over an empty set.
pub fn identity_only_denial(caller: &Caller, action: &str, resource: &str) -> Option<String> {
    let context = principal_context(caller);
    let published = principal_published_keys(caller);
    let granting = iam::Request::new(action, resource, &caller.principals)
        .with_context(&context)
        .with_published_keys(&published);
    // The one decision ladder of this service, over the identity-based documents and the
    // credential's scope alone: such a call addresses no resource whose own policy could
    // grant or deny it, so nothing is handed in for that half.
    identity_denial(caller, action, resource, &granting, || false)
}

/// Why the policies in force refuse the caller a call this service makes to ANOTHER
/// service's resource FOR it — the object write a table export performs, the object read a
/// table import performs — or `None` when they permit it.
///
/// Such a call is made with the CALLER's permissions rather than with this service's own, so
/// it is decided by exactly the two documents the other service would decide it by: the
/// identity-based documents in force on the caller, and the resource-based `policy` the
/// addressed resource carries (`None` for one carrying none). The rule is the module docs'
/// rule, over the two halves such a call has: an explicit `Deny` in either refuses it
/// whatever any grant says, and otherwise it needs an `Allow` — in the caller's own
/// documents, or in the resource's policy naming the caller — with the credential's own
/// scope capping whatever they grant ([`identity_denial`], the one decision ladder of this
/// service).
///
/// `action` is the IAM action of the OTHER service (`s3:PutObject`) and `resource` that
/// service's own ARN for what the call addresses, since that is what a statement about it is
/// written over. The condition keys are the CALLER's own ([`principal_context`]): this
/// service's data-plane keys are about a call on a table and say nothing about a call on
/// another service's resource.
pub fn performed_for_caller_denial(
    caller: &Caller,
    policy: Option<&str>,
    action: &str,
    resource: &str,
) -> Option<String> {
    let context = principal_context(caller);
    let published = principal_published_keys(caller);
    // An explicit deny in the resource's own policy wins over every grant, including the
    // account's own credential. It is decided against every spelling the caller may be
    // named by, its account's included.
    if let Some(policy) = policy {
        let denied = caller.denied_spellings();
        let denying = iam::Request::new(action, resource, &denied)
            .with_context(&context)
            .with_published_keys(&published);
        if iam::resource_decision(policy, &denying) == Decision::Denied {
            return Some(not_authorized(
                &caller.arn,
                action,
                resource,
                iam::RESOURCE_POLICY_EXPLICIT_DENY,
            ));
        }
    }
    let granting = iam::Request::new(action, resource, &caller.principals)
        .with_context(&context)
        .with_published_keys(&published);
    identity_denial(caller, action, resource, &granting, || {
        // The one grant a RESOURCE's own policy can make: an `Allow` that names this
        // principal directly, which is a permission of its own. Read only where the
        // identity-based documents did not already grant the call.
        policy.is_some_and(|policy| {
            iam::resource_decision(policy, &granting) == Decision::Allowed
        })
    })
}

/// The `AccessDeniedException` message a CONTROL-PLANE call on one table earns when the
/// policies in force do not permit it, or `None` when they do.
///
/// A call that reads or changes the TABLE itself — its description, its settings, its tags,
/// a backup or an export of it — is authorized exactly as a call that reads or writes its
/// items is: against the table's own resource-based policy and the documents in force on
/// the caller, for the IAM action the operation performs (`dynamodb:DescribeTable`) on the
/// table's ARN. What it does NOT carry is a data-plane call's own context: it addresses no
/// partition, names no attribute and asks for no selection, so it answers for each of those
/// keys with no value at all ([`RequestKeys::control_plane`]) and a statement conditioned on
/// one of them does not apply to it.
///
/// What one decision costs is what it READS: the documents the caller was resolved with
/// ([`request_caller`], read before this service's store was locked) and the one document
/// the table carries, each already held to the size the operation that accepted it admits
/// (`service_common::iam_authorization::MAX_POLICY_BYTES`). Nothing here is parsed from the
/// request being served, nothing grows with it, and the evaluation itself answers a decision
/// for every input — the shared evaluator reports an undecidable clause as undecided rather
/// than failing — so the work under the caller's store guard does not grow with what the
/// caller sent.
pub fn control_denial(
    caller: &Caller,
    table: &Table,
    action: &str,
    now: SystemTime,
) -> Option<String> {
    denial_keys(caller, table, action, now, &RequestKeys::control_plane())
}

/// The `AccessDeniedException` message a data-plane request that names no attribute of its
/// own earns when the policies in force do not permit it, or `None` when they do. See
/// [`denial_keys`], which this is the case of for a call that only addresses partitions.
pub fn denial_addressing(
    caller: &Caller,
    table: &Table,
    action: &str,
    now: SystemTime,
    addressed: &AddressedKeys,
) -> Option<String> {
    denial_keys(
        caller,
        table,
        action,
        now,
        &RequestKeys::addressing(addressed.clone()),
    )
}

/// The `AccessDeniedException` message a data-plane request earns when the policies in
/// force do not permit it, or `None` when they do.
///
/// `action` is the IAM action the operation performs (`dynamodb:GetItem`) and `keys` the
/// condition keys the request itself answers for ([`RequestKeys`]). The decision is the
/// module docs' rule, over the table's own policy, the caller's identity-based documents and
/// the session policies its credential was scoped with, with the request's own condition
/// keys supplied so a CONDITIONED statement is decided on the request rather than left
/// undecided; a table with no policy attached — or one whose application is not due yet —
/// states nothing, so such a request is decided by the identity-based half alone, and a
/// credential no mint scoped is capped by nothing.
pub fn denial_keys(
    caller: &Caller,
    table: &Table,
    action: &str,
    now: SystemTime,
    keys: &RequestKeys,
) -> Option<String> {
    let refusal = |reason: &str| Some(not_authorized(&caller.arn, action, &table.arn, reason));
    // "PutResourcePolicy is an asynchronous operation" and "the policy application is
    // eventually consistent" (the `PutResourcePolicy` doc): what decides this request is
    // the revision in force at this instant, which inside the span of the newest write is
    // the one the resource carried before it ([`super::ResourcePolicySlot::visible`]).
    let policy = table.resource_policy.visible(now);
    let context = condition_context(caller, keys);
    let published = published_condition_keys(caller, keys);

    // An explicit deny wins over every grant, including the account's own credential. It
    // is decided against every spelling the caller may be named by, its account's
    // included, and over this request's own condition keys, so a statement whose
    // `Condition` this request does not satisfy refuses nothing.
    if let Some(policy) = policy {
        let denied = caller.denied_spellings();
        // The spellings THIS document decides the caller by: it holds an identity principal
        // by that identity's unique id, so an ARN it holds by one of the caller's own ids
        // names the caller whatever name that identity carries now, and one it holds by
        // another id names the identity that id belongs to rather than this caller
        // ([`super::AttachedPolicy::spellings_decided`]).
        let named = policy.spellings_decided(&denied, &caller.unique_ids);
        let denying = iam::Request::new(action, &table.arn, &named)
            .with_context(&context)
            .with_published_keys(&published);
        if policy.decision(&denying) == Decision::Denied {
            return refusal("with an explicit deny in a resource-based policy");
        }
    }
    let granting = iam::Request::new(action, &table.arn, &caller.principals)
        .with_context(&context)
        .with_published_keys(&published);

    // What the CALLER may do at all, in IAM's order — the half of the decision every call
    // of this service is judged by ([`identity_denial`]), handed the one grant a TABLE's
    // own policy can make: an `Allow` that names this principal directly, which is a
    // permission of its own ("the resource-based policy and the identity-based policy are
    // evaluated together, and access is granted if either allows it"). Read only where the
    // identity-based documents did not already grant the call.
    identity_denial(caller, action, &table.arn, &granting, || {
        policy.is_some_and(|policy| {
            // Read by the same holding the deny above was: the document's principals name
            // the identities their unique ids belong to.
            let named = policy.spellings_decided(&caller.principals, &caller.unique_ids);
            let granting = iam::Request::new(action, &table.arn, &named)
                .with_context(&context)
                .with_published_keys(&published);
            policy.decision(&granting) == Decision::Allowed
        })
    })
}

/// The `AccessDeniedException` message the policies in force on the CALLER earn a request,
/// or `None` when they permit it: the one decision ladder this service judges every call by,
/// so the account-level gate ([`account_denial`]) and the table-level one ([`denial_keys`])
/// cannot drift apart.
///
/// `granting` is the request as the policy evaluator reads it, already carrying the
/// condition keys this call answers for, and `granted_by_resource` what the addressed
/// RESOURCE's own policy says about it — `false` for a call that addresses no resource of
/// its own, and read only where the identity-based documents did not already grant the call.
/// A resource policy's DENY is not this function's: it refuses whatever any grant says, so
/// the caller that holds such a document decides it before reaching here.
///
/// In IAM's order: the scope the credential was minted with can only refuse, so its explicit
/// deny is taken first ([`sts_store::ScopingPolicies::decision`], the tree's one reading of a
/// resolved scope) — a credential scoped by no document this emulator can read is scoped by
/// nothing, which is the position of a session policy that allows the request. The account's
/// own published credential holds no IAM documents and owns its resources (see the module
/// docs). Otherwise an explicit deny in an identity-based document refuses; the call then
/// needs a grant — from those documents, from the resource's own policy, or unreadable
/// because the principal carries a document this emulator does not hold — and finally that
/// grant has to pass the scope, whose refusal names THEM as the document that withheld it,
/// because the two need different fixes.
fn identity_denial(
    caller: &Caller,
    action: &str,
    resource: &str,
    granting: &iam::Request<'_>,
    granted_by_resource: impl FnOnce() -> bool,
) -> Option<String> {
    let refusal = |reason: &str| Some(not_authorized(&caller.arn, action, resource, reason));
    let scoped = caller.scope.decision(granting);
    if scoped == Decision::Denied {
        return refusal("with an explicit deny in a session policy");
    }
    if caller.account_credential {
        return None;
    }
    let decided = iam::identity_decision(&caller.documents, granting);
    if decided == Decision::Denied {
        return refusal("with an explicit deny in an identity-based policy");
    }
    let granted =
        decided == Decision::Allowed || granted_by_resource() || caller.unreadable_attachment;
    if !granted {
        return refusal(&format!(
            "because no identity-based policy allows the {action} action"
        ));
    }
    if scoped != Decision::Allowed {
        return refusal(&format!(
            "because no session policy allows the {action} action"
        ));
    }
    None
}
