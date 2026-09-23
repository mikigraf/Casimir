//! Redaction for explicitly shared artifacts. Raw local evidence remains private.
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

fn sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_'], "");
    key == "token"
        || [
            "apikey",
            "authtoken",
            "accesstoken",
            "refreshtoken",
            "sessiontoken",
            "idtoken",
            "githubtoken",
            "gitlabtoken",
            "privatekey",
            "password",
            "secret",
            "authorization",
            "cookie",
            "credential",
        ]
        .iter()
        .any(|s| key.contains(s))
}
pub fn redact(text: &str) -> String {
    static TOKENS: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENTS: OnceLock<Regex> = OnceLock::new();
    let tokens = TOKENS.get_or_init(|| Regex::new(r"(?i)(?:sk-(?:ant-)?[a-z0-9_-]{12,}|gh[pousr]_[a-z0-9_]{12,}|github_pat_[a-z0-9_]{12,}|bearer\s+[a-z0-9._~+/=-]+)").unwrap());
    let assignments = ASSIGNMENTS.get_or_init(|| Regex::new(r#"(?i)((?:[a-z0-9_]*(?:api[_-]?key|auth[_-]?token|access[_-]?token|refresh[_-]?token|password|secret)|authorization|cookie)\s*[=:]\s*["']?)[^\s"',;}]+"#).unwrap());
    let mut out = text.to_owned();
    for (key, value) in std::env::vars_os() {
        if sensitive(&key.to_string_lossy()) {
            let value = value.to_string_lossy();
            if value.len() >= 6 {
                out = out.replace(value.as_ref(), "[REDACTED]");
            }
        }
    }
    out = tokens.replace_all(&out, "[REDACTED]").into_owned();
    assignments.replace_all(&out, "${1}[REDACTED]").into_owned()
}
pub fn redact_value(value: &mut Value) {
    match value {
        Value::String(s) => *s = redact(s),
        Value::Array(items) => items.iter_mut().for_each(redact_value),
        Value::Object(items) => {
            for (key, value) in items {
                if sensitive(key) {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    redact_value(value);
                }
            }
        }
        _ => {}
    }
}
pub fn share(value: &Value) -> Value {
    let mut data = value.clone();
    redact_value(&mut data);
    // Checkpoint identifiers and local source paths are not a portable sharing format.
    if let Some(obj) = data.as_object_mut() {
        for key in ["checkpoints", "harnessLogPath", "path", "workspace"] {
            obj.remove(key);
        }
    }
    serde_json::json!({"schemaVersion":1,"redacted":true,"notice":"Credentials matching known patterns and current credential environment values were removed. Review before sharing; arbitrary secrets cannot be identified perfectly.","data":data})
}
