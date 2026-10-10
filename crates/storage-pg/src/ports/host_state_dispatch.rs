//! Typed command dispatch for the host's one [`PgHostStateParticipant`].
//!
//! [`PgCommandDispatcher`] is that participant. It routes each request to the
//! [`HostStateHandler`] registered for its command type. Before a handler runs
//! the dispatcher compares the command's owner and every owner the payload
//! names with the owner the engine stamped on the permit (Lean
//! `HostStateMaintenance.PayloadAllowed`) and wraps the command in an
//! [`AgreedCommand`], which only that comparison can construct. When the
//! dispatcher calls a handler, the command's owners agreed with the permit it
//! passes; the handler never sees the [`HostStateRequest`] envelope. An
//! `AgreedCommand` is not bound to a permit: host code that keeps one and
//! calls [`HostStateHandler::handle`] itself with another permit is outside
//! that guarantee.
use std::any::TypeId;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;

use async_trait::async_trait;
use proxima_core::storage_ports::{
    HostStateCommand, HostStateOutcome, HostStateParticipantId, HostStatePayloadOwners,
    HostStateReply, HostStateRequest, HostStateWritePermit, StateSurfaceName,
};
use proxima_core::{Owner, StorageError};
use sqlx::{Postgres, Transaction};

use super::{PgHostStateLifecyclePort, PgHostStateParticipant};

/// Host SQL for one command type, run on the unit's live transaction.
///
/// The handler returns the typed [`HostStateOutcome`]; the dispatcher builds
/// the [`HostStateReply`], so the reply cannot disagree with `C::Outcome`:
///
/// ```compile_fail
/// # use async_trait::async_trait;
/// # use proxima_core::storage_ports::{HostStateCommand, HostStateOutcome, HostStateParticipantId, HostStateWritePermit, StateSurfaceName};
/// # use proxima_core::{Owner, StorageError};
/// # use proxima_storage_pg::{AgreedCommand, HostStateHandler};
/// # use sqlx::{Postgres, Transaction};
/// # struct Bump { owner: Owner }
/// # impl HostStateCommand for Bump {
/// #     const PARTICIPANT_ID: HostStateParticipantId = HostStateParticipantId::new("example");
/// #     const TABLES: &'static [StateSurfaceName] = &[StateSurfaceName::new("example.counter")];
/// #     type Outcome = u8;
/// #     fn owner(&self) -> Owner { self.owner }
/// # }
/// struct BumpHandler;
///
/// #[async_trait]
/// impl HostStateHandler<Bump> for BumpHandler {
///     async fn handle(
///         &self,
///         _tx: &mut Transaction<'_, Postgres>,
///         _permit: &HostStateWritePermit,
///         _command: AgreedCommand<Bump>,
///     ) -> Result<HostStateOutcome<String>, StorageError> {
///         Ok(HostStateOutcome::Permitted("not a u8".into()))
///     }
/// }
/// ```
///
/// Its parameter is an [`AgreedCommand`], which host code cannot construct, so
/// every command a handler receives had its owners compared with a permit; the
/// dispatcher passes that same permit. It still stamps `permit.owner()` and
/// touches only the tables the command declares
/// (`docs/19-host-state-maintenance.md`).
#[async_trait]
pub trait HostStateHandler<C: HostStateCommand>: Send + Sync + 'static {
    /// Run `command` on `tx`.
    ///
    /// # Errors
    ///
    /// Storage faults from the host-owned statements. Returning `Err` poisons
    /// the unit of work as for any participant error.
    async fn handle(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        permit: &HostStateWritePermit,
        command: AgreedCommand<C>,
    ) -> Result<HostStateOutcome<C::Outcome>, StorageError>;
}

/// A host-state command whose owner and every payload owner equal the owner
/// the engine stamped on the permit (Lean `PayloadAllowed`).
///
/// Only [`PgCommandDispatcher`] makes one, after that comparison succeeded;
/// there is no public constructor, no `Clone` and no `Default`. A
/// [`HostStateHandler`] takes this instead of a bare command, so handler code
/// cannot be called with a command that skipped the comparison:
///
/// ```compile_fail
/// use proxima_storage_pg::AgreedCommand;
///
/// // The field is private; the dispatcher is the only constructor.
/// let _forged = AgreedCommand { command: 7_u8 };
/// ```
#[derive(Debug)]
pub struct AgreedCommand<C> {
    command: C,
}

impl<C: HostStatePayloadOwners> AgreedCommand<C> {
    /// Lean `PayloadAllowed`: the command's owner and every owner its payload
    /// names equal the owner the engine stamped on the permit. The one place
    /// this is checked, and the only way to build the value.
    fn agree(command: C, stamped: &Owner) -> Result<Self, StorageError> {
        if command.owner() == *stamped
            && command
                .payload_owners()
                .iter()
                .all(|owner| owner == stamped)
        {
            Ok(Self { command })
        } else {
            Err(StorageError::ConstraintViolation(
                "host-state command owner does not match write permit".into(),
            ))
        }
    }
}

impl<C> AgreedCommand<C> {
    /// The command, by value.
    #[must_use]
    pub fn into_inner(self) -> C {
        self.command
    }
}

impl<C> Deref for AgreedCommand<C> {
    type Target = C;

    fn deref(&self) -> &C {
        &self.command
    }
}

/// A [`PgCommandDispatcher::register`] call the dispatcher cannot accept.
/// Returned at boot, before any unit of work runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandRegistrationError {
    /// `C::PARTICIPANT_ID` is not this dispatcher's participant.
    #[error(
        "command `{command}` names participant `{}`, but this dispatcher is participant `{}`",
        .found.as_str(),
        .dispatcher.as_str()
    )]
    ForeignParticipant {
        /// Type name of the rejected command.
        command: &'static str,
        /// Participant of the dispatcher.
        dispatcher: HostStateParticipantId,
        /// Participant the command names.
        found: HostStateParticipantId,
    },
    /// `C::TABLES` names a table outside the dispatcher's declared tables.
    #[error(
        "command `{command}` names table `{}`, which participant `{}` does not declare",
        .table.as_str(),
        .dispatcher.as_str()
    )]
    UndeclaredTable {
        /// Type name of the rejected command.
        command: &'static str,
        /// Participant of the dispatcher.
        dispatcher: HostStateParticipantId,
        /// The first table outside the declared set.
        table: StateSurfaceName,
    },
    /// A handler is already registered for this command type.
    #[error("command `{command}` is already registered on participant `{}`", .dispatcher.as_str())]
    DuplicateCommand {
        /// Type name of the rejected command.
        command: &'static str,
        /// Participant of the dispatcher.
        dispatcher: HostStateParticipantId,
    },
}

/// One command type's handler behind a type-erased door.
#[async_trait]
trait Route: Send + Sync {
    fn command_type(&self) -> TypeId;
    fn command_name(&self) -> &'static str;

    /// Take `request` when it carries this route's command type, else hand it
    /// back untouched.
    async fn dispatch(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        permit: &HostStateWritePermit,
        request: HostStateRequest,
    ) -> Result<Routed, StorageError>;
}

enum Routed {
    Handled(HostStateReply),
    Unclaimed(HostStateRequest),
}

struct TypedRoute<C, H> {
    handler: H,
    command: PhantomData<fn(C)>,
}

#[async_trait]
impl<C, H> Route for TypedRoute<C, H>
where
    C: HostStatePayloadOwners,
    H: HostStateHandler<C>,
{
    fn command_type(&self) -> TypeId {
        TypeId::of::<C>()
    }

    fn command_name(&self) -> &'static str {
        std::any::type_name::<C>()
    }

    async fn dispatch(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        permit: &HostStateWritePermit,
        request: HostStateRequest,
    ) -> Result<Routed, StorageError> {
        let command = match request.try_downcast::<C>() {
            Ok(command) => command,
            Err(request) => return Ok(Routed::Unclaimed(request)),
        };
        let command = AgreedCommand::agree(command, permit.owner())?;
        let outcome = self.handler.handle(tx, permit, command).await?;
        Ok(Routed::Handled(reply_for(outcome)))
    }
}

fn reply_for<T: Send + 'static>(outcome: HostStateOutcome<T>) -> HostStateReply {
    match outcome {
        HostStateOutcome::Permitted(value) => HostStateReply::permitted(value),
        HostStateOutcome::AlreadyApplied(value) => HostStateReply::already_applied(value),
        HostStateOutcome::Refused(value) => HostStateReply::refused(value),
    }
}

/// The host's one [`PgHostStateParticipant`], serving several command types.
///
/// Each command type is registered with its [`HostStateHandler`]; `apply`
/// routes a request by its command type, agrees the command's owners with the
/// permit once (see the module doc), and builds the typed reply. The
/// dispatcher adds no per-command authority: scope stays the participant and
/// its declared tables, and a handler still stamps the permit's owner and
/// touches only its command's tables.
///
/// ```
/// use async_trait::async_trait;
/// use proxima_core::storage_ports::{
///     HostStateCommand, HostStateOutcome, HostStateParticipantId, HostStatePayloadOwners,
///     HostStateWritePermit, StateSurfaceName,
/// };
/// use proxima_core::{Owner, StorageError};
/// use proxima_storage_pg::{
///     AgreedCommand, CommandRegistrationError, HostStateHandler, PgCommandDispatcher,
/// };
/// use sqlx::{Postgres, Transaction};
///
/// const PARTICIPANT: HostStateParticipantId = HostStateParticipantId::new("example");
/// const TABLES: &[StateSurfaceName] = &[StateSurfaceName::new("example.counter")];
///
/// struct Bump {
///     owner: Owner,
/// }
///
/// impl HostStateCommand for Bump {
///     const PARTICIPANT_ID: HostStateParticipantId = PARTICIPANT;
///     const TABLES: &'static [StateSurfaceName] = TABLES;
///     type Outcome = u8;
///     fn owner(&self) -> Owner {
///         self.owner
///     }
/// }
///
/// impl HostStatePayloadOwners for Bump {
///     fn payload_owners(&self) -> Vec<Owner> {
///         Vec::new()
///     }
/// }
///
/// struct BumpHandler;
///
/// #[async_trait]
/// impl HostStateHandler<Bump> for BumpHandler {
///     async fn handle(
///         &self,
///         _tx: &mut Transaction<'_, Postgres>,
///         _permit: &HostStateWritePermit,
///         _command: AgreedCommand<Bump>,
///     ) -> Result<HostStateOutcome<u8>, StorageError> {
///         Ok(HostStateOutcome::Permitted(1))
///     }
/// }
///
/// let dispatcher = PgCommandDispatcher::new(PARTICIPANT, TABLES).register::<Bump, _>(BumpHandler)?;
/// # let _ = dispatcher;
/// # Ok::<(), CommandRegistrationError>(())
/// ```
///
/// A command that does not state its payload owners cannot be registered:
///
/// ```compile_fail
/// # use async_trait::async_trait;
/// # use proxima_core::storage_ports::{
/// #     HostStateCommand, HostStateOutcome, HostStateParticipantId, HostStateWritePermit,
/// #     StateSurfaceName,
/// # };
/// # use proxima_core::{Owner, StorageError};
/// # use proxima_storage_pg::{AgreedCommand, HostStateHandler, PgCommandDispatcher};
/// # use sqlx::{Postgres, Transaction};
/// # const PARTICIPANT: HostStateParticipantId = HostStateParticipantId::new("example");
/// # const TABLES: &[StateSurfaceName] = &[StateSurfaceName::new("example.counter")];
/// # struct Bump { owner: Owner }
/// # impl HostStateCommand for Bump {
/// #     const PARTICIPANT_ID: HostStateParticipantId = PARTICIPANT;
/// #     const TABLES: &'static [StateSurfaceName] = TABLES;
/// #     type Outcome = u8;
/// #     fn owner(&self) -> Owner { self.owner }
/// # }
/// # struct BumpHandler;
/// # #[async_trait]
/// # impl HostStateHandler<Bump> for BumpHandler {
/// #     async fn handle(
/// #         &self,
/// #         _tx: &mut Transaction<'_, Postgres>,
/// #         _permit: &HostStateWritePermit,
/// #         _command: AgreedCommand<Bump>,
/// #     ) -> Result<HostStateOutcome<u8>, StorageError> {
/// #         Ok(HostStateOutcome::Permitted(1))
/// #     }
/// # }
/// // `Bump` implements no `HostStatePayloadOwners`.
/// let _ = PgCommandDispatcher::new(PARTICIPANT, TABLES).register::<Bump, _>(BumpHandler);
/// ```
pub struct PgCommandDispatcher {
    participant_id: HostStateParticipantId,
    declared_tables: &'static [StateSurfaceName],
    routes: Vec<Box<dyn Route>>,
    lifecycle: Option<Arc<dyn PgHostStateLifecyclePort>>,
}

impl fmt::Debug for PgCommandDispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PgCommandDispatcher")
            .field("participant_id", &self.participant_id)
            .field("declared_tables", &self.declared_tables)
            .field(
                "commands",
                &self
                    .routes
                    .iter()
                    .map(|route| route.command_name())
                    .collect::<Vec<_>>(),
            )
            .field("lifecycle", &self.lifecycle.is_some())
            .finish()
    }
}

impl PgCommandDispatcher {
    /// A dispatcher for `participant_id` with no command registered yet.
    /// `declared_tables` is the participant's frozen table set
    /// ([`PgHostStateParticipant::declared_tables`]).
    #[must_use]
    pub fn new(
        participant_id: HostStateParticipantId,
        declared_tables: &'static [StateSurfaceName],
    ) -> Self {
        Self {
            participant_id,
            declared_tables,
            routes: Vec::new(),
            lifecycle: None,
        }
    }

    /// Serve command type `C` with `handler`.
    ///
    /// # Errors
    ///
    /// [`CommandRegistrationError`] when `C::PARTICIPANT_ID` is not this
    /// dispatcher's, `C::TABLES` names a table outside the declared tables, or
    /// `C` is already registered.
    pub fn register<C, H>(mut self, handler: H) -> Result<Self, CommandRegistrationError>
    where
        C: HostStatePayloadOwners,
        H: HostStateHandler<C>,
    {
        let command = std::any::type_name::<C>();
        if C::PARTICIPANT_ID != self.participant_id {
            return Err(CommandRegistrationError::ForeignParticipant {
                command,
                dispatcher: self.participant_id,
                found: C::PARTICIPANT_ID,
            });
        }
        if let Some(table) = C::TABLES
            .iter()
            .find(|table| !self.declared_tables.contains(table))
        {
            return Err(CommandRegistrationError::UndeclaredTable {
                command,
                dispatcher: self.participant_id,
                table: *table,
            });
        }
        if self
            .routes
            .iter()
            .any(|route| route.command_type() == TypeId::of::<C>())
        {
            return Err(CommandRegistrationError::DuplicateCommand {
                command,
                dispatcher: self.participant_id,
            });
        }
        self.routes.push(Box::new(TypedRoute {
            handler,
            command: PhantomData,
        }));
        Ok(self)
    }

    /// Attach the participant's erase/export port, as
    /// [`PgHostStateParticipant::lifecycle_port`] returns it.
    #[must_use]
    pub fn with_lifecycle_port(mut self, port: Arc<dyn PgHostStateLifecyclePort>) -> Self {
        self.lifecycle = Some(port);
        self
    }
}

#[async_trait]
impl PgHostStateParticipant for PgCommandDispatcher {
    fn participant_id(&self) -> HostStateParticipantId {
        self.participant_id
    }

    fn declared_tables(&self) -> &'static [StateSurfaceName] {
        self.declared_tables
    }

    fn lifecycle_port(&self) -> Option<Arc<dyn PgHostStateLifecyclePort>> {
        self.lifecycle.clone()
    }

    async fn apply(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        permit: &HostStateWritePermit,
        mut request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        for route in &self.routes {
            match route.dispatch(tx, permit, request).await? {
                Routed::Handled(reply) => return Ok(reply),
                Routed::Unclaimed(unclaimed) => request = unclaimed,
            }
        }
        Err(StorageError::ConstraintViolation(format!(
            "host-state participant {} has no handler registered for this command",
            self.participant_id.as_str()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::{GroupId, UserId};
    use uuid::Uuid;

    const PARTICIPANT: HostStateParticipantId = HostStateParticipantId::new("dispatch_test");
    const TABLE_A: StateSurfaceName = StateSurfaceName::new("dispatch_test.a");
    const TABLE_B: StateSurfaceName = StateSurfaceName::new("dispatch_test.b");
    const DECLARED: &[StateSurfaceName] = &[TABLE_A];

    macro_rules! command {
        ($name:ident, $participant:expr, $tables:expr) => {
            struct $name;
            impl HostStateCommand for $name {
                const PARTICIPANT_ID: HostStateParticipantId = $participant;
                const TABLES: &'static [StateSurfaceName] = $tables;
                type Outcome = ();
                fn owner(&self) -> Owner {
                    Owner::Group(GroupId::new(Uuid::nil()))
                }
            }
            impl HostStatePayloadOwners for $name {
                fn payload_owners(&self) -> Vec<Owner> {
                    Vec::new()
                }
            }
        };
    }

    command!(Declared, PARTICIPANT, &[TABLE_A]);
    command!(OtherDeclared, PARTICIPANT, &[TABLE_A]);
    command!(
        ForeignCommand,
        HostStateParticipantId::new("someone_else"),
        &[TABLE_A]
    );
    command!(OutsideTables, PARTICIPANT, &[TABLE_A, TABLE_B]);

    struct NoopHandler;

    #[async_trait]
    impl<C: HostStateCommand<Outcome = ()>> HostStateHandler<C> for NoopHandler {
        async fn handle(
            &self,
            _tx: &mut Transaction<'_, Postgres>,
            _permit: &HostStateWritePermit,
            _command: AgreedCommand<C>,
        ) -> Result<HostStateOutcome<()>, StorageError> {
            Ok(HostStateOutcome::Permitted(()))
        }
    }

    fn dispatcher() -> PgCommandDispatcher {
        PgCommandDispatcher::new(PARTICIPANT, DECLARED)
    }

    #[test]
    fn distinct_commands_register_and_the_dispatcher_is_the_participant() {
        let dispatcher = dispatcher()
            .register::<Declared, _>(NoopHandler)
            .expect("first command")
            .register::<OtherDeclared, _>(NoopHandler)
            .expect("a second, distinct command");
        assert_eq!(dispatcher.participant_id(), PARTICIPANT);
        assert_eq!(dispatcher.declared_tables(), DECLARED);
        assert!(dispatcher.lifecycle_port().is_none());
        assert_eq!(dispatcher.routes.len(), 2);
    }

    #[test]
    fn a_command_of_another_participant_is_refused_at_registration() {
        let error = dispatcher()
            .register::<ForeignCommand, _>(NoopHandler)
            .expect_err("another participant's command");
        assert!(
            matches!(
                error,
                CommandRegistrationError::ForeignParticipant { dispatcher, found, .. }
                    if dispatcher == PARTICIPANT && found.as_str() == "someone_else"
            ),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("someone_else") && message.contains("dispatch_test"),
            "{message}"
        );
    }

    #[test]
    fn a_table_outside_the_declared_set_is_refused_at_registration() {
        let error = dispatcher()
            .register::<OutsideTables, _>(NoopHandler)
            .expect_err("a table the participant does not declare");
        assert!(
            matches!(
                error,
                CommandRegistrationError::UndeclaredTable { table, .. } if table == TABLE_B
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("dispatch_test.b"), "{error}");
    }

    #[test]
    fn a_second_registration_of_one_command_type_is_refused() {
        let error = dispatcher()
            .register::<Declared, _>(NoopHandler)
            .expect("first registration")
            .register::<Declared, _>(NoopHandler)
            .expect_err("the same command type twice");
        assert!(
            matches!(error, CommandRegistrationError::DuplicateCommand { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("Declared"), "{error}");
    }

    #[derive(Debug)]
    struct Naming {
        owner: Owner,
        payload: Vec<Owner>,
    }

    impl HostStateCommand for Naming {
        const PARTICIPANT_ID: HostStateParticipantId = PARTICIPANT;
        const TABLES: &'static [StateSurfaceName] = &[TABLE_A];
        type Outcome = ();
        fn owner(&self) -> Owner {
            self.owner
        }
    }

    impl HostStatePayloadOwners for Naming {
        fn payload_owners(&self) -> Vec<Owner> {
            self.payload.clone()
        }
    }

    #[test]
    fn the_command_owner_and_every_payload_owner_must_equal_the_stamped_owner() {
        let stamped = Owner::Group(GroupId::new(Uuid::from_u128(1)));
        let foreign = Owner::Personal(UserId::new(Uuid::from_u128(2)));
        let agreeing = |payload: Vec<Owner>| {
            AgreedCommand::agree(
                Naming {
                    owner: stamped,
                    payload,
                },
                &stamped,
            )
        };

        let agreed = agreeing(Vec::new()).expect("a payload naming no owner");
        assert_eq!(agreed.owner(), stamped, "the agreed command derefs to C");
        assert_eq!(agreed.into_inner().owner, stamped);
        assert!(agreeing(vec![stamped, stamped]).is_ok());
        for payload in [
            vec![foreign],
            vec![stamped, foreign],
            vec![foreign, stamped],
        ] {
            let error = agreeing(payload).expect_err("a payload owner differs");
            assert!(
                matches!(&error, StorageError::ConstraintViolation(message)
                    if message == "host-state command owner does not match write permit"),
                "{error:?}"
            );
        }

        let error = AgreedCommand::agree(
            Naming {
                owner: foreign,
                payload: vec![stamped],
            },
            &stamped,
        )
        .expect_err("the command owner differs, whatever the payload names");
        assert!(
            matches!(error, StorageError::ConstraintViolation(_)),
            "{error:?}"
        );
    }
}
