use super::{
    Engine, HashSet, Owner, PersonalityInstanceId, PersonalityStatus, Serialize, StorageError,
    WakeEntryRow, WakeEntryTriggerKind, is_enabled_chat_message_wake,
};

#[derive(Debug, Clone, Serialize)]
pub struct WakeCoordinationContext {
    pub chat_targets: Vec<WakeCoordinationTarget>,
    pub wake_path: WakePath,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeCoordinationTarget {
    pub personality_instance_id: uuid::Uuid,
    pub display_name: String,
    pub root_perspective_memory_id: uuid::Uuid,
    pub chat_message_wake_entry_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakePath {
    pub upstream: Vec<WakePathNode>,
    pub current: WakePathNode,
    pub downstream: Vec<WakePathNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakePathNode {
    pub personality_instance_id: uuid::Uuid,
    pub display_name: String,
    pub root_perspective_memory_id: uuid::Uuid,
    pub wake_entry_id: uuid::Uuid,
    pub wake_entry_label: String,
    pub trigger_schema_id: String,
    pub produces_schema_ids: Vec<String>,
}

/// # Errors
///
/// Returns `StorageError` when personality instances cannot be listed.
pub async fn build_wake_coordination_context(
    engine: &Engine,
    owner: &Owner,
    current_personality: PersonalityInstanceId,
    current_wake_entry: &WakeEntryRow,
) -> Result<WakeCoordinationContext, StorageError> {
    let rows = engine
        .storage()
        .list_personality_instances(owner, false)
        .await?;
    let mut chat_targets = Vec::new();
    let mut current = None;
    let mut all_nodes = Vec::new();

    for row in rows {
        if row.status != PersonalityStatus::Active {
            continue;
        }
        let chat_message_wake_entry_ids: Vec<_> = row
            .wake_entries
            .iter()
            .filter(|entry| is_enabled_chat_message_wake(entry))
            .map(|entry| entry.wake_entry_id)
            .collect();
        if row.personality_instance_id != current_personality
            && !chat_message_wake_entry_ids.is_empty()
        {
            chat_targets.push(WakeCoordinationTarget {
                personality_instance_id: row.personality_instance_id.into_inner(),
                display_name: row.display_name.clone(),
                root_perspective_memory_id: row.current_root_perspective_memory_id.into_inner(),
                chat_message_wake_entry_ids,
            });
        }

        for entry in row.wake_entries {
            if !entry.enabled || entry.trigger_kind != WakeEntryTriggerKind::OnMemory {
                continue;
            }
            let node = WakePathNode {
                personality_instance_id: row.personality_instance_id.into_inner(),
                display_name: row.display_name.clone(),
                root_perspective_memory_id: row.current_root_perspective_memory_id.into_inner(),
                wake_entry_id: entry.wake_entry_id,
                wake_entry_label: entry.label.clone(),
                trigger_schema_id: entry.trigger_id.clone(),
                produces_schema_ids: Vec::new(),
            };
            if row.personality_instance_id == current_personality
                && entry.wake_entry_id == current_wake_entry.wake_entry_id
            {
                current = Some(node.clone());
            }
            all_nodes.push(node);
        }
    }

    let current = current.unwrap_or_else(|| WakePathNode {
        personality_instance_id: current_personality.into_inner(),
        display_name: String::new(),
        root_perspective_memory_id: uuid::Uuid::nil(),
        wake_entry_id: current_wake_entry.wake_entry_id,
        wake_entry_label: current_wake_entry.label.clone(),
        trigger_schema_id: current_wake_entry.trigger_id.clone(),
        produces_schema_ids: Vec::new(),
    });
    let current_produces: HashSet<_> = current.produces_schema_ids.iter().cloned().collect();
    let upstream = all_nodes
        .iter()
        .filter(|node| {
            node.wake_entry_id != current.wake_entry_id
                && node
                    .produces_schema_ids
                    .iter()
                    .any(|schema| schema == &current.trigger_schema_id)
        })
        .cloned()
        .collect();
    let downstream = all_nodes
        .into_iter()
        .filter(|node| {
            node.wake_entry_id != current.wake_entry_id
                && current_produces.contains(&node.trigger_schema_id)
        })
        .collect();

    Ok(WakeCoordinationContext {
        chat_targets,
        wake_path: WakePath {
            upstream,
            current,
            downstream,
        },
    })
}
