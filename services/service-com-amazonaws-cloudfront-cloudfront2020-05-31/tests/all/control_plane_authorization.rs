// SPDX-License-Identifier: CC0-1.0

//! EVERY control-plane operation of this service is authorized by IAM before the service
//! acts, end to end over HTTP.
//!
//! The RULE asserted here: a call to this service is the IAM action
//! `cloudfront:<OperationName>` on the resource the request addresses — the ARN of the
//! resource its route (or, for the tagging calls, its `Resource` parameter) names, and the
//! any-resource wildcard for a call that addresses no one resource — and the identity-based
//! policies in force on the calling principal decide it: an explicit `Deny` about it refuses
//! it whatever grants it, otherwise it needs an `Allow` about it, and with neither it falls
//! to IAM's default deny. A call the policies do not permit is refused with `AccessDenied`
//! (HTTP 403) in IAM's own sentence, and the decision is taken BEFORE the resource is
//! resolved — so a caller holding no grant that names a resource the account does not hold
//! reads that refusal rather than the not-found error, and nothing is created.
//!
//! `invalidation_authorization.rs` states the same rule over the three invalidation
//! operations; what this suite spans is the rest of the surface, which is where the rule
//! holds just as it does there: a READ of a distribution's configuration, a read of the
//! distribution itself, a LISTING of the account's distributions, a CREATE that addresses no
//! resource yet, a read of a resource kind addressed by its own route, and a TAGGING call
//! whose resource travels in a query parameter rather than on the route. Nothing about the
//! grant is specific to an operation: one grant naming an action covers that action alone,
//! and a grant scoped to one distribution's ARN does not reach another's.
//!
//! Every fact the rule needs is staged through the SERVICE that owns it, on one router over
//! one engine: the role and its policy through IAM's own `CreateRole` / `PutRolePolicy`, and
//! the session through STS's own `AssumeRole`, so the documents this service reads are the
//! ones a client really wrote and the requests under test are signed with the access key id
//! STS minted for that session — the credential an assumed-role caller would use.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{batch_xml, config, ACCOUNT_ID, API_VERSION};

/// The hosts the three services' calls are addressed to.
const HOST: &str = "cloudfront.amazonaws.com";
const IAM_HOST: &str = "iam.amazonaws.com";
const STS_HOST: &str = "sts.amazonaws.com";

/// The API versions the two awsQuery services' calls carry.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The access key id the account's OWN published credential presents, which neither store
/// holds a principal for — the credential every row's staging is done with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The name of the one inline policy a row's role carries.
const INLINE_POLICY_NAME: &str = "probe";

/// The session name every `AssumeRole` below asks for.
const SESSION_NAME: &str = "probe-session";

/// The wire error code an authorization refusal carries, and the status it answers with.
const ACCESS_DENIED: &str = "AccessDenied";
const FORBIDDEN: u16 = 403;

/// A cache policy id of the right form that the account holds no policy under is never
/// needed: the rows that read a cache policy read the one the suite stages. This is the
/// `CachePolicyConfig` they stage it with — `Name` and `MinTTL`, the two members the model
/// makes required.
fn cache_policy_config(name: &str) -> String {
    format!(
        "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name><MinTTL>1</MinTTL></CachePolicyConfig>"
    )
}

/// One emulator: CloudFront, IAM and STS over ONE shared engine, the shape
/// `core/router/src/lib.rs` mounts them in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` presenting `access_key_id` and naming `service` in its credential
/// scope, which is what routes the request and what names the principal it speaks for.
fn authorization(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// What the emulator answered: the status and the raw body.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The wire error code (`<Code>` of the restXml error envelope), or `""`.
    fn code(&self) -> String {
        element(&self.body, "Code").unwrap_or_default()
    }

    /// The refusal's message, or `""`.
    fn message(&self) -> String {
        element(&self.body, "Message").unwrap_or_default()
    }
}

/// Call one CloudFront operation signed with `access_key_id`.
async fn cloudfront(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> Answer {
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST)
        .header("authorization", authorization("cloudfront", access_key_id));
    if body.is_some() {
        request = request.header("content-type", "application/xml");
    }
    let request = request
        .body(::bytes::Bytes::from(body.unwrap_or_default().to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Answer { status, body }
}

/// Call one awsQuery operation of `service` as the account's own credential, and answer the
/// XML body. Every call here is setup, so a non-200 fails the case loudly.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(
            &::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        );
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("authorization", authorization(service, ACCOUNT_KEY_ID))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(::bytes::Bytes::from(body))
        .expect("the setup request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    assert_eq!(
        status, 200,
        "setup {action}: expected HTTP 200 from {service}, got {status} with {text}"
    );
    text
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// Create the role `role_name` carrying `statements` as its one inline policy (when the row
/// states any), assume it through STS, and answer the session's access key id with the
/// session ARN a refusal names.
async fn assumed_role(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    statements: &[(&str, &str, String)],
) -> (String, String) {
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"arn:aws:iam::{ACCOUNT_ID}:root\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}"
    );
    query(
        router,
        "iam",
        IAM_HOST,
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    if !statements.is_empty() {
        query(
            router,
            "iam",
            IAM_HOST,
            IAM_VERSION,
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", INLINE_POLICY_NAME.to_string()),
                ("PolicyDocument", policy(statements)),
            ],
        )
        .await;
    }
    let assumed = query(
        router,
        "sts",
        STS_HOST,
        STS_VERSION,
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{ACCOUNT_ID}:role/{role_name}"),
            ),
            ("RoleSessionName", SESSION_NAME.to_string()),
        ],
    )
    .await;
    let access_key_id = element(&assumed, "AccessKeyId")
        .unwrap_or_else(|| panic!("case {case:?}: AssumeRole answered no key id: {assumed}"));
    let arn = element(&assumed, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: AssumeRole answered no session ARN: {assumed}"));
    (access_key_id, arn)
}

/// An identity policy with one statement per `(effect, action, resource)` triple.
fn policy(statements: &[(&str, &str, String)]) -> String {
    let rendered: Vec<String> = statements
        .iter()
        .map(|(effect, action, resource)| {
            ::serde_json::json!({
                "Effect": effect,
                "Action": action,
                "Resource": resource,
            })
            .to_string()
        })
        .collect();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
        rendered.join(",")
    )
}

/// The ARN of a distribution of this account, as this service mints one: CloudFront is
/// GLOBAL, so the Region segment is empty.
fn distribution_arn(id: &str) -> String {
    format!("arn:aws:cloudfront::{ACCOUNT_ID}:distribution/{id}")
}

/// Which of IAM's rules refused the call, as the clause its sentence closes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    /// No readable document allowed the action — IAM's default deny.
    NoPolicyAllows,
    /// An explicit `Deny` in an identity-based document.
    ExplicitDeny,
}

impl Clause {
    /// The clause's own text, written as the literal IAM states rather than through the
    /// implementation's constant.
    fn text(self, action: &str) -> String {
        match self {
            Self::NoPolicyAllows => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// What a row expects of the call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    /// IAM permits it, so the operation answers its own success status.
    Allowed(u16),
    /// IAM refuses it: HTTP 403 `AccessDenied`, whose sentence closes with this clause and
    /// names this resource (`None` for a call that addresses none, where the sentence has
    /// nothing to name).
    Refused(Clause, Option<Resource>),
}

/// Which resource a refusal's sentence names, in terms the row can state before the
/// distributions are created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resource {
    /// The distribution the row's call addresses.
    ThisDistribution,
    /// Its sibling, which the create rows address.
    SiblingDistribution,
}

/// The resources a row's statements may be scoped to, and what the call addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Every resource — the `*` a service-wide grant states.
    Everything,
    /// This distribution's own ARN.
    ThisDistribution,
    /// Its sibling's ARN, which no row's call addresses except the create rows'.
    SiblingDistribution,
}

/// Which call a row makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// `GET /2020-05-31/distribution/{Id}/config` — the read the reported defect was found
    /// on.
    GetDistributionConfig,
    /// `GET /2020-05-31/distribution/{Id}`.
    GetDistribution,
    /// `GET /2020-05-31/distribution` — a listing, which addresses no one resource.
    ListDistributions,
    /// `POST /2020-05-31/distribution` — a create, which addresses no resource yet.
    CreateDistribution,
    /// `GET /2020-05-31/cache-policy/{Id}` — a kind this gate mints no ARN for, so the call
    /// is judged against the any-resource wildcard.
    GetCachePolicy,
    /// `GET /2020-05-31/tagging?Resource=<arn>` — the resource travels in a query parameter
    /// rather than on the route.
    ListTagsForResource,
    /// `POST /2020-05-31/distribution/{Id}/invalidation` on the SIBLING distribution, which
    /// is what a grant scoped to the other one must not reach.
    InvalidateSibling,
    /// `GET /2020-05-31/distribution/{Id}/invalidation`.
    ListInvalidations,
}

impl Call {
    /// The operation's own name, which is the action IAM publishes it as.
    fn operation(self) -> &'static str {
        match self {
            Self::GetDistributionConfig => "GetDistributionConfig",
            Self::GetDistribution => "GetDistribution",
            Self::ListDistributions => "ListDistributions",
            Self::CreateDistribution => "CreateDistribution",
            Self::GetCachePolicy => "GetCachePolicy",
            Self::ListTagsForResource => "ListTagsForResource",
            Self::InvalidateSibling => "CreateInvalidation",
            Self::ListInvalidations => "ListInvalidations",
        }
    }
}

/// What the suite staged once, as the account, for every row to call against.
struct Staged {
    /// The distribution the rows address.
    distribution: String,
    /// Its sibling, which the create rows address and the scoped grants name.
    sibling: String,
    /// The cache policy the cache-policy rows read.
    cache_policy: String,
}

/// One row: the call, the statements the role carries, and the verdict.
struct Row {
    case: &'static str,
    call: Call,
    /// The statements of the role's one inline policy; empty for a role carrying no policy
    /// at all.
    statements: &'static [(&'static str, &'static str, Scope)],
    expected: Expect,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a distribution's configuration, under the grant naming that action",
            call: Call::GetDistributionConfig,
            statements: &[("Allow", "cloudfront:GetDistributionConfig", Scope::Everything)],
            expected: Expect::Allowed(200),
        },
        Row {
            // The reported defect: a role holding exactly these three grants could still
            // read the configuration of the distribution it invalidated.
            case: "a distribution's configuration, under the invalidation grants alone",
            call: Call::GetDistributionConfig,
            statements: &[
                ("Allow", "cloudfront:CreateInvalidation", Scope::Everything),
                ("Allow", "cloudfront:ListDistributions", Scope::Everything),
                ("Allow", "cloudfront:GetDistribution", Scope::Everything),
            ],
            expected: Expect::Refused(Clause::NoPolicyAllows, Some(Resource::ThisDistribution)),
        },
        Row {
            case: "a distribution's configuration, scoped to this distribution's ARN",
            call: Call::GetDistributionConfig,
            statements: &[(
                "Allow",
                "cloudfront:GetDistributionConfig",
                Scope::ThisDistribution,
            )],
            expected: Expect::Allowed(200),
        },
        Row {
            case: "a distribution's configuration, scoped to ANOTHER distribution's ARN",
            call: Call::GetDistributionConfig,
            statements: &[(
                "Allow",
                "cloudfront:GetDistributionConfig",
                Scope::SiblingDistribution,
            )],
            expected: Expect::Refused(Clause::NoPolicyAllows, Some(Resource::ThisDistribution)),
        },
        Row {
            case: "a distribution's configuration, under another service's actions",
            call: Call::GetDistributionConfig,
            statements: &[("Allow", "logs:*", Scope::Everything)],
            expected: Expect::Refused(Clause::NoPolicyAllows, Some(Resource::ThisDistribution)),
        },
        Row {
            case: "a distribution read, under this service's whole namespace",
            call: Call::GetDistribution,
            statements: &[("Allow", "cloudfront:*", Scope::Everything)],
            expected: Expect::Allowed(200),
        },
        Row {
            case: "a distribution read, with an explicit deny beside that namespace grant",
            call: Call::GetDistribution,
            statements: &[
                ("Allow", "cloudfront:*", Scope::Everything),
                ("Deny", "cloudfront:GetDistribution", Scope::ThisDistribution),
            ],
            expected: Expect::Refused(Clause::ExplicitDeny, Some(Resource::ThisDistribution)),
        },
        Row {
            case: "the listing of the account's distributions, under no policy at all",
            call: Call::ListDistributions,
            statements: &[],
            expected: Expect::Refused(Clause::NoPolicyAllows, None),
        },
        Row {
            case: "the listing, under the grant naming that action",
            call: Call::ListDistributions,
            statements: &[("Allow", "cloudfront:ListDistributions", Scope::Everything)],
            expected: Expect::Allowed(200),
        },
        Row {
            case: "a create, under a read's grant alone",
            call: Call::CreateDistribution,
            statements: &[("Allow", "cloudfront:GetDistribution", Scope::Everything)],
            expected: Expect::Refused(Clause::NoPolicyAllows, None),
        },
        Row {
            case: "a create, under the grant naming that action",
            call: Call::CreateDistribution,
            statements: &[("Allow", "cloudfront:CreateDistribution", Scope::Everything)],
            expected: Expect::Allowed(201),
        },
        Row {
            case: "a cache policy read, under no policy at all",
            call: Call::GetCachePolicy,
            statements: &[],
            expected: Expect::Refused(Clause::NoPolicyAllows, None),
        },
        Row {
            case: "a cache policy read, under the grant naming that action",
            call: Call::GetCachePolicy,
            statements: &[("Allow", "cloudfront:GetCachePolicy", Scope::Everything)],
            expected: Expect::Allowed(200),
        },
        Row {
            case: "a cache policy read, under another action's grant",
            call: Call::GetCachePolicy,
            statements: &[("Allow", "cloudfront:GetDistribution", Scope::Everything)],
            expected: Expect::Refused(Clause::NoPolicyAllows, None),
        },
        Row {
            case: "a tagging read, scoped to the distribution it names",
            call: Call::ListTagsForResource,
            statements: &[(
                "Allow",
                "cloudfront:ListTagsForResource",
                Scope::ThisDistribution,
            )],
            expected: Expect::Allowed(200),
        },
        Row {
            case: "a tagging read, scoped to ANOTHER distribution",
            call: Call::ListTagsForResource,
            statements: &[(
                "Allow",
                "cloudfront:ListTagsForResource",
                Scope::SiblingDistribution,
            )],
            expected: Expect::Refused(Clause::NoPolicyAllows, Some(Resource::ThisDistribution)),
        },
        Row {
            case: "an invalidation of the sibling, scoped to the other distribution",
            call: Call::InvalidateSibling,
            statements: &[(
                "Allow",
                "cloudfront:CreateInvalidation",
                Scope::ThisDistribution,
            )],
            expected: Expect::Refused(
                Clause::NoPolicyAllows,
                Some(Resource::SiblingDistribution),
            ),
        },
        Row {
            case: "an invalidation of the sibling, scoped to the sibling",
            call: Call::InvalidateSibling,
            statements: &[(
                "Allow",
                "cloudfront:CreateInvalidation",
                Scope::SiblingDistribution,
            )],
            expected: Expect::Allowed(201),
        },
        Row {
            case: "the invalidations of a distribution, under the create's grant alone",
            call: Call::ListInvalidations,
            statements: &[("Allow", "cloudfront:CreateInvalidation", Scope::Everything)],
            expected: Expect::Refused(Clause::NoPolicyAllows, Some(Resource::ThisDistribution)),
        },
    ]
}

/// Stage, as the ACCOUNT, the two distributions and the cache policy every row calls
/// against.
async fn stage(router: &MultiServiceRouter) -> Staged {
    let distribution = create_distribution(router, "staged", "ref-staged").await;
    let sibling = create_distribution(router, "sibling", "ref-sibling").await;
    let created = cloudfront(
        router,
        ACCOUNT_KEY_ID,
        "POST",
        &format!("/{API_VERSION}/cache-policy"),
        Some(&cache_policy_config("staged-policy")),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateCachePolicy must succeed, got {} with {}",
        created.status, created.body
    );
    let cache_policy = element(&created.body, "Id")
        .unwrap_or_else(|| panic!("setup: no cache policy Id in {}", created.body));
    Staged {
        distribution,
        sibling,
        cache_policy,
    }
}

/// Create one distribution as the account and answer the id this service minted for it.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &'static str,
    reference: &str,
) -> String {
    let created = cloudfront(
        router,
        ACCOUNT_KEY_ID,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&config(case, reference).xml()),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup {case}: CreateDistribution must succeed, got {} with {}",
        created.status, created.body
    );
    element(&created.body, "Id")
        .unwrap_or_else(|| panic!("setup {case}: no distribution Id in {}", created.body))
}

/// Make the row's call as `access_key_id`.
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    call: Call,
    staged: &Staged,
    reference: &str,
) -> Answer {
    let distribution = &staged.distribution;
    let sibling = &staged.sibling;
    match call {
        Call::GetDistributionConfig => {
            cloudfront(
                router,
                access_key_id,
                "GET",
                &format!("/{API_VERSION}/distribution/{distribution}/config"),
                None,
            )
            .await
        }
        Call::GetDistribution => {
            cloudfront(
                router,
                access_key_id,
                "GET",
                &format!("/{API_VERSION}/distribution/{distribution}"),
                None,
            )
            .await
        }
        Call::ListDistributions => {
            cloudfront(
                router,
                access_key_id,
                "GET",
                &format!("/{API_VERSION}/distribution"),
                None,
            )
            .await
        }
        Call::CreateDistribution => {
            cloudfront(
                router,
                access_key_id,
                "POST",
                &format!("/{API_VERSION}/distribution"),
                Some(&config("under test", reference).xml()),
            )
            .await
        }
        Call::GetCachePolicy => {
            cloudfront(
                router,
                access_key_id,
                "GET",
                &format!("/{API_VERSION}/cache-policy/{}", staged.cache_policy),
                None,
            )
            .await
        }
        Call::ListTagsForResource => {
            cloudfront(
                router,
                access_key_id,
                "GET",
                &format!(
                    "/{API_VERSION}/tagging?Resource={}",
                    distribution_arn(distribution)
                ),
                None,
            )
            .await
        }
        Call::InvalidateSibling => {
            cloudfront(
                router,
                access_key_id,
                "POST",
                &format!("/{API_VERSION}/distribution/{sibling}/invalidation"),
                Some(&batch_xml(reference, &["/index.html"])),
            )
            .await
        }
        Call::ListInvalidations => {
            cloudfront(
                router,
                access_key_id,
                "GET",
                &format!("/{API_VERSION}/distribution/{distribution}/invalidation"),
                None,
            )
            .await
        }
    }
}

#[tokio::test]
async fn every_control_plane_call_is_decided_by_the_policies_in_force_on_the_caller() {
    let router = emulator();
    let staged = stage(&router).await;
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let statements: Vec<(&str, &str, String)> = row
            .statements
            .iter()
            .map(|(effect, action, scope)| {
                let resource = match scope {
                    Scope::Everything => "*".to_string(),
                    Scope::ThisDistribution => distribution_arn(&staged.distribution),
                    Scope::SiblingDistribution => distribution_arn(&staged.sibling),
                };
                (*effect, *action, resource)
            })
            .collect();
        let (access_key_id, session) =
            assumed_role(&router, case, &format!("probe-{index}"), &statements).await;

        let answered = call_as(
            &router,
            &access_key_id,
            row.call,
            &staged,
            &format!("ref-row-{index}"),
        )
        .await;

        match row.expected {
            Expect::Allowed(status) => assert_eq!(
                answered.status,
                status,
                "case {case:?}: expected {status} from {}, got {} with {}",
                row.call.operation(),
                answered.status,
                answered.body
            ),
            Expect::Refused(clause, resource) => {
                assert_eq!(
                    answered.status,
                    FORBIDDEN,
                    "case {case:?}: expected HTTP {FORBIDDEN} from {}, got {} with {}",
                    row.call.operation(),
                    answered.status,
                    answered.body
                );
                assert_eq!(
                    answered.code(),
                    ACCESS_DENIED,
                    "case {case:?}: expected the code {ACCESS_DENIED}, got {}",
                    answered.body
                );
                // IAM's own sentence: the session that signed, the action it asked for, the
                // resource it addressed when it addressed one, and which of IAM's rules
                // refused it.
                let action = format!("cloudfront:{}", row.call.operation());
                let named = match resource {
                    Some(Resource::ThisDistribution) => {
                        format!(" on resource: {}", distribution_arn(&staged.distribution))
                    }
                    Some(Resource::SiblingDistribution) => {
                        format!(" on resource: {}", distribution_arn(&staged.sibling))
                    }
                    None => String::new(),
                };
                let expected = format!(
                    "User: {session} is not authorized to perform: {action}{named} {}",
                    clause.text(&action)
                );
                assert_eq!(
                    answered.message(),
                    expected,
                    "case {case:?}: expected the sentence {expected:?}, got {}",
                    answered.body
                );
            }
        }
    }
}

/// One row of the ORDER table: whether the role holds a grant covering the action, and the
/// answer a call naming a distribution the account does not hold earns under it.
struct Order {
    case: &'static str,
    /// The statements of the role's one inline policy; empty for a role carrying none.
    statements: &'static [(&'static str, &'static str)],
    expected_code: &'static str,
    expected_status: u16,
}

/// RULE (the ORDER): authorization is decided before the resource is resolved, so a caller
/// holding no grant that names a distribution the account does not hold reads the
/// authorization refusal — while the SAME call under a grant reads that the distribution is
/// not there. Asserted here on a read of the configuration, which is where the reported
/// defect was found; `invalidation_authorization.rs` asserts it on the create.
#[tokio::test]
async fn an_unauthorized_call_is_refused_before_the_resource_is_resolved() {
    /// A distribution id of the right shape that the account holds no distribution under.
    const ABSENT: &str = "E2ABSENTABSENT";
    let rows = [
        Order {
            case: "no grant at all",
            statements: &[],
            expected_code: ACCESS_DENIED,
            expected_status: FORBIDDEN,
        },
        Order {
            case: "a grant covering the action",
            statements: &[("Allow", "cloudfront:*")],
            expected_code: "NoSuchDistribution",
            expected_status: 404,
        },
    ];
    let router = emulator();
    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let statements: Vec<(&str, &str, String)> = row
            .statements
            .iter()
            .map(|(effect, action)| (*effect, *action, "*".to_string()))
            .collect();
        let (access_key_id, _) =
            assumed_role(&router, case, &format!("order-{index}"), &statements).await;
        let answered = cloudfront(
            &router,
            &access_key_id,
            "GET",
            &format!("/{API_VERSION}/distribution/{ABSENT}/config"),
            None,
        )
        .await;
        assert_eq!(
            answered.status, row.expected_status,
            "case {case:?}: expected HTTP {}, got {} with {}",
            row.expected_status, answered.status, answered.body
        );
        assert_eq!(
            answered.code(),
            row.expected_code,
            "case {case:?}: expected the code {}, got {}",
            row.expected_code,
            answered.body
        );
    }
}

/// RULE (the caller this gate does not bound): the account's own published credential holds
/// no identity-based policies, so every call made with it is served — which is what every
/// other suite in this crate relies on.
#[tokio::test]
async fn the_accounts_own_credential_is_served_across_the_surface() {
    let router = emulator();
    let staged = stage(&router).await;
    for (call, status) in [
        (Call::GetDistributionConfig, 200),
        (Call::GetDistribution, 200),
        (Call::ListDistributions, 200),
        (Call::CreateDistribution, 201),
        (Call::GetCachePolicy, 200),
        (Call::ListTagsForResource, 200),
        (Call::InvalidateSibling, 201),
        (Call::ListInvalidations, 200),
    ] {
        let answered = call_as(
            &router,
            ACCOUNT_KEY_ID,
            call,
            &staged,
            &format!("ref-account-{}", call.operation()),
        )
        .await;
        assert_eq!(
            answered.status,
            status,
            "case {:?}: expected {status} for the account's own credential, got {} with {}",
            call.operation(),
            answered.status,
            answered.body
        );
    }
}
