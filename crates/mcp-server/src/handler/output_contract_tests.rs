use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use super::{host_tool_metadata, structured_tool_output};
use crate::McpHostTool;

#[derive(Debug, Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<String>>>);

#[derive(Default)]
struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(self.0, " {}={value:?}", field.name()).unwrap();
    }
}

impl Subscriber for CapturedLogs {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap()
            .push(format!("{}{}", event.metadata().level(), fields.0));
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

fn host_tool(output_schema: Value) -> McpHostTool {
    McpHostTool {
        name: "host_shapes".into(),
        description: "Output contract fixture".into(),
        args_schema: json!({"type": "object"}),
        output_schema,
        effect: proxima_core::ToolEffect::ReadOnly,
    }
}

#[test]
fn host_object_union_metadata_has_object_type_and_retains_its_branches() {
    for union in ["oneOf", "anyOf"] {
        let schema = json!({
            "title": "HostUnion",
            union: [
                {"type": "object", "properties": {"value": {"type": "string"}}},
                {"type": "object", "properties": {"count": {"type": "integer"}}},
            ],
        });
        let metadata = host_tool_metadata(host_tool(schema.clone())).expect("object union listed");
        let metadata = serde_json::to_value(metadata).unwrap();
        assert_eq!(metadata["outputSchema"]["type"], "object");
        assert_eq!(metadata["outputSchema"]["title"], schema["title"]);
        assert_eq!(metadata["outputSchema"][union], schema[union]);
    }
}

#[test]
fn invalid_host_output_schemas_are_not_listed_and_warn_with_the_tool_name() {
    for schema in [
        json!(true),
        json!(false),
        json!({}),
        json!({"type": "string"}),
        json!({"type": "array", "items": {"type": "object"}}),
        json!({"type": ["object", "null"]}),
        json!({"anyOf": [{"type": "object"}, {"type": "null"}]}),
        json!({"oneOf": []}),
    ] {
        let logs = CapturedLogs::default();
        tracing::subscriber::with_default(logs.clone(), || {
            assert!(
                host_tool_metadata(host_tool(schema.clone())).is_none(),
                "{schema}"
            );
        });
        let events = logs.0.lock().unwrap();
        assert!(
            events.iter().any(|event| event.contains("WARN")
                && event.contains("tool=host_shapes")
                && event
                    .contains("host tool output schema must describe JSON objects; not listed")),
            "{events:?}"
        );
    }
}

#[test]
fn nonobject_tool_output_is_redacted_and_logs_only_the_tool_name() {
    for output in [
        json!(null),
        json!("private-output"),
        json!(42),
        json!(true),
        json!(["private-output"]),
    ] {
        let logs = CapturedLogs::default();
        let error = tracing::subscriber::with_default(logs.clone(), || {
            structured_tool_output("host_shapes", output).expect_err("non-object output refused")
        });
        assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, "internal server error");
        assert!(error.data.is_none());
        let events = logs.0.lock().unwrap();
        assert!(
            events.iter().any(|event| event.contains("ERROR")
                && event.contains("tool=host_shapes")
                && event.contains("mcp tool output must be a JSON object")),
            "{events:?}"
        );
        assert!(events.iter().all(|event| !event.contains("private-output")));
    }
    let object = json!({"answer": [42], "nested": {"value": null}});
    let (structured, text) = structured_tool_output("host_shapes", object.clone()).unwrap();
    assert_eq!(structured, object);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), object);
}
