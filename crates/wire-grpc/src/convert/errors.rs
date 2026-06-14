use prost::Message as _;
use tonic::Status;

use proxima_core::{ErrorCode as CoreErrorCode, ProtocolError};

use crate::pb::{self, ErrorCode as PbErrorCode};

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

pub fn protocol_error_to_status(err: ProtocolError) -> Status {
    let code = match err.code {
        CoreErrorCode::AuthRequired => tonic::Code::Unauthenticated,
        CoreErrorCode::Forbidden => tonic::Code::PermissionDenied,
        CoreErrorCode::UnknownSchema => tonic::Code::InvalidArgument,
        CoreErrorCode::AlreadyIngested
        | CoreErrorCode::IdempotencyConflict
        | CoreErrorCode::TriggerConflict => tonic::Code::AlreadyExists,
        CoreErrorCode::NotFound => tonic::Code::NotFound,
        CoreErrorCode::InvalidArgument | CoreErrorCode::DuplicateTriggerInRequest => {
            tonic::Code::InvalidArgument
        }
        CoreErrorCode::ToolNotRegistered => tonic::Code::FailedPrecondition,
        CoreErrorCode::Internal => tonic::Code::Internal,
    };

    let pb_error = pb::ProtocolError {
        code: pb_error_code_from_core(err.code) as i32,
        message: err.message.clone(),
        details: Vec::new(),
        request_id: err.request_id,
    };

    let mut status = Status::new(code, err.message);
    status.metadata_mut().insert_bin(
        "proxima-error-bin",
        tonic::metadata::MetadataValue::from_bytes(&pb_error.encode_to_vec()),
    );
    status
}

fn pb_error_code_from_core(code: CoreErrorCode) -> PbErrorCode {
    match code {
        CoreErrorCode::AuthRequired => PbErrorCode::AuthRequired,
        CoreErrorCode::Forbidden => PbErrorCode::Forbidden,
        CoreErrorCode::UnknownSchema => PbErrorCode::UnknownSchema,
        CoreErrorCode::AlreadyIngested | CoreErrorCode::IdempotencyConflict => {
            PbErrorCode::IdempotencyConflict
        }
        CoreErrorCode::NotFound => PbErrorCode::NotFound,
        CoreErrorCode::InvalidArgument => PbErrorCode::InvalidArgument,
        CoreErrorCode::ToolNotRegistered => PbErrorCode::ToolNotRegistered,
        CoreErrorCode::TriggerConflict => PbErrorCode::TriggerConflict,
        CoreErrorCode::DuplicateTriggerInRequest => PbErrorCode::DuplicateTriggerInRequest,
        CoreErrorCode::Internal => PbErrorCode::Internal,
    }
}
