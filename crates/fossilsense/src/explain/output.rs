use anyhow::Result;
use serde_json::{json, Value};
use std::io::Write;

pub(super) fn encode(mut report: Value, limit: usize) -> Result<Vec<u8>> {
    if let Some(bytes) = attempt(&report, limit)? {
        return Ok(bytes);
    }
    report["outputTruncated"] = json!(true);
    for key in [
        "extraction",
        "persistence",
        "region",
        "presentation",
        "recall",
        "language",
        "inclusion",
    ] {
        if let Some(stage) = report["stages"].get_mut(key) {
            stage["observationStatus"] = stage["status"].clone();
            stage["status"] = json!("truncated");
            stage["reason"] = json!("output_byte_budget");
            stage["counts"]["observedBeforeOutputLimit"] = stage["counts"]["returned"].clone();
            stage["counts"]["returned"] = json!(0);
            stage["evidence"] = json!({"omittedForOutputLimit":true});
        }
        if let Some(bytes) = attempt(&report, limit)? {
            return Ok(bytes);
        }
    }
    // Even index metadata is untrusted input. Preserve the observation status
    // while making excessive stored strings visibly unavailable.
    if let Some(metadata) = report.get_mut("indexObservation") {
        *metadata =
            json!({"status":"truncated","consistency":"unknown","reason":"output_byte_budget"});
    }
    attempt(&report, limit)?
        .ok_or_else(|| anyhow::anyhow!("diagnostic summary exceeds output budget"))
}

fn attempt(report: &Value, limit: usize) -> Result<Option<Vec<u8>>> {
    struct Capped {
        bytes: Vec<u8>,
        limit: usize,
        overflow: bool,
    }
    impl Write for Capped {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                self.overflow = true;
                return Err(std::io::Error::other("diagnostic output limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Capped {
        bytes: Vec::with_capacity(limit.min(32 * 1024)),
        limit,
        overflow: false,
    };
    let result = serde_json::to_writer(&mut writer, report);
    if writer.overflow {
        return Ok(None);
    }
    result?;
    Ok(Some(writer.bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diagnostic_output_cap_preserves_valid_json_and_marks_omitted_evidence() {
        let report = json!({"formatVersion":1,"outputTruncated":false,"stages":{"extraction":{"status":"found","reason":"observed_parser_facts","counts":{"returned":1},"limit":256,"evidence":{"facts":[{"signature":"x".repeat(20_000)}]}}}});
        let bytes = encode(report, 1024).unwrap();
        assert!(bytes.len() <= 1024);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["formatVersion"], 1);
        assert_eq!(value["outputTruncated"], true);
        assert_eq!(value["stages"]["extraction"]["status"], "truncated");
        assert_eq!(value["stages"]["extraction"]["counts"]["returned"], 0);
        assert_eq!(
            value["stages"]["extraction"]["counts"]["observedBeforeOutputLimit"],
            1
        );
    }
}
