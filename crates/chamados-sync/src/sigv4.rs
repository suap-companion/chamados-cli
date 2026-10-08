//! AWS Signature Version 4, the request signing every S3-compatible service understands.
//!
//! Only what the synchronization needs: a header-signed request (no presigned URLs) for the `s3`
//! service. The hash and the HMAC come from the RustCrypto crates; nothing cryptographic is hand-rolled.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";

/// Credentials and scope of a signature.
#[derive(Debug, Clone, Copy)]
pub struct Signer<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
}

/// Lowercase hexadecimal SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// `YYYYMMDDTHHMMSSZ`, the `x-amz-date` format, for `time` (UTC).
pub fn amz_date(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Civil date (year, month, day) of the day number `days` since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// Percent-encodes a URL path, keeping `/` and the unreserved characters (S3 encodes the path once).
pub fn encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The `Authorization` header value for a request.
///
/// `headers` are the headers to sign, **lowercase names, sorted by name**, and must include `host` and
/// `x-amz-date`; `canonical_path` is the already encoded path; there is no query string.
pub fn authorization(
    signer: &Signer<'_>,
    method: &str,
    canonical_path: &str,
    headers: &[(String, String)],
    payload_hash: &str,
    amz_date: &str,
) -> String {
    let signed_headers = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let canonical_request = format!(
        "{method}\n{canonical_path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let day = &amz_date[..8];
    let scope = format!("{day}/{}/{SERVICE}/aws4_request", signer.region);
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let secret = format!("AWS4{}", signer.secret_access_key);
    let mut key = hmac_sha256(secret.as_bytes(), day.as_bytes());
    for part in [signer.region, SERVICE, "aws4_request"] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    let signature = hex(&hmac_sha256(&key, string_to_sign.as_bytes()));
    format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        signer.access_key_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn matches_the_aws_documentation_example() {
        // "GET Object" example of the AWS SigV4 documentation.
        let signer = Signer {
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
        };
        let empty = sha256_hex(b"");
        assert_eq!(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let signed = headers(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            ("range", "bytes=0-9"),
            ("x-amz-content-sha256", &empty),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        let value = authorization(
            &signer,
            "GET",
            "/test.txt",
            &signed,
            &empty,
            "20130524T000000Z",
        );
        assert_eq!(
            value,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn signs_a_conditional_put_like_an_independent_implementation() {
        // The expected signature was computed with Python's hashlib/hmac, step by step.
        let signer = Signer {
            access_key_id: "ID",
            secret_access_key: "segredo",
            region: "auto",
        };
        let body_hash = sha256_hex(b"corpo");
        assert_eq!(
            body_hash,
            "bd67f7f69bad25ef1d44ec288d5081c491b75d0d042a4fc1d00e6cd90774b10b"
        );
        let signed = headers(&[
            ("host", "conta.r2.cloudflarestorage.com"),
            ("if-none-match", "*"),
            ("x-amz-content-sha256", &body_hash),
            ("x-amz-date", "20260101T120000Z"),
        ]);
        let value = authorization(
            &signer,
            "PUT",
            "/bucket/pre/fix/doc.bin",
            &signed,
            &body_hash,
            "20260101T120000Z",
        );
        assert!(value.starts_with("AWS4-HMAC-SHA256 Credential=ID/20260101/auto/s3/aws4_request, "));
        assert!(value.ends_with(
            "Signature=f3ad7c581c10fd3ffd0ae79e4eb2991da67a08c58939acb3e699c22349a43edb"
        ));
    }

    #[test]
    fn formats_dates_and_paths() {
        assert_eq!(amz_date(UNIX_EPOCH), "19700101T000000Z");
        assert_eq!(
            amz_date(UNIX_EPOCH + Duration::from_secs(1_767_268_800)),
            "20260101T120000Z"
        );
        // A leap day and the last second of a year.
        assert_eq!(
            amz_date(UNIX_EPOCH + Duration::from_secs(1_709_164_800 + 3661)),
            "20240229T010101Z"
        );
        assert_eq!(
            amz_date(UNIX_EPOCH + Duration::from_secs(1_798_761_599)),
            "20261231T235959Z"
        );
        assert_eq!(
            encode_path("/b/pré fixo/doc 1.bin"),
            "/b/pr%C3%A9%20fixo/doc%201.bin"
        );
        assert_eq!(encode_path("/a-b_c.d~e/f"), "/a-b_c.d~e/f");
    }

    #[test]
    fn a_clock_before_the_epoch_does_not_panic() {
        let before = UNIX_EPOCH - Duration::from_secs(5);
        assert_eq!(amz_date(before), "19700101T000000Z");
    }
}
