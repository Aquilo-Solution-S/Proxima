mod context_validation;
mod edges;
mod emit_plan;
mod emit_request;
mod ingest;
mod input_validation;
mod plan_persistence;
mod retry;
mod retry_support;
#[cfg(test)]
mod tests;
mod types;

use proxima_core::{InputContractId, OperatorId};
use uuid::Uuid;

pub use emit_plan::CodeEmitExecutionPlanTool;
pub use emit_request::CodeEmitExecutionRequestTool;
pub use retry::CodeRetryExecutionRequestTool;
pub use types::{
    CodeEmitExecutionPlanArgs, CodeEmitExecutionPlanOutput, CodeEmitExecutionRequestArgs,
    CodeEmitExecutionRequestOutput, CodeRetryExecutionRequestArgs, CodeRetryExecutionRequestOutput,
    ExecutionPlanItemArgs, ExecutionPlanItemKind, ExecutionPlanItemOutput,
};

const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
pub const CODE_TARGETS_EXECUTION_REQUEST_RELATION: &str = "proxima-code/targets-execution-request";

const EXECUTION_PLAN_OPERATOR_NAMESPACE: Uuid = Uuid::from_bytes([
    0x65, 0xf8, 0x8d, 0xc6, 0x96, 0x8c, 0x45, 0x9b, 0x8d, 0x32, 0x9a, 0xde, 0x41, 0xfa, 0x5f, 0x21,
]);
const EXECUTION_PLAN_INPUT_CONTRACT_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa5, 0x1e, 0xb1, 0x22, 0xad, 0x14, 0x41, 0xda, 0xa9, 0x25, 0x11, 0x4d, 0x91, 0xa0, 0xf0, 0xdd,
]);

fn execution_plan_operator_id() -> OperatorId {
    OperatorId::new(Uuid::new_v5(
        &EXECUTION_PLAN_OPERATOR_NAMESPACE,
        b"proxima-code/emit_execution_plan-v1",
    ))
}

fn execution_plan_input_contract_id() -> InputContractId {
    InputContractId::new(Uuid::new_v5(
        &EXECUTION_PLAN_INPUT_CONTRACT_NAMESPACE,
        b"proxima-code/execution-plan:plan-source-v1",
    ))
}

pub const CODE_HAS_ACCEPTANCE_CRITERIA_RELATION: &str = "proxima-code/has-acceptance-criteria";
const ACCEPTANCE_CRITERIA_SOURCE_ID: &str = "proxima-code/acceptance-criteria";
const TEST_REQUEST_SOURCE_ID: &str = "proxima-code/test-request";
