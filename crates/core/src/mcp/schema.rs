//! MCP tool argument- and output-schema generation.
//!
//! The single source of truth for a tool's argument schema is its Rust
//! `Args` type, and for its output schema its `Output` type.
//! `mcp_tool_schema` and `mcp_output_schema` both produce a `$ref`-free /
//! `$defs`-free JSON Schema draft 2020-12 document so that MCP clients
//! which do not resolve `$ref` still render every field (see commit
//! 37f209b). They differ in the client-facing normalization applied
//! afterwards; see `mcp_output_schema`.
//!
//! What reaches the wire is plain JSON Schema: no `x-` extension, no
//! `$schema`, no Rust type name as `title`, no non-standard `format`. A
//! dispatcher's per-action contract is typed data
//! ([`McpDispatcherSchema`]) the flat `inputSchema` is rendered from, so a
//! caller shown a subset of actions is shown only their fields and prose.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;

/// A tool's argument schema as registration derives it from `Args`.
#[derive(Debug, Clone)]
pub(crate) struct McpArgsSchema {
    /// The complete wire `inputSchema`.
    pub(crate) schema: serde_json::Value,
    /// A tagged-enum dispatcher's per-action contract; `None` for a flat
    /// tool.
    pub(crate) dispatcher: Option<McpDispatcherSchema>,
}

/// Generate the `$ref`-free draft-2020-12 argument schema for `T`.
///
/// Panics at registration (startup) if `T` is recursive: `schemars`
/// cannot inline a recursive subschema, so it emits a `$ref` that no
/// inlining pass can eliminate. A recursive MCP tool argument type is a
/// registration error. So is a dispatcher two of whose actions give one
/// field incompatible schemas: the flat `inputSchema` could not describe
/// both.
pub(crate) fn mcp_tool_schema<T: JsonSchema>() -> McpArgsSchema {
    let mut value = generated_schema::<T>(SchemaSettings::draft2020_12());
    let dispatcher = derive_dispatcher(&value).unwrap_or_else(|error| {
        panic!(
            "MCP tool type `{}` has an invalid dispatcher schema: {error}",
            std::any::type_name::<T>(),
        )
    });
    if let Some(dispatcher) = &dispatcher {
        value = dispatcher.render(|_| true);
    }
    ensure_client_safe_root::<T>(&mut value);
    assert!(
        !schema_contains_ref(&value),
        "MCP tool type `{}` is recursive: schemars emitted a $ref that \
         cannot be inlined. MCP tool argument types must be non-recursive.",
        std::any::type_name::<T>(),
    );
    remove_definition_containers(&mut value);
    McpArgsSchema {
        schema: value,
        dispatcher,
    }
}

/// `T`'s schema with subschemas inlined, `$schema` and the Rust type name
/// (`title`) dropped from the root, every non-standard `format` and
/// `default: null` removed, and doc-comment wraps rejoined.
fn generated_schema<T: JsonSchema>(mut settings: SchemaSettings) -> serde_json::Value {
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("JsonSchema must serialize");
    if let Some(root) = value.as_object_mut() {
        root.remove("$schema");
        root.remove("title");
    }
    strip_nonstandard_formats(&mut value);
    strip_null_defaults(&mut value);
    unwrap_descriptions(&mut value);
    value
}

/// Drop `default: null`: an optional field's absence already means none,
/// so the keyword is payload that tells a model nothing.
fn strip_null_defaults(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if map.get("default").is_some_and(serde_json::Value::is_null) {
        map.remove("default");
    }
    for_each_subschema_mut(map, &mut strip_null_defaults);
}

/// Rejoin the hard wraps a multi-line `///` comment leaves in a
/// `description`. A blank line (paragraph) and a line opening a list item
/// (`- `, `* `, `1. `) keep their break; every other break was the source
/// file's line width, not the author's meaning.
fn unwrap_descriptions(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if let Some(serde_json::Value::String(text)) = map.get_mut("description")
        && text.contains('\n')
    {
        *text = unwrap_lines(text);
    }
    for_each_subschema_mut(map, &mut unwrap_descriptions);
}

fn unwrap_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.split('\n').map(str::trim_end).peekable();
    while let Some(line) = lines.next() {
        out.push_str(line);
        let Some(next) = lines.peek() else {
            break;
        };
        let next = next.trim_start();
        let keeps_break = line.is_empty()
            || next.is_empty()
            || next.starts_with("- ")
            || next.starts_with("* ")
            || next.split_once(". ").is_some_and(|(number, _)| {
                !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
            });
        out.push(if keeps_break { '\n' } else { ' ' });
    }
    out
}

/// The `format` values JSON Schema 2020-12 defines. schemars also writes
/// Rust widths (`uint32`, `int64`, `float`, …), which no JSON Schema
/// validator or model API knows; the bound they imply is `minimum` or the
/// server's own decode.
const STANDARD_FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "uri-template",
    "json-pointer",
    "relative-json-pointer",
    "regex",
];

fn strip_nonstandard_formats(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if map
        .get("format")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|format| !STANDARD_FORMATS.contains(&format))
    {
        map.remove("format");
    }
    for_each_subschema_mut(map, &mut strip_nonstandard_formats);
}

/// The output schema as `tools/list` sends it: validation keywords only.
///
/// Output-schema prose never reaches a model — model APIs carry a tool's
/// name, description and input schema — so `outputSchema` is a client-side
/// validation contract and its annotations are payload. Drops
/// `description`, `title`, `examples`, `default`, `$comment`, `$schema` and
/// non-standard `format` at every schema position; property *names* are
/// untouched. The registry keeps the documented schema for the REST
/// projection's `OpenAPI` document.
#[must_use]
pub fn mcp_wire_output_schema(schema: &serde_json::Value) -> serde_json::Value {
    let mut wire = schema.clone();
    if let Some(root) = wire.as_object_mut() {
        root.remove("$schema");
    }
    strip_annotations(&mut wire);
    strip_nonstandard_formats(&mut wire);
    wire
}

fn strip_annotations(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    for key in ["description", "title", "examples", "default", "$comment"] {
        map.remove(key);
    }
    for_each_subschema_mut(map, &mut strip_annotations);
}

/// Generate a `$ref`-free draft-2020-12 *output* schema for `T`.
///
/// Generated for serialization, because a reply is what `T` serializes to:
/// a field `#[serde(skip_serializing_if)]` may omit is optional here, where
/// the deserialize contract would require it and a strict client would
/// refuse the reply.
///
/// MCP output schemas declare an object root. Output unions retain their
/// branches, so clients can validate each reply variant; unlike argument
/// dispatchers, their fields are never merged into one flat call surface.
/// Recursive output types still panic at registration because their `$ref`
/// cannot be inlined.
pub(crate) fn mcp_output_schema<T: JsonSchema>() -> Result<serde_json::Value, String> {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("JsonSchema must serialize");
    assert!(
        !schema_contains_ref(&value),
        "MCP tool output type `{}` is recursive: schemars emitted a $ref that \
         cannot be inlined. MCP tool output types must be non-recursive.",
        std::any::type_name::<T>(),
    );
    normalize_mcp_output_schema(&mut value)?;
    Ok(value)
}

/// Require an MCP output schema to declare an object root.
///
/// When the root type is absent, nonempty `anyOf`/`oneOf` unions whose branches
/// each declare `type: "object"` receive that root type. Every present union
/// must meet this condition; branches and other keywords are preserved.
/// An explicit object root already excludes non-objects: `type` and root
/// combinators are conjunctive, so a union cannot override that constraint.
///
/// # Errors
///
/// Rejects non-object root types, boolean schemas, and missing object evidence.
/// The schema is unchanged on failure.
pub fn normalize_mcp_output_schema(value: &mut serde_json::Value) -> Result<(), String> {
    let root = value.as_object_mut().ok_or_else(|| {
        "MCP output schema must be a JSON object declaring type object".to_owned()
    })?;
    if let Some(kind) = root.get("type") {
        return if kind == "object" {
            Ok(())
        } else {
            Err("MCP output schema root type must be exactly object".to_owned())
        };
    }
    let mut has_union = false;
    for keyword in ["anyOf", "oneOf"] {
        let Some(branches) = root.get(keyword) else {
            continue;
        };
        has_union = true;
        let branches = branches
            .as_array()
            .filter(|branches| !branches.is_empty())
            .ok_or_else(|| {
                format!("MCP output schema {keyword} must be a nonempty object union")
            })?;
        for (index, branch) in branches.iter().enumerate() {
            if branch.get("type").and_then(serde_json::Value::as_str) != Some("object") {
                return Err(format!(
                    "MCP output schema {keyword} branch {index} must declare type object"
                ));
            }
        }
    }
    if !has_union {
        return Err("MCP output schema must declare type object or an object union".to_owned());
    }
    root.insert("type".to_owned(), serde_json::json!("object"));
    Ok(())
}

/// Phrases a description uses to promise that a parameter's floor is 1.
const CLAIMS_MIN_ONE: &[&str] = &[
    "0 is rejected",
    "at least 1",
    "must be >= 1",
    "Must be >= 1",
];

/// Phrases a description uses to introduce a ceiling, each followed
/// immediately by the number.
///
/// Deliberately a closed list rather than a general parser. Bounds
/// written as free prose cannot be matched reliably. A closed list
/// under-reports rather than over-reports: a missed bound costs one
/// undeclared keyword while a false one costs a suite nobody trusts.
const CEILING_PHRASES: &[&str] = &["1 to ", "at most ", "At most "];

/// The ceiling `prose` promises, or `None` if it states none in a
/// recognised phrasing.
///
/// Only the *number* is read from prose. Which keyword should carry it is
/// decided by the parameter's own JSON type, not by the unit word after
/// the number — `at most 16 tags` and `at most 16` mean the same thing on
/// an array, and guessing from English is the part that goes wrong.
fn claimed_ceiling(prose: &str) -> Option<u64> {
    for phrase in CEILING_PHRASES {
        let Some(at) = prose.find(phrase) else {
            continue;
        };
        let after = &prose[at + phrase.len()..];
        let number = after
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches([')', ',', '.', ';', ':']);
        if let Ok(value) = number.parse::<u64>() {
            return Some(value);
        }
    }
    None
}

/// The schema keyword that carries a ceiling for a parameter of this type:
/// `maxItems` for an array, `maxLength` for a string, `maximum` for a
/// number. `None` when the type is absent or is something else.
fn ceiling_keyword(spec: &serde_json::Value) -> Option<&'static str> {
    // An `Option<T>` emits `type: ["string", "null"]`, so match on
    // membership rather than equality.
    let mentions = |wanted: &str| match spec.get("type") {
        Some(serde_json::Value::String(one)) => one == wanted,
        Some(serde_json::Value::Array(many)) => many.iter().any(|item| item == wanted),
        _ => false,
    };
    if mentions("array") {
        Some("maxItems")
    } else if mentions("string") {
        Some("maxLength")
    } else if mentions("integer") || mentions("number") {
        Some("maximum")
    } else {
        None
    }
}

/// Report parameters whose description promises a bound that the schema does
/// not declare, as `"<tool>.<field>: ..."` strings. Empty means every promise
/// is machine-readable.
///
/// Both ends are checked. A Rust `Option<u32>`/`usize` emits `minimum: 0` from
/// its type and a signed type emits nothing, so a strict JSON-Schema client is
/// told `limit: 0` validates and only learns otherwise from a runtime
/// rejection; `#[schemars(range(min = 1))]` fixes it. Ceilings are worse,
/// because Rust supplies no default at all: nothing in `String` says 240, so a
/// client is told a 30,000-character body validates and pays to send it before
/// being refused. `#[schemars(length(max = 240))]` emits `maxLength` on a
/// string and `maxItems` on a `Vec`.
///
/// In-tree suites run this over the core registry and over `proxima-code`;
/// an out-of-tree flavor can call it on its own frozen registry to get the
/// same guarantee. This deliberately is *not* enforced in `try_freeze` —
/// unlike an undeclared `EFFECT`, which stops a gate from working, a
/// bound stated only in prose is a documentation defect and should not stop
/// an existing deployment from booting.
#[must_use]
pub fn schema_bound_mismatches(registry: &crate::FlavorRegistryFrozen) -> Vec<String> {
    let mut offenders = Vec::new();
    for tool in registry.list_mcp_tools() {
        let Some(properties) = tool
            .args_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for (field, spec) in properties {
            // A shared dispatcher field carries every action's prose inline,
            // so the top-level description is the whole claim. Descriptions
            // wrap, so a claim can straddle a newline.
            let joined = spec
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if CLAIMS_MIN_ONE.iter().any(|claim| joined.contains(claim)) {
                let minimum = spec.get("minimum").and_then(serde_json::Value::as_i64);
                if minimum != Some(1) {
                    offenders.push(format!(
                        "{}.{field}: description promises a minimum of 1, schema says {minimum:?}",
                        tool.name
                    ));
                }
            }
            if let Some(promised) = claimed_ceiling(&joined) {
                match ceiling_keyword(spec) {
                    Some(keyword) => {
                        let declared = spec.get(keyword).and_then(serde_json::Value::as_u64);
                        if declared != Some(promised) {
                            offenders.push(format!(
                                "{}.{field}: description promises a maximum of {promised}, \
                                 schema {keyword} says {declared:?}",
                                tool.name
                            ));
                        }
                    }
                    None => offenders.push(format!(
                        "{}.{field}: description promises a maximum of {promised}, but the \
                         schema declares no type that can carry one",
                        tool.name
                    )),
                }
            }
        }
    }
    offenders
}

/// One action of a tagged-enum dispatcher, derived from its variant.
#[derive(Debug, Clone, PartialEq)]
pub struct McpActionSchema {
    /// The discriminator value that selects this action.
    pub action: String,
    /// What the action does: the variant's doc comment.
    pub description: Option<String>,
    /// The closed schema of this action's own fields: discriminator
    /// removed, root branch fields hoisted as unconstrained properties so
    /// the root names the whole vocabulary.
    pub argument_schema: serde_json::Value,
}

impl McpActionSchema {
    /// The root fields this action accepts, as its schema derives them.
    #[must_use]
    pub fn allowed_fields(&self) -> Vec<String> {
        analyze_root_fields(&self.argument_schema)
            .map(|fields| fields.allowed)
            .unwrap_or_default()
    }

    /// The root fields this action requires, as its schema derives them.
    #[must_use]
    pub fn required_fields(&self) -> Vec<String> {
        analyze_root_fields(&self.argument_schema)
            .map(|fields| fields.required)
            .unwrap_or_default()
    }
}

/// A dispatcher's argument contract, one entry per action in declaration
/// order, derived from an internally tagged `Args` enum.
///
/// The flat wire `inputSchema` is rendered from it ([`Self::render`]):
/// whole at registration, narrowed per caller. Field lists, per-action prose
/// and the action enum therefore cannot disagree with what the caller may
/// run.
#[derive(Debug, Clone, PartialEq)]
pub struct McpDispatcherSchema {
    /// The serde tag (`#[serde(tag = "...")]`). `try_freeze` requires
    /// `action`, the key scope keys and REST routes read.
    pub discriminator: String,
    /// The enum's own doc comment.
    pub description: Option<String>,
    pub actions: Vec<McpActionSchema>,
}

impl McpDispatcherSchema {
    /// The action named `action`.
    #[must_use]
    pub fn action(&self, action: &str) -> Option<&McpActionSchema> {
        self.actions.iter().find(|schema| schema.action == action)
    }

    /// The flat `inputSchema` over the actions `permitted` admits.
    ///
    /// - the discriminator is a string enum of those actions; its
    ///   description gives one line per action: what it does, then its
    ///   required and optional fields;
    /// - every other property is a field of at least one of them. A field
    ///   whose schemas differ only in nullability is widened to admit both;
    ///   one an action leaves unconstrained stays unconstrained;
    /// - a field whose prose differs by action carries each action's prose
    ///   under a "Depends on" heading naming the discriminator, so no client
    ///   has to read anything but the schema.
    ///
    /// Registration refuses a dispatcher whose actions give a field
    /// incompatible schemas; a hand-built schema with such a field renders
    /// it unconstrained.
    #[must_use]
    pub fn render(&self, permitted: impl Fn(&str) -> bool) -> serde_json::Value {
        let actions = self
            .actions
            .iter()
            .filter(|schema| permitted(&schema.action))
            .collect::<Vec<_>>();
        render_dispatcher(self, &actions)
    }
}

/// Derive the dispatcher contract of a schemars root `oneOf` for an
/// internally tagged enum, or `None` when the root is not that shape.
///
/// Anthropic/OpenAI-compatible tool schemas cannot rely on a root-level
/// union, so the wire carries the rendered flat object. Runtime serde
/// validation remains authoritative for per-action required fields.
///
/// # Errors
///
/// An unresolved, non-local or cyclic local reference, or a field two
/// actions give incompatible schemas.
fn derive_dispatcher(value: &serde_json::Value) -> Result<Option<McpDispatcherSchema>, String> {
    let Some(raw_variants) = value
        .as_object()
        .and_then(|map| map.get("oneOf"))
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(None);
    };

    // Resolve every definition first: an unresolved/non-local/cyclic
    // reference must fail registration even when no variant reaches it.
    let defs = local_defs(value);
    for definition in defs.draft.values().chain(defs.legacy.values()) {
        let mut checked_definition = definition.clone();
        inline_variant_refs(&mut checked_definition, &defs)?;
    }
    let mut variants = Vec::with_capacity(raw_variants.len());
    for raw_variant in raw_variants {
        let mut variant = raw_variant.clone();
        inline_variant_refs(&mut variant, &defs)?;
        remove_definition_containers(&mut variant);
        variants.push(variant);
    }

    // The discriminator KEY: the single property present across every
    // variant with a distinct string `const` per variant — for an
    // internally tagged enum, the `#[serde(tag = ...)]` field.
    let Some(discriminator) = detect_discriminator_key(&variants) else {
        return Ok(None);
    };
    let mut actions = Vec::with_capacity(variants.len());
    for variant in &variants {
        // A variant without an object root or a string `const` under the
        // discriminator is not the shape this derives; leave it a union.
        let (Some(action), Some(argument_schema)) = (
            root_const_value(variant, &discriminator),
            normalize_action_argument_schema(variant, &discriminator),
        ) else {
            return Ok(None);
        };
        actions.push(McpActionSchema {
            action,
            description: root_variant_description(variant).map(one_line),
            argument_schema,
        });
    }
    if actions.is_empty() {
        return Ok(None);
    }
    let dispatcher = McpDispatcherSchema {
        discriminator,
        description: value
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        actions,
    };
    // Every subset of mutually compatible actions merges, so checking the
    // whole set once is what makes every narrowed render exact.
    for (name, occurrences) in field_occurrences(&dispatcher.actions.iter().collect::<Vec<_>>()) {
        merge_typed(name, &occurrences)?;
    }
    Ok(Some(dispatcher))
}

/// Field name → every (action, property schema) naming it, in first-seen
/// order so the rendered properties follow declaration order.
type FieldOccurrences<'a> = Vec<(&'a str, Vec<(&'a str, &'a serde_json::Value)>)>;

fn field_occurrences<'a>(actions: &[&'a McpActionSchema]) -> FieldOccurrences<'a> {
    let mut fields: FieldOccurrences<'a> = Vec::new();
    for action in actions {
        let properties = action
            .argument_schema
            .get("properties")
            .and_then(serde_json::Value::as_object);
        for (name, property) in properties.into_iter().flatten() {
            let occurrence = (action.action.as_str(), property);
            match fields.iter_mut().find(|(field, _)| field == name) {
                Some((_, occurrences)) => occurrences.push(occurrence),
                None => fields.push((name.as_str(), vec![occurrence])),
            }
        }
    }
    fields
}

/// The flat object over `actions`.
fn render_dispatcher(
    dispatcher: &McpDispatcherSchema,
    actions: &[&McpActionSchema],
) -> serde_json::Value {
    let discriminator = dispatcher.discriminator.as_str();
    let mut properties = serde_json::Map::new();
    properties.insert(
        discriminator.to_owned(),
        serde_json::json!({
            "type": "string",
            "enum": actions.iter().map(|action| action.action.as_str()).collect::<Vec<_>>(),
            "description": action_guide(actions),
        }),
    );
    for (name, occurrences) in field_occurrences(actions) {
        properties.insert(
            name.to_owned(),
            merge_field(name, discriminator, &occurrences),
        );
    }
    let mut root = serde_json::Map::new();
    root.insert("type".to_owned(), serde_json::json!("object"));
    if let Some(description) = &dispatcher.description {
        root.insert("description".to_owned(), serde_json::json!(description));
    }
    root.insert(
        "properties".to_owned(),
        serde_json::Value::Object(properties),
    );
    root.insert("required".to_owned(), serde_json::json!([discriminator]));
    root.insert("additionalProperties".to_owned(), serde_json::json!(false));
    serde_json::Value::Object(root)
}

/// The discriminator's description: one line per action — what it does,
/// then its required and optional fields. Standard clients render a
/// property's description and nothing else, so the per-action contract
/// lives here.
fn action_guide(actions: &[&McpActionSchema]) -> String {
    use std::fmt::Write as _;
    let mut guide =
        String::from("The action to run. Each action takes only the fields listed for it.");
    for action in actions {
        let (allowed, required) = (action.allowed_fields(), action.required_fields());
        let optional = allowed
            .iter()
            .filter(|field| !required.contains(field))
            .map(String::as_str)
            .collect::<Vec<_>>();
        write!(guide, "\n- {}:", action.action).expect("write to String is infallible");
        if let Some(description) = &action.description {
            write!(guide, " {}", sentence(description)).expect("write to String is infallible");
        }
        if !required.is_empty() {
            write!(guide, " Required: {}.", required.join(", "))
                .expect("write to String is infallible");
        }
        if !optional.is_empty() {
            write!(guide, " Optional: {}.", optional.join(", "))
                .expect("write to String is infallible");
        }
        if allowed.is_empty() {
            guide.push_str(" No fields.");
        }
    }
    guide
}

/// One field as the flat object advertises it, from every action naming it.
///
/// Identical occurrences are the field as declared. Otherwise the schemas
/// merge — nullability widens, an unconstrained occurrence (a hoisted
/// branch field) leaves the field unconstrained — `title` goes, `default`
/// stays only when every action agrees, and the prose is
/// [`shared_description`].
fn merge_field(
    name: &str,
    discriminator: &str,
    occurrences: &[(&str, &serde_json::Value)],
) -> serde_json::Value {
    let first = occurrences[0].1;
    if occurrences.iter().all(|(_, schema)| *schema == first) {
        return first.clone();
    }
    let unconstrained = occurrences
        .iter()
        .any(|(_, schema)| is_unconstrained(schema));
    let mut field = match merge_typed(name, occurrences) {
        Ok(Some(merged)) if !unconstrained => merged,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    if let Some(map) = field.as_object_mut() {
        map.remove("title");
        let default = first.get("default");
        match default.filter(|_| {
            occurrences
                .iter()
                .all(|(_, schema)| schema.get("default") == default)
        }) {
            Some(default) => map.insert("default".to_owned(), default.clone()),
            None => map.remove("default"),
        };
        match shared_description(discriminator, occurrences) {
            Some(description) => map.insert("description".to_owned(), description.into()),
            None => map.remove("description"),
        };
    }
    field
}

/// A schema that admits every value: `{}`/`true` up to annotations — what
/// hoisting a branch-only field leaves at an action's root.
fn is_unconstrained(schema: &serde_json::Value) -> bool {
    let shape = validation_shape(schema);
    shape == serde_json::json!({}) || shape == serde_json::Value::Bool(true)
}

/// The constrained occurrences of one field merged by widening
/// nullability; `None` when every occurrence is unconstrained.
///
/// # Errors
///
/// Two constrained occurrences whose validation differs beyond nullability:
/// no flat property describes both.
fn merge_typed(
    name: &str,
    occurrences: &[(&str, &serde_json::Value)],
) -> Result<Option<serde_json::Value>, String> {
    let mut merged: Option<serde_json::Value> = None;
    for (action, schema) in occurrences {
        if is_unconstrained(schema) {
            continue;
        }
        merged = Some(match merged {
            None => (*schema).clone(),
            Some(current) => nullable_compatible_schema(&current, schema).ok_or_else(|| {
                format!(
                    "conflicting property `{name}` while flattening action `{action}`: \
                     {current:#} vs {schema:#}"
                )
            })?,
        });
    }
    Ok(merged)
}

/// One description when every action describing the field agrees; else
/// each distinct text under the actions it belongs to.
fn shared_description(
    discriminator: &str,
    occurrences: &[(&str, &serde_json::Value)],
) -> Option<String> {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for (action, schema) in occurrences {
        let Some(description) = schema
            .get("description")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        match groups.iter_mut().find(|(text, _)| *text == description) {
            Some((_, actions)) => actions.push(action),
            None => groups.push((description, vec![action])),
        }
    }
    match groups.as_slice() {
        [] => None,
        [(description, _)] => Some((*description).to_owned()),
        _ => {
            Some(
                std::iter::once(format!("Depends on `{discriminator}`:"))
                    .chain(groups.iter().map(|(text, actions)| {
                        format!("- {}: {}", actions.join(", "), one_line(text))
                    }))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    }
}

/// `text` on one line: a doc comment's wrapped paragraphs joined by spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` on one line, ending as a sentence.
fn sentence(text: &str) -> String {
    let mut line = one_line(text);
    if !line.ends_with(['.', '!', '?']) {
        line.push('.');
    }
    line
}

/// The root fields of one action schema. This is deliberately a root-only
/// analysis: a nested object's fields are that object's contract, not fields
/// accepted beside the dispatcher's discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootFieldAnalysis {
    pub(crate) allowed: Vec<String>,
    pub(crate) required: Vec<String>,
}

/// Analyse the field vocabulary of an action schema, including root
/// combinator branches. The same implementation feeds the derived metadata
/// and the registry freeze check; keeping the walk here prevents authorization
/// from silently drifting away from the advertised schema.
pub(crate) fn analyze_root_fields(schema: &serde_json::Value) -> Result<RootFieldAnalysis, String> {
    let (allowed, required) = analyze_root_node(schema, true)?;
    if required
        .iter()
        .any(|field| !allowed.iter().any(|name| name == field))
    {
        return Err(format!(
            "required field set {required:?} is not a subset of allowed fields {allowed:?}"
        ));
    }
    Ok(RootFieldAnalysis { allowed, required })
}

/// Check the structural promises made by a normalized action schema. Branches
/// may contribute root properties only when those names are also hoisted into
/// the root; otherwise the flat schema and the preserved conditional subtree
/// describe different call surfaces.
pub(crate) fn validate_closed_root_schema(schema: &serde_json::Value) -> Result<(), String> {
    let map = schema
        .as_object()
        .ok_or_else(|| "action_schema must be a JSON object".to_string())?;
    if map.get("type") != Some(&serde_json::Value::String("object".to_string())) {
        return Err("action_schema root must declare type object".to_string());
    }
    if map.get("additionalProperties") != Some(&serde_json::Value::Bool(false)) {
        return Err("action_schema root additionalProperties must be false".to_string());
    }
    let properties = map
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "action_schema root properties must be an object".to_string())?;
    let names = properties.keys().collect::<std::collections::BTreeSet<_>>();
    if names
        .iter()
        .any(|name| name.is_empty() || **name == "action")
    {
        return Err("action_schema root has an empty or action property".to_string());
    }
    validate_root_branches_against_properties(schema, &names)
}

fn validate_root_branches_against_properties(
    schema: &serde_json::Value,
    root_names: &std::collections::BTreeSet<&String>,
) -> Result<(), String> {
    let map = schema
        .as_object()
        .ok_or_else(|| "action_schema branch must be a JSON object".to_string())?;
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = map.get(keyword) {
            let branches = branches
                .as_array()
                .ok_or_else(|| format!("action_schema {keyword} must be an array"))?;
            for branch in branches {
                validate_one_root_branch(branch, keyword, root_names)?;
            }
        }
    }
    for keyword in ["if", "then", "else"] {
        if let Some(branch) = map.get(keyword) {
            validate_one_root_branch(branch, keyword, root_names)?;
        }
    }
    Ok(())
}

fn validate_one_root_branch(
    branch: &serde_json::Value,
    keyword: &str,
    root_names: &std::collections::BTreeSet<&String>,
) -> Result<(), String> {
    let Some(map) = branch.as_object() else {
        return branch
            .is_boolean()
            .then_some(())
            .ok_or_else(|| format!("action_schema {keyword} branch must be an object"));
    };
    let contributes_fields = root_node_has_fields(branch)?;
    if contributes_fields && !schema_allows_object(map.get("type")) {
        return Err(format!(
            "action_schema {keyword} branch has an explicit non-object type with root fields"
        ));
    }
    if let Some(additional) = map.get("additionalProperties")
        && additional != &serde_json::Value::Bool(false)
    {
        return Err(format!(
            "action_schema {keyword} branch additionalProperties must be false"
        ));
    }
    if let Some(properties) = map.get("properties") {
        let properties = properties.as_object().ok_or_else(|| {
            format!("action_schema {keyword} branch properties must be an object")
        })?;
        for name in properties.keys() {
            if name.is_empty() || name == "action" {
                return Err(format!(
                    "action_schema {keyword} branch has an empty or action property"
                ));
            }
            if !root_names.contains(name) {
                return Err(format!(
                    "action_schema {keyword} branch property {name} is not hoisted at root"
                ));
            }
        }
    }
    for name in parse_required(map.get("required"), true)? {
        if !root_names.iter().any(|root| *root == &name) {
            return Err(format!(
                "action_schema {keyword} branch required field {name} is not hoisted at root"
            ));
        }
    }
    validate_root_branches_against_properties(branch, root_names)
}

fn analyze_root_node(
    schema: &serde_json::Value,
    reject_action: bool,
) -> Result<(Vec<String>, Vec<String>), String> {
    let Some(map) = schema.as_object() else {
        return schema
            .is_boolean()
            .then_some((Vec::new(), Vec::new()))
            .ok_or_else(|| "schema root/branch must be a JSON object".to_string());
    };

    let mut allowed = Vec::new();
    if let Some(properties) = map.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| "schema properties must be an object".to_string())?;
        for name in properties.keys() {
            if name.is_empty() {
                return Err("schema property names must not be empty".to_string());
            }
            if reject_action && name == "action" {
                return Err("action discriminator must not appear in an action schema".to_string());
            }
            push_unique(&mut allowed, name);
        }
    }
    let mut required = parse_required(map.get("required"), reject_action)?;

    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = map.get(keyword) {
            let branches = branches
                .as_array()
                .ok_or_else(|| format!("schema {keyword} must be an array"))?;
            if branches.is_empty() {
                return Err(format!("schema {keyword} must not be empty"));
            }
            let mut branch_analysis = Vec::with_capacity(branches.len());
            for branch in branches {
                validate_root_branch_closed(branch, keyword)?;
                branch_analysis.push(analyze_root_node(branch, reject_action)?);
            }
            for (branch_allowed, _) in &branch_analysis {
                for field in branch_allowed {
                    push_unique(&mut allowed, field);
                }
            }
            let common = branch_analysis
                .first()
                .map(|(_, fields)| fields.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|field| {
                    branch_analysis
                        .iter()
                        .all(|(_, fields)| fields.iter().any(|name| name == field))
                })
                .collect::<Vec<_>>();
            for field in common {
                push_unique(&mut required, &field);
            }
        }
    }

    if let Some(branches) = map.get("allOf") {
        let branches = branches
            .as_array()
            .ok_or_else(|| "schema allOf must be an array".to_string())?;
        if branches.is_empty() {
            return Err("schema allOf must not be empty".to_string());
        }
        for branch in branches {
            validate_root_branch_closed(branch, "allOf")?;
            let (branch_allowed, branch_required) = analyze_root_node(branch, reject_action)?;
            for field in branch_allowed {
                push_unique(&mut allowed, &field);
            }
            for field in branch_required {
                push_unique(&mut required, &field);
            }
        }
    }

    // Conditions add possible root fields, but their requirements remain
    // conditional and therefore cannot become flat authorization fields.
    for keyword in ["if", "then", "else"] {
        if let Some(branch) = map.get(keyword) {
            validate_root_branch_closed(branch, keyword)?;
            let (branch_allowed, _) = analyze_root_node(branch, reject_action)?;
            for field in branch_allowed {
                push_unique(&mut allowed, &field);
            }
        }
    }

    if (!allowed.is_empty() || !required.is_empty()) && !schema_allows_object(map.get("type")) {
        return Err(
            "schema root/branch has an explicit non-object type with root fields".to_string(),
        );
    }
    Ok((allowed, required))
}

fn parse_required(
    required: Option<&serde_json::Value>,
    reject_action: bool,
) -> Result<Vec<String>, String> {
    let Some(required) = required else {
        return Ok(Vec::new());
    };
    let items = required
        .as_array()
        .ok_or_else(|| "schema required must be an array".to_string())?;
    let mut fields = Vec::with_capacity(items.len());
    for item in items {
        let field = item
            .as_str()
            .ok_or_else(|| "schema required entries must be strings".to_string())?;
        if field.is_empty() {
            return Err("schema required names must not be empty".to_string());
        }
        if reject_action && field == "action" {
            return Err(
                "action discriminator must not be required by an action schema".to_string(),
            );
        }
        if fields.iter().any(|name| name == field) {
            return Err(format!("schema required contains duplicate field {field}"));
        }
        fields.push(field.to_string());
    }
    Ok(fields)
}

fn push_unique(fields: &mut Vec<String>, field: &str) {
    if !fields.iter().any(|name| name == field) {
        fields.push(field.to_string());
    }
}

fn validate_root_branch_closed(branch: &serde_json::Value, keyword: &str) -> Result<(), String> {
    let Some(map) = branch.as_object() else {
        return branch
            .is_boolean()
            .then_some(())
            .ok_or_else(|| format!("schema {keyword} branch must be an object schema"));
    };
    let contributes_fields = root_node_has_fields(branch)?;
    if contributes_fields && !schema_allows_object(map.get("type")) {
        return Err(format!(
            "schema {keyword} branch has an explicit non-object type with root fields"
        ));
    }
    if let Some(additional) = map.get("additionalProperties")
        && additional != &serde_json::Value::Bool(false)
    {
        return Err(format!(
            "schema {keyword} branch additionalProperties must be false, found {additional}"
        ));
    }
    Ok(())
}

fn schema_allows_object(schema_type: Option<&serde_json::Value>) -> bool {
    match schema_type {
        Some(serde_json::Value::String(kind)) => kind == "object",
        Some(serde_json::Value::Array(kinds)) => kinds.iter().any(|kind| kind == "object"),
        // Object applicators are conditional on the instance type in JSON
        // Schema. The normalized action root already fixes the instance to an
        // object, so an applicator branch may omit `type`; only an explicit
        // incompatible type is contradictory.
        None => true,
        _ => false,
    }
}

/// Report whether a schema node contributes fields at its own root. This
/// deliberately follows only root applicators; property schemas are nested
/// contracts and never affect the dispatcher's flat field vocabulary.
fn root_node_has_fields(schema: &serde_json::Value) -> Result<bool, String> {
    let Some(map) = schema.as_object() else {
        return Ok(false);
    };
    let direct = if let Some(properties) = map.get("properties") {
        !properties
            .as_object()
            .ok_or_else(|| "schema properties must be an object".to_string())?
            .is_empty()
    } else {
        false
    };
    let required = !parse_required(map.get("required"), true)?.is_empty();
    let mut contributes = direct || required;
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = map.get(keyword) {
            let branches = branches
                .as_array()
                .ok_or_else(|| format!("schema {keyword} must be an array"))?;
            for branch in branches {
                contributes |= root_node_has_fields(branch)?;
            }
        }
    }
    for keyword in ["if", "then", "else"] {
        if let Some(branch) = map.get(keyword) {
            contributes |= root_node_has_fields(branch)?;
        }
    }
    Ok(contributes)
}

/// Normalize one raw tagged-enum variant into its action's own argument
/// schema. The copied variant's local refs are expanded first; this pass
/// then removes only the direct discriminator and performs the root
/// closure/field hoist. Nested property schemas remain otherwise untouched.
fn normalize_action_argument_schema(
    variant: &serde_json::Value,
    discriminator: &str,
) -> Option<serde_json::Value> {
    let mut schema = variant.clone();
    {
        let map = schema.as_object_mut()?;
        let variant_type_is_object =
            map.get("type") == Some(&serde_json::Value::String("object".to_string()));
        if !variant_type_is_object {
            return None;
        }
        if let Some(properties) = map
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
        {
            properties.remove(discriminator);
        }
        remove_root_discriminator(map, discriminator);
        let mut required = map.remove("required");
        if let Some(items) = required.as_mut().and_then(serde_json::Value::as_array_mut) {
            items.retain(|field| field.as_str() != Some(discriminator));
            if items.is_empty() {
                required = None;
            }
        }
        if let Some(required) = required {
            map.insert("required".to_string(), required);
        }

        // A variant's root is always closed after the discriminator is removed.
        // Explicit reopening is a schema-generation error, not a reason to weaken
        // the flat dispatcher precheck.
        if let Some(additional) = map.get("additionalProperties")
            && additional != &serde_json::Value::Bool(false)
        {
            return None;
        }
        map.insert(
            "type".to_string(),
            serde_json::Value::String("object".to_string()),
        );
        map.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(false),
        );
    }

    let (hoisted, _) = analyze_root_node(&schema, true).ok()?;
    let mut merged = serde_json::Map::new();
    if let Some(root_properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    {
        for (name, property) in root_properties {
            merged.insert(name.clone(), property.clone());
        }
    }
    // analyze_root_node sees branch fields but deliberately never walks
    // nested objects. Pull only those root branch properties into the flat
    // object; each branch remains in place for validation.
    for name in hoisted {
        if merged.contains_key(&name) {
            continue;
        }
        // Branch-only constraints remain in their conditional/combinator
        // branch. A neutral root property makes the flat object vocabulary
        // closed without accidentally requiring a branch's value shape on
        // every action input.
        merged.insert(name, serde_json::Value::Object(serde_json::Map::new()));
    }
    schema
        .as_object_mut()?
        .insert("properties".to_string(), serde_json::Value::Object(merged));
    let fields = analyze_root_fields(&schema).ok()?;
    let required = fields
        .required
        .iter()
        .map(|field| serde_json::Value::String(field.clone()))
        .collect::<Vec<_>>();
    let map = schema.as_object_mut()?;
    if required.is_empty() {
        map.remove("required");
    } else {
        map.insert("required".to_string(), serde_json::Value::Array(required));
    }
    validate_closed_root_schema(&schema).ok()?;
    Some(schema)
}

fn root_const_value(schema: &serde_json::Value, discriminator: &str) -> Option<String> {
    let mut values = Vec::new();
    collect_root_consts(schema, &mut values)?;
    let mut found = None;
    for (name, value) in values {
        if name != discriminator {
            continue;
        }
        if found.as_deref().is_some_and(|previous| previous != value) {
            return None;
        }
        found = Some(value);
    }
    found
}

fn root_variant_description(schema: &serde_json::Value) -> Option<&str> {
    schema
        .as_object()?
        .get("description")
        .and_then(serde_json::Value::as_str)
}

fn remove_root_discriminator(map: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = map
            .get_mut(keyword)
            .and_then(serde_json::Value::as_array_mut)
        {
            for branch in branches {
                if let Some(branch_map) = branch.as_object_mut() {
                    if let Some(properties) = branch_map
                        .get_mut("properties")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        properties.remove(key);
                    }
                    if let Some(required) = branch_map
                        .get_mut("required")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        required.retain(|field| field.as_str() != Some(key));
                        if required.is_empty() {
                            branch_map.remove("required");
                        }
                    }
                    remove_root_discriminator(branch_map, key);
                }
            }
        }
    }
    for keyword in ["if", "then", "else"] {
        if let Some(branch_map) = map
            .get_mut(keyword)
            .and_then(serde_json::Value::as_object_mut)
        {
            if let Some(properties) = branch_map
                .get_mut("properties")
                .and_then(serde_json::Value::as_object_mut)
            {
                properties.remove(key);
            }
            if let Some(required) = branch_map
                .get_mut("required")
                .and_then(serde_json::Value::as_array_mut)
            {
                required.retain(|field| field.as_str() != Some(key));
                if required.is_empty() {
                    branch_map.remove("required");
                }
            }
            remove_root_discriminator(branch_map, key);
        }
    }
}

#[derive(Default)]
struct LocalDefs {
    draft: serde_json::Map<String, serde_json::Value>,
    legacy: serde_json::Map<String, serde_json::Value>,
}

// Draft 2020-12 keywords whose values contain subschemas. Keeping this
// vocabulary shared prevents ref expansion, definition cleanup, and freeze
// detection from disagreeing about which JSON objects are schemas rather
// than literal instance or annotation data.
const SUBSCHEMA_ARRAY_KEYWORDS: &[&str] = &["oneOf", "anyOf", "allOf", "prefixItems"];
const SUBSCHEMA_SINGLE_KEYWORDS: &[&str] = &[
    "if",
    "then",
    "else",
    "not",
    "items",
    "contains",
    "additionalItems",
    "additionalProperties",
    "propertyNames",
    "unevaluatedProperties",
    "unevaluatedItems",
    "contentSchema",
];
const SUBSCHEMA_MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "$defs",
    "definitions",
];

fn for_each_subschema_mut(
    map: &mut serde_json::Map<String, serde_json::Value>,
    visit: &mut impl FnMut(&mut serde_json::Value),
) {
    let _ = for_each_subschema_result(map, &mut |child| {
        visit(child);
        Ok(())
    });
}

fn for_each_subschema_result(
    map: &mut serde_json::Map<String, serde_json::Value>,
    visit: &mut impl FnMut(&mut serde_json::Value) -> Result<(), String>,
) -> Result<(), String> {
    for &key in SUBSCHEMA_ARRAY_KEYWORDS {
        if let Some(items) = map.get_mut(key).and_then(serde_json::Value::as_array_mut) {
            for item in items {
                visit(item)?;
            }
        }
    }
    for &key in SUBSCHEMA_SINGLE_KEYWORDS {
        if let Some(child) = map.get_mut(key) {
            if key == "items"
                && let Some(items) = child.as_array_mut()
            {
                // Legacy tuple validation used an array here; draft 2020-12
                // uses `prefixItems`, but walking both costs nothing.
                for item in items {
                    visit(item)?;
                }
            } else {
                visit(child)?;
            }
        }
    }
    for &key in SUBSCHEMA_MAP_KEYWORDS {
        if let Some(children) = map.get_mut(key).and_then(serde_json::Value::as_object_mut) {
            for child in children.values_mut() {
                visit(child)?;
            }
        }
    }
    Ok(())
}

fn local_defs(value: &serde_json::Value) -> LocalDefs {
    let mut defs = LocalDefs::default();
    if let Some(entries) = value.get("$defs").and_then(serde_json::Value::as_object) {
        defs.draft.clone_from(entries);
    }
    if let Some(entries) = value
        .get("definitions")
        .and_then(serde_json::Value::as_object)
    {
        defs.legacy.clone_from(entries);
    }
    defs
}

/// Inline only a copied dispatcher variant against the generated root's local
/// definitions. `$ref` siblings are a conjunction in draft 2020-12, so keep
/// them as `allOf` rather than letting a sibling overwrite the referenced
/// target. Every target and sibling is recursively resolved before the copy is
/// returned; unresolved, non-local and cyclic references are registration
/// errors.
fn inline_variant_refs(value: &mut serde_json::Value, defs: &LocalDefs) -> Result<(), String> {
    inline_value(value, defs, &mut Vec::new())
}

fn inline_value(
    value: &mut serde_json::Value,
    defs: &LocalDefs,
    stack: &mut Vec<String>,
) -> Result<(), String> {
    if let serde_json::Value::Array(items) = value {
        for item in items {
            inline_value(item, defs, stack)?;
        }
        return Ok(());
    }
    let Some(map) = value.as_object_mut() else {
        return Ok(());
    };
    if map.contains_key("$ref") {
        let reference = map
            .get("$ref")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "$ref must be a string".to_string())?
            .to_string();
        let target = if let Some(name) = reference.strip_prefix("#/$defs/") {
            defs.draft
                .get(name)
                .ok_or_else(|| format!("unresolved local reference {reference}"))?
        } else if let Some(name) = reference.strip_prefix("#/definitions/") {
            defs.legacy
                .get(name)
                .ok_or_else(|| format!("unresolved local reference {reference}"))?
        } else {
            return Err(format!("non-local reference {reference}"));
        };
        if stack.iter().any(|seen| seen == &reference) {
            return Err(format!("cyclic local reference {reference}"));
        }

        let siblings = map
            .iter()
            .filter(|(key, _)| key.as_str() != "$ref")
            .map(|(key, child)| (key.clone(), child.clone()))
            .collect::<serde_json::Map<_, _>>();
        let mut replacement = target.clone();
        stack.push(reference);
        inline_value(&mut replacement, defs, stack)?;
        stack.pop();
        let mut sibling_schema = serde_json::Value::Object(siblings);
        inline_value(&mut sibling_schema, defs, stack)?;

        let resolved_object = replacement.get("type") == Some(&serde_json::json!("object"));
        let has_siblings = !sibling_schema
            .as_object()
            .is_some_and(serde_json::Map::is_empty);
        let description = sibling_schema
            .get("description")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                replacement
                    .get("description")
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_string);
        if has_siblings {
            *value = serde_json::json!({
                "allOf": [replacement, sibling_schema]
            });
            // Preserve an explicitly object-shaped resolved root so a
            // `$ref` arm with annotation/validation siblings remains a valid
            // dispatcher variant without fabricating an untyped root.
            if resolved_object {
                value
                    .as_object_mut()
                    .expect("composed reference schema is an object")
                    .insert(
                        "type".to_string(),
                        serde_json::Value::String("object".to_string()),
                    );
            }
            if let Some(description) = description {
                value
                    .as_object_mut()
                    .expect("composed reference schema is an object")
                    .insert(
                        "description".to_string(),
                        serde_json::Value::String(description),
                    );
            }
        } else {
            *value = replacement;
        }
        return Ok(());
    }
    for_each_subschema_result(map, &mut |child| inline_value(child, defs, stack))
}

fn remove_definition_containers(value: &mut serde_json::Value) {
    remove_definition_containers_from_schema(value);
}

fn remove_definition_containers_from_schema(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    map.remove("$defs");
    map.remove("definitions");
    for_each_subschema_mut(map, &mut remove_definition_containers_from_schema);
}

/// Top-level property names in `args_schema` that carry no non-empty
/// `description`. Used by tool registration to warn (never fail) on
/// under-documented MCP tool fields.
pub(crate) fn undescribed_property_names(args_schema: &serde_json::Value) -> Vec<String> {
    let Some(properties) = args_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let mut out = properties
        .iter()
        .filter(|(_, schema)| {
            schema
                .get("description")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|description| description.trim().is_empty())
        })
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    out.sort();
    out
}

/// Detect the internally-tagged discriminator KEY across a root `oneOf`'s
/// variants.
///
/// An internally-tagged enum (`#[serde(tag = "k")]`) emits one object variant
/// per case, each carrying the tag field as a string `const` whose value is the
/// (renamed) variant name. The discriminator is the property name that, across
/// *all* variants, is present with a string `const` and takes a *distinct*
/// value per variant. Returns `Some(key)` iff exactly one such key exists;
/// otherwise `None`, signalling the caller to leave the schema unflattened.
fn detect_discriminator_key(variants: &[serde_json::Value]) -> Option<String> {
    use std::collections::{BTreeMap, BTreeSet};

    // For each candidate property key, collect one unambiguous string `const`
    // value from every variant. A key carrying two different root constants
    // in one variant is not a discriminator candidate.
    let mut const_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for variant in variants {
        let mut root_consts = Vec::new();
        collect_root_consts(variant, &mut root_consts)?;
        let mut unique_consts = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        for (name, value) in root_consts {
            if ambiguous.contains(&name) {
                continue;
            }
            if let Some(previous) = unique_consts.get(&name) {
                if previous != &value {
                    unique_consts.remove(&name);
                    ambiguous.insert(name);
                }
            } else {
                unique_consts.insert(name, value);
            }
        }
        if !ambiguous.is_empty() {
            return None;
        }
        for (name, value) in unique_consts {
            const_values.entry(name).or_default().push(value);
        }
    }

    let mut candidates = const_values.into_iter().filter(|(_, values)| {
        // Present in every variant, with a distinct value in each.
        values.len() == variants.len()
            && values.iter().collect::<BTreeSet<_>>().len() == values.len()
    });
    let key = candidates.next()?.0;
    if candidates.next().is_some() {
        // Ambiguous: more than one property qualifies as a discriminator.
        return None;
    }
    Some(key)
}

fn collect_root_consts(schema: &serde_json::Value, out: &mut Vec<(String, String)>) -> Option<()> {
    let Some(map) = schema.as_object() else {
        return schema.is_boolean().then_some(());
    };
    if let Some(properties) = map.get("properties").and_then(serde_json::Value::as_object) {
        for (name, property_schema) in properties {
            if let Some(value) = property_schema
                .get("const")
                .and_then(serde_json::Value::as_str)
            {
                out.push((name.clone(), value.to_string()));
            }
        }
    }
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = map.get(keyword).and_then(serde_json::Value::as_array) {
            for branch in branches {
                collect_root_consts(branch, out)?;
            }
        }
    }
    for keyword in ["if", "then", "else"] {
        if let Some(branch) = map.get(keyword) {
            collect_root_consts(branch, out)?;
        }
    }
    Some(())
}

/// Merge two shared root property schemas when their only validation
/// difference is nullability. The result is canonical and therefore does not
/// depend on which action's schema was encountered first. A schema pair with a
/// different validation shape is not widened here; the existing conflict
/// error remains the honest result for incompatible action contracts.
fn nullable_compatible_schema(
    first: &serde_json::Value,
    second: &serde_json::Value,
) -> Option<serde_json::Value> {
    if validation_shape(first) != validation_shape(second) {
        return None;
    }
    let mut widened = first.clone();
    merge_nullable_nodes(&mut widened, second);
    Some(widened)
}

fn merge_nullable_nodes(first: &mut serde_json::Value, second: &serde_json::Value) {
    let (Some(first_map), Some(second_map)) = (first.as_object_mut(), second.as_object()) else {
        return;
    };
    if let (Some(first_type), Some(second_type)) = (first_map.get("type"), second_map.get("type"))
        && let Some(type_union) = nullable_type_union(first_type, second_type)
    {
        first_map.insert("type".to_string(), type_union);
    }

    for &key in SUBSCHEMA_ARRAY_KEYWORDS {
        let Some(first_items) = first_map
            .get_mut(key)
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        let Some(second_items) = second_map.get(key).and_then(serde_json::Value::as_array) else {
            continue;
        };
        for (first_item, second_item) in first_items.iter_mut().zip(second_items) {
            merge_nullable_nodes(first_item, second_item);
        }
    }
    for &key in SUBSCHEMA_SINGLE_KEYWORDS {
        let Some(first_child) = first_map.get_mut(key) else {
            continue;
        };
        let Some(second_child) = second_map.get(key) else {
            continue;
        };
        if key == "items"
            && let (Some(first_items), Some(second_items)) =
                (first_child.as_array_mut(), second_child.as_array())
        {
            for (first_item, second_item) in first_items.iter_mut().zip(second_items) {
                merge_nullable_nodes(first_item, second_item);
            }
        } else {
            merge_nullable_nodes(first_child, second_child);
        }
    }
    for &key in SUBSCHEMA_MAP_KEYWORDS {
        let Some(first_children) = first_map
            .get_mut(key)
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        let Some(second_children) = second_map.get(key).and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for (name, first_child) in first_children {
            if let Some(second_child) = second_children.get(name) {
                merge_nullable_nodes(first_child, second_child);
            }
        }
    }
}

fn nullable_type_union(
    first: &serde_json::Value,
    second: &serde_json::Value,
) -> Option<serde_json::Value> {
    let mut kinds = std::collections::BTreeSet::new();
    for value in [first, second] {
        match value {
            serde_json::Value::String(kind) => {
                kinds.insert(kind.clone());
            }
            serde_json::Value::Array(many) => {
                for kind in many {
                    kinds.insert(kind.as_str()?.to_string());
                }
            }
            _ => return None,
        }
    }
    if kinds.len() == 1 {
        return kinds.into_iter().next().map(serde_json::Value::String);
    }
    let null = kinds.remove("null");
    let mut values = kinds
        .into_iter()
        .map(serde_json::Value::String)
        .collect::<Vec<_>>();
    if null {
        values.push(serde_json::Value::String("null".to_string()));
    }
    Some(serde_json::Value::Array(values))
}

fn validation_shape(value: &serde_json::Value) -> serde_json::Value {
    let mut value = value.clone();
    strip_non_validation_fields(&mut value);
    normalize_nullable_types(&mut value);
    value
}

fn strip_non_validation_fields(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    for key in ["description", "title", "default"] {
        map.remove(key);
    }
    for_each_subschema_mut(map, &mut strip_non_validation_fields);
}

fn normalize_nullable_types(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if let Some(type_value) = map.get_mut("type")
        && let serde_json::Value::Array(types) = type_value
    {
        types.retain(|item| item != "null");
        if types.len() == 1 {
            *type_value = types[0].clone();
        }
    }
    for_each_subschema_mut(map, &mut normalize_nullable_types);
}

/// Ensure the generated schema is acceptable as an MCP `inputSchema` root.
///
/// MCP clients such as Pi require every tool input schema to declare
/// `type: "object"` and a root `properties` object. Provider-compatible
/// tool schemas must also avoid root combinators.
fn ensure_client_safe_root<T: JsonSchema>(value: &mut serde_json::Value) {
    let serde_json::Value::Object(map) = value else {
        panic!(
            "MCP tool type `{}` root schema must be an object schema document",
            std::any::type_name::<T>(),
        );
    };
    for keyword in ["oneOf", "anyOf", "allOf"] {
        assert!(
            !map.contains_key(keyword),
            "MCP tool type `{}` leaves root schema combinator `{keyword}` after normalization. A dispatcher Args type must be an internally tagged enum (`#[serde(tag = \"...\")]`) whose variants each carry the tag as a distinct string `const`; adjacently/externally tagged enums, untagged enums, or otherwise heterogeneous variants cannot be made client-safe. Use such an enum or a plain struct Args type.",
            std::any::type_name::<T>(),
        );
    }
    if let Some(root_type) = map.get("type") {
        assert_eq!(
            root_type,
            "object",
            "MCP tool type `{}` root schema type must be `object`, got {root_type:#}",
            std::any::type_name::<T>(),
        );
    } else {
        map.insert(
            "type".to_string(),
            serde_json::Value::String("object".to_string()),
        );
    }
    map.entry("properties".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
}

/// True if `value` contains a `$ref` keyword in a JSON-Schema node.
///
/// Schema maps such as `properties` are not themselves schema nodes: a user
/// may legitimately name a property `$ref`, `$defs` or `definitions`. Walking
/// only subschema-valued keywords keeps those instance names distinct from
/// actual schema keywords while still finding refs in local definitions.
pub(crate) fn schema_contains_ref(value: &serde_json::Value) -> bool {
    schema_contains_keyword(value, "$ref")
}

pub(crate) fn schema_contains_defs(value: &serde_json::Value) -> bool {
    schema_contains_keyword(value, "$defs") || schema_contains_keyword(value, "definitions")
}

fn schema_contains_keyword(value: &serde_json::Value, keyword: &str) -> bool {
    let Some(map) = value.as_object() else {
        return value.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| schema_contains_keyword(item, keyword))
        });
    };
    if map.contains_key(keyword) {
        return true;
    }
    for &key in SUBSCHEMA_ARRAY_KEYWORDS {
        if let Some(items) = map.get(key).and_then(serde_json::Value::as_array)
            && items
                .iter()
                .any(|item| schema_contains_keyword(item, keyword))
        {
            return true;
        }
    }
    for &key in SUBSCHEMA_SINGLE_KEYWORDS {
        if let Some(child) = map.get(key)
            && schema_contains_keyword(child, keyword)
        {
            return true;
        }
    }
    for &key in SUBSCHEMA_MAP_KEYWORDS {
        if let Some(children) = map.get(key).and_then(serde_json::Value::as_object)
            && children
                .values()
                .any(|child| schema_contains_keyword(child, keyword))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::Deserialize;

    #[test]
    fn output_object_unions_gain_a_root_type_without_changing_branches() {
        for keyword in ["anyOf", "oneOf"] {
            let branches = serde_json::json!([
                { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] },
                { "type": "object", "properties": { "count": { "type": "integer" } }, "required": ["count"] }
            ]);
            let mut schema = serde_json::json!({ "title": "Output union", keyword: branches });
            normalize_mcp_output_schema(&mut schema).unwrap();
            assert_eq!(schema["type"], "object");
            assert_eq!(schema[keyword], branches);
            assert_eq!(schema["title"], "Output union");
            assert!(schema.get("properties").is_none());
        }
    }

    #[test]
    fn output_normalization_requires_every_present_union_to_be_objects() {
        let object_branch = serde_json::json!([{ "type": "object" }]);
        let mut valid = serde_json::json!({ "anyOf": object_branch, "oneOf": object_branch });
        normalize_mcp_output_schema(&mut valid).unwrap();
        assert_eq!(valid["type"], "object");
        for mut invalid in [
            serde_json::json!(true),
            serde_json::json!(false),
            serde_json::json!({}),
            serde_json::json!({ "properties": { "value": { "type": "string" } } }),
            serde_json::json!({ "type": ["object"] }),
            serde_json::json!({ "type": "null" }),
            serde_json::json!({ "type": "array" }),
            serde_json::json!({ "type": "string", "anyOf": object_branch }),
            serde_json::json!({ "anyOf": [] }),
            serde_json::json!({ "oneOf": true }),
            serde_json::json!({ "anyOf": [{ "type": "object" }, { "type": "string" }] }),
            serde_json::json!({ "oneOf": [{ "type": "object" }, {}] }),
            serde_json::json!({ "anyOf": object_branch, "oneOf": [] }),
        ] {
            let original = invalid.clone();
            assert!(
                normalize_mcp_output_schema(&mut invalid).is_err(),
                "{invalid}"
            );
            assert_eq!(invalid, original, "invalid schemas must remain unchanged");
        }
    }

    #[test]
    fn generated_output_unions_preserve_variant_validation() {
        let untagged = mcp_output_schema::<UntaggedRootUnion>().unwrap();
        assert_eq!(untagged["type"], "object");
        assert_eq!(untagged["anyOf"].as_array().unwrap().len(), 2);
        let tagged = mcp_output_schema::<CollidingDispatcher>().unwrap();
        assert_eq!(tagged["type"], "object");
        assert_eq!(tagged["oneOf"].as_array().unwrap().len(), 2);
        assert!(tagged.get("properties").is_none());
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Inner {
        label: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Nested {
        inner: Inner,
        count: u32,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct ReservedSchemaPropertyNames {
        #[serde(rename = "$ref")]
        reference: String,
        #[serde(rename = "$defs")]
        defs: String,
        #[serde(rename = "definitions")]
        definitions: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Recursive {
        /// A self-referential field makes this type unrepresentable as a
        /// finite `$ref`-free schema.
        next: Option<Box<Recursive>>,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Described {
        /// Description authored as a doc-comment.
        documented: String,
        #[schemars(description = "Description authored as a schemars attribute.")]
        attributed: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    #[serde(untagged)]
    enum UntaggedRootUnion {
        Text { text: String },
        Count { count: u32 },
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "action", rename_all = "snake_case")]
    enum CollidingDispatcher {
        Text { value: String },
        Count { value: u32 },
    }

    /// A dispatcher whose discriminator tag is `kind`, not `action` — the
    /// out-of-tree flavor shape (e.g. working-hero's query tool). The flattener
    /// must honor the actual serde tag rather than assuming `action`.
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "kind")]
    enum Demo {
        /// Inspect one value without changing it.
        #[serde(rename = "a")]
        A { x: Option<String> },
        /// Apply the requested change.
        #[serde(rename = "b")]
        B {},
    }

    /// A `kind`-tagged dispatcher with a field (`shared`) whose prose differs
    /// by variant, so the render inlines each variant's prose. That text must
    /// name the actual discriminator (`kind`), not a hardcoded `action`.
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "kind")]
    enum DemoShared {
        #[serde(rename = "left")]
        Left {
            /// Text for the left side.
            shared: Option<String>,
        },
        #[serde(rename = "right")]
        Right {
            /// Text for the right side.
            shared: String,
            /// Only the right side counts.
            count: u32,
        },
        #[serde(rename = "middle")]
        Middle {
            /// Text for the left side.
            shared: Option<String>,
        },
    }

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "shape", deny_unknown_fields)]
    enum NestedPayload {
        Text { action: String, text: String },
        Number { number: i64 },
    }

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
    enum ConditionalDispatcher {
        Submit { payload: NestedPayload },
        Clear {},
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(tag = "action")]
    enum RootConditionalDispatcher {
        Choose { mode: String },
        Reset {},
    }

    impl JsonSchema for RootConditionalDispatcher {
        fn schema_name() -> std::borrow::Cow<'static, str> {
            "RootConditionalDispatcher".into()
        }

        fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
            schemars::json_schema!({
                "type": "object",
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "choose", "type": "string" },
                            "mode": { "type": "string" }
                        },
                        "required": ["action", "mode"],
                        "additionalProperties": false,
                        "if": {
                            // Presence matters for the condition: without
                            // this requirement, an absent mode would satisfy
                            // the const-only property subschema.
                            "properties": { "mode": { "const": "strict" } },
                            "required": ["mode"]
                        },
                        "then": {
                            "description": "strict-only branch",
                            "properties": { "level": {} },
                            "required": ["level"]
                        }
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "reset", "type": "string" }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                ]
            })
        }
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(tag = "action")]
    enum UnhoistedBranchRequiredDispatcher {
        Choose { mode: String },
        Reset {},
    }

    impl JsonSchema for UnhoistedBranchRequiredDispatcher {
        fn schema_name() -> std::borrow::Cow<'static, str> {
            "UnhoistedBranchRequiredDispatcher".into()
        }

        fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
            schemars::json_schema!({
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "choose", "type": "string" },
                            "mode": { "type": "string" }
                        },
                        "required": ["action", "mode"],
                        "additionalProperties": false,
                        "if": {
                            "type": "object",
                            "properties": { "mode": { "const": "strict" } }
                        },
                        "then": {
                            "type": "object",
                            "required": ["x"]
                        }
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "reset", "type": "string" }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                ]
            })
        }
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(tag = "action")]
    enum RefDispatcher {
        Submit { payload: String },
        Clear {},
    }

    impl JsonSchema for RefDispatcher {
        fn schema_name() -> std::borrow::Cow<'static, str> {
            "RefDispatcher".into()
        }

        fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
            schemars::json_schema!({
                "$defs": {
                    "Payload": {
                        "type": "object",
                        "properties": { "value": { "type": "string" } },
                        "required": ["value"],
                        "additionalProperties": false
                    },
                    "Submit": {
                        "type": "object",
                        "properties": {
                            "action": { "const": "submit", "type": "string" },
                            "payload": {
                                "$ref": "#/$defs/Payload",
                                "description": "payload"
                            }
                        },
                        "required": ["action", "payload"],
                        "description": "target submit"
                    }
                },
                "oneOf": [
                    {
                        "$ref": "#/$defs/Submit",
                        "description": "submit",
                        "properties": { "extra": { "type": "integer" } }
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "clear", "type": "string" }
                        },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                ]
            })
        }
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(tag = "action")]
    enum ConditionalFieldCollisionDispatcher {
        Conditional { mode: String },
        Typed { x: i64 },
    }

    impl JsonSchema for ConditionalFieldCollisionDispatcher {
        fn schema_name() -> std::borrow::Cow<'static, str> {
            "ConditionalFieldCollisionDispatcher".into()
        }

        fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
            schemars::json_schema!({
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "conditional", "type": "string" },
                            "mode": { "type": "string" }
                        },
                        "required": ["action"],
                        "additionalProperties": false,
                        "if": {
                            "type": "object",
                            "properties": { "mode": { "const": "strict" } },
                            "required": ["mode"]
                        },
                        "then": {
                            "type": "object",
                            "properties": { "x": { "type": "string" } }
                        }
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "typed", "type": "string" },
                            "x": { "type": "integer" }
                        },
                        "required": ["action", "x"],
                        "additionalProperties": false
                    }
                ]
            })
        }
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(tag = "action")]
    enum UntypedVariantDispatcher {
        Run { value: String },
        Clear {},
    }

    impl JsonSchema for UntypedVariantDispatcher {
        fn schema_name() -> std::borrow::Cow<'static, str> {
            "UntypedVariantDispatcher".into()
        }

        fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
            schemars::json_schema!({
                "oneOf": [
                    {
                        "properties": {
                            "action": { "const": "run", "type": "string" },
                            "value": { "type": "string" }
                        },
                        "required": ["action", "value"]
                    },
                    {
                        "properties": {
                            "action": { "const": "clear", "type": "string" }
                        },
                        "required": ["action"]
                    }
                ]
            })
        }
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct PartiallyDescribed {
        #[schemars(description = "A described field.")]
        described: String,
        bare: String,
    }

    fn dispatcher(args: &McpArgsSchema) -> &McpDispatcherSchema {
        args.dispatcher
            .as_ref()
            .unwrap_or_else(|| panic!("a tagged enum derives a dispatcher: {:#}", args.schema))
    }

    #[test]
    fn the_action_guide_gives_each_action_its_prose_and_fields() {
        let args = mcp_tool_schema::<Demo>();
        let guide = args
            .schema
            .pointer("/properties/kind/description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("kind discriminator description present: {:#}", args.schema));
        assert!(
            guide.contains("\n- a: Inspect one value without changing it. Optional: x."),
            "{guide}"
        );
        assert!(
            guide.contains("\n- b: Apply the requested change. No fields."),
            "{guide}"
        );
    }

    #[test]
    fn undescribed_property_names_flags_only_bare_fields() {
        let schema = mcp_tool_schema::<PartiallyDescribed>().schema;
        assert_eq!(
            undescribed_property_names(&schema),
            vec!["bare".to_string()]
        );
    }

    #[test]
    fn shared_field_prose_is_inlined_per_action_under_the_actual_discriminator() {
        let schema = mcp_tool_schema::<DemoShared>().schema;
        let shared = &schema["properties"]["shared"];
        assert_eq!(
            shared["description"],
            "Depends on `kind`:\n- left, middle: Text for the left side.\n- right: Text for the right side.",
            "{schema:#}"
        );
        // Nullability widens; the prose of a one-action field is its own.
        assert_eq!(shared["type"], serde_json::json!(["string", "null"]));
        assert_eq!(
            schema["properties"]["count"]["description"],
            "Only the right side counts."
        );
    }

    #[test]
    fn a_narrowed_render_names_only_the_permitted_actions() {
        let args = mcp_tool_schema::<DemoShared>();
        let narrowed = dispatcher(&args).render(|action| action != "right");
        assert_eq!(
            narrowed["properties"]["kind"]["enum"],
            serde_json::json!(["left", "middle"])
        );
        assert!(
            narrowed.pointer("/properties/count").is_none(),
            "{narrowed:#}"
        );
        assert_eq!(
            narrowed["properties"]["shared"]["description"],
            "Text for the left side."
        );
        let guide = narrowed["properties"]["kind"]["description"]
            .as_str()
            .unwrap();
        assert!(!guide.contains("- right"), "{guide}");
        assert_eq!(dispatcher(&args).render(|_| true), args.schema);
    }

    #[test]
    fn invalid_dispatcher_refs_fail_registration() {
        let cases = [
            (
                serde_json::json!("#/$defs/Missing"),
                serde_json::json!({ "Known": { "type": "string" } }),
                "unresolved local reference",
            ),
            (
                serde_json::json!("https://example.test/schema"),
                serde_json::json!({ "Known": { "type": "string" } }),
                "non-local reference",
            ),
            (
                serde_json::json!("#/$defs/A"),
                serde_json::json!({
                    "A": { "$ref": "#/$defs/B" },
                    "B": { "$ref": "#/$defs/A" }
                }),
                "cyclic local reference",
            ),
        ];
        for (reference, defs, expected) in cases {
            let schema = serde_json::json!({
                "$defs": defs,
                "oneOf": [{
                    "type": "object",
                    "properties": {
                        "action": { "const": "run", "type": "string" },
                        "value": { "$ref": reference }
                    },
                    "required": ["action", "value"],
                    "additionalProperties": false
                }]
            });
            let error = derive_dispatcher(&schema).expect_err("invalid refs reject");
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn core_goal_modify_nullable_evidence_is_safe_on_the_flat_root() {
        use crate::mcp::core_tools::goal::{CoreGoalArgs, CoreGoalTool};
        use crate::mcp::{McpTool, validate_action_args};

        let registry = crate::FlavorRegistry::default().freeze_or_panic_for_tests();
        let descriptor = registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == "core_goal")
            .expect("core_goal registered");
        let argument = descriptor
            .action_argument_schema("modify")
            .expect("modify action schema");
        assert_eq!(
            argument["properties"]["evidence"]["type"],
            serde_json::json!(["array", "null"])
        );

        for evidence in [serde_json::Value::Null, serde_json::json!(["A:example"])] {
            let value = serde_json::json!({
                "action": "modify",
                "goal": "G:example",
                "schema_id": "goal",
                "title": "title",
                "text": "text",
                "evidence": evidence.clone()
            });
            assert!(
                validate_action_args("core_goal", CoreGoalTool::ACTION_ARG_SPECS, &value).is_ok(),
                "flat precheck rejects {value:#}"
            );
            assert!(
                evaluates(&descriptor.args_schema, &value),
                "flat schema rejects {value:#}"
            );
            assert!(
                serde_json::from_value::<CoreGoalArgs>(value).is_ok(),
                "serde rejects {evidence:#}"
            );
        }
    }

    /// Every registered tool's wire schemas are plain JSON Schema: no `x-`
    /// keyword, no `$schema`, no root Rust type name, no non-standard
    /// `format`; a dispatcher's is its full render.
    #[test]
    fn registered_wire_schemas_are_plain_json_schema() {
        fn keywords(value: &serde_json::Value, out: &mut Vec<String>) {
            let Some(map) = value.as_object() else {
                return;
            };
            for (key, child) in map {
                out.push(key.clone());
                if let Some(format) = child.as_str().filter(|_| key == "format") {
                    out.push(format!("format:{format}"));
                }
            }
            let mut map = map.clone();
            for_each_subschema_mut(&mut map, &mut |child| keywords(child, out));
        }
        let registry = crate::FlavorRegistry::default().freeze_or_panic_for_tests();
        for tool in registry.list_mcp_tools() {
            let mut found = Vec::new();
            keywords(&tool.args_schema, &mut found);
            keywords(&mcp_wire_output_schema(&tool.output_schema), &mut found);
            assert!(
                !tool.args_schema.to_string().contains("\"default\":null"),
                "{}: default null",
                tool.name
            );
            for keyword in &found {
                assert!(
                    !keyword.starts_with("x-") && keyword != "$schema",
                    "{}: {keyword}",
                    tool.name
                );
                if let Some(format) = keyword.strip_prefix("format:") {
                    assert!(
                        STANDARD_FORMATS.contains(&format),
                        "{}: {format}",
                        tool.name
                    );
                }
            }
            assert!(tool.args_schema.get("title").is_none(), "{}", tool.name);
            if let Some(dispatcher) = &tool.dispatcher_schema {
                assert_eq!(
                    dispatcher.render(|_| true),
                    tool.args_schema,
                    "{}",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn doc_comment_wraps_are_rejoined_but_paragraphs_and_lists_kept() {
        #[derive(Deserialize, JsonSchema)]
        #[allow(dead_code)]
        struct Wrapped {
            /// A description a source file
            /// wrapped at its line width.
            field: String,
        }
        assert_eq!(
            unwrap_lines(
                "One sentence wrapped\nat the source width.\n\nA paragraph:\n- one\n- two\n1. three"
            ),
            "One sentence wrapped at the source width.\n\nA paragraph:\n- one\n- two\n1. three"
        );
        assert_eq!(
            mcp_tool_schema::<Wrapped>().schema["properties"]["field"]["description"],
            "A description a source file wrapped at its line width."
        );
    }

    #[test]
    fn the_wire_output_schema_keeps_validation_and_property_names() {
        let documented = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Reply",
            "description": "What the tool returns.",
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "A field named description.",
                    "examples": ["text"]
                },
                "count": { "type": "integer", "format": "uint32", "minimum": 0, "default": 1 },
                "at": { "type": "string", "format": "date-time", "title": "When" }
            },
            "required": ["description"]
        });
        assert_eq!(
            mcp_wire_output_schema(&documented),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string" },
                    "count": { "type": "integer", "minimum": 0 },
                    "at": { "type": "string", "format": "date-time" }
                },
                "required": ["description"]
            })
        );
    }

    /// A reply is what its type *serializes* to: a field serde may skip is
    /// optional, else a strict client refuses the reply that omits it.
    #[test]
    fn output_schemas_follow_the_serialize_contract() {
        #[derive(serde::Serialize, JsonSchema)]
        struct Reply {
            always: u32,
            #[serde(skip_serializing_if = "core::ops::Not::not")]
            deferred: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            maybe: Option<u32>,
        }
        let schema = mcp_output_schema::<Reply>().unwrap();
        assert_eq!(
            schema["required"],
            serde_json::json!(["always"]),
            "{schema:#}"
        );
    }

    #[test]
    #[should_panic(expected = "is recursive")]
    fn recursive_type_panics() {
        let _ = mcp_tool_schema::<Recursive>();
    }

    #[test]
    #[should_panic(expected = "root schema combinator")]
    fn unflattenable_root_union_panics() {
        let _ = mcp_tool_schema::<UntaggedRootUnion>();
    }

    #[test]
    #[should_panic(expected = "root schema type")]
    fn non_object_root_panics() {
        let _ = mcp_tool_schema::<String>();
    }

    #[test]
    #[should_panic(expected = "conflicting property")]
    fn tagged_enum_duplicate_incompatible_properties_panic() {
        let _ = mcp_tool_schema::<CollidingDispatcher>();
    }

    #[test]
    fn non_action_discriminator_flattens_under_its_own_tag() {
        let args = mcp_tool_schema::<Demo>();
        let schema = &args.schema;

        assert_eq!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("object"),
            "kind-tagged dispatcher must have an object root: {schema:#}",
        );
        assert!(
            schema
                .get("properties")
                .is_some_and(serde_json::Value::is_object),
            "kind-tagged dispatcher must expose a top-level properties object: {schema:#}",
        );
        for combinator in ["oneOf", "anyOf", "allOf"] {
            assert!(
                schema.get(combinator).is_none(),
                "kind-tagged dispatcher must not leave a root {combinator}: {schema:#}",
            );
        }

        // The discriminator lives at `properties.kind`, NOT `properties.action`.
        assert!(
            schema.pointer("/properties/action").is_none(),
            "kind-tagged dispatcher must not invent an `action` discriminator: {schema:#}",
        );
        let mut kind_values = schema
            .pointer("/properties/kind/enum")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| {
                panic!("discriminator must live at properties.kind.enum: {schema:#}")
            })
            .iter()
            .map(|value| value.as_str().expect("kind enum values are strings"))
            .collect::<Vec<_>>();
        kind_values.sort_unstable();
        assert_eq!(
            kind_values,
            ["a", "b"],
            "kind enum must carry the renamed variant values: {schema:#}",
        );

        // `required` names the detected discriminator, not `action`.
        let required = schema
            .pointer("/required")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("flattened schema must declare required: {schema:#}"));
        assert!(
            required.iter().any(|item| item == "kind"),
            "flattened schema must require the `kind` discriminator: {schema:#}",
        );
        assert!(
            !required.iter().any(|item| item == "action"),
            "flattened schema must not require a phantom `action` field: {schema:#}",
        );

        // The typed contract is keyed by the variant values, not by `action`.
        let dispatcher = dispatcher(&args);
        assert_eq!(dispatcher.discriminator, "kind");
        assert_eq!(
            dispatcher
                .actions
                .iter()
                .map(|action| action.action.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"],
        );
        assert_eq!(
            dispatcher
                .action("a")
                .and_then(|a| a.description.as_deref()),
            Some("Inspect one value without changing it.")
        );

        // The optional `x` field of variant `a` survives as a top-level property.
        assert!(
            schema.pointer("/properties/x").is_some(),
            "variant fields must be merged into top-level properties: {schema:#}",
        );
    }

    #[test]
    fn action_argument_schema_preserves_nested_union_and_closes_only_its_root() {
        let args = mcp_tool_schema::<ConditionalDispatcher>();
        let argument = &dispatcher(&args)
            .action("submit")
            .expect("submit action schema present")
            .argument_schema;
        assert_eq!(argument.get("type"), Some(&serde_json::json!("object")));
        assert_eq!(
            argument.get("additionalProperties"),
            Some(&serde_json::Value::Bool(false))
        );
        assert!(argument.pointer("/properties/action").is_none());
        assert!(argument.pointer("/properties/payload/oneOf").is_some());
        assert!(
            argument
                .pointer("/properties/payload/oneOf/0/properties/action")
                .is_some()
        );
        assert!(
            argument
                .pointer("/properties/payload/properties/text")
                .is_none()
        );
        assert!(
            argument
                .pointer("/properties/payload/properties/number")
                .is_none()
        );
        assert!(!schema_contains_ref(argument));
        assert!(argument.get("$defs").is_none());
        let fields = analyze_root_fields(argument).unwrap();
        assert_eq!(fields.allowed, ["payload"]);
        assert_eq!(fields.required, ["payload"]);
    }

    #[test]
    fn root_condition_fields_are_hoisted_but_requirements_stay_conditional() {
        let args = mcp_tool_schema::<RootConditionalDispatcher>();
        let choose = dispatcher(&args)
            .action("choose")
            .expect("choose action schema present");
        let argument = &choose.argument_schema;
        assert_eq!(
            argument["properties"]["mode"],
            serde_json::json!({
                "type": "string"
            })
        );
        assert_eq!(argument["properties"]["level"], serde_json::json!({}));
        assert!(argument.pointer("/if/properties/mode").is_some());
        assert!(argument.pointer("/then/properties/level").is_some());
        assert!(argument.pointer("/properties/action").is_none());
        let fields = analyze_root_fields(argument).unwrap();
        assert_eq!(fields.allowed, ["mode", "level"]);
        assert_eq!(fields.required, ["mode"]);
        assert!(
            choose.description.is_none(),
            "a conditional branch description is not an action description"
        );
        assert!(!evaluates(argument, &serde_json::json!({})));
        assert!(!evaluates(argument, &serde_json::json!({ "level": 1 })));
        assert!(evaluates(argument, &serde_json::json!({ "mode": "loose" })));
        assert!(evaluates(
            argument,
            &serde_json::json!({ "mode": "loose", "level": "text" })
        ));
        assert!(!evaluates(
            argument,
            &serde_json::json!({ "mode": "strict" })
        ));
        assert!(evaluates(
            argument,
            &serde_json::json!({ "mode": "strict", "level": 2 })
        ));
    }

    #[test]
    fn root_field_analysis_accepts_empty_required_and_rejects_bad_names() {
        let empty = serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        });
        assert_eq!(
            analyze_root_fields(&empty).unwrap().required,
            Vec::<String>::new()
        );
        for required in [
            serde_json::json!([""]),
            serde_json::json!(["value", "value"]),
        ] {
            let mut malformed = empty.clone();
            malformed["required"] = required;
            assert!(analyze_root_fields(&malformed).is_err(), "{malformed:#}");
        }
    }

    #[test]
    fn object_applicators_may_omit_type_but_reject_an_explicit_non_object_type() {
        let implicit_object = serde_json::json!({
            "type": "object",
            "properties": { "mode": { "type": "string" } },
            "additionalProperties": false,
            "then": {
                "properties": { "mode": { "const": "strict" } },
                "required": ["mode"]
            }
        });
        validate_closed_root_schema(&implicit_object)
            .expect("object applicators inherit the action root instance type");
        assert_eq!(
            analyze_root_fields(&implicit_object)
                .expect("implicit object applicator analyzes")
                .allowed,
            vec!["mode"]
        );

        let explicit_string = serde_json::json!({
            "type": "object",
            "properties": { "mode": { "type": "string" } },
            "additionalProperties": false,
            "then": {
                "type": "string",
                "properties": { "mode": { "const": "strict" } }
            }
        });
        let error = validate_closed_root_schema(&explicit_string)
            .expect_err("an explicit non-object type contradicts root fields");
        assert!(error.contains("explicit non-object type"), "{error}");
    }

    #[test]
    fn validation_shape_never_rewrites_literal_instance_objects() {
        let left = serde_json::json!({
            "const": {
                "description": "left",
                "type": ["string", "null"]
            },
            "description": "annotation"
        });
        let right = serde_json::json!({
            "const": {
                "description": "right",
                "type": "string"
            },
            "description": "other annotation"
        });
        assert_ne!(
            validation_shape(&left),
            validation_shape(&right),
            "literal const objects are validation data, not nested schemas"
        );

        let mut annotation_only = left.clone();
        annotation_only["description"] = serde_json::json!("different annotation");
        assert_eq!(
            validation_shape(&left),
            validation_shape(&annotation_only),
            "schema-node annotations do not change compatibility"
        );
    }

    #[test]
    #[should_panic(expected = "root schema combinator")]
    fn an_untyped_action_variant_cannot_be_fabricated_as_an_object() {
        let _ = mcp_tool_schema::<UntypedVariantDispatcher>();
    }

    #[test]
    #[should_panic(expected = "root schema combinator")]
    fn an_unhoisted_branch_requirement_cannot_be_fabricated_as_a_flat_field() {
        let _ = mcp_tool_schema::<UnhoistedBranchRequiredDispatcher>();
    }

    /// Small draft-2020-12 evaluator for this test module. It intentionally
    /// implements only the keywords emitted by the dispatcher fixture, so the
    /// acceptance matrix below exercises the advertised contract rather than
    /// relying on a second production validator.
    fn evaluates(schema: &serde_json::Value, value: &serde_json::Value) -> bool {
        if let Some(constant) = schema.get("const")
            && constant != value
        {
            return false;
        }
        if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array)
            && !values.iter().any(|candidate| candidate == value)
        {
            return false;
        }
        if let Some(schema_type) = schema.get("type") {
            let matches = match schema_type {
                serde_json::Value::String(kind) => matches_type(kind, value),
                serde_json::Value::Array(kinds) => kinds
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|kind| matches_type(kind, value)),
                _ => false,
            };
            if !matches {
                return false;
            }
        }
        if let Some(object) = value.as_object() {
            let properties = schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array)
                && required
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|field| !object.contains_key(field))
            {
                return false;
            }
            if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false))
                && object.keys().any(|field| {
                    !properties.is_some_and(|properties| properties.contains_key(field))
                })
            {
                return false;
            }
            if properties.is_some_and(|properties| {
                properties.iter().any(|(field, child)| {
                    object
                        .get(field)
                        .is_some_and(|present| !evaluates(child, present))
                })
            }) {
                return false;
            }
        }
        for keyword in ["oneOf", "anyOf", "allOf"] {
            if let Some(branches) = schema.get(keyword).and_then(serde_json::Value::as_array) {
                let matches = branches
                    .iter()
                    .filter(|branch| evaluates(branch, value))
                    .count();
                let valid = match keyword {
                    "oneOf" => matches == 1,
                    "anyOf" => matches > 0,
                    _ => matches == branches.len(),
                };
                if !valid {
                    return false;
                }
            }
        }
        if let Some(condition) = schema.get("if")
            && evaluates(condition, value)
        {
            if let Some(then) = schema.get("then")
                && !evaluates(then, value)
            {
                return false;
            }
        } else if let Some(otherwise) = schema.get("else")
            && !evaluates(otherwise, value)
        {
            return false;
        }
        true
    }

    fn matches_type(kind: &str, value: &serde_json::Value) -> bool {
        match kind {
            "object" => value.is_object(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            _ => true,
        }
    }
}
