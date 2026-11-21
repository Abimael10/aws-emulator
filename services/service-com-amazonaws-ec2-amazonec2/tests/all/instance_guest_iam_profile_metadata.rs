// SPDX-License-Identifier: CC0-1.0

//! What a machine's own metadata service publishes about the IAM INSTANCE PROFILE the
//! instance runs as, read by a program of its guest the way an agent reads it.
//!
//! RULE: the metadata service publishes the instance profile the instance RUNS AS — the
//! profile this service holds for it at `iam/info`, and the NAME of the role that profile
//! carries under `iam/security-credentials/` — and publishes nothing at a path whose value the
//! machine has none of: an instance that runs as no profile publishes neither category, and a
//! machine whose role nothing resolved publishes the profile and no role. The role's name is
//! not this service's fact (IAM holds which role a profile carries), so it reaches the machine
//! from the caller that reached the guest ([`GuestIdentity`]) rather than being invented here.
//!
//! The rows differ along what the rule generalises over: whether the instance runs as a
//! profile at all, whether its role was resolved, and the path read (the role listing, the
//! profile document, the listing of the whole catalog).
//!
//! The launch goes through the service's own `ec2Query` API; the guest program is performed
//! through the seam a service with an agent inside the guest reaches
//! (`run_in_instance_guest_with`), which is the only way anything runs a program in a machine
//! of this emulator.

use std::time::{Duration, Instant};

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, GuestIdentity, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The ARN of the instance profile a row's launch names. A well-formed ARN of this account:
/// no IAM service is mounted beside EC2 here, so the association reports the profile as the
/// request spelled it, which is exactly the "a profile IAM holds no record of" reading.
const PROFILE_ARN: &str = "arn:aws:iam::000000000000:instance-profile/guest-identity-profile";

/// How long a case waits for the guest to come up, and how often it looks. Generous on
/// purpose — many times the span a launch owes its boot — so the case does not depend on how
/// fast the host runs.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// RULE: the machine's metadata service publishes the profile it runs as and the role that
/// profile carries, and nothing for a category the machine has no value for.
#[tokio::test]
async fn a_guest_reads_the_instance_profile_its_machine_runs_as() {
    /// One row: what it is, whether the launch names a profile, the role the caller resolved
    /// for the machine, and what the metadata service must then publish.
    struct Row {
        case: &'static str,
        with_profile: bool,
        role_name: Option<&'static str>,
        /// The lines `iam/security-credentials/` must answer with.
        listed_role: &'static [&'static str],
        /// Whether `iam/info` must carry the profile's ARN.
        publishes_profile: bool,
    }
    let rows: &[Row] = &[
        Row {
            case: "a machine running as a profile whose role was resolved",
            with_profile: true,
            role_name: Some("guest-identity-role"),
            listed_role: &["guest-identity-role"],
            publishes_profile: true,
        },
        Row {
            case: "a machine running as a profile whose role nothing resolved",
            with_profile: true,
            role_name: None,
            listed_role: &[],
            publishes_profile: true,
        },
        Row {
            case: "a machine running as no instance profile",
            with_profile: false,
            role_name: Some("a-role-of-no-profile"),
            listed_role: &[],
            publishes_profile: false,
        },
    ];
    for row in rows {
        let (router, engine) = super::ec2_query::emulator();
        let image_id = {
            let store = engine.state::<Ec2State>();
            let mut guard = store.lock();
            guard.insert_image(ImageSpec {
                name: "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64".to_string(),
                ..ImageSpec::default()
            })
        };
        let instance_id = launch(&router, row.case, &image_id, row.with_profile).await;
        booted(&router, &engine, row.case, &instance_id).await;
        let identity = GuestIdentity {
            iam_role_name: row.role_name.map(str::to_string),
            iam_role_credentials: None,
        };

        // The role listing: the one line naming the role a guest's agent reads to learn which
        // role it is running as, and nothing at all for a machine that publishes none.
        let listed = read(&engine, &instance_id, &identity, "iam/security-credentials/");
        assert_eq!(
            listed, row.listed_role,
            "case {:?}: expected the role listing {:?}, got {listed:?}",
            row.case, row.listed_role
        );

        // The profile document: the ARN the launch named, under the code AWS's own document
        // carries — and nothing for a machine that runs as no profile.
        let document = read(&engine, &instance_id, &identity, "iam/info");
        if row.publishes_profile {
            let text = document.join("\n");
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|error| {
                panic!(
                    "case {:?}: the profile document must be JSON ({error}): {text:?}",
                    row.case
                )
            });
            assert_eq!(
                parsed["Code"].as_str(),
                Some("Success"),
                "case {:?}: expected the document's own code, got {parsed}",
                row.case
            );
            assert_eq!(
                parsed["InstanceProfileArn"].as_str(),
                Some(PROFILE_ARN),
                "case {:?}: expected the ARN the launch named, got {parsed}",
                row.case
            );
        } else {
            assert!(
                document.is_empty(),
                "case {:?}: expected no profile document at all, got {document:?}",
                row.case
            );
        }

        // And the listing of the whole catalog offers exactly the categories the machine
        // publishes a value for, so a reader is not pointed at one that answers nothing.
        let catalog = read(&engine, &instance_id, &identity, "");
        let offers = |category: &str| catalog.iter().any(|line| line.trim() == category);
        assert_eq!(
            offers("iam/info"),
            row.publishes_profile,
            "case {:?}: the catalog must offer iam/info exactly where it answers, got {catalog:?}",
            row.case
        );
        assert_eq!(
            offers("iam/security-credentials"),
            !row.listed_role.is_empty(),
            "case {:?}: the catalog must offer the role listing exactly where it answers, \
             got {catalog:?}",
            row.case
        );
    }
}

/// What the machine's metadata service answers a guest program that reads `category`, as the
/// lines the read wrote.
///
/// One read per run, so what the machine published for THIS path is what the run's output is.
/// The read is the IMDSv1 form — an image with no `ImdsSupport` leaves the launch's
/// `HttpTokens` optional, so a `curl` with no session token is answered.
fn read(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    instance_id: &str,
    identity: &GuestIdentity,
    category: &str,
) -> Vec<String> {
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
            identity,
        )
        .unwrap_or_else(|| panic!("the booted guest of {instance_id} must perform the read"));
    assert_eq!(
        run.status, 0,
        "the read of {category:?} must run to success, got {} and {:?}",
        run.status, run.output
    );
    run.output
}

/// Launch one instance, running as [`PROFILE_ARN`] when the row says so, as the id EC2
/// minted.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    with_profile: bool,
) -> String {
    let mut params = vec![
        ("ImageId".to_string(), image_id.to_string()),
        ("InstanceType".to_string(), "t3.micro".to_string()),
        ("MinCount".to_string(), "1".to_string()),
        ("MaxCount".to_string(), "1".to_string()),
    ];
    if with_profile {
        params.push((
            "IamInstanceProfile.Arn".to_string(),
            PROFILE_ARN.to_string(),
        ));
    }
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
    engine: &std::sync::Arc<engine::InMemoryEngine>,
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
            "case {case:?}: DescribeInstances must succeed, got {status}: {body}"
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
