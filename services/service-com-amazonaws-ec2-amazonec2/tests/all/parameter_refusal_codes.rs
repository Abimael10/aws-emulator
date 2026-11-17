// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the CODE and the SENTENCE this service answers a request the MODEL's
//! own rules refuse — an absent `@required` member, a value outside a declared enum catalog,
//! a page size outside a declared `@range` — driven over the wire the service speaks:
//! `ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML.
//!
//! EC2 declares no error SHAPES at all (`smithy_query(query="operation", …)` reports
//! `errors: []` for every operation), so the ec2Query `<Code>` is the whole contract a client
//! branches on. The rules under test:
//!
//! 1. A parameter the request LEFT OUT is `MissingParameter`, naming that parameter as the
//!    model declares it, whichever operation was called and whatever kind the member is.
//! 2. A value outside a modelled CATALOG this service has EC2's wording for carries EC2's own
//!    code and that type's own sentence — never the boundary's validation clause.
//! 3. A name the INSTANCE-TYPE catalog does not hold is `InvalidInstanceType` naming every
//!    such name, for the operation that reports ON the catalog; for a member that carries the
//!    type as a parameter VALUE it stays `InvalidParameterValue`.
//! 4. A page size outside the range an operation publishes is a page size it will not page
//!    with, in EC2's own code and sentence, with the BOUND taken from that operation's own
//!    `@range` and the parameter spelling from the operation.
//! 5. A modelled enum value this service reads without regard to case is SERVED, so the
//!    request reaches the parameters a handler refuses it for.
//!
//! Every table row is a distinct input along what each rule generalises over — which
//! operation, which member kind, which catalog, which bound, which case of a value — so the
//! request any one recording made is one row rather than the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, emulator, error_code, error_message, indexed};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// One `action` call carrying `params`, as its HTTP status, error code and message.
async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> (String, String) {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
    );
    (error_code(&body), error_message(&body))
}

/// A `(name, value)` form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

// ---------------------------------------------------------------------------
// Rule 1: a parameter the request left out
// ---------------------------------------------------------------------------

/// EC2's sentence for a parameter the request left out, spelled here independently of the
/// service so the suite pins the WIRE text.
fn missing_parameter_message(parameter: &str) -> String {
    format!(
        "The request must include the {parameter} parameter. Add the required parameter and \
         retry the request."
    )
}

/// One row of the absent-parameter table: the case's name, the operation, the form fields it
/// sends, and the parameter the refusal has to name.
type AbsentParameterRow = (
    &'static str,
    &'static str,
    Vec<(String, String)>,
    &'static str,
);

/// A `@required` member the request does not carry is refused `MissingParameter`, naming that
/// member as the model declares it — whichever operation, and whatever kind the member is.
#[tokio::test]
async fn a_parameter_the_request_left_out_is_named_in_its_own_refusal() {
    let (router, _engine) = emulator();

    // Rows span the member KINDS the model declares a required member with, and the
    // operations that carry them: a string id, a list of ids, a list of structures, an enum,
    // and a structure of its own — each named as
    // `smithy_query(query="operation",operation="<the row's action>")` declares it.
    let rows: Vec<AbsentParameterRow> = vec![
        ("a string id", "DeleteVpc", Vec::new(), "VpcId"),
        (
            "a list of ids",
            "ReleaseHosts",
            Vec::new(),
            "HostIds",
        ),
        (
            "a list of structures",
            "CreateClientVpnEndpoint",
            vec![
                p("ClientCidrBlock", "10.0.0.0/22"),
                p("ServerCertificateArn", "arn:aws:acm:us-east-1:000000000000:certificate/x"),
                p("ConnectionLogOptions.Enabled", "false"),
            ],
            "AuthenticationOptions",
        ),
        (
            "an enum",
            "ModifyVpcBlockPublicAccessOptions",
            Vec::new(),
            "InternetGatewayBlockMode",
        ),
        (
            "a structure",
            "CreateFleet",
            vec![p(
                "LaunchTemplateConfigs.1.LaunchTemplateSpecification.LaunchTemplateId",
                "lt-00000000000000001",
            )],
            "TargetCapacitySpecification",
        ),
    ];

    for (case, action, params, parameter) in rows {
        let (code, message) = refused(&router, case, action, &params).await;
        assert_eq!(
            code,
            ec2::state::MISSING_PARAMETER,
            "case {case:?}: expected {} from {action}, got {code:?}",
            ec2::state::MISSING_PARAMETER
        );
        assert_eq!(
            message,
            missing_parameter_message(parameter),
            "case {case:?}: expected the sentence naming {parameter}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a value outside a modelled catalog
// ---------------------------------------------------------------------------

/// One row of the catalog table: the case's name, the operation, the form fields it sends,
/// and the error code and sentence it has to be refused with.
type CatalogRow = (
    &'static str,
    &'static str,
    Vec<(String, String)>,
    &'static str,
    String,
);

/// A value outside a modelled catalog this service has EC2's wording for carries EC2's own
/// code and the sentence EC2 words for that parameter TYPE.
#[tokio::test]
async fn a_value_outside_a_modelled_catalog_carries_its_own_sentence() {
    let (router, _engine) = emulator();

    // One row per catalog this service has EC2's wording for, each with a value outside it:
    // the data export's output format, a Capacity Reservation's instance platform, a reported
    // reason code, and the billing-request caller role. The sentences are spelled here
    // independently of the service.
    let account = ::engine::emulator_account_id();
    let rows: Vec<CatalogRow> = vec![
        (
            "an output format the catalog does not hold",
            "CreateCapacityManagerDataExport",
            vec![
                p("S3BucketName", "capacity-exports"),
                p("Schedule", "hourly"),
                p("OutputFormat", "xml"),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
            "The value 'xml' for OutputFormat isn't valid. Valid values: CSV or Parquet."
                .to_string(),
        ),
        (
            "an instance platform the catalog does not hold",
            "CreateCapacityReservation",
            vec![
                p("InstanceType", "m5.large"),
                p("InstancePlatform", "Not A Real Platform"),
                p("InstanceCount", "1"),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
            format!("Invalid instance platform: Not A Real Platform , accountId: {account}"),
        ),
        (
            "a reason code the catalog does not hold",
            "ReportInstanceStatus",
            vec![
                p("InstanceId.1", "i-00000000000000001"),
                p("Status", "impaired"),
                p("ReasonCode.1", "not-a-real-reason-code"),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
            "invalid reason code not-a-real-reason-code".to_string(),
        ),
        (
            "a caller role the catalog does not hold",
            "DescribeCapacityReservationBillingRequests",
            vec![p("Role", "invalid-role-value")],
            ec2::state::INVALID_PARAMETER_VALUE,
            "invalid-role-value is not a valid value for RoleType. Specify either odcr-owner \
             or unused-reservation-billing-owner."
                .to_string(),
        ),
    ];

    for (case, action, params, expected_code, expected_message) in rows {
        let (code, message) = refused(&router, case, action, &params).await;
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected {expected_code} from {action}, got {code:?}"
        );
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected {expected_message:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the instance types a report is asked about
// ---------------------------------------------------------------------------

/// A type the catalog does not hold, asked of the operation that reports ON the catalog, is
/// `InvalidInstanceType` naming EVERY such type the request supplied, in the order it named
/// them — while a member that carries the type as a parameter VALUE keeps
/// `InvalidParameterValue`.
#[tokio::test]
async fn instance_types_the_catalog_does_not_hold_are_reported_as_nonexistent() {
    let (router, _engine) = emulator();

    // A type the model DOES declare, so the mixed row below is refused for the unknown names
    // alone (the first value of the generated catalog, read from the model rather than
    // written here).
    let published = ec2::ops::types::INSTANCE_TYPE_VALUES[0];
    // The same published name in another ASCII case, which this member is NOT read without
    // regard to case and so is a name the catalog does not hold.
    let wrong_case = published.to_uppercase();

    // Rows: one unknown type, two of them, one beside a published one, and one whose
    // spelling differs from a published type only in case (EC2 holds this member to the
    // value set as declared).
    let rows: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        ("one unknown type", vec!["t9.enormous"], vec!["t9.enormous"]),
        (
            "two unknown types",
            vec!["t9.enormous", "z1.colossal"],
            vec!["t9.enormous", "z1.colossal"],
        ),
        (
            "an unknown type beside a published one",
            vec![published, "t9.enormous"],
            vec!["t9.enormous"],
        ),
        (
            "a published type in the wrong case",
            vec![wrong_case.as_str()],
            vec![wrong_case.as_str()],
        ),
    ];

    for (case, asked, unknown) in rows {
        let (code, message) = refused(
            &router,
            case,
            "DescribeInstanceTypes",
            &indexed("InstanceType", &asked),
        )
        .await;
        assert_eq!(
            code,
            ec2::state::INVALID_INSTANCE_TYPE,
            "case {case:?}: expected {} for {asked:?}, got {code:?}",
            ec2::state::INVALID_INSTANCE_TYPE
        );
        assert_eq!(
            message,
            format!(
                "The following supplied instance types do not exist: [{}]",
                unknown.join(", ")
            ),
            "case {case:?}: expected the unknown types named in request order, got {message:?}"
        );
    }

    // The same value, carried as the type a resource is PLACED as, is the value of a
    // parameter rather than a lookup of the catalog.
    let (code, _message) = refused(
        &router,
        "the type a launch places an instance as",
        "RunInstances",
        &[
            p("ImageId", "ami-00000000000000001"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("InstanceType", "t9.enormous"),
        ],
    )
    .await;
    assert_eq!(
        code,
        ec2::state::INVALID_PARAMETER_VALUE,
        "the type a launch places an instance as: expected {}, got {code:?}",
        ec2::state::INVALID_PARAMETER_VALUE
    );

    // A request naming only published types is not refused for its types at all.
    let (status, body) = call(
        &router,
        REGION,
        "DescribeInstanceTypes",
        &indexed("InstanceType", &[published]),
    )
    .await;
    assert_eq!(
        status, 200,
        "a published type must be described, got {status} with body {body}"
    );
}

// ---------------------------------------------------------------------------
// Rule 4: a page size outside the published range
// ---------------------------------------------------------------------------

/// A page size outside the range an operation publishes is refused with EC2's own code for a
/// page size it will not page with, naming the value, the parameter and the bound the
/// operation's own `@range` publishes.
#[tokio::test]
async fn a_page_size_outside_the_published_range_names_the_bound_it_missed() {
    let (router, _engine) = emulator();

    // Rows: the two operations whose own wording EC2 was recorded with, one past each of
    // their published bounds, and two further paged describes that answer the wording every
    // operation with no recorded sentence of its own does — one of them with a DIFFERENT
    // published ceiling, so the bound in the sentence is read from the model rather than
    // copied from another operation.
    //
    // (case, action, the page size, the code, the sentence)
    let rows: Vec<(&str, &str, &str, &str, String)> = vec![
        (
            "above the ceiling DescribeSecurityGroups publishes",
            "DescribeSecurityGroups",
            "1001",
            ec2::state::INVALID_PARAMETER_VALUE,
            "Value ( 1001 ) for parameter maxResults is invalid. Expecting a value smaller \
             than 1000."
                .to_string(),
        ),
        (
            "below the floor DescribeSecurityGroups publishes",
            "DescribeSecurityGroups",
            "4",
            ec2::state::INVALID_MAX_RESULTS,
            "Value ( 4 ) for parameter maxResults is invalid. Expecting a value greater than 5."
                .to_string(),
        ),
        (
            "above the ceiling DescribeSubnets publishes",
            "DescribeSubnets",
            "1001",
            ec2::state::INVALID_PARAMETER_VALUE,
            "Value ( 1001 ) for parameter MaxResults is invalid. Expecting a value smaller \
             than or equal to 1000."
                .to_string(),
        ),
        (
            "below the floor DescribeSubnets publishes",
            "DescribeSubnets",
            "4",
            ec2::state::INVALID_MAX_RESULTS,
            "Value ( 4 ) for parameter MaxResults is invalid. Expecting a value greater than \
             or equal to 5."
                .to_string(),
        ),
        (
            // The code is recorded PER BOUND as well as per operation: this operation was
            // recorded answering `InvalidParameterValue` for a page size below its floor,
            // where the two rows above answer EC2's own `InvalidMaxResults` there.
            "below the floor DescribeVpcs publishes",
            "DescribeVpcs",
            "4",
            ec2::state::INVALID_PARAMETER_VALUE,
            "Value ( 4 ) for parameter MaxResults is invalid. Expecting a value greater than \
             or equal to 5."
                .to_string(),
        ),
        (
            "above the ceiling DescribeVpcs publishes",
            "DescribeVpcs",
            "1001",
            ec2::state::INVALID_PARAMETER_VALUE,
            "Value ( 1001 ) for parameter MaxResults is invalid. Expecting a value smaller \
             than or equal to 1000."
                .to_string(),
        ),
        (
            // An operation whose own refusal is recorded answers it for EVERY value below the
            // floor, so a page of none and a negative page are refused in the same words as
            // the positive page size above, not in another dialect.
            "a page of none DescribeVpcs will not page with",
            "DescribeVpcs",
            "0",
            ec2::state::INVALID_PARAMETER_VALUE,
            "Value ( 0 ) for parameter MaxResults is invalid. Expecting a value greater than \
             or equal to 5."
                .to_string(),
        ),
        (
            "a negative page DescribeVpcs will not page with",
            "DescribeVpcs",
            "-1",
            ec2::state::INVALID_PARAMETER_VALUE,
            "Value ( -1 ) for parameter MaxResults is invalid. Expecting a value greater than \
             or equal to 5."
                .to_string(),
        ),
        (
            "above the ceiling an operation with no recorded wording publishes",
            "DescribeTransitGatewayRouteTables",
            "1001",
            ec2::state::INVALID_MAX_RESULTS,
            "Value ( 1001 ) for parameter maxResults is invalid. Expecting a value smaller \
             than or equal to 1000."
                .to_string(),
        ),
        (
            // `DITMaxResults` publishes `range 5..100`, so the bound in the sentence is this
            // operation's own and not the 1000 of the rows above.
            "above the smaller ceiling DescribeInstanceTypes publishes",
            "DescribeInstanceTypes",
            "101",
            ec2::state::INVALID_MAX_RESULTS,
            "Value ( 101 ) for parameter maxResults is invalid. Expecting a value smaller \
             than or equal to 100."
                .to_string(),
        ),
    ];

    for (case, action, page_size, expected_code, expected_message) in rows {
        let (code, message) =
            refused(&router, case, action, &[p("MaxResults", page_size)]).await;
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected {expected_code} from {action}, got {code:?}"
        );
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected {expected_message:?}, got {message:?}"
        );
    }

    // Each published bound itself is served: the refusals above are about what lies OUTSIDE
    // the range, not about naming a page size at all.
    for (case, action, page_size) in [
        ("the floor DescribeSecurityGroups publishes", "DescribeSecurityGroups", "5"),
        ("the ceiling DescribeSecurityGroups publishes", "DescribeSecurityGroups", "1000"),
        ("the floor DescribeSubnets publishes", "DescribeSubnets", "5"),
        ("the ceiling DescribeSubnets publishes", "DescribeSubnets", "1000"),
        ("the floor DescribeVpcs publishes", "DescribeVpcs", "5"),
        ("the ceiling DescribeVpcs publishes", "DescribeVpcs", "1000"),
    ] {
        let (status, body) = call(&router, REGION, action, &[p("MaxResults", page_size)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a published page size must be served, got {status} with body {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 5: a modelled enum value read without regard to case
// ---------------------------------------------------------------------------

/// `CreateInstanceExportTask` reads its `DiskImageFormat` without regard to ASCII case, so a
/// request spelling a published format in another case is SERVED as far as the instance it
/// names — and refused for that instance, in EC2's own sentence for an id that resolves to
/// nothing. A format outside the catalog in any case is still refused for the format.
#[tokio::test]
async fn a_disk_image_format_in_another_case_reaches_the_instance_it_names() {
    let (router, _engine) = emulator();

    // A well-formed instance id this store holds no instance for.
    let instance_id = "i-0000000000deadbee";
    let base = |format: &str| -> Vec<(String, String)> {
        vec![
            p("InstanceId", instance_id),
            p("TargetEnvironment", "vmware"),
            p("ExportToS3.DiskImageFormat", format),
            p("ExportToS3.ContainerFormat", "ova"),
            p("ExportToS3.S3Bucket", "exports"),
        ]
    };

    // Rows: the declared spelling and two others of the same value — each read as the format
    // it spells, so each call is answered for the INSTANCE.
    for case in ["VMDK", "vmdk", "vMdK"] {
        let (code, message) = refused(
            &router,
            case,
            "CreateInstanceExportTask",
            &base(case),
        )
        .await;
        assert_eq!(
            code,
            ec2::state::INVALID_PARAMETER,
            "case {case:?}: expected {} for an instance nothing holds, got {code:?}",
            ec2::state::INVALID_PARAMETER
        );
        assert_eq!(
            message,
            format!(
                "The instance ID provided ({instance_id}) does not exist. Please make sure the \
                 ID is correct."
            ),
            "case {case:?}: expected EC2's sentence for an id that resolves to nothing, got \
             {message:?}"
        );
    }

    // A value the catalog does not hold in ANY case is still refused for the format, so the
    // reading above re-spells a declared value rather than accepting any value at all.
    let (status, body) = call(
        &router,
        REGION,
        "CreateInstanceExportTask",
        &base("qcow2"),
    )
    .await;
    assert_eq!(
        status, 400,
        "a format outside the catalog must be refused, got {status} with body {body}"
    );
    assert_ne!(
        error_code(&body),
        ec2::state::INVALID_PARAMETER,
        "a format outside the catalog is refused for the FORMAT, not for the instance: {body}"
    );
}
