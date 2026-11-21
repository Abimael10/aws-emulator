// SPDX-License-Identifier: CC0-1.0

//! The CREDENTIALS a machine's own metadata service publishes for the IAM role its instance
//! profile carries, read by a program of its guest the way an SDK or an agent reads them.
//!
//! RULE: an instance that runs as an instance profile has temporary credentials FOR the role
//! that profile carries published to its guest under that role's name —
//! `iam/security-credentials/<role name>`, the document AWS publishes there (`Code`,
//! `LastUpdated`, `Type`, `AccessKeyId`, `SecretAccessKey`, `Token`, `Expiration`) — and those
//! credentials are a SESSION of that role: a call signed with them is made by
//! `arn:aws:sts::<account>:assumed-role/<role name>/<instance id>`, whose user id is the
//! composite of the role's own id and the instance's. The credentials belong to the instance
//! rather than to one read, so reading the path twice reads one credential; and they belong to
//! the role the instance runs as NOW, so replacing the association serves a session of the new
//! role and publishes nothing under the old role's name.
//!
//! The rows differ along what the rule generalises over: whether the profile's name is the
//! role's name or another name entirely, the IAM path the profile and role are held under, and
//! the name a read asks the credentials under (the role's, and a name that is not the role's).
//! The swap is its own case, which is the one thing a single row cannot carry.
//!
//! Every expected value is read back from the service that OWNS it — the role's id and ARN
//! from IAM's own response, the caller identity from the token service's own
//! `GetCallerIdentity` — rather than written into the table: a credential is minted by the
//! emulator's credential store and an id by IAM, so this suite asserts the services agree
//! about one instance rather than asserting a literal.
//!
//! The launch goes through EC2's own `ec2Query` API; the guest program is performed through the
//! seam a service with an agent inside the guest reaches (`run_in_instance_guest_with`), which
//! is the only way anything runs a program in a machine of this emulator, with the identity
//! that caller resolves for the machine — the role's name and the credentials minted for it
//! (`instance_role_credentials`), exactly as Systems Manager's Run Command resolves them.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, GuestIdentity, ImageSpec};
use emulator_server::CredentialStore;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The IAM API version a client sends in an awsQuery body.
const IAM_API_VERSION: &str = "2010-05-08";

/// The STS API version a client sends in an awsQuery body.
const STS_API_VERSION: &str = "2011-06-15";

/// The access key id the fixtures are provisioned under: the account's own published
/// credential, which holds no IAM policies.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The trust policy a role an instance runs as carries ("the IAM role … must trust the Amazon
/// EC2 service to assume it").
const EC2_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The image every launch names, whose name leaves the launch's `HttpTokens` optional — so the
/// IMDSv1 read below is answered without a session token.
const IMAGE_NAME: &str = "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64";

/// The `Code` AWS's own credentials document carries for a role the metadata service resolved.
const DOCUMENT_CODE: &str = "Success";

/// The `Type` it carries.
const DOCUMENT_TYPE: &str = "AWS-HMAC";

/// How long a case waits for the guest to come up, and how often it looks. Generous on
/// purpose — many times the span a launch owes its boot — so the case does not depend on how
/// fast the host runs.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// The emulator the cases drive: EC2 beside the IAM and STS services on ONE shared engine, so
/// the role a case creates is the record the mint reads and the session it mints is the one the
/// token service resolves.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What IAM holds about one role an instance runs as.
#[derive(Debug, Clone)]
struct HeldRole {
    /// The role's name, which is the path segment its credentials are published under.
    name: String,
    /// `Role$RoleId`, which the session's user id is built from.
    id: String,
    /// The name of the instance profile holding it.
    profile: String,
}

/// RULE: the metadata service publishes the credentials of the role the instance's profile
/// carries, under that role's name, and they are a session of that role.
#[tokio::test]
async fn a_guest_is_served_the_credentials_of_the_role_its_instance_profile_carries() {
    // (what the case is, the profile's name, the role's name, the IAM path both are held
    // under)
    let rows: &[(&str, &str, &str, &str)] = &[
        (
            "a profile and role named alike, as a client that names both once creates them",
            "imds-alike",
            "imds-alike",
            "/",
        ),
        (
            "a profile whose role is named differently, as a template's generated names are",
            "stack-instance-profile-1A2B3C",
            "stack-instance-role-4D5E6F",
            "/",
        ),
        (
            "a profile and role held under an IAM path",
            "pathed-profile",
            "pathed-role",
            "/sim-profile-path/",
        ),
    ];
    for (case, profile_name, role_name, path) in rows {
        let (router, engine) = emulator();
        let credentials = CredentialStore::default();
        let role = held_role(&router, case, profile_name, role_name, path).await;
        let instance_id = launch(&router, case, &image(&engine), Some(profile_name)).await;
        booted(&router, &engine, case, &instance_id).await;

        // The listing names the role whose credentials the guest's agents use.
        let listed = read(&engine, &credentials, &instance_id, "iam/security-credentials/");
        assert_eq!(
            listed,
            vec![role.name.clone()],
            "case {case:?}: expected the listing to name the role, got {listed:?}"
        );

        // The document published UNDER that name: every member AWS's own document carries,
        // with the credential members the token service minted.
        let served = document(&engine, &credentials, &instance_id, &role.name, case);
        assert_eq!(
            served["Code"].as_str(),
            Some(DOCUMENT_CODE),
            "case {case:?}: expected the document's own code, got {served}"
        );
        assert_eq!(
            served["Type"].as_str(),
            Some(DOCUMENT_TYPE),
            "case {case:?}: expected the document's own type, got {served}"
        );
        for member in ["AccessKeyId", "SecretAccessKey", "Token"] {
            let value = served[member].as_str().unwrap_or_default();
            assert!(
                !value.is_empty(),
                "case {case:?}: expected the document to carry {member}, got {served}"
            );
        }
        let issued = served["LastUpdated"].as_str().unwrap_or_default();
        let expires = served["Expiration"].as_str().unwrap_or_default();
        assert!(
            !issued.is_empty() && expires > issued,
            "case {case:?}: expected the credentials to expire after they were minted, got \
             {issued:?} and {expires:?}"
        );

        // They belong to the INSTANCE rather than to one read: a second read is served the
        // same credential, not a freshly minted one.
        let again = document(&engine, &credentials, &instance_id, &role.name, case);
        assert_eq!(
            again["AccessKeyId"], served["AccessKeyId"],
            "case {case:?}: expected the second read to be served the same credential, got \
             {again} after {served}"
        );

        // And nothing is published under a name that is not the role's.
        let other = read(
            &engine,
            &credentials,
            &instance_id,
            &format!("iam/security-credentials/{}-other", role.name),
        );
        assert!(
            other.is_empty(),
            "case {case:?}: expected no credentials under another name, got {other:?}"
        );

        // The credentials are a SESSION of the role, named for the instance: the token
        // service's own `GetCallerIdentity` says who a call signed with them is made by.
        let access_key_id = served["AccessKeyId"].as_str().unwrap_or_default();
        let identity = caller_identity(&router, case, access_key_id).await;
        assert_eq!(
            identity,
            (
                format!("{}:{instance_id}", role.id),
                format!(
                    "arn:aws:sts::{}:assumed-role/{}/{instance_id}",
                    engine::emulator_account_id(),
                    role.name
                ),
            ),
            "case {case:?}: expected the credentials to speak for the role as this instance"
        );
    }
}

/// RULE: the credentials belong to the role the instance runs as NOW — replacing the
/// association serves a session of the new role, and publishes nothing under the old role's
/// name.
#[tokio::test]
async fn replacing_the_association_changes_the_credentials_the_guest_is_served() {
    let case = "a role swap through ReplaceIamInstanceProfileAssociation";
    let (router, engine) = emulator();
    let credentials = CredentialStore::default();
    let first = held_role(&router, case, "swap-first-profile", "swap-first-role", "/").await;
    let second = held_role(&router, case, "swap-second-profile", "swap-second-role", "/").await;
    let instance_id = launch(&router, case, &image(&engine), Some(&first.profile)).await;
    booted(&router, &engine, case, &instance_id).await;

    let before = document(&engine, &credentials, &instance_id, &first.name, case);
    let association_id = association_over(&router, case, &instance_id).await;

    let (status, body) = call(
        &router,
        REGION,
        "ReplaceIamInstanceProfileAssociation",
        &[
            ("AssociationId".to_string(), association_id),
            (
                "IamInstanceProfile.Name".to_string(),
                second.profile.clone(),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the replacement must be served, got {status}: {body}"
    );

    // The listing now names the new role, and the credentials published under it are another
    // session — of the new role, as the token service reports it.
    let listed = read(&engine, &credentials, &instance_id, "iam/security-credentials/");
    assert_eq!(
        listed,
        vec![second.name.clone()],
        "case {case:?}: expected the listing to name the new role, got {listed:?}"
    );
    let after = document(&engine, &credentials, &instance_id, &second.name, case);
    assert_ne!(
        after["AccessKeyId"], before["AccessKeyId"],
        "case {case:?}: expected the swap to change the credential served, got {after} \
         against {before}"
    );
    let identity = caller_identity(
        &router,
        case,
        after["AccessKeyId"].as_str().unwrap_or_default(),
    )
    .await;
    assert_eq!(
        identity,
        (
            format!("{}:{instance_id}", second.id),
            format!(
                "arn:aws:sts::{}:assumed-role/{}/{instance_id}",
                engine::emulator_account_id(),
                second.name
            ),
        ),
        "case {case:?}: expected the new credentials to speak for the new role"
    );

    // And the old role's name publishes nothing: the machine no longer runs as it.
    let stale = read(
        &engine,
        &credentials,
        &instance_id,
        &format!("iam/security-credentials/{}", first.name),
    );
    assert!(
        stale.is_empty(),
        "case {case:?}: expected nothing under the old role's name, got {stale:?}"
    );
}

/// Create the role `role_name` under `path`, an instance profile named `profile_name` under
/// the same path, and bind the one to the other — the chain a client provisions before it
/// launches an instance that runs as a profile — and answer what IAM holds about the role.
async fn held_role(
    router: &MultiServiceRouter,
    case: &str,
    profile_name: &str,
    role_name: &str,
    path: &str,
) -> HeldRole {
    let created = iam_call(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("Path", path),
            ("AssumeRolePolicyDocument", EC2_TRUST_POLICY),
        ],
    )
    .await;
    let id = element(&created, "RoleId")
        .unwrap_or_else(|| panic!("case {case:?}: IAM reported no RoleId: {created}"));
    iam_call(
        router,
        case,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile_name), ("Path", path)],
    )
    .await;
    iam_call(
        router,
        case,
        "AddRoleToInstanceProfile",
        &[
            ("InstanceProfileName", profile_name),
            ("RoleName", role_name),
        ],
    )
    .await;
    HeldRole {
        name: role_name.to_string(),
        id,
        profile: profile_name.to_string(),
    }
}

/// One IAM call made by the account's own published credential, whose body must be served.
async fn iam_call(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = super::ec2_query::query_call_as(
        router,
        REGION,
        "iam",
        IAM_API_VERSION,
        ACCOUNT_KEY_ID,
        action,
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must be served, got {status}: {body}"
    );
    body
}

/// Who the token service reports a call signed by `access_key_id` is made by, as
/// `(UserId, Arn)`.
async fn caller_identity(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) -> (String, String) {
    let (status, body) = super::ec2_query::query_call_as(
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
        "case {case:?}: the credentials must resolve to a caller, got {status}: {body}"
    );
    (
        element(&body, "UserId")
            .unwrap_or_else(|| panic!("case {case:?}: no UserId in {body}")),
        element(&body, "Arn").unwrap_or_else(|| panic!("case {case:?}: no Arn in {body}")),
    )
}

/// The image the launches name, seeded in the account's catalog.
fn image(engine: &Arc<engine::InMemoryEngine>) -> String {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    guard.insert_image(ImageSpec {
        name: IMAGE_NAME.to_string(),
        ..ImageSpec::default()
    })
}

/// Launch one instance, running as the instance profile `profile_name` when the case names
/// one, as the id EC2 minted.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    profile_name: Option<&str>,
) -> String {
    let mut params = vec![
        ("ImageId".to_string(), image_id.to_string()),
        ("InstanceType".to_string(), "t3.micro".to_string()),
        ("MinCount".to_string(), "1".to_string()),
        ("MaxCount".to_string(), "1".to_string()),
    ];
    if let Some(profile_name) = profile_name {
        params.push((
            "IamInstanceProfile.Name".to_string(),
            profile_name.to_string(),
        ));
    }
    let (status, body) = call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must be served, got {status}: {body}"
    );
    elements(&body, "instanceId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// The id of the association the instance holds, as EC2's own describe reports it.
async fn association_over(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeIamInstanceProfileAssociations",
        &[
            ("Filter.1.Name".to_string(), "instance-id".to_string()),
            ("Filter.1.Value.1".to_string(), instance_id.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the association describe must be served, got {status}: {body}"
    );
    elements(&body, "associationId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no association over {instance_id}: {body}"))
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

/// The credentials document the guest reads under the role name `role_name`, parsed.
fn document(
    engine: &Arc<engine::InMemoryEngine>,
    credentials: &CredentialStore,
    instance_id: &str,
    role_name: &str,
    case: &str,
) -> serde_json::Value {
    let read = read(
        engine,
        credentials,
        instance_id,
        &format!("iam/security-credentials/{role_name}"),
    )
    .join("\n");
    serde_json::from_str(&read).unwrap_or_else(|error| {
        panic!("case {case:?}: the credentials document must be JSON ({error}): {read:?}")
    })
}
