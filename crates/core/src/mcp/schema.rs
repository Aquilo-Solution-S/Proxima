//! MCP tool argument- and output-schema generation.
//!
//! The single source of truth for a tool's argument schema is its Rust
//! `Args` type, and for its output schema its `Output` type.
//! `mcp_tool_schema` and `mcp_output_schema` both produce a `$ref`-free /
//! `$defs`-free JSON Schema draft 2020-12 document so that MCP clients
//! which do not resolve `$ref` still render every field (see commit
//! 37f209b). They differ in the client-facing normalization applied
//! afterwards; see `mcp_output_schema`.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;

/// Generate a `$ref`-free draft-2020-12 argument schema for `T`.
///
/// Panics at registration (startup) if `T` is recursive: `schemars`
/// cannot inline a recursive subschema, so it emits a `$ref` that no
/// inlining pass can eliminate. A recursive MCP tool argument type is a
/// registration error.
pub(crate) fn mcp_tool_schema<T: JsonSchema>() -> serde_json::Value {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("JsonSchema must serialize");
    flatten_root_tagged_enum(&mut value).unwrap_or_else(|error| {
        panic!(
            "MCP tool type `{}` has invalid local dispatcher schema references: {error}",
            std::any::type_name::<T>(),
        )
    });
    ensure_client_safe_root::<T>(&mut value);
    assert!(
        !schema_contains_ref(&value),
        "MCP tool type `{}` is recursive: schemars emitted a $ref that \
         cannot be inlined. MCP tool argument types must be non-recursive.",
        std::any::type_name::<T>(),
    );
    remove_definition_containers(&mut value);
    value
}

/// Generate a `$ref`-free draft-2020-12 *output* schema for `T`.
///
/// Deliberately a sibling of [`mcp_tool_schema`] rather than a reuse of it.
/// The two share the generator settings and the recursion guard — the
/// `$ref`-free promise is about clients, not about direction — but the two
/// normalization passes `mcp_tool_schema` runs afterwards both encode
/// argument-side assumptions that are wrong here:
///
/// - `flatten_root_tagged_enum` rewrites an internally tagged enum into a
///   flat object with a merged property set, `additionalProperties: false`
///   and an `x-proxima-actions` extension. That is the *dispatcher call
///   surface* — a description of which fields a caller may send for which
///   action. An output union is not a call surface: a client validating a
///   reply needs to know which variant it got, and a merged object claims
///   every variant's fields belong to every variant.
/// - `ensure_client_safe_root` forces an object root and rejects root
///   combinators, because a provider tool `inputSchema` must be an object.
///   Nothing constrains an output that way, and the in-tree outputs prove
///   it: the four action-dispatcher tools answer with `#[serde(untagged)]`
///   enums whose schema root is `anyOf`, and a tool with nothing to say
///   answers `()`, whose root is `type: "null"`.
///
/// A union root is therefore preserved as generated: it is the honest
/// description of a reply that really is one of several shapes.
pub(crate) fn mcp_output_schema<T: JsonSchema>() -> serde_json::Value {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("JsonSchema must serialize");
    // JSON Schema permits a bare boolean as a whole document (`true` accepts
    // everything, `false` accepts nothing), and schemars emits `true` for an
    // unconstrained type such as `serde_json::Value`. MCP's `outputSchema`
    // is typed as an object, so spell the same two schemas as the object
    // forms that mean exactly the same thing.
    match value {
        serde_json::Value::Bool(true) => value = serde_json::json!({}),
        serde_json::Value::Bool(false) => value = serde_json::json!({ "not": {} }),
        _ => {}
    }
    assert!(
        !schema_contains_ref(&value),
        "MCP tool output type `{}` is recursive: schemars emitted a $ref that \
         cannot be inlined. MCP tool output types must be non-recursive.",
        std::any::type_name::<T>(),
    );
    value
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
/// unlike an undeclared `ANNOTATIONS`, which stops a gate from working, a
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
            // A dispatcher's top-level description is a placeholder pointing at
            // `x-proxima-actions`, where the real prose lives; `minimum` stays
            // on the top-level property.
            let mut prose = vec![
                spec.get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            ];
            if let Some(actions) = tool
                .args_schema
                .get("x-proxima-actions")
                .and_then(serde_json::Value::as_object)
            {
                prose.extend(actions.values().filter_map(|action| {
                    action
                        .get("field_descriptions")
                        .and_then(|described| described.get(field))
                        .and_then(serde_json::Value::as_str)
                }));
            }
            // Descriptions wrap, so a claim can straddle a newline.
            let joined = prose
                .join(" ")
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

/// Flatten a schemars root `oneOf` for an internally tagged enum into a plain
/// object schema whose discriminator is the enum's own `#[serde(tag = "...")]`
/// key (e.g. `action`, `kind`), exposed as a string enum.
///
/// Anthropic/OpenAI-compatible tool schemas cannot rely on a root-level union.
/// Runtime serde validation remains authoritative for per-action required
/// fields; the flattened schema is the MCP/client-facing discovery surface.
fn flatten_root_tagged_enum(value: &mut serde_json::Value) -> Result<bool, String> {
    let Some(raw_variants) = value
        .as_object()
        .and_then(|map| map.get("oneOf"))
        .and_then(serde_json::Value::as_array)
        .cloned()
    else {
        return Ok(false);
    };

    // Keep the generated root untouched until every variant has been copied,
    // resolved and validated. An unresolved/non-local/cyclic reference must
    // fail registration with the original `$defs` still present rather than
    // silently dropping the only diagnostic evidence.
    let defs = local_defs(value);
    for definition in defs.draft.values().chain(defs.legacy.values()) {
        let mut checked_definition = definition.clone();
        inline_variant_refs(&mut checked_definition, &defs)?;
    }
    let mut variants = Vec::with_capacity(raw_variants.len());
    for raw_variant in raw_variants {
        let mut variant = raw_variant;
        inline_variant_refs(&mut variant, &defs)?;
        remove_definition_containers(&mut variant);
        variants.push(variant);
    }

    // Detect the discriminator KEY: the single property name present across
    // every variant carrying a string `const`, with a distinct value per
    // variant. For an internally-tagged enum this is the `#[serde(tag = ...)]`
    // field. Bail (leaving the schema unflattened) if there is not exactly one.
    let Some(discriminator) = detect_discriminator_key(&variants) else {
        return Ok(false);
    };

    let mut action_values = Vec::with_capacity(variants.len());
    let mut merged_properties = serde_json::Map::new();
    let mut generated_placeholders = std::collections::BTreeSet::new();
    let mut action_metadata = serde_json::Map::new();
    let mut field_occurrences = std::collections::BTreeMap::<String, usize>::new();

    for variant in &variants {
        // A missing-properties / missing-const variant means this is not the
        // internally-tagged shape we can flatten; bail and leave it unflattened.
        if merge_variant(
            variant,
            &discriminator,
            &mut action_values,
            &mut merged_properties,
            &mut generated_placeholders,
            &mut action_metadata,
            &mut field_occurrences,
        )
        .is_none()
        {
            return Ok(false);
        }
    }

    if action_values.is_empty() {
        return Ok(false);
    }

    for (field, count) in field_occurrences {
        if count > 1
            && let Some(property_schema) = merged_properties.get_mut(&field)
        {
            neutralize_shared_property_description(property_schema, &field, &discriminator);
        }
    }

    let discriminator_description = {
        let base = format!(
            "Dispatcher {discriminator} to execute. Additional fields depend on the selected {discriminator}."
        );
        let signatures = action_signature_block(&action_values, &action_metadata);
        if signatures.is_empty() {
            base
        } else {
            // Standard MCP clients render a property's description but not our
            // `x-proxima-actions` extension, so the per-action field contract is
            // inlined here where every client can see it.
            format!("{base}\nAction signatures (required, then +optional):\n{signatures}")
        }
    };
    merged_properties.insert(
        discriminator.clone(),
        serde_json::json!({
            "type": "string",
            "enum": action_values,
            "description": discriminator_description,
        }),
    );
    let Some(map) = value.as_object_mut() else {
        return Ok(false);
    };
    map.remove("oneOf");
    map.remove("$defs");
    map.remove("definitions");
    map.insert(
        "properties".to_string(),
        serde_json::Value::Object(merged_properties),
    );
    map.insert(
        "required".to_string(),
        serde_json::json!([discriminator.clone()]),
    );
    map.insert(
        "additionalProperties".to_string(),
        serde_json::Value::Bool(false),
    );
    map.insert(
        "x-proxima-actions".to_string(),
        serde_json::Value::Object(action_metadata),
    );
    Ok(true)
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

/// Normalize one raw tagged-enum variant into the action-only schema carried
/// in x-proxima-actions. The copied variant's local refs are expanded first;
/// this pass then removes only the direct discriminator and performs the root
/// closure/field hoist. Nested property schemas remain otherwise untouched.
struct NormalizedActionArgumentSchema {
    schema: serde_json::Value,
    generated_placeholders: std::collections::BTreeSet<String>,
}

fn normalize_action_argument_schema(
    variant: &serde_json::Value,
    discriminator: &str,
) -> Option<NormalizedActionArgumentSchema> {
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
    let mut generated_placeholders = std::collections::BTreeSet::new();
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
        generated_placeholders.insert(name.clone());
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
    Some(NormalizedActionArgumentSchema {
        schema,
        generated_placeholders,
    })
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

/// Fold one tagged-enum variant into the flattener's accumulators.
///
/// Returns `None` when the variant is not the expected internally-tagged object
/// shape (no object root, or no string `const` under `discriminator`), which
/// signals the caller to abort flattening and leave the schema as a root union.
fn merge_variant(
    variant: &serde_json::Value,
    discriminator: &str,
    action_values: &mut Vec<serde_json::Value>,
    merged_properties: &mut serde_json::Map<String, serde_json::Value>,
    generated_placeholders: &mut std::collections::BTreeSet<String>,
    action_metadata: &mut serde_json::Map<String, serde_json::Value>,
    field_occurrences: &mut std::collections::BTreeMap<String, usize>,
) -> Option<()> {
    let action = root_const_value(variant, discriminator)?;
    action_values.push(serde_json::Value::String(action.clone()));
    let normalized = normalize_action_argument_schema(variant, discriminator)?;
    let argument_schema = normalized.schema;
    let action_placeholders = normalized.generated_placeholders;
    let fields = analyze_root_fields(&argument_schema).ok()?;
    let allowed_fields = fields
        .allowed
        .iter()
        .map(|field| serde_json::Value::String(field.clone()))
        .collect::<Vec<_>>();
    let required = fields
        .required
        .iter()
        .map(|field| serde_json::Value::String(field.clone()))
        .collect::<Vec<_>>();
    let mut field_descriptions = serde_json::Map::new();
    let normalized_properties = argument_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or(())
        .ok()?;
    for (name, property_schema) in normalized_properties {
        if name != discriminator {
            *field_occurrences.entry(name.clone()).or_default() += 1;
            if let Some(description) = property_schema
                .get("description")
                .and_then(serde_json::Value::as_str)
            {
                field_descriptions.insert(
                    name.clone(),
                    serde_json::Value::String(description.to_string()),
                );
            }
            merge_property_schema(
                merged_properties,
                generated_placeholders,
                name,
                property_schema,
                action_placeholders.contains(name),
                &action,
                discriminator,
            );
        }
    }
    let mut metadata = serde_json::json!({
        "allowed_fields": allowed_fields,
        "required_fields": required,
        "field_descriptions": field_descriptions,
        "argument_schema": argument_schema,
    });
    if let Some(description) = root_variant_description(variant) {
        metadata
            .as_object_mut()
            .expect("action metadata is an object")
            .insert(
                "description".to_string(),
                serde_json::Value::String(description.to_string()),
            );
    }
    action_metadata.insert(action, metadata);
    Some(())
}

/// Build a compact one-line-per-action signature block from the accumulated
/// `x-proxima-actions` metadata: `- <action>: <required> (+ <optional>)`.
/// Optional fields are `allowed_fields` minus `required_fields`. Empty when no
/// action carries any field.
fn action_signature_block(
    action_values: &[serde_json::Value],
    action_metadata: &serde_json::Map<String, serde_json::Value>,
) -> String {
    use std::fmt::Write as _;
    let mut lines = Vec::new();
    for action_value in action_values {
        let Some(action) = action_value.as_str() else {
            continue;
        };
        let Some(meta) = action_metadata
            .get(action)
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        let field_list = |key: &str| -> Vec<String> {
            meta.get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let required = field_list("required_fields");
        let optional: Vec<String> = field_list("allowed_fields")
            .into_iter()
            .filter(|field| !required.contains(field))
            .collect();
        let mut line = format!("- {action}: ");
        match (required.is_empty(), optional.is_empty()) {
            (true, true) => line.push_str("(no args)"),
            (true, false) => {
                write!(line, "(+ {})", optional.join(", ")).expect("write to String is infallible");
            }
            (false, true) => line.push_str(&required.join(", ")),
            (false, false) => {
                line.push_str(&required.join(", "));
                write!(line, " (+ {})", optional.join(", "))
                    .expect("write to String is infallible");
            }
        }
        lines.push(line);
    }
    lines.join("\n")
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

fn merge_property_schema(
    merged_properties: &mut serde_json::Map<String, serde_json::Value>,
    generated_placeholders: &mut std::collections::BTreeSet<String>,
    name: &str,
    property_schema: &serde_json::Value,
    incoming_is_placeholder: bool,
    action: &str,
    discriminator: &str,
) {
    let Some(existing) = merged_properties.get_mut(name) else {
        merged_properties.insert(name.to_string(), property_schema.clone());
        if incoming_is_placeholder {
            generated_placeholders.insert(name.to_string());
        }
        return;
    };
    let existing_is_placeholder = generated_placeholders.contains(name);
    if existing_is_placeholder || incoming_is_placeholder {
        // A generated `{}` is a compatibility projection, not a claim that
        // this action accepts every property shape only because the other
        // action's root field happened to be typed. Keep it widest so the
        // flattened root cannot reject a value accepted by either action.
        if !existing_is_placeholder {
            *existing = serde_json::Value::Object(serde_json::Map::new());
            generated_placeholders.insert(name.to_string());
        }
        return;
    }
    if let Some(widened) = nullable_compatible_schema(existing, property_schema) {
        let changed = existing != &widened || existing != property_schema;
        *existing = widened;
        if changed {
            neutralize_shared_property_description(existing, name, discriminator);
        }
        return;
    }
    panic!(
        "conflicting property `{name}` while flattening action `{action}`: {existing:#} vs {property_schema:#}"
    );
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

fn neutralize_shared_property_description(
    value: &mut serde_json::Value,
    name: &str,
    discriminator: &str,
) {
    if let serde_json::Value::Object(map) = value {
        for key in ["default", "title"] {
            map.remove(key);
        }
        map.insert(
            "description".to_string(),
            serde_json::Value::String(format!(
                "Shared dispatcher field `{name}`. Semantics and requiredness depend on `{discriminator}`; see `x-proxima-actions` or `proxima://tools` for {discriminator}-specific guidance."
            )),
        );
    }
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

    /// A `kind`-tagged dispatcher with a field (`shared`) present in more than
    /// one variant, so the flattener neutralizes its description. That guidance
    /// text must name the actual discriminator (`kind`), not a hardcoded
    /// `action`.
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "kind")]
    enum DemoShared {
        #[serde(rename = "left")]
        Left { shared: Option<String> },
        #[serde(rename = "right")]
        Right { shared: Option<String> },
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

    /// The prose parser is pinned against the phrasings that actually ship
    /// rather than invented ones.
    #[test]
    fn the_ceiling_parser_reads_the_phrasings_that_ship() {
        for (prose, expected) in [
            (
                "Body text for the derived memory, 1 to 20000 chars.",
                Some(20_000),
            ),
            (
                "Short title for the agent-observed Fact, 1 to 240 chars.",
                Some(240),
            ),
            (
                "Optional tags for later search, at most 16. Each is stored",
                Some(16),
            ),
            (
                "Optional tag filter, at most 16 tags. Tags are matched",
                Some(16),
            ),
            (
                "Child goals to create (1 to 50); each is set Active",
                Some(50),
            ),
            ("At most 64 patterns.", Some(64)),
            (
                "Search query over owner-visible memories. 1 to 512 chars.",
                Some(512),
            ),
        ] {
            assert_eq!(claimed_ceiling(prose), expected, "prose: {prose}");
        }
    }

    /// Under-reporting is the deliberate failure mode: a phrasing outside
    /// the closed list yields nothing rather than a wrong number.
    #[test]
    fn the_ceiling_parser_declines_rather_than_guesses() {
        for prose in [
            "Omit or null for 10.",
            "Defaults to 8 when omitted.",
            "Page past the 50-result cap by passing next_cursor back.",
            "at most a handful",
            "",
        ] {
            assert_eq!(claimed_ceiling(prose), None, "prose: {prose}");
        }
    }

    /// The keyword follows the JSON type, not the English unit word, so an
    /// `Option<String>` (which emits `["string", "null"]`) still resolves.
    #[test]
    fn the_ceiling_keyword_follows_the_declared_type() {
        let cases = [
            (serde_json::json!({"type": "string"}), Some("maxLength")),
            (
                serde_json::json!({"type": ["string", "null"]}),
                Some("maxLength"),
            ),
            (serde_json::json!({"type": "array"}), Some("maxItems")),
            (
                serde_json::json!({"type": ["array", "null"]}),
                Some("maxItems"),
            ),
            (serde_json::json!({"type": "integer"}), Some("maximum")),
            (serde_json::json!({"type": "boolean"}), None),
            (serde_json::json!({}), None),
        ];
        for (spec, expected) in cases {
            assert_eq!(ceiling_keyword(&spec), expected, "spec: {spec}");
        }
    }

    #[test]
    fn dispatcher_description_carries_per_action_signatures() {
        let schema = mcp_tool_schema::<Demo>();
        let description = schema
            .pointer("/properties/kind/description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("kind discriminator description present: {schema:#}"));
        assert!(
            description.contains("Action signatures"),
            "dispatcher description must inline the signature block: {description}",
        );
        // Variant `a` carries one optional field `x`; variant `b` carries none.
        assert!(
            description.contains("- a: (+ x)"),
            "per-action signature must list optional fields: {description}",
        );
        assert!(
            description.contains("- b: (no args)"),
            "a variant with no fields must render `(no args)`: {description}",
        );
    }

    #[test]
    fn dispatcher_metadata_carries_variant_descriptions() {
        let schema = mcp_tool_schema::<Demo>();
        assert_eq!(
            schema
                .pointer("/x-proxima-actions/a/description")
                .and_then(serde_json::Value::as_str),
            Some("Inspect one value without changing it."),
            "the variant doc-comment must describe its derived action: {schema:#}",
        );
        assert_eq!(
            schema
                .pointer("/x-proxima-actions/b/description")
                .and_then(serde_json::Value::as_str),
            Some("Apply the requested change."),
            "each action keeps its own variant description: {schema:#}",
        );
    }

    #[test]
    fn undescribed_property_names_flags_only_bare_fields() {
        let schema = mcp_tool_schema::<PartiallyDescribed>();
        assert_eq!(
            undescribed_property_names(&schema),
            vec!["bare".to_string()]
        );
    }

    #[test]
    fn shared_field_description_names_the_actual_discriminator() {
        let schema = mcp_tool_schema::<DemoShared>();
        let description = schema
            .pointer("/properties/shared/description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("shared field description present: {schema:#}"));
        assert!(
            description.contains("depend on `kind`"),
            "shared-field guidance must name the actual discriminator `kind`: {description}",
        );
        assert!(
            !description.contains("`action`"),
            "shared-field guidance must not leak the hardcoded `action` discriminator: {description}",
        );
    }

    #[test]
    fn nested_struct_schema_is_inlined() {
        let schema = mcp_tool_schema::<Nested>();
        assert!(
            !schema_contains_ref(&schema),
            "nested struct schema must be $ref-free: {schema:#}",
        );
        assert!(
            schema.get("$defs").is_none(),
            "nested struct schema must be $defs-free: {schema:#}",
        );
        assert!(
            schema
                .pointer("/properties/inner/properties/label")
                .is_some(),
            "the inlined Inner subschema must expose its fields: {schema:#}",
        );
    }

    #[test]
    fn definition_keyword_property_names_are_not_removed() {
        let schema = mcp_tool_schema::<ReservedSchemaPropertyNames>();
        assert!(schema.pointer("/properties/$ref").is_some());
        assert!(schema.pointer("/properties/$defs").is_some());
        assert!(schema.pointer("/properties/definitions").is_some());

        let mut legacy_subschemas = serde_json::json!({
            "type": "object",
            "additionalItems": {
                "$defs": { "Nested": { "type": "string" } }
            },
            "dependencies": {
                "value": {
                    "definitions": { "Nested": { "type": "string" } }
                }
            }
        });
        remove_definition_containers(&mut legacy_subschemas);
        assert!(
            legacy_subschemas
                .pointer("/additionalItems/$defs")
                .is_none()
        );
        assert!(
            legacy_subschemas
                .pointer("/dependencies/value/definitions")
                .is_none()
        );
    }

    #[test]
    fn dispatcher_refs_are_inlined_per_variant_and_keep_siblings_conjunctive() {
        let schema = mcp_tool_schema::<RefDispatcher>();
        assert!(
            schema.get("$defs").is_none(),
            "root defs must be consumed: {schema:#}"
        );
        assert!(
            !schema_contains_ref(&schema),
            "dispatcher must be ref-free: {schema:#}"
        );
        let argument = &schema["x-proxima-actions"]["submit"]["argument_schema"];
        assert_eq!(
            schema.pointer("/x-proxima-actions/submit/description"),
            Some(&serde_json::json!("submit"))
        );
        assert_eq!(argument["type"], serde_json::json!("object"));
        assert_eq!(
            argument["allOf"][1]["description"],
            serde_json::json!("submit")
        );
        assert!(argument["allOf"][1].get("type").is_none());
        assert_eq!(
            argument["allOf"][1]["properties"]["extra"]["type"],
            serde_json::json!("integer")
        );
        assert_eq!(argument["properties"]["extra"], serde_json::json!({}));
        let payload = &argument["allOf"][0]["properties"]["payload"];
        assert_eq!(payload["allOf"][0]["type"], serde_json::json!("object"));
        assert_eq!(
            payload["allOf"][0]["required"],
            serde_json::json!(["value"])
        );
        assert_eq!(
            payload["allOf"][1]["description"],
            serde_json::json!("payload")
        );
        assert!(payload.pointer("/allOf/0/$ref").is_none());
        assert!(!schema_contains_ref(argument));
        assert!(evaluates(
            argument,
            &serde_json::json!({
                "payload": { "value": "ok" }
            })
        ));
        assert!(!evaluates(
            argument,
            &serde_json::json!({
                "payload": { "value": 7 }
            })
        ));
        assert!(evaluates(
            argument,
            &serde_json::json!({
                "payload": { "value": "ok" },
                "extra": 7
            })
        ));
        assert!(!evaluates(
            argument,
            &serde_json::json!({
                "payload": { "value": "ok" },
                "extra": "wrong"
            })
        ));

        let defs = local_defs(&serde_json::json!({
            "$defs": { "Value": { "type": "string" } }
        }));
        let mut sibling_schema = serde_json::json!({
            "$ref": "#/$defs/Value",
            "const": "strict"
        });
        inline_variant_refs(&mut sibling_schema, &defs).expect("local ref resolves");
        assert_eq!(
            sibling_schema,
            serde_json::json!({
                "allOf": [
                    { "type": "string" },
                    { "const": "strict" }
                ]
            })
        );
        assert!(evaluates(&sibling_schema, &serde_json::json!("strict")));
        assert!(!evaluates(&sibling_schema, &serde_json::json!("loose")));
    }

    #[test]
    fn ref_siblings_can_reuse_a_definition_without_being_marked_cyclic() {
        let defs = local_defs(&serde_json::json!({
            "$defs": { "Value": { "type": "string" } }
        }));
        let mut schema = serde_json::json!({
            "$ref": "#/$defs/Value",
            "allOf": [{ "$ref": "#/$defs/Value" }]
        });
        inline_variant_refs(&mut schema, &defs).expect("sibling ref is not cyclic");
        assert_eq!(
            schema,
            serde_json::json!({
                "allOf": [
                    { "type": "string" },
                    { "allOf": [{ "type": "string" }] }
                ]
            })
        );
        assert!(evaluates(&schema, &serde_json::json!("value")));
        assert!(!evaluates(&schema, &serde_json::json!(7)));
    }

    #[test]
    fn instance_values_named_like_schema_keywords_are_not_inlined() {
        let defs = local_defs(&serde_json::json!({
            "$defs": { "Value": { "type": "string" } }
        }));
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "literal": {
                    "const": { "$ref": "literal" },
                    "default": { "$ref": "default" },
                    "examples": [{ "$ref": "example" }]
                }
            }
        });
        inline_variant_refs(&mut schema, &defs).expect("instance data is not a schema ref");
        assert_eq!(
            schema["properties"]["literal"]["const"],
            serde_json::json!({ "$ref": "literal" })
        );
        assert_eq!(
            schema["properties"]["literal"]["default"],
            serde_json::json!({ "$ref": "default" })
        );
    }

    #[test]
    fn invalid_dispatcher_refs_leave_the_root_untouched_for_registration_failure() {
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
            let mut schema = serde_json::json!({
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
            let error = flatten_root_tagged_enum(&mut schema).expect_err("invalid refs reject");
            assert!(error.contains(expected), "{error}");
            assert!(
                schema.get("oneOf").is_some(),
                "invalid refs must not flatten: {schema:#}"
            );
            assert!(
                schema.get("$defs").is_some(),
                "invalid refs must retain defs: {schema:#}"
            );
        }
    }

    #[test]
    fn conditional_neutral_hoists_do_not_override_typed_sibling_fields() {
        let schema = mcp_tool_schema::<ConditionalFieldCollisionDispatcher>();
        assert_eq!(
            schema["properties"]["x"],
            serde_json::json!({
                "description": "Shared dispatcher field `x`. Semantics and requiredness depend on `action`; see `x-proxima-actions` or `proxima://tools` for action-specific guidance."
            })
        );
        assert_eq!(
            schema["x-proxima-actions"]["conditional"]["argument_schema"]["properties"]["x"],
            serde_json::json!({})
        );
        assert_eq!(
            schema["x-proxima-actions"]["typed"]["argument_schema"]["properties"]["x"]["type"],
            serde_json::json!("integer")
        );
        assert!(evaluates(
            &schema,
            &serde_json::json!({
                "action": "conditional",
                "mode": "loose",
                "x": "text"
            })
        ));
        assert!(evaluates(
            &schema,
            &serde_json::json!({
                "action": "conditional",
                "mode": "strict",
                "x": "text"
            })
        ));
        assert!(evaluates(
            &schema,
            &serde_json::json!({
                "action": "typed",
                "x": 7
            })
        ));
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
        let argument = &descriptor.args_schema["x-proxima-actions"]["modify"]["argument_schema"];
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

    #[test]
    fn nullable_shared_fields_merge_to_the_same_widest_schema_in_either_order() {
        let required = serde_json::json!({
            "type": "array",
            "items": { "type": "string" }
        });
        let optional = serde_json::json!({
            "type": ["array", "null"],
            "items": { "type": ["string", "null"] }
        });
        let merge = |first: &serde_json::Value, second: &serde_json::Value| {
            let mut properties = serde_json::Map::new();
            let mut placeholders = std::collections::BTreeSet::new();
            merge_property_schema(
                &mut properties,
                &mut placeholders,
                "value",
                first,
                false,
                "first",
                "action",
            );
            merge_property_schema(
                &mut properties,
                &mut placeholders,
                "value",
                second,
                false,
                "second",
                "action",
            );
            properties.remove("value").expect("merged property")
        };
        let left_then_right = merge(&required, &optional);
        let right_then_left = merge(&optional, &required);
        assert_eq!(left_then_right, right_then_left);
        assert_eq!(
            left_then_right["type"],
            serde_json::json!(["array", "null"])
        );
        assert_eq!(
            left_then_right["items"]["type"],
            serde_json::json!(["string", "null"])
        );
    }

    #[test]
    fn root_const_extraction_and_detection_share_the_full_root_walk() {
        let variants = vec![
            serde_json::json!({
                "type": "object",
                "if": { "properties": { "action": { "const": "left" } } }
            }),
            serde_json::json!({
                "type": "object",
                "then": { "properties": { "action": { "const": "right" } } }
            }),
        ];
        assert_eq!(
            root_const_value(&variants[0], "action"),
            Some("left".to_string())
        );
        assert_eq!(
            root_const_value(&variants[1], "action"),
            Some("right".to_string())
        );
        assert_eq!(
            detect_discriminator_key(&variants),
            Some("action".to_string())
        );

        let ambiguous = serde_json::json!({
            "type": "object",
            "properties": { "action": { "const": "left" } },
            "else": { "properties": { "action": { "const": "other" } } }
        });
        assert_eq!(root_const_value(&ambiguous, "action"), None);
        assert_eq!(
            detect_discriminator_key(&[
                ambiguous,
                serde_json::json!({
                    "type": "object",
                    "properties": { "action": { "const": "right" } }
                })
            ]),
            None
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
    fn field_descriptions_survive() {
        let schema = mcp_tool_schema::<Described>();
        assert_eq!(
            schema
                .pointer("/properties/documented/description")
                .and_then(serde_json::Value::as_str),
            Some("Description authored as a doc-comment."),
            "doc-comment description must survive into the schema: {schema:#}",
        );
        assert_eq!(
            schema
                .pointer("/properties/attributed/description")
                .and_then(serde_json::Value::as_str),
            Some("Description authored as a schemars attribute."),
            "schemars-attribute description must survive into the schema: {schema:#}",
        );
    }

    #[test]
    fn non_action_discriminator_flattens_under_its_own_tag() {
        let schema = mcp_tool_schema::<Demo>();

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

        // `x-proxima-actions` is keyed by the variant values, not by `action`.
        let actions = schema
            .get("x-proxima-actions")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| {
                panic!("flattened schema must expose x-proxima-actions: {schema:#}")
            });
        assert!(
            actions.contains_key("a") && actions.contains_key("b"),
            "x-proxima-actions must be keyed by the kind values a/b: {schema:#}",
        );
        assert_eq!(
            actions.len(),
            2,
            "x-proxima-actions must hold one entry per variant: {schema:#}",
        );

        // The optional `x` field of variant `a` survives as a top-level property.
        assert!(
            schema.pointer("/properties/x").is_some(),
            "variant fields must be merged into top-level properties: {schema:#}",
        );
    }

    #[test]
    fn action_argument_schema_preserves_nested_union_and_closes_only_its_root() {
        let schema = mcp_tool_schema::<ConditionalDispatcher>();
        let argument = schema
            .pointer("/x-proxima-actions/submit/argument_schema")
            .unwrap_or_else(|| panic!("submit action schema present: {schema:#}"));
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
        assert_eq!(
            schema.pointer("/x-proxima-actions/submit/allowed_fields"),
            Some(&serde_json::json!(["payload"]))
        );
        assert_eq!(
            schema.pointer("/x-proxima-actions/submit/required_fields"),
            Some(&serde_json::json!(["payload"]))
        );
        assert!(
            schema
                .pointer("/x-proxima-actions/submit/allowed_fields/0/text")
                .is_none()
        );
    }

    #[test]
    fn root_condition_fields_are_hoisted_but_requirements_stay_conditional() {
        let schema = mcp_tool_schema::<RootConditionalDispatcher>();
        let argument = schema
            .pointer("/x-proxima-actions/choose/argument_schema")
            .unwrap_or_else(|| panic!("choose action schema present: {schema:#}"));
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
        assert_eq!(
            schema.pointer("/x-proxima-actions/choose/allowed_fields"),
            Some(&serde_json::json!(["mode", "level"]))
        );
        assert_eq!(
            schema.pointer("/x-proxima-actions/choose/required_fields"),
            Some(&serde_json::json!(["mode"]))
        );
        assert!(
            schema
                .pointer("/x-proxima-actions/choose/description")
                .is_none(),
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
    fn root_condition_evaluator_matches_the_conditional_contract() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string" },
                "level": {}
            },
            "additionalProperties": false,
            "if": {
                "properties": { "mode": { "type": "string", "const": "strict" } }
            },
            "then": { "required": ["level"] }
        });
        assert!(evaluates(&schema, &serde_json::json!({ "mode": "loose" })));
        assert!(!evaluates(
            &schema,
            &serde_json::json!({ "mode": "strict" })
        ));
        assert!(evaluates(
            &schema,
            &serde_json::json!({ "mode": "strict", "level": 2 })
        ));

        let broad_string = serde_json::json!({
            "type": "string",
            "if": { "const": "strict" },
            "then": { "const": "strict" }
        });
        assert!(evaluates(&broad_string, &serde_json::json!("strict")));
        assert!(evaluates(&broad_string, &serde_json::json!("loose")));
    }

    #[test]
    fn non_object_root_applicators_without_fields_are_allowed() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "mode": { "type": "string" } },
            "required": ["mode"],
            "additionalProperties": false,
            "if": { "type": "string" },
            "then": { "const": false }
        });
        validate_closed_root_schema(&schema).expect("field-free applicators are valid");
        assert_eq!(
            analyze_root_fields(&schema)
                .expect("field-free applicators analyze")
                .allowed,
            vec!["mode"]
        );
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

    #[test]
    fn argument_schema_matches_flat_precheck_and_serde_matrix() {
        use crate::mcp::{McpActionArgSpec, McpToolAudience, validate_action_args};

        const SPECS: &[McpActionArgSpec] = &[McpActionArgSpec {
            action: "submit",
            allowed_fields: &["payload"],
            required_fields: &["payload"],
            annotations: None,
            audience: McpToolAudience::Shared,
        }];
        let schema = mcp_tool_schema::<ConditionalDispatcher>();
        let argument = &schema["x-proxima-actions"]["submit"]["argument_schema"];
        let cases = [
            (
                "text",
                serde_json::json!({
                    "action": "submit",
                "payload": { "shape": "Text", "action": "nested", "text": "ok" }
                }),
                true,
                true,
                true,
            ),
            (
                "number",
                serde_json::json!({
                    "action": "submit",
                "payload": { "shape": "Number", "number": 7 }
                }),
                true,
                true,
                true,
            ),
            (
                "missing nested",
                serde_json::json!({
                    "action": "submit",
                "payload": { "shape": "Text", "action": "nested" }
                }),
                true,
                false,
                false,
            ),
            (
                "conflicting shapes",
                serde_json::json!({
                    "action": "submit",
                "payload": { "shape": "Text", "action": "nested", "text": "ok", "number": 7 }
                }),
                true,
                false,
                false,
            ),
            (
                "top-level unknown",
                serde_json::json!({
                    "action": "submit",
                "payload": { "shape": "Text", "action": "nested", "text": "ok" },
                    "extra": true
                }),
                false,
                false,
                false,
            ),
            (
                "nested unknown",
                serde_json::json!({
                    "action": "submit",
                "payload": { "shape": "Text", "action": "nested", "text": "ok", "extra": true }
                }),
                true,
                false,
                false,
            ),
            ("empty", serde_json::json!({}), false, false, false),
        ];
        for (name, value, flat_ok, schema_ok, serde_ok) in cases {
            assert_eq!(
                validate_action_args("conditional", SPECS, &value).is_ok(),
                flat_ok,
                "{name}"
            );
            let mut actionless = value.clone();
            actionless
                .as_object_mut()
                .expect("matrix values are objects")
                .remove("action");
            assert_eq!(evaluates(argument, &actionless), schema_ok, "{name}");
            assert_eq!(
                serde_json::from_value::<ConditionalDispatcher>(value).is_ok(),
                serde_ok,
                "{name}"
            );
        }
    }
}
