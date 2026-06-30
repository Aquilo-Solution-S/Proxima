use super::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, EdgePayload, FactPayload,
    GoalPayload, PerspectivePayload, ProtocolPayload, SidecarPayload,
};

pub(super) fn decode_protocol_payload<T>(value: &serde_json::Value) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<T>(value.clone()).map_err(|e| e.to_string())
}

pub(super) fn ingest_fact_payload<F>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    F: FactPayload + Send + Sync,
{
    let payload = decode_protocol_payload::<F>(value)?;
    let key_bytes = Some(payload.receipt_key());
    let rendered_text = Some(payload.render());
    Ok(ProtocolPayload {
        key_bytes,
        sidecar_payload: SidecarPayload::fact(payload),
        rendered_text,
        content_hash: None,
    })
}

pub(super) fn ingest_abstraction_payload<A>(
    value: &serde_json::Value,
) -> Result<ProtocolPayload, String>
where
    A: AbstractionPayload + Send + Sync,
{
    let payload = decode_protocol_payload::<A>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::abstraction(payload),
        rendered_text: None,
        content_hash: None,
    })
}

pub(super) fn ingest_perspective_payload<P>(
    value: &serde_json::Value,
) -> Result<ProtocolPayload, String>
where
    P: PerspectivePayload + Send + Sync,
{
    let payload = decode_protocol_payload::<P>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::perspective(payload),
        rendered_text: None,
        content_hash: None,
    })
}

pub(super) fn ingest_goal_payload<G>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    G: GoalPayload,
{
    let payload = decode_protocol_payload::<G>(value)?;
    let key_bytes = Some(payload.goal_key());
    Ok(ProtocolPayload {
        key_bytes,
        sidecar_payload: SidecarPayload::goal(payload),
        rendered_text: None,
        content_hash: None,
    })
}

pub(super) fn ingest_edge_payload<E>(value: &serde_json::Value) -> Result<ProtocolPayload, String>
where
    E: EdgePayload + Send + Sync,
{
    let payload = decode_protocol_payload::<E>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::edge(payload),
        rendered_text: None,
        content_hash: None,
    })
}

pub(super) fn ingest_cited_object_payload<C>(
    value: &serde_json::Value,
) -> Result<ProtocolPayload, String>
where
    C: CitedObjectPayload,
{
    let payload = decode_protocol_payload::<C>(value)?;
    let content_hash = Some(payload.idempotency_key());
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::cited_object(payload),
        rendered_text: None,
        content_hash,
    })
}

pub(super) fn ingest_citation_mapping_payload<M>(
    value: &serde_json::Value,
) -> Result<ProtocolPayload, String>
where
    M: CitationMappingPayload,
{
    let payload = decode_protocol_payload::<M>(value)?;
    Ok(ProtocolPayload {
        key_bytes: None,
        sidecar_payload: SidecarPayload::citation_mapping(payload),
        rendered_text: None,
        content_hash: None,
    })
}
