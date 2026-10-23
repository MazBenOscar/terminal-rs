use serde_json::{Value, json};

fn definition<'a>(schema: &'a Value, name: &str) -> &'a Value {
    schema["definitions"]
        .get(name)
        .or_else(|| schema["definitions"]["v2"].get(name))
        .unwrap_or_else(|| panic!("missing definition {name}"))
}

fn variant<'a>(schema: &'a Value, name: &str, tag: &str, spelling: &str) -> &'a Value {
    definition(schema, name)["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| {
            value["properties"][tag]["enum"].as_array().is_some_and(|values| values.contains(&json!(spelling)))
        })
        .unwrap_or_else(|| panic!("missing {name}.{tag}={spelling}"))
}

fn fields(value: &Value, names: &str) {
    for name in names.split_whitespace() {
        assert!(value["properties"].get(name).is_some(), "missing field {name}");
    }
}

#[test]
fn committed_schema_covers_every_wire_method_field_and_enum() {
    let schema: Value = serde_json::from_str(include_str!("../schema/codex-app-server-0.163.0.schema.json")).unwrap();
    for method in [
        "initialize",
        "account/read",
        "account/login/start",
        "account/login/cancel",
        "model/list",
        "thread/start",
        "turn/start",
        "turn/interrupt",
        "turn/steer",
        "account/rateLimits/read",
    ] {
        fields(variant(&schema, "ClientRequest", "method", method), "id method params");
    }
    fields(variant(&schema, "ClientNotification", "method", "initialized"), "method");
    for method in [
        "item/tool/call",
        "item/commandExecution/requestApproval",
        "item/fileChange/requestApproval",
        "item/permissions/requestApproval",
        "applyPatchApproval",
        "execCommandApproval",
    ] {
        fields(variant(&schema, "ServerRequest", "method", method), "id method params");
    }
    for method in [
        "item/started",
        "item/completed",
        "item/agentMessage/delta",
        "item/reasoning/textDelta",
        "item/reasoning/summaryTextDelta",
        "thread/tokenUsage/updated",
        "account/rateLimits/updated",
        "turn/completed",
        "account/login/completed",
        "error",
    ] {
        fields(variant(&schema, "ServerNotification", "method", method), "method params");
    }
    for (name, names) in [
        ("InitializeParams", "clientInfo capabilities"),
        ("ClientInfo", "name version"),
        ("InitializeCapabilities", "experimentalApi"),
        ("GetAccountParams", "refreshToken"),
        ("GetAccountResponse", "account"),
        ("CancelLoginAccountParams", "loginId"),
        ("AccountLoginCompletedNotification", "loginId success"),
        ("ModelListParams", "cursor includeHidden"),
        ("ModelListResponse", "data nextCursor"),
        ("Model", "id model displayName defaultReasoningEffort supportedReasoningEfforts"),
        ("ReasoningEffortOption", "reasoningEffort description"),
        ("ThreadStartParams", "dynamicTools environments cwd sandbox baseInstructions ephemeral serviceTier"),
        ("ThreadStartResponse", "thread"),
        ("Thread", "id"),
        ("TurnStartParams", "threadId input model effort sandboxPolicy additionalContext serviceTier environments"),
        ("TurnStartResponse", "turn"),
        ("Turn", "id status"),
        ("TurnInterruptParams", "threadId turnId"),
        ("TurnSteerParams", "threadId expectedTurnId input additionalContext"),
        ("AdditionalContextEntry", "kind value"),
        ("DynamicToolCallParams", "arguments callId threadId tool turnId"),
        ("DynamicToolCallResponse", "success contentItems"),
        ("CommandExecutionRequestApprovalResponse", "decision"),
        ("FileChangeRequestApprovalResponse", "decision"),
        ("PermissionsRequestApprovalResponse", "permissions scope"),
        ("ItemStartedNotification", "threadId turnId item"),
        ("ItemCompletedNotification", "item"),
        ("AgentMessageDeltaNotification", "delta"),
        ("ReasoningTextDeltaNotification", "delta"),
        ("ReasoningSummaryTextDeltaNotification", "delta"),
        ("GetAccountRateLimitsResponse", "rateLimits rateLimitsByLimitId ordinaryUsageAllowed"),
        ("RateLimitSnapshot", "primary secondary"),
        ("RateLimitWindow", "usedPercent"),
        ("TurnCompletedNotification", "turn"),
    ] {
        fields(definition(&schema, name), names);
    }
    for kind in crate::transport::ALLOWED_ITEMS.iter().copied().chain([
        "commandExecution",
        "fileChange",
        "mcpToolCall",
        "webSearch",
    ]) {
        fields(variant(&schema, "ThreadItem", "type", kind), "type");
    }
    fields(variant(&schema, "DynamicToolSpec", "type", "function"), "type name description inputSchema deferLoading");
    fields(variant(&schema, "Account", "type", "chatgpt"), "type");
    fields(variant(&schema, "Account", "type", "apiKey"), "type");
    fields(variant(&schema, "LoginAccountParams", "type", "chatgpt"), "type");
    fields(variant(&schema, "LoginAccountResponse", "type", "chatgpt"), "type authUrl loginId");
    fields(variant(&schema, "SandboxPolicy", "type", "readOnly"), "type networkAccess");
    fields(variant(&schema, "UserInput", "type", "text"), "type text");
    let image = variant(&schema, "UserInput", "type", "image");
    assert!(image["anyOf"].as_array().unwrap().iter().any(|value| value["properties"].get("url").is_some()));
    fields(variant(&schema, "DynamicToolCallOutputContentItem", "type", "inputText"), "type text");
    fields(variant(&schema, "DynamicToolCallOutputContentItem", "type", "inputImage"), "type imageUrl");
    for (name, spellings) in [
        ("SandboxMode", &["read-only"][..]),
        ("AdditionalContextKind", &["untrusted"][..]),
        ("TurnStatus", &["failed", "interrupted", "completed"][..]),
        ("PermissionGrantScope", &["turn"][..]),
    ] {
        for spelling in spellings {
            assert!(
                definition(&schema, name)["enum"].as_array().unwrap().contains(&json!(spelling)),
                "{name} {spelling}"
            );
        }
    }
    for method in crate::transport::ALLOWED_NOTIFICATIONS {
        fields(variant(&schema, "ServerNotification", "method", method), "method params");
    }
    for name in ["ThreadStartParams", "TurnStartParams"] {
        fields(definition(&schema, name), "approvalPolicy approvalsReviewer");
    }
    assert!(
        definition(&schema, "AskForApproval")["oneOf"][0]["enum"].as_array().unwrap().contains(&json!("untrusted"))
    );
    assert!(definition(&schema, "ApprovalsReviewer")["enum"].as_array().unwrap().contains(&json!("user")));
    // Effort is deliberately open in 0.163.0, not a closed Rust enum.
    assert_eq!(definition(&schema, "ReasoningEffort")["type"], "string");
    for name in ["CommandExecutionApprovalDecision", "FileChangeApprovalDecision"] {
        assert!(definition(&schema, name).to_string().contains("\"decline\""));
    }
    assert!(definition(&schema, "ReviewDecision").to_string().contains("\"denied\""));
}
