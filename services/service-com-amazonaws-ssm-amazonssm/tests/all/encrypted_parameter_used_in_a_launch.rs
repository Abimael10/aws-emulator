// SPDX-License-Identifier: CC0-1.0

//! What a client that keeps a secret in Parameter Store and launches an instance it wants
//! Systems Manager to manage depends on, end to end over one emulator: the secret is
//! unreadable from its stored form, and the instance it launched becomes a managed node.
//!
//! Two rules, each asserted over the class it generalises over rather than over one
//! parameter and one launch:
//!
//! 1. The value a read reports for a `SecureString` is the value held AT REST — the KMS
//!    ciphertext its put sealed — unless the read asks for decryption: "Return decrypted
//!    values for secure string parameters"
//!    (`smithy_query(query="doc",shape="GetParameterRequest",member="WithDecryption")`).
//!    That holds for every read that reports a parameter's value, so the rows below span
//!    the three readers (`GetParameter`, `GetParameters`, `GetParametersByPath`), both
//!    spellings of asking for no decryption, and values differing in size and bytes.
//! 2. An Amazon EC2 instance is a managed node exactly when the components that own the
//!    evidence say so — EC2 holds it running with an instance profile, and IAM holds that
//!    profile with a role EC2 may act as whose policies let the agent register — so HOW
//!    the launch spelled the profile is not part of the question. The rows span the two
//!    spellings `IamInstanceProfileSpecification` carries (`Name` and `Arn`), including a
//!    profile IAM holds under a path, and an ARN IAM holds no profile under, which
//!    registers nothing.
//!
//! The two rules meet in the third case: a decrypting read is a READ, so the secret a
//! client opened to feed into its launch is still held as the same ciphertext once the
//! node it launched is reporting itself.
//!
//! Every call goes through the wire a client uses — `awsJson1_1` for Systems Manager and
//! KMS, an `ec2Query`/`awsQuery` form POST for Amazon EC2 and IAM — so each component's
//! state is written and read through the service that owns it, and nothing reaches into a
//! store. The wire helpers, the instance-profile setup and the registration poll are
//! `ec2_managed_node_registration`'s; this suite adds only what its own rows need.
//!
//! Nothing here waits a fixed time: the registration span is a stored due instant, so a
//! case LOOKS AGAIN until the node is reported or a generous deadline passes. The instance
//! that must NOT register is launched BEFORE the ones that must, so by the time the last
//! registration has been observed its own span has passed too — which is what makes "it is
//! not reported" an answer rather than a race.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{
    ec2, element, iam, iam_ok, image_id, node, node_when_registered, trust_policy,
    AL2023_PARAMETER, EC2_PRINCIPAL, GRANTS_REGISTRATION,
};
use crate::parameter_store::{call_in_region, kms_key_with_alias, ACCOUNT, REGION};

/// The alias every case points its key at, so the key a parameter is sealed under is one
/// KMS's own operations created.
const SUITE_ALIAS: &str = "alias/encrypted-parameter-in-a-launch";

/// The hierarchy this suite's parameters live under, which is also what the hierarchy read
/// below lists.
const SUITE_PATH: &str = "/launch-secret";

/// The instance type every launch names, so the rows differ in what the rule is about
/// rather than in the hardware.
const INSTANCE_TYPE: &str = "t3.micro";

/// A router with Systems Manager, KMS, Amazon EC2 and IAM on ONE shared engine — the four
/// components the two rules are derived from, mounted the way the router mounts every
/// service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope, which is where each call's Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Store one `SecureString` under `name`, sealed under `key_id`.
async fn put_secure_string(router: &MultiServiceRouter, case: &str, name: &str, value: &str, key_id: &str) {
    let (status, body) = call_in_region(
        router,
        REGION,
        "PutParameter",
        &json!({ "Name": name, "Type": "SecureString", "Value": value, "KeyId": key_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutParameter {name} must succeed, got {status} with {body}"
    );
}

/// The three reads that report a parameter's value, as the body each one takes for `name`
/// and the way its answer is narrowed back to that one parameter.
#[derive(Debug, Clone, Copy)]
enum Reader {
    /// `GetParameter`, which reports the one parameter it was asked for.
    One,
    /// `GetParameters`, which reports a batch — the same value, reached through the
    /// reader a client resolving several names at once uses.
    Batch,
    /// `GetParametersByPath`, which reports every parameter of a hierarchy.
    Hierarchy,
}

impl Reader {
    /// The operation name this reader calls.
    fn operation(self) -> &'static str {
        match self {
            Self::One => "GetParameter",
            Self::Batch => "GetParameters",
            Self::Hierarchy => "GetParametersByPath",
        }
    }

    /// The `Parameter` this reader reports for `name`, with `WithDecryption` set as
    /// `with_decryption` says (omitted when it is `None`).
    async fn read(
        self,
        router: &MultiServiceRouter,
        case: &str,
        name: &str,
        with_decryption: Option<bool>,
    ) -> Value {
        let mut body = match self {
            Self::One => json!({ "Name": name }),
            Self::Batch => json!({ "Names": [name] }),
            Self::Hierarchy => json!({ "Path": SUITE_PATH }),
        };
        if let Some(flag) = with_decryption {
            body["WithDecryption"] = json!(flag);
        }
        let operation = self.operation();
        let (status, answered) = call_in_region(router, REGION, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} {body} must be served, got {status} with {answered}"
        );
        let reported = match self {
            Self::One => answered["Parameter"].clone(),
            Self::Batch | Self::Hierarchy => answered["Parameters"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .find(|parameter| parameter["Name"] == json!(name))
                .cloned()
                .unwrap_or(Value::Null),
        };
        assert!(
            reported.is_object(),
            "case {case:?}: {operation} must report {name}, got {answered}"
        );
        reported
    }
}

/// The `Value` a reported parameter carries.
fn reported_value(case: &str, parameter: &Value) -> String {
    parameter["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the read reports a Value, got {parameter}"))
        .to_string()
}

/// RULE 1: the value a read reports for a `SecureString` is what the parameter holds at
/// rest — a KMS ciphertext, not the value — unless the read asks for decryption, whichever
/// read reports it.
#[tokio::test]
async fn a_secure_string_is_reported_as_its_ciphertext_unless_the_read_asks_for_decryption() {
    let router = emulator();
    let key_id = kms_key_with_alias(&router, SUITE_ALIAS).await;

    // Each row: the case, the parameter's name, and the value its put seals — differing in
    // size and in the bytes the value carries, which is what the rule generalises over.
    let rows: Vec<(&str, String, String)> = vec![
        ("one byte", format!("{SUITE_PATH}/tiny"), "x".to_string()),
        (
            "an ordinary secret",
            format!("{SUITE_PATH}/password"),
            "s3cr3t-value".to_string(),
        ),
        (
            "bytes beyond ASCII",
            format!("{SUITE_PATH}/unicode"),
            "sécret-ключ-🔐".to_string(),
        ),
        (
            "a comma-separated value",
            format!("{SUITE_PATH}/commas"),
            "alpha,beta,gamma".to_string(),
        ),
        (
            "a kilobyte of it",
            format!("{SUITE_PATH}/long"),
            "v".repeat(1024),
        ),
    ];

    for (case, name, value) in &rows {
        put_secure_string(&router, case, name, value, &key_id).await;
    }

    for (case, name, value) in &rows {
        let mut sealed_forms: Vec<(Reader, Option<bool>, String)> = Vec::new();
        for reader in [Reader::One, Reader::Batch, Reader::Hierarchy] {
            // Both spellings of asking for no decryption report the stored form; asking
            // for decryption reports the value.
            for flag in [None, Some(false)] {
                let case = format!("{case} through {} with {flag:?}", reader.operation());
                let stored =
                    reported_value(&case, &reader.read(&router, &case, name, flag).await);
                assert_ne!(
                    &stored, value,
                    "case {case:?}: a read that asks for no decryption must not report the \
                     value, got {stored:?}"
                );
                let blob = ::aws_smithy_types::base64::decode(&stored).unwrap_or_else(|error| {
                    panic!("case {case:?}: the stored form is base64, got {stored:?}: {error}")
                });
                assert!(
                    !blob.is_empty() && blob != value.as_bytes(),
                    "case {case:?}: the reported bytes are a ciphertext, not the value, got \
                     {stored:?}"
                );
                sealed_forms.push((reader, flag, stored));
            }

            let case = format!("{case} through {} with Some(true)", reader.operation());
            let opened = reported_value(&case, &reader.read(&router, &case, name, Some(true)).await);
            assert_eq!(
                &opened, value,
                "case {case:?}: a read that asks for decryption must report the value the put \
                 wrote, got {opened:?}"
            );
        }

        // One parameter holds ONE value at rest, so every read that reports it undecrypted
        // reports the same stored form.
        let (first_reader, first_flag, first) = &sealed_forms[0];
        for (reader, flag, stored) in &sealed_forms[1..] {
            assert_eq!(
                stored, first,
                "case {case:?}: {} with {flag:?} must report the stored form {} with \
                 {first_flag:?} reported, got {stored:?} against {first:?}",
                reader.operation(),
                first_reader.operation()
            );
        }
    }
}

/// Create one instance profile whose role may register — the chain a client builds before a
/// launch — at `path` when a case names one, and answer the ARN IAM minted for it.
///
/// The profile is created BEFORE its role is added, so the first `<Arn>` of the answer is
/// the profile's own.
async fn registering_profile(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    path: Option<&str>,
) -> String {
    let trust = trust_policy(EC2_PRINCIPAL);
    iam_ok(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    iam_ok(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "registration"),
            ("PolicyDocument", GRANTS_REGISTRATION),
        ],
    )
    .await;
    let mut parameters = vec![("InstanceProfileName", name)];
    if let Some(path) = path {
        parameters.push(("Path", path));
    }
    let (status, body) = iam(router, "CreateInstanceProfile", &parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateInstanceProfile {parameters:?} must succeed, got {status} with \
         {body}"
    );
    let arn = element(&body, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: IAM reports the profile's ARN, got {body}"));
    iam_ok(
        router,
        case,
        "AddRoleToInstanceProfile",
        &[("InstanceProfileName", name), ("RoleName", name)],
    )
    .await;
    arn
}

/// Launch one instance from `image` with `profile` naming the instance profile the way a
/// row spells it, as the instance id Amazon EC2 answered.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image: &str,
    profile: &(&str, String),
) -> String {
    let (member, value) = profile;
    let parameters = vec![
        ("ImageId", image),
        ("InstanceType", INSTANCE_TYPE),
        ("MinCount", "1"),
        ("MaxCount", "1"),
        (member, value.as_str()),
    ];
    let (status, body) = ec2(router, REGION, "RunInstances", &parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances {parameters:?} must launch, got {status} with {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an instance id: {body}"))
}

/// RULE 2: whether an instance is a managed node is what EC2 and IAM hold about it, so a
/// launch that names its instance profile by name, by ARN, or by the ARN of a profile held
/// under a path all register — and an ARN IAM holds no profile under registers nothing.
#[tokio::test]
async fn an_instance_becomes_an_online_managed_node_however_its_profile_was_named() {
    let router = emulator();
    let image = image_id(&router, AL2023_PARAMETER).await;

    // The instance that must NOT become a node is launched FIRST, so its own span has
    // passed by the time the last registration below has been observed.
    let unheld = (
        "IamInstanceProfile.Arn",
        format!("arn:aws:iam::{ACCOUNT}:instance-profile/no-such-profile"),
    );
    let unmanaged = launch(&router, "an ARN IAM holds no profile under", &image, &unheld).await;

    // Each row: the case, the profile's name, the path IAM holds it under, and the member
    // of `IamInstanceProfileSpecification` the launch spells it with.
    let rows: Vec<(&str, &str, Option<&str>, &str)> = vec![
        ("named by name", "launch-by-name", None, "IamInstanceProfile.Name"),
        ("named by ARN", "launch-by-arn", None, "IamInstanceProfile.Arn"),
        (
            "named by the ARN of a profile under a path",
            "launch-by-path",
            Some("/dept/eng/"),
            "IamInstanceProfile.Arn",
        ),
    ];

    let mut launched: Vec<(&str, String)> = Vec::new();
    for (case, name, path, member) in &rows {
        let arn = registering_profile(&router, case, name, *path).await;
        let spelled = if *member == "IamInstanceProfile.Name" {
            (*member, (*name).to_string())
        } else {
            (*member, arn)
        };
        launched.push((case, launch(&router, case, &image, &spelled).await));
    }

    for (case, instance) in &launched {
        let reported = node_when_registered(&router, case, REGION, instance).await;
        assert_eq!(
            reported["InstanceId"].as_str(),
            Some(instance.as_str()),
            "case {case:?}: the node reported must be the instance that was launched, got \
             {reported}"
        );
        assert_eq!(
            reported["PingStatus"].as_str(),
            Some("Online"),
            "case {case:?}: a node that has registered is pinging, got {reported}"
        );
        assert_eq!(
            reported["ResourceType"].as_str(),
            Some("EC2Instance"),
            "case {case:?}: the node is the EC2 instance it registered from, got {reported}"
        );
    }

    let unregistered = node(&router, REGION, &unmanaged).await;
    assert!(
        unregistered.is_none(),
        "case \"an ARN IAM holds no profile under\": an instance whose profile IAM holds no \
         record of registers no node, got {unregistered:?}"
    );
}

/// RULE 1 and RULE 2 together, and the invariant that joins them: opening a secret is a
/// READ, so the value a client decrypted to feed its launch is still held as the same
/// ciphertext once the instance it launched is reporting itself as a managed node.
#[tokio::test]
async fn the_secret_a_launch_consumed_is_unchanged_at_rest_once_its_node_is_online() {
    let case = "a secret consumed by a launch";
    let router = emulator();
    let key_id = kms_key_with_alias(&router, SUITE_ALIAS).await;
    let name = format!("{SUITE_PATH}/consumed");
    let value = "bootstrap-token-9f2";
    put_secure_string(&router, case, &name, value, &key_id).await;

    let sealed = reported_value(case, &Reader::One.read(&router, case, &name, None).await);
    assert_ne!(
        sealed, value,
        "case {case:?}: the stored form must not be the value, got {sealed:?}"
    );

    // What a client does with such a parameter before it launches: open it, then launch.
    let opened = reported_value(
        case,
        &Reader::One.read(&router, case, &name, Some(true)).await,
    );
    assert_eq!(
        opened, value,
        "case {case:?}: the decrypting read reports the value the put wrote, got {opened:?}"
    );

    let image = image_id(&router, AL2023_PARAMETER).await;
    let profile = registering_profile(&router, case, "launch-consuming-a-secret", None).await;
    let instance = launch(
        &router,
        case,
        &image,
        &("IamInstanceProfile.Arn", profile),
    )
    .await;
    let reported = node_when_registered(&router, case, REGION, &instance).await;
    assert_eq!(
        reported["PingStatus"].as_str(),
        Some("Online"),
        "case {case:?}: the launched instance must be reported Online, got {reported}"
    );

    // Neither the decrypting read nor the launch changed what the parameter holds.
    for flag in [None, Some(false)] {
        let still = reported_value(case, &Reader::One.read(&router, case, &name, flag).await);
        assert_eq!(
            still, sealed,
            "case {case:?}: the stored ciphertext is unchanged by a read of it (asked with \
             {flag:?}), got {still:?} against {sealed:?}"
        );
    }
    let again = reported_value(
        case,
        &Reader::One.read(&router, case, &name, Some(true)).await,
    );
    assert_eq!(
        again, value,
        "case {case:?}: the value still opens to what the put wrote, got {again:?}"
    );
}
