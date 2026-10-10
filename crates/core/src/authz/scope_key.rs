//! Typed tool-scope keys: the entries of a [`ToolScope::Palette`](super::ToolScope).
//!
//! A palette holds three kinds of key, each with one canonical spelling:
//!
//! | Key | Spelling | Names |
//! |---|---|---|
//! | [`ScopeKey::Tool`] | `tool` | a flat tool, or a dispatcher as a whole |
//! | [`ScopeKey::Action`] | `tool:action` | one action of a dispatcher |
//! | [`ScopeKey::Resource`] | `resource:name` | a `proxima://` resource read |
//!
//! [`ScopeKey::parse`] is the one parser and [`Display`](std::fmt::Display)
//! the one printer: `parse(s).to_string() == s` for every canonical key. The
//! components validate on construction, so a name can never contain the `:`
//! that separates them and a tool can never spell a resource key.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::mcp::provider_safe_tool_name;

/// The reserved first segment of a resource key, and so the one tool name
/// that cannot head an action.
const RESOURCE_PREFIX: &str = "resource";
const SEPARATOR: char = ':';

/// Which part of a key an error is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKeyPart {
    Tool,
    Action,
    /// The name after `resource:`.
    Resource,
}

impl fmt::Display for ScopeKeyPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Tool => "tool name",
            Self::Action => "action name",
            Self::Resource => "resource name",
        })
    }
}

/// Why a string is not a [`ScopeKey`], or not one of its parts.
///
/// Carries no input text, so a caller that holds the entry names it
/// (`PROXIMA_TOOL_ALLOW` does) and a message never repeats request data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopeKeyError {
    /// The key, or a part of it, is empty: `""`, `":set"`, `"goal:"`,
    /// `"resource:"`.
    #[error("{0} must not be empty")]
    Empty(ScopeKeyPart),
    /// A part holds the `:` that separates parts.
    #[error("{0} must not contain ':'")]
    Delimiter(ScopeKeyPart),
    /// A part is outside `[A-Za-z0-9_.-]` (whitespace and `/` included) or
    /// holds two dots in a row: the grammar of
    /// [`provider_safe_tool_name`].
    #[error("{0} must be provider-safe: letters, digits, '_', '-' and single '.' only")]
    NotProviderSafe(ScopeKeyPart),
    /// More than one `:`, and no key has more than two parts.
    #[error("a scope key has at most two ':'-separated parts")]
    TooManyParts,
    /// `resource` heading an action: `resource:x` is a resource key, so a
    /// dispatcher cannot be called `resource`. A flat tool called `resource`
    /// is fine; its key is `resource`.
    #[error("'resource' cannot name a dispatcher: 'resource:<name>' is a resource key")]
    ReservedToolName,
}

/// A name every part of a key is made of: non-empty, no `:`, and its own
/// provider-safe form.
fn validate_name(part: ScopeKeyPart, text: &str) -> Result<(), ScopeKeyError> {
    if text.is_empty() {
        Err(ScopeKeyError::Empty(part))
    } else if text.contains(SEPARATOR) {
        Err(ScopeKeyError::Delimiter(part))
    } else if provider_safe_tool_name(text) != text {
        Err(ScopeKeyError::NotProviderSafe(part))
    } else {
        Ok(())
    }
}

macro_rules! key_name {
    ($(#[$meta:meta])* $name:ident, $part:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// # Errors
            ///
            /// [`ScopeKeyError`] when `text` is empty, holds a `:`, or is not
            /// provider-safe.
            pub fn parse(text: &str) -> Result<Self, ScopeKeyError> {
                validate_name($part, text)?;
                Ok(Self(text.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

key_name!(
    /// A tool's name as a scope key sees it: 1 or more characters of
    /// `[A-Za-z0-9_.-]`, no `..`, never a `:`. Equal to its provider-safe
    /// form, so it is also the tool's wire name.
    ///
    /// The field is private, so the only way to hold one is a name that
    /// passed [`Self::parse`]:
    ///
    /// ```compile_fail
    /// let _ = proxima_core::ToolName("core_goal:set".to_owned());
    /// ```
    ToolName,
    ScopeKeyPart::Tool
);

key_name!(
    /// A dispatcher action's name, from the same grammar as [`ToolName`].
    ActionName,
    ScopeKeyPart::Action
);

/// A resource scope key, `resource:` plus a name from the [`ToolName`]
/// grammar. Its [`Display`](fmt::Display) is the whole key; the name alone is
/// [`Self::name`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceKey(String);

impl ResourceKey {
    /// `name` is what follows `resource:`.
    ///
    /// # Errors
    ///
    /// [`ScopeKeyError`] when `name` is empty, holds a `:`, or is not
    /// provider-safe.
    pub fn parse(name: &str) -> Result<Self, ScopeKeyError> {
        validate_name(ScopeKeyPart::Resource, name)?;
        Ok(Self(name.to_owned()))
    }

    /// The part after `resource:`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{RESOURCE_PREFIX}{SEPARATOR}{}", self.0)
    }
}

/// One entry of a [`ToolScope::Palette`](super::ToolScope): what the scope
/// gate and the listings ask a palette about.
///
/// Built by [`Self::parse`], [`Self::action`] and `From` a validated
/// component. [`Self::Action`] is `#[non_exhaustive]`, so outside this crate
/// it cannot be written as a literal and a `resource` tool cannot be forced
/// into one:
///
/// ```compile_fail
/// let tool = proxima_core::ToolName::parse("resource").unwrap();
/// let action = proxima_core::ActionName::parse("memory").unwrap();
/// let _ = proxima_core::ScopeKey::Action { tool, action };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScopeKey {
    /// A flat tool, or a dispatcher as a whole: `tool`.
    Tool(ToolName),
    /// One action of a dispatcher: `tool:action`.
    #[non_exhaustive]
    Action { tool: ToolName, action: ActionName },
    /// A resource read: `resource:name`.
    Resource(ResourceKey),
}

impl ScopeKey {
    /// The one parser: the prefix `resource:` gives a [`Self::Resource`], one
    /// other `:` an [`Self::Action`], no `:` a [`Self::Tool`]. Case is
    /// significant (`Resource:x` is an action of the tool `Resource`).
    ///
    /// # Errors
    ///
    /// [`ScopeKeyError`] for an empty key or part, a `:` inside a part, more
    /// than one separator, or a part that is not provider-safe.
    pub fn parse(text: &str) -> Result<Self, ScopeKeyError> {
        if let Some(name) = text
            .strip_prefix(RESOURCE_PREFIX)
            .and_then(|rest| rest.strip_prefix(SEPARATOR))
        {
            return ResourceKey::parse(name).map(Self::Resource);
        }
        match text.split_once(SEPARATOR) {
            None => ToolName::parse(text).map(Self::Tool),
            Some((_, action)) if action.contains(SEPARATOR) => Err(ScopeKeyError::TooManyParts),
            Some((tool, action)) => {
                Self::action(ToolName::parse(tool)?, ActionName::parse(action)?)
            }
        }
    }

    /// The `tool:action` leaf.
    ///
    /// # Errors
    ///
    /// [`ScopeKeyError::ReservedToolName`] for the tool `resource`, whose
    /// leaf would spell a resource key.
    pub fn action(tool: ToolName, action: ActionName) -> Result<Self, ScopeKeyError> {
        if tool.as_str() == RESOURCE_PREFIX {
            return Err(ScopeKeyError::ReservedToolName);
        }
        Ok(Self::Action { tool, action })
    }

    /// The canonical spelling as its parts, so ordering and printing need no
    /// intermediate string.
    fn parts(&self) -> [&str; 3] {
        match self {
            Self::Tool(tool) => [tool.as_str(), "", ""],
            Self::Action { tool, action } => [tool.as_str(), ":", action.as_str()],
            Self::Resource(resource) => ["resource:", resource.name(), ""],
        }
    }

    fn canonical_bytes(&self) -> impl Iterator<Item = u8> {
        self.parts().into_iter().flat_map(str::bytes)
    }
}

impl From<ToolName> for ScopeKey {
    fn from(tool: ToolName) -> Self {
        Self::Tool(tool)
    }
}

impl From<ResourceKey> for ScopeKey {
    fn from(resource: ResourceKey) -> Self {
        Self::Resource(resource)
    }
}

impl fmt::Display for ScopeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.parts()
            .into_iter()
            .try_for_each(|part| formatter.write_str(part))
    }
}

impl FromStr for ScopeKey {
    type Err = ScopeKeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// The order of the canonical strings, which is the order palettes were
/// sorted in while they held strings.
impl Ord for ScopeKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.canonical_bytes().cmp(other.canonical_bytes())
    }
}

impl PartialOrd for ScopeKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionName, ResourceKey, ScopeKey, ScopeKeyError, ScopeKeyPart, ToolName};
    use crate::protocol::{action, resource, tool};

    fn parse(text: &str) -> Result<ScopeKey, ScopeKeyError> {
        ScopeKey::parse(text)
    }

    fn tool_key(name: &str) -> ScopeKey {
        ScopeKey::Tool(ToolName::parse(name).expect("a tool name"))
    }

    fn leaf(tool: &str, action: &str) -> ScopeKey {
        ScopeKey::action(
            ToolName::parse(tool).expect("a tool name"),
            ActionName::parse(action).expect("an action name"),
        )
        .expect("a leaf")
    }

    fn resource_key(name: &str) -> ScopeKey {
        ScopeKey::Resource(ResourceKey::parse(name).expect("a resource name"))
    }

    #[test]
    fn the_three_spellings_parse_to_their_kind() {
        assert_eq!(
            parse("core_search_memories"),
            Ok(tool_key("core_search_memories"))
        );
        assert_eq!(parse("core_goal:set"), Ok(leaf("core_goal", "set")));
        assert_eq!(parse("resource:memory"), Ok(resource_key("memory")));
        // The name grammar allows `-` and `.`: `resource:memory-lineage`,
        // `flavor.tool_name`.
        assert_eq!(
            parse("resource:memory-lineage"),
            Ok(resource_key("memory-lineage"))
        );
        assert_eq!(
            parse("proxima-code_x:a.b"),
            Ok(leaf("proxima-code_x", "a.b"))
        );
    }

    #[test]
    fn a_flat_tool_named_resource_is_the_key_resource() {
        let key = parse("resource").expect("a flat tool called resource");
        assert_eq!(key, tool_key("resource"));
        assert_eq!(key.to_string(), "resource");
    }

    /// `parse` and `Display` are inverse for every key in the vocabulary of
    /// the protocol, and the printed form is the input.
    #[test]
    fn every_protocol_constant_prints_back_as_itself() {
        let tools = [
            tool::CORE_SEARCH_MEMORIES,
            tool::CORE_RECALL,
            tool::CORE_THINK,
            tool::CORE_MEMORY_SPACES,
            tool::CORE_REMEMBER,
            tool::CORE_EPISODE_COMMIT,
            tool::CORE_RECORD_UTTERANCE,
            tool::CORE_DERIVE,
            tool::CORE_INTERPRET,
            tool::CORE_GOAL,
            tool::CORE_FACT,
            tool::CORE_MEMBERSHIP,
            tool::CORE_TRANSFER,
            tool::CORE_UPLOAD,
            tool::CORE_FORGET,
        ];
        let actions = [
            action::CORE_FACT_CITATION_OF_FACT,
            action::CORE_FACT_FACTS_CITING_OBJECT,
            action::CORE_GOAL_SET,
            action::CORE_GOAL_TRANSITION,
            action::CORE_GOAL_MODIFY,
            action::CORE_GOAL_MARK_ACHIEVED,
            action::CORE_GOAL_DECOMPOSE,
            action::CORE_MEMBERSHIP_ADD_MEMBER,
            action::CORE_MEMBERSHIP_REMOVE_MEMBER,
            action::CORE_MEMBERSHIP_LIST_MEMBERS,
            action::CORE_TRANSFER_TO_OWNER,
            action::CORE_UPLOAD_PREPARE,
            action::CORE_UPLOAD_COMPLETE,
            action::CORE_UPLOAD_ABORT,
            action::CORE_UPLOAD_READ_URL,
        ];
        let resources = [
            resource::MEMORY,
            resource::MEMORIES,
            resource::MEMORY_LINEAGE,
            resource::TOOLS,
            resource::GRAPH,
            resource::CHANGE_EVENTS,
            resource::WAKE_CANDIDATES,
            resource::SCHEMAS,
            resource::SCHEMA,
            resource::GOALS,
            resource::GOAL,
        ];
        for text in tools {
            assert!(matches!(parse(text), Ok(ScopeKey::Tool(_))), "{text}");
        }
        for text in actions {
            assert!(matches!(parse(text), Ok(ScopeKey::Action { .. })), "{text}");
        }
        for text in resources {
            assert!(matches!(parse(text), Ok(ScopeKey::Resource(_))), "{text}");
        }
        for text in tools.into_iter().chain(actions).chain(resources) {
            let key = parse(text).unwrap_or_else(|error| panic!("{text}: {error}"));
            assert_eq!(key.to_string(), text);
            assert_eq!(text.parse::<ScopeKey>(), Ok(key));
        }
    }

    /// The spec's bad-data list, each with the reason it is refused.
    #[test]
    fn the_parser_refuses_what_is_not_a_key() {
        use ScopeKeyError::{Delimiter, Empty, NotProviderSafe, TooManyParts};
        use ScopeKeyPart::{Action, Resource, Tool};

        let refused = [
            // empty key and empty parts
            ("", Empty(Tool)),
            (":", Empty(Tool)),
            (":set", Empty(Tool)),
            ("core_goal:", Empty(Action)),
            ("resource:", Empty(Resource)),
            // a separator beyond the first
            ("a:b:c", TooManyParts),
            ("core_goal:set:", TooManyParts),
            ("::", TooManyParts),
            ("resource:a:b", Delimiter(Resource)),
            // whitespace anywhere
            (" core_goal", NotProviderSafe(Tool)),
            ("core_goal ", NotProviderSafe(Tool)),
            ("core goal", NotProviderSafe(Tool)),
            ("core_goal: set", NotProviderSafe(Action)),
            ("core_goal:set\n", NotProviderSafe(Action)),
            ("resource: memory", NotProviderSafe(Resource)),
            // characters outside the grammar
            ("a/b", NotProviderSafe(Tool)),
            ("proxima-code/register_repo", NotProviderSafe(Tool)),
            ("core_goal:a/b", NotProviderSafe(Action)),
            ("caf\u{e9}", NotProviderSafe(Tool)),
            ("a\0b", NotProviderSafe(Tool)),
            ("a..b", NotProviderSafe(Tool)),
            ("resource:a/b", NotProviderSafe(Resource)),
        ];
        for (text, reason) in refused {
            assert_eq!(parse(text), Err(reason), "{text:?}");
        }
    }

    #[test]
    fn the_prefix_is_case_sensitive_and_exact() {
        // Only the exact lowercase prefix makes a resource key.
        assert_eq!(parse("Resource:memory"), Ok(leaf("Resource", "memory")));
        assert_eq!(parse("RESOURCE:memory"), Ok(leaf("RESOURCE", "memory")));
        assert_eq!(parse("resources:memory"), Ok(leaf("resources", "memory")));
        assert_eq!(parse("res:memory"), Ok(leaf("res", "memory")));
        // Names are case-sensitive too: two spellings, two keys.
        assert_ne!(parse("Core_Goal"), parse("core_goal"));
        assert_ne!(parse("core_goal:Set"), parse("core_goal:set"));
    }

    /// A tool named so that `tool:action` spells `resource:x` cannot be built:
    /// not by `action`, and not by `parse`, which reads that spelling as a
    /// resource.
    #[test]
    fn no_action_spells_a_resource_key() {
        let resource_tool = ToolName::parse("resource").expect("a flat tool name");
        let name = ActionName::parse("memory").expect("an action name");

        assert_eq!(
            ScopeKey::action(resource_tool, name),
            Err(ScopeKeyError::ReservedToolName)
        );
        assert!(matches!(
            parse("resource:memory"),
            Ok(ScopeKey::Resource(_))
        ));
    }

    #[test]
    fn components_refuse_what_they_cannot_hold() {
        assert_eq!(
            ToolName::parse(""),
            Err(ScopeKeyError::Empty(ScopeKeyPart::Tool))
        );
        assert_eq!(
            ToolName::parse("a:b"),
            Err(ScopeKeyError::Delimiter(ScopeKeyPart::Tool))
        );
        assert_eq!(
            ActionName::parse("a b"),
            Err(ScopeKeyError::NotProviderSafe(ScopeKeyPart::Action))
        );
        assert_eq!(
            ActionName::parse(":"),
            Err(ScopeKeyError::Delimiter(ScopeKeyPart::Action))
        );
        // `resource:memory` is not a resource *name*: the prefix is not part
        // of it.
        assert_eq!(
            ResourceKey::parse("resource:memory"),
            Err(ScopeKeyError::Delimiter(ScopeKeyPart::Resource))
        );
        assert_eq!(
            ResourceKey::parse(""),
            Err(ScopeKeyError::Empty(ScopeKeyPart::Resource))
        );
    }

    /// The grammar is the one `provider_safe_tool_name` defines, character
    /// class by character class.
    #[test]
    fn a_name_is_exactly_its_own_provider_safe_form() {
        for text in ["a", "A9", "a_b", "a-b", "a.b", "a.b.c", "_", "-", "."] {
            assert!(ToolName::parse(text).is_ok(), "{text}");
        }
        for text in ["a..b", "..", "a b", "a/b", "a,b", "é", "a\tb", "a;b"] {
            assert!(ToolName::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn ordering_is_the_order_of_the_canonical_strings() {
        // `-` (0x2d) sorts before `:` (0x3a): by string `a-b` precedes `a:b`,
        // while a component-wise order (`a` before `a-b`) would put them the
        // other way round.
        let mut keys: Vec<ScopeKey> = [
            "resource:z",
            "a:b",
            "a-b",
            "a",
            "resource",
            "a-b:c",
            "b",
            "resource-x",
        ]
        .iter()
        .map(|text| parse(text).expect("a key"))
        .collect();
        let mut texts: Vec<String> = keys.iter().map(ToString::to_string).collect();
        keys.sort();
        texts.sort();
        assert_eq!(
            keys.iter().map(ToString::to_string).collect::<Vec<_>>(),
            texts
        );
    }
}
