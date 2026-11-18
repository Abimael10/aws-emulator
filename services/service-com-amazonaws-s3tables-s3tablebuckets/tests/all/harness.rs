// SPDX-License-Identifier: CC0-1.0

//! The one harness the S3 Tables suites drive the emulator through.
//!
//! Every suite goes over the router (`MultiServiceRouter::dispatch` on a restJson1
//! request whose SigV4 credential scope names the `s3tables` signing name), so each
//! case exercises routing, deserialization, the handler, the persisted state and
//! serialization — the same path an AWS SDK client takes. The request helpers live
//! here rather than in one suite, so two suites cannot disagree about how an
//! operation is addressed.
//!
//! `table_bucket_lifecycle.rs` still carries its own copy of the router bootstrap:
//! it covers the bucket create/read rules and is left to migrate with its next edit
//! rather than be rewritten from under its owner here. (No list of suites is kept
//! here, so this comment cannot rot — `tests/all/main.rs` declares one module per
//! suite file beside it.)

// Each suite uses the part of the harness its own rules need; an item no suite has
// reached yet is still part of the contract this file states.
#![allow(dead_code)]

use std::sync::Arc;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::Value;
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`engine::emulator_account_id()`),
/// restated so an ARN assertion reads whole.
pub const ACCOUNT: &str = "000000000000";

/// The access key id every request this harness sends presents in its SigV4
/// credential scope, which is the identity an audit record attributes a call to.
pub const PRESENTED_AKID: &str = "AKIDTESTS3TABLES";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub error_code: Option<String>,
    pub body: Value,
}

impl Reply {
    /// A member of the body, as a string. Panics when it is absent, because a
    /// required member the model declares is never absent from a success.
    pub fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "response has no string `{member}` member: {body}",
                    body = self.body
                )
            })
            .to_string()
    }

    /// A member of the body, as a string, or `None` when the response carries
    /// none.
    pub fn optional_text(&self, member: &str) -> Option<String> {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// A member of the body, as a list.
    pub fn list(&self, member: &str) -> Vec<Value> {
        self.body
            .get(member)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "response has no list `{member}` member: {body}",
                    body = self.body
                )
            })
    }

    /// The `message` member of an error body, when it carries one.
    pub fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("<no message>")
            .to_string()
    }
}

/// Which path a request arrives on: a client's own, or the in-process callback
/// one emulated service reaches another through. The two differ by nothing but
/// the framework's stamp, which is what a handler reads to tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    Client,
    InProcess,
}

/// The emulator under test: ONE shared engine with the S3 Tables service mounted
/// on it, reached the way the production router reaches it.
pub struct Emulator {
    router: MultiServiceRouter,
    pub engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    pub fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(s3tables::EmulatedService::new(Arc::clone(&engine)));
        // The KMS service beside it, on the SAME engine — the assembly
        // `core/router` performs, minus the other services. A suite that
        // configures SSE-KMS encryption needs a key KMS itself holds
        // ([`Self::create_kms_key`]), because what this service stores for such a
        // configuration is decided by whether KMS resolves the key it names.
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            Arc::clone(&engine),
        ));
        // And STS, so a suite can ask who the calls below speak for the way a
        // client does ([`Self::caller_identity_arn`]) rather than spelling a
        // principal ARN itself.
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// The ARN `sts:GetCallerIdentity` reports for the credential every call
    /// this harness signs with — the principal a refusal of those calls names.
    ///
    /// STS speaks `awsQuery`: a form-encoded `POST /`, answered in XML.
    pub async fn caller_identity_arn(&self, region: &str) -> String {
        let request = http::Request::builder()
            .method("POST")
            .uri("https://sts.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={PRESENTED_AKID}/20260101/{region}/sts/\
                     aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(
                "Action=GetCallerIdentity&Version=2011-06-15".as_bytes().to_vec(),
            ))
            .expect("a well-formed STS request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "STS GetCallerIdentity must succeed, got {status} with {text}"
        );
        let open = "<Arn>";
        let close = "</Arn>";
        let start = text
            .find(open)
            .unwrap_or_else(|| panic!("GetCallerIdentity answers an Arn, got {text}"))
            + open.len();
        let end = text[start..]
            .find(close)
            .unwrap_or_else(|| panic!("the Arn element must close, got {text}"))
            + start;
        text[start..end].to_string()
    }

    /// A fresh customer managed KMS key in `region`, by the ARN KMS minted for
    /// it — created through KMS's OWN `CreateKey`, so the key a configuration
    /// names is one that service answers for.
    ///
    /// KMS speaks `awsJson1_1`: `POST /` with `X-Amz-Target: TrentService.<op>`.
    pub async fn create_kms_key(&self, region: &str) -> String {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://kms.{region}.amazonaws.com/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "TrentService.CreateKey")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={PRESENTED_AKID}/20260101/{region}/kms/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(b"{}".to_vec()))
            .expect("a well-formed KMS request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "KMS CreateKey in {region} must succeed, got {status} {payload}"
        );
        payload["KeyMetadata"]["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("KMS CreateKey must report an ARN, got {payload}"))
            .to_string()
    }

    /// Call one operation over the wire. `region` is the region the request's
    /// SigV4 credential scope names, which is the region the handlers scope an
    /// ARN to.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        region: &str,
        body: Option<&Value>,
    ) -> Reply {
        self.dispatch(method, path, region, body, Arrival::Client)
            .await
    }

    /// Call one operation the way another emulated service reaches this one:
    /// over the in-process callback, which the framework stamps with the
    /// chain's depth. Everything else about the request is what [`Self::call`]
    /// sends.
    pub async fn call_in_process(
        &self,
        method: &str,
        path: &str,
        region: &str,
        body: Option<&Value>,
    ) -> Reply {
        self.dispatch(method, path, region, body, Arrival::InProcess)
            .await
    }

    async fn dispatch(
        &self,
        method: &str,
        path: &str,
        region: &str,
        body: Option<&Value>,
        arrival: Arrival,
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={PRESENTED_AKID}/20260101/{region}/s3tables/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0000"
        );
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://s3tables.{region}.amazonaws.com{path}"))
            .header("authorization", authorization)
            .header("content-type", "application/json");
        if arrival == Arrival::InProcess {
            // The stamp the framework's callback transport puts on every call one
            // emulated service makes into another. It is a request EXTENSION, so
            // it cannot arrive from a socket however a client signs.
            builder = builder.extension(smithy_server_runtime::CallbackRecursionDepth(1));
        }
        let request = builder
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .rsplit('#')
                    .next()
                    .unwrap_or(value)
                    .split(':')
                    .next()
                    .unwrap_or(value)
                    .to_string()
            });
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    pub async fn create_table_bucket(&self, region: &str, request: &Value) -> Reply {
        self.call("PUT", "/buckets", region, Some(request)).await
    }

    pub async fn get_table_bucket(&self, region: &str, arn: &str) -> Reply {
        self.call("GET", &format!("/buckets/{}", encode(arn)), region, None)
            .await
    }

    pub async fn get_table_bucket_encryption(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/buckets/{}/encryption", encode(arn)),
            region,
            None,
        )
        .await
    }

    /// `DeleteTableBucketEncryption`: the bucket's ARN is the one path label.
    pub async fn delete_table_bucket_encryption(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/buckets/{}/encryption", encode(arn)),
            region,
            None,
        )
        .await
    }

    /// `PutTableBucketEncryption`: the bucket's ARN is the one path label and
    /// the configuration travels as the body's `encryptionConfiguration`.
    pub async fn put_table_bucket_encryption(
        &self,
        region: &str,
        arn: &str,
        configuration: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!("/buckets/{}/encryption", encode(arn)),
            region,
            Some(&serde_json::json!({ "encryptionConfiguration": configuration })),
        )
        .await
    }

    /// `GetTableEncryption`: the bucket's ARN, the namespace and the table's
    /// name are path labels.
    pub async fn get_table_encryption(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/tables/{}/{}/{}/encryption",
                encode(arn),
                encode(namespace),
                encode(name)
            ),
            region,
            None,
        )
        .await
    }

    pub async fn put_table_bucket_metrics_configuration(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/buckets/{}/metrics", encode(arn)),
            region,
            None,
        )
        .await
    }

    pub async fn get_table_bucket_metrics_configuration(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/buckets/{}/metrics", encode(arn)),
            region,
            None,
        )
        .await
    }

    pub async fn delete_table_bucket_metrics_configuration(
        &self,
        region: &str,
        arn: &str,
    ) -> Reply {
        self.call(
            "DELETE",
            &format!("/buckets/{}/metrics", encode(arn)),
            region,
            None,
        )
        .await
    }

    /// `PutTableBucketStorageClass`: the bucket's ARN is a path label and the
    /// configuration travels as the body's `storageClassConfiguration`.
    pub async fn put_table_bucket_storage_class(
        &self,
        region: &str,
        arn: &str,
        configuration: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!("/buckets/{}/storage-class", encode(arn)),
            region,
            Some(&serde_json::json!({ "storageClassConfiguration": configuration })),
        )
        .await
    }

    pub async fn get_table_bucket_storage_class(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/buckets/{}/storage-class", encode(arn)),
            region,
            None,
        )
        .await
    }

    /// `GetTableStorageClass`: the bucket's ARN, the namespace and the table's
    /// name are path labels.
    pub async fn get_table_storage_class(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/tables/{}/{}/{}/storage-class",
                encode(arn),
                encode(namespace),
                encode(name)
            ),
            region,
            None,
        )
        .await
    }

    /// `PutTableBucketReplication`: the bucket's ARN and an optional
    /// `versionToken` travel as query members, the configuration as the body's
    /// `configuration`.
    pub async fn put_table_bucket_replication(
        &self,
        region: &str,
        query: &[(&str, String)],
        configuration: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!("/table-bucket-replication{}", query_string(query)),
            region,
            Some(&serde_json::json!({ "configuration": configuration })),
        )
        .await
    }

    /// `GetTableBucketReplication`, which addresses the bucket by a query member.
    pub async fn get_table_bucket_replication(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/table-bucket-replication{}",
                query_string(&[("tableBucketARN", arn.to_string())])
            ),
            region,
            None,
        )
        .await
    }

    /// `DeleteTableBucketReplication` with whichever query members the case
    /// names, so a suite can present a `versionToken` or leave it out.
    pub async fn delete_table_bucket_replication(
        &self,
        region: &str,
        query: &[(&str, String)],
    ) -> Reply {
        self.call(
            "DELETE",
            &format!("/table-bucket-replication{}", query_string(query)),
            region,
            None,
        )
        .await
    }

    /// `PutTableReplication`: the TABLE's ARN and an optional `versionToken`
    /// travel as query members, the configuration as the body's `configuration`.
    pub async fn put_table_replication(
        &self,
        region: &str,
        query: &[(&str, String)],
        configuration: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!("/table-replication{}", query_string(query)),
            region,
            Some(&serde_json::json!({ "configuration": configuration })),
        )
        .await
    }

    /// `GetTableReplication`, which addresses the table by a query member.
    pub async fn get_table_replication(&self, region: &str, table_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/table-replication{}",
                query_string(&[("tableArn", table_arn.to_string())])
            ),
            region,
            None,
        )
        .await
    }

    /// `PutTableRecordExpirationConfiguration`: the table's ARN travels as a
    /// query member and the configuration as the body's `value`.
    pub async fn put_table_record_expiration_configuration(
        &self,
        region: &str,
        table_arn: &str,
        value: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/table-record-expiration{}",
                query_string(&[("tableArn", table_arn.to_string())])
            ),
            region,
            Some(&serde_json::json!({ "value": value })),
        )
        .await
    }

    pub async fn get_table_record_expiration_configuration(
        &self,
        region: &str,
        table_arn: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/table-record-expiration{}",
                query_string(&[("tableArn", table_arn.to_string())])
            ),
            region,
            None,
        )
        .await
    }

    pub async fn get_table_record_expiration_job_status(
        &self,
        region: &str,
        table_arn: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/table-record-expiration-job-status{}",
                query_string(&[("tableArn", table_arn.to_string())])
            ),
            region,
            None,
        )
        .await
    }

    /// `GetTableBucketMaintenanceConfiguration`: the bucket's ARN is the one
    /// path label.
    pub async fn get_table_bucket_maintenance_configuration(
        &self,
        region: &str,
        arn: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!("/buckets/{}/maintenance", encode(arn)),
            region,
            None,
        )
        .await
    }

    /// `PutTableBucketMaintenanceConfiguration`: the maintenance type is a path
    /// label too, and the configuration travels as the body's `value`.
    pub async fn put_table_bucket_maintenance_configuration(
        &self,
        region: &str,
        arn: &str,
        maintenance_type: &str,
        value: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/buckets/{}/maintenance/{}",
                encode(arn),
                encode(maintenance_type)
            ),
            region,
            Some(&serde_json::json!({ "value": value })),
        )
        .await
    }

    /// `GetTableMaintenanceConfiguration`: the bucket's ARN, the namespace and
    /// the table's name are path labels.
    pub async fn get_table_maintenance_configuration(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/tables/{}/{}/{}/maintenance",
                encode(arn),
                encode(namespace),
                encode(name)
            ),
            region,
            None,
        )
        .await
    }

    /// `GetTableMaintenanceJobStatus`: the bucket's ARN, the namespace and the
    /// table's name are path labels.
    pub async fn get_table_maintenance_job_status(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/tables/{}/{}/{}/maintenance-job-status",
                encode(arn),
                encode(namespace),
                encode(name)
            ),
            region,
            None,
        )
        .await
    }

    /// `PutTableMaintenanceConfiguration`: the maintenance type is a path label
    /// too, and the configuration travels as the body's `value`.
    pub async fn put_table_maintenance_configuration(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
        maintenance_type: &str,
        value: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/tables/{}/{}/{}/maintenance/{}",
                encode(arn),
                encode(namespace),
                encode(name),
                encode(maintenance_type)
            ),
            region,
            Some(&serde_json::json!({ "value": value })),
        )
        .await
    }

    /// `ListTagsForResource`: the resource's ARN is the one path label
    /// (`GET /tag/{resourceArn}`), and the resource it names may be a table
    /// bucket or a table.
    pub async fn list_tags_for_resource(&self, region: &str, resource_arn: &str) -> Reply {
        self.call("GET", &format!("/tag/{}", encode(resource_arn)), region, None)
            .await
    }

    pub async fn list_table_buckets(&self, region: &str, query: &[(&str, String)]) -> Reply {
        self.call("GET", &format!("/buckets{}", query_string(query)), region, None)
            .await
    }

    pub async fn delete_table_bucket(&self, region: &str, arn: &str) -> Reply {
        self.call("DELETE", &format!("/buckets/{}", encode(arn)), region, None)
            .await
    }

    pub async fn create_namespace(&self, region: &str, arn: &str, request: &Value) -> Reply {
        self.call(
            "PUT",
            &format!("/namespaces/{}", encode(arn)),
            region,
            Some(request),
        )
        .await
    }

    pub async fn get_namespace(&self, region: &str, arn: &str, namespace: &str) -> Reply {
        self.call(
            "GET",
            &format!("/namespaces/{}/{}", encode(arn), encode(namespace)),
            region,
            None,
        )
        .await
    }

    /// `ListNamespaces`: the bucket's ARN is a path label and `prefix`,
    /// `continuationToken` and `maxNamespaces` travel as query members.
    pub async fn list_namespaces(
        &self,
        region: &str,
        arn: &str,
        query: &[(&str, String)],
    ) -> Reply {
        self.call(
            "GET",
            &format!("/namespaces/{}{}", encode(arn), query_string(query)),
            region,
            None,
        )
        .await
    }

    pub async fn delete_namespace(&self, region: &str, arn: &str, namespace: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/namespaces/{}/{}", encode(arn), encode(namespace)),
            region,
            None,
        )
        .await
    }

    pub async fn create_table(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        request: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!("/tables/{}/{}", encode(arn), encode(namespace)),
            region,
            Some(request),
        )
        .await
    }

    /// `GetTable` addressed by the bucket, the namespace and the name.
    pub async fn get_table(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/get-table{}",
                query_string(&[
                    ("tableBucketARN", arn.to_string()),
                    ("namespace", namespace.to_string()),
                    ("name", name.to_string()),
                ])
            ),
            region,
            None,
        )
        .await
    }

    /// `GetTable` addressed by the table's own ARN.
    pub async fn get_table_by_arn(&self, region: &str, table_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/get-table{}",
                query_string(&[("tableArn", table_arn.to_string())])
            ),
            region,
            None,
        )
        .await
    }

    /// `GetTable` with whichever members the case names, so a suite can address
    /// it incompletely on purpose.
    pub async fn get_table_with(&self, region: &str, query: &[(&str, String)]) -> Reply {
        self.call("GET", &format!("/get-table{}", query_string(query)), region, None)
            .await
    }

    pub async fn list_tables(
        &self,
        region: &str,
        arn: &str,
        query: &[(&str, String)],
    ) -> Reply {
        self.call(
            "GET",
            &format!("/tables/{}{}", encode(arn), query_string(query)),
            region,
            None,
        )
        .await
    }

    pub async fn delete_table(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
        query: &[(&str, String)],
    ) -> Reply {
        self.call(
            "DELETE",
            &format!(
                "/tables/{}/{}/{}{}",
                encode(arn),
                encode(namespace),
                encode(name),
                query_string(query)
            ),
            region,
            None,
        )
        .await
    }

    /// The Iceberg REST catalog's table endpoint for one table: the warehouse
    /// prefix is the table bucket's ARN, so its `:` and `/` travel encoded
    /// exactly as a path label's do.
    pub async fn catalog_table(
        &self,
        method: &str,
        region: &str,
        arn: &str,
        namespace: &str,
        table: &str,
        body: Option<&Value>,
    ) -> Reply {
        self.call(
            method,
            &catalog_table_path(arn, namespace, table),
            region,
            body,
        )
        .await
    }

    /// The catalog's `LoadTable`: the table's Iceberg metadata document.
    pub async fn load_table(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        table: &str,
    ) -> Reply {
        self.catalog_table("GET", region, arn, namespace, table, None)
            .await
    }

    /// The catalog's table existence check.
    pub async fn table_exists(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        table: &str,
    ) -> Reply {
        self.catalog_table("HEAD", region, arn, namespace, table, None)
            .await
    }

    /// The catalog's commit: the requirements and updates travel in the body.
    pub async fn commit_table(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        table: &str,
        request: &Value,
    ) -> Reply {
        self.catalog_table("POST", region, arn, namespace, table, Some(request))
            .await
    }

    /// The catalog's configuration endpoint, with whichever query members the
    /// case names, so a suite can ask for a warehouse or leave it out.
    pub async fn catalog_config(&self, region: &str, query: &[(&str, String)]) -> Reply {
        self.call(
            "GET",
            &format!("/iceberg/v1/config{}", query_string(query)),
            region,
            None,
        )
        .await
    }

    /// The catalog's namespace listing for one warehouse, with whichever query
    /// members the case names.
    pub async fn catalog_namespaces(
        &self,
        region: &str,
        arn: &str,
        query: &[(&str, String)],
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/iceberg/v1/{}/namespaces{}",
                encode(arn),
                query_string(query)
            ),
            region,
            None,
        )
        .await
    }

    /// One namespace of the catalog: `GET` loads it, `HEAD` answers whether it
    /// is there.
    pub async fn catalog_namespace(
        &self,
        method: &str,
        region: &str,
        arn: &str,
        namespace: &str,
    ) -> Reply {
        self.call(
            method,
            &format!(
                "/iceberg/v1/{}/namespaces/{}",
                encode(arn),
                encode(namespace)
            ),
            region,
            None,
        )
        .await
    }

    /// The catalog's table listing for one namespace.
    pub async fn catalog_tables(&self, region: &str, arn: &str, namespace: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/iceberg/v1/{}/namespaces/{}/tables",
                encode(arn),
                encode(namespace)
            ),
            region,
            None,
        )
        .await
    }

    /// `PutTableBucketPolicy`: the bucket's ARN is the one path label and the
    /// document travels as the body's `resourcePolicy`.
    pub async fn put_table_bucket_policy(&self, region: &str, arn: &str, policy: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/buckets/{}/policy", encode(arn)),
            region,
            Some(&serde_json::json!({ "resourcePolicy": policy })),
        )
        .await
    }

    pub async fn get_table_bucket_policy(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/buckets/{}/policy", encode(arn)),
            region,
            None,
        )
        .await
    }

    pub async fn delete_table_bucket_policy(&self, region: &str, arn: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/buckets/{}/policy", encode(arn)),
            region,
            None,
        )
        .await
    }

    /// `GetTableMetadataLocation`: the bucket's ARN, the namespace and the
    /// table's name are path labels.
    pub async fn get_table_metadata_location(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
    ) -> Reply {
        self.call(
            "GET",
            &format!(
                "/tables/{}/{}/{}/metadata-location",
                encode(arn),
                encode(namespace),
                encode(name)
            ),
            region,
            None,
        )
        .await
    }

    /// `UpdateTableMetadataLocation`: addressed like the read above, with
    /// `metadataLocation` and the caller's `versionToken` in the body.
    pub async fn update_table_metadata_location(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
        request: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/tables/{}/{}/{}/metadata-location",
                encode(arn),
                encode(namespace),
                encode(name)
            ),
            region,
            Some(request),
        )
        .await
    }

    /// `TagResource`: the resource's ARN is the one path label and the tags
    /// travel as the body's `tags`.
    pub async fn tag_resource(&self, region: &str, resource_arn: &str, tags: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/tag/{}", encode(resource_arn)),
            region,
            Some(&serde_json::json!({ "tags": tags })),
        )
        .await
    }

    /// `UntagResource`: the resource's ARN is the one path label and the keys to
    /// remove travel as repeated `tagKeys` query parameters
    /// (`DELETE /tag/{resourceArn}?tagKeys=…`).
    pub async fn untag_resource(
        &self,
        region: &str,
        resource_arn: &str,
        tag_keys: &[&str],
    ) -> Reply {
        self.call("DELETE", &untag_path(resource_arn, tag_keys), region, None)
            .await
    }

    /// `UntagResource` as another emulated service reaches it, on the in-process
    /// path the tags this service's own control plane sets travel on.
    pub async fn untag_resource_in_process(
        &self,
        region: &str,
        resource_arn: &str,
        tag_keys: &[&str],
    ) -> Reply {
        self.call_in_process("DELETE", &untag_path(resource_arn, tag_keys), region, None)
            .await
    }

    /// `TagResource` as another emulated service reaches it while provisioning a
    /// resource on a caller's behalf.
    pub async fn tag_resource_in_process(
        &self,
        region: &str,
        resource_arn: &str,
        tags: &Value,
    ) -> Reply {
        self.call_in_process(
            "POST",
            &format!("/tag/{}", encode(resource_arn)),
            region,
            Some(&serde_json::json!({ "tags": tags })),
        )
        .await
    }

    /// `CreateTable` on that same in-process path.
    pub async fn create_table_in_process(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        request: &Value,
    ) -> Reply {
        self.call_in_process(
            "PUT",
            &format!("/tables/{}/{}", encode(arn), encode(namespace)),
            region,
            Some(request),
        )
        .await
    }

    /// `RenameTable`: the bucket's ARN, the namespace and the table's CURRENT
    /// name are path labels, and the coordinates the table moves to —
    /// `newNamespaceName`, `newName` — travel in the body with the caller's
    /// `versionToken`.
    pub async fn rename_table(
        &self,
        region: &str,
        arn: &str,
        namespace: &str,
        name: &str,
        request: &Value,
    ) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/tables/{}/{}/{}/rename",
                encode(arn),
                encode(namespace),
                encode(name)
            ),
            region,
            Some(request),
        )
        .await
    }
}

impl Default for Emulator {
    fn default() -> Self {
        Self::new()
    }
}

/// The `UntagResource` request path for `resource_arn` and the keys to remove:
/// the ARN is the one path label and each key travels as its own `tagKeys` query
/// parameter, so both arrival paths address the operation the same way.
fn untag_path(resource_arn: &str, tag_keys: &[&str]) -> String {
    let query: Vec<(&str, String)> = tag_keys
        .iter()
        .map(|key| ("tagKeys", (*key).to_string()))
        .collect();
    format!("/tag/{}{}", encode(resource_arn), query_string(&query))
}

/// An `@httpLabel` is ONE path segment and a query value ends at the next `&`, so
/// an ARN's `:` and `/` have to travel as `%3A` and `%2F` or the request
/// addresses a different route. A `+` and a SPACE travel escaped for the same
/// reason: a query string decodes a literal `+` as a space and a raw space ends
/// the request line, so a value carrying either would arrive as a different
/// value — which is why SigV4's own URI encoding escapes both.
pub fn encode(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('+', "%2B")
        .replace(' ', "%20")
}

/// The path of the Iceberg REST catalog's table endpoint for one table.
pub fn catalog_table_path(arn: &str, namespace: &str, table: &str) -> String {
    format!(
        "/iceberg/v1/{}/namespaces/{}/tables/{}",
        encode(arn),
        encode(namespace),
        encode(table)
    )
}

/// The query string for `parameters`, or the empty string when there are none.
pub fn query_string(parameters: &[(&str, String)]) -> String {
    if parameters.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = parameters
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect();
    format!("?{}", pairs.join("&"))
}

/// The ARN `CreateTableBucket` must mint for `name` in `region`.
pub fn expected_bucket_arn(region: &str, name: &str) -> String {
    format!("arn:aws:s3tables:{region}:{ACCOUNT}:bucket/{name}")
}

/// A timestamp member as epoch seconds, read from the ONE wire form the model
/// declares for it: `@timestampFormat("date-time")`, so an RFC 3339 string. Any
/// other form — the epoch-seconds number the protocol defaults to — is the defect,
/// not an accepted alternative.
pub fn timestamp_seconds(body: &Value, member: &str, case: &str) -> f64 {
    match body.get(member) {
        Some(Value::String(text)) => DateTime::from_str(text, Format::DateTime)
            .unwrap_or_else(|error| {
                panic!("case {case:?}: {member} {text:?} is not a date-time: {error}")
            })
            .as_secs_f64(),
        actual => panic!(
            "case {case:?}: expected {member} to be an RFC 3339 date-time string, got {actual:?}"
        ),
    }
}

/// Assert `reply` is the modelled 200 of a read or a setup call. The ONE copy of that
/// assertion the suites share.
pub fn assert_served(reply: &Reply, operation: &str, case: &str) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: {operation} must be served, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
}

/// The same assertion for a call whose reply the caller goes on to read.
pub fn served(reply: Reply, operation: &str, case: &str) -> Reply {
    assert_served(&reply, operation, case);
    reply
}

pub fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}
