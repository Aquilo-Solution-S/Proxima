use crate::flavor::FLAVOR_0;
use crate::flavor::contract::ResourceContract;
use crate::{ScopeKey, ToolName};

use super::core_tools;

/// What calling a tool, or one action of a dispatcher, does to state.
///
/// THE behaviour declaration: a flat tool declares one
/// ([`crate::Tool::EFFECT`]), a dispatcher one per action
/// ([`crate::mcp::McpActionArgSpec::effect`],
/// [`crate::mcp::McpArgvActionSpec::effect`]), a host tool one
/// (`McpHostTool::effect`). Every other answer is derived from it:
///
/// | Reader | Derived answer |
/// |---|---|
/// | owner-role gate | [`Self::ReadOnly`] needs read access, anything else write |
/// | MCP / REST / `OpenAPI` hints | [`McpToolAnnotations::registered`], [`McpToolAnnotations::host`] |
/// | REST `QUERY` | admitted for [`Self::ReadOnly`] only |
/// | [`crate::UnitOfWork::erase_own_series`] | admits only a call whose action is [`Self::Destructive`] |
///
/// Ordered by strength, `ReadOnly < Additive < Destructive`; a dispatcher's
/// tool-level effect is the [`Self::join`] of its actions, so a dispatcher
/// with one destructive action is a destructive tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolEffect {
    /// Reads only; idempotent by construction.
    ReadOnly,
    /// Writes; removes nothing it did not create.
    Additive(Replay),
    /// May remove or overwrite what it did not create.
    Destructive(Replay),
}

/// What an identical second call does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Replay {
    /// Nothing further: the second call lands on the first one's result.
    Idempotent,
    /// Acts again.
    NonIdempotent,
}

impl ToolEffect {
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    #[must_use]
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Destructive(_))
    }

    /// [`Self::ReadOnly`] replays as [`Replay::Idempotent`].
    #[must_use]
    pub const fn replay(self) -> Replay {
        match self {
            Self::ReadOnly => Replay::Idempotent,
            Self::Additive(replay) | Self::Destructive(replay) => replay,
        }
    }

    const fn strength(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Additive(_) => 1,
            Self::Destructive(_) => 2,
        }
    }

    /// The effect of a surface that may run either: the stronger kind, and
    /// idempotent only when both are.
    #[must_use]
    pub const fn join(self, other: Self) -> Self {
        let replay = match (self.replay(), other.replay()) {
            (Replay::Idempotent, Replay::Idempotent) => Replay::Idempotent,
            _ => Replay::NonIdempotent,
        };
        let strongest = if self.strength() >= other.strength() {
            self
        } else {
            other
        };
        match strongest {
            Self::ReadOnly => Self::ReadOnly,
            Self::Additive(_) => Self::Additive(replay),
            Self::Destructive(_) => Self::Destructive(replay),
        }
    }

    /// [`Self::join`] over `effects`; `None` when there are none.
    #[must_use]
    pub fn strongest(effects: impl IntoIterator<Item = Self>) -> Option<Self> {
        effects.into_iter().reduce(Self::join)
    }
}

/// The MCP behaviour hints (`readOnlyHint`, `destructiveHint`,
/// `idempotentHint`, `openWorldHint`) as the wire carries them.
///
/// A projection of a [`ToolEffect`], never a declaration: nothing reads it
/// back. `destructive` and `idempotent` are `None` for a read, where MCP
/// gives them no meaning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct McpToolAnnotations {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

impl McpToolAnnotations {
    /// A registered substrate or flavor tool: `openWorldHint: false`, since
    /// it reaches this deployment's own state and nothing else.
    #[must_use]
    pub const fn registered(effect: ToolEffect) -> Self {
        let mut hints = Self::host(effect);
        hints.open_world = Some(false);
        hints
    }

    /// A host tool: no `openWorldHint`, since the host did not say what else
    /// its tool reaches.
    #[must_use]
    pub const fn host(effect: ToolEffect) -> Self {
        let (destructive, idempotent) = match effect {
            ToolEffect::ReadOnly => (None, None),
            ToolEffect::Additive(replay) => {
                (Some(false), Some(matches!(replay, Replay::Idempotent)))
            }
            ToolEffect::Destructive(replay) => {
                (Some(true), Some(matches!(replay, Replay::Idempotent)))
            }
        };
        Self {
            read_only: Some(effect.is_read_only()),
            destructive,
            idempotent,
            open_world: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreActionMeta {
    pub tool: &'static str,
    pub action: &'static str,
    pub scope_key: &'static str,
    pub produces_schema_ids: &'static [&'static str],
}

/// Flavor #0's `proxima://` resources, in declaration order.
///
/// The list is the contract's, not a second copy of it: advertising a
/// resource and dispatching it read the same `ResourceContract` entry.
#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_resources() -> impl Iterator<Item = &'static ResourceContract> {
    FLAVOR_0.resources.iter()
}

#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_actions() -> impl Iterator<Item = &'static CoreActionMeta> {
    core_tools::goal::CORE_GOAL_ACTIONS
        .iter()
        .chain(core_tools::fact::CORE_FACT_ACTIONS.iter())
        .chain(core_tools::membership::CORE_MEMBERSHIP_ACTIONS.iter())
        .chain(core_tools::transfer::CORE_TRANSFER_ACTIONS.iter())
        .chain(core_tools::upload::CORE_UPLOAD_ACTIONS.iter())
}

#[must_use]
pub fn core_action_meta(tool: &str, action: &str) -> Option<&'static CoreActionMeta> {
    all_core_actions().find(|meta| meta.tool == tool && meta.action == action)
}

/// Every scope key a `ToolScope::Palette` can be asked about, for a frozen
/// registry.
///
/// The scope gate is key membership ([`crate::ToolScope::allows`]), and
/// `read_resource` funnels through that same gate with the resource's scope key
/// standing in for a tool name — so a palette assembled from tools alone denies
/// every `proxima://` read rather than merely not advertising it. Resource keys
/// are therefore part of the canonical enumeration, not an optional extra a
/// caller remembers to append.
///
/// Flat tools contribute their id; dispatchers contribute one `tool:action`
/// leaf per action, because the gate authorizes them at that granularity
/// ([`McpToolDescriptor::palette_keys`](crate::mcp::McpToolDescriptor::palette_keys),
/// argv-keyed dispatchers included). Sorted by canonical spelling, without
/// duplicates.
#[must_use]
pub fn canonical_scope_keys(registry: &crate::FlavorRegistryFrozen) -> Vec<ScopeKey> {
    canonical_scope_keys_excluding(registry, &[])
}

/// [`canonical_scope_keys`] minus every id in `exclude`.
///
/// Exclusion is applied to the *tool name* before its actions are expanded, so
/// naming a dispatcher removes all of its leaves in one step and an action
/// added to it later cannot silently re-enter the palette. Resource keys are
/// excluded by their exact scope key. An entry of `exclude` that is no scope
/// key, or names no registered tool or resource, excludes nothing.
///
/// # Panics
///
/// Never for a registry from `try_freeze`, which checks that every tool name
/// and action is a scope key part.
#[must_use]
pub fn canonical_scope_keys_excluding(
    registry: &crate::FlavorRegistryFrozen,
    exclude: &[&str],
) -> Vec<ScopeKey> {
    let excluded: std::collections::HashSet<ScopeKey> = exclude
        .iter()
        .filter_map(|id| ScopeKey::parse(id).ok())
        .collect();
    let mut keys = Vec::new();
    for tool in registry.list_mcp_tools() {
        let name = ToolName::parse(tool.name)
            .expect("try_freeze refuses a tool name that is no scope key");
        if excluded.contains(&ScopeKey::Tool(name)) {
            continue;
        }
        keys.extend(tool.palette_keys());
    }
    keys.extend(
        all_core_resources()
            .map(|resource| ScopeKey::Resource(resource.key()))
            .filter(|key| !excluded.contains(key)),
    );
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod effect_tests {
    use super::{McpToolAnnotations, Replay, ToolEffect};

    const ALL: [ToolEffect; 5] = [
        ToolEffect::ReadOnly,
        ToolEffect::Additive(Replay::Idempotent),
        ToolEffect::Additive(Replay::NonIdempotent),
        ToolEffect::Destructive(Replay::Idempotent),
        ToolEffect::Destructive(Replay::NonIdempotent),
    ];

    /// A lattice join: commutative, idempotent, associative, with
    /// `ReadOnly` as its bottom — so a dispatcher's effect does not depend
    /// on the order its actions are declared in.
    #[test]
    fn join_is_a_lattice_join_with_read_only_at_the_bottom() {
        for a in ALL {
            assert_eq!(a.join(a), a);
            assert_eq!(a.join(ToolEffect::ReadOnly), a);
            for b in ALL {
                assert_eq!(a.join(b), b.join(a));
                for c in ALL {
                    assert_eq!(a.join(b).join(c), a.join(b.join(c)));
                }
            }
        }
    }

    #[test]
    fn join_takes_the_stronger_kind_and_idempotence_only_from_both() {
        assert_eq!(
            ToolEffect::Additive(Replay::Idempotent)
                .join(ToolEffect::Destructive(Replay::Idempotent)),
            ToolEffect::Destructive(Replay::Idempotent)
        );
        assert_eq!(
            ToolEffect::Destructive(Replay::Idempotent)
                .join(ToolEffect::Additive(Replay::NonIdempotent)),
            ToolEffect::Destructive(Replay::NonIdempotent)
        );
        assert_eq!(
            ToolEffect::ReadOnly.join(ToolEffect::Additive(Replay::Idempotent)),
            ToolEffect::Additive(Replay::Idempotent),
            "a read replays idempotently, so it does not spoil a sibling's idempotence"
        );
        assert_eq!(ToolEffect::strongest([]), None);
    }

    /// The projection never emits a combination MCP gives no meaning:
    /// a read carries no destructive/idempotent hint, a write carries both.
    #[test]
    fn every_effect_projects_to_one_wire_shape() {
        for effect in ALL {
            let hints = McpToolAnnotations::registered(effect);
            assert_eq!(hints.read_only, Some(effect.is_read_only()));
            assert_eq!(hints.open_world, Some(false));
            if effect.is_read_only() {
                assert_eq!((hints.destructive, hints.idempotent), (None, None));
            } else {
                assert_eq!(hints.destructive, Some(effect.is_destructive()));
                assert_eq!(
                    hints.idempotent,
                    Some(effect.replay() == Replay::Idempotent)
                );
            }
            assert_eq!(
                McpToolAnnotations::host(effect),
                McpToolAnnotations {
                    open_world: None,
                    ..hints
                }
            );
        }
    }
}

#[cfg(test)]
mod scope_key_tests {
    use super::{all_core_resources, canonical_scope_keys, canonical_scope_keys_excluding};
    use crate::{FlavorRegistry, ScopeKey};

    fn default_registry() -> crate::FlavorRegistryFrozen {
        FlavorRegistry::default()
            .try_freeze()
            .expect("the default registry seals")
    }

    /// `ResourceContract::key` parses the declared spelling and panics on
    /// anything else; this is the table that keeps it from ever doing so.
    #[test]
    fn every_core_resource_declares_a_resource_key() {
        let mut seen = 0;
        for resource in all_core_resources() {
            assert_eq!(
                resource.key().to_string(),
                resource.scope_key,
                "{} prints as the string it declares",
                resource.name
            );
            seen += 1;
        }
        assert!(seen > 0, "the table is not empty");
    }

    /// The canonical external strings did not change: every key of the
    /// default registry's palette prints as the string that parses to it,
    /// and the palette is sorted by that string without duplicates.
    #[test]
    fn every_canonical_key_round_trips_through_its_canonical_string() {
        let keys = canonical_scope_keys(&default_registry());
        assert!(!keys.is_empty());
        for key in &keys {
            let text = key.to_string();
            let parsed = ScopeKey::parse(&text).expect("a canonical key parses");
            assert_eq!(&parsed, key, "{text}");
            assert_eq!(parsed.to_string(), text);
        }
        let spelled: Vec<String> = keys.iter().map(ToString::to_string).collect();
        let mut sorted = spelled.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(spelled, sorted, "key order is the order of the strings");
    }

    /// Excluding a dispatcher removes its leaves; an entry that is no key, or
    /// names nothing, excludes nothing and does not fail.
    #[test]
    fn exclusion_takes_a_tool_or_a_resource_and_ignores_text_that_is_no_key() {
        let registry = default_registry();
        let all = canonical_scope_keys(&registry);
        let leaf = ScopeKey::parse("core_goal:set").expect("a leaf");
        let resource = ScopeKey::parse("resource:memory").expect("a resource");
        assert!(all.contains(&leaf));
        assert!(all.contains(&resource));

        let without = canonical_scope_keys_excluding(
            &registry,
            &[
                "core_goal",
                "resource:memory",
                "",
                "a:b:c",
                "not a key",
                "nope",
            ],
        );
        assert!(!without.contains(&leaf));
        assert!(!without.contains(&resource));
        assert!(
            without.iter().all(|key| all.contains(key)),
            "exclusion never adds a key"
        );
        assert_eq!(
            canonical_scope_keys_excluding(&registry, &["", "a:b:c", "not a key", "nope"]),
            all
        );
    }
}
