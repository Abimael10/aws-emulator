// SPDX-License-Identifier: CC0-1.0

//! WHICH half of EC2's id rule a refusal carries, driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol"). EC2 models no
//! error shapes at all, so the wire CODE is the whole contract a client can branch on: an
//! inverted verdict makes a recoverable not-found look like a permanent input bug and the
//! other way round.
//!
//! The rule under test, one level above any one operation: an operation that screens a
//! resource id asks the FORM question BEFORE the EXISTENCE question. A value the service
//! cannot READ as an id of the kind the member names — a value outside the two printed
//! forms, and a long-form value whose obfuscated identifier this service never issued, which
//! there is nothing to decode — is refused before any lookup with the malformed code and
//! sentence that id kind and that operation carry; only a value it CAN read, which resolves
//! to nothing, is refused with the not-found code and sentence. The retired SHORT form
//! carries no identifier to decode, so it is always looked up.
//!
//! The table spans what the rule generalises over: five id KINDS (a VPC, an EBS volume, an
//! AMI and an EC2 instance, plus a customer-owned address pool whose refusals are shaped
//! differently), seven OPERATIONS that screen one of them, both LAYERS a bad id is refused at
//! (the resource's own `Invalid<Kind>.Malformed` and the parameter layer's
//! `InvalidParameterValue`), and three SHAPES of unreadable value beside the readable one.
//! Every never-issued identifier is derived from the STORE's own mint rather than written
//! here, so no row is a recorded value. The last test closes the rule from the other side:
//! an id the mint DID hand out is read and looked up, so the not-found half stays reachable
//! and a live resource is still served.

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use ec2::state::LONG_RESOURCE_ID_DIGITS;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, emulator, error_code, error_message, indexed};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The HTTP status every refusal in this suite carries: a fault of the request.
const REFUSAL: u16 = 400;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// `identifier` as the long-form id of the kind `prefix` names — the form
/// `ComAmazonawsEc2Amazonec2State::next_id` mints.
fn long_id(prefix: &str, identifier: u64) -> String {
    format!(
        "{prefix}-{identifier:0width$x}",
        width = LONG_RESOURCE_ID_DIGITS
    )
}

/// The highest identifier this service's shared mint has handed out, read out of the store
/// the handlers read.
fn issued_identifier(engine: &engine::InMemoryEngine) -> u64 {
    engine.state::<Ec2State>().lock().id_counter
}

/// A long-form id of the kind `prefix` names whose identifier the mint has NOT reached, so
/// this service issued it to nothing at all. Derived from the store's own counter, so it
/// cannot drift from the mint.
fn never_issued(engine: &engine::InMemoryEngine, prefix: &str) -> String {
    long_id(prefix, issued_identifier(engine) + 1_000)
}

/// How one operation names the resource its request is about, given the id under test.
type IdParams = fn(&str) -> Vec<(String, String)>;

/// How a refusal's sentence is built from the id the request sent.
type Sentence = fn(&str) -> String;

/// One id screen under test: the operation, the id kind it screens, and the code and
/// sentence each half of the rule carries there.
struct Screen {
    /// What this screen is, named in every assertion of it.
    case: &'static str,
    /// The `Action` the request carries.
    action: &'static str,
    /// The id prefix of the kind the screened member names.
    prefix: &'static str,
    /// The request members, built around the id under test.
    params: IdParams,
    /// The code of the FORM half.
    malformed_code: &'static str,
    /// The sentence of the FORM half.
    malformed_message: Sentence,
    /// The code of the EXISTENCE half.
    not_found_code: &'static str,
    /// The sentence of the EXISTENCE half.
    not_found_message: Sentence,
}

/// Every screen the rule is asked of.
fn screens() -> Vec<Screen> {
    vec![
        Screen {
            case: "CreateCarrierGateway's VpcId",
            action: "CreateCarrierGateway",
            prefix: "vpc",
            params: |vpc_id| vec![p("VpcId", vpc_id)],
            malformed_code: "InvalidVpcID.Malformed",
            malformed_message: |id| format!("The vpc ID '{id}' is malformed"),
            not_found_code: "InvalidVpcID.NotFound",
            not_found_message: |id| format!("The vpc ID '{id}' does not exist"),
        },
        Screen {
            case: "CopyVolumes' SourceVolumeId",
            action: "CopyVolumes",
            prefix: "vol",
            params: |volume_id| vec![p("SourceVolumeId", volume_id)],
            malformed_code: "InvalidVolumeID.Malformed",
            malformed_message: |id| format!("The volume ID '{id}' is malformed"),
            not_found_code: "InvalidVolume.NotFound",
            not_found_message: |id| format!("The volume '{id}' does not exist."),
        },
        // The same rule at the PARAMETER layer: this operation validates its id LIST as a
        // request parameter, so the FORM half names the parameter and the form it expected.
        Screen {
            case: "DescribeVolumes' VolumeIds, validated as a parameter",
            action: "DescribeVolumes",
            prefix: "vol",
            params: |volume_id| indexed("VolumeId", &[volume_id]),
            malformed_code: "InvalidParameterValue",
            malformed_message: |id| {
                format!("Value ({id}) for parameter volumes is invalid. Expected: 'vol-...'.")
            },
            not_found_code: "InvalidVolume.NotFound",
            not_found_message: |id| format!("The volume '{id}' does not exist."),
        },
        Screen {
            case: "GetImageAncestry's ImageId",
            action: "GetImageAncestry",
            prefix: "ami",
            params: |image_id| vec![p("ImageId", image_id)],
            malformed_code: "InvalidAMIID.Malformed",
            malformed_message: |id| format!("The image ID '{id}' is malformed"),
            not_found_code: "InvalidAMIID.NotFound",
            not_found_message: |id| format!("The image id '[{id}]' does not exist"),
        },
        Screen {
            case: "CreateStoreImageTask's ImageId",
            action: "CreateStoreImageTask",
            prefix: "ami",
            params: |image_id| vec![p("ImageId", image_id), p("Bucket", "ami-archive")],
            malformed_code: "InvalidAMIID.Malformed",
            malformed_message: |id| format!("The image ID '{id}' is malformed"),
            not_found_code: "InvalidAMIID.NotFound",
            not_found_message: |id| format!("The image id '[{id}]' does not exist"),
        },
        Screen {
            case: "AssociateIamInstanceProfile's InstanceId",
            action: "AssociateIamInstanceProfile",
            prefix: "i",
            params: |instance_id| {
                vec![
                    p("InstanceId", instance_id),
                    p("IamInstanceProfile.Name", "app-profile"),
                ]
            },
            malformed_code: "InvalidInstanceID.Malformed",
            malformed_message: |id| format!("The instance ID '{id}' is malformed"),
            not_found_code: "InvalidInstanceID.NotFound",
            not_found_message: |id| format!("The instance ID '{id}' does not exist"),
        },
    ]
}

/// RULE: every screen answers the FORM half for a value it cannot read as an id of its kind
/// — a long-form identifier the mint never handed out, a digit count outside both published
/// forms, and a value that is no id at all — and the EXISTENCE half for the retired short
/// form, which carries no identifier to decode and so is looked up.
#[tokio::test]
async fn every_id_screen_answers_the_half_of_the_rule_its_value_is_due() {
    let (router, engine) = emulator();
    // One served call first, so the store the rows read their never-issued identifiers out
    // of is the store the handlers have already reached.
    let (status, body) = api(&router, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "the Region must report its zones, got {status} with body {body}"
    );

    for screen in screens() {
        let prefix = screen.prefix;
        // (what the row probes, the value, whether the service can READ it as an id)
        let rows: Vec<(String, String, bool)> = vec![
            (
                "a long-form identifier the mint never handed out".to_string(),
                never_issued(&engine, prefix),
                false,
            ),
            (
                "a digit count outside both published forms".to_string(),
                format!("{prefix}-0123456789"),
                false,
            ),
            (
                "a value that is no id at all".to_string(),
                "not-an-id-at-all".to_string(),
                false,
            ),
            (
                "the retired short form, nothing created under it".to_string(),
                format!("{prefix}-0abc1234"),
                true,
            ),
        ];

        for (row, id, readable) in rows {
            let case = format!("{}: {row}", screen.case);
            let params = (screen.params)(&id);
            let (status, body) = api(&router, screen.action, &params).await;
            let (expected_code, expected_message) = if readable {
                (screen.not_found_code, (screen.not_found_message)(&id))
            } else {
                (screen.malformed_code, (screen.malformed_message)(&id))
            };
            assert_eq!(
                (status, error_code(&body).as_str()),
                (REFUSAL, expected_code),
                "case {case:?} ({id}): expected {REFUSAL} {expected_code}, got {status} with \
                 body {body}"
            );
            assert_eq!(
                error_message(&body),
                expected_message,
                "case {case:?} ({id}): expected {expected_message:?}, got body {body}"
            );
        }
    }
}

/// The SAME rule where the refusals are shaped differently: `DeleteCoipPool` answers its own
/// `InvalidIpv4PoolCoipId.Malformed` for a value that is no customer-owned address pool id at
/// all, before any lookup, and `InvalidParameterValue` naming the pool for a value that reads
/// as a pool id and names none — so the two classes are still told apart.
#[tokio::test]
async fn the_address_pool_screen_tells_an_unreadable_id_from_an_absent_pool() {
    let (router, _engine) = emulator();
    let prefix = ec2::state::COIP_POOL_ID_PREFIX;

    // (what the row probes, the value, whether it READS as a pool id of this kind)
    let rows: Vec<(&str, String, bool)> = vec![
        // The resource's own name is not its id prefix: a value carrying it is no pool id.
        (
            "the resource type's name in place of the id prefix",
            format!("{}-00000000000000000", ec2::state::RESOURCE_TYPE_COIP_POOL),
            false,
        ),
        (
            "a non-hexadecimal suffix",
            format!("{prefix}zzzzzzzz"),
            false,
        ),
        (
            "another resource kind's id",
            "subnet-00000000000000000".to_string(),
            false,
        ),
        (
            "a pool id of a published length that names no pool",
            format!("{prefix}00000000000000000"),
            true,
        ),
    ];

    for (case, pool_id, reads_as_one) in rows {
        let (status, body) = api(&router, "DeleteCoipPool", &[p("CoipPoolId", &pool_id)]).await;
        let (expected_code, expected_message) = if reads_as_one {
            (
                ec2::state::INVALID_PARAMETER_VALUE,
                format!("The pool ID '{pool_id}' does not exist."),
            )
        } else {
            (
                ec2::state::INVALID_IPV4_POOL_COIP_ID_MALFORMED,
                format!(
                    "The {resource} ID '{pool_id}' is malformed",
                    resource = ec2::state::RESOURCE_TYPE_COIP_POOL
                ),
            )
        };
        assert_eq!(
            (status, error_code(&body).as_str()),
            (REFUSAL, expected_code),
            "case {case:?} ({pool_id}): expected {REFUSAL} {expected_code}, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case:?} ({pool_id}): expected {expected_message:?}, got body {body}"
        );
    }
}

/// The other side of the rule: an id the mint DID hand out is READ, so it is looked up — a
/// live resource is served, and the same id once the resource is gone is the EXISTENCE half
/// rather than the FORM one. Without this, the form question could swallow every id and the
/// not-found half would be unreachable.
#[tokio::test]
async fn an_id_the_mint_handed_out_is_looked_up_rather_than_refused_as_unreadable() {
    let (router, _engine) = emulator();
    let zone = ec2::state::availability_zones_in_region(REGION)[0]
        .zone_name
        .clone();
    let (status, body) = api(
        &router,
        "CreateVolume",
        &[
            p("AvailabilityZone", zone),
            p("Size", "8"),
            p("VolumeType", "gp3"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the volume under test must be created, got {status} with body {body}"
    );
    let held = element(&body, "volumeId")
        .unwrap_or_else(|| panic!("the create must report the volume it minted: {body}"));

    // Served: the id the mint handed out resolves, so the screens above refused the values
    // they named rather than the operation.
    let (status, body) = api(&router, "DescribeVolumes", &indexed("VolumeId", &[held.as_str()])).await;
    assert_eq!(
        (status, element(&body, "volumeId").as_deref()),
        (200, Some(held.as_str())),
        "the created volume must be describable, got {status} with body {body}"
    );
    let (status, body) = api(
        &router,
        "CopyVolumes",
        &[p("SourceVolumeId", &held), p("VolumeType", "gp3")],
    )
    .await;
    assert_eq!(
        status, 200,
        "a copy of the created volume must be served, got {status} with body {body}"
    );

    // Gone: the SAME id, which the mint did hand out, is now the existence half — for the
    // operation that resolves one named volume and for the one that validates its id list as
    // a parameter alike.
    let (status, body) = api(&router, "DeleteVolume", &[p("VolumeId", &held)]).await;
    assert_eq!(
        status, 200,
        "the volume under test must be deletable, got {status} with body {body}"
    );
    for (case, action, params) in [
        (
            "the operation that resolves one named volume",
            "CopyVolumes",
            vec![p("SourceVolumeId", &held)],
        ),
        (
            "the operation that validates its id list as a parameter",
            "DescribeVolumes",
            indexed("VolumeId", &[held.as_str()]),
        ),
    ] {
        let (status, body) = api(&router, action, &params).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (REFUSAL, "InvalidVolume.NotFound"),
            "case {case:?} on {action}: expected {REFUSAL} InvalidVolume.NotFound, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The volume '{held}' does not exist."),
            "case {case:?} on {action}: unexpected message in body {body}"
        );
    }
}
