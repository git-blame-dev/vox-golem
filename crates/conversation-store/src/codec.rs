//! One canonical Operation codec for storage, JNI and exact-body receipts.
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use voxgolem_core::durable_conversation::{
    DomainError, Operation, Record, MAX_ENCODED_OPERATION_BYTES,
};

pub fn invalid(field: &str) -> DomainError {
    DomainError::Invalid {
        field: field.into(),
    }
}
pub fn limit(name: &str) -> DomainError {
    DomainError::LimitExceeded { limit: name.into() }
}

pub fn canonical_record(raw: &str) -> Result<Record, DomainError> {
    if raw.len() > MAX_ENCODED_OPERATION_BYTES {
        return Err(limit("encoded_operation_bytes"));
    }
    let operation: Operation = serde_json::from_str(raw).map_err(|_| invalid("operation_json"))?;
    record(operation)
}

pub fn record(operation: Operation) -> Result<Record, DomainError> {
    let encoded = encode(
        &operation,
        MAX_ENCODED_OPERATION_BYTES,
        "encoded_operation_bytes",
    )?;
    let record = Record {
        body_key: format!("{:x}", Sha256::digest(encoded.as_bytes())),
        operation,
    };
    record.validate()?;
    Ok(record)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    body_key: String,
    operation: Box<RawValue>,
}

pub fn parse_record(raw: &str) -> Result<Record, DomainError> {
    if raw.len() > MAX_ENCODED_OPERATION_BYTES + 256 {
        return Err(limit("encoded_operation_bytes"));
    }
    let wire: WireRecord = serde_json::from_str(raw).map_err(|_| invalid("record_json"))?;
    let record = canonical_record(wire.operation.get())?;
    if record.body_key != wire.body_key {
        return Err(invalid("body_key_sha256_mismatch"));
    }
    Ok(record)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    max: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.len() > self.max.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("JSON byte limit"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn encode(value: &impl Serialize, max: usize, bound: &str) -> Result<String, DomainError> {
    let mut writer = BoundedWriter { bytes: vec![], max };
    serde_json::to_writer(&mut writer, value).map_err(|_| limit(bound))?;
    String::from_utf8(writer.bytes).map_err(|_| invalid("encoding"))
}
