// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the way a DELETE reads the resource id it is given, driven over the
//! wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rule under test: an EC2 delete resolves ONE named resource, and it DECODES that id
//! before it looks anything up — the long EC2 id form is an obfuscated identifier the service
//! reads rather than free text — so a value no issue of that kind ever produced is refused
//! with the kind's `.Malformed` code and its own sentence, while the kind's `NotFound` code is
//! reserved for an identifier this service DID issue that names no resource any more. A client
//! that retries a `NotFound` as eventual consistency therefore never loops on input it should
//! have been told is unreadable.
//!
//! Driven through three deletes of three different resource kinds — a DHCP options set, an
//! internet gateway and an Amazon FPGA Image — because the rule is about the delete and not
//! about one kind: each has its own id prefix, its own two codes and its own sentences, and
//! each row of the table below is a distinct way for a value to fall outside the identifiers
//! the mint has issued (the documentation-shaped long form, the short form, the all-zero
//! placeholder, another kind's prefix, a non-hexadecimal suffix and the prefix alone). The
//! round trip at the end is the other side of the rule: the id of a resource this service
//! minted is deleted, and the SAME id then draws that kind's not-found refusal.
//!
//! Every expected code and sentence is written out here, independently of the service.

use ec2::state::{
    INVALID_DHCP_OPTIONS_ID_MALFORMED, INVALID_DHCP_OPTION_ID_NOT_FOUND,
    INVALID_FPGA_IMAGE_ID_MALFORMED, INVALID_FPGA_IMAGE_ID_NOT_FOUND,
    INVALID_INTERNET_GATEWAY_ID_MALFORMED, INVALID_INTERNET_GATEWAY_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::fpga_image_tasks_and_volume_io::seed_fpga_image;
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// One delete under test: the action, the member it reads the id in, the id prefix of the kind
/// it resolves, and the two refusals that kind answers.
struct Delete {
    action: &'static str,
    member: &'static str,
    prefix: &'static str,
    malformed_code: &'static str,
    not_found_code: &'static str,
}

impl Delete {
    /// The sentence this delete answers a value no issue of its kind ever produced with.
    fn malformed_message(&self, id: &str) -> String {
        match self.action {
            // The DHCP options set and the internet gateway each name the resource kind as the
            // modelled `ResourceType` spells it.
            "DeleteDhcpOptions" => format!("The dhcp-options ID '{id}' is malformed"),
            "DeleteInternetGateway" => format!("The internet-gateway ID '{id}' is malformed"),
            // The FPGA-image kind words an unreadable id its own way.
            _ => format!("Image ID '{id}' is invalid."),
        }
    }

    /// The sentence this delete answers an identifier it DID issue that names no resource.
    fn not_found_message(&self, id: &str) -> String {
        match self.action {
            "DeleteDhcpOptions" => format!("The dhcpOption ID '{id}' does not exist"),
            "DeleteFpgaImage" => format!("Image ID '{id}' not found."),
            _ => format!("The internetGateway ID '{id}' does not exist"),
        }
    }

    /// One call of this delete, naming `id`.
    async fn invoke(&self, router: &MultiServiceRouter, id: &str) -> (u16, String) {
        call(router, REGION, self.action, &[param(self.member, id)]).await
    }
}

/// The three deletes this rule is read through.
fn deletes() -> [Delete; 3] {
    [
        Delete {
            action: "DeleteDhcpOptions",
            member: "DhcpOptionsId",
            prefix: "dopt",
            malformed_code: INVALID_DHCP_OPTIONS_ID_MALFORMED,
            not_found_code: INVALID_DHCP_OPTION_ID_NOT_FOUND,
        },
        Delete {
            action: "DeleteInternetGateway",
            member: "InternetGatewayId",
            prefix: "igw",
            malformed_code: INVALID_INTERNET_GATEWAY_ID_MALFORMED,
            not_found_code: INVALID_INTERNET_GATEWAY_ID_NOT_FOUND,
        },
        Delete {
            action: "DeleteFpgaImage",
            member: "FpgaImageId",
            prefix: "afi",
            malformed_code: INVALID_FPGA_IMAGE_ID_MALFORMED,
            not_found_code: INVALID_FPGA_IMAGE_ID_NOT_FOUND,
        },
    ]
}

/// `(case, the suffix)`: each row makes a value the mint cannot have produced, for a different
/// reason. The prefix of the kind under test is prepended, so one table serves all three.
const UNISSUED_SUFFIXES: [(&str, &str); 5] = [
    // The documentation-shaped long form, which sits astronomically above every identifier
    // the mint has reached.
    ("the documentation-shaped long form", "-0123456789abcdef0"),
    ("the short id form", "-01234567"),
    ("the all-zero placeholder", "-00000000000000000"),
    ("a suffix outside the hexadecimal alphabet", "-0123456789abcdefg"),
    ("the prefix alone", "-"),
];

/// RULE: every value the mint never issued is refused with the kind's malformed code and its
/// own sentence, by each of the three deletes.
#[tokio::test]
async fn a_delete_refuses_an_identifier_no_issue_ever_produced() {
    let (router, _engine) = super::ec2_query::emulator();

    for delete in deletes() {
        for (case, suffix) in UNISSUED_SUFFIXES {
            let id = format!("{prefix}{suffix}", prefix = delete.prefix);
            let row = format!("{} with {case}", delete.action);
            let (status, body) = delete.invoke(&router, &id).await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {row:?}: expected HTTP {REFUSAL_STATUS} for {id:?}, got {status} with body \
                 {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, delete.malformed_code,
                "case {row:?}: expected the code {:?} for {id:?}, got {code:?} in {body}",
                delete.malformed_code
            );
            let expected_message = delete.malformed_message(&id);
            let message = error_message(&body);
            assert_eq!(
                message, expected_message,
                "case {row:?}: expected the message {expected_message:?}, got {message:?}"
            );
        }

        // Another kind's prefix is the same judgement: the value is no identifier of the kind
        // this delete resolves.
        let foreign = "vpc-0123456789abcdef0";
        let row = format!("{} with another kind's prefix", delete.action);
        let (status, body) = delete.invoke(&router, foreign).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {row:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, delete.malformed_code,
            "case {row:?}: expected the code {:?}, got {code:?} in {body}",
            delete.malformed_code
        );
    }
}

/// RULE, the other side: the id of a resource this service MINTED is deleted, and the same id
/// then draws that kind's not-found refusal rather than its malformed one.
#[tokio::test]
async fn a_delete_answers_not_found_for_an_identifier_it_issued() {
    let (router, engine) = super::ec2_query::emulator();

    // One minted id per kind, each created the way this crate creates that resource: the two
    // VPC-plane resources over the wire, and the AFI through the shared seeding helper,
    // because `CreateFpgaImage` reads a design checkpoint out of Amazon S3 that this emulator
    // holds no object for.
    let (status, body) = call(
        &router,
        REGION,
        "CreateDhcpOptions",
        &[
            param("DhcpConfiguration.1.Key", "domain-name-servers"),
            param("DhcpConfiguration.1.Value.1", "10.90.0.2"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the DHCP options set: expected HTTP 200, got {status} with body {body}"
    );
    let dhcp_options_id =
        element(&body, "dhcpOptionsId").unwrap_or_else(|| panic!("no dhcpOptionsId in {body}"));

    let (status, body) = call(&router, REGION, "CreateInternetGateway", &[]).await;
    assert_eq!(
        status, 200,
        "creating the internet gateway: expected HTTP 200, got {status} with body {body}"
    );
    let internet_gateway_id = element(&body, "internetGatewayId")
        .unwrap_or_else(|| panic!("no internetGatewayId in {body}"));

    let (fpga_image_id, _) = seed_fpga_image(&engine, "delete-id-decoding");

    let minted = [dhcp_options_id, internet_gateway_id, fpga_image_id];
    for (delete, id) in deletes().into_iter().zip(minted) {
        let row = format!("{} of a minted id", delete.action);
        let (status, body) = delete.invoke(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected HTTP 200 deleting {id}, got {status} with body {body}"
        );

        // The resource is gone, and the identifier is still one this service issued: the
        // answer is the kind's not-found refusal, in that kind's own sentence.
        let (status, body) = delete.invoke(&router, &id).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {row:?}: expected HTTP {REFUSAL_STATUS} from the second delete of {id}, got \
             {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, delete.not_found_code,
            "case {row:?}: expected the code {:?} from the second delete of {id}, got {code:?} \
             in {body}",
            delete.not_found_code
        );
        let expected_message = delete.not_found_message(&id);
        let message = error_message(&body);
        assert_eq!(
            message, expected_message,
            "case {row:?}: expected the message {expected_message:?}, got {message:?}"
        );
    }
}
