use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

fn main() {
    if let Err(error) = run() {
        eprintln!("update signature verification failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let public_key = arguments
        .next()
        .ok_or_else(|| String::from("missing base64 public key"))?;
    let signature_path = arguments
        .next()
        .ok_or_else(|| String::from("missing signature path"))?;
    let artifact_path = arguments
        .next()
        .ok_or_else(|| String::from("missing artifact path"))?;
    if arguments.next().is_some() {
        return Err(String::from("unexpected additional arguments"));
    }
    verify_file(
        &public_key,
        Path::new(&signature_path),
        Path::new(&artifact_path),
    )
}

fn verify_file(
    public_key: &str,
    signature_path: &Path,
    artifact_path: &Path,
) -> Result<(), String> {
    let signature = std::fs::read_to_string(signature_path)
        .map_err(|error| format!("failed to read signature: {error}"))?;
    let public_key = decode_public_key(public_key)?;
    let signature = decode_signature(signature.trim())?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|error| format!("unsupported signature: {error}"))?;
    let file =
        File::open(artifact_path).map_err(|error| format!("failed to open artifact: {error}"))?;
    let mut reader = BufReader::new(file);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("failed to read artifact: {error}"))?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier
        .finalize()
        .map_err(|error| format!("invalid signature: {error}"))
}

fn decode_public_key(encoded: &str) -> Result<PublicKey, String> {
    PublicKey::decode(&decode_base64_text(encoded, "public key")?)
        .map_err(|error| format!("invalid public key: {error}"))
}

fn decode_signature(encoded: &str) -> Result<Signature, String> {
    Signature::decode(&decode_base64_text(encoded, "signature")?)
        .map_err(|error| format!("invalid signature encoding: {error}"))
}

fn decode_base64_text(encoded: &str, name: &str) -> Result<String, String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("invalid base64 {name}: {error}"))?;
    String::from_utf8(decoded).map_err(|error| format!("non-UTF-8 {name}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{decode_public_key, decode_signature};

    const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEMwNjVCODhEQUZCNUI2NgpSV1JtVy92YWlGc0dEQm94ZG10c0ZUVllqTDR2N0ZkUGZ1RlJldVRUazM2Mit1Q252UG40SmU1Kwo=";
    const OTHER_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQxNkJDQzUwRjg3OTAxOQpSV1Faa0ljUHhid1dEWWdURlBHYzRQM3A3cVF5c2l4WHNNdzBPRHRrVys5OEoxNWRJTjNpcTNncQo=";
    const SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVSbVcvdmFpRnNHRExwb3RYV1VBeTk5WFdFcXhlWktYaW8wbFZ3UEdJd01hL1hQdEhGU2NhOHRqNkRwL3FlU0o2TWJHNVp2UDBneVVzNXRSQVZIRmRvcFdkZDNMMkY2Znc0PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzg1MTI0MzQwCWZpbGU6dGVzdC1maXh0dXJlLnR4dApFL09EalArMGFwNUR1VWFpa2dRWkdGWkdBck0vQ2phZmhnRE0zTk92c2xwdnA4Snd3NE9kUVdoaEVHdisxVDNoV21tRGpUTitZQTFaTVhqc0YzdE1CZz09Cg==";
    const MESSAGE: &[u8] = b"VoxGolem updater verifier fixture\n";

    #[test]
    fn matching_key_accepts_and_different_key_rejects_fixture() {
        let signature = decode_signature(SIGNATURE).expect("decode signature");
        decode_public_key(PUBLIC_KEY)
            .expect("decode matching key")
            .verify(MESSAGE, &signature, true)
            .expect("matching signature");
        assert!(decode_public_key(OTHER_PUBLIC_KEY)
            .expect("decode other key")
            .verify(MESSAGE, &signature, true)
            .is_err());
    }
}
