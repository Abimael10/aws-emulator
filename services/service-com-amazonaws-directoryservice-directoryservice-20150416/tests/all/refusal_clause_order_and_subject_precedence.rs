// SPDX-License-Identifier: CC0-1.0

//! Two rules about WHAT ORDER a refused request is answered in, driven end to end over HTTP:
//! `POST /` with `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1`
//! body, dispatched by the multi-service router, so nothing here reaches into the state struct.
//!
//! 1. **The clauses of ONE constraint refusal are stated in this service's own order** — the
//!    order the layer in front of it walks the table of findings it collected, keyed on the
//!    member path each clause is stated at (`service::stated_clause_order`) — which is neither
//!    the order the model declares those members nor the order the request wrote them. Every
//!    clause, the count the envelope states and the member each names stay the framework's;
//!    only where each clause sits in the message is this service's.
//! 2. **A registration resolves the directory it names before it reads the certificate it
//!    carries**: `RegisterCertificate` against an id this account holds no directory under is
//!    the modelled `DirectoryDoesNotExistException`, with this service's one sentence for that
//!    failure, whatever the body holds — and the body is still read once the directory
//!    resolves, which is what shows the check was ordered rather than dropped.
//!
//! The tables span what each rule generalises over: for (1) the pair or triple of members, the
//! operation, the constraint class each clause is about (a member's own expression, one the
//! boundary validator cannot apply, and one a LIST's elements broke) and the order the walk
//! finds them in; for (2) the form of the well-formed directory id and the form of the
//! certificate body, including a body that IS a certificate.

use serde_json::{json, Value};

use crate::certificates::{certificate_pem, describe, directory_in_service, register};
use crate::harness::{call, service, string};

/// A `Password` the modelled expression admits (eight to sixty-four characters with three of
/// Active Directory's four character classes), so a row about another member is not also about
/// this one.
const GOOD_PASSWORD: &str = "Sim2RealPassword";

/// A `Password` that expression does not admit: one character class and far too short.
const WEAK_PASSWORD: &str = "weak";

/// A `Name` the modelled `^([a-zA-Z0-9]+[\\.-])+([a-zA-Z0-9])+$` does not admit: one label,
/// with none of the separators the expression requires between labels.
const SINGLE_LABEL_NAME: &str = "singlelabel";

/// The `Name` expression, as the model declares it and as a clause about that member quotes it.
const NAME_PATTERN: &str = r"^([a-zA-Z0-9]+[\\.-])+([a-zA-Z0-9])+$";

/// A `ShortName` the modelled `^[^\\/:*?"<>|.]+[^\\/:*?"<>|]*$` does not admit: it carries one
/// of the characters that expression excludes.
const BAD_SHORT_NAME: &str = "bad:name";

/// A `Description` the modelled `^([a-zA-Z0-9_])[\\a-zA-Z0-9_@#%*+=:?./!\s-]*$` does not
/// admit: it opens on a character the first class excludes.
const BAD_DESCRIPTION: &str = "-nope";

/// A well-formed `DirectoryId` ( `^d-[0-9a-f]{10}$` ) used where the row is about something
/// other than the id.
const SOME_DIRECTORY_ID: &str = "d-0123456789";

/// The member path each clause of `message` is stated at, in the order the message states them.
fn clause_members(message: &str, case: &str) -> Vec<String> {
    let (_, stated) = message.split_once(" detected: ").unwrap_or_else(|| {
        panic!("case {case:?}: expected a counted validation envelope, got {message:?}")
    });
    stated
        .split("; ")
        .map(|clause| {
            let (_, named) = clause.split_once(" at '").unwrap_or_else(|| {
                panic!("case {case:?}: expected a clause naming a member, got {clause:?}")
            });
            let (path, _) = named.split_once('\'').unwrap_or_else(|| {
                panic!("case {case:?}: expected a quoted member path, got {clause:?}")
            });
            path.to_string()
        })
        .collect()
}

/// One row of a clause table: a request that breaks several members' constraints, and the
/// member paths the one refusal's clauses must be stated at, in order. Shared by both tables
/// below, so the order a row asserts means the same thing in each.
struct ClauseOrderCase {
    case: &'static str,
    operation: &'static str,
    body: Value,
    stated_at: &'static [&'static str],
}

/// RULE 1: the clauses of one refusal come out in this service's order — by the bucket the
/// member path each clause is stated at sits in — whichever order the walk found them and
/// whatever class each clause is about.
///
/// Each row says whether its order is one a RECORDED refusal of this service states, or one
/// the same rule PREDICTS for a member set no recording covers: a change to the width the
/// order is computed at (`service::stated_clause_order::FINDINGS_TABLE_BUCKETS`) may move a
/// predicted row, and must never move a recorded one.
#[tokio::test]
async fn the_clauses_of_one_refusal_are_stated_in_this_services_order() {
    let router = service();

    let rows = vec![
        ClauseOrderCase {
            // RECORDED. The administrator password's expression is one the boundary validator
            // cannot apply and the name's is one it can, so this row is also about the two
            // classes being counted and ordered in ONE envelope.
            case: "a create whose password and name both break their expressions",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": SINGLE_LABEL_NAME,
                "Password": WEAK_PASSWORD,
                "VpcSettings": { "VpcId": "vpc-0a1b2c3d", "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
            }),
            stated_at: &["password", "name"],
        },
        ClauseOrderCase {
            // PREDICTED. The same pair on the operation that creates a Simple AD directory:
            // the order is the member paths', so it does not belong to one operation.
            case: "a Simple AD create whose password and name both break their expressions",
            operation: "CreateDirectory",
            body: json!({
                "Name": SINGLE_LABEL_NAME,
                "Password": WEAK_PASSWORD,
                "Size": "Small",
            }),
            stated_at: &["password", "name"],
        },
        ClauseOrderCase {
            // PREDICTED: a third member beside the recorded pair.
            case: "a create whose password, name and short name all break their expressions",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": SINGLE_LABEL_NAME,
                "ShortName": BAD_SHORT_NAME,
                "Password": WEAK_PASSWORD,
                "VpcSettings": { "VpcId": "vpc-0a1b2c3d", "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
            }),
            stated_at: &["password", "name", "shortName"],
        },
        ClauseOrderCase {
            // PREDICTED. No password violation here, so these three clauses are the
            // framework's own walk alone — and the walk reports them in the model's member
            // order (name, shortName, description), which is not the order they are stated
            // in.
            case: "a create whose name, short name and description break their expressions",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": SINGLE_LABEL_NAME,
                "ShortName": BAD_SHORT_NAME,
                "Description": BAD_DESCRIPTION,
                "Password": GOOD_PASSWORD,
                "VpcSettings": { "VpcId": "vpc-0a1b2c3d", "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
            }),
            stated_at: &["description", "name", "shortName"],
        },
        ClauseOrderCase {
            // RECORDED. Two members of the structure the create NESTS: the clauses state them
            // by their dotted paths, and the order is their member NAMES' — which is the
            // reverse of the order those paths sort in.
            case: "a create whose nested placement breaks both of its members",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": "placement.example.com",
                "Password": GOOD_PASSWORD,
                "VpcSettings": { "VpcId": "bad-vpc", "SubnetIds": ["bad-subnet-1", "bad-subnet-2"] },
            }),
            stated_at: &["vpcSettings.vpcId", "vpcSettings.subnetIds"],
        },
        ClauseOrderCase {
            // PREDICTED. A clause stated at a LIST member beside a plain member's own clause:
            // the walk finds the plain member first, and the list member is stated first.
            case: "a forwarder whose remote domain and whose address list are both refused",
            operation: "CreateConditionalForwarder",
            body: json!({
                "DirectoryId": SOME_DIRECTORY_ID,
                "RemoteDomainName": "not a domain!",
                "DnsIpAddrs": ["999.999.999.999"],
            }),
            stated_at: &["dnsIpAddrs", "remoteDomainName"],
        },
    ];

    for row in rows {
        let (status, parsed) = call(&router, row.operation, row.body.clone()).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the broken constraints to refuse the request, got {status} \
             with {parsed}",
            row.case
        );
        assert_eq!(
            parsed["__type"],
            json!("ValidationException"),
            "case {:?}: expected the framework's constraint refusal, got {parsed}",
            row.case
        );
        let message = parsed["message"].as_str().unwrap_or_default();
        let expected_count = row.stated_at.len();
        let plural = if expected_count == 1 { "error" } else { "errors" };
        assert!(
            message.starts_with(&format!("{expected_count} validation {plural} detected: ")),
            "case {:?}: expected {expected_count} counted clause(s), got {message:?}",
            row.case
        );
        let stated = clause_members(message, row.case);
        assert_eq!(
            stated, row.stated_at,
            "case {:?}: expected the clauses stated at {:?}, got {stated:?} in {message:?}",
            row.case, row.stated_at
        );
    }
}

/// RULE 1, the recorded wording of the pair that shows the order: both clauses in one
/// envelope, the password's first and stating NO value because the model marks that member
/// `@sensitive`, the name's second and quoting the value the request sent.
#[tokio::test]
async fn the_two_clauses_of_a_create_are_worded_as_aws_words_them() {
    let router = service();
    let case = "a password and a name that both break their expressions";
    let (status, parsed) = call(
        &router,
        "CreateMicrosoftAD",
        json!({
            "Name": SINGLE_LABEL_NAME,
            "Password": WEAK_PASSWORD,
            "VpcSettings": { "VpcId": "vpc-0a1b2c3d", "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"] },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the request to be refused, got {status} with {parsed}"
    );
    let message = parsed["message"].as_str().unwrap_or_default();
    let (counted, stated) = message.split_once(" detected: ").unwrap_or_else(|| {
        panic!("case {case:?}: expected a counted validation envelope, got {message:?}")
    });
    assert_eq!(
        counted, "2 validation errors",
        "case {case:?}: expected both violations counted together, got {message:?}"
    );
    let clauses: Vec<&str> = stated.split("; ").collect();
    assert_eq!(
        clauses.len(),
        2,
        "case {case:?}: expected two clauses, got {message:?}"
    );
    assert!(
        clauses[0].starts_with(
            "Value at 'password' failed to satisfy constraint: Member must satisfy regular \
             expression pattern: "
        ),
        "case {case:?}: expected the password clause first, stating no value, got {:?}",
        clauses[0]
    );
    assert!(
        !message.contains(WEAK_PASSWORD),
        "case {case:?}: a @sensitive member's clause must not echo the value, got {message:?}"
    );
    assert_eq!(
        clauses[1],
        format!(
            "Value '{SINGLE_LABEL_NAME}' at 'name' failed to satisfy constraint: Member must \
             satisfy regular expression pattern: {NAME_PATTERN}"
        ),
        "case {case:?}: expected the name clause second, quoting the value, got {:?}",
        clauses[1]
    );
}

/// A `SubnetIds` entry outside the modelled `^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$`, so a
/// row about a list member is refused for its ELEMENTS.
const BAD_SUBNET_ID: &str = "bad-subnet-1";

/// The other entry of that list, also outside the expression: two offending entries are still
/// ONE clause, so a row's count does not depend on how many of them offend.
const OTHER_BAD_SUBNET_ID: &str = "bad-subnet-2";

/// A well-formed `VpcId`, used where a row is not about that member.
const GOOD_VPC_ID: &str = "vpc-0a1b2c3d";

/// A `VpcId` outside the modelled `^(vpc-[0-9a-f]{8}|vpc-[0-9a-f]{17})$`.
const BAD_VPC_ID: &str = "bad-vpc-id";

/// RULE: the constraints one request breaks are ONE counted refusal with one clause per
/// member, whichever layer found each — the boundary validator's own walk, this service's
/// check of a `@pattern` that walk cannot compile, or the clause it states at a list member
/// for a constraint the list's ELEMENTS broke. The count is the number of members refused and
/// no clause is dropped because another class was also broken.
#[tokio::test]
async fn the_clauses_every_layer_found_are_counted_in_one_envelope() {
    let router = service();

    // The rows differ along which classes the request breaks together, how many members each
    // refusal is about, the operation, and how many entries of the offending list offend.
    let rows = vec![
        ClauseOrderCase {
            // RECORDED: the four members a recorded refusal of one create states together.
            case: "a create breaking all three classes at once",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": SINGLE_LABEL_NAME,
                "Password": WEAK_PASSWORD,
                "VpcSettings": {
                    "VpcId": BAD_VPC_ID,
                    "SubnetIds": [BAD_SUBNET_ID, OTHER_BAD_SUBNET_ID],
                },
            }),
            stated_at: &[
                "password",
                "vpcSettings.vpcId",
                "vpcSettings.subnetIds",
                "name",
            ],
        },
        ClauseOrderCase {
            case: "a create whose password and whose subnet list are both refused",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": "composed.example.com",
                "Password": WEAK_PASSWORD,
                "VpcSettings": { "VpcId": GOOD_VPC_ID, "SubnetIds": [BAD_SUBNET_ID] },
            }),
            stated_at: &["password", "vpcSettings.subnetIds"],
        },
        ClauseOrderCase {
            case: "a Simple AD create whose password, name and subnet list are all refused",
            operation: "CreateDirectory",
            body: json!({
                "Name": SINGLE_LABEL_NAME,
                "Password": WEAK_PASSWORD,
                "Size": "Small",
                "VpcSettings": {
                    "VpcId": GOOD_VPC_ID,
                    "SubnetIds": [BAD_SUBNET_ID, OTHER_BAD_SUBNET_ID],
                },
            }),
            stated_at: &["password", "vpcSettings.subnetIds", "name"],
        },
        ClauseOrderCase {
            case: "a create whose password alone is refused",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": "composed.example.com",
                "Password": WEAK_PASSWORD,
                "VpcSettings": {
                    "VpcId": GOOD_VPC_ID,
                    "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"],
                },
            }),
            stated_at: &["password"],
        },
        ClauseOrderCase {
            case: "a create whose subnet list alone is refused",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": "composed.example.com",
                "Password": GOOD_PASSWORD,
                "VpcSettings": { "VpcId": GOOD_VPC_ID, "SubnetIds": [BAD_SUBNET_ID] },
            }),
            stated_at: &["vpcSettings.subnetIds"],
        },
    ];

    for row in rows {
        let (status, parsed) = call(&router, row.operation, row.body.clone()).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the broken constraints to refuse the request, got {status} \
             with {parsed}",
            row.case
        );
        assert_eq!(
            parsed["__type"],
            json!("ValidationException"),
            "case {:?}: expected the framework's constraint refusal, got {parsed}",
            row.case
        );
        let message = parsed["message"].as_str().unwrap_or_default();
        let expected_count = row.stated_at.len();
        let plural = if expected_count == 1 { "error" } else { "errors" };
        assert!(
            message.starts_with(&format!("{expected_count} validation {plural} detected: ")),
            "case {:?}: expected {expected_count} counted clause(s), got {message:?}",
            row.case
        );
        let stated = clause_members(message, row.case);
        assert_eq!(
            stated, row.stated_at,
            "case {:?}: expected the clauses stated at {:?}, got {stated:?} in {message:?}",
            row.case, row.stated_at
        );
        // The password is `@sensitive`, so however the clauses compose the refusal still
        // echoes no password.
        assert!(
            !message.contains(WEAK_PASSWORD),
            "case {:?}: a @sensitive member's clause must not echo the value, got {message:?}",
            row.case
        );
    }
}

/// RULE, the recorded WORDING of the composed refusal: the whole sentence AWS answers a
/// create breaking all three classes with — the clause count, the four clauses in order, the
/// `@sensitive` password clause stating no value, the nested placement's two clauses with the
/// list member's whole submitted list and its element set, and the name's own clause.
///
/// The password's clause is read back from the refusal of a request that breaks THAT member
/// alone rather than written out here, so this row asserts the whole composed sentence
/// without restating a model expression a sibling clause already pins
/// (`published_pattern_text`, `validation_pattern_wording`).
#[tokio::test]
async fn the_composed_refusal_is_worded_as_aws_words_it() {
    let router = service();

    // The `VpcId` and `SubnetId` expressions, as the model declares them and as a clause
    // about those members quotes them
    // (`smithy_query(query="shape",shape="VpcId,SubnetId")`).
    const VPC_PATTERN: &str = r"^(vpc-[0-9a-f]{8}|vpc-[0-9a-f]{17})$";
    const SUBNET_PATTERN: &str = r"^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$";
    // The requirement the nested element set closes with for a list member the model never
    // leaves optional, as the framework words an absent member.
    const ENTRY_PRESENT: &str = "Member must not be null";

    let case = "the password's own clause";
    let (status, parsed) = call(
        &router,
        "CreateMicrosoftAD",
        json!({
            "Name": "worded.example.com",
            "Password": WEAK_PASSWORD,
            "VpcSettings": { "VpcId": GOOD_VPC_ID, "SubnetIds": ["subnet-0a1b2c3d"] },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the weak password to refuse the request, got {status} with \
         {parsed}"
    );
    let alone = parsed["message"].as_str().unwrap_or_default().to_string();
    let password_clause = alone
        .strip_prefix("1 validation error detected: ")
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected one counted clause, got {alone:?}")
        })
        .to_string();
    assert!(
        password_clause.starts_with("Value at 'password' failed to satisfy constraint: "),
        "case {case:?}: expected the clause to state no value for a @sensitive member, got \
         {password_clause:?}"
    );

    let case = "all four members of one create";
    let (status, parsed) = call(
        &router,
        "CreateMicrosoftAD",
        json!({
            "Name": SINGLE_LABEL_NAME,
            "Password": WEAK_PASSWORD,
            "VpcSettings": {
                "VpcId": BAD_VPC_ID,
                "SubnetIds": [BAD_SUBNET_ID, OTHER_BAD_SUBNET_ID],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the broken constraints to refuse the request, got {status} \
         with {parsed}"
    );
    let message = parsed["message"].as_str().unwrap_or_default();
    assert_eq!(
        message,
        format!(
            "4 validation errors detected: {password_clause}; Value '{BAD_VPC_ID}' at \
             'vpcSettings.vpcId' failed to satisfy constraint: Member must satisfy regular \
             expression pattern: {VPC_PATTERN}; Value '[{BAD_SUBNET_ID}, \
             {OTHER_BAD_SUBNET_ID}]' at 'vpcSettings.subnetIds' failed to satisfy constraint: \
             Member must satisfy constraint: [Member must satisfy regular expression pattern: \
             {SUBNET_PATTERN}, {ENTRY_PRESENT}]; Value '{SINGLE_LABEL_NAME}' at 'name' failed \
             to satisfy constraint: Member must satisfy regular expression pattern: \
             {NAME_PATTERN}"
        ),
        "case {case:?}: expected the whole composed sentence, got {message:?}"
    );
}

/// Well-formed `DirectoryId`s this account holds no directory under: the `d-` prefix and ten
/// hexadecimal digits, with different digits, so no row can be refused for its form instead of
/// for naming nothing.
const ABSENT_DIRECTORIES: [(&str, &str); 4] = [
    ("ten decimal digits", "d-1234567890"),
    ("all zeros", "d-0000000000"),
    ("all f", "d-ffffffffff"),
    ("every digit class", "d-0a1b2c3d4e"),
];

/// RULE 2: the directory a registration names is resolved first, so an id naming no stored
/// directory is the modelled not-found — whatever the certificate body carries, a body that is
/// not a certificate at all included.
#[tokio::test]
async fn a_registration_resolves_its_directory_before_it_reads_the_certificate() {
    let router = service();
    let (certificate, _) = certificate_pem(Some("ordering.example.com"), (2031, 4, 4));

    // The rows differ along the form of the body: not PEM at all, PEM framing around something
    // that is not a certificate, the longest body the modelled `length 1..8192` admits, and a
    // body that IS a well-formed certificate.
    let framed = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        "not base64 at all"
    );
    let longest = "n".repeat(8192);
    let bodies: [(&str, &str); 4] = [
        ("not PEM at all", "not-a-pem-not-a-pem-not-a-pem-"),
        ("PEM framing around something that is not a certificate", framed.as_str()),
        ("the longest body the member admits", longest.as_str()),
        ("a well-formed certificate", certificate.as_str()),
    ];

    for (form, absent) in ABSENT_DIRECTORIES {
        for (body_case, body) in bodies {
            let case = format!("a directory id of {form} / {body_case}");
            // Half the rows name the function the certificate performs and half leave it to
            // the documented default, so the ordering does not depend on that member either.
            let mut request = json!({ "DirectoryId": absent, "CertificateData": body });
            if body_case.starts_with("not PEM") || body_case.starts_with("the longest") {
                request["Type"] = json!("ClientLDAPS");
            }
            let (status, parsed) = call(&router, "RegisterCertificate", request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the absent directory to be refused, got {status} with \
                 {parsed}"
            );
            assert_eq!(
                parsed["__type"],
                json!("DirectoryDoesNotExistException"),
                "case {case:?}: the missing directory must be reported before the certificate \
                 is read, got {parsed}"
            );
            let request_id = string(&parsed, "RequestId", &case);
            assert_eq!(
                parsed["Message"].as_str().unwrap_or_default(),
                format!("Directory: '{absent}' does not exist. : RequestId: {request_id}"),
                "case {case:?}: expected this service's not-found sentence, got {parsed}"
            );
        }

        // The control AWS answers identically: the listing of the same directory's
        // registrations reports the same class and the same sentence.
        let case = format!("ListCertificates on a directory id of {form}");
        let (status, parsed) = call(
            &router,
            "ListCertificates",
            json!({ "DirectoryId": absent }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the absent directory to be refused, got {status} with \
             {parsed}"
        );
        assert_eq!(
            parsed["__type"],
            json!("DirectoryDoesNotExistException"),
            "case {case:?}: expected the same class as the registration, got {parsed}"
        );
        let request_id = string(&parsed, "RequestId", &case);
        assert_eq!(
            parsed["Message"].as_str().unwrap_or_default(),
            format!("Directory: '{absent}' does not exist. : RequestId: {request_id}"),
            "case {case:?}: expected the same sentence as the registration, got {parsed}"
        );
    }
}

/// RULE 2, the other half: the ordering does not DROP the body check — once the directory
/// resolves, a body that is not a certificate is still refused as such, and one that is a
/// certificate is registered and reported back from the store.
#[tokio::test]
async fn once_the_directory_resolves_the_certificate_is_still_read() {
    let router = service();
    let directory_id = directory_in_service(&router, "ordering.example.com", "the ordering control").await;

    for (case, body) in [
        ("not PEM at all", "not-a-pem-not-a-pem-not-a-pem-".to_string()),
        (
            "PEM framing around something that is not a certificate",
            format!(
                "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
                "not base64 at all"
            ),
        ),
    ] {
        let (status, parsed) = call(
            &router,
            "RegisterCertificate",
            json!({ "DirectoryId": &directory_id, "CertificateData": body }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the body to be refused on a stored directory, got {status} \
             with {parsed}"
        );
        assert_eq!(
            parsed["__type"],
            json!("InvalidCertificateException"),
            "case {case:?}: expected the certificate class once the directory resolves, got \
             {parsed}"
        );
        let request_id = string(&parsed, "RequestId", case);
        assert_eq!(
            parsed["Message"].as_str().unwrap_or_default(),
            format!(
                "Certificate is not valid. Could not register the certificate with the \
                 directory {directory_id} : RequestId: {request_id}"
            ),
            "case {case:?}: expected this service's invalid-certificate sentence, got {parsed}"
        );
    }

    // ...and the registration a well-formed certificate asks for is served and read back.
    let case = "a well-formed certificate on a stored directory";
    let (certificate, expires_at) = certificate_pem(Some("served.example.com"), (2032, 6, 6));
    let certificate_id = register(
        &router,
        json!({ "DirectoryId": &directory_id, "CertificateData": certificate }),
        case,
    )
    .await;
    let reported = describe(&router, &directory_id, &certificate_id, case).await;
    assert_eq!(
        reported["CommonName"],
        json!("served.example.com"),
        "case {case:?}: expected the certificate's own subject common name, got {reported}"
    );
    assert_eq!(
        reported["ExpiryDateTime"].as_f64().map(|at| at as i64),
        Some(expires_at),
        "case {case:?}: expected the certificate's own notAfter bound, got {reported}"
    );
}
