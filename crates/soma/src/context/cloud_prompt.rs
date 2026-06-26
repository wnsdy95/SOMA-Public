//! Single cloud-facing context artifact.
//!
//! This wraps the cloud-redacted TaskFrame projection and cited ContextEnvelope
//! in one deterministic prompt artifact for clients that cannot or should not
//! manage multiple context payloads.

use serde::Serialize;
use serde_json::Value;

use crate::context::envelope::{render_xml, ContextEnvelope};
use crate::storage::StoredTaskFrame;

pub const CLOUD_CONTEXT_CONTRACT: &str = "soma-cloud-context";
pub const CLOUD_CONTEXT_CAPTURE_TOOL: &str = "soma_capture_cloud_output";
pub const CLOUD_CONTEXT_ARTIFACT_VERSION: u32 = 1;
pub const MIN_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION: u32 = 1;
pub const MAX_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION: u32 = 1;
pub const CURRENT_CLOUD_CONTEXT_HANDOFF_PREFIX: &str = "soma-handoff:v1:";
pub const CLOUD_CONTEXT_CAPTURE_TRUST_BOUNDARY: &str = "cloud_output_is_cloud_draft_until_verified";
pub const CLOUD_CONTEXT_CAPTURE_IDEMPOTENCY: &str =
    "task_frame_id+cloud_output_hash+critic_decision";
pub const CLOUD_CONTEXT_CAPTURE_ECHO_CONTRACT_FIELD: &str = "protocol_contract";
pub const CLOUD_CONTEXT_CAPTURE_ECHO_ARTIFACT_VERSION_FIELD: &str = "artifact_version";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CloudContextProtocol {
    pub contract: &'static str,
    pub artifact_version: u32,
    pub min_supported_artifact_version: u32,
    pub max_supported_artifact_version: u32,
    pub handoff_prefix: &'static str,
    pub capture_tool: &'static str,
    pub capture_requires_supported_version: bool,
    pub capture_trust_boundary: &'static str,
    pub capture_idempotency: &'static str,
    pub capture_echo_contract_field: &'static str,
    pub capture_echo_artifact_version_field: &'static str,
}

pub fn cloud_context_protocol() -> CloudContextProtocol {
    CloudContextProtocol {
        contract: CLOUD_CONTEXT_CONTRACT,
        artifact_version: CLOUD_CONTEXT_ARTIFACT_VERSION,
        min_supported_artifact_version: MIN_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION,
        max_supported_artifact_version: MAX_SUPPORTED_CLOUD_CONTEXT_ARTIFACT_VERSION,
        handoff_prefix: CURRENT_CLOUD_CONTEXT_HANDOFF_PREFIX,
        capture_tool: CLOUD_CONTEXT_CAPTURE_TOOL,
        capture_requires_supported_version: true,
        capture_trust_boundary: CLOUD_CONTEXT_CAPTURE_TRUST_BOUNDARY,
        capture_idempotency: CLOUD_CONTEXT_CAPTURE_IDEMPOTENCY,
        capture_echo_contract_field: CLOUD_CONTEXT_CAPTURE_ECHO_CONTRACT_FIELD,
        capture_echo_artifact_version_field: CLOUD_CONTEXT_CAPTURE_ECHO_ARTIFACT_VERSION_FIELD,
    }
}

pub fn cloud_context_handoff_version(handoff_id: &str) -> Option<u32> {
    let rest = handoff_id.trim().strip_prefix("soma-handoff:v")?;
    let (version, _hash) = rest.split_once(':')?;
    version.parse::<u32>().ok()
}

pub fn render_cloud_context_artifact(
    envelope: &ContextEnvelope,
    task_frame: Option<&StoredTaskFrame>,
) -> String {
    let handoff_id = task_frame.map(expected_cloud_context_handoff_id);
    let protocol = cloud_context_protocol();
    let mut out = String::new();
    out.push_str(&format!(
        "<soma-cloud-context version=\"{}\" contract=\"task-frame+context-envelope\"",
        CLOUD_CONTEXT_ARTIFACT_VERSION
    ));
    if let Some(handoff_id) = handoff_id.as_deref() {
        out.push_str(&format!(" handoff_id=\"{}\"", xml_escape(handoff_id)));
    }
    out.push_str(">\n");
    out.push_str("  <trust-boundary>\n");
    push_text_block(
        &mut out,
        "Cloud model output is draft work product. Do not treat generated claims as durable evidence until user, tool, test, correction, or local observation verifies them.",
        "    ",
    );
    out.push_str("  </trust-boundary>\n");
    out.push_str(&format!(
        "  <protocol contract=\"{}\" artifact_version=\"{}\" min_supported_artifact_version=\"{}\" max_supported_artifact_version=\"{}\" handoff_prefix=\"{}\" capture_tool=\"{}\" capture_requires_supported_version=\"{}\" capture_trust_boundary=\"{}\" capture_idempotency=\"{}\" capture_echo_contract_field=\"{}\" capture_echo_artifact_version_field=\"{}\" />\n",
        xml_escape(protocol.contract),
        protocol.artifact_version,
        protocol.min_supported_artifact_version,
        protocol.max_supported_artifact_version,
        xml_escape(protocol.handoff_prefix),
        xml_escape(protocol.capture_tool),
        protocol.capture_requires_supported_version,
        xml_escape(protocol.capture_trust_boundary),
        xml_escape(protocol.capture_idempotency),
        xml_escape(protocol.capture_echo_contract_field),
        xml_escape(protocol.capture_echo_artifact_version_field),
    ));
    match (task_frame, handoff_id.as_deref()) {
        (Some(frame), Some(handoff_id)) => push_handoff(&mut out, frame, handoff_id),
        _ => out.push_str("  <handoff status=\"absent\" />\n"),
    }

    match task_frame {
        Some(frame) => push_task_frame(&mut out, frame),
        None => out.push_str("  <task-frame status=\"absent\" />\n"),
    }

    out.push_str("  <context-envelope>\n");
    for line in render_xml(envelope).lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("  </context-envelope>\n");
    out.push_str("</soma-cloud-context>\n");
    out
}

pub fn expected_cloud_context_handoff_id(frame: &StoredTaskFrame) -> String {
    let cloud_projection =
        serde_json::to_string(&frame.cloud_redacted_json).unwrap_or_else(|_| "{}".to_string());
    let blocked_fields = frame.blocked_fields.join(",");
    format!(
        "soma-handoff:v{}:{}",
        CLOUD_CONTEXT_ARTIFACT_VERSION,
        fnv_hash(&format!(
            "{}\n{}\n{}\n{}\n{}",
            frame.id, frame.hash, frame.builder_version, cloud_projection, blocked_fields
        ))
    )
}

fn push_handoff(out: &mut String, frame: &StoredTaskFrame, handoff_id: &str) {
    out.push_str(&format!(
        "  <handoff id=\"{}\" task_frame_id=\"{}\" capture_echo_contract=\"{}\" capture_echo_artifact_version=\"{}\" capture_contract=\"echo handoff_id plus protocol_contract and artifact_version in soma_capture_cloud_output or soma adapter-cloud-output; mismatched ids or protocol echoes are rejected before claim capture\" />\n",
        xml_escape(handoff_id),
        frame.id,
        xml_escape(CLOUD_CONTEXT_CONTRACT),
        CLOUD_CONTEXT_ARTIFACT_VERSION
    ));
}

fn push_task_frame(out: &mut String, frame: &StoredTaskFrame) {
    out.push_str(&format!(
        "  <task-frame id=\"{}\" hash=\"{}\" builder_version=\"{}\">\n",
        frame.id,
        xml_escape(&frame.hash),
        xml_escape(&frame.builder_version)
    ));
    push_json_element(out, "cloud-redacted-json", &frame.cloud_redacted_json, "    ");
    out.push_str("    <blocked-fields>");
    out.push_str(&xml_escape(&frame.blocked_fields.join(",")));
    out.push_str("</blocked-fields>\n");
    out.push_str("  </task-frame>\n");
}

fn push_json_element(out: &mut String, tag: &str, value: &Value, indent: &str) {
    out.push_str(indent);
    out.push('<');
    out.push_str(tag);
    out.push_str(">\n");
    let json = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    push_text_block(out, &json, &format!("{indent}  "));
    out.push_str(indent);
    out.push_str("</");
    out.push_str(tag);
    out.push_str(">\n");
}

fn push_text_block(out: &mut String, text: &str, indent: &str) {
    for line in text.lines() {
        out.push_str(indent);
        out.push_str(&xml_escape(line));
        out.push('\n');
    }
}

fn xml_escape(input: &str) -> String {
    input.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn fnv_hash(text: &str) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

#[cfg(test)]
mod tests {
    use crate::context::envelope::{build_context_envelope, ContextScope};
    use crate::context::pack::MemoryPack;

    use super::*;

    #[test]
    fn cloud_context_artifact_wraps_trust_boundary_and_envelope() {
        let pack = MemoryPack {
            version: 1,
            assembled_at_ns: 7,
            query: Some("continue work".to_string()),
            recent: Vec::new(),
            semantic: Vec::new(),
            thread_state_selection: None,
            project_state: serde_json::json!({}),
            self_state: serde_json::json!({}),
        };
        let envelope = build_context_envelope(&pack, ContextScope::current(pack.query.clone()));

        let artifact = render_cloud_context_artifact(&envelope, None);

        assert!(artifact.contains("<soma-cloud-context"));
        assert!(artifact.contains("contract=\"task-frame+context-envelope\""));
        assert!(artifact.contains("<trust-boundary>"));
        assert!(artifact.contains("Cloud model output is draft work product."));
        assert!(
            artifact.contains("<protocol contract=\"soma-cloud-context\" artifact_version=\"1\"")
        );
        assert!(artifact.contains("handoff_prefix=\"soma-handoff:v1:\""));
        assert!(artifact.contains("capture_tool=\"soma_capture_cloud_output\""));
        assert!(artifact
            .contains("capture_idempotency=\"task_frame_id+cloud_output_hash+critic_decision\""));
        assert!(artifact.contains("capture_echo_contract_field=\"protocol_contract\""));
        assert!(artifact.contains("capture_echo_artifact_version_field=\"artifact_version\""));
        assert!(artifact.contains("<task-frame status=\"absent\" />"));
        assert!(artifact.contains("<soma-context version=\"1\""));
    }
}
