// SPDX-License-Identifier: CC0-1.0

//! The EC2 instance a user association names is resolved against the instances the
//! ACCOUNT has, asked of the service that owns instances.
//!
//! The rule under test: an operation of this service that acts on a caller-named EC2
//! instance — `AssociateUser`, `ListUserAssociations` and `DisassociateUser`, the three
//! whose input carries an `InstanceId` (`smithy_query(query="operation",
//! operation="AssociateUser,ListUserAssociations,DisassociateUser")`) — authorizes the
//! request exactly when the account has an instance of that id. `InstanceId` is a plain
//! `String` in this
//! service's model and the model declares no set of ids, so an instance a client launched
//! with `ec2:RunInstances` is a target the whole user-association plane accepts, and only
//! an id no instance of the account has is refused.
//!
//! An instance is EC2's resource, so EC2 is mounted beside this service on ONE engine and
//! driven through the AWS SDK over the same in-process callback this service's own
//! resolution reaches it through. Nothing here reaches into either store: an instance is
//! believed to exist only because EC2 answered for it, and an association only because a
//! later read of this service reported it.
//!
//! The rows span what the rule generalises over: where the instance came from (a launch of
//! one, one instance of a batch, a later launch, this service's own fleet seam, an instance
//! registered in that seam at runtime), the state EC2 reports it in (`pending`, `running`,
//! `terminated`), and the form, size and case of an id no instance of the account has.
//!
//! An id the account has NO instance of is derived so that it cannot BE one it has — EC2
//! allocates an identifier unpredictably, so a near miss is a changed digit rather than a
//! written one ([`near_miss_on`]) — and that premise is established by asking the owner of
//! instances ([`Fleet::has_instance`]) rather than assumed of a drawn value.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use service_com_amazonaws_licensemanagerusersubscriptions_licensemanagerusersubscriptions::state::seed_account_instances;
use service_com_amazonaws_licensemanagerusersubscriptions_licensemanagerusersubscriptions::ComAmazonawsLicensemanagerusersubscriptionsLicensemanagerusersubscriptionsState as ServiceState;
use service_com_amazonaws_licensemanagerusersubscriptions_licensemanagerusersubscriptions::EmulatedService;

// ONE wire harness per service: the dispatch plumbing, the directory fleet and the reply
// reader come from the sibling suite rather than a second copy of them here.
use crate::identity_provider_subscriptions::{
    as_str, call, create_directories, create_network, provider_union, register, Harness, Reply,
    ACCOUNT_SECURITY_GROUPS, ACCOUNT_SUBNETS, ACCOUNT_VPC, PRODUCTS, REGION,
};

/// The directory the registration these associations are made through is for, put in the
/// account's directory fleet by the harness below.
const DIRECTORY: &str = "d-0123456789";

/// The AMI a launch names. Nothing here asserts anything about the image.
const AMI: &str = "ami-00112233445566778";

/// How long EC2 is given to bring an instance to a state its own clock owes it — a launch
/// to `running`, a termination to `terminated`. Nothing in the emulator sleeps; this is the
/// test waiting for that clock.
///
/// GENEROUS, by orders of magnitude, against the span that move is owed at: the wait below
/// ends on the CONDITION, so nothing is gained by bounding it tightly, and a budget close to
/// that span would make the outcome depend on how fast the host happens to be running.
const EC2_BUDGET: Duration = Duration::from_secs(60);

/// The `InstanceState$Name` values this suite names: the state a launch starts in, the one
/// it reaches, and the terminal one a termination reaches.
const STATE_PENDING: &str = "pending";
const STATE_RUNNING: &str = "running";
const STATE_TERMINATED: &str = "terminated";

/// The status a fresh association carries, and the one it carries once its end was
/// accepted. Spelled out here rather than read from the production constants, so a change
/// to them fails a test instead of silently changing what this suite accepts.
const STATUS_ASSOCIATING: &str = "ASSOCIATING";
const STATUS_DISASSOCIATING: &str = "DISASSOCIATING";

/// The sentence this service fails closed with
/// (`scaffolded::UNAUTHORIZED_EXCEPTION_MESSAGE`), the code it carries and the status it is
/// reported with (`scaffolded::UNAUTHORIZED_EXCEPTION_STATUS`), spelled out for the same
/// reason.
const UNAUTHORIZED_CODE: &str = "UnauthorizedException";
const UNAUTHORIZED_MESSAGE: &str =
    "Unauthorized. Please check the request and access policy and try again.";
const REFUSAL_STATUS: u16 = 401;

/// Every operation that acts on the EC2 instance its request names, as the route a client
/// reaches it at. Each one's request is addressed by the same
/// `(Username, InstanceId, IdentityProvider)` members, so one body drives all three.
const GATED_PATHS: &[(&str, &str)] = &[
    ("AssociateUser", "/user/AssociateUser"),
    ("ListUserAssociations", "/user/ListUserAssociations"),
    ("DisassociateUser", "/user/DisassociateUser"),
];

/// An id that differs from `instance_id` in its LAST digit only: a near miss on it, whatever
/// digit it ends in.
///
/// EC2 allocates an instance identifier UNPREDICTABLY — a launch mints `i-` and 17
/// hexadecimal digits of a drawn identifier — so the last digit of an id it handed out is not
/// a value the caller knows. A derivation that WRITES one fixed digit there therefore names
/// the instance itself whenever that is the digit the id already carries, which is an id the
/// account HAS rather than one it has none of. The replacement is chosen against the digit
/// found, and is another lowercase hexadecimal digit, so the near miss keeps the form and the
/// length of the id it was derived from.
pub(crate) fn near_miss_on(instance_id: &str) -> String {
    // The precondition, stated where the four suites that share this helper pass through:
    // an instance id is a non-empty ASCII value, so its last digit is its last byte.
    assert!(
        !instance_id.is_empty() && instance_id.is_ascii(),
        "an instance id is a non-empty ASCII value, got {instance_id:?}"
    );
    let (head, last) = instance_id.split_at(instance_id.len() - 1);
    let replacement = if last == "0" { "1" } else { "0" };
    let near_miss = format!("{head}{replacement}");
    assert_ne!(
        near_miss, instance_id,
        "a near miss must differ from the id it was derived from"
    );
    near_miss
}

/// This service and EC2 — the owner of the instances — over ONE shared engine, plus an EC2
/// client over the router's own in-process callback: the very seam this service's instance
/// resolution reaches EC2 through.
pub(crate) struct Fleet {
    pub(crate) h: Harness,
    pub(crate) ec2: ::aws_sdk_ec2::Client,
}

impl Fleet {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The tests sign nothing; authentication is the framework's concern, not this
        // service's.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
            Arc::clone(&engine),
        ));
        // `RegisterIdentityProvider` resolves a `DirectoryId` against the account's
        // directories, so the directory the registration names is in that fleet first.
        create_directories(&engine, &[DIRECTORY]);
        // And it resolves the `Settings` of that registration against what the account has
        // in Amazon EC2, so the network [`register_provider`] names is created there first.
        create_network(
            &engine,
            ACCOUNT_VPC,
            ACCOUNT_SUBNETS,
            ACCOUNT_SECURITY_GROUPS,
        );
        let callback = EmbeddedEmulator::from_router(router.clone());
        let config = ::aws_sdk_ec2::Config::builder()
            .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
            .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
            .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        Self {
            h: Harness { router, engine },
            ec2: ::aws_sdk_ec2::Client::from_conf(config),
        }
    }

    /// Launch `count` instances through EC2 itself and answer the ids it minted, in launch
    /// order.
    pub(crate) async fn launch(&self, count: i32) -> Vec<String> {
        let launched = self
            .ec2
            .run_instances()
            .image_id(AMI)
            .min_count(count)
            .max_count(count)
            .send()
            .await
            .unwrap_or_else(|error| panic!("setup: EC2 must launch {count} instances: {error:?}"));
        let ids: Vec<String> = launched
            .instances()
            .iter()
            .filter_map(|instance| instance.instance_id().map(str::to_string))
            .collect();
        assert_eq!(
            ids.len(),
            usize::try_from(count).expect("a non-negative count"),
            "setup: expected {count} launched instance ids, got {ids:?}"
        );
        ids
    }

    /// Terminate `instance_id` through EC2 itself.
    async fn terminate(&self, instance_id: &str) {
        self.ec2
            .terminate_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("setup: EC2 must terminate {instance_id}: {error:?}")
            });
    }

    /// The `InstanceState$Name` EC2 reports for `instance_id`.
    pub(crate) async fn instance_state(&self, instance_id: &str) -> String {
        let described = self
            .ec2
            .describe_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .unwrap_or_else(|error| panic!("EC2 must describe {instance_id}: {error:?}"));
        described
            .reservations()
            .iter()
            .flat_map(::aws_sdk_ec2::types::Reservation::instances)
            .find(|instance| instance.instance_id() == Some(instance_id))
            .and_then(|instance| instance.state()?.name().map(|name| name.as_str().to_string()))
            .unwrap_or_else(|| panic!("EC2 must report a state for {instance_id}"))
    }

    /// Whether the account has an instance of `instance_id`, as the service that OWNS
    /// instances answers — in any state it reports one in, the terminal `terminated`
    /// included, since EC2 goes on reporting a terminated instance for a while.
    ///
    /// How a row that must name an id the account has NO instance of establishes that
    /// premise instead of assuming it of a value EC2 drew. A describe of an id EC2 was
    /// allocated nothing for fails as a whole, which is the same fact asked for here: the
    /// account has no such instance.
    pub(crate) async fn has_instance(&self, instance_id: &str) -> bool {
        self.ec2
            .describe_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .is_ok_and(|described| {
                described
                    .reservations()
                    .iter()
                    .flat_map(::aws_sdk_ec2::types::Reservation::instances)
                    .any(|instance| instance.instance_id() == Some(instance_id))
            })
    }

    /// Wait until EC2 reports `instance_id` in `state`, and answer it.
    pub(crate) async fn settled_in(&self, instance_id: &str, state: &str) -> String {
        let deadline = Instant::now() + EC2_BUDGET;
        loop {
            let reported = self.instance_state(instance_id).await;
            if reported == state {
                return reported;
            }
            assert!(
                Instant::now() < deadline,
                "setup: {instance_id} must reach {state}, EC2 reports {reported}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// One `(case, instance id)` row the rule is asserted on, with the state EC2 reports the
/// instance in when EC2 is the one that has it.
#[derive(Debug)]
struct Row {
    case: String,
    instance_id: String,
}

pub(crate) async fn associate(h: &Harness, username: &str, instance_id: &str) -> Reply {
    call(
        h,
        "/user/AssociateUser",
        &json!({
            "Username": username,
            "InstanceId": instance_id,
            "IdentityProvider": provider_union(DIRECTORY),
        }),
    )
    .await
}

pub(crate) async fn list_associations(h: &Harness, instance_id: &str) -> Reply {
    call(
        h,
        "/user/ListUserAssociations",
        &json!({
            "InstanceId": instance_id,
            "IdentityProvider": provider_union(DIRECTORY),
        }),
    )
    .await
}

async fn disassociate(h: &Harness, username: &str, instance_id: &str) -> Reply {
    call(
        h,
        "/user/DisassociateUser",
        &json!({
            "Username": username,
            "InstanceId": instance_id,
            "IdentityProvider": provider_union(DIRECTORY),
        }),
    )
    .await
}

async fn list_instances(h: &Harness) -> Reply {
    call(h, "/instance/ListInstances", &json!({})).await
}

/// The registration the user-association plane resolves a request through, so a call that
/// is authorized is answered rather than refused for an unregistered provider.
pub(crate) async fn register_provider(h: &Harness) {
    register(
        h,
        &json!({
            "IdentityProvider": provider_union(DIRECTORY),
            "Product": PRODUCTS[0],
            "Settings": {
                "Subnets": [ACCOUNT_SUBNETS[0]],
                "SecurityGroupId": ACCOUNT_SECURITY_GROUPS[0],
            },
        }),
    )
    .await
    .ok("the registration the associations are made through");
}

/// RULE: every instance the ACCOUNT has is a target the user-association plane serves —
/// the association is minted in `ASSOCIATING` on that instance, a later read reports it,
/// the instance enters this service's own view, and the association can be ended — whether
/// the instance came from EC2 (one launch, a batch, a later launch, before or after it
/// reached `running`) or from this service's own fleet seam.
#[tokio::test]
async fn every_instance_the_account_has_carries_the_user_association_plane() {
    let fleet = Fleet::new();
    register_provider(&fleet.h).await;

    let mut rows: Vec<Row> = Vec::new();

    // An instance launched on its own, used straight away: EC2 reports it `pending`, which
    // is an instance the account has.
    let single = fleet.launch(1).await;
    assert_eq!(
        fleet.instance_state(&single[0]).await,
        STATE_PENDING,
        "setup: a just-launched instance must be {STATE_PENDING}"
    );
    rows.push(Row {
        case: format!("an instance launched on its own, still {STATE_PENDING}"),
        instance_id: single[0].clone(),
    });

    // One instance of a batch, once EC2's own clock has brought it to `running`.
    let batch = fleet.launch(3).await;
    let running = batch[1].clone();
    fleet.settled_in(&running, STATE_RUNNING).await;
    rows.push(Row {
        case: format!("one instance of a batch of three, {STATE_RUNNING}"),
        instance_id: running,
    });
    rows.push(Row {
        case: "another instance of the same batch".to_string(),
        instance_id: batch[2].clone(),
    });

    // A second, later launch: the ids a launch mints are not the only ones that resolve.
    let later = fleet.launch(1).await;
    rows.push(Row {
        case: "an instance of a second, later launch".to_string(),
        instance_id: later[0].clone(),
    });

    // This service's own fleet seam, which answers in a process with no EC2 beside it: an
    // instance EC2 never launched, so the two halves of the resolution are both covered.
    let seeded = seed_account_instances();
    rows.push(Row {
        case: "an instance of this service's own fleet seam".to_string(),
        instance_id: seeded[0].0.clone(),
    });
    rows.push(Row {
        case: "another instance of that seam".to_string(),
        instance_id: seeded[2].0.clone(),
    });

    // And one registered in that seam at runtime, with an id of EC2's 8-hex-digit form, so
    // the rows differ in id SIZE as well as in origin.
    let registered = "i-0abc1234";
    {
        let store = fleet.h.engine.state::<ServiceState>();
        store
            .lock()
            .register_account_instance(registered, format!("{REGION}a"));
    }
    rows.push(Row {
        case: "an instance registered in that seam at runtime".to_string(),
        instance_id: registered.to_string(),
    });

    for (index, row) in rows.iter().enumerate() {
        let case = &row.case;
        let username = format!("user{index}");

        // The association is minted on the instance the request named.
        let reply = associate(&fleet.h, &username, &row.instance_id).await;
        let body = reply.ok(case);
        let summary = body
            .get("InstanceUserSummary")
            .unwrap_or_else(|| panic!("case {case:?}: no InstanceUserSummary in {body:?}"));
        assert_eq!(
            as_str(summary, "InstanceId", case),
            row.instance_id,
            "case {case:?}: expected the instance the request named, got {summary:?}"
        );
        assert_eq!(
            as_str(summary, "Status", case),
            STATUS_ASSOCIATING,
            "case {case:?}: expected {STATUS_ASSOCIATING}, got {summary:?}"
        );
        assert_eq!(
            as_str(summary, "Username", case),
            username,
            "case {case:?}: expected the user the request named, got {summary:?}"
        );

        // And a later read of this service reports it: the association was persisted, not
        // only answered.
        let listed = list_associations(&fleet.h, &row.instance_id).await;
        let body = listed.ok(case);
        let summaries = body
            .get("InstanceUserSummaries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no InstanceUserSummaries in {body:?}"));
        assert!(
            summaries
                .iter()
                .any(|entry| entry.get("Username").and_then(Value::as_str) == Some(&username)),
            "case {case:?}: expected {username} among the associations of {}, got {summaries:?}",
            row.instance_id
        );

        // The association is also what brings the instance into this service's own view.
        let instances = list_instances(&fleet.h).await;
        let body = instances.ok(case);
        let reported = body
            .get("InstanceSummaries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no InstanceSummaries in {body:?}"));
        assert!(
            reported.iter().any(|entry| entry
                .get("InstanceId")
                .and_then(Value::as_str)
                == Some(row.instance_id.as_str())),
            "case {case:?}: expected {} among the instances this service reports, got \
             {reported:?}",
            row.instance_id
        );

        // The whole plane, not just its entry point: the association can be ended again.
        let ended = disassociate(&fleet.h, &username, &row.instance_id).await;
        let body = ended.ok(case);
        let summary = body
            .get("InstanceUserSummary")
            .unwrap_or_else(|| panic!("case {case:?}: no InstanceUserSummary in {body:?}"));
        assert_eq!(
            as_str(summary, "Status", case),
            STATUS_DISASSOCIATING,
            "case {case:?}: expected {STATUS_DISASSOCIATING}, got {summary:?}"
        );
    }
}

/// RULE, the other side: an id no instance of the account has is refused, with the same
/// code, status and sentence on every operation that acts on a caller-named instance — and
/// an instance EC2 has TERMINATED is no longer one the account has.
#[tokio::test]
async fn an_id_no_instance_of_the_account_has_is_refused_on_every_gated_operation() {
    let fleet = Fleet::new();
    register_provider(&fleet.h).await;

    let launched = fleet.launch(1).await;
    let launched = launched[0].clone();
    // An instance of the account, terminated through EC2 itself: EC2 goes on reporting it
    // for a while, in the terminal state, and it runs nothing a subscription can be served
    // from.
    let terminated = fleet.launch(1).await;
    let terminated = terminated[0].clone();
    fleet.terminate(&terminated).await;
    fleet.settled_in(&terminated, STATE_TERMINATED).await;

    let seeded = seed_account_instances();
    let held = seeded[0].0.clone();
    let rows: Vec<Row> = vec![
        Row {
            case: "an id of EC2's 17-hex-digit form that no launch minted".to_string(),
            instance_id: "i-0fffffffffffffff0".to_string(),
        },
        Row {
            case: "an id of EC2's 8-hex-digit form that no launch minted".to_string(),
            instance_id: "i-0fffffff".to_string(),
        },
        Row {
            case: "a near miss on a launched instance".to_string(),
            instance_id: near_miss_on(&launched),
        },
        Row {
            case: "an upper-case spelling of a launched instance".to_string(),
            instance_id: launched.to_ascii_uppercase(),
        },
        Row {
            case: "an upper-case spelling of an instance of the fleet seam".to_string(),
            instance_id: held.to_ascii_uppercase(),
        },
        Row {
            case: "a value that is not an instance id at all".to_string(),
            instance_id: "not-an-instance".to_string(),
        },
        Row {
            case: format!("an instance EC2 reports {STATE_TERMINATED}"),
            instance_id: terminated.clone(),
        },
    ];

    // THE PREMISE of the rows above, established by ASKING the service that owns instances
    // rather than assumed of ids it allocated: the account has no instance of any of them.
    // The terminated row is the one exception — EC2 goes on reporting that instance in its
    // terminal state, which is exactly what makes it an instance the account no longer has —
    // so it is checked against the state EC2 reports it in instead. The other place an
    // instance of this account can be reported from is this service's own fleet seam, which
    // holds the seeded ids these rows only spell differently.
    for row in &rows {
        let case = &row.case;
        if row.instance_id == terminated {
            assert_eq!(
                fleet.instance_state(&row.instance_id).await,
                STATE_TERMINATED,
                "setup: case {case:?}: EC2 must report {} {STATE_TERMINATED}",
                row.instance_id
            );
            continue;
        }
        assert!(
            !fleet.has_instance(&row.instance_id).await,
            "setup: case {case:?}: the account must have no instance of {}",
            row.instance_id
        );
    }

    for row in &rows {
        for (operation, path) in GATED_PATHS {
            let case = format!("{operation}: {}", row.case);
            let reply = call(
                &fleet.h,
                path,
                &json!({
                    "Username": "someone",
                    "InstanceId": row.instance_id,
                    "IdentityProvider": provider_union(DIRECTORY),
                }),
            )
            .await;
            assert_eq!(
                reply.error_type.as_deref(),
                Some(UNAUTHORIZED_CODE),
                "case {case:?}: expected {UNAUTHORIZED_CODE}, got {reply:?}"
            );
            assert_eq!(
                reply.status, REFUSAL_STATUS,
                "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {reply:?}"
            );
            assert_eq!(
                reply.message(),
                Some(UNAUTHORIZED_MESSAGE),
                "case {case:?}: wrong refusal sentence in {reply:?}"
            );
        }
    }

    // The control: the instance the near-miss and upper-case rows were derived from is
    // itself served, so those rows are refused for the id they name and not because the
    // plane refuses everything.
    let reply = associate(&fleet.h, "someone", &launched).await;
    let body = reply.ok("the launched instance the refused rows were derived from");
    let summary = body
        .get("InstanceUserSummary")
        .unwrap_or_else(|| panic!("no InstanceUserSummary in {body:?}"));
    assert_eq!(
        as_str(summary, "InstanceId", "the launched instance"),
        launched,
        "the launched instance must be served, got {summary:?}"
    );
}

/// Every digit an EC2 instance id can end in: the lowercase hexadecimal alphabet the
/// identifier of an id is printed in (`i-` and 17 hexadecimal digits), so the table below
/// spans every id a launch can hand out rather than the digits one run happened to draw.
const ID_LAST_DIGITS: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// An id of EC2's 17-hex-digit form ending in `digit`, and carrying `digit` as its second
/// digit as well, so no row's near miss can be another row's instance.
fn instance_id_ending_in(digit: char) -> String {
    let instance_id = format!("i-0{digit}a1b2c3d4e5f6a7{digit}");
    assert_eq!(
        instance_id.len(),
        "i-".len() + 17,
        "an id of EC2's 17-hex-digit form, got {instance_id:?}"
    );
    instance_id
}

/// RULE: whatever digit an instance of the account ends in, an id that differs from it in
/// that digit alone is an id no instance of the account has, and is refused on every
/// operation that acts on a caller-named instance with the same code, status and sentence —
/// while the instance it was derived from is itself served.
///
/// The rows span the digit the id ends in (every one of [`ID_LAST_DIGITS`]) and where the
/// instance came from (this service's own fleet seam, and a launch of EC2's own, whose last
/// digit EC2 drew rather than the test).
#[tokio::test]
async fn a_near_miss_on_an_instance_of_the_account_is_refused_whatever_digit_it_ends_in() {
    let fleet = Fleet::new();
    register_provider(&fleet.h).await;

    let mut rows: Vec<Row> = Vec::new();
    for digit in ID_LAST_DIGITS {
        let instance_id = instance_id_ending_in(*digit);
        {
            let store = fleet.h.engine.state::<ServiceState>();
            store
                .lock()
                .register_account_instance(instance_id.clone(), format!("{REGION}a"));
        }
        rows.push(Row {
            case: format!("an instance of the fleet seam whose id ends in {digit:?}"),
            instance_id,
        });
    }
    // And one EC2 itself launched, whose last digit the owner of instances drew: the row a
    // derivation that writes a fixed digit collides with whenever that is the digit drawn.
    let launched = fleet.launch(1).await;
    rows.push(Row {
        case: "an instance EC2 itself launched".to_string(),
        instance_id: launched[0].clone(),
    });

    for (index, row) in rows.iter().enumerate() {
        let case = &row.case;

        // The control first: the instance the near miss is derived from is one the account
        // has, so the plane serves it.
        let reply = associate(&fleet.h, &format!("user{index}"), &row.instance_id).await;
        let body = reply.ok(case);
        let summary = body
            .get("InstanceUserSummary")
            .unwrap_or_else(|| panic!("case {case:?}: no InstanceUserSummary in {body:?}"));
        assert_eq!(
            as_str(summary, "InstanceId", case),
            row.instance_id,
            "case {case:?}: expected the instance the request named, got {summary:?}"
        );

        // The near miss differs from it in its last digit alone, and names no instance of
        // the account: EC2, the owner, reports none of that id, and the only other place an
        // instance of this account is reported from is the fleet seam this test wrote the
        // rows into.
        let near_miss = near_miss_on(&row.instance_id);
        assert_ne!(
            near_miss, row.instance_id,
            "case {case:?}: a near miss must not be the instance it was derived from"
        );
        assert!(
            !fleet.has_instance(&near_miss).await,
            "setup: case {case:?}: the account must have no instance of {near_miss}"
        );

        for (operation, path) in GATED_PATHS {
            let case = format!("{operation}: a near miss on {case}");
            let reply = call(
                &fleet.h,
                path,
                &json!({
                    "Username": "someone",
                    "InstanceId": near_miss,
                    "IdentityProvider": provider_union(DIRECTORY),
                }),
            )
            .await;
            assert_eq!(
                reply.error_type.as_deref(),
                Some(UNAUTHORIZED_CODE),
                "case {case:?}: expected {UNAUTHORIZED_CODE}, got {reply:?}"
            );
            assert_eq!(
                reply.status, REFUSAL_STATUS,
                "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {reply:?}"
            );
            assert_eq!(
                reply.message(),
                Some(UNAUTHORIZED_MESSAGE),
                "case {case:?}: wrong refusal sentence in {reply:?}"
            );
        }
    }
}
