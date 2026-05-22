use super::*;

pub(in crate::chat) async fn load_thread_messages(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedMessage>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_messages(&ctx.owner, thread_key, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_replies(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedReply>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_replies(&ctx.owner, thread_key, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_end_requests(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedEndRequest>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_end_requests(&ctx.owner, thread_key, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_ended(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<Option<LoadedEnded>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_ended(&ctx.owner, thread_key)
        .await?)
}

pub(in crate::chat) async fn load_thread_compactions(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedCompaction>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_compactions(&ctx.owner, thread_key, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_summaries(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedSummary>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_summaries(&ctx.owner, thread_key, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_approval_policies(
    ctx: &McpToolCtx,
    target_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalPolicy>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_approval_policies(&ctx.owner, target_memory_ids, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_approval_votes(
    ctx: &McpToolCtx,
    policy_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalVote>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_approval_votes(&ctx.owner, policy_memory_ids, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_approval_decisions(
    ctx: &McpToolCtx,
    policy_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalDecision>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_approval_decisions(&ctx.owner, policy_memory_ids, limit)
        .await?)
}

pub(in crate::chat) async fn load_thread_edges(
    ctx: &McpToolCtx,
    thread_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedThreadEdge>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_edges(&ctx.owner, thread_memory_ids, limit)
        .await?)
}
