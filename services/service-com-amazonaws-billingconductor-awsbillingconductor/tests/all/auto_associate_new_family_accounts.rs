// SPDX-License-Identifier: CC0-1.0

//! Automatic account association, end to end over the service's real `restJson1` wire
//! through [`MultiServiceRouter::dispatch`].
//!
//! RULE under test: the billing group that holds `AccountGrouping.AutoAssociate` "will
//! automatically associate newly added Amazon Web Services accounts that join your
//! consolidated billing family" (`smithy_query(query="doc",shape="AccountGrouping",member="AutoAssociate")`),
//! so an account that joins the family while a group elects that flag becomes a member of
//! THAT group — reported against it by `ListAccountAssociations`, counted in its `Size`,
//! and a member every other rule of membership agrees on (the `MONITORED` and
//! billing-group filters, the one-account-one-group refusal, a `DisassociateAccounts`).
//! An account that was already in the family when the flag was elected is not newly added
//! and stays in no group; so does every joining account while NO group elects the flag;
//! and an account a caller has taken out of the electing group is not taken back.
//!
//! Which accounts a consolidated billing family holds is an AWS Organizations fact that no
//! request of this API writes, so a join is arranged the two ways this service learns a
//! roster: through the Organizations control plane mounted on the same engine (an account
//! recorded in that store, the way `tests/all/organization_linked_account_roster.rs`
//! arranges a family, since `CreateAccount` is still a stub there), and through the roster
//! this store records on its own (`tests/all/billing_family_membership.rs`'s
//! `record_billing_family`).
//!
//! The rows span what the rule generalises over: where the roster comes from (either
//! source), whether the group elects the flag (`true`, `false`, no flag at all), how many
//! accounts join at once (one, three), which operation elected it (`CreateBillingGroup`,
//! `UpdateBillingGroup`), and when the account joined relative to the election (before,
//! after, after it moved to another group, after it was turned off).

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::{
    ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The region every request is signed for; this service's resources are not regional, so
/// no assertion depends on it.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix the Organizations emulator routes on (that crate's
/// `OPERATIONS.md`, "Wire protocol").
const ORGANIZATIONS_TARGET_PREFIX: &str = "AWSOrganizationsV20161128";

/// The routes these rows drive, at the paths their modelled HTTP bindings name.
const CREATE_PLAN_PATH: &str = "/create-pricing-plan";
const CREATE_GROUP_PATH: &str = "/create-billing-group";
const UPDATE_GROUP_PATH: &str = "/update-billing-group";
const LIST_GROUPS_PATH: &str = "/list-billing-groups";
const LIST_ASSOCIATIONS_PATH: &str = "/list-account-associations";
const ASSOCIATE_PATH: &str = "/associate-accounts";
const DISASSOCIATE_PATH: &str = "/disassociate-accounts";

/// The two `Association` filter values that are not a billing group ARN, as
/// `ListAccountAssociationsFilter.Association` enumerates them.
const MONITORED: &str = "MONITORED";
const UNMONITORED: &str = "UNMONITORED";

/// The `ValidationExceptionReason` the model enumerates for an account another billing
/// group already holds.
const ACCOUNTS_ALREADY_ASSOCIATED: &str = "ACCOUNTS_ALREADY_ASSOCIATED";

/// Account ids: twelve digits, as `AccountId`'s modelled `^[0-9]{12}$` requires.
/// [`PRIOR`] is in the family BEFORE any group elects the flag; the others join after.
const PRIOR: &str = "111122223333";
const JOINED_A: &str = "222233334444";
const JOINED_B: &str = "333344445555";
const JOINED_C: &str = "444455556666";

/// Where the consolidated billing family's roster comes from — the two sources this
/// service answers membership from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Roster {
    /// The roster AWS Organizations records, read from that store on the shared engine.
    Organizations,
    /// The roster this service's own store records, beside the payer designation.
    Store,
}

/// A live emulator with the emulated account designated the organization's PAYER account —
/// Billing Conductor's control plane is authorized for the payer account alone, so this is
/// the world in which these calls are reachable.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    roster: Roster,
    /// The accounts the family holds so far, in join order.
    family: Vec<String>,
}

impl Emulator {
    async fn new(roster: Roster) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        engine
            .state::<BillingConductorState>()
            .lock()
            .payer_account_id = Some(::engine::emulator_account_id().to_string());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        if roster == Roster::Organizations {
            router.register_service(
                service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
                    Arc::clone(&engine),
                ),
            );
            // Mounting the Organizations control plane establishes an organization for the
            // emulated account, so that store is cleared and the organization these cases
            // answer for is created through its own `CreateOrganization`.
            *engine
                .state::<service_com_amazonaws_organizations_awsorganizationsv20161128::state::ComAmazonawsOrganizationsAwsorganizationsv20161128State>()
                .lock() = Default::default();
        }
        let emulator = Self {
            router,
            engine,
            roster,
            family: Vec::new(),
        };
        if roster == Roster::Organizations {
            let (status, created) = emulator.organizations("CreateOrganization", &json!({})).await;
            assert_eq!(
                status, 200,
                "setup CreateOrganization: expected 200, got {status} with {created}"
            );
        }
        emulator
    }

    /// POST a JSON body to one of Billing Conductor's routes and answer the status plus
    /// the parsed response body.
    async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!(
                "https://billingconductor.{REGION}.amazonaws.com{path}"
            ))
            .header("host", format!("billingconductor.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/billingconductor/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Call one operation of the Organizations control plane mounted beside this service,
    /// over THAT service's wire protocol (`awsJson1_1`, routed by `X-Amz-Target`).
    async fn organizations(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("organizations.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("{ORGANIZATIONS_TARGET_PREFIX}.{operation}"),
            )
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/organizations/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=00"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Put `accounts` IN the consolidated billing family, through whichever source this
    /// case's roster comes from — an account joins a family in AWS Organizations, so no
    /// request of this API can do it.
    fn join(&mut self, accounts: &[&str]) {
        for account in accounts {
            self.family.push((*account).to_string());
        }
        match self.roster {
            Roster::Organizations => {
                for account in accounts {
                    record_member_account(&self.engine, account);
                }
            }
            Roster::Store => {
                let roster: Vec<&str> = self.family.iter().map(String::as_str).collect();
                crate::billing_family_membership::record_billing_family(&self.engine, &roster);
            }
        }
    }

    /// `CreatePricingPlan`: the created plan's full ARN — a billing group's computation
    /// preference must name a plan that exists.
    async fn create_plan(&self, name: &str) -> String {
        let (status, body) = self.post(CREATE_PLAN_PATH, &json!({"Name": name})).await;
        assert_eq!(
            status, 200,
            "setup CreatePricingPlan {name}: expected 200, got {status} with {body}"
        );
        body["Arn"].as_str().unwrap_or_default().to_string()
    }

    /// `CreateBillingGroup` over no accounts, with `auto` carried in
    /// `AccountGrouping.AutoAssociate` when the case names one: the created group's ARN.
    async fn create_group(&self, name: &str, plan: &str, auto: Option<bool>) -> String {
        let mut grouping = json!({"LinkedAccountIds": []});
        if let Some(auto) = auto {
            grouping["AutoAssociate"] = json!(auto);
        }
        let (status, body) = self
            .post(
                CREATE_GROUP_PATH,
                &json!({
                    "Name": name,
                    "AccountGrouping": grouping,
                    "ComputationPreference": {"PricingPlanArn": plan},
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup CreateBillingGroup {name}: expected 200, got {status} with {body}"
        );
        body["Arn"].as_str().unwrap_or_default().to_string()
    }

    /// `UpdateBillingGroup`, turning `AccountGrouping.AutoAssociate` on or off.
    async fn set_auto_associate(&self, group_arn: &str, auto: bool) {
        let (status, body) = self
            .post(
                UPDATE_GROUP_PATH,
                &json!({"Arn": group_arn, "AccountGrouping": {"AutoAssociate": auto}}),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup UpdateBillingGroup AutoAssociate={auto}: expected 200, got {status} with \
             {body}"
        );
    }

    /// The `LinkedAccounts` page of a `ListAccountAssociations` read carrying `filters`, as
    /// `AccountId` -> `BillingGroupArn` — the group being `None` for an account in no
    /// billing group, and an absent key an account this service reports no element for.
    async fn list_associations(&self, filters: Value) -> BTreeMap<String, Option<String>> {
        let body = if filters.is_null() {
            json!({})
        } else {
            json!({"Filters": filters})
        };
        let (status, answered) = self.post(LIST_ASSOCIATIONS_PATH, &body).await;
        assert_eq!(
            status, 200,
            "ListAccountAssociations {body}: expected 200, got {status} with {answered}"
        );
        answered["LinkedAccounts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|element| {
                (
                    element["AccountId"].as_str().unwrap_or_default().to_string(),
                    element["BillingGroupArn"].as_str().map(str::to_string),
                )
            })
            .collect()
    }

    /// The billing group `ListAccountAssociations` reports `account_id` against, `None`
    /// when it reports the account with no group or reports no element for it.
    async fn association_of(&self, account_id: &str) -> Option<String> {
        self.list_associations(Value::Null)
            .await
            .get(account_id)
            .cloned()
            .flatten()
    }

    /// `Size` — "The number of accounts in the particular billing group" — as
    /// `ListBillingGroups` reports it, or -1 when the listing holds no such group.
    async fn group_size(&self, group_arn: &str) -> i64 {
        let (status, body) = self.post(LIST_GROUPS_PATH, &json!({})).await;
        assert_eq!(
            status, 200,
            "ListBillingGroups: expected 200, got {status} with {body}"
        );
        body["BillingGroups"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|group| group["Arn"].as_str() == Some(group_arn))
            .and_then(|group| group["Size"].as_i64())
            .unwrap_or(-1)
    }
}

/// Record `account_id` as a member account of the organization the emulated account is in.
///
/// `CreateAccount` and `InviteAccountToOrganization` are stubs in the Organizations
/// emulator, so no wire call adds a member account: the record is written into that
/// service's own store — the state its handlers answer from — and is DERIVED from the
/// management account's record, so nothing beside the id is invented here. The same seam
/// `tests/all/organization_linked_account_roster.rs` arranges its family through.
fn record_member_account(engine: &Arc<engine::InMemoryEngine>, account_id: &str) {
    let organizations =
        service_com_amazonaws_organizations_awsorganizationsv20161128::state::service_state(
            &**engine,
        )
        .expect("the Organizations store must be reachable on the shared engine");
    let mut store = organizations.lock();
    let management_id = store
        .organization
        .as_ref()
        .expect("the account must already be in an organization")
        .master_account_id
        .clone();
    let mut record = store
        .accounts
        .get(&management_id)
        .expect("the organization must hold its management account")
        .clone();
    record.arn = record.arn.replace(&management_id, account_id);
    record.id = account_id.to_string();
    store.accounts.insert(account_id.to_string(), record);
}

/// The wire error code of a failure response (`__type`), without any namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// One family that gains accounts while a group may or may not elect the flag.
struct Row {
    case: &'static str,
    /// Where the family's roster comes from.
    roster: Roster,
    /// `AccountGrouping.AutoAssociate` as the create carries it, `None` for a create that
    /// names no flag at all.
    auto: Option<bool>,
    /// The accounts that join the family AFTER the group was created.
    joining: &'static [&'static str],
    /// Whether the electing group takes them in.
    associated: bool,
}

/// The rows: the roster's source, the flag the group carries, and how many accounts join.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one account joins, the group elects the flag, organization roster",
            roster: Roster::Organizations,
            auto: Some(true),
            joining: &[JOINED_A],
            associated: true,
        },
        Row {
            case: "one account joins, the group elects the flag, store-recorded roster",
            roster: Roster::Store,
            auto: Some(true),
            joining: &[JOINED_A],
            associated: true,
        },
        Row {
            case: "three accounts join, the group elects the flag, organization roster",
            roster: Roster::Organizations,
            auto: Some(true),
            joining: &[JOINED_A, JOINED_B, JOINED_C],
            associated: true,
        },
        Row {
            case: "three accounts join, the group elects the flag, store-recorded roster",
            roster: Roster::Store,
            auto: Some(true),
            joining: &[JOINED_A, JOINED_B, JOINED_C],
            associated: true,
        },
        Row {
            case: "the group declines the flag, organization roster",
            roster: Roster::Organizations,
            auto: Some(false),
            joining: &[JOINED_A],
            associated: false,
        },
        Row {
            case: "the group declines the flag, store-recorded roster",
            roster: Roster::Store,
            auto: Some(false),
            joining: &[JOINED_A, JOINED_B],
            associated: false,
        },
        Row {
            case: "the group names no flag at all, organization roster",
            roster: Roster::Organizations,
            auto: None,
            joining: &[JOINED_A],
            associated: false,
        },
        Row {
            case: "the group names no flag at all, store-recorded roster",
            roster: Roster::Store,
            auto: None,
            joining: &[JOINED_A, JOINED_B],
            associated: false,
        },
    ]
}

/// RULE: an account that joins the consolidated billing family while a billing group
/// elects `AccountGrouping.AutoAssociate` is associated with that group and counted in its
/// `Size`; one that joins while no group elects it is in no billing group, and an account
/// that was already in the family when the group was created is not newly added either.
#[tokio::test]
async fn a_group_electing_auto_associate_takes_in_the_accounts_that_join_the_family() {
    for row in rows() {
        let mut em = Emulator::new(row.roster).await;
        em.join(&[PRIOR]);
        let plan = em.create_plan("AutoPlan").await;
        let group = em.create_group("AutoGroup", &plan, row.auto).await;

        em.join(row.joining);

        let associations = em.list_associations(Value::Null).await;
        let expected = row.associated.then(|| group.clone());
        for account in row.joining {
            assert!(
                associations.contains_key(*account),
                "case {:?}: expected the joined account {account} to be reported as a linked \
                 account, got {associations:?}",
                row.case
            );
            assert_eq!(
                associations.get(*account).cloned().flatten(),
                expected,
                "case {:?}: expected the joined account {account} to be reported against \
                 {expected:?}, got {associations:?}",
                row.case
            );
        }
        assert_eq!(
            associations.get(PRIOR).cloned().flatten(),
            None,
            "case {:?}: an account already in the family when the group was created is not a \
             newly added account, got {associations:?}",
            row.case
        );
        let expected_size = if row.associated {
            i64::try_from(row.joining.len()).unwrap_or(-1)
        } else {
            0
        };
        assert_eq!(
            em.group_size(&group).await,
            expected_size,
            "case {:?}: expected Size {expected_size} for the group",
            row.case
        );
    }
}

/// RULE: an automatically associated account is a member every rule of membership agrees
/// on — the `MONITORED` and billing-group `Association` filters select it, the
/// one-account-one-group refusal protects it, and `DisassociateAccounts` takes it out.
#[tokio::test]
async fn an_automatically_associated_account_is_a_member_every_reader_agrees_on() {
    for roster in [Roster::Organizations, Roster::Store] {
        let case = format!("{roster:?} roster");
        let mut em = Emulator::new(roster).await;
        em.join(&[PRIOR]);
        let plan = em.create_plan("AutoPlan").await;
        let auto_group = em.create_group("AutoGroup", &plan, Some(true)).await;
        let other_group = em.create_group("OtherGroup", &plan, None).await;

        em.join(&[JOINED_A]);

        let monitored = em.list_associations(json!({"Association": MONITORED})).await;
        assert_eq!(
            monitored.get(JOINED_A).cloned().flatten(),
            Some(auto_group.clone()),
            "case {case:?}: the MONITORED filter must select the automatic member, got \
             {monitored:?}"
        );
        let unmonitored = em
            .list_associations(json!({"Association": UNMONITORED}))
            .await;
        assert!(
            !unmonitored.contains_key(JOINED_A),
            "case {case:?}: the UNMONITORED filter must not select the automatic member, got \
             {unmonitored:?}"
        );
        let by_group = em
            .list_associations(json!({"Association": auto_group.clone()}))
            .await;
        assert_eq!(
            by_group.keys().cloned().collect::<Vec<String>>(),
            vec![JOINED_A.to_string()],
            "case {case:?}: the billing-group filter must select exactly the automatic member, \
             got {by_group:?}"
        );

        // "not already associated with another billing group": the automatic membership is
        // the same membership an `AssociateAccounts` would have made, so a second group
        // asking for the account is refused over it.
        let (status, refused) = em
            .post(
                ASSOCIATE_PATH,
                &json!({"Arn": other_group, "AccountIds": [JOINED_A]}),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for an account the electing group holds, got \
             {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {refused}"
        );
        assert_eq!(
            refused["Reason"].as_str(),
            Some(ACCOUNTS_ALREADY_ASSOCIATED),
            "case {case:?}: expected the {ACCOUNTS_ALREADY_ASSOCIATED} reason, got {refused}"
        );
        let expected_message =
            format!("account {JOINED_A} is already associated with billing group {auto_group}");
        assert_eq!(
            refused["Message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the message {expected_message:?}, got {refused}"
        );

        // The automatic member is in the group's own account list too, so the request that
        // takes a member out finds it there.
        let (status, body) = em
            .post(
                DISASSOCIATE_PATH,
                &json!({"Arn": auto_group, "AccountIds": [JOINED_A]}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DisassociateAccounts to take the automatic member out, got \
             {status} with {body}"
        );
        assert_eq!(
            em.association_of(JOINED_A).await,
            None,
            "case {case:?}: expected the disassociated account to be in no billing group"
        );
        assert_eq!(
            em.group_size(&auto_group).await,
            0,
            "case {case:?}: expected the group's Size to drop with the membership"
        );
    }
}

/// RULE: the association is owed to an account that JOINS the family, so a release is not
/// undone — an account the caller took out of the electing group stays out however many
/// times the family is read — while an account that joins afterwards is still taken in.
#[tokio::test]
async fn a_released_account_is_not_taken_back_but_a_later_join_is() {
    for roster in [Roster::Organizations, Roster::Store] {
        let case = format!("{roster:?} roster");
        let mut em = Emulator::new(roster).await;
        em.join(&[PRIOR]);
        let plan = em.create_plan("AutoPlan").await;
        let group = em.create_group("AutoGroup", &plan, Some(true)).await;

        em.join(&[JOINED_A]);
        assert_eq!(
            em.association_of(JOINED_A).await,
            Some(group.clone()),
            "case {case:?}: expected the joined account to be taken in"
        );

        let (status, body) = em
            .post(
                DISASSOCIATE_PATH,
                &json!({"Arn": group, "AccountIds": [JOINED_A]}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DisassociateAccounts to be served, got {status} with {body}"
        );
        // Read it back twice: the account stays out however often the family is looked at.
        for read in 1..=2 {
            assert_eq!(
                em.association_of(JOINED_A).await,
                None,
                "case {case:?}: read {read} must leave the released account in no billing group"
            );
        }
        assert_eq!(
            em.group_size(&group).await,
            0,
            "case {case:?}: expected the released account to be out of the group's Size"
        );

        em.join(&[JOINED_B]);
        assert_eq!(
            em.association_of(JOINED_B).await,
            Some(group.clone()),
            "case {case:?}: expected an account joining after the release to be taken in"
        );
        assert_eq!(
            em.group_size(&group).await,
            1,
            "case {case:?}: expected the group to count only the account that joined last"
        );
    }
}

/// RULE: the group that takes a joining account in is the one electing the flag WHEN the
/// account joins, whichever operation elected it — so an `UpdateBillingGroup` that moves
/// the election moves where the next joining account goes, and one that turns it off leaves
/// the next one in no group at all.
#[tokio::test]
async fn the_election_moves_with_the_flag_and_the_accounts_that_join_follow_it() {
    for roster in [Roster::Organizations, Roster::Store] {
        let case = format!("{roster:?} roster");
        let mut em = Emulator::new(roster).await;
        em.join(&[PRIOR]);
        let plan = em.create_plan("AutoPlan").await;
        let first = em.create_group("FirstGroup", &plan, Some(true)).await;
        let second = em.create_group("SecondGroup", &plan, None).await;

        em.join(&[JOINED_A]);
        assert_eq!(
            em.association_of(JOINED_A).await,
            Some(first.clone()),
            "case {case:?}: expected the group electing the flag at create time to take the \
             account in"
        );

        // The election hands over: the holder gives the flag up and the other group takes
        // it, which is the only order the state-wide cap on holders admits.
        em.set_auto_associate(&first, false).await;
        em.set_auto_associate(&second, true).await;

        em.join(&[JOINED_B]);
        assert_eq!(
            em.association_of(JOINED_B).await,
            Some(second.clone()),
            "case {case:?}: expected the group electing the flag now to take the next account in"
        );
        assert_eq!(
            em.association_of(JOINED_A).await,
            Some(first.clone()),
            "case {case:?}: expected the earlier member to stay in the group that took it"
        );

        // Nobody elects it any more: an account that joins now is in no billing group.
        em.set_auto_associate(&second, false).await;
        em.join(&[JOINED_C]);
        assert_eq!(
            em.association_of(JOINED_C).await,
            None,
            "case {case:?}: expected an account joining while no group elects the flag to be in \
             no billing group"
        );
        assert_eq!(
            em.group_size(&first).await,
            1,
            "case {case:?}: expected the first group to hold the account it took"
        );
        assert_eq!(
            em.group_size(&second).await,
            1,
            "case {case:?}: expected the second group to hold the account it took"
        );
    }
}
