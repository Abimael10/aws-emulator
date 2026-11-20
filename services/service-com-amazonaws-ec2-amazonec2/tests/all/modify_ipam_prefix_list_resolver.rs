// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyIpamPrefixListResolver`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * modifying an IPAM prefix list resolver WRITES exactly the members the request names — "you
//!   can update the description and CIDR selection rules" and "these rules replace the existing
//!   rules entirely" (`ModifyIpamPrefixListResolver`,
//!   `ModifyIpamPrefixListResolverRequest$Rules`) — leaves every other member of the stored
//!   resolver as it was, answers and reads back through `DescribeIpamPrefixListResolvers` in the
//!   transient `modify-in-progress` state, and the CLOCK — not a later call — moves it on to
//!   `modify-complete`;
//! * a request that names neither member changes neither, and a request naming only one leaves
//!   the other;
//! * an id the account holds no resolver for is refused with this resource's own `NotFound` code
//!   and the quoted, id-interpolated sentence this operation words it with, a resolver whose
//!   deletion has already been asked for with `IncorrectState` naming the state it is in, and a
//!   rule set no documented constraint admits with EC2's code for that class — none of which
//!   writes anything;
//! * `DryRun` answers the permission question with `DryRunOperation` and writes nothing, whether
//!   or not the resolver the request names exists.
//!
//! Each table row is a distinct input along what those rules generalise over: which members the
//! request carries, how many CIDR selection rules the resolver held and how many replace them,
//! of which rule type and with how many conditions, which address family the resolver holds, and
//! how much of the resolver's life the clock has already run when the modification arrives.

use std::time::{Duration, Instant};

use ec2::ops::types::IPAM_PREFIX_LIST_RESOLVER_STATE_VALUES;
use ec2::state::{
    ADDRESS_FAMILY_IPV4, ADDRESS_FAMILY_IPV6, DRY_RUN_OPERATION, INCORRECT_STATE,
    INVALID_IPAM_PREFIX_LIST_RESOLVER_ID_NOT_FOUND, INVALID_IPAM_SCOPE_ID_NOT_FOUND,
    INVALID_PARAMETER_VALUE, IPAM_PREFIX_LIST_RESOLVER_REFUSAL_NAME,
    IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_COMPLETE,
    IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_IN_PROGRESS, IPAM_STATE_CREATE_COMPLETE,
    IPAM_STATE_MODIFY_COMPLETE, IPAM_STATE_MODIFY_IN_PROGRESS,
    MAX_IPAM_PREFIX_LIST_RESOLVER_RULES, RESOURCE_TYPE_IPAM_PREFIX_LIST_RESOLVER, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for, which is therefore each IPAM's home Region.
const REGION: &str = "us-east-1";

/// The action under test.
const MODIFY: &str = "ModifyIpamPrefixListResolver";

/// The action that creates the resolvers it modifies.
const CREATE: &str = "CreateIpamPrefixListResolver";

/// The action that reads them back.
const DESCRIBE: &str = "DescribeIpamPrefixListResolvers";

/// The action that puts a resolver beyond modifying.
const DELETE: &str = "DeleteIpamPrefixListResolver";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// A well-formed `IpamPrefixListResolverId` the account holds no resolver for.
///
/// Spelled from the top of the hexadecimal range rather than the bottom: this store mints ids
/// from a monotonic counter (`next_id`), so an id this high cannot collide with one a created
/// resolver was given however many ids a run has minted.
const UNKNOWN_RESOLVER_ID: &str = "ipam-plr-0ffffffffffffffff";

/// A well-formed `IpamScopeId` the account holds no scope for, by the same reading.
const UNKNOWN_SCOPE_ID: &str = "ipam-scope-0ffffffffffffffff";

/// One rule type of the modelled catalog
/// (`ec2::ops::types::IPAM_PREFIX_LIST_RESOLVER_RULE_TYPE_VALUES` carries `static-cidr`,
/// `ipam-resource-cidr` and `ipam-pool-cidr`).
const RULE_TYPE_STATIC_CIDR: &str = "static-cidr";

/// A second entry of that catalog, which selects from a resource rather than a fixed CIDR.
const RULE_TYPE_IPAM_RESOURCE_CIDR: &str = "ipam-resource-cidr";

/// A third, which selects from a pool.
const RULE_TYPE_IPAM_POOL_CIDR: &str = "ipam-pool-cidr";

/// One entry of the modelled condition-operation catalog
/// (`ec2::ops::types::IPAM_PREFIX_LIST_RESOLVER_RULE_CONDITION_OPERATION_VALUES`).
const CONDITION_OPERATION: &str = "equals";

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it: the router and the
/// engine its store lives in.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One call of `action` signed for [`REGION`].
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of `action` requiring success, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The IPAM a suite creates through the operation that owns it: the id a resolver create names
/// it by, and one of the scopes it came up with, which a CIDR selection rule may select from.
struct CreatedIpam {
    id: String,
    scope: String,
}

/// Create one IPAM through `CreateIpam` and answer what a request names it by.
async fn create_ipam(case: &str, router: &MultiServiceRouter) -> CreatedIpam {
    let body = ok(case, router, "CreateIpam", &[]).await;
    let read = |name: &str| -> String {
        element(&body, name)
            .unwrap_or_else(|| panic!("case {case:?}: no <{name}> on the created IPAM in {body}"))
    };
    CreatedIpam {
        id: read("ipamId"),
        scope: read("privateDefaultScopeId"),
    }
}

/// One resolver as a response reports it: every member read off the wire except the state, which
/// is what a modification MOVES and so is asserted on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedResolver {
    resolver_id: Option<String>,
    resolver_arn: Option<String>,
    ipam_arn: Option<String>,
    ipam_region: Option<String>,
    owner_id: Option<String>,
    description: Option<String>,
    address_family: Option<String>,
    tags: Vec<(String, String)>,
}

/// The resolver element of `body` as [`ReportedResolver`] and the state it reports: a create, a
/// modify, a delete and a describe all carry the same `IpamPrefixListResolver` shape, so one
/// reader serves them all.
fn read_resolver(case: &str, body: &str) -> (ReportedResolver, Option<String>) {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamPrefixListResolverId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no resolver in {body}"));
    let tags = child(node, "tagSet")
        .map(|set| {
            set.children()
                .filter(|item| item.is_element())
                .map(|item| {
                    (
                        child_text(item, "key").unwrap_or_default(),
                        child_text(item, "value").unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let reported = ReportedResolver {
        resolver_id: child_text(node, "ipamPrefixListResolverId"),
        resolver_arn: child_text(node, "ipamPrefixListResolverArn"),
        ipam_arn: child_text(node, "ipamArn"),
        ipam_region: child_text(node, "ipamRegion"),
        owner_id: child_text(node, "ownerId"),
        description: child_text(node, "description"),
        address_family: child_text(node, "addressFamily"),
        tags,
    };
    let state = child_text(node, "state");
    (reported, state)
}

/// The resolver `DescribeIpamPrefixListResolvers` reports for `resolver_id`, and the state it
/// reports it in.
async fn describe_one(
    case: &str,
    router: &MultiServiceRouter,
    resolver_id: &str,
) -> (ReportedResolver, Option<String>) {
    let body = ok(
        case,
        router,
        DESCRIBE,
        &indexed("IpamPrefixListResolverId", &[resolver_id]),
    )
    .await;
    read_resolver(case, &body)
}

/// The state `DescribeIpamPrefixListResolvers` reports for `resolver_id` right now.
async fn described_state(case: &str, router: &MultiServiceRouter, resolver_id: &str) -> String {
    describe_one(case, router, resolver_id)
        .await
        .1
        .unwrap_or_default()
}

/// The state `DescribeIpamPrefixListResolvers` reports for `resolver_id` once it is `wanted`:
/// the state is looked at AGAIN in a loop until it has moved or a deadline far past
/// [`TRANSITION_DELAY`] passes, so the wait is for the CONDITION rather than for a span of the
/// clock.
async fn described_state_once(
    case: &str,
    router: &MultiServiceRouter,
    resolver_id: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let state = described_state(case, router, resolver_id).await;
        if state == wanted || Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The state the stored resolver `resolver_id` is in right now, read straight out of the store;
/// `None` when the account holds no resolver under that id.
fn stored_state(engine: &engine::InMemoryEngine, resolver_id: &str) -> Option<String> {
    let state = ec2::state::service_state(engine).expect("the EC2 store");
    let store = state.lock();
    store
        .ipam_prefix_list_resolvers
        .get(resolver_id)
        .and_then(|resolver| resolver.state.clone())
}

/// Whether the stored resolver `resolver_id` is in one of the two MODIFY states — what a refused
/// modification must NOT have written.
///
/// The exact state a resolver was in before a refused modification is not what a row asserts:
/// the CLOCK moves a created resolver from `create-in-progress` to `create-complete` on its own,
/// whether or not anybody calls, so pinning that word would make a row fail for a move the clock
/// owed rather than for a write the refusal made.
fn is_being_modified(engine: &engine::InMemoryEngine, resolver_id: &str) -> bool {
    stored_state(engine, resolver_id).is_some_and(|state| {
        state == IPAM_STATE_MODIFY_IN_PROGRESS || state == IPAM_STATE_MODIFY_COMPLETE
    })
}

/// One stored CIDR selection rule, flattened to the values a row can compare: the rule type, the
/// static CIDR, the IPAM scope it selects from, the resource type and how many conditions it
/// carries.
type StoredRule = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    usize,
);

/// The CIDR selection rules the store holds for one resolver.
///
/// The modelled [`ec2::ops::types::IpamPrefixListResolver`] carries no member for them, and
/// `GetIpamPrefixListResolverRules` — the operation that reports them — is still a scaffold stub
/// with its own open issue, so the store is the only place the stored rules can be read today.
fn stored_rules(engine: &engine::InMemoryEngine, resolver_id: &str) -> Vec<StoredRule> {
    let state = ec2::state::service_state(engine).expect("the EC2 store");
    let store = state.lock();
    store
        .ipam_prefix_list_resolver_rules
        .get(resolver_id)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|rule| {
            (
                rule.rule_type.clone(),
                rule.static_cidr.clone(),
                rule.ipam_scope_id.clone(),
                rule.resource_type.clone(),
                rule.conditions.as_ref().map_or(0, Vec::len),
            )
        })
        .collect()
}

/// One CIDR selection rule a request names, as a row spells it.
#[derive(Debug, Clone, Copy)]
struct RuleSpec {
    /// The rule type, from the modelled catalog.
    rule_type: &'static str,
    /// The fixed CIDR this rule selects, if it names one.
    static_cidr: Option<&'static str>,
    /// Whether this rule selects from the IPAM's own private scope.
    scope: bool,
    /// The `Cidr` of each condition this rule carries, each with [`CONDITION_OPERATION`].
    conditions: &'static [&'static str],
}

/// The form fields for a whole `Rules` list, as ec2Query flattens it into indexed `Rule.N` keys.
fn rule_params(ipam: &CreatedIpam, rules: &[RuleSpec]) -> Vec<Param> {
    let mut params = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        let position = index + 1;
        params.push(param(&format!("Rule.{position}.RuleType"), rule.rule_type));
        if let Some(cidr) = rule.static_cidr {
            params.push(param(&format!("Rule.{position}.StaticCidr"), cidr));
        }
        if rule.scope {
            params.push(param(&format!("Rule.{position}.IpamScopeId"), &ipam.scope));
        }
        for (nth, cidr) in rule.conditions.iter().enumerate() {
            let condition = nth + 1;
            params.push(param(
                &format!("Rule.{position}.Condition.{condition}.Operation"),
                CONDITION_OPERATION,
            ));
            params.push(param(
                &format!("Rule.{position}.Condition.{condition}.Cidr"),
                cidr,
            ));
        }
    }
    params
}

/// The rules of `rules` as the store holds them, which is what [`stored_rules`] must answer.
fn expected_rules(ipam: &CreatedIpam, rules: &[RuleSpec]) -> Vec<StoredRule> {
    rules
        .iter()
        .map(|rule| {
            (
                Some(rule.rule_type.to_string()),
                rule.static_cidr.map(str::to_string),
                rule.scope.then(|| ipam.scope.clone()),
                None,
                rule.conditions.len(),
            )
        })
        .collect()
}

/// The form fields for one `ipam-prefix-list-resolver` tag specification over `tags`.
fn tag_params(tags: &[(&str, &str)]) -> Vec<Param> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        RESOURCE_TYPE_IPAM_PREFIX_LIST_RESOLVER,
    )];
    for (index, (key, value)) in tags.iter().enumerate() {
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Key", index + 1),
            key,
        ));
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Value", index + 1),
            value,
        ));
    }
    params
}

/// Create one resolver in the IPAM `ipam` names and answer its id.
async fn create_resolver(
    case: &str,
    router: &MultiServiceRouter,
    ipam: &CreatedIpam,
    family: &str,
    description: Option<&str>,
    tags: &[(&str, &str)],
    rules: &[RuleSpec],
) -> String {
    let mut params = vec![param("IpamId", &ipam.id), param("AddressFamily", family)];
    if let Some(description) = description {
        params.push(param("Description", description));
    }
    params.extend(tag_params(tags));
    params.extend(rule_params(ipam, rules));
    let body = ok(case, router, CREATE, &params).await;
    read_resolver(case, &body)
        .0
        .resolver_id
        .unwrap_or_else(|| panic!("case {case:?}: no resolver id in {body}"))
}

// ---------------------------------------------------------------------------
// The words the suite pins
// ---------------------------------------------------------------------------

/// The state words this suite asserts on are the model's own: each is a member of the catalog
/// the model publishes for `IpamPrefixListResolverState`, so a row cannot pin a word the API does
/// not have.
#[test]
fn the_asserted_state_words_are_the_models_own() {
    for (case, word) in [
        ("a resolver being modified", IPAM_STATE_MODIFY_IN_PROGRESS),
        ("a resolver whose modification is done", IPAM_STATE_MODIFY_COMPLETE),
        ("a resolver in service", IPAM_STATE_CREATE_COMPLETE),
        (
            "a resolver being deleted",
            IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_IN_PROGRESS,
        ),
    ] {
        assert!(
            IPAM_PREFIX_LIST_RESOLVER_STATE_VALUES.contains(&word),
            "case {case:?}: expected {word:?} to be a member of the modelled catalog, which \
             carries {IPAM_PREFIX_LIST_RESOLVER_STATE_VALUES:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// What a modification answers and writes
// ---------------------------------------------------------------------------

/// One modification to apply: a distinct input along what the operation generalises over.
struct Row {
    /// What this row is called in a failure message.
    case: &'static str,
    /// The address family the resolver holds.
    family: &'static str,
    /// The description the create carried, if any.
    created_description: Option<&'static str>,
    /// The CIDR selection rules the create carried.
    created_rules: &'static [RuleSpec],
    /// The tags the create carried, which no modification touches.
    tags: &'static [(&'static str, &'static str)],
    /// Whether the clock has already finished the resolver's creation when the modify arrives.
    settled: bool,
    /// The description the modification names, if any.
    new_description: Option<&'static str>,
    /// The rule set the modification names, if any — it replaces the held one entirely.
    new_rules: Option<&'static [RuleSpec]>,
}

/// One static-CIDR rule.
const ONE_STATIC: &[RuleSpec] = &[RuleSpec {
    rule_type: RULE_TYPE_STATIC_CIDR,
    static_cidr: Some("10.1.0.0/16"),
    scope: false,
    conditions: &[],
}];

/// Two static-CIDR rules, so a row can shrink a rule set as well as grow one.
const TWO_STATIC: &[RuleSpec] = &[
    RuleSpec {
        rule_type: RULE_TYPE_STATIC_CIDR,
        static_cidr: Some("10.2.0.0/16"),
        scope: false,
        conditions: &[],
    },
    RuleSpec {
        rule_type: RULE_TYPE_STATIC_CIDR,
        static_cidr: Some("10.3.0.0/16"),
        scope: false,
        conditions: &[],
    },
];

/// A rule that selects from the IPAM's own scope under two conditions, and one that selects from
/// a pool: a rule set differing from the static ones in type, in what it names and in how many
/// conditions it carries.
const SCOPED_AND_POOLED: &[RuleSpec] = &[
    RuleSpec {
        rule_type: RULE_TYPE_IPAM_RESOURCE_CIDR,
        static_cidr: None,
        scope: true,
        conditions: &["10.4.0.0/16", "2001:db8::/32"],
    },
    RuleSpec {
        rule_type: RULE_TYPE_IPAM_POOL_CIDR,
        static_cidr: None,
        scope: true,
        conditions: &[],
    },
];

/// The modifications the suite is asserted over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a description set on a resolver that had none, its rules left alone",
            family: ADDRESS_FAMILY_IPV4,
            created_description: None,
            created_rules: ONE_STATIC,
            tags: &[],
            settled: false,
            new_description: Some("now described"),
            new_rules: None,
        },
        Row {
            case: "a description replaced and the rule set replaced entirely, once the \
                   creation has settled",
            family: ADDRESS_FAMILY_IPV6,
            created_description: Some("the first purpose"),
            created_rules: TWO_STATIC,
            tags: &[("Name", "replaced"), ("Team", "network")],
            settled: true,
            new_description: Some("the second purpose"),
            new_rules: Some(SCOPED_AND_POOLED),
        },
        Row {
            case: "a rule set given to a resolver created without one, its description left \
                   as it was",
            family: ADDRESS_FAMILY_IPV4,
            created_description: Some("kept"),
            created_rules: &[],
            tags: &[("Name", "kept")],
            settled: false,
            new_description: None,
            new_rules: Some(TWO_STATIC),
        },
        Row {
            case: "a rule set of two replaced by one",
            family: ADDRESS_FAMILY_IPV4,
            created_description: Some("shrinking"),
            created_rules: TWO_STATIC,
            tags: &[],
            settled: true,
            new_description: None,
            new_rules: Some(ONE_STATIC),
        },
        Row {
            case: "a modification that names neither member",
            family: ADDRESS_FAMILY_IPV6,
            created_description: Some("unchanged"),
            created_rules: ONE_STATIC,
            tags: &[("Name", "unchanged")],
            settled: true,
            new_description: None,
            new_rules: None,
        },
    ]
}

/// The rule: a modification applies the members the request names to the stored resolver, leaves
/// every other one as it was, answers the record the store then holds — in the transient
/// `modify-in-progress` state — the describe reads that back, the CLOCK moves it on to
/// `modify-complete`, and no other resolver of the account is touched.
#[tokio::test]
async fn modifying_a_resolver_writes_what_the_request_names_and_reaches_modify_complete() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = emulator();
        let ipam = create_ipam(case, &router).await;
        let resolver_id = create_resolver(
            case,
            &router,
            &ipam,
            row.family,
            row.created_description,
            row.tags,
            row.created_rules,
        )
        .await;
        // The control resolver every row keeps beside the one it modifies: each modification is
        // checked to touch only the resolver it names.
        let control_id = create_resolver(
            case,
            &router,
            &ipam,
            row.family,
            Some("the control resolver"),
            &[],
            ONE_STATIC,
        )
        .await;

        if row.settled {
            assert_eq!(
                described_state_once(case, &router, &resolver_id, IPAM_STATE_CREATE_COMPLETE)
                    .await,
                IPAM_STATE_CREATE_COMPLETE,
                "case {case:?}: the clock settles a created resolver before the modification"
            );
        }
        let (before, _) = describe_one(case, &router, &resolver_id).await;

        let mut params = vec![param("IpamPrefixListResolverId", &resolver_id)];
        if let Some(description) = row.new_description {
            params.push(param("Description", description));
        }
        if let Some(rules) = row.new_rules {
            params.extend(rule_params(&ipam, rules));
        }
        let body = ok(case, &router, MODIFY, &params).await;

        let (modified, state) = read_resolver(case, &body);
        assert_eq!(
            state.as_deref(),
            Some(IPAM_STATE_MODIFY_IN_PROGRESS),
            "case {case:?}: a modification is answered while it is still under way, got {body}"
        );
        // Exactly the members the request named have moved; everything else is the resolver the
        // store already held, because a modification reports a resource rather than rebuilding
        // one.
        let expected_description = row
            .new_description
            .map(str::to_string)
            .or_else(|| before.description.clone());
        let expected = ReportedResolver {
            description: expected_description.clone(),
            ..before.clone()
        };
        assert_eq!(
            modified, expected,
            "case {case:?}: expected only the named members to change, got {body}"
        );
        assert_eq!(
            modified.address_family.as_deref(),
            Some(row.family),
            "case {case:?}: no modification moves the family the resolver holds, got {body}"
        );

        // The rule set the request named REPLACES the held one entirely; a request naming none
        // leaves it.
        let wanted_rules = expected_rules(&ipam, row.new_rules.unwrap_or(row.created_rules));
        assert_eq!(
            stored_rules(&engine, &resolver_id),
            wanted_rules,
            "case {case:?}: expected the resolver to hold the rule set this request leaves it"
        );

        // The write the response reported is in the store, and the describe reads it back.
        assert_eq!(
            stored_state(&engine, &resolver_id).as_deref(),
            Some(IPAM_STATE_MODIFY_IN_PROGRESS),
            "case {case:?}: the store must hold the modification the response reported"
        );
        let (described, described_state) = describe_one(case, &router, &resolver_id).await;
        assert_eq!(
            described_state.as_deref(),
            Some(IPAM_STATE_MODIFY_IN_PROGRESS),
            "case {case:?}: the describe reports the modification a caller asked for"
        );
        assert_eq!(
            described, expected,
            "case {case:?}: the describe reports the same resolver the modification answered"
        );

        // The other resolver of the account kept both its state and its rules.
        assert!(
            !is_being_modified(&engine, &control_id),
            "case {case:?}: no other resolver of the account may be modified, the store holds \
             {state:?}",
            state = stored_state(&engine, &control_id)
        );
        assert_eq!(
            stored_rules(&engine, &control_id),
            expected_rules(&ipam, ONE_STATIC),
            "case {case:?}: no other resolver's rules may be replaced"
        );

        // The move the clock owed falls due on its own: the state is looked at again until it
        // holds, so nothing here depends on how fast the host runs.
        assert_eq!(
            described_state_once(case, &router, &resolver_id, IPAM_STATE_MODIFY_COMPLETE).await,
            IPAM_STATE_MODIFY_COMPLETE,
            "case {case:?}: the clock owed this resolver its terminal state"
        );
        // The modification settled; the record and the rules it left are still what the store
        // holds.
        assert_eq!(
            describe_one(case, &router, &resolver_id).await.0,
            expected,
            "case {case:?}: the settled resolver reports what the modification wrote"
        );
        assert_eq!(
            stored_rules(&engine, &resolver_id),
            wanted_rules,
            "case {case:?}: the settled resolver still holds the rule set it was given"
        );
    }
}

/// The rule: a modification is written against the resolver as it ACTUALLY stands, so a second
/// one replaces what the first left — the rules of the last modification are the rules the
/// resolver holds, and its description the last one named.
#[tokio::test]
async fn a_second_modification_replaces_what_the_first_left() {
    let case = "two modifications in a row";
    let (router, engine) = emulator();
    let ipam = create_ipam(case, &router).await;
    let resolver_id = create_resolver(
        case,
        &router,
        &ipam,
        ADDRESS_FAMILY_IPV4,
        Some("first"),
        &[],
        ONE_STATIC,
    )
    .await;

    for (step, (description, rules)) in [
        ("second", TWO_STATIC),
        ("third", SCOPED_AND_POOLED),
        ("fourth", ONE_STATIC),
    ]
    .into_iter()
    .enumerate()
    {
        let mut params = vec![
            param("IpamPrefixListResolverId", &resolver_id),
            param("Description", description),
        ];
        params.extend(rule_params(&ipam, rules));
        let body = ok(case, &router, MODIFY, &params).await;
        let (modified, state) = read_resolver(case, &body);
        assert_eq!(
            modified.description.as_deref(),
            Some(description),
            "case {case:?}: step {step} expected the description it named, got {body}"
        );
        assert_eq!(
            state.as_deref(),
            Some(IPAM_STATE_MODIFY_IN_PROGRESS),
            "case {case:?}: step {step} is answered while it is still under way, got {body}"
        );
        assert_eq!(
            stored_rules(&engine, &resolver_id),
            expected_rules(&ipam, rules),
            "case {case:?}: step {step} expected the rule set it named to replace the held one"
        );
    }

    assert_eq!(
        described_state_once(case, &router, &resolver_id, IPAM_STATE_MODIFY_COMPLETE).await,
        IPAM_STATE_MODIFY_COMPLETE,
        "case {case:?}: the clock owed the last modification its terminal state"
    );
}

// ---------------------------------------------------------------------------
// What the modification refuses
// ---------------------------------------------------------------------------

/// One row of the refusal table: the parameters beside the resolver's own id, and what EC2
/// answers them with.
struct RefusalRow {
    case: &'static str,
    /// The fields the request carries beside `IpamPrefixListResolverId`.
    extra: fn(&CreatedIpam) -> Vec<Param>,
    /// The id the request names, or `None` for the resolver the account holds.
    requested: Option<&'static str>,
    status: u16,
    code: &'static str,
}

/// The rule: a request the service cannot serve is refused with the code for its class — an id
/// the account holds no resolver for with this resource's own `NotFound`, a rule naming an IPAM
/// scope the account holds none of with the scope's own `NotFound`, and a value no documented
/// constraint admits with EC2's code for that class — and NOTHING is written: neither the
/// resolver's members, nor its rules, nor its state.
#[tokio::test]
async fn the_modification_refuses_what_it_cannot_write() {
    let rows: Vec<RefusalRow> = vec![
        RefusalRow {
            case: "a well-formed id the account holds no resolver for",
            extra: |_| Vec::new(),
            requested: Some(UNKNOWN_RESOLVER_ID),
            status: 400,
            code: INVALID_IPAM_PREFIX_LIST_RESOLVER_ID_NOT_FOUND,
        },
        RefusalRow {
            case: "an id of another resource kind",
            extra: |_| Vec::new(),
            requested: Some("ipam-0ffffffffffffffff"),
            status: 400,
            code: INVALID_IPAM_PREFIX_LIST_RESOLVER_ID_NOT_FOUND,
        },
        RefusalRow {
            case: "not an id at all",
            extra: |_| Vec::new(),
            requested: Some("my-resolver"),
            status: 400,
            code: INVALID_IPAM_PREFIX_LIST_RESOLVER_ID_NOT_FOUND,
        },
        RefusalRow {
            case: "a replacing rule naming a scope the account holds none of",
            extra: |_| {
                vec![
                    param("Rule.1.RuleType", RULE_TYPE_IPAM_POOL_CIDR),
                    param("Rule.1.IpamScopeId", UNKNOWN_SCOPE_ID),
                ]
            },
            requested: None,
            status: 400,
            code: INVALID_IPAM_SCOPE_ID_NOT_FOUND,
        },
        RefusalRow {
            case: "a replacing static CIDR that is not an address prefix",
            extra: |_| {
                vec![
                    param("Rule.1.RuleType", RULE_TYPE_STATIC_CIDR),
                    param("Rule.1.StaticCidr", "10.0.0.0/33"),
                ]
            },
            requested: None,
            status: 400,
            code: INVALID_PARAMETER_VALUE,
        },
        RefusalRow {
            case: "a replacing rule condition CIDR that is not an address prefix",
            extra: |_| {
                vec![
                    param("Rule.1.RuleType", RULE_TYPE_IPAM_RESOURCE_CIDR),
                    param("Rule.1.Condition.1.Operation", CONDITION_OPERATION),
                    param("Rule.1.Condition.1.Cidr", "not-a-cidr"),
                ]
            },
            requested: None,
            status: 400,
            code: INVALID_PARAMETER_VALUE,
        },
        RefusalRow {
            case: "more replacing rules than one resolver carries",
            extra: |_| {
                let mut params = Vec::new();
                for position in 1..=(MAX_IPAM_PREFIX_LIST_RESOLVER_RULES + 1) {
                    params.push(param(
                        &format!("Rule.{position}.RuleType"),
                        RULE_TYPE_STATIC_CIDR,
                    ));
                    params.push(param(&format!("Rule.{position}.StaticCidr"), "10.0.0.0/16"));
                }
                params
            },
            requested: None,
            status: 400,
            code: INVALID_PARAMETER_VALUE,
        },
        RefusalRow {
            case: "a rule type outside the modelled enum",
            extra: |_| vec![param("Rule.1.RuleType", "sometimes-cidr")],
            requested: None,
            status: 400,
            code: "ValidationError",
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, engine) = emulator();
        let ipam = create_ipam(case, &router).await;
        let resolver_id = create_resolver(
            case,
            &router,
            &ipam,
            ADDRESS_FAMILY_IPV4,
            Some("held"),
            &[("Name", "held")],
            ONE_STATIC,
        )
        .await;
        let (before, _) = describe_one(case, &router, &resolver_id).await;
        let rules_before = stored_rules(&engine, &resolver_id);

        let mut params = vec![param(
            "IpamPrefixListResolverId",
            row.requested.unwrap_or(resolver_id.as_str()),
        )];
        params.push(param("Description", "a description nothing may keep"));
        params.extend((row.extra)(&ipam));
        let (status, body) = invoke(&router, MODIFY, &params).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {expected}, got {status} with body {body}",
            expected = row.status
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: expected {expected}, got body {body}",
            expected = row.code
        );

        assert_eq!(
            describe_one(case, &router, &resolver_id).await.0,
            before,
            "case {case:?}: a refused modification writes none of the resolver's members"
        );
        assert_eq!(
            stored_rules(&engine, &resolver_id),
            rules_before,
            "case {case:?}: a refused modification replaces no rule"
        );
        assert!(
            !is_being_modified(&engine, &resolver_id),
            "case {case:?}: a refused modification must not have started one, the store holds \
             {state:?}",
            state = stored_state(&engine, &resolver_id)
        );
    }
}

/// The rule: an id the account holds no resolver for is refused with the quoted,
/// id-interpolated sentence this operation words that code with — the resource's modelled type,
/// then the id the REQUEST named — so a different id gives a different sentence.
#[tokio::test]
async fn an_unresolvable_id_is_refused_with_the_requested_id_in_the_sentence() {
    for (case, requested) in [
        (
            "a well-formed id the account holds no resolver for",
            UNKNOWN_RESOLVER_ID,
        ),
        (
            "a well-formed short-form id the account holds no resolver for",
            "ipam-plr-0fffffff",
        ),
        ("an id of another resource kind", "ipam-0ffffffffffffffff"),
        ("not an id at all", "my-resolver"),
    ] {
        let (router, _engine) = emulator();
        let (status, body) = invoke(
            &router,
            MODIFY,
            &[param("IpamPrefixListResolverId", requested)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_IPAM_PREFIX_LIST_RESOLVER_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_IPAM_PREFIX_LIST_RESOLVER_ID_NOT_FOUND}, got body \
             {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "The {RESOURCE_TYPE_IPAM_PREFIX_LIST_RESOLVER} ID '{requested}' does not exist"
            ),
            "case {case:?}: expected the id the request named in the sentence, got body {body}"
        );
    }
}

/// The rule: a resolver whose deletion has already been asked for has nothing left to modify, so
/// the modification is refused with `IncorrectState` naming the state the resolver is actually in
/// — while the deletion is under way and once the clock has finished it — and nothing is written.
#[tokio::test]
async fn a_resolver_on_its_way_out_is_refused_as_an_incorrect_state() {
    for (case, wait) in [
        ("while the deletion is still under way", false),
        ("once the clock has finished the deletion", true),
    ] {
        let (router, engine) = emulator();
        let ipam = create_ipam(case, &router).await;
        let resolver_id = create_resolver(
            case,
            &router,
            &ipam,
            ADDRESS_FAMILY_IPV4,
            Some("going"),
            &[],
            ONE_STATIC,
        )
        .await;
        ok(
            case,
            &router,
            DELETE,
            &[param("IpamPrefixListResolverId", &resolver_id)],
        )
        .await;

        // Once the clock has finished the deletion the state is terminal, so the sentence is
        // exact; while it is still under way the clock may finish it between the call and this
        // assertion, so either delete state's sentence is accepted.
        let expected_states: Vec<String> = if wait {
            vec![
                described_state_once(
                    case,
                    &router,
                    &resolver_id,
                    IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_COMPLETE,
                )
                .await,
            ]
        } else {
            vec![
                IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_IN_PROGRESS.to_string(),
                IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_COMPLETE.to_string(),
            ]
        };
        let wanted: Vec<String> = expected_states
            .iter()
            .map(|state| {
                format!(
                    "{IPAM_PREFIX_LIST_RESOLVER_REFUSAL_NAME} '{resolver_id}' is in the \
                     '{state}' state."
                )
            })
            .collect();

        let (status, body) = invoke(
            &router,
            MODIFY,
            &[
                param("IpamPrefixListResolverId", &resolver_id),
                param("Description", "a description a dead resolver cannot take"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INCORRECT_STATE,
            "case {case:?}: expected {INCORRECT_STATE}, got body {body}"
        );
        let message = error_message(&body);
        assert!(
            wanted.contains(&message),
            "case {case:?}: expected one of {wanted:?}, got {message:?}"
        );
        assert!(
            !is_being_modified(&engine, &resolver_id),
            "case {case:?}: a refused modification must not have moved the resolver, the store \
             holds {state:?}",
            state = stored_state(&engine, &resolver_id)
        );
    }
}

/// The rule: `DryRun` is a question about the CALLER, answered with `DryRunOperation` before
/// anything is resolved or written — so it answers the same whether or not the resolver the
/// request names exists, and the account's resolvers are left exactly as they were.
#[tokio::test]
async fn a_dry_run_answers_the_permission_question_and_writes_nothing() {
    for (case, requested) in [
        ("a resolver the account holds", None),
        (
            "an id the account holds no resolver for",
            Some(UNKNOWN_RESOLVER_ID),
        ),
    ] {
        let (router, engine) = emulator();
        let ipam = create_ipam(case, &router).await;
        let resolver_id = create_resolver(
            case,
            &router,
            &ipam,
            ADDRESS_FAMILY_IPV4,
            Some("before the probe"),
            &[],
            ONE_STATIC,
        )
        .await;
        let (before, _) = describe_one(case, &router, &resolver_id).await;

        let mut params = vec![
            param(
                "IpamPrefixListResolverId",
                requested.unwrap_or(resolver_id.as_str()),
            ),
            param("Description", "after the probe"),
            param("DryRun", "true"),
        ];
        params.extend(rule_params(&ipam, TWO_STATIC));
        let (status, body) = invoke(&router, MODIFY, &params).await;
        assert_eq!(
            status, DRY_RUN_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            DRY_RUN_OPERATION,
            "case {case:?}: expected {DRY_RUN_OPERATION}, got body {body}"
        );
        assert_eq!(
            describe_one(case, &router, &resolver_id).await.0,
            before,
            "case {case:?}: a dry run must write none of the resolver's members"
        );
        assert_eq!(
            stored_rules(&engine, &resolver_id),
            expected_rules(&ipam, ONE_STATIC),
            "case {case:?}: a dry run must replace no rule"
        );
        assert!(
            !is_being_modified(&engine, &resolver_id),
            "case {case:?}: a dry run must write nothing, the store holds {state:?}",
            state = stored_state(&engine, &resolver_id)
        );

        // The same request without the flag is what does the work, which is what makes the
        // answer above the permitted side of the probe.
        let mut params = vec![
            param("IpamPrefixListResolverId", &resolver_id),
            param("Description", "after the probe"),
        ];
        params.extend(rule_params(&ipam, TWO_STATIC));
        let body = ok(case, &router, MODIFY, &params).await;
        let (modified, state) = read_resolver(case, &body);
        assert_eq!(
            state.as_deref(),
            Some(IPAM_STATE_MODIFY_IN_PROGRESS),
            "case {case:?}: the modification the dry run stood for succeeds, got {body}"
        );
        assert_eq!(
            modified.description.as_deref(),
            Some("after the probe"),
            "case {case:?}: expected the description the request named, got {body}"
        );
        assert_eq!(
            stored_rules(&engine, &resolver_id),
            expected_rules(&ipam, TWO_STATIC),
            "case {case:?}: expected the rule set the request named"
        );
    }
}
