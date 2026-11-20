// SPDX-License-Identifier: CC0-1.0

use super::*;

/// What one `CreateResolverRule` request asked for, apart from the token it
/// carried: the members that DEFINE the rule it would make.
///
/// It exists so the token a create carries can be compared against the request
/// the stored rule was made from, in [`rule_content_digest`], without a handler
/// listing those members a second time.
#[derive(Debug, Clone, Copy)]
pub struct RuleContent<'a> {
    pub name: Option<&'a str>,
    pub rule_type: &'a str,
    /// The domain name in the CANONICAL form a rule stores it in
    /// ([`canonical_domain_name`]), so two requests that differ only in how
    /// they wrote the root label are one request.
    pub domain_name: Option<&'a str>,
    /// The target addresses in the NORMALIZED form a rule stores them in
    /// ([`normalize_target_addresses`]), for the same reason.
    pub target_ips: &'a [TargetAddress],
    pub resolver_endpoint_id: Option<&'a str>,
    pub delegation_record: Option<&'a str>,
    pub tags: &'a [Tag],
}

/// A digest of everything a `CreateResolverRule` request asked for beside its
/// `CreatorRequestId`.
///
/// `CreatorRequestId` "identifies the request", so deciding whether a replayed
/// token is a RETRY of the request that minted the rule, or a different request
/// wearing the same token, is a comparison of the rest of the request against
/// the rule that token made. Every member is fed length-prefixed
/// ([`feed_field`]), so no two different requests collide by running their
/// values together, and the tag list is folded order-insensitively because a
/// tag set is not ordered.
pub fn rule_content_digest(content: &RuleContent<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"route53resolver/resolver-rule/content/v1");
    feed_field(&mut hasher, content.name.unwrap_or_default().as_bytes());
    feed_field(&mut hasher, content.rule_type.as_bytes());
    feed_field(
        &mut hasher,
        content.domain_name.unwrap_or_default().as_bytes(),
    );
    feed_field(
        &mut hasher,
        content.resolver_endpoint_id.unwrap_or_default().as_bytes(),
    );
    feed_field(
        &mut hasher,
        content.delegation_record.unwrap_or_default().as_bytes(),
    );
    for target in content.target_ips {
        feed_field(&mut hasher, target.ip.clone().unwrap_or_default().as_bytes());
        feed_field(
            &mut hasher,
            target.ipv6.clone().unwrap_or_default().as_bytes(),
        );
        feed_field(
            &mut hasher,
            target.port.unwrap_or_default().to_string().as_bytes(),
        );
        feed_field(
            &mut hasher,
            target.protocol.clone().unwrap_or_default().as_bytes(),
        );
        feed_field(
            &mut hasher,
            target
                .server_name_indication
                .clone()
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    let mut tags: Vec<String> = content
        .tags
        .iter()
        .map(|tag| format!("{}={}", tag.key, tag.value))
        .collect();
    tags.sort();
    for tag in &tags {
        feed_field(&mut hasher, tag.as_bytes());
    }
    hex::encode(&hasher.finalize()[..16])
}

/// The sentences a Resolver rule's `StatusMessage` states for the lifecycle step
/// a call left it in: the settled step a create reached, and the settled step an
/// update left the rule's new settings in ([`RULE_STATUS_COMPLETE`] in both
/// cases, which is the only step a rule of this store is ever moved into by a
/// caller).
///
/// `ResolverRule$StatusMessage` is "a detailed description of the status of a
/// Resolver rule"
/// (`smithy_query(query="doc",shape="ResolverRule",member="StatusMessage")`),
/// and what Route 53 Resolver describes there is the step the LAST call on the
/// rule reached: fixed text for that step, with nothing the caller asked for in
/// it, exactly as a DNS Firewall resource's step sentence is
/// ([`FIREWALL_DOMAIN_LIST_STATUS_MESSAGE_COMPLETE`]) — so a rule a caller has
/// changed describes its status by the change, not by its creation.
pub const RULE_STATUS_MESSAGE_CREATED: &str = "Successfully created Resolver Rule.";
pub const RULE_STATUS_MESSAGE_UPDATED: &str = "Successfully updated Resolver Rule.";

/// The `StatusMessage` a Resolver rule reports for the step `stated`, opened by
/// the trace clause Route 53 Resolver stamps on it: the trace id of the request
/// that reached the step, in brackets, ahead of the sentence.
///
/// The id is the trace of the REQUEST that moved the rule into that step
/// ([`request_trace_root`]), the same id this service's refusals quote
/// ([`traced_message`]) — so it is a different value on every call and is never
/// derived from anything the caller asked about. It is stamped onto the record
/// by the call that reached the step and carried by every later projection of
/// the rule, which is why a read reports the sentence the last write answered
/// rather than a freshly traced one. The whole message fits `StatusMessage`
/// (length 0..255).
pub fn rule_status_message(stated: &str, trace_root: &str) -> String {
    format!("[Trace id: {trace_root}] {stated}")
}

/// A Resolver rule, as stored.
#[derive(Debug, Clone)]
pub struct ResolverRuleRecord {
    /// Position in the store's creation order, from
    /// [`ComAmazonawsRoute53resolverRoute53resolverState::id_seq`].
    pub seq: u64,
    pub id: String,
    pub arn: String,
    pub creator_request_id: String,
    pub name: Option<String>,
    pub domain_name: Option<String>,
    pub rule_type: String,
    pub target_ips: Vec<TargetAddress>,
    pub resolver_endpoint_id: Option<String>,
    pub delegation_record: Option<String>,
    pub owner_id: String,
    pub share_status: String,
    /// The `ResolverRuleStatus` this rule reports; see [`RULE_STATUS_COMPLETE`]
    /// on why a stored rule is already settled.
    pub status: String,
    /// The `StatusMessage` describing the step [`Self::status`] names, as the
    /// call that created the rule stamped it ([`rule_status_message`]), so every
    /// projection of the rule — the create's, the read's, the listing's and the
    /// update's — describes its status with the same sentence.
    pub status_message: Option<String>,
    pub tags: Vec<Tag>,
    pub creation_time: String,
    pub modification_time: String,
    /// The resource policy attached to this rule, as the last
    /// `PutResolverRulePolicy` for it wrote it, and `None` while no policy has
    /// been attached.
    ///
    /// Held on the rule's own record, not in a table beside it, because a
    /// resource policy belongs to the resource it shares: a read of the rule's
    /// policy resolves through the same record every other rule operation
    /// reads, and a rule the store no longer holds has no policy to report.
    pub resolver_rule_policy: Option<String>,
}

impl ResolverRuleRecord {
    /// What the request that made this rule asked for, for the replay
    /// comparison [`rule_content_digest`] describes.
    pub fn content(&self) -> RuleContent<'_> {
        RuleContent {
            name: self.name.as_deref(),
            rule_type: &self.rule_type,
            domain_name: self.domain_name.as_deref(),
            target_ips: &self.target_ips,
            resolver_endpoint_id: self.resolver_endpoint_id.as_deref(),
            delegation_record: self.delegation_record.as_deref(),
            tags: &self.tags,
        }
    }

    /// Project the record onto the modelled `ResolverRule`.
    ///
    /// This is the ONE serializer of the shape: the create, the read, the list
    /// and the update all answer through it, so no operation can report a rule
    /// with fewer members than another.
    pub fn to_rule(&self) -> ResolverRule {
        ResolverRule {
            id: Some(self.id.clone()),
            creator_request_id: Some(self.creator_request_id.clone()),
            arn: Some(self.arn.clone()),
            domain_name: self.domain_name.clone(),
            status: Some(self.status.clone()),
            status_message: self.status_message.clone(),
            rule_type: Some(self.rule_type.clone()),
            name: self.name.clone(),
            // `TargetIps` is modelled with a minimum length of 1, so an empty
            // list is left unset rather than serialized as `[]`.
            target_ips: if self.target_ips.is_empty() {
                None
            } else {
                Some(self.target_ips.clone())
            },
            resolver_endpoint_id: self.resolver_endpoint_id.clone(),
            owner_id: Some(self.owner_id.clone()),
            share_status: Some(self.share_status.clone()),
            creation_time: Some(self.creation_time.clone()),
            modification_time: Some(self.modification_time.clone()),
            delegation_record: self.delegation_record.clone(),
        }
    }

    /// Whether this rule satisfies one `ListResolverRules` filter: whether the
    /// value it presents for that member is among the filter's values.
    ///
    /// A set lookup per record and per filter key, as
    /// [`ResolverEndpointRecord::matches_filter`] is, so a filtered page costs
    /// O(stored rules x filter keys) and not O(values the request carried). A
    /// member the rule does not carry matches nothing.
    pub fn matches_filter(&self, key: RuleFilterKey, values: &HashSet<String>) -> bool {
        match key {
            RuleFilterKey::CreatorRequestId => values.contains(self.creator_request_id.as_str()),
            RuleFilterKey::DomainName => self
                .domain_name
                .as_deref()
                .is_some_and(|domain| values.contains(domain)),
            RuleFilterKey::Name => self.name.as_deref().is_some_and(|name| values.contains(name)),
            RuleFilterKey::ResolverEndpointId => self
                .resolver_endpoint_id
                .as_deref()
                .is_some_and(|id| values.contains(id)),
            RuleFilterKey::Status => values.contains(self.status.as_str()),
            RuleFilterKey::Type => values.contains(self.rule_type.as_str()),
        }
    }
}

/// The `OwnerId` a Resolver rule the SERVICE defined reports: Route 53 Resolver
/// itself, not the account that reads the rule.
///
/// A rule nothing in the account created is a resource of the service, which is
/// what naming the service as its owner says about it — the same thing a
/// published DNS Firewall domain list says through
/// [`MANAGED_FIREWALL_DOMAIN_LIST_OWNER`]. It fits `AccountId`
/// (length 12..32).
///
/// Source: the Resolver rule catalog every account and Region is served, whose
/// owner name, ids and shape Route 53 Resolver publishes for its own rules and
/// nothing derives; it carries no account of any caller and names nothing of
/// this emulator's.
pub const AUTODEFINED_RESOLVER_RULE_OWNER: &str = "Route 53 Resolver";

/// The `RuleTypeOption` of a rule that resolves a name recursively, out of the
/// model's enum (`smithy_query(query="shape",shape="RuleTypeOption")`):
/// "currently, only Resolver can create rules that have a value of `RECURSIVE`
/// for `RuleType`", so it is the type of the rules in
/// [`AUTODEFINED_RESOLVER_RULES`] and of no rule a caller creates.
pub const RULE_TYPE_RECURSIVE: &str = "RECURSIVE";

/// One Resolver rule Route 53 Resolver defines for an account itself, rather
/// than one a `CreateResolverRule` made.
///
/// An autodefined rule belongs to the service, not to the account that reads it:
/// it is served to every account in every Region, no request created it, and
/// nothing an account does changes it. So — exactly as a published DNS Firewall
/// domain list is ([`ManagedFirewallDomainList`]) — it is NOT a stored record
/// but a catalog entry, and every member a response reports for it is computed
/// from this entry plus the Region the request was signed for.
#[derive(Debug, Clone, Copy)]
pub struct AutodefinedResolverRule {
    /// The `Id` Route 53 Resolver defines this rule under: the service's own
    /// identifier for it, identical for every account and every process that
    /// reads it, derived from nothing of the caller's, and fitting `ResourceId`
    /// (length 1..64).
    pub defined_id: &'static str,
    /// The rule's `Name`, as the service defines it.
    pub name: &'static str,
    /// The `DomainName` the rule selects on, in the canonical fully-qualified
    /// form every rule reports one in ([`canonical_domain_name`]).
    pub domain_name: &'static str,
    /// The rule's `RuleType` — what Resolver does with a query matching that
    /// domain.
    pub rule_type: &'static str,
}

/// The Resolver rules Route 53 Resolver defines for every account, in the order
/// `ListResolverRules` reports them: ahead of the account's own rules, because
/// they existed before any of them.
///
/// The recursive rule for the DNS root is what makes a VPC's Resolver answer
/// every name no other rule selects by resolving it on the public internet,
/// which is why every account and Region has it and why no caller can create
/// one ([`RULE_TYPE_RECURSIVE`]).
///
/// Source: the Resolver rule catalog cited on
/// [`AUTODEFINED_RESOLVER_RULE_OWNER`]; the id, name, domain and type are the
/// service's own, and the Region of the request is the only part of the entry's
/// projection that varies.
pub const AUTODEFINED_RESOLVER_RULES: &[AutodefinedResolverRule] = &[AutodefinedResolverRule {
    defined_id: "rslvr-autodefined-rr-internet-resolver",
    name: "Internet Resolver",
    domain_name: ".",
    rule_type: RULE_TYPE_RECURSIVE,
}];

impl AutodefinedResolverRule {
    /// This rule's `Id`: the one the service defines it under
    /// ([`Self::defined_id`]), so every read of it — by any account, in any
    /// process — reports the same id, and nothing of the caller's reaches it.
    pub fn id(&self) -> &'static str {
        self.defined_id
    }

    /// This rule's `Arn`: the defined id under this service's own namespace in
    /// the Region the request was signed for, with NO account field.
    ///
    /// The rule is not a resource of the account reading it — that is what the
    /// owner it reports ([`AUTODEFINED_RESOLVER_RULE_OWNER`]) says — so the
    /// account field of its ARN is empty, and this does not go through
    /// `::engine::emulator_arn`, which exists to root a resource of THIS
    /// account.
    pub fn arn(&self, region: &str) -> String {
        format!(
            "arn:aws:{ARN_NAMESPACE}:{region}::autodefined-rule/{}",
            self.id()
        )
    }

    /// Project the catalog entry onto the modelled `ResolverRule`, the shape
    /// `ListResolverRules` and `GetResolverRule` report a rule as.
    ///
    /// The members a rule of the account carries because a request made it —
    /// `TargetIps`, `ResolverEndpointId`, `DelegationRecord`, the creation and
    /// modification instants, and the `StatusMessage` describing the step a call
    /// reached ([`RULE_STATUS_MESSAGE_CREATED`]) — are absent, because no call
    /// made this rule and no call has changed it.
    pub fn to_rule(&self, region: &str) -> ResolverRule {
        ResolverRule {
            id: Some(self.id().to_string()),
            // `CreatorRequestId` is "a unique string that you specified when
            // you created the Resolver rule": nothing the account did created
            // this rule, so there is no such string to report and the member is
            // served empty.
            creator_request_id: Some(String::new()),
            arn: Some(self.arn(region)),
            domain_name: Some(self.domain_name.to_string()),
            // The rule is in effect for every account the moment the account
            // exists, so it stands in the settled step
            // ([`RULE_STATUS_COMPLETE`]) and no call of this account's ever
            // moves it out of it.
            status: Some(RULE_STATUS_COMPLETE.to_string()),
            status_message: None,
            rule_type: Some(self.rule_type.to_string()),
            name: Some(self.name.to_string()),
            target_ips: None,
            resolver_endpoint_id: None,
            owner_id: Some(AUTODEFINED_RESOLVER_RULE_OWNER.to_string()),
            // The service shares the rule with nobody: every account simply has
            // it.
            share_status: Some(SHARE_STATUS_NOT_SHARED.to_string()),
            creation_time: None,
            modification_time: None,
            delegation_record: None,
        }
    }

    /// Whether this rule satisfies one `ListResolverRules` filter, decided on
    /// the values it presents exactly as [`ResolverRuleRecord::matches_filter`]
    /// decides it for a stored rule — so a filtered listing selects out of the
    /// whole collection and not only out of the account's own rules. A member
    /// the rule does not carry matches nothing.
    pub fn matches_filter(&self, key: RuleFilterKey, values: &HashSet<String>) -> bool {
        match key {
            // The empty `CreatorRequestId` the rule reports: no request created
            // it, so only a filter asking for that empty string selects it.
            RuleFilterKey::CreatorRequestId => values.contains(""),
            RuleFilterKey::DomainName => values.contains(self.domain_name),
            RuleFilterKey::Name => values.contains(self.name),
            RuleFilterKey::ResolverEndpointId => false,
            RuleFilterKey::Status => values.contains(RULE_STATUS_COMPLETE),
            RuleFilterKey::Type => values.contains(self.rule_type),
        }
    }
}

/// The autodefined rule this `ResolverRuleId` names, or `None` for an id that
/// names none.
///
/// Every operation that resolves a `ResolverRuleId` to a rule it REPORTS
/// consults this after the store, so an id `ListResolverRules` reported means
/// one rule throughout the service rather than resolving in the operation that
/// lists them and nowhere else.
pub fn autodefined_resolver_rule(id: &str) -> Option<&'static AutodefinedResolverRule> {
    AUTODEFINED_RESOLVER_RULES
        .iter()
        .find(|defined| defined.id() == id)
}

/// Every autodefined rule's projection for `region` that `filters` select: the
/// FIXED prefix of the Resolver rule collection.
///
/// `ListResolverRules` pages this ahead of the account's own rules, so a rule a
/// caller creates while it is paging only ever appends past the page it has
/// already read.
pub fn autodefined_rules_page(
    region: &str,
    filters: &[(RuleFilterKey, HashSet<String>)],
) -> Vec<ResolverRule> {
    AUTODEFINED_RESOLVER_RULES
        .iter()
        .filter(|defined| {
            filters
                .iter()
                .all(|(key, values)| defined.matches_filter(*key, values))
        })
        .map(|defined| defined.to_rule(region))
        .collect()
}

/// The member a `ListResolverRules` filter selects on.
///
/// The catalog is exactly the one the model documents for this operation
/// (`smithy_query(query="doc",shape="Filter",member="Name")`, section
/// **ListResolverRules**): `CreatorRequestId`, `DomainName`, `Name`,
/// `ResolverEndpointId`, `Status`, `Type`. It is a different catalog from
/// [`FilterKey`]'s, because the valid filter names "depend on the action that
/// you're including the filter in".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleFilterKey {
    CreatorRequestId,
    DomainName,
    Name,
    ResolverEndpointId,
    Status,
    Type,
}

impl RuleFilterKey {
    /// The canonical name of this key, used to describe a filter set in a
    /// pagination cursor's scope.
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::CreatorRequestId => "CreatorRequestId",
            Self::DomainName => "DomainName",
            Self::Name => "Name",
            Self::ResolverEndpointId => "ResolverEndpointId",
            Self::Status => "Status",
            Self::Type => "Type",
        }
    }

    /// Whether the values of this filter name a rule domain, which is matched in
    /// the canonical fully-qualified form a rule is stored and reported in (see
    /// [`canonical_domain_name`]): the doc asks a caller to "include a trailing
    /// dot (.) after the domain name", and canonicalising the value makes both
    /// spellings of one domain select the same rules.
    pub fn selects_on_domain_name(self) -> bool {
        matches!(self, Self::DomainName)
    }

    /// Resolve a wire filter name, or `None` when it is outside the documented
    /// catalog. The name is folded by [`fold_filter_name`], as
    /// [`FilterKey::parse`] folds the endpoint catalog's names.
    pub fn parse(name: &str) -> Option<Self> {
        match fold_filter_name(name).as_str() {
            "CREATORREQUESTID" => Some(Self::CreatorRequestId),
            "DOMAINNAME" => Some(Self::DomainName),
            "NAME" => Some(Self::Name),
            "RESOLVERENDPOINTID" => Some(Self::ResolverEndpointId),
            "STATUS" => Some(Self::Status),
            "TYPE" => Some(Self::Type),
            _ => None,
        }
    }
}

/// One association between a Resolver rule and a VPC, as stored.
#[derive(Debug, Clone)]
pub struct ResolverRuleAssociationRecord {
    /// Position in the store's creation order, from
    /// [`ComAmazonawsRoute53resolverRoute53resolverState::id_seq`].
    pub seq: u64,
    pub id: String,
    pub resolver_rule_id: String,
    pub vpc_id: String,
    pub name: Option<String>,
    /// The transient `ResolverRuleAssociationStatus` reported until
    /// [`Self::complete_at`] falls due.
    pub status: String,
    pub status_message: Option<String>,
    /// The instant at which the move to `COMPLETE` is owed.
    pub complete_at: Instant,
}

impl ResolverRuleAssociationRecord {
    /// This association's `ResolverRuleAssociationStatus` as of `now`: the
    /// transient status until the transition the clock owes falls due,
    /// `COMPLETE` after it.
    pub fn status_at(&self, now: Instant) -> &str {
        if now < self.complete_at {
            &self.status
        } else {
            RULE_ASSOCIATION_STATUS_COMPLETE
        }
    }

    /// Project the record onto the modelled `ResolverRuleAssociation`.
    pub fn to_association(&self, now: Instant) -> ResolverRuleAssociation {
        ResolverRuleAssociation {
            id: Some(self.id.clone()),
            resolver_rule_id: Some(self.resolver_rule_id.clone()),
            name: self.name.clone(),
            vpc_id: Some(self.vpc_id.clone()),
            status: Some(self.status_at(now).to_string()),
            status_message: self.status_message.clone(),
        }
    }

    /// Whether this association satisfies one `ListResolverRuleAssociations`
    /// filter: whether the value it presents for that member is among the
    /// filter's values, as [`ResolverRuleRecord::matches_filter`] decides it for
    /// a rule. A member the association does not carry matches nothing, and the
    /// status it is matched on is the one it reports as of `now`.
    pub fn matches_filter(
        &self,
        key: RuleAssociationFilterKey,
        values: &HashSet<String>,
        now: Instant,
    ) -> bool {
        match key {
            RuleAssociationFilterKey::Name => {
                self.name.as_deref().is_some_and(|name| values.contains(name))
            }
            RuleAssociationFilterKey::ResolverRuleId => {
                values.contains(self.resolver_rule_id.as_str())
            }
            RuleAssociationFilterKey::Status => values.contains(self.status_at(now)),
            RuleAssociationFilterKey::VpcId => values.contains(self.vpc_id.as_str()),
        }
    }
}

/// The member a `ListResolverRuleAssociations` filter selects on.
///
/// The catalog is exactly the one the model documents for this operation
/// (`smithy_query(query="doc",shape="Filter",member="Name")`, section
/// **ListResolverRuleAssociations**): `Name`, `ResolverRuleId`, `Status`,
/// `VPCId`. It is a different catalog from [`RuleFilterKey`]'s, because the
/// valid filter names "depend on the action that you're including the filter
/// in".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAssociationFilterKey {
    Name,
    ResolverRuleId,
    Status,
    VpcId,
}

impl RuleAssociationFilterKey {
    /// The canonical name of this key, used to describe a filter set in a
    /// pagination cursor's scope.
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::ResolverRuleId => "ResolverRuleId",
            Self::Status => "Status",
            Self::VpcId => "VPCId",
        }
    }

    /// Resolve a wire filter name, or `None` when it is outside the documented
    /// catalog. The name is folded by [`fold_filter_name`], as
    /// [`RuleFilterKey::parse`] folds the rule catalog's names.
    pub fn parse(name: &str) -> Option<Self> {
        match fold_filter_name(name).as_str() {
            "NAME" => Some(Self::Name),
            "RESOLVERRULEID" => Some(Self::ResolverRuleId),
            "STATUS" => Some(Self::Status),
            "VPCID" => Some(Self::VpcId),
            _ => None,
        }
    }
}
