// SPDX-License-Identifier: CC0-1.0

//! The HTTP harness every suite in this crate drives the emulator through: this one
//! service mounted on one shared engine behind the same `MultiServiceRouter` the
//! production entrypoint serves, plus one method per modelled route.
//!
//! Amazon EC2 is mounted BESIDE it on that same engine, because a mount target is placed
//! in a subnet and a subnet is EC2's resource: every subnet, VPC, zone and address range a
//! suite names is one EC2 minted ([`Emulator::account_subnets`]), and the facts a mount
//! target reports about its placement are compared against what EC2 itself answers for
//! that subnet rather than against a value written out in a suite.
//!
//! It lives here, and not in a suite, so a new suite reuses the request builder, the
//! SigV4 scoping the handlers read the Region from, and the clock-settling helper
//! instead of copying them.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_efs_magnolioapiservice_v20150201::state;
use service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService;
use service_common::posix_shell_session::EnvironmentPrograms;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// A SigV4 `Authorization` header scoped to `region` and to EFS's signing name, which
/// is what the router routes on and what the handlers read the Region from.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/elasticfilesystem/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
    )
}

/// One HTTP answer, reduced to what the assertions read.
pub struct Reply {
    pub status: u16,
    /// `x-amzn-errortype`, the header restJson1 resolves an error shape from.
    pub error_type: Option<String>,
    pub body: Value,
    pub body_len: usize,
}

impl Reply {
    pub fn get(&self, key: &str) -> &Value {
        &self.body[key]
    }

    pub fn string(&self, key: &str) -> String {
        self.body[key].as_str().unwrap_or_default().to_string()
    }
}

/// The emulator under test: this one service and Amazon EC2 — the owner of the subnets a
/// mount target is placed in — mounted on one shared engine.
pub struct Emulator {
    router: MultiServiceRouter,
    /// The one engine both services are mounted on, kept so a suite that drives this
    /// service's NATIVE data plane can reach its state accessors the way the component that
    /// delivers a program to a machine does.
    engine: Arc<engine::InMemoryEngine>,
    region: String,
}

impl Emulator {
    pub fn new() -> Self {
        Self::in_region("us-east-1")
    }

    pub fn in_region(region: &str) -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_ec2_amazonec2::EmulatedService::new(Arc::clone(&engine)),
        );
        Self {
            router,
            engine,
            region: region.to_string(),
        }
    }

    /// The NFS client programs this service hands the component that reaches into a machine
    /// standing in `vpc_id`, with the exports this data plane offers read AT `at` — the seam
    /// a guest's own shell session performs `mount` and `umount` through.
    ///
    /// The instant is the caller's, exactly as it is for the component that delivers a program
    /// to a machine, so a suite names the instant it wants the exports read at instead of
    /// waiting for one.
    pub fn nfs_client(&self, at: SystemTime, vpc_id: Option<&str>) -> Box<dyn EnvironmentPrograms> {
        state::nfs_client_programs(&*self.engine, at, vpc_id)
            .expect("the engine this service is mounted on must hold its store")
    }

    /// An EC2 client for this emulator's Region over the router's own in-process callback
    /// — the very seam the EFS handlers resolve a subnet through. No socket is opened: the
    /// endpoint URL is only the address that transport routes by.
    pub fn ec2(&self) -> ::aws_sdk_ec2::Client {
        let callback = EmbeddedEmulator::from_router(self.router.clone());
        let config = ::aws_sdk_ec2::Config::builder()
            .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
            .region(::aws_sdk_ec2::config::Region::new(self.region.clone()))
            .endpoint_url(format!("https://ec2.{}.amazonaws.com", self.region))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        ::aws_sdk_ec2::Client::from_conf(config)
    }

    /// The subnets the account has in this emulator's Region, as EC2 answers for them,
    /// ordered by Availability Zone: the default VPC's default subnets, whose ids EC2
    /// minted. Nothing here derives an identifier, a zone or an address range of its own.
    pub async fn account_subnets(&self) -> Vec<SubnetFacts> {
        let described = self
            .ec2()
            .describe_subnets()
            .send()
            .await
            .expect("EC2 must report the account's subnets");
        let mut facts: Vec<SubnetFacts> = described
            .subnets()
            .iter()
            .filter_map(|subnet| {
                Some(SubnetFacts {
                    subnet_id: subnet.subnet_id()?.to_string(),
                    vpc_id: subnet.vpc_id()?.to_string(),
                    zone_name: subnet.availability_zone()?.to_string(),
                    availability_zone_id: subnet.availability_zone_id()?.to_string(),
                    cidr_block: subnet.cidr_block()?.to_string(),
                })
            })
            .filter(|facts| facts.zone_name.starts_with(&self.region))
            .collect();
        facts.sort_by(|left, right| left.availability_zone_id.cmp(&right.availability_zone_id));
        facts
    }

    /// The `index`-th subnet of [`Self::account_subnets`], for a case that needs just one.
    pub async fn account_subnet(&self, index: usize) -> SubnetFacts {
        let subnets = self.account_subnets().await;
        subnets.get(index).cloned().unwrap_or_else(|| {
            panic!(
                "the account must have at least {} subnet(s) in {}, got {subnets:?}",
                index + 1,
                self.region
            )
        })
    }

    pub async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://elasticfilesystem.{}.amazonaws.com{path}",
                self.region
            ))
            .header("host", format!("elasticfilesystem.{}.amazonaws.com", self.region))
            .header("authorization", authorization(&self.region));
        let bytes = match &body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                bytes::Bytes::from(serde_json::to_vec(value).expect("a serializable body"))
            }
            None => bytes::Bytes::new(),
        };
        let request = builder.body(bytes).expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let payload = response.into_body();
        Reply {
            status,
            error_type,
            body: serde_json::from_slice(&payload).unwrap_or(Value::Null),
            body_len: payload.len(),
        }
    }

    pub async fn create_file_system(&self, body: Value) -> Reply {
        self.send("POST", "/2015-02-01/file-systems", Some(body)).await
    }

    pub async fn describe_file_systems(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/2015-02-01/file-systems".to_string()
        } else {
            format!("/2015-02-01/file-systems?{query}")
        };
        self.send("GET", &path, None).await
    }

    pub async fn delete_file_system(&self, id: &str) -> Reply {
        self.send("DELETE", &format!("/2015-02-01/file-systems/{id}"), None)
            .await
    }

    pub async fn create_mount_target(&self, body: Value) -> Reply {
        self.send("POST", "/2015-02-01/mount-targets", Some(body)).await
    }

    /// `PUT /2015-02-01/mount-targets/{MountTargetId}/security-groups`, the modelled
    /// `ModifyMountTargetSecurityGroups` binding.
    pub async fn modify_mount_target_security_groups(&self, id: &str, body: Value) -> Reply {
        self.send(
            "PUT",
            &format!("/2015-02-01/mount-targets/{id}/security-groups"),
            Some(body),
        )
        .await
    }

    /// `GET /2015-02-01/mount-targets/{MountTargetId}/security-groups`, the modelled
    /// `DescribeMountTargetSecurityGroups` binding.
    pub async fn describe_mount_target_security_groups(&self, id: &str) -> Reply {
        self.send(
            "GET",
            &format!("/2015-02-01/mount-targets/{id}/security-groups"),
            None,
        )
        .await
    }

    pub async fn describe_mount_targets(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/2015-02-01/mount-targets".to_string()
        } else {
            format!("/2015-02-01/mount-targets?{query}")
        };
        self.send("GET", &path, None).await
    }

    pub async fn delete_mount_target(&self, id: &str) -> Reply {
        self.send("DELETE", &format!("/2015-02-01/mount-targets/{id}"), None)
            .await
    }

    /// `DELETE /2015-02-01/access-points/{AccessPointId}`, the modelled
    /// `DeleteAccessPoint` binding. The label takes an id or an access-point ARN, so the
    /// value is percent-encoded nowhere: both forms are sent as the caller wrote them.
    pub async fn delete_access_point(&self, id: &str) -> Reply {
        self.send("DELETE", &format!("/2015-02-01/access-points/{id}"), None)
            .await
    }

    pub async fn update_file_system_protection(&self, id: &str, body: Value) -> Reply {
        self.send(
            "PUT",
            &format!("/2015-02-01/file-systems/{id}/protection"),
            Some(body),
        )
        .await
    }

    /// `GET /2015-02-01/file-systems/{FileSystemId}/backup-policy`, the modelled
    /// `DescribeBackupPolicy` binding.
    pub async fn describe_backup_policy(&self, id: &str) -> Reply {
        self.send(
            "GET",
            &format!("/2015-02-01/file-systems/{id}/backup-policy"),
            None,
        )
        .await
    }

    /// `PUT /2015-02-01/file-systems/{FileSystemId}/backup-policy`, the modelled
    /// `PutBackupPolicy` binding.
    pub async fn put_backup_policy(&self, id: &str, body: Value) -> Reply {
        self.send(
            "PUT",
            &format!("/2015-02-01/file-systems/{id}/backup-policy"),
            Some(body),
        )
        .await
    }

    /// `GET /2015-02-01/file-systems/{FileSystemId}/policy`, the modelled
    /// `DescribeFileSystemPolicy` binding.
    pub async fn describe_file_system_policy(&self, id: &str) -> Reply {
        self.send("GET", &format!("/2015-02-01/file-systems/{id}/policy"), None)
            .await
    }

    /// `PUT /2015-02-01/file-systems/{FileSystemId}/policy`, the modelled
    /// `PutFileSystemPolicy` binding.
    pub async fn put_file_system_policy(&self, id: &str, body: Value) -> Reply {
        self.send(
            "PUT",
            &format!("/2015-02-01/file-systems/{id}/policy"),
            Some(body),
        )
        .await
    }

    /// `DELETE /2015-02-01/file-systems/{FileSystemId}/policy`, the modelled
    /// `DeleteFileSystemPolicy` binding.
    pub async fn delete_file_system_policy(&self, id: &str) -> Reply {
        self.send(
            "DELETE",
            &format!("/2015-02-01/file-systems/{id}/policy"),
            None,
        )
        .await
    }

    pub async fn create_replication_configuration(&self, source_id: &str, body: Value) -> Reply {
        self.send(
            "POST",
            &format!("/2015-02-01/file-systems/{source_id}/replication-configuration"),
            Some(body),
        )
        .await
    }

    pub async fn describe_replication_configurations(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/2015-02-01/file-systems/replication-configurations".to_string()
        } else {
            format!("/2015-02-01/file-systems/replication-configurations?{query}")
        };
        self.send("GET", &path, None).await
    }

    pub async fn delete_replication_configuration(&self, source_id: &str, query: &str) -> Reply {
        let path = format!("/2015-02-01/file-systems/{source_id}/replication-configuration");
        let path = if query.is_empty() {
            path
        } else {
            format!("{path}?{query}")
        };
        self.send("DELETE", &path, None).await
    }

    /// Create one `available` file system, waiting out the clock-owed
    /// `creating` -> `available` transition so a mount target or a replication
    /// configuration can be added to it.
    pub async fn available_file_system(&self, token: &str) -> String {
        let created = self
            .create_file_system(json!({ "CreationToken": token }))
            .await;
        assert_eq!(
            created.status, 201,
            "creating file system {token}: expected 201, got {} {:?}",
            created.status, created.body
        );
        let id = created.string("FileSystemId");
        self.wait_until_available(&id).await;
        id
    }

    /// Look again through `DescribeFileSystems` until `file_system_id` reports
    /// `available` — the clock-owed end of the `creating` span ([`state::CREATING_FOR`])
    /// — or until a deadline far longer than that span has passed.
    ///
    /// The CONDITION is polled rather than a fixed span slept for, so a suite that needs
    /// a settled file system does not depend on how fast the host runs.
    pub async fn wait_until_available(&self, file_system_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let reply = self
                .describe_file_systems(&format!("FileSystemId={file_system_id}"))
                .await;
            if reply.get("FileSystems")[0]["LifeCycleState"].as_str() == Some("available") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "file system {file_system_id}: never reached available, last read {:?}",
                reply.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// How long a poll loop waits before looking again. It bounds nothing: the loop above
/// runs until its CONDITION holds or its own generous deadline passes.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One IAM resource-policy document around `statement`, as the suites that submit a
/// `FileSystemPolicy` build it.
pub fn policy_document(statement: Value) -> Value {
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// Wait past the clock-owed `creating` -> `available` transition. The handlers never
/// sleep for it — they store the due instant — so only a test that wants the settled
/// state waits here.
pub async fn settle() {
    tokio::time::sleep(state::CREATING_FOR + Duration::from_millis(150)).await;
}

/// Wait past the clock-owed removal of a resource an accepted delete moved to
/// `deleting`. Like [`settle`], the handlers store the due instant instead of sleeping
/// for it, so only a test that wants the completed deletion waits here.
pub async fn settle_deletion() {
    tokio::time::sleep(state::DELETING_FOR + Duration::from_millis(150)).await;
}

/// `^fs-[0-9a-f]{8,40}$` / `^fsmt-[0-9a-f]{8,40}$`, the modelled id patterns.
pub fn matches_id_pattern(id: &str, prefix: &str) -> bool {
    match id.strip_prefix(prefix) {
        Some(hex) => {
            (8..=40).contains(&hex.len())
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
        None => false,
    }
}

/// One subnet of the account, exactly as Amazon EC2 reports it. Everything a mount target
/// reports about its placement is compared against these values.
#[derive(Debug, Clone)]
pub struct SubnetFacts {
    pub subnet_id: String,
    pub vpc_id: String,
    pub zone_name: String,
    pub availability_zone_id: String,
    pub cidr_block: String,
}

impl SubnetFacts {
    /// The `offset`-th address of this subnet's block, for a case that names an address
    /// explicitly. Read off the block EC2 reported, so the address is one of the subnet's
    /// own whatever block the account's network has.
    pub fn address_at(&self, offset: u32) -> String {
        let block: ::ipnet::Ipv4Net = self.cidr_block.parse().unwrap_or_else(|error| {
            panic!("EC2's block {:?} must parse: {error}", self.cidr_block)
        });
        Ipv4Addr::from(u32::from(block.network()).saturating_add(offset)).to_string()
    }

    /// Whether `address` lies inside this subnet's block.
    pub fn holds(&self, address: &str) -> bool {
        match (
            self.cidr_block.parse::<::ipnet::Ipv4Net>(),
            address.parse::<Ipv4Addr>(),
        ) {
            (Ok(block), Ok(address)) => block.contains(&address),
            _ => false,
        }
    }
}
