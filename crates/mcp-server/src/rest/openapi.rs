//! `OpenAPI` 3.2 document generated from the frozen tool manifest.
//!
//! REST is a rendering of the tool manifest, not a second API (17 §Claim),
//! so nothing here is authored per tool: every path, operation, and request
//! schema is derived from `McpToolDescriptor` and `CoreResourceMeta`. A tool
//! added to a flavor crate appears in this document with no edit here.
//!
//! Both version floors are forced rather than preferred. Below 3.1 the
//! schemars-generated draft 2020-12 `args_schema` would need a
//! down-converter, which is new code that can be wrong. Below 3.2 the Path
//! Item Object has no `query` field, so the `QUERY` operation a read-only
//! tool exposes could only be smuggled through `additionalOperations`.
//! 3.2 keeps the 2020-12 dialect, so the newer floor costs no schema
//! fidelity.
//!
//! The generator is a pure function of the descriptor lists, so it is
//! unit-testable without a database, a transport, or a router.

use std::collections::BTreeMap;

use proxima_core::mcp::{CoreResourceMeta, McpToolAnnotations, McpToolDescriptor};
use serde_json::{Map, Value, json};

use crate::McpAuthContext;
use crate::handler::{
    action_allowed_for_auth, annotations_for_auth, project_dispatcher_actions_for_auth,
};

/// The `query` Path Item fixed field arrives in 3.2; the 2020-12 dialect is
/// unchanged from 3.1, so `args_schema` embeds verbatim.
const OPENAPI_VERSION: &str = "3.2.0";

/// Dialect of every embedded schema — what schemars emits for `T::Args`.
const JSON_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Name of the single security scheme; every operation requires it.
const BEARER_SCHEME: &str = "bearerAuth";

/// Call context is header-borne on REST: there is no JSON-RPC peer identity
/// to author it from, and the reserved body names MCP silently strips are a
/// `400` here instead (17 §Call Context). Tuples are
/// `(header, required, description)`.
const CONTEXT_HEADERS: &[(&str, bool, &str)] = &[
    (
        "X-Proxima-Owner",
        true,
        "Selects the owner for this call. REST is stateless: send it on every \
         request rather than relying on a bound session id.",
    ),
    (
        "X-Proxima-Model-Id",
        false,
        "Authoring model id recorded on writes. Falls back to the token's \
         model id, then `unknown`. Sending it in the request body is a `400`.",
    ),
    (
        "X-Proxima-Self-Perspective",
        false,
        "`P:` reference for the caller's self perspective. Sending it in the \
         request body is a `400`.",
    ),
];

/// The statuses of 17 §Status mapping, each rendered as RFC 9457
/// `application/problem+json`. The mapping itself belongs to the routing
/// module; this list only has to advertise the same set.
const PROBLEM_STATUSES: &[(u16, &str)] = &[
    (
        400,
        "Invalid input, a reserved call-context argument in the request body, \
         an unknown schema, or a body `action` conflicting with the route.",
    ),
    (401, "Bearer token missing or invalid."),
    (403, "Not authorized for this tool or action."),
    (404, "No such tool, action, resource, or memory."),
    (
        409,
        "Idempotency conflict, duplicate ingest, trigger conflict, or a \
         suppressed subject.",
    ),
    (422, "The write would violate F/A/P layering."),
    (500, "Internal error. The detail is deliberately generic."),
    (
        503,
        "A caller-actionable precondition is unavailable; retry is meaningful.",
    ),
];

/// Build the `OpenAPI` 3.2 document for one caller's scope-filtered surface.
///
/// `tools` and `resources` are already filtered by the caller's `ToolScope`,
/// exactly as `tools/list` filters — this function applies no gate of its
/// own. `public_url` becomes the single `servers` entry when the deployment
/// knows its externally reachable origin.
#[must_use]
pub fn document(
    tools: &[&McpToolDescriptor],
    resources: &[&CoreResourceMeta],
    public_url: Option<&str>,
    auth: Option<&McpAuthContext>,
) -> Value {
    // Sorted so two calls with the same surface produce byte-identical
    // documents; serde_json preserves insertion order in this workspace.
    let mut paths: BTreeMap<String, Value> = BTreeMap::new();
    for tool in tools {
        collect_tool_paths(tool, auth, &mut paths);
    }
    for resource in resources {
        collect_resource_path(resource, &mut paths);
    }

    let mut root = Map::new();
    root.insert("openapi".to_string(), json!(OPENAPI_VERSION));
    root.insert("jsonSchemaDialect".to_string(), json!(JSON_SCHEMA_DIALECT));
    root.insert(
        "info".to_string(),
        json!({
            "title": "Proxima REST surface",
            "version": env!("CARGO_PKG_VERSION"),
            "description":
                "Generated from the frozen tool manifest. Every operation \
                 terminates in the same dispatch seam MCP uses, so this \
                 surface grants no authority MCP does not already grant. The \
                 document reflects the presenting token's scope and is served \
                 `Cache-Control: private, no-store`.",
        }),
    );
    if let Some(url) = public_url {
        root.insert("servers".to_string(), json!([{ "url": url }]));
    }
    root.insert(
        "paths".to_string(),
        Value::Object(paths.into_iter().collect()),
    );
    root.insert("components".to_string(), components());
    Value::Object(root)
}

/// One generated operation. The only thing that differs between the two
/// methods on a read-only path is the method itself, so the body is built
/// once and rendered twice.
#[derive(Debug)]
struct Operation<'a> {
    id: String,
    summary: String,
    description: &'a str,
    parameters: Vec<Value>,
    request_schema: Option<Value>,
    produces_schema_ids: &'a [&'a str],
    /// The tool's derived reply schema. `None` for resources, which are
    /// served straight off the dispatch seam and have no descriptor.
    output_schema: Option<&'a Value>,
    annotations: McpToolAnnotations,
}

impl Operation<'_> {
    /// Render this operation under `method`, suffixing `operationId` so the
    /// `post` and `query` operations of one read-only path stay distinct.
    fn render(&self, method: &str) -> Value {
        let mut operation = Map::new();
        operation.insert(
            "operationId".to_string(),
            json!(format!("{}__{method}", self.id)),
        );
        operation.insert("summary".to_string(), json!(self.summary));
        if !self.description.is_empty() {
            operation.insert("description".to_string(), json!(self.description));
        }
        // Silence means write, matching `McpToolDescriptor::is_read_only` —
        // so read-only is always stated. The other two hints carry no such
        // default, and inventing one would misreport a tool that said nothing.
        operation.insert(
            "x-proxima-read-only".to_string(),
            json!(self.annotations.read_only.unwrap_or(false)),
        );
        if let Some(destructive) = self.annotations.destructive {
            operation.insert("x-proxima-destructive".to_string(), json!(destructive));
        }
        if let Some(idempotent) = self.annotations.idempotent {
            operation.insert("x-proxima-idempotent".to_string(), json!(idempotent));
        }
        if !self.parameters.is_empty() {
            operation.insert("parameters".to_string(), json!(self.parameters));
        }
        if let Some(schema) = &self.request_schema {
            operation.insert(
                "requestBody".to_string(),
                json!({
                    "required": true,
                    "content": { "application/json": { "schema": schema } },
                }),
            );
        }
        operation.insert(
            "responses".to_string(),
            responses(self.produces_schema_ids, self.output_schema),
        );
        // Stated per operation rather than once at the root: this document is
        // generated per caller and routinely sliced by client generators, and
        // a root default does not survive that.
        let mut requirement = Map::new();
        requirement.insert(BEARER_SCHEME.to_string(), json!([]));
        operation.insert(
            "security".to_string(),
            Value::Array(vec![Value::Object(requirement)]),
        );
        Value::Object(operation)
    }
}

/// Whole-tool path plus one path per dispatcher action.
fn collect_tool_paths(
    tool: &McpToolDescriptor,
    auth: Option<&McpAuthContext>,
    paths: &mut BTreeMap<String, Value>,
) {
    let annotations = annotations_for_auth(auth, tool).unwrap_or_default();
    let projected_schema = project_dispatcher_actions_for_auth(tool, auth);
    let whole = Operation {
        id: tool.name.to_string(),
        summary: format!("Invoke {}", tool.name),
        description: tool.description,
        parameters: context_parameter_refs(),
        request_schema: Some(embeddable_schema(&projected_schema)),
        produces_schema_ids: tool.produces_schema_ids,
        output_schema: Some(&tool.output_schema),
        annotations,
    };
    paths.insert(format!("/v1/tools/{}", tool.name), tool_path_item(&whole));

    // Action routes come from `action_arg_specs`, which is what the router
    // enumerates — not from the `x-proxima-actions` extension. The two are
    // not interchangeable even though both registration entry points now
    // fill the specs and `try_freeze` refuses a registry where they
    // disagree: the extension is the derived, client-facing *description*
    // of a dispatcher (it carries per-field prose the specs do not), while
    // the specs are the enumeration every seam dispatches on. Reading the
    // enumeration off the enumeration is what keeps this document and the
    // router describing one surface.
    let extension = tool
        .args_schema
        .get("x-proxima-actions")
        .and_then(Value::as_object);
    let discriminator = discriminator_key(&tool.args_schema).unwrap_or("action");
    for spec in tool
        .action_arg_specs
        .iter()
        .filter(|spec| auth.is_none() || action_allowed_for_auth(auth, tool, spec.action))
    {
        let action = spec.action;
        // The extension carries the same field sets plus per-field prose, so
        // prefer it; a tool that declared specs without it still narrows.
        let synthesized;
        let action_meta = if let Some(meta) = extension.and_then(|map| map.get(action)) {
            meta
        } else {
            synthesized = json!({
                "allowed_fields": spec.allowed_fields,
                "required_fields": spec.required_fields,
                "field_descriptions": {},
            });
            &synthesized
        };
        // Per-action, not tool-level. The spec is the same authority the
        // owner-role gate and router read; missing annotations stay a write.
        let action_annotations = spec.annotations.unwrap_or_default();
        let action_description = tool
            .resolved_action_description(action)
            .unwrap_or(tool.description);
        let narrowed = Operation {
            id: format!("{}__{action}", tool.name),
            summary: format!("Invoke {} action `{action}`", tool.name),
            description: action_description,
            parameters: context_parameter_refs(),
            request_schema: Some(narrowed_action_schema(
                &tool.args_schema,
                discriminator,
                action_meta,
            )),
            produces_schema_ids: tool.produces_schema_ids,
            output_schema: Some(&tool.output_schema),
            annotations: action_annotations,
        };
        paths.insert(
            format!("/v1/tools/{}/{action}", tool.name),
            tool_path_item(&narrowed),
        );
    }
}

/// `POST` always; `QUERY` as well when the tool declared itself read-only.
///
/// `POST` is kept alongside `QUERY` rather than replaced because middleboxes
/// routinely reject unrecognized methods, and a read unreachable through a
/// customer proxy is worse than a read with imprecise semantics
/// (17 §Methods).
fn tool_path_item(operation: &Operation<'_>) -> Value {
    let mut item = Map::new();
    item.insert("post".to_string(), operation.render("post"));
    if operation.annotations.read_only.unwrap_or(false) {
        // A real Path Item fixed field in 3.2 — not `additionalOperations`.
        item.insert("query".to_string(), operation.render("query"));
    }
    Value::Object(item)
}

/// `GET /v1/resources/{path}`, reconstructed from the resource's own
/// `proxima://` template. The mapping is total and mechanical, and REST adds
/// no URI parser of its own (17 §Resource path mapping).
fn collect_resource_path(resource: &CoreResourceMeta, paths: &mut BTreeMap<String, Value>) {
    let Some((path, query_variables)) = resource_route(resource.uri_template) else {
        return;
    };
    let mut parameters = context_parameter_refs();
    parameters.extend(path_template_variables(&path).into_iter().map(|name| {
        json!({
            "name": name,
            "in": "path",
            "required": true,
            "schema": { "type": "string" },
        })
    }));
    parameters.extend(query_variables.into_iter().map(|name| {
        json!({
            "name": name,
            "in": "query",
            "required": false,
            "schema": { "type": "string" },
        })
    }));
    let operation = Operation {
        id: format!("resources__{}", resource.name),
        summary: resource.title.to_string(),
        description: resource.description,
        parameters,
        request_schema: None,
        // Resources are the browsable read surface; every one of them is a
        // read, which is why they exist as a separate concept from tools.
        produces_schema_ids: &[],
        output_schema: None,
        annotations: McpToolAnnotations::new().read_only(true),
    };
    let mut item = Map::new();
    item.insert("get".to_string(), operation.render("get"));
    paths.insert(path, Value::Object(item));
}

/// Split a `proxima://` URI template into a REST path and its RFC 6570 query
/// variable names: `proxima://memory/{id}{?expand_neighbors}` becomes
/// `/v1/resources/memory/{id}` and `["expand_neighbors"]`.
fn resource_route(uri_template: &str) -> Option<(String, Vec<String>)> {
    let rest = uri_template.strip_prefix("proxima://")?;
    let (path, query) = match rest.split_once("{?") {
        Some((path, query)) => (path, query.trim_end_matches('}')),
        None => (rest, ""),
    };
    let variables = query
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    Some((format!("/v1/resources/{path}"), variables))
}

/// The `{name}` placeholders of a path template, in order of appearance.
fn path_template_variables(path: &str) -> Vec<String> {
    let mut variables = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        variables.push(after[..close].to_string());
        rest = &after[close + 1..];
    }
    variables
}

/// Narrow a dispatcher tool's flattened `args_schema` to a single action.
///
/// The flattened schema an MCP client sees is the union of every variant's
/// fields behind one discriminator, with per-action requiredness recorded
/// only in `x-proxima-actions` and enforced at runtime by serde. A REST
/// route supplies the action itself, so the operation can advertise exactly
/// that variant: `allowed_fields` selects the properties, `required_fields`
/// becomes `required`, the discriminator property is dropped because the
/// path carries it, and each field's variant-specific prose replaces the
/// neutralized "shared dispatcher field" text the flattener wrote for fields
/// that appear in more than one variant. Strictly more precise than the MCP
/// surface, and the main reason this generator exists.
fn narrowed_action_schema(args_schema: &Value, discriminator: &str, action_meta: &Value) -> Value {
    let flattened = args_schema.get("properties").and_then(Value::as_object);
    let descriptions = action_meta
        .get("field_descriptions")
        .and_then(Value::as_object);
    let mut properties = Map::new();
    for field in string_list(action_meta, "allowed_fields") {
        if field == discriminator {
            continue;
        }
        let Some(property) = flattened.and_then(|properties| properties.get(&field)) else {
            continue;
        };
        let mut property = property.clone();
        if let Some(description) = descriptions
            .and_then(|descriptions| descriptions.get(&field))
            .and_then(Value::as_str)
            && let Some(map) = property.as_object_mut()
        {
            map.insert("description".to_string(), json!(description));
        }
        properties.insert(field, property);
    }
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": string_list(action_meta, "required_fields"),
        "additionalProperties": false,
    })
}

/// The discriminator key the flattener chose (`action`, `kind`, …).
///
/// It writes `required = [discriminator]` on the flattened root, which is
/// the only place the key survives; a flavor dispatcher may tag on something
/// other than `action`.
fn discriminator_key(args_schema: &Value) -> Option<&str> {
    args_schema.get("required")?.as_array()?.first()?.as_str()
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The document declares the dialect once through `jsonSchemaDialect`, so
/// the root `$schema` schemars emits is redundant in every operation body.
fn embeddable_schema(args_schema: &Value) -> Value {
    let mut schema = args_schema.clone();
    if let Some(map) = schema.as_object_mut() {
        map.shift_remove("$schema");
    }
    schema
}

fn responses(produces_schema_ids: &[&str], output_schema: Option<&Value>) -> Value {
    let mut responses = Map::new();
    responses.insert(
        "200".to_string(),
        success_response(produces_schema_ids, output_schema),
    );
    for (status, _) in PROBLEM_STATUSES {
        responses.insert(
            status.to_string(),
            json!({ "$ref": format!("#/components/responses/Problem{status}") }),
        );
    }
    Value::Object(responses)
}

/// The success body of an operation.
///
/// The reply a client validates, taken from the tool's own `Output` type.
///
/// `produces_schema_ids` names the *registry* payloads a tool writes and is
/// kept alongside as an annotation — it answers a different question, and a
/// tool can write payloads without echoing them. Resources have no
/// descriptor, so they fall back to an unconstrained object.
fn success_response(produces_schema_ids: &[&str], output_schema: Option<&Value>) -> Value {
    let mut schema = if let Some(Value::Object(map)) = output_schema {
        map.clone()
    } else {
        let mut map = Map::new();
        map.insert("type".to_string(), json!("object"));
        map
    };
    if !produces_schema_ids.is_empty() {
        schema.insert(
            "x-proxima-produces-schema-ids".to_string(),
            json!(produces_schema_ids),
        );
    }
    json!({
        "description": "Tool or resource result.",
        "content": { "application/json": { "schema": Value::Object(schema) } },
    })
}

fn components() -> Value {
    let mut parameters = Map::new();
    for (header, required, description) in CONTEXT_HEADERS {
        parameters.insert(
            parameter_component_name(header),
            json!({
                "name": header,
                "in": "header",
                "required": required,
                "description": description,
                "schema": { "type": "string" },
            }),
        );
    }

    let mut error_responses = Map::new();
    for (status, description) in PROBLEM_STATUSES {
        error_responses.insert(
            format!("Problem{status}"),
            json!({
                "description": description,
                "content": {
                    "application/problem+json": {
                        "schema": { "$ref": "#/components/schemas/ProblemDetails" },
                    },
                },
            }),
        );
    }

    json!({
        "securitySchemes": {
            BEARER_SCHEME: {
                "type": "http",
                "scheme": "bearer",
                "description":
                    "The same bearer token the MCP surface accepts. REST grants \
                     no authority MCP does not already grant.",
            },
        },
        "parameters": Value::Object(parameters),
        "responses": Value::Object(error_responses),
        "schemas": {
            "ProblemDetails": {
                "type": "object",
                "description":
                    "RFC 9457 problem detail. `detail` is the error's \
                     client-facing message, never a formatted source chain.",
                "properties": {
                    "type": { "type": "string", "format": "uri" },
                    "title": { "type": "string" },
                    "status": { "type": "integer" },
                    "detail": { "type": "string" },
                    "instance": { "type": "string" },
                    "request_id": { "type": "string" },
                },
                "required": ["type", "title", "status"],
            },
        },
    })
}

fn context_parameter_refs() -> Vec<Value> {
    CONTEXT_HEADERS
        .iter()
        .map(|(header, _, _)| {
            json!({
                "$ref": format!(
                    "#/components/parameters/{}",
                    parameter_component_name(header),
                ),
            })
        })
        .collect()
}

fn parameter_component_name(header: &str) -> String {
    header.replace('-', "")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proxima_core::mcp::CORE_RESOURCES;
    use proxima_core::protocol::tool as protocol_tool;
    use proxima_core::{FlavorRegistry, FlavorRegistryFrozen};

    use super::{CoreResourceMeta, McpToolDescriptor, Value, document, json};

    fn frozen_registry() -> FlavorRegistryFrozen {
        FlavorRegistry::default()
            .try_freeze()
            .expect("the built-in core registry freezes")
    }

    /// Generated from the real frozen registry, so the tests exercise the
    /// descriptors the server actually serves rather than hand-made ones.
    fn core_document(registry: &FlavorRegistryFrozen) -> Value {
        let tools: Vec<&McpToolDescriptor> = registry.list_mcp_tools().iter().collect();
        let resources: Vec<&CoreResourceMeta> = CORE_RESOURCES.iter().collect();
        document(&tools, &resources, Some("https://proxima.example"), None)
    }

    fn path_item<'a>(document: &'a Value, path: &str) -> &'a Value {
        document
            .pointer(&format!(
                "/paths/{}",
                path.replace('~', "~0").replace('/', "~1")
            ))
            .unwrap_or_else(|| panic!("path {path} present: {document:#}"))
    }

    /// Every `operationId` in the document, in document order.
    fn operation_ids(document: &Value) -> Vec<String> {
        let mut ids = Vec::new();
        let paths = document
            .get("paths")
            .and_then(Value::as_object)
            .expect("paths object");
        for item in paths.values() {
            let Some(item) = item.as_object() else {
                continue;
            };
            for operation in item.values() {
                if let Some(id) = operation.get("operationId").and_then(Value::as_str) {
                    ids.push(id.to_string());
                }
            }
        }
        ids
    }

    #[test]
    fn document_pins_openapi_3_2_and_the_2020_12_dialect() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        assert_eq!(
            document.get("openapi").and_then(Value::as_str),
            Some("3.2.0"),
            "3.2 is the floor at which `query` is a Path Item fixed field: {document:#}",
        );
        assert_eq!(
            document.get("jsonSchemaDialect").and_then(Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema"),
            "embedded schemas are schemars draft 2020-12: {document:#}",
        );
        assert_eq!(
            document.pointer("/servers/0/url").and_then(Value::as_str),
            Some("https://proxima.example"),
        );
    }

    #[test]
    fn public_url_is_the_only_source_of_servers() {
        let registry = frozen_registry();
        let tools: Vec<&McpToolDescriptor> = registry.list_mcp_tools().iter().collect();
        let document = document(&tools, &[], None, None);
        assert!(
            document.get("servers").is_none(),
            "without a public url the document must declare no server: {document:#}",
        );
    }

    #[test]
    fn a_read_only_tool_exposes_both_post_and_query() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let item = path_item(
            &document,
            &format!("/v1/tools/{}", protocol_tool::CORE_SEARCH_MEMORIES),
        );
        assert!(
            item.get("post").is_some() && item.get("query").is_some(),
            "a read-only tool is reachable by both methods: {item:#}",
        );
        assert!(
            item.get("additionalOperations").is_none(),
            "`query` is a 3.2 fixed field, not an additional operation: {item:#}",
        );
        assert_eq!(
            item.pointer("/post/x-proxima-read-only")
                .and_then(Value::as_bool),
            Some(true),
        );
        assert_ne!(
            item.pointer("/post/operationId"),
            item.pointer("/query/operationId"),
            "the two operations on one path must not collide: {item:#}",
        );
    }

    /// The generator enumerates dispatcher actions from the same place the
    /// router does — `McpToolDescriptor::action_arg_specs` — so a flavor
    /// dispatcher is documented exactly as a substrate one, and a descriptor
    /// the freeze guard would reject still cannot produce a 404-ing route.
    ///
    /// The two sources are not interchangeable. `x-proxima-actions` is
    /// stamped into `args_schema` by the schema pass for *any* internally
    /// tagged `Args`: it is the derived client-facing description, and it
    /// carries per-field prose the specs do not. The specs are the
    /// enumeration. Reading the enumeration off the description is what
    /// produced the second case below.
    #[test]
    fn action_routes_follow_the_specs_the_router_reads() {
        const SPECS: &[proxima_core::mcp::McpActionArgSpec] =
            &[proxima_core::mcp::McpActionArgSpec {
                action: "look",
                allowed_fields: &["id"],
                required_fields: &["id"],
                annotations: Some(
                    proxima_core::mcp::McpToolAnnotations::new()
                        .read_only(true)
                        .open_world(false),
                ),
            }];

        fn stub(
            action_arg_specs: &'static [proxima_core::mcp::McpActionArgSpec],
        ) -> McpToolDescriptor {
            McpToolDescriptor {
                name: "proxima-stub_dispatch",
                description: "stub",
                origin: proxima_core::mcp::McpToolOrigin::Flavor("proxima-stub".to_string()),
                produces_schema_ids: &[],
                args_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["look"] },
                        "id": { "type": "string" },
                    },
                    "required": ["action"],
                    "x-proxima-actions": {
                        "look": {
                            "allowed_fields": ["id"],
                            "required_fields": ["id"],
                            "field_descriptions": {},
                        },
                    },
                }),
                output_schema: json!({ "type": "object" }),
                action_arg_specs,
                annotations: None,
                call: &|_, _| Box::pin(async { Ok(Value::Null) }),
            }
        }

        // A flavor dispatcher that declared its actions: both forms are
        // advertised, and the narrowed one is the more precise operation.
        let declared = stub(SPECS);
        let declared_document = document(&[&declared], &[], None, None);
        let paths = declared_document
            .get("paths")
            .and_then(Value::as_object)
            .expect("paths object");
        assert!(
            paths.contains_key("/v1/tools/proxima-stub_dispatch"),
            "the whole-tool route is advertised: {paths:#?}",
        );
        assert!(
            paths.contains_key("/v1/tools/proxima-stub_dispatch/look"),
            "a flavor dispatcher's action route is advertised, exactly as a \
             substrate one is: {paths:#?}",
        );
        let narrowed = path_item(&declared_document, "/v1/tools/proxima-stub_dispatch/look")
            .pointer("/post/requestBody/content/application~1json/schema")
            .expect("the narrowed operation carries a request schema");
        assert!(
            narrowed.pointer("/properties/id").is_some(),
            "the narrowed schema keeps the action's own fields: {narrowed:#}",
        );
        assert!(
            narrowed.pointer("/properties/action").is_none(),
            "`action` is carried by the route, not the body: {narrowed:#}",
        );
        assert_eq!(
            narrowed.get("required"),
            Some(&json!(["id"])),
            "the action's required_fields become the schema's required: {narrowed:#}",
        );

        // The extension without the specs. `try_freeze` refuses to seal a
        // registry containing this, so it is unreachable through the
        // registry — but the generator must not be the thing that depends on
        // that, because advertising a route the router would 404 is worse
        // than advertising one action fewer.
        let undeclared = stub(&[]);
        let undeclared_document = document(&[&undeclared], &[], None, None);
        let paths = undeclared_document
            .get("paths")
            .and_then(Value::as_object)
            .expect("paths object");
        assert!(
            paths.contains_key("/v1/tools/proxima-stub_dispatch"),
            "the whole-tool route is still advertised: {paths:#?}",
        );
        assert!(
            !paths.contains_key("/v1/tools/proxima-stub_dispatch/look"),
            "no action route may be advertised when the router would not \
             serve one: {paths:#?}",
        );
    }

    /// The 200 body is the tool's own derived reply schema, not a bare
    /// object. `produces_schema_ids` rides alongside as an annotation because
    /// it answers a different question — which registry payloads the tool
    /// writes — and a tool can write payloads without echoing them.
    #[test]
    fn a_success_response_carries_the_derived_output_schema() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let schema = path_item(
            &document,
            &format!("/v1/tools/{}", protocol_tool::CORE_REMEMBER),
        )
        .pointer("/post/responses/200/content/application~1json/schema")
        .expect("a 200 schema");
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{schema:#}",
        );
        assert!(
            schema.get("properties").is_some_and(Value::is_object),
            "the reply schema must describe real fields, not an empty object: {schema:#}",
        );

        let remember = registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == protocol_tool::CORE_REMEMBER)
            .expect("core_remember is registered");
        assert_eq!(
            schema.get("properties"),
            remember.output_schema.get("properties"),
            "the document must carry the manifest's schema verbatim",
        );
    }

    #[test]
    fn a_write_tool_exposes_post_only() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let item = path_item(
            &document,
            &format!("/v1/tools/{}", protocol_tool::CORE_REMEMBER),
        );
        assert!(item.get("post").is_some(), "{item:#}");
        assert!(
            item.get("query").is_none(),
            "a write tool must not advertise QUERY: {item:#}",
        );
        assert_eq!(
            item.pointer("/post/x-proxima-read-only")
                .and_then(Value::as_bool),
            Some(false),
        );
    }

    /// Method selection is per-action, not per-tool. `core_upload` is a write
    /// dispatcher, but its `read_url` action is annotated read-only — so the
    /// action path must offer QUERY while its sibling write actions must not.
    /// Resolving this at tool level would both hide real reads and, on a
    /// dispatcher annotated read-only at tool level, offer a safe
    /// auto-retryable method on a write action added later.
    #[test]
    fn action_paths_gate_methods_per_action_not_per_tool() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let parent = path_item(
            &document,
            &format!("/v1/tools/{}", protocol_tool::CORE_UPLOAD),
        );
        assert!(
            parent.get("query").is_none(),
            "the parent dispatcher is a write tool: {parent:#}",
        );

        let read_url = path_item(
            &document,
            &format!("/v1/tools/{}/read_url", protocol_tool::CORE_UPLOAD),
        );
        assert!(
            read_url.get("query").is_some(),
            "a read-only action must advertise QUERY even on a write parent: {read_url:#}",
        );
        assert_eq!(
            read_url
                .pointer("/query/x-proxima-read-only")
                .and_then(Value::as_bool),
            Some(true),
        );

        let prepare = path_item(
            &document,
            &format!("/v1/tools/{}/prepare", protocol_tool::CORE_UPLOAD),
        );
        assert!(
            prepare.get("query").is_none(),
            "a write action must not advertise QUERY: {prepare:#}",
        );
    }

    /// The narrowing is the reason this generator exists, so it is checked
    /// against every real dispatcher action rather than one sampled case.
    #[test]
    fn dispatcher_action_schemas_are_narrowed_to_that_action() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let mut checked = 0usize;
        for tool in registry.list_mcp_tools() {
            let Some(actions) = tool
                .args_schema
                .get("x-proxima-actions")
                .and_then(Value::as_object)
            else {
                continue;
            };
            let discriminator = super::discriminator_key(&tool.args_schema).unwrap_or("action");
            for (action, meta) in actions {
                let item = path_item(&document, &format!("/v1/tools/{}/{action}", tool.name));
                let schema = item
                    .pointer("/post/requestBody/content/application~1json/schema")
                    .unwrap_or_else(|| panic!("narrowed request schema present: {item:#}"));

                let advertised: BTreeSet<String> = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .expect("narrowed schema has properties")
                    .keys()
                    .cloned()
                    .collect();
                let expected: BTreeSet<String> = super::string_list(meta, "allowed_fields")
                    .into_iter()
                    .filter(|field| field != discriminator)
                    .collect();
                assert_eq!(
                    advertised, expected,
                    "{}/{action} must advertise exactly its allowed fields: {schema:#}",
                    tool.name,
                );
                assert!(
                    schema
                        .pointer(&format!("/properties/{discriminator}"))
                        .is_none(),
                    "{}/{action} must drop the `{discriminator}` property; the route supplies it: {schema:#}",
                    tool.name,
                );
                assert_eq!(
                    super::string_list(schema, "required"),
                    super::string_list(meta, "required_fields"),
                    "{}/{action} must carry the action's own required set: {schema:#}",
                    tool.name,
                );
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "the core registry must contribute at least one dispatcher action",
        );
    }

    #[test]
    fn operation_ids_are_unique_across_the_document() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let ids = operation_ids(&document);
        let unique: BTreeSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "operationId collision among {} operations",
            ids.len(),
        );
        assert!(
            !ids.is_empty(),
            "the core registry must generate operations",
        );
    }

    #[test]
    fn every_operation_requires_the_bearer_scheme() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let paths = document
            .get("paths")
            .and_then(Value::as_object)
            .expect("paths object");
        for (path, item) in paths {
            for (method, operation) in item.as_object().expect("path item object") {
                assert!(
                    operation.pointer("/security/0/bearerAuth").is_some(),
                    "{method} {path} must require the bearer scheme: {operation:#}",
                );
            }
        }
        assert!(
            document
                .pointer("/components/securitySchemes/bearerAuth/scheme")
                .and_then(Value::as_str)
                == Some("bearer"),
            "{document:#}",
        );
    }

    #[test]
    fn resource_templates_map_onto_v1_resource_paths() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let item = path_item(&document, "/v1/resources/memory/{id}");
        assert!(item.get("get").is_some(), "{item:#}");
        let parameters = item
            .pointer("/get/parameters")
            .and_then(Value::as_array)
            .expect("parameters present");
        assert!(
            parameters
                .iter()
                .any(
                    |parameter| parameter.get("name").and_then(Value::as_str) == Some("id")
                        && parameter.get("in").and_then(Value::as_str) == Some("path")
                ),
            "the `{{id}}` template variable must become a path parameter: {parameters:#?}",
        );
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter.get("name").and_then(Value::as_str)
                    == Some("expand_neighbors")
                    && parameter.get("in").and_then(Value::as_str) == Some("query")),
            "RFC 6570 query variables must become query parameters: {parameters:#?}",
        );
    }

    #[test]
    fn call_context_headers_are_documented_on_every_operation() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        for header in [
            "X-Proxima-Owner",
            "X-Proxima-Model-Id",
            "X-Proxima-Self-Perspective",
        ] {
            let component = format!("/components/parameters/{}", header.replace('-', ""));
            assert_eq!(
                document
                    .pointer(&format!("{component}/name"))
                    .and_then(Value::as_str),
                Some(header),
                "{document:#}",
            );
        }
        let item = path_item(
            &document,
            &format!("/v1/tools/{}", protocol_tool::CORE_REMEMBER),
        );
        let parameters = item
            .pointer("/post/parameters")
            .and_then(Value::as_array)
            .expect("parameters present");
        assert_eq!(parameters.len(), 3, "{parameters:#?}");
    }

    #[test]
    fn error_statuses_reference_the_shared_problem_component() {
        let registry = frozen_registry();
        let document = core_document(&registry);
        let item = path_item(
            &document,
            &format!("/v1/tools/{}", protocol_tool::CORE_REMEMBER),
        );
        for status in ["400", "401", "403", "404", "409", "422", "500", "503"] {
            assert_eq!(
                item.pointer(&format!("/post/responses/{status}/$ref"))
                    .and_then(Value::as_str),
                Some(format!("#/components/responses/Problem{status}").as_str()),
                "{item:#}",
            );
            assert_eq!(
                document
                    .pointer(&format!(
                        "/components/responses/Problem{status}/content/application~1problem+json/schema/$ref"
                    ))
                    .and_then(Value::as_str),
                Some("#/components/schemas/ProblemDetails"),
                "{document:#}",
            );
        }
    }
}
