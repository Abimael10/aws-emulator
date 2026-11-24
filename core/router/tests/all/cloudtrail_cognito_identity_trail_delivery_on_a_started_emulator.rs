// SPDX-License-Identifier: CC0-1.0

//! What a LOGGING trail delivers into its bucket on the emulator a customer STARTS: the
//! binary, on its own port, with request authentication on, reached over TLS with
//! SigV4-signed calls — and, for the identity data plane, with no credential at all.
//!
//! RULE: while a trail is logging, every MANAGEMENT call the account serves is delivered into
//! that trail's bucket as an object under the documented key layout
//! (`<key-prefix>/AWSLogs/<account>/CloudTrail/<region>/YYYY/MM/DD/`, each file named
//! `<account>_CloudTrail_<region>_<minute>_<unique>.json.gz`), and a call on the CONTENTS of a
//! resource — a Cognito identity-pool call of the identity flow — is delivered there only
//! while one of that trail's event selectors admits data events of THAT resource type: the
//! same call made while the trail's selectors name another type is delivered nowhere. The
//! delivery is what makes a data event observable at all, since event history carries none,
//! and the rule holds on the emulator as it is STARTED and not only on a router a test
//! assembles in its own process.
//!
//! Two trails of one account are the instrument, each delivering under its own `S3KeyPrefix`
//! into one bucket, so what arrived for one class is never read as the other's:
//!
//! * the MANAGEMENT trail keeps the selectors a create gives it ("trails created without
//!   specific event selectors will be configured to log all read and write management events,
//!   and no data events"), so only management records land under its prefix;
//! * the DATA trail carries an advanced event selector naming the `Data` category throughout
//!   and no management events at all, so ONLY data events can land under its prefix — first
//!   one naming a resource type this account never serves, then the identity-pool one, each
//!   installed BEFORE the trail logs anything. Which data events a trail logs is stated per
//!   resource type rather than by a flag, so the contrast is between two SELECTORS and not
//!   between a trail that logs and one that does not.
//!
//! What a delivered FILE is (a gzip stream whose JSON holds its records under `Records`) and
//! what each RECORD carries (the category, the `managementEvent` flag, the resource it names
//! and the redaction of every secret the call presented) are pinned where the gzip reader
//! lives: the CloudTrail crate's own `tests/all/trail_log_file_delivery_of_data_events.rs`,
//! and `service_common::trail_log_files`' own tests. `core/router/Cargo.toml` is a pinned path
//! in this repository, so this test crate can take no decompressor: here a delivered file is
//! asserted by its key layout and by WHICH trail's prefix it arrived under, which is what the
//! selector decides.
//!
//! The started binary is driven through the harness this test crate publishes for exactly that
//! ([`crate::cfn_deployed_pipeline_on_a_started_emulator::Started`]), so how the emulator is
//! started, trusted and signed for is stated once for every suite that starts it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{Started, REGION};
use crate::query_wire::elements;

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`), which owns
/// the bucket, the pools and the trails, and names every delivered log file's prefix.
const ACCOUNT: &str = "000000000000";

/// The one bucket both trails deliver into, and the key prefix each of them delivers under.
const BUCKET: &str = "cognito-identity-trail-delivery";
const MANAGEMENT_PREFIX: &str = "mgmt";
const DATA_PREFIX: &str = "data";

/// The two trails: the one that keeps a create's own selectors, and the one whose advanced
/// event selector names the `Data` category.
const MANAGEMENT_TRAIL: &str = "cognito-identity-management";
const DATA_TRAIL: &str = "cognito-identity-data";

/// The `X-Amz-Target` prefix of each service called below, and the `awsJson1_1` content type
/// all three speak.
const CLOUDTRAIL_TARGET_PREFIX: &str = "CloudTrail_20131101.";
const IDENTITY_TARGET_PREFIX: &str = "AWSCognitoIdentityService.";
const USER_POOLS_TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService.";
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.1";

/// The `eventCategory` an advanced event selector pins a data event by, and the resource type
/// an identity pool is named by in a record's `resources` and in a selector's `resources.type`
/// field.
const DATA_CATEGORY: &str = "Data";
const IDENTITY_POOL_TYPE: &str = "AWS::Cognito::IdentityPool";

/// Another data-event resource type, naming a class this account never serves at all — no
/// function is created, let alone invoked, anywhere in this suite — which is what the data
/// trail's FIRST selector admits (`DataResource$Type` lists it among the basic types).
const FUNCTION_TYPE: &str = "AWS::Lambda::Function";

/// The segments of a CloudTrail log-file key, and what such a file's name ends with.
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";
const LOG_FILE_SERVICE_SEGMENT: &str = "CloudTrail";
const LOG_FILE_SUFFIX: &str = ".json.gz";

/// The user the fixture user pool signs in, and the password it signs in with.
const POOL_USER: &str = "delivery-user";
const POOL_PASSWORD: &str = "Delivery-Passw0rd!1";

/// How long a delivery is waited for, and how often it is looked for. The wait is on a
/// CONDITION and the span is generous, so a loaded host spends none of it; what it bounds is a
/// delivery that never comes.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How many times an ABSENCE is looked for before it is believed. A delivery is made while the
/// call that caused it is being served — the positive legs below find theirs on the first
/// look — so a prefix that is still empty after this many looks is empty because the selector
/// in force withheld the record and not because the write had not landed yet.
const ABSENCE_LOOKS: usize = 10;

/// One answered call, reduced to what the assertions read.
struct Answer {
    status: u16,
    text: String,
}

impl Answer {
    fn of((status, text): (u16, String)) -> Self {
        Self { status, text }
    }

    fn json(&self) -> Value {
        if self.text.is_empty() {
            return Value::Null;
        }
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

/// The emulator this suite starts, with the calls it makes on it.
struct Account<'a> {
    started: &'a Started,
}

impl Account<'_> {
    /// One SIGNED `awsJson1_1` call that has to be served.
    fn served(&self, signing_name: &str, target: &str, operation: &str, body: &Value) -> Value {
        let answer = Answer::of(self.started.rpc(
            signing_name,
            &format!("{target}{operation}"),
            JSON_CONTENT_TYPE,
            body,
        ));
        assert_eq!(
            answer.status,
            200,
            "{signing_name} {operation} must answer 200, got {} with {}; the log:\n{}",
            answer.status,
            answer.text,
            self.started.log()
        );
        answer.json()
    }

    /// One CloudTrail call that has to be served.
    fn cloudtrail(&self, operation: &str, body: &Value) -> Value {
        self.served("cloudtrail", CLOUDTRAIL_TARGET_PREFIX, operation, body)
    }

    /// One SIGNED Cognito Identity call — the identity-pool administration, which a client
    /// makes with AWS developer credentials.
    fn identity(&self, operation: &str, body: &Value) -> Value {
        self.served("cognito-identity", IDENTITY_TARGET_PREFIX, operation, body)
    }

    /// One Amazon Cognito user pools call.
    fn user_pools(&self, operation: &str, body: &Value) -> Value {
        self.served("cognito-idp", USER_POOLS_TARGET_PREFIX, operation, body)
    }

    /// One UNSIGNED Cognito Identity call, carrying no credential at all and naming the
    /// emulator's own address as its host — the way an end user holding no AWS credential
    /// makes a call of the identity flow, whose model marks it `@optionalAuth`.
    fn identity_anonymously(&self, operation: &str, body: &Value) -> Value {
        let answer = Answer::of(self.started.unsigned(
            &self.started.authority(),
            "POST",
            "/",
            &[
                ("content-type", JSON_CONTENT_TYPE),
                (
                    "x-amz-target",
                    &format!("{IDENTITY_TARGET_PREFIX}{operation}"),
                ),
            ],
            body.to_string().into_bytes(),
        ));
        assert_eq!(
            answer.status,
            200,
            "an unsigned Cognito Identity {operation} must answer 200, got {} with {}; \
             the log:\n{}",
            answer.status,
            answer.text,
            self.started.log()
        );
        answer.json()
    }

    /// The one bucket both trails deliver into, with the policy that lets CloudTrail write a
    /// log file into it — the policy CloudTrail's own documentation states: the ACL check on
    /// the bucket, and the object write under it.
    fn log_bucket(&self) {
        let created = Answer::of(self.started.call(
            "s3",
            "PUT",
            &format!("/{BUCKET}"),
            &[("content-type", "application/xml")],
            Vec::new(),
        ));
        assert_eq!(
            created.status, 200,
            "CreateBucket({BUCKET:?}) must succeed, got {} with {}",
            created.status, created.text
        );
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Sid": "AWSCloudTrailAclCheck",
                    "Effect": "Allow",
                    "Principal": { "Service": "cloudtrail.amazonaws.com" },
                    "Action": "s3:GetBucketAcl",
                    "Resource": format!("arn:aws:s3:::{BUCKET}"),
                },
                {
                    "Sid": "AWSCloudTrailWrite",
                    "Effect": "Allow",
                    "Principal": { "Service": "cloudtrail.amazonaws.com" },
                    "Action": "s3:PutObject",
                    "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                    "Condition": {
                        "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
                    },
                },
            ],
        })
        .to_string();
        let policied = Answer::of(self.started.call(
            "s3",
            "PUT",
            &format!("/{BUCKET}?policy"),
            &[("content-type", "application/json")],
            policy.into_bytes(),
        ));
        assert!(
            (200..300).contains(&policied.status),
            "PutBucketPolicy({BUCKET:?}) must succeed, got {} with {}",
            policied.status,
            policied.text
        );
    }

    /// Every object key the bucket holds under one trail's whole log-file area, as Amazon S3's
    /// own list reports them.
    fn delivered_keys(&self, key_prefix: &str) -> Vec<String> {
        let prefix = format!("{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/");
        let listed = Answer::of(self.started.call(
            "s3",
            "GET",
            &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
            &[],
            Vec::new(),
        ));
        assert_eq!(
            listed.status, 200,
            "ListObjectsV2({BUCKET:?}, {prefix:?}) must succeed, got {} with {}",
            listed.status, listed.text
        );
        let mut keys = elements(&listed.text, "Key");
        keys.sort();
        keys
    }

    /// Look under one trail's log-file area until it holds at least `wanted` objects, or
    /// [`SETTLE_DEADLINE`] has passed — the CONDITION is what is waited for, and the deadline
    /// only bounds a wait that will never be satisfied.
    fn delivered(&self, key_prefix: &str, wanted: usize) -> Vec<String> {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let keys = self.delivered_keys(key_prefix);
            if keys.len() >= wanted || Instant::now() >= deadline {
                return keys;
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
    }

    /// Look under one trail's log-file area [`ABSENCE_LOOKS`] times over and require it EMPTY
    /// every time, so an absence is read after the delivery latency the positive legs
    /// demonstrate rather than before it.
    fn stays_empty(&self, case: &str, key_prefix: &str) {
        for look in 1..=ABSENCE_LOOKS {
            let keys = self.delivered_keys(key_prefix);
            assert!(
                keys.is_empty(),
                "case {case:?}: expected nothing delivered under {key_prefix:?} (look \
                 {look} of {ABSENCE_LOOKS}), got {keys:?}; the log:\n{}",
                self.started.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        }
    }

    /// A user pool with one password client and one confirmed user, and the ID token that
    /// user's sign-in answers: the provider name an identity pool trusts, that client's id,
    /// and the token a login presents under the provider.
    fn user_pool_login(&self) -> (String, String, String) {
        let created = self.user_pools("CreateUserPool", &json!({ "PoolName": "delivery-pool" }));
        let pool_id = created["UserPool"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateUserPool must report an Id, got {created}"))
            .to_string();
        let client = self.user_pools(
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": pool_id,
                "ClientName": "delivery-client",
                "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
            }),
        );
        let client_id = client["UserPoolClient"]["ClientId"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateUserPoolClient must report a ClientId, got {client}"))
            .to_string();
        self.user_pools(
            "AdminCreateUser",
            &json!({
                "UserPoolId": pool_id,
                "Username": POOL_USER,
                "TemporaryPassword": "Temporary-Passw0rd!1",
                "MessageAction": "SUPPRESS",
            }),
        );
        self.user_pools(
            "AdminSetUserPassword",
            &json!({
                "UserPoolId": pool_id,
                "Username": POOL_USER,
                "Password": POOL_PASSWORD,
                "Permanent": true,
            }),
        );
        let signed_in = self.user_pools(
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": POOL_USER, "PASSWORD": POOL_PASSWORD },
            }),
        );
        let id_token = signed_in["AuthenticationResult"]["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("InitiateAuth must report an IdToken, got {signed_in}"))
            .to_string();
        // "Amazon Cognito user pool: `cognito-idp.<region>.amazonaws.com/<YOUR_USER_POOL_ID>`"
        // — the provider name Cognito Identity's model publishes for a user pool.
        (
            format!("cognito-idp.{REGION}.amazonaws.com/{pool_id}"),
            client_id,
            id_token,
        )
    }

    /// One identity pool that admits no unauthenticated identity and trusts `provider`'s
    /// `client_id` — the configuration a client that signs its users in with a user pool
    /// writes — answering the pool's id.
    fn identity_pool(&self, name: &str, provider: &str, client_id: &str) -> String {
        let created = self.identity(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": false,
                "CognitoIdentityProviders": [
                    { "ProviderName": provider, "ClientId": client_id },
                ],
            }),
        );
        created["IdentityPoolId"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateIdentityPool must report an id, got {created}"))
            .to_string()
    }

    /// The two calls of the identity flow against one pool, made anonymously with the login
    /// the user pool issued: mint the identity, then mint its OpenID token.
    fn identity_flow(&self, identity_pool_id: &str, provider: &str, id_token: &str) {
        let minted = self.identity_anonymously(
            "GetId",
            &json!({
                "IdentityPoolId": identity_pool_id,
                "Logins": { provider: id_token },
            }),
        );
        let identity_id = minted["IdentityId"]
            .as_str()
            .unwrap_or_else(|| panic!("GetId must report an IdentityId, got {minted}"))
            .to_string();
        self.identity_anonymously(
            "GetOpenIdToken",
            &json!({
                "IdentityId": identity_id,
                "Logins": { provider: id_token },
            }),
        );
    }

    /// Create one trail delivering under `key_prefix`, answering its ARN. It is not logging
    /// yet: a trail delivers nothing until [`Self::start_logging`], which is what lets a
    /// selector be installed BEFORE the trail logs anything at all.
    fn create_trail(&self, name: &str, key_prefix: &str) -> String {
        let created = self.cloudtrail(
            "CreateTrail",
            &json!({
                "Name": name,
                "S3BucketName": BUCKET,
                "S3KeyPrefix": key_prefix,
                "IsMultiRegionTrail": false,
                "IncludeGlobalServiceEvents": false,
            }),
        );
        created["TrailARN"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateTrail must report a TrailARN, got {created}"))
            .to_string()
    }

    /// Start `trail_arn` logging, and read back that it is.
    fn start_logging(&self, trail_arn: &str) {
        self.cloudtrail("StartLogging", &json!({ "Name": trail_arn }));
        let status = self.cloudtrail("GetTrailStatus", &json!({ "Name": trail_arn }));
        assert_eq!(
            status["IsLogging"],
            json!(true),
            "trail {trail_arn:?} must report it is logging, got {status}; the log:\n{}",
            self.started.log()
        );
    }
}

/// One advanced event selector admitting the data events of one resource type, as
/// `AdvancedEventSelector` carries it: the category, and the type of the resource the call
/// acted on.
fn data_event_selector(name: &str, resource_type: &str) -> Value {
    json!([{
        "Name": name,
        "FieldSelectors": [
            { "Field": "eventCategory", "Equals": [DATA_CATEGORY] },
            { "Field": "resources.type", "Equals": [resource_type] },
        ],
    }])
}

/// Assert `key` is a log-file key of the trail delivering under `key_prefix`: the documented
/// layout down to the DAY, and a file name carrying the account, the service and the Region.
fn assert_log_file_key(case: &str, key_prefix: &str, key: &str) {
    let area = format!(
        "{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/{LOG_FILE_SERVICE_SEGMENT}/{REGION}/"
    );
    let Some(rest) = key.strip_prefix(&area) else {
        panic!("case {case:?}: expected a key under {area:?}, got {key:?}");
    };
    let (day, name) = rest
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected a day folder in {rest:?}"));
    let parts: Vec<&str> = day.split('/').collect();
    let dated = parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts
            .iter()
            .all(|part| part.chars().all(|digit| digit.is_ascii_digit()));
    assert!(
        dated,
        "case {case:?}: expected the YYYY/MM/DD of the records, got {day:?} in {key:?}"
    );
    let expected_name = format!("{ACCOUNT}_{LOG_FILE_SERVICE_SEGMENT}_{REGION}_");
    assert!(
        name.starts_with(&expected_name) && name.ends_with(LOG_FILE_SUFFIX),
        "case {case:?}: expected a file named {expected_name}<minute>_<unique>\
         {LOG_FILE_SUFFIX}, got {name:?}"
    );
}

#[test]
fn a_logging_trail_delivers_identity_pool_data_events_only_once_a_selector_names_them() {
    let started = Started::start();
    let account = Account { started: &started };
    account.log_bucket();

    // The MANAGEMENT trail: a create's own selectors, which is "all read and write management
    // events, and no data events" — the documented default a client reads back before it asks
    // for anything else.
    let management_trail = account.create_trail(MANAGEMENT_TRAIL, MANAGEMENT_PREFIX);
    let default_selectors =
        account.cloudtrail("GetEventSelectors", &json!({ "TrailName": management_trail }));
    assert_eq!(
        default_selectors["EventSelectors"][0]["DataResources"],
        json!([]),
        "case \"a trail created with default selectors\": expected no data resource, got \
         {default_selectors}"
    );
    assert_eq!(
        default_selectors.get("AdvancedEventSelectors"),
        None,
        "case \"a trail created with default selectors\": expected no AdvancedEventSelectors \
         member, got {default_selectors}"
    );

    // The DATA trail: an advanced event selector naming the `Data` category throughout, so no
    // management event can ever land under its prefix. Its first selector names a resource type
    // this account never serves, and it is installed BEFORE the trail logs anything, so every
    // object under its prefix is a data event the selector in force admitted.
    let data_trail = account.create_trail(DATA_TRAIL, DATA_PREFIX);
    let withholding = data_event_selector("functions-only", FUNCTION_TYPE);
    let put = account.cloudtrail(
        "PutEventSelectors",
        &json!({ "TrailName": data_trail, "AdvancedEventSelectors": withholding }),
    );
    assert_eq!(
        put["AdvancedEventSelectors"], withholding,
        "case \"a selector naming another resource type\": PutEventSelectors must echo it, \
         got {put}"
    );
    account.start_logging(&data_trail);
    account.start_logging(&management_trail);

    // The pools of the account, each trusting the user pool that signs its users in: one
    // addressed while no selector names identity-pool data events, one addressed after.
    let (provider, client_id, id_token) = account.user_pool_login();
    let withheld_pool = account.identity_pool("delivery-withheld", &provider, &client_id);
    let selected_pool = account.identity_pool("delivery-selected", &provider, &client_id);

    // A MANAGEMENT call of the same service, which the management trail logs from the moment it
    // started: the positive control a consumer of the bucket reads before judging any absence.
    account.identity(
        "TagResource",
        &json!({
            "ResourceArn":
                format!("arn:aws:cognito-identity:{REGION}:{ACCOUNT}:identitypool/{selected_pool}"),
            "Tags": { "delivery": "management-marker" },
        }),
    );
    let management_keys = account.delivered(MANAGEMENT_PREFIX, 1);
    assert!(
        !management_keys.is_empty(),
        "case \"a management call of a logging trail's Region\": expected a delivered log file \
         under {MANAGEMENT_PREFIX:?}, got none; the log:\n{}",
        started.log()
    );
    for key in &management_keys {
        assert_log_file_key(
            "a management call of a logging trail's Region",
            MANAGEMENT_PREFIX,
            key,
        );
    }
    let status = account.cloudtrail("GetTrailStatus", &json!({ "Name": management_trail }));
    assert!(
        status["LatestDeliveryTime"].is_number(),
        "case \"a trail that has delivered a log file\": expected a LatestDeliveryTime, got \
         {status}"
    );

    // The WITHHELD leg: the identity flow of the first pool, while the only data-event selector
    // of the account names another resource type. Nothing may reach the data trail's prefix,
    // which only a data event can reach at all.
    account.identity_flow(&withheld_pool, &provider, &id_token);
    account.stays_empty(
        "a data call no selector of the account names",
        DATA_PREFIX,
    );

    // The selector that names identity-pool data events, read back verbatim — and reported in
    // place of the basic selectors, not beside them.
    let naming = data_event_selector("identity-pools", IDENTITY_POOL_TYPE);
    let put = account.cloudtrail(
        "PutEventSelectors",
        &json!({ "TrailName": data_trail, "AdvancedEventSelectors": naming }),
    );
    assert_eq!(
        put["AdvancedEventSelectors"], naming,
        "case \"the identity-pool data selector\": PutEventSelectors must echo it, got {put}"
    );
    let read = account.cloudtrail("GetEventSelectors", &json!({ "TrailName": data_trail }));
    assert_eq!(
        read["AdvancedEventSelectors"], naming,
        "case \"the identity-pool data selector\": GetEventSelectors must report it verbatim, \
         got {read}"
    );
    assert_eq!(
        read.get("EventSelectors"),
        None,
        "case \"the identity-pool data selector\": GetEventSelectors must no longer report \
         EventSelectors, got {read}"
    );

    // Still nothing under the data prefix: a record of the withheld pair that arrived late
    // would be found here, so what the next leg finds is the next leg's own.
    account.stays_empty(
        "a data call no selector of the account named, once the selector changed",
        DATA_PREFIX,
    );

    // The DELIVERED leg: the identical two calls of the identity flow, against the pool the
    // selector now names.
    account.identity_flow(&selected_pool, &provider, &id_token);
    let delivered = account.delivered(DATA_PREFIX, 1);
    assert!(
        !delivered.is_empty(),
        "case \"the identity flow of a pool a selector names\": expected the two calls \
         delivered under {DATA_PREFIX:?}, got nothing; the log:\n{}",
        started.log()
    );
    // At most one file per call: CloudTrail may carry both records in one file, and nothing
    // else can arrive here — a record of the withheld pair would show up as a third key.
    assert!(
        delivered.len() <= 2,
        "case \"the identity flow of a pool a selector names\": expected at most one file per \
         call under {DATA_PREFIX:?}, got {delivered:?}; the log:\n{}",
        started.log()
    );
    for key in &delivered {
        assert_log_file_key(
            "the identity flow of a pool a selector names",
            DATA_PREFIX,
            key,
        );
    }
}
