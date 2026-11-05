//! Host-only retirement joins the existing owner-fenced series inverse.

use super::Engine;
use super::unit_of_work::own_memory_schemas;
use crate::authz::SystemAuthority;
use crate::error::ProtocolError;
use crate::verbs::own_erase::{
    EraseMode, SeriesEraseError, SeriesEraseReceipt, SeriesEraseSelection,
};
use crate::{OwnerId, SchemaId, SpiiceContract};

/// A retired target resolved from this engine's frozen composition under its
/// boot-bound system authority. It carries no payload reader or writer.
pub struct RetiredFactErase<'a> {
    engine: &'a Engine,
    authority: &'a SystemAuthority,
    contract: &'static SpiiceContract,
    schema: SchemaId,
}

impl std::fmt::Debug for RetiredFactErase<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetiredFactErase")
            .field("spiice_id", &self.contract.spiice_id)
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Resolve a declared retired Fact for host maintenance. Active, unknown,
    /// core and foreign schemas grant no retirement capability.
    ///
    /// # Errors
    /// A foreign boot authority is forbidden; a target absent from the named
    /// spiice's frozen retired declaration is invalid.
    pub fn retired_fact_erase<'a>(
        &'a self,
        authority: &'a SystemAuthority,
        spiice_id: &str,
        schema: &SchemaId,
    ) -> Result<RetiredFactErase<'a>, ProtocolError> {
        // Use the same boot-authority boundary as every system erase.
        self.system_unit_of_work(authority)?;
        let contract = self
            .registry()
            .spiice_contract(spiice_id)
            .filter(|contract| {
                contract
                    .retired_facts
                    .iter()
                    .any(|retired| retired.id.render() == schema.as_str())
            })
            .ok_or_else(|| {
                ProtocolError::invalid_argument(
                    "schema",
                    "target is not a declared retired Fact of this spiice",
                )
            })?;
        Ok(RetiredFactErase {
            engine: self,
            authority,
            contract,
            schema: schema.clone(),
        })
    }
}

impl RetiredFactErase<'_> {
    /// Next at most `MAX_ERASE_SERIES_PER_CALL` distinct owners after `after`,
    /// ordered by owner id. Drain each owner's pages before advancing the cursor.
    /// This inventory uses raw admissions, independently of retention settings.
    ///
    /// # Errors
    /// Storage failures, including backends without a platform inventory.
    pub async fn owners(&self, after: Option<OwnerId>) -> Result<Vec<OwnerId>, ProtocolError> {
        self.engine
            .storage()
            .write_session
            .retired_fact_owners(&self.schema, after)
            .await
            .map_err(|err| ProtocolError::internal(format!("retired Fact inventory: {err}")))
    }

    /// Erase and commit one owner's next bounded page through the existing
    /// series closure, lifecycle callbacks, vector inverse and cold-object debt.
    /// Active own schemas reached by references retain their ordinary scope;
    /// unknown/foreign schemas and cross-owner closures are refused.
    ///
    /// # Errors
    /// The same scope, size, transaction and storage failures as a series erase.
    pub async fn erase_page(&self, owner: OwnerId) -> Result<SeriesEraseReceipt, SeriesEraseError> {
        let mut unit = self.engine.system_unit_of_work(self.authority)?;
        let mut schemas = own_memory_schemas(self.contract);
        schemas.extend(
            self.contract
                .retired_facts
                .iter()
                .map(|retired| retired.id.render()),
        );
        let receipt = unit
            .erase_series_page(
                self.contract.spiice_id,
                owner,
                &schemas,
                SeriesEraseSelection::RetiredSchema(&self.schema),
                None,
                EraseMode::Erase,
            )
            .await?;
        unit.commit().await?;
        Ok(receipt)
    }
}
