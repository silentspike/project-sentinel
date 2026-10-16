use super::*;

#[test]
fn funded_context_head_and_retained_history_have_independent_exact_bounds() {
    let head = ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES;
    let history = MAX_RETAINED_HISTORY_BYTES;
    assert!(funded_context_size_is_bounded(head + history - 2, history));
    assert!(!funded_context_size_is_bounded(head + history - 1, history));
    assert!(!funded_context_size_is_bounded(history + 1, history + 1));
    assert!(funded_context_size_is_bounded(head, 2));
    assert!(!funded_context_size_is_bounded(head + 1, 2));
    assert!(!funded_context_size_is_bounded(1, 1));
    assert!(!funded_context_size_is_bounded(1, 2));
    assert!(!funded_context_size_is_bounded(usize::MAX, 2));
}

#[test]
fn typed_context_size_counts_utf8_escaping_and_only_the_authorization_array() {
    // This is a size-policy fixture, not a valid journal or funding grant.
    let mut call = fixture();
    call.grant.schema_version = 5;
    call.context.tool_catalog = serde_json::json!({"size_probe": "\u{00e9}\"\\".repeat(100)});
    let context_bytes = crate::digest::serialized_json_size(&call.context).unwrap();
    assert_eq!(context_bytes, serde_json::to_vec(&call.context).unwrap().len());
    let history_bytes = crate::digest::serialized_json_size(
        &call.context.source_session.continuation.as_ref().unwrap().authorizations,
    ).unwrap();
    let head_bytes = context_bytes - history_bytes + 2;
    let padding = "x".repeat(ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES - head_bytes);
    call.context.tool_catalog["size_probe"] = serde_json::json!(format!(
        "{}{}", call.context.tool_catalog["size_probe"].as_str().unwrap(), padding,
    ));
    let before = serde_json::to_vec(&call.context).unwrap();
    let digest = call.context_digest().unwrap();
    assert!(before.len() > ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES);
    for schema in [5, 6] {
        call.grant.schema_version = schema;
        assert!(call.context.size_is_bounded(&call.grant).unwrap());
    }
    for schema in 1..=4 {
        call.grant.schema_version = schema;
        assert!(!call.context.size_is_bounded(&call.grant).unwrap());
    }
    call.grant.schema_version = 5;
    assert_eq!(serde_json::to_vec(&call.context).unwrap(), before);
    assert_eq!(call.context_digest().unwrap(), digest);
    let original = call.context.tool_catalog["size_probe"].as_str().unwrap().to_owned();
    call.context.tool_catalog["size_probe"] = serde_json::json!(format!("{original}x"));
    assert!(!call.context.size_is_bounded(&call.grant).unwrap());
}

#[test]
fn context_history_cardinality_and_absent_history_remain_bounded() {
    let mut call = fixture();
    call.grant.schema_version = 5;
    let state = call.context.source_session.continuation.as_mut().unwrap();
    state.authorizations.resize(usize::from(crate::ADAPTIVE_RESUME_MAX_REVIEWS),
        state.authorizations[0].clone());
    assert!(call.context.size_is_bounded(&call.grant).unwrap());
    let state = call.context.source_session.continuation.as_mut().unwrap();
    state.authorizations.push(state.authorizations[0].clone());
    assert!(!call.context.size_is_bounded(&call.grant).unwrap());
    call.context.source_session.continuation = None;
    assert!(call.context.size_is_bounded(&call.grant).unwrap());
    call.context.tool_catalog = serde_json::json!({"oversize": "x".repeat(ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES)});
    assert!(!call.context.size_is_bounded(&call.grant).unwrap());
}
