// SPDX-License-Identifier: CC0-1.0

//! WHICH roles yield an instance a session at all, and WHOSE session it is.
//!
//! RULE under test: an instance obtains the credentials of the role its instance profile
//! carries by ASSUMING that role, so it obtains them only where that role's trust policy
//! admits the Amazon EC2 service principal — a profile whose role trusts another service
//! publishes no credentials under its name at all, while still LISTING the role, because which
//! role the profile carries is a fact whatever that role trusts. And the session an instance
//! obtains is the INSTANCE's, not the role's: two instances running as ONE profile hold two
//! sessions, each speaking for `<role id>:<instance id>` with its own instance's id, which is
//! what makes a grant conditioned on that composite authorize one instance and no other.
//!
//! The rows differ along what the rule generalises over: WHICH service principal the role
//! trusts (Amazon EC2, two other services, and a document naming several principals of which
//! Amazon EC2 is one, so nothing is keyed to one spelling or to a single-principal document).
//! The per-instance half is asserted on every row that obtains credentials, with two instances
//! launched from the one profile.
//!
//! What the credentials DOCUMENT carries, that reading the path twice reads one credential, and
//! that replacing the association serves a session of the new role are
//! `tests/all/instance_guest_iam_role_credentials.rs`'s rule; nothing of it is re-asserted
//! here.
//!
//! Every expected value is read back from the service that OWNS it — the role's unique id from
//! IAM's own answer to `CreateRole`, the account from the token service's own answer — rather
//! than written into the table. The launch goes through EC2's own `ec2Query` API; the metadata
//! read is performed through the seam a service with an agent inside the guest reaches
//! (`run_in_instance_guest_with`), with the identity that caller resolves for the machine —
//! the role's name and the credentials minted for it (`instance_role_credentials`), exactly as
//! Systems Manager's Run Command resolves them.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, GuestIdentity, ImageSpec};
use emulator_server::CredentialStore;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, query_call_as};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The IAM API version an awsQuery body carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The AWS STS API version an awsQuery body carries.
const STS_API_VERSION: &str = "2011-06-15";

/// The access key id the fixtures are provisioned under: the account's own published
/// credential.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The service principal a role has to trust for an instance to assume it through its instance
/// profile, spelled out here rather than imported from the service crate, so a wrong constant
/// in the implementation cannot make this suite pass.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The image every launch names, whose name leaves the launch's `HttpTokens` optional — so the
/// IMDSv1 read below is answered without a session token.
const IMAGE_NAME: &str = "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64";

/// How long a case waits for a guest to come up, and how often it looks. Generous on purpose —
/// many times the span a launch owes its boot — so a case does not depend on how fast the host
/// runs.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// How many instances of the one profile each row launches: the session is the INSTANCE's, so
/// one instance cannot show that.
const INSTANCES_PER_ROW: usize = 2;

/// One row: what it is, the service principals the profile's role trusts, and whether an
/// instance of that profile may assume it.
struct Row {
    case: &'static str,
    trusts: &'static [&'static str],
    assumable: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role trusting Amazon EC2, as an instance profile's role is created",
            trusts: &[EC2_PRINCIPAL],
            assumable: true,
        },
        Row {
            case: "a role trusting several services, Amazon EC2 among them",
            trusts: &["ssm.amazonaws.com", EC2_PRINCIPAL, "ecs.amazonaws.com"],
            assumable: true,
        },
        Row {
            case: "a role trusting AWS Lambda, which no instance may assume",
            trusts: &["lambda.amazonaws.com"],
            assumable: false,
        },
        Row {
            case: "a role trusting AWS Glue, which no instance may assume either",
            trusts: &["glue.amazonaws.com"],
            assumable: false,
        },
    ]
}

/// RULE: only a role an instance may assume yields credentials, and the session each instance
/// obtains speaks for that instance.
#[tokio::test]
async fn only_a_role_an_instance_may_assume_yields_it_a_session_of_its_own() {
    for (ordinal, row) in rows().into_iter().enumerate() {
        let (router, engine) = emulator();
        let credentials = CredentialStore::default();
        let image_id = seeded_image(&engine);
        let name = format!("imds-assumable-{ordinal}");
        let role_id = create_role(&router, row.case, &name, row.trusts).await;
        create_profile_holding_role(&router, row.case, &name, &name).await;

        let mut launched: Vec<String> = Vec::new();
        for _ in 0..INSTANCES_PER_ROW {
            launched.push(launch(&router, row.case, &image_id, &name).await);
        }
        for instance_id in &launched {
            booted(&router, &engine, row.case, instance_id).await;
        }

        // WHICH role the profile carries is published whatever that role trusts: the listing is
        // a fact of the profile, not a grant.
        for instance_id in &launched {
            let listed = read(&engine, &credentials, instance_id, "iam/security-credentials/");
            assert_eq!(
                listed,
                vec![name.clone()],
                "case {:?}: the metadata service must list the role the profile carries, got \
                 {listed:?}",
                row.case
            );
        }

        let mut spoken_for: Vec<String> = Vec::new();
        for instance_id in &launched {
            let document = read(
                &engine,
                &credentials,
                instance_id,
                &format!("iam/security-credentials/{name}"),
            );
            if !row.assumable {
                assert!(
                    document.is_empty(),
                    "case {:?}: a role no instance may assume must yield no credentials, got \
                     {document:?}",
                    row.case
                );
                continue;
            }
            let read = document.join("\n");
            let parsed: serde_json::Value = serde_json::from_str(&read).unwrap_or_else(|error| {
                panic!(
                    "case {:?}: the credentials document must be JSON ({error}): {read:?}",
                    row.case
                )
            });
            let access_key_id = parsed["AccessKeyId"].as_str().unwrap_or_default();
            assert!(
                !access_key_id.is_empty(),
                "case {:?}: the document must carry an AccessKeyId, got {parsed}",
                row.case
            );

            // WHO that credential speaks for, answered by the token service itself: the
            // composite of the role's own unique id and THIS instance's id.
            let identity = caller_identity(&router, row.case, access_key_id).await;
            assert_eq!(
                identity.user_id,
                format!("{role_id}:{instance_id}"),
                "case {:?}: expected the role id and this instance's id joined by a colon, got \
                 {:?}",
                row.case,
                identity.user_id
            );
            assert_eq!(
                identity.arn,
                format!(
                    "arn:aws:sts::{}:assumed-role/{name}/{instance_id}",
                    identity.account
                ),
                "case {:?}: expected the assumed-role session ARN of this instance, got {:?}",
                row.case,
                identity.arn
            );
            spoken_for.push(identity.user_id);
        }

        // Two instances of ONE profile hold two sessions: the credential is the instance's, not
        // the role's, so neither instance can speak for the other.
        if row.assumable {
            let mut distinct = spoken_for.clone();
            distinct.sort();
            distinct.dedup();
            assert_eq!(
                distinct.len(),
                INSTANCES_PER_ROW,
                "case {:?}: expected each instance a session of its own, got {spoken_for:?}",
                row.case
            );
        }
    }
}

/// The emulator the rows drive: EC2 beside IAM and AWS STS on ONE shared engine, so the role a
/// row creates is the record the mint reads and the session it mints is the one the token
/// service resolves an identity from.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this suite is about; the router still reads the SigV4 credential
    // scope, which is where each call's Region — and WHO made it — comes from.
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What `sts:GetCallerIdentity` answered for one credential.
struct CallerIdentity {
    user_id: String,
    account: String,
    arn: String,
}

/// The image every launch names: which image an instance came from has nothing to do with the
/// identity it runs as, so one serves every row.
fn seeded_image(engine: &Arc<engine::InMemoryEngine>) -> String {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    guard.insert_image(ImageSpec {
        name: IMAGE_NAME.to_string(),
        ..ImageSpec::default()
    })
}

/// A trust policy admitting the service principals `trusts` names and nothing else — one
/// `Service` string for a document naming one, the list form for a document naming several,
/// which is how a role trusted by more than one service is written.
fn trust_policy(trusts: &[&str]) -> String {
    let services = match trusts {
        [one] => format!("\"{one}\""),
        several => format!(
            "[{}]",
            several
                .iter()
                .map(|service| format!("\"{service}\""))
                .collect::<Vec<_>>()
                .join(",")
        ),
    };
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":{services}}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// Create the role `name` trusting `trusts`, and answer the unique id IAM minted for it — the
/// value every `UserId` assertion below is taken against, read from the service that owns it
/// rather than written into the table.
async fn create_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    trusts: &[&str],
) -> String {
    let (status, body) = iam_call(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy(trusts)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM must hold the role {name:?}, got {status}: {body}"
    );
    element(&body, "RoleId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole must report a RoleId: {body}"))
}

/// Create the instance profile `profile_name` and put the role `role_name` in it, the way AWS
/// builds one.
async fn create_profile_holding_role(
    router: &MultiServiceRouter,
    case: &str,
    profile_name: &str,
    role_name: &str,
) {
    for (action, params) in [
        (
            "CreateInstanceProfile",
            vec![("InstanceProfileName", profile_name)],
        ),
        (
            "AddRoleToInstanceProfile",
            vec![
                ("InstanceProfileName", profile_name),
                ("RoleName", role_name),
            ],
        ),
    ] {
        let (status, body) = iam_call(router, action, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: IAM {action} must be served, got {status}: {body}"
        );
    }
}

/// One IAM `awsQuery` call made by the account's own published credential.
async fn iam_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    query_call_as(
        router,
        REGION,
        "iam",
        IAM_API_VERSION,
        ACCOUNT_KEY_ID,
        action,
        params,
    )
    .await
}

/// What the token service reports for a request signed with `access_key_id` — the one place WHO
/// a credential speaks for is answered, and the same place every other service reads it.
async fn caller_identity(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) -> CallerIdentity {
    let (status, body) = query_call_as(
        router,
        REGION,
        "sts",
        STS_API_VERSION,
        access_key_id,
        "GetCallerIdentity",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCallerIdentity must be served, got {status}: {body}"
    );
    let read = |name: &str| {
        element(&body, name)
            .unwrap_or_else(|| panic!("case {case:?}: the answer must carry {name}: {body}"))
    };
    CallerIdentity {
        user_id: read("UserId"),
        account: read("Account"),
        arn: read("Arn"),
    }
}

/// Launch one instance from `image_id` running as `profile_name`, and answer the instance id
/// EC2 minted.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    profile_name: &str,
) -> String {
    let params = vec![
        ("ImageId".to_string(), image_id.to_string()),
        ("InstanceType".to_string(), "t3.micro".to_string()),
        ("MinCount".to_string(), "1".to_string()),
        ("MaxCount".to_string(), "1".to_string()),
        (
            "IamInstanceProfile.Name".to_string(),
            profile_name.to_string(),
        ),
    ];
    let (status, body) = call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status}: {body}"
    );
    elements(&body, "instanceId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// Look at the instance through the API until its guest has booted — the write a launch owes
/// the clock, which `DescribeInstances` materialises for any caller.
async fn booted(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    instance_id: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = call(
            router,
            REGION,
            "DescribeInstances",
            &[("InstanceId.1".to_string(), instance_id.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInstances must be served, got {status}: {body}"
        );
        let up = {
            let store = engine.state::<Ec2State>();
            let guard = store.lock();
            guard.instance_guest_is_up(instance_id)
        };
        if up {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the guest of {instance_id} to come up with nothing but \
             the clock running"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What the machine's metadata service answers a guest program that reads `category`, as the
/// lines the read wrote.
///
/// The identity the guest runs under is resolved the way the caller that reaches a guest
/// resolves it — the role's name from IAM's records and the credentials minted for that role
/// out of the emulator's credential store — so the read is answered by the same machine a Run
/// Command invocation reads.
fn read(
    engine: &Arc<engine::InMemoryEngine>,
    credentials: &CredentialStore,
    instance_id: &str,
    category: &str,
) -> Vec<String> {
    let identity = GuestIdentity {
        iam_role_name: ec2::state::instance_profile_role_name(&**engine, instance_id),
        iam_role_credentials: ec2::state::instance_role_credentials(
            &**engine,
            credentials,
            instance_id,
            SystemTime::now(),
        ),
    };
    let address = ec2::state::metadata_address();
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let run = guard
        .run_in_instance_guest_with(
            instance_id,
            &[format!(
                "curl -s http://{address}/latest/meta-data/{category}"
            )],
            None,
            &identity,
        )
        .unwrap_or_else(|| panic!("the booted guest of {instance_id} must perform the read"));
    assert_eq!(
        run.status, 0,
        "the read of {category:?} must run to success, got {} and {:?}",
        run.status, run.output
    );
    run.output
}
