// SPDX-License-Identifier: CC0-1.0

//! The transfer locations a DataSync agent reaches over a file-server protocol — NFS, SMB
//! and HDFS — driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: FmrsService.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router. Nothing reaches into the state struct, so a response that is not
//! computed from persisted state cannot pass.
//!
//! The first suite covers the rule the whole family shares with the agent surface: an
//! operation that NAMES an agent resolves its `AgentArn` against the agents this account
//! has activated, and refuses one that resolves to nothing exactly as `DescribeAgent`
//! refuses it.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite signs for. It is deliberately NOT the
/// emulator's default, so an ARN that carries it proves the region came from the
/// request's SigV4 credential scope.
const SIGNED_REGION: &str = "us-west-2";

/// The canonical account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// An activation key in the modelled `ActivationKey` shape
/// (`^[A-Z0-9]{5}(-[A-Z0-9]{5}){4}$`).
const ACTIVATION_KEY: &str = "AAAAA-11111-BBBBB-22222-CCCCC";

/// A router with just this service mounted on a fresh engine.
fn service() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_datasync_fmrsservice::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_1` call, returning the HTTP status and the parsed response body.
async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("FmrsService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDDATASYNCTEST/20260101/{SIGNED_REGION}\
                 /datasync/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or_else(|_| {
            serde_json::json!({ "__unparsed": String::from_utf8_lossy(&payload) })
        })
    };
    (status, parsed)
}

/// A call that must succeed, returning its body.
async fn ok(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
    case: &str,
) -> serde_json::Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// A call that must be refused with HTTP 400 and the one modelled client error, returning
/// the refusal body.
async fn refused(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
    case: &str,
) -> serde_json::Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to answer 400, got {status} with {parsed}"
    );
    let wire_type = parsed["__type"].as_str().unwrap_or_default();
    assert!(
        wire_type.contains("InvalidRequestException"),
        "case {case:?}: expected an InvalidRequestException from {operation}, got {parsed}"
    );
    parsed
}

/// A required string member of a response body.
fn string(body: &serde_json::Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// The modelled `LocationArn` shape: `…:location/loc-[0-9a-z]{17}`.
fn assert_location_arn(arn: &str, case: &str) {
    let prefix = format!("arn:aws:datasync:{SIGNED_REGION}:{ACCOUNT}:location/loc-");
    assert!(
        arn.starts_with(&prefix),
        "case {case:?}: expected a location ARN starting {prefix:?}, got {arn:?}"
    );
    let id = &arn[prefix.len()..];
    assert!(
        id.len() == 17 && id.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
        "case {case:?}: expected a 17-character lowercase alphanumeric location id, got {id:?}"
    );
}

/// `count` agents this account has really activated, by ARN.
async fn given_agents(router: &MultiServiceRouter, count: usize, case: &str) -> Vec<String> {
    let mut activated: Vec<String> = Vec::with_capacity(count);
    for _ in 0..count {
        let created = ok(
            router,
            "CreateAgent",
            serde_json::json!({ "ActivationKey": ACTIVATION_KEY }),
            case,
        )
        .await;
        activated.push(string(&created, "AgentArn", case));
    }
    activated
}

/// A syntactically valid `AgentArn` — the modelled pattern is
/// `…:datasync:<region>:<account>:agent/agent-[0-9a-z]{17}` — that names no agent this
/// account ever activated.
fn unregistered_agent_arn(region: &str, id: &str) -> String {
    assert_eq!(id.len(), 17, "an agent id is 17 characters, got {id:?}");
    format!("arn:aws:datasync:{region}:{ACCOUNT}:agent/agent-{id}")
}

/// One operation that NAMES agents, with a request that is well-formed apart from the
/// agents it points at.
struct AgentNaming {
    case: &'static str,
    operation: &'static str,
    /// The request, built from the agent ARNs the case wants named.
    request: fn(&[String]) -> serde_json::Value,
}

/// Every operation of this service whose request NAMES agents, with the member that
/// carries them: `OnPremConfig.AgentArns` for NFS and `AgentArns` for the rest.
fn agent_naming_operations() -> Vec<AgentNaming> {
    vec![
        AgentNaming {
            case: "CreateLocationNfs, agents under OnPremConfig",
            operation: "CreateLocationNfs",
            request: |agents| {
                serde_json::json!({
                    "Subdirectory": "/export",
                    "ServerHostname": "nas.example.com",
                    "OnPremConfig": { "AgentArns": agents },
                })
            },
        },
        AgentNaming {
            case: "CreateLocationHdfs",
            operation: "CreateLocationHdfs",
            request: |agents| {
                serde_json::json!({
                    "NameNodes": [{ "Hostname": "namenode.example.com", "Port": 8020 }],
                    "AuthenticationType": "SIMPLE",
                    "SimpleUser": "hadoop",
                    "AgentArns": agents,
                })
            },
        },
        AgentNaming {
            case: "CreateLocationSmb",
            operation: "CreateLocationSmb",
            request: |agents| {
                serde_json::json!({
                    "Subdirectory": "/share",
                    "ServerHostname": "fileserver.example.com",
                    "User": "guest",
                    "Password": "sh4re",
                    "AgentArns": agents,
                })
            },
        },
        AgentNaming {
            case: "CreateLocationObjectStorage",
            operation: "CreateLocationObjectStorage",
            request: |agents| {
                serde_json::json!({
                    "ServerHostname": "store.example.com",
                    "BucketName": "transfer-bucket",
                    "AgentArns": agents,
                })
            },
        },
        AgentNaming {
            case: "CreateLocationAzureBlob",
            operation: "CreateLocationAzureBlob",
            request: |agents| {
                serde_json::json!({
                    "ContainerUrl": "https://hotstore.blob.core.windows.net/transfer-in",
                    "AuthenticationType": "NONE",
                    "AgentArns": agents,
                })
            },
        },
        AgentNaming {
            case: "CreateLocationS3",
            operation: "CreateLocationS3",
            request: |agents| {
                serde_json::json!({
                    "S3BucketArn": "arn:aws:s3:::transfer-bucket",
                    "S3Config": { "BucketAccessRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/datasync") },
                    "AgentArns": agents,
                })
            },
        },
    ]
}

/// RULE: an operation that names a DataSync agent by its `AgentArn` resolves that ARN
/// against the agents this account has activated before it does anything else, and
/// refuses an ARN no activated agent answers to with the modelled client error (HTTP 400
/// `InvalidRequestException`) and the message `"<agent-id> does not exist or has been
/// deleted."` — identically wherever an agent is named.
///
/// Each row asserts the whole refusal envelope against the one `DescribeAgent` gives for
/// the SAME ARN, so the two cannot drift apart. The rows span what the rule generalises
/// over: which operation names the agent, which member carries it
/// (`OnPremConfig.AgentArns` or `AgentArns`), how many ARNs the list holds, which
/// position in the list the unregistered one sits in, and which Region its ARN names.
#[tokio::test]
async fn every_operation_that_names_an_agent_refuses_one_that_was_never_activated() {
    /// How the unregistered ARN sits among the agents a request names.
    struct Placement {
        case: &'static str,
        /// The id of the agent that is not there.
        unregistered_id: &'static str,
        /// The Region its ARN names.
        region: &'static str,
        /// How many activated agents the request names BEFORE it.
        activated_before: usize,
        /// How many activated agents it names after it.
        activated_after: usize,
    }

    let placements = [
        Placement {
            case: "the only agent named",
            unregistered_id: "0123456789abcdefg",
            region: SIGNED_REGION,
            activated_before: 0,
            activated_after: 0,
        },
        Placement {
            case: "the first of two, the second activated",
            unregistered_id: "zyxwvutsrqponmlkj",
            region: SIGNED_REGION,
            activated_before: 0,
            activated_after: 1,
        },
        Placement {
            case: "the last of three, the others activated",
            unregistered_id: "9fedcba987654321f",
            region: SIGNED_REGION,
            activated_before: 2,
            activated_after: 0,
        },
        Placement {
            case: "an agent of another Region",
            unregistered_id: "abcdef01234567890",
            region: "eu-central-1",
            activated_before: 1,
            activated_after: 1,
        },
    ];

    for placement in &placements {
        for naming in agent_naming_operations() {
            let case = format!("{}: {}", naming.case, placement.case);
            // One account per case, so the agents named below are the only ones held.
            let router = service();
            let activated = given_agents(
                &router,
                placement.activated_before + placement.activated_after,
                &case,
            )
            .await;

            let unregistered = unregistered_agent_arn(placement.region, placement.unregistered_id);
            let mut named: Vec<String> = activated[..placement.activated_before].to_vec();
            named.push(unregistered.clone());
            named.extend_from_slice(&activated[placement.activated_before..]);

            // What the agent surface answers for that ARN: the refusal every operation
            // that names the agent owes its caller.
            let from_describe = refused(
                &router,
                "DescribeAgent",
                serde_json::json!({ "AgentArn": &unregistered }),
                &case,
            )
            .await;
            assert_eq!(
                string(&from_describe, "message", &case),
                format!("agent-{id} does not exist or has been deleted.", id = placement.unregistered_id),
                "case {case:?}: DescribeAgent must name the agent id the request carried, \
                 got {from_describe}"
            );

            let refusal = refused(&router, naming.operation, (naming.request)(&named), &case).await;
            assert_eq!(
                refusal, from_describe,
                "case {case:?}: {operation} must refuse the unregistered agent exactly as \
                 DescribeAgent does, got {refusal}",
                operation = naming.operation
            );

            // Nothing was stored: the refused create minted no location at all.
            let listed = ok(&router, "ListLocations", serde_json::json!({}), &case).await;
            let held = listed["Locations"].as_array().map(Vec::len).unwrap_or(0);
            assert_eq!(
                held, 0,
                "case {case:?}: the refusal must come before anything is stored, got {listed}"
            );
        }
    }
}

/// RULE (the other half of the same resolution): an `AgentArn` every activated agent of
/// the account answers to is accepted, and the created location reports exactly the agents
/// it was given.
///
/// The rows differ along the count of agents named, from one through the modelled maximum
/// of eight (`AgentArnList` is `len 1..8`).
#[tokio::test]
async fn an_activated_agent_is_accepted_and_reported_by_the_location_it_serves() {
    for count in [1usize, 2, 8] {
        let case = format!("{count} activated agents");
        let router = service();
        let agents = given_agents(&router, count, &case).await;

        let created = ok(
            &router,
            "CreateLocationSmb",
            serde_json::json!({
                "Subdirectory": "/share",
                "ServerHostname": "fileserver.example.com",
                "User": "transfer",
                "Password": "sh4re",
                "AgentArns": agents,
            }),
            &case,
        )
        .await;
        let arn = string(&created, "LocationArn", &case);
        assert_location_arn(&arn, &case);

        let described = ok(
            &router,
            "DescribeLocationSmb",
            serde_json::json!({ "LocationArn": &arn }),
            &case,
        )
        .await;
        assert_eq!(
            described["AgentArns"],
            serde_json::json!(agents),
            "case {case:?}: expected the agents the request named, got {described}"
        );
    }
}

/// RULE: `CreateLocationNfs` stores the whole location it mints and `DescribeLocationNfs`
/// reports it from that state, with the mount version the request asked for or the
/// `AUTOMATIC` default the member documents.
///
/// The rows span what the rule generalises over: the `Subdirectory` spellings that denote
/// one export path, and every `NfsVersion` the model admits
/// (`smithy_query(query="shape",shape="NfsVersion")` -> `AUTOMATIC | NFS3 | NFS4_0 |
/// NFS4_1 | NFS4_2`) beside a request that declares none.
#[tokio::test]
async fn created_nfs_locations_are_reported_from_state() {
    struct Row {
        case: &'static str,
        subdirectory: &'static str,
        server_hostname: &'static str,
        version: Option<&'static str>,
        expected_uri: &'static str,
        expected_version: &'static str,
    }

    let rows = [
        Row {
            case: "no mount options at all",
            subdirectory: "/export",
            server_hostname: "nas.example.com",
            version: None,
            expected_uri: "nfs://nas.example.com/export/",
            expected_version: "AUTOMATIC",
        },
        Row {
            case: "the default named explicitly, export path without slashes",
            subdirectory: "export/share",
            server_hostname: "nas.example.com",
            version: Some("AUTOMATIC"),
            expected_uri: "nfs://nas.example.com/export/share/",
            expected_version: "AUTOMATIC",
        },
        Row {
            case: "version 3, trailing slash",
            subdirectory: "/export/legacy/",
            server_hostname: "10.0.0.7",
            version: Some("NFS3"),
            expected_uri: "nfs://10.0.0.7/export/legacy/",
            expected_version: "NFS3",
        },
        Row {
            case: "version 4.0",
            subdirectory: "/v4",
            server_hostname: "files.example.org",
            version: Some("NFS4_0"),
            expected_uri: "nfs://files.example.org/v4/",
            expected_version: "NFS4_0",
        },
        Row {
            case: "version 4.1",
            subdirectory: "/v41",
            server_hostname: "files.example.org",
            version: Some("NFS4_1"),
            expected_uri: "nfs://files.example.org/v41/",
            expected_version: "NFS4_1",
        },
        Row {
            case: "version 4.2",
            subdirectory: "/v42",
            server_hostname: "files.example.org",
            version: Some("NFS4_2"),
            expected_uri: "nfs://files.example.org/v42/",
            expected_version: "NFS4_2",
        },
    ];

    let router = service();
    let agents = given_agents(&router, 1, "activating the suite's agent").await;
    let mut minted: Vec<String> = Vec::new();

    for row in &rows {
        let mut request = serde_json::json!({
            "Subdirectory": row.subdirectory,
            "ServerHostname": row.server_hostname,
            "OnPremConfig": { "AgentArns": agents },
            "Tags": [{ "Key": "team", "Value": "storage" }],
        });
        if let Some(version) = row.version {
            request["MountOptions"] = serde_json::json!({ "Version": version });
        }

        let created = ok(&router, "CreateLocationNfs", request, row.case).await;
        let arn = string(&created, "LocationArn", row.case);
        assert_location_arn(&arn, row.case);
        assert!(
            !minted.contains(&arn),
            "case {:?}: every create mints a fresh ARN, but {arn:?} was minted before",
            row.case
        );
        minted.push(arn.clone());

        let described = ok(
            &router,
            "DescribeLocationNfs",
            serde_json::json!({ "LocationArn": &arn }),
            row.case,
        )
        .await;
        assert_eq!(
            string(&described, "LocationArn", row.case),
            arn,
            "case {:?}: the describe must report the ARN it was asked about, got {described}",
            row.case
        );
        assert_eq!(
            string(&described, "LocationUri", row.case),
            row.expected_uri,
            "case {:?}: expected the file server and export path the request named, \
             got {described}",
            row.case
        );
        assert_eq!(
            described["OnPremConfig"],
            serde_json::json!({ "AgentArns": agents }),
            "case {:?}: expected the agents the request named, got {described}",
            row.case
        );
        assert_eq!(
            string(&described["MountOptions"], "Version", row.case),
            row.expected_version,
            "case {:?}: expected mount version {:?}, got {described}",
            row.case,
            row.expected_version
        );
        assert!(
            described["CreationTime"].as_f64().unwrap_or_default() > 0.0,
            "case {:?}: expected the creation time the location was stored with, got {described}",
            row.case
        );
    }
}

/// RULE: `CreateLocationSmb` stores the whole location it mints and `DescribeLocationSmb`
/// reports it from that state, with the authentication protocol and the mount version the
/// request asked for or the defaults their members document (`NTLM` and `AUTOMATIC`).
///
/// The rows span what the rule generalises over: both `SmbAuthenticationType` values
/// (`NTLM | KERBEROS`), every `SmbVersion` the model admits (`AUTOMATIC | SMB1 | SMB2 |
/// SMB2_0 | SMB3`) beside a request that declares none, a domain and a Kerberos principal
/// against neither, and `DnsIpAddresses` at each bound of the modelled `len 0..2`.
#[tokio::test]
async fn created_smb_locations_are_reported_from_state() {
    struct Row {
        case: &'static str,
        request: serde_json::Value,
        expected_uri: &'static str,
        expected_authentication_type: &'static str,
        expected_version: &'static str,
        expected_dns: serde_json::Value,
    }

    let rows = vec![
        Row {
            case: "defaults: no authentication type, no mount options, no DNS servers",
            request: serde_json::json!({
                "Subdirectory": "/share",
                "ServerHostname": "fileserver.example.com",
                "User": "transfer",
                "Password": "sh4re",
            }),
            expected_uri: "smb://fileserver.example.com/share/",
            expected_authentication_type: "NTLM",
            expected_version: "AUTOMATIC",
            expected_dns: serde_json::json!([]),
        },
        Row {
            case: "NTLM with a domain, one DNS server and SMB 3",
            request: serde_json::json!({
                "Subdirectory": "share/finance/",
                "ServerHostname": "dc1.example.com",
                "User": "transfer",
                "Domain": "EXAMPLE",
                "Password": "sh4re",
                "AuthenticationType": "NTLM",
                "MountOptions": { "Version": "SMB3" },
                "DnsIpAddresses": ["10.0.0.2"],
            }),
            expected_uri: "smb://dc1.example.com/share/finance/",
            expected_authentication_type: "NTLM",
            expected_version: "SMB3",
            expected_dns: serde_json::json!(["10.0.0.2"]),
        },
        Row {
            case: "Kerberos with a principal, two DNS servers and SMB 2.1",
            request: serde_json::json!({
                "Subdirectory": "/kerberos",
                "ServerHostname": "kdc.example.com",
                "AuthenticationType": "KERBEROS",
                "KerberosPrincipal": "HOST/transfer@EXAMPLE.ORG",
                "KerberosKeytab": "a2V5dGFiCg==",
                "KerberosKrb5Conf": "W2xpYmRlZmF1bHRzXQo=",
                "MountOptions": { "Version": "SMB2" },
                "DnsIpAddresses": ["10.0.0.2", "10.0.0.3"],
            }),
            expected_uri: "smb://kdc.example.com/kerberos/",
            expected_authentication_type: "KERBEROS",
            expected_version: "SMB2",
            expected_dns: serde_json::json!(["10.0.0.2", "10.0.0.3"]),
        },
        Row {
            case: "SMB 1",
            request: serde_json::json!({
                "Subdirectory": "/legacy",
                "ServerHostname": "old.example.com",
                "User": "transfer",
                "Password": "sh4re",
                "MountOptions": { "Version": "SMB1" },
            }),
            expected_uri: "smb://old.example.com/legacy/",
            expected_authentication_type: "NTLM",
            expected_version: "SMB1",
            expected_dns: serde_json::json!([]),
        },
        Row {
            case: "SMB 2.0",
            request: serde_json::json!({
                "Subdirectory": "/legacy2",
                "ServerHostname": "old.example.com",
                "User": "transfer",
                "Password": "sh4re",
                "MountOptions": { "Version": "SMB2_0" },
            }),
            expected_uri: "smb://old.example.com/legacy2/",
            expected_authentication_type: "NTLM",
            expected_version: "SMB2_0",
            expected_dns: serde_json::json!([]),
        },
    ];

    let router = service();
    let agents = given_agents(&router, 1, "activating the suite's agent").await;
    let mut managed_secrets: Vec<String> = Vec::new();

    for row in &rows {
        let mut request = row.request.clone();
        request["AgentArns"] = serde_json::json!(agents);

        let created = ok(&router, "CreateLocationSmb", request.clone(), row.case).await;
        let arn = string(&created, "LocationArn", row.case);
        assert_location_arn(&arn, row.case);

        let described = ok(
            &router,
            "DescribeLocationSmb",
            serde_json::json!({ "LocationArn": &arn }),
            row.case,
        )
        .await;
        assert_eq!(
            string(&described, "LocationUri", row.case),
            row.expected_uri,
            "case {:?}: expected the file server and share the request named, got {described}",
            row.case
        );
        assert_eq!(
            string(&described, "AuthenticationType", row.case),
            row.expected_authentication_type,
            "case {:?}: expected authentication type {:?}, got {described}",
            row.case,
            row.expected_authentication_type
        );
        assert_eq!(
            string(&described["MountOptions"], "Version", row.case),
            row.expected_version,
            "case {:?}: expected mount version {:?}, got {described}",
            row.case,
            row.expected_version
        );
        assert_eq!(
            described["DnsIpAddresses"], row.expected_dns,
            "case {:?}: expected the DNS servers the request named, got {described}",
            row.case
        );
        // `User`, `Domain` and `KerberosPrincipal` are each optional, so a location
        // reports exactly the ones its own request named.
        for member in ["User", "Domain", "KerberosPrincipal"] {
            assert_eq!(
                described[member], request[member],
                "case {:?}: expected {member} to be reported as the request named it, \
                 got {described}",
                row.case
            );
        }
        assert!(
            described["CreationTime"].as_f64().unwrap_or_default() > 0.0,
            "case {:?}: expected the creation time the location was stored with, got {described}",
            row.case
        );

        // The password never comes back: DataSync keeps it in a secret it manages, and
        // the location reports only that secret's ARN.
        let body = described.to_string();
        assert!(
            !body.contains("sh4re"),
            "case {:?}: the password must not be reported at all, got {described}",
            row.case
        );
        if request["Password"].is_string() {
            let secret = string(&described["ManagedSecretConfig"], "SecretArn", row.case);
            assert!(
                !managed_secrets.contains(&secret),
                "case {:?}: each location gets its own DataSync-managed secret, got {secret:?} \
                 again",
                row.case
            );
            managed_secrets.push(secret);
        }
    }
}

/// RULE: `CreateLocationHdfs` stores the whole location it mints and
/// `DescribeLocationHdfs` reports it from that state, with the block size, replication
/// factor and Quality of Protection settings the request asked for or the defaults their
/// members document (128 MiB, three DataNodes and `PRIVACY` for both protection
/// settings, each the other's value when only one is named).
///
/// The rows span what the rule generalises over: a request that declares none of the
/// cluster settings against one that declares each, both `HdfsAuthenticationType` values
/// (`SIMPLE | KERBEROS`), one NameNode against several, the documented subdirectory
/// default, each published bound of `HdfsBlockSize` (`1048576..1073741824`) and of
/// `HdfsReplicationFactor` (`1..512`), and a `QopConfiguration` naming one setting only.
#[tokio::test]
async fn created_hdfs_locations_are_reported_from_state() {
    struct Row {
        case: &'static str,
        request: serde_json::Value,
        expected_uri: &'static str,
        expected_block_size: i64,
        expected_replication_factor: i64,
        expected_rpc_protection: &'static str,
        expected_data_transfer_protection: &'static str,
    }

    let rows = vec![
        Row {
            case: "defaults: no subdirectory, block size, replication factor or QOP",
            request: serde_json::json!({
                "NameNodes": [{ "Hostname": "namenode.example.com", "Port": 8020 }],
                "AuthenticationType": "SIMPLE",
                "SimpleUser": "hadoop",
            }),
            expected_uri: "hdfs://namenode.example.com:8020/",
            expected_block_size: 128 * 1024 * 1024,
            expected_replication_factor: 3,
            expected_rpc_protection: "PRIVACY",
            expected_data_transfer_protection: "PRIVACY",
        },
        Row {
            case: "every cluster setting at its published minimum, two NameNodes",
            request: serde_json::json!({
                "Subdirectory": "/data/warehouse",
                "NameNodes": [
                    { "Hostname": "nn1.example.com", "Port": 1 },
                    { "Hostname": "nn2.example.com", "Port": 65536 },
                ],
                "BlockSize": 1048576,
                "ReplicationFactor": 1,
                "AuthenticationType": "SIMPLE",
                "SimpleUser": "hadoop",
                "QopConfiguration": {
                    "RpcProtection": "AUTHENTICATION",
                    "DataTransferProtection": "INTEGRITY",
                },
            }),
            expected_uri: "hdfs://nn1.example.com:1/data/warehouse/",
            expected_block_size: 1048576,
            expected_replication_factor: 1,
            expected_rpc_protection: "AUTHENTICATION",
            expected_data_transfer_protection: "INTEGRITY",
        },
        Row {
            case: "every cluster setting at its published maximum",
            request: serde_json::json!({
                "Subdirectory": "max/",
                "NameNodes": [{ "Hostname": "nn3.example.com", "Port": 9000 }],
                "BlockSize": 1073741824,
                "ReplicationFactor": 512,
                "AuthenticationType": "SIMPLE",
                "SimpleUser": "hadoop",
                "QopConfiguration": { "RpcProtection": "DISABLED", "DataTransferProtection": "DISABLED" },
            }),
            expected_uri: "hdfs://nn3.example.com:9000/max/",
            expected_block_size: 1073741824,
            expected_replication_factor: 512,
            expected_rpc_protection: "DISABLED",
            expected_data_transfer_protection: "DISABLED",
        },
        Row {
            case: "one protection setting named, the other assuming the same value",
            request: serde_json::json!({
                "Subdirectory": "/qop",
                "NameNodes": [{ "Hostname": "nn4.example.com", "Port": 8020 }],
                "AuthenticationType": "SIMPLE",
                "SimpleUser": "hadoop",
                "QopConfiguration": { "DataTransferProtection": "AUTHENTICATION" },
            }),
            expected_uri: "hdfs://nn4.example.com:8020/qop/",
            expected_block_size: 128 * 1024 * 1024,
            expected_replication_factor: 3,
            expected_rpc_protection: "AUTHENTICATION",
            expected_data_transfer_protection: "AUTHENTICATION",
        },
        Row {
            case: "Kerberos authentication and a key management server",
            request: serde_json::json!({
                "Subdirectory": "/secure",
                "NameNodes": [{ "Hostname": "nn5.example.com", "Port": 8020 }],
                "AuthenticationType": "KERBEROS",
                "KerberosPrincipal": "hdfs/transfer@EXAMPLE.ORG",
                "KerberosKeytab": "a2V5dGFiCg==",
                "KerberosKrb5Conf": "W2xpYmRlZmF1bHRzXQo=",
                "KmsKeyProviderUri": "kms://http@kms.example.com:9600/kms",
            }),
            expected_uri: "hdfs://nn5.example.com:8020/secure/",
            expected_block_size: 128 * 1024 * 1024,
            expected_replication_factor: 3,
            expected_rpc_protection: "PRIVACY",
            expected_data_transfer_protection: "PRIVACY",
        },
    ];

    let router = service();
    let agents = given_agents(&router, 1, "activating the suite's agent").await;

    for row in &rows {
        let mut request = row.request.clone();
        request["AgentArns"] = serde_json::json!(agents);

        let created = ok(&router, "CreateLocationHdfs", request.clone(), row.case).await;
        let arn = string(&created, "LocationArn", row.case);
        assert_location_arn(&arn, row.case);

        let described = ok(
            &router,
            "DescribeLocationHdfs",
            serde_json::json!({ "LocationArn": &arn }),
            row.case,
        )
        .await;
        assert_eq!(
            string(&described, "LocationUri", row.case),
            row.expected_uri,
            "case {:?}: expected the NameNode the request named, got {described}",
            row.case
        );
        assert_eq!(
            described["NameNodes"], request["NameNodes"],
            "case {:?}: expected the NameNodes the request named, got {described}",
            row.case
        );
        assert_eq!(
            described["BlockSize"].as_i64(),
            Some(row.expected_block_size),
            "case {:?}: expected block size {}, got {described}",
            row.case,
            row.expected_block_size
        );
        assert_eq!(
            described["ReplicationFactor"].as_i64(),
            Some(row.expected_replication_factor),
            "case {:?}: expected replication factor {}, got {described}",
            row.case,
            row.expected_replication_factor
        );
        assert_eq!(
            string(&described["QopConfiguration"], "RpcProtection", row.case),
            row.expected_rpc_protection,
            "case {:?}: expected RPC protection {:?}, got {described}",
            row.case,
            row.expected_rpc_protection
        );
        assert_eq!(
            string(
                &described["QopConfiguration"],
                "DataTransferProtection",
                row.case
            ),
            row.expected_data_transfer_protection,
            "case {:?}: expected data transfer protection {:?}, got {described}",
            row.case,
            row.expected_data_transfer_protection
        );
        assert_eq!(
            string(&described, "AuthenticationType", row.case),
            string(&request, "AuthenticationType", row.case),
            "case {:?}: expected the authentication type the request named, got {described}",
            row.case
        );
        // `SimpleUser`, `KerberosPrincipal` and `KmsKeyProviderUri` are each optional, so
        // a location reports exactly the ones its own request named.
        for member in ["SimpleUser", "KerberosPrincipal", "KmsKeyProviderUri"] {
            assert_eq!(
                described[member], request[member],
                "case {:?}: expected {member} to be reported as the request named it, \
                 got {described}",
                row.case
            );
        }
        assert!(
            described["AgentArns"] == serde_json::json!(agents),
            "case {:?}: expected the agents the request named, got {described}",
            row.case
        );
        assert!(
            described["CreationTime"].as_f64().unwrap_or_default() > 0.0,
            "case {:?}: expected the creation time the location was stored with, got {described}",
            row.case
        );

        // The key table never comes back: DataSync keeps it in a secret it manages.
        if request["KerberosKeytab"].is_string() {
            assert!(
                described["ManagedSecretConfig"]["SecretArn"].is_string(),
                "case {:?}: a Kerberos key table is kept in a DataSync-managed secret, \
                 got {described}",
                row.case
            );
        }
    }
}

/// RULE: a `CreateLocationHdfs` that selects an authentication type without a parameter
/// that type requires is refused with the modelled client error, and stores nothing — each
/// of those parameters documents itself as required for its own authentication type.
///
/// The rows span the set: the parameter `SIMPLE` requires, and each of the three
/// `KERBEROS` requires.
#[tokio::test]
async fn an_hdfs_create_refuses_an_authentication_type_without_its_parameters() {
    struct Row {
        case: &'static str,
        authentication_type: &'static str,
        /// The members of a complete request for that type which this row leaves out.
        omitted: &'static str,
    }

    let rows = [
        Row {
            case: "SIMPLE without a user name",
            authentication_type: "SIMPLE",
            omitted: "SimpleUser",
        },
        Row {
            case: "KERBEROS without a principal",
            authentication_type: "KERBEROS",
            omitted: "KerberosPrincipal",
        },
        Row {
            case: "KERBEROS without a key table",
            authentication_type: "KERBEROS",
            omitted: "KerberosKeytab",
        },
        Row {
            case: "KERBEROS without a krb5 configuration",
            authentication_type: "KERBEROS",
            omitted: "KerberosKrb5Conf",
        },
    ];

    let router = service();
    let agents = given_agents(&router, 1, "activating the suite's agent").await;

    for row in &rows {
        let mut request = serde_json::json!({
            "NameNodes": [{ "Hostname": "namenode.example.com", "Port": 8020 }],
            "AuthenticationType": row.authentication_type,
            "AgentArns": agents,
            "SimpleUser": "hadoop",
            "KerberosPrincipal": "hdfs/transfer@EXAMPLE.ORG",
            "KerberosKeytab": "a2V5dGFiCg==",
            "KerberosKrb5Conf": "W2xpYmRlZmF1bHRzXQo=",
        });
        request
            .as_object_mut()
            .expect("a request object")
            .remove(row.omitted);

        let refusal = refused(&router, "CreateLocationHdfs", request, row.case).await;
        assert_eq!(
            string(&refusal, "message", row.case),
            format!(
                "If {authentication_type} is specified for AuthenticationType, {omitted} is \
                 required.",
                authentication_type = row.authentication_type,
                omitted = row.omitted
            ),
            "case {:?}: expected the refusal to name the parameter the type requires, \
             got {refusal}",
            row.case
        );
    }

    let listed = ok(
        &router,
        "ListLocations",
        serde_json::json!({}),
        "nothing was stored",
    )
    .await;
    let held = listed["Locations"].as_array().map(Vec::len).unwrap_or(0);
    assert_eq!(
        held, 0,
        "a refused create must store nothing at all, got {listed}"
    );
}

/// RULE: a `LocationArn` addresses one location of one kind, so a read of one kind refuses
/// an ARN naming a location of another, and an ARN this account never minted.
///
/// The rows differ along which of the three agent-backed families was created and which
/// family's read was asked for, so no pairing is privileged.
#[tokio::test]
async fn a_file_server_location_read_resolves_only_its_own_kind() {
    let router = service();
    let case = "one location of each agent-backed kind";
    let agents = given_agents(&router, 1, case).await;

    let creates = [
        (
            "DescribeLocationNfs",
            "an NFS",
            "CreateLocationNfs",
            serde_json::json!({
                "Subdirectory": "/export",
                "ServerHostname": "nas.example.com",
                "OnPremConfig": { "AgentArns": agents },
            }),
        ),
        (
            "DescribeLocationSmb",
            "an SMB",
            "CreateLocationSmb",
            serde_json::json!({
                "Subdirectory": "/share",
                "ServerHostname": "fileserver.example.com",
                "User": "transfer",
                "Password": "sh4re",
                "AgentArns": agents,
            }),
        ),
        (
            "DescribeLocationHdfs",
            "an HDFS",
            "CreateLocationHdfs",
            serde_json::json!({
                "NameNodes": [{ "Hostname": "namenode.example.com", "Port": 8020 }],
                "AuthenticationType": "SIMPLE",
                "SimpleUser": "hadoop",
                "AgentArns": agents,
            }),
        ),
    ];

    let mut held: Vec<(usize, String)> = Vec::new();
    for (index, (_, _, operation, request)) in creates.iter().enumerate() {
        let created = ok(&router, operation, request.clone(), case).await;
        held.push((index, string(&created, "LocationArn", case)));
    }

    for (index, arn) in &held {
        for (other, (describe, described_kind, _, _)) in creates.iter().enumerate() {
            let case = format!("{describe} asked about location {index}");
            if other == *index {
                let described = ok(
                    &router,
                    describe,
                    serde_json::json!({ "LocationArn": arn }),
                    &case,
                )
                .await;
                assert_eq!(
                    &string(&described, "LocationArn", &case),
                    arn,
                    "case {case:?}: the read must report its own location, got {described}"
                );
            } else {
                let refusal = refused(
                    &router,
                    describe,
                    serde_json::json!({ "LocationArn": arn }),
                    &case,
                )
                .await;
                assert_eq!(
                    string(&refusal, "message", &case),
                    format!("Location {arn} is not {described_kind} location."),
                    "case {case:?}: expected the wrong-kind refusal, got {refusal}"
                );
            }
        }
    }

    // An ARN this account never minted resolves to nothing in every one of the three.
    let absent = format!("arn:aws:datasync:{SIGNED_REGION}:{ACCOUNT}:location/loc-0abcdef1234567890");
    for (describe, _, _, _) in &creates {
        let case = format!("{describe} asked about a location that was never minted");
        let refusal = refused(
            &router,
            describe,
            serde_json::json!({ "LocationArn": &absent }),
            &case,
        )
        .await;
        assert_eq!(
            string(&refusal, "message", &case),
            format!("Location {absent} is not found."),
            "case {case:?}: expected the not-found refusal, got {refusal}"
        );
    }
}
