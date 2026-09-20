/-
Causa — the Proxima kernel (umbrella re-export).

Load order is a DAG: Prelude < Owner < Identity < Memory < Knowledge
< Goals < Edges (pins; no Edge table) < Authorization < HostStateMaintenance
< HostStateWriteAttribution < EdgeAuthorization < Operators < Provenance
< Wake < Citations < Compliance < HostStateLifecycle
< Principles < Flavor < Publication (the Fact outbox — imports Flavor)
< PublicationOrigin < HostStateFactCopyErase. HostStateWriteAttribution imports
HostStateMaintenance and does not alter its authorization relation. The final
copy-inverse layer carries the identity projection between lifecycle and
publication-origin source wrappers over Proxima's native SourceId token.

v0.0.8: Memory/Goal are (handle, t); origins/refs pin t; no FactEntity.
Content is an owner-scoped payload sort; Self is a cue-indexed query.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Knowledge
import Causa.Goals
import Causa.Edges
import Causa.Authorization
import Causa.HostStateMaintenance
import Causa.HostStateMaintenanceAudit
import Causa.HostStateWriteAttribution
import Causa.HostStateWriteAttributionAudit
import Causa.EdgeAuthorization
import Causa.Operators
import Causa.Provenance
import Causa.Wake
import Causa.Citations
import Causa.Compliance
import Causa.HostStateLifecycle
import Causa.HostStateLifecycleAudit
import Causa.Principles
import Causa.Flavor
import Causa.Publication
import Causa.PublicationOrigin
import Causa.PublicationOriginAudit
import Causa.HostStateFactCopyErase
import Causa.HostStateFactCopyEraseAudit
