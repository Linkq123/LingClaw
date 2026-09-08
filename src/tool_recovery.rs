/// Conservative structured retry identity, mirrored by frontend/src/toolRecovery.ts.
/// Only a read's line window is advisory: a successful read of the same file
/// resolves a failed window request. Other operations retain every argument,
/// including action, workspace, query, command, agent and delegated task inputs.
pub(crate) fn canonical_tool_retry_key(name: &str, arguments: &str) -> String {
    let name = name.trim();
    let Ok(mut args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return format!("{name}:raw:{arguments}");
    };
    if name == "read_file"
        && let Some(object) = args.as_object_mut()
    {
        object.remove("start_line");
        object.remove("end_line");
    }
    format!("{name}:args:{args}")
}

#[cfg(test)]
#[path = "tests/tool_recovery_tests.rs"]
mod tests;
