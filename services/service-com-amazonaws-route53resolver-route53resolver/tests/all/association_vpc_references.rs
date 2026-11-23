// SPDX-License-Identifier: CC0-1.0

//! A Resolver resource is associated only with a VPC the account really holds.
//!
//! The rule under test: a write that associates a Resolver resource WITH a VPC
//! resolves the VPC id the request names against the account's own Amazon EC2
//! VPC inventory, and an id that resolves to nothing is refused at HTTP 400 with
//! the error that operation's own model gives it, whose message names the
//! offending id taken from the request, with nothing written.
//!
//! The three writes that name such a VPC are `AssociateResolverRule`'s `VPCId`,
//! `AssociateResolverQueryLogConfig`'s `ResourceId` ("the ID of an Amazon VPC
//! that you want this query logging configuration to log queries for") and
//! `AssociateFirewallRuleGroup`'s `VpcId`, and all three are exercised here. The
//! first two model `InvalidParameterException` and report the sentence Amazon
//! EC2 words for an id its inventory does not hold; the DNS Firewall write
//! models no `InvalidParameterException` at all and reports the one body that
//! slice answers for a resource the caller cannot address — the coded can't-find
//! sentence naming the id, closed with the trace clause of the request, the same
//! body its per-VPC configuration reads answer
//! (`smithy_query(query="operation",operation="AssociateResolverRule,AssociateResolverQueryLogConfig,AssociateFirewallRuleGroup")`).
//! The matching `Disassociate*` operations also take a VPC id, but there it
//! SELECTS an association the service already holds rather than creating one, so
//! it is not part of this rule (`tests/all/resolver_rule_disassociation.rs` owns
//! that half). Which resource a request naming NEITHER a held VPC nor a stored
//! owner is refused for is each path's own, as its recorded refusal states it:
//! the rule and DNS Firewall associates report the VPC, the query-logging
//! associate the configuration it would be made FOR.
//!
//! The rows differ along what the rule generalises over: which operation makes
//! the association, the form of the id (both of Amazon EC2's published forms and
//! an id of no EC2 form), its length (both published bounds of `ResourceId`'s
//! length 1..64), whether it differs from a held id only by case, and whether the
//! VPC never existed or was taken out of the account's inventory after an
//! association was already made in it.
//!
//! Every case goes over the wire the service actually speaks, through the shared
//! [`super::harness`].

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::harness::{call, emulator, error_type, ok, string_member, Emulator};

/// The two id forms Amazon EC2 publishes for a VPC — "vpc-" and 8 or 17
/// hexadecimal characters — used here for ids no VPC of the account carries.
const LONG_FORM_ID: &str = "vpc-0123456789abcdef0";
const SHORT_FORM_ID: &str = "vpc-0a1b2c3d";

/// The VPC id a served row associates with, which the account holds.
const HELD_ID: &str = "vpc-00000000000000011";

/// One of the three association writes: which resource it associates, how its
/// request names the VPC, and the modelled error it refuses an unresolvable VPC
/// id with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Association {
    /// `AssociateResolverRule`: a Resolver rule and a VPC.
    ResolverRule,
    /// `AssociateResolverQueryLogConfig`: a query logging configuration and a
    /// VPC.
    QueryLogConfig,
    /// `AssociateFirewallRuleGroup`: a DNS Firewall rule group and a VPC.
    FirewallRuleGroup,
}

const ASSOCIATIONS: [Association; 3] = [
    Association::ResolverRule,
    Association::QueryLogConfig,
    Association::FirewallRuleGroup,
];

impl Association {
    /// The operation that makes this association.
    fn operation(self) -> &'static str {
        match self {
            Self::ResolverRule => "AssociateResolverRule",
            Self::QueryLogConfig => "AssociateResolverQueryLogConfig",
            Self::FirewallRuleGroup => "AssociateFirewallRuleGroup",
        }
    }

    /// The modelled error an id that names no VPC of the account is refused
    /// with: the error this operation's model gives it for a resource it cannot
    /// resolve.
    fn refusal(self) -> &'static str {
        match self {
            Self::ResolverRule | Self::QueryLogConfig => "InvalidParameterException",
            Self::FirewallRuleGroup => "ResourceNotFoundException",
        }
    }

    /// The `Message` that refusal carries for the id `unknown`, which named no
    /// VPC of the account.
    ///
    /// The two `InvalidParameterException` paths report the sentence Amazon EC2
    /// words for an id its own inventory does not hold; the DNS Firewall path
    /// reports that slice's coded can't-find sentence, closed with the trace
    /// clause of the request, so only the sentence before the clause is compared
    /// here (the clause itself is
    /// [`super::firewall_not_found_trace_clause`]'s).
    fn refusal_message(self, unknown: &str) -> String {
        match self {
            Self::ResolverRule | Self::QueryLogConfig => {
                format!("The vpc ID '{unknown}' does not exist")
            }
            Self::FirewallRuleGroup => {
                format!("[RSLVR-02025] Can’t find the resource with ID \"{unknown}\".")
            }
        }
    }

    /// The part of a refusal `Message` this rule fixes: everything before the
    /// trace clause the DNS Firewall slice closes its refusals with.
    fn stated_sentence(self, message: &str) -> String {
        match self {
            Self::ResolverRule | Self::QueryLogConfig => message.to_string(),
            Self::FirewallRuleGroup => super::harness::traced_message(message)
                .unwrap_or_else(|| {
                    panic!("the refusal must close with its trace clause, got {message:?}")
                })
                .0
                .to_string(),
        }
    }

    /// The sentence a request naming BOTH an unresolvable VPC and a resource
    /// this service holds nothing under is refused with.
    ///
    /// Which of the two the refusal is about is each path's own, as its recorded
    /// refusal states it: the rule associate and the DNS Firewall associate
    /// report the VPC, so the sentence is [`Self::refusal_message`]'s, while the
    /// query-logging associate resolves the CONFIGURATION first and reports its
    /// coded sentence instead.
    fn missing_owner_sentence(self, unknown_vpc: &str) -> String {
        match self {
            Self::ResolverRule | Self::FirewallRuleGroup => self.refusal_message(unknown_vpc),
            Self::QueryLogConfig => {
                "[RSLVR-01601] The specified query logging configuration doesn't exist.".to_string()
            }
        }
    }

    /// The response member carrying the association the write mints.
    fn answer(self) -> &'static str {
        match self {
            Self::ResolverRule => "ResolverRuleAssociation",
            Self::QueryLogConfig => "ResolverQueryLogConfigAssociation",
            Self::FirewallRuleGroup => "FirewallRuleGroupAssociation",
        }
    }

    /// The member of the minted association that carries the VPC id.
    fn answered_vpc_member(self) -> &'static str {
        match self {
            Self::ResolverRule => "VPCId",
            Self::QueryLogConfig => "ResourceId",
            Self::FirewallRuleGroup => "VpcId",
        }
    }

    /// Create the resource this association is made FOR, and answer its id.
    async fn owner(self, router: &Emulator, tag: &str) -> String {
        let (operation, request, member) = match self {
            // A SYSTEM rule needs no Resolver endpoint behind it, so the rule
            // this suite associates is one the create answers on its own.
            Self::ResolverRule => (
                "CreateResolverRule",
                json!({
                    "CreatorRequestId": format!("rule-{tag}"),
                    "Name": format!("rule-{tag}"),
                    "RuleType": "SYSTEM",
                    "DomainName": format!("{tag}.example.com"),
                }),
                "ResolverRule",
            ),
            Self::QueryLogConfig => {
                // The create resolves its `DestinationArn` against the store of
                // the service that owns the destination, so the account holds
                // that bucket first.
                router.hold_query_log_destinations(["arn:aws:s3:::amzn-s3-demo-bucket"]);
                (
                    "CreateResolverQueryLogConfig",
                    json!({
                        "CreatorRequestId": format!("config-{tag}"),
                        "Name": format!("config-{tag}"),
                        "DestinationArn": "arn:aws:s3:::amzn-s3-demo-bucket",
                    }),
                    "ResolverQueryLogConfig",
                )
            }
            Self::FirewallRuleGroup => (
                "CreateFirewallRuleGroup",
                json!({
                    "CreatorRequestId": format!("group-{tag}"),
                    "Name": format!("group-{tag}"),
                }),
                "FirewallRuleGroup",
            ),
        };
        let created = ok(router, operation, request).await;
        let resource = created
            .get(member)
            .unwrap_or_else(|| panic!("the create answers with a {member}, got {created}"));
        string_member(resource, "Id")
    }

    /// The associate request body naming `owner` and `vpc`.
    ///
    /// `tag` distinguishes the request from the suite's others, which the DNS
    /// Firewall write needs: its `CreatorRequestId` is `@required`, and a repeat
    /// of one resolves to the association it already made.
    fn request(self, owner: &str, vpc: &str, tag: &str) -> Value {
        match self {
            Self::ResolverRule => json!({ "ResolverRuleId": owner, "VPCId": vpc }),
            Self::QueryLogConfig => {
                json!({ "ResolverQueryLogConfigId": owner, "ResourceId": vpc })
            }
            Self::FirewallRuleGroup => json!({
                "CreatorRequestId": format!("assoc-{tag}"),
                "FirewallRuleGroupId": owner,
                "VpcId": vpc,
                // Inside the published bounds of `Priority` ("the allowed values
                // for Priority are between 100 and 9900"), and distinct per
                // request, because one VPC holds one rule group per priority.
                "Priority": 100 + (tag.len() as i64 % 100),
                "Name": format!("assoc-{tag}"),
            }),
        }
    }

    /// Read the association `id` names back through the operation that answers
    /// it, and answer the record itself.
    async fn read_back(self, router: &Emulator, id: &str) -> Value {
        let (operation, member) = match self {
            Self::ResolverRule => ("GetResolverRuleAssociation", "ResolverRuleAssociationId"),
            Self::QueryLogConfig => (
                "GetResolverQueryLogConfigAssociation",
                "ResolverQueryLogConfigAssociationId",
            ),
            Self::FirewallRuleGroup => (
                "GetFirewallRuleGroupAssociation",
                "FirewallRuleGroupAssociationId",
            ),
        };
        let fetched = ok(router, operation, json!({ member: id })).await;
        fetched
            .get(self.answer())
            .cloned()
            .unwrap_or_else(|| panic!("the read answers with a {}, got {fetched}", self.answer()))
    }

    /// How many associations of `owner` the store holds, read over the wire.
    ///
    /// The query-logging slice is counted by the `AssociationCount` of the
    /// configuration itself ("the number of VPCs that are associated with the
    /// query logging configuration"), the other two by their list operations.
    async fn stored(self, router: &Emulator, owner: &str) -> usize {
        match self {
            Self::ResolverRule => {
                let listed = ok(router, "ListResolverRuleAssociations", json!({})).await;
                listed
                    .get("ResolverRuleAssociations")
                    .and_then(Value::as_array)
                    .map_or(0, |items| {
                        items
                            .iter()
                            .filter(|item| string_member(item, "ResolverRuleId") == owner)
                            .count()
                    })
            }
            Self::QueryLogConfig => {
                let fetched = ok(
                    router,
                    "GetResolverQueryLogConfig",
                    json!({ "ResolverQueryLogConfigId": owner }),
                )
                .await;
                fetched
                    .get("ResolverQueryLogConfig")
                    .and_then(|config| config.get("AssociationCount"))
                    .and_then(Value::as_i64)
                    .unwrap_or_default()
                    .max(0) as usize
            }
            Self::FirewallRuleGroup => {
                let listed = ok(
                    router,
                    "ListFirewallRuleGroupAssociations",
                    json!({ "FirewallRuleGroupId": owner }),
                )
                .await;
                listed
                    .get("FirewallRuleGroupAssociations")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            }
        }
    }
}

/// One row of the refusal table: a VPC id the account holds no VPC under.
#[derive(Debug)]
struct RefusedCase {
    case: &'static str,
    unknown: String,
}

/// An associate naming a VPC the account does not hold is refused at HTTP 400
/// with the error that operation models for it, the offending id named in the
/// message, and writes nothing.
#[tokio::test]
async fn an_associate_naming_a_vpc_the_account_does_not_hold_is_refused() {
    let cases = vec![
        RefusedCase {
            case: "an EC2 long-form id",
            unknown: LONG_FORM_ID.to_string(),
        },
        RefusedCase {
            case: "an EC2 short-form id",
            unknown: SHORT_FORM_ID.to_string(),
        },
        RefusedCase {
            case: "an id of no EC2 form",
            unknown: "not-in-this-account".to_string(),
        },
        RefusedCase {
            // `ResourceId`, which every one of the three members targets, has
            // length 1..64 (`smithy_query(query="shape",shape="ResourceId")`).
            case: "the shortest ResourceId (length 1)",
            unknown: "v".to_string(),
        },
        RefusedCase {
            case: "the longest ResourceId (length 64)",
            unknown: "v".repeat(64),
        },
        RefusedCase {
            case: "an id differing from one the account holds only by case",
            unknown: HELD_ID.to_uppercase(),
        },
    ];

    for association in ASSOCIATIONS {
        let mut messages: Vec<String> = Vec::new();
        for RefusedCase { case, unknown } in &cases {
            let router = emulator();
            // The VPC the account DOES hold, so every row is about the one id
            // that resolves to nothing rather than about an empty inventory.
            router.hold_vpcs([HELD_ID]);
            let owner = association.owner(&router, "refused").await;

            let (status, parsed) = call(
                &router,
                association.operation(),
                association.request(&owner, unknown, "refused"),
            )
            .await;
            assert_eq!(
                status,
                400,
                "case {case:?}: {} must answer HTTP 400, got {status}: {parsed}",
                association.operation()
            );
            assert!(
                error_type(&parsed).contains(association.refusal()),
                "case {case:?}: {} must answer {}, got {parsed}",
                association.operation(),
                association.refusal()
            );
            let message = string_member(&parsed, "Message");
            let stated = association.stated_sentence(&message);
            assert_eq!(
                stated,
                association.refusal_message(unknown),
                "case {case:?}: {} must name the id that resolved to nothing, got {parsed}",
                association.operation()
            );
            messages.push(stated);

            assert_eq!(
                association.stored(&router, &owner).await,
                0,
                "case {case:?}: a refused {} must write no association",
                association.operation()
            );
        }

        // The message is built from the request and not from fixed text: two
        // different unresolvable ids give two different messages.
        let distinct_ids: BTreeSet<&String> = cases.iter().map(|row| &row.unknown).collect();
        let distinct_messages: BTreeSet<&String> = messages.iter().collect();
        assert_eq!(
            distinct_messages.len(),
            distinct_ids.len(),
            "{}: every distinct VPC id must give a distinct Message, got {messages:?}",
            association.operation()
        );
    }
}

/// An associate whose VPC the account holds is served, whatever form that id
/// takes, and the association reads back naming that VPC.
#[tokio::test]
async fn an_associate_whose_vpc_the_account_holds_is_served() {
    // Rows spanning the forms and lengths a held VPC id takes: Amazon EC2's two
    // published forms and both published bounds of `ResourceId`'s length 1..64.
    let held = [
        ("an EC2 long-form id", LONG_FORM_ID.to_string()),
        ("an EC2 short-form id", SHORT_FORM_ID.to_string()),
        ("the shortest ResourceId (length 1)", "v".to_string()),
        ("the longest ResourceId (length 64)", "v".repeat(64)),
    ];

    for association in ASSOCIATIONS {
        let router = emulator();
        let owner = association.owner(&router, "served").await;

        for (index, (case, vpc)) in held.iter().enumerate() {
            router.hold_vpcs([vpc]);
            let tag = format!("served-{index}");
            let made = ok(
                &router,
                association.operation(),
                association.request(&owner, vpc, &tag),
            )
            .await;
            let record = made
                .get(association.answer())
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no association in {made}"));
            assert_eq!(
                string_member(&record, association.answered_vpc_member()),
                *vpc,
                "case {case:?}: {} must report the VPC the request named, got {record}",
                association.operation()
            );

            // The round trip: the reader answers the association that was
            // written, naming the same VPC.
            let id = string_member(&record, "Id");
            let read = association.read_back(&router, &id).await;
            assert_eq!(
                string_member(&read, association.answered_vpc_member()),
                *vpc,
                "case {case:?}: the stored association must name that same VPC, got {read}"
            );
        }

        assert_eq!(
            association.stored(&router, &owner).await,
            held.len(),
            "{}: every served associate must be stored",
            association.operation()
        );
    }
}

/// The refusal is keyed on the account's inventory and not on the shape of the
/// id: the very same associate is served while the VPC is held and refused once
/// it is gone.
#[tokio::test]
async fn a_vpc_taken_out_of_the_account_stops_resolving() {
    for association in ASSOCIATIONS {
        let router = emulator();
        router.hold_vpcs([HELD_ID]);
        let owner = association.owner(&router, "released").await;

        let made = ok(
            &router,
            association.operation(),
            association.request(&owner, HELD_ID, "released-first"),
        )
        .await;
        assert!(
            made.get(association.answer()).is_some(),
            "{}: an associate with a VPC the account holds must answer an association, got {made}",
            association.operation()
        );

        router.release_vpc(HELD_ID);

        // A second association of the same pair would be refused for being a
        // duplicate, so the VPC that stopped resolving is named by a request for
        // a SECOND owner — the same call, the same id, a different inventory.
        let other = association.owner(&router, "released-other").await;
        let (status, parsed) = call(
            &router,
            association.operation(),
            association.request(&other, HELD_ID, "released-second"),
        )
        .await;
        assert_eq!(
            status,
            400,
            "{}: an associate with a VPC the account no longer holds must answer HTTP 400, \
             got {status}: {parsed}",
            association.operation()
        );
        assert_eq!(
            association.stated_sentence(&string_member(&parsed, "Message")),
            association.refusal_message(HELD_ID),
            "{}: the refusal must name the VPC that stopped resolving, got {parsed}",
            association.operation()
        );
        assert_eq!(
            association.stored(&router, &other).await,
            0,
            "{}: the refused associate must write nothing",
            association.operation()
        );
    }
}

/// Each associate is refused for the resource it resolves FIRST: a request
/// naming a VPC the account does not hold is answered for THAT by the rule and
/// DNS Firewall writes, even when the resource it names does not exist either,
/// while the query-logging write resolves its configuration first and is
/// answered for that one.
#[tokio::test]
async fn a_request_naming_neither_resource_is_refused_for_the_one_resolved_first() {
    // The id of each kind that this service holds no resource under, in the form
    // it mints: a Resolver rule, a query logging configuration and a DNS
    // Firewall rule group.
    for (association, missing) in [
        (Association::ResolverRule, "rslvr-rr-0000000000000000"),
        (Association::QueryLogConfig, "rqlc-0000000000000000"),
        (Association::FirewallRuleGroup, "rslvr-frg-0000000000000000"),
    ] {
        let router = emulator();
        let (status, parsed) = call(
            &router,
            association.operation(),
            association.request(missing, LONG_FORM_ID, "missing-owner"),
        )
        .await;
        assert_eq!(
            status,
            400,
            "{}: a request naming no stored resource must answer HTTP 400, got {status}: {parsed}",
            association.operation()
        );
        assert!(
            error_type(&parsed).contains(association.refusal()),
            "{}: the resource resolved first is what the request is refused for, got {parsed}",
            association.operation()
        );
        let message = string_member(&parsed, "Message");
        let stated = super::harness::traced_message(&message)
            .map_or(message.clone(), |(stated, _)| stated.to_string());
        assert_eq!(
            stated,
            association.missing_owner_sentence(LONG_FORM_ID),
            "{}: the refusal must name the resource resolved first, got {parsed}",
            association.operation()
        );
    }
}
