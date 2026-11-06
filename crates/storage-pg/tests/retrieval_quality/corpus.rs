//! Authored, non-sensitive fixture facts and independently phrased queries.
//! These judgments exercise retrieval evaluation, not production quality.

pub struct Note {
    pub key: &'static str,
    pub tag: &'static str,
    pub text: &'static str,
}

pub static NOTES: [[Note; 12]; 2] = [
    [
        Note {
            key: "invoice",
            tag: "finance",
            text: "Supplier invoices require a matching purchase order before payment. Put the invoice number on the payment reference.",
        },
        Note {
            key: "purchase",
            tag: "finance",
            text: "Every purchase order needs a budget code and the buyer's approval. Attach the signed order to the supplier payment request.",
        },
        Note {
            key: "archive",
            tag: "finance",
            text: "Store settled invoices with their payment confirmations in the annual finance archive. Draft quotations belong in a separate folder.",
        },
        Note {
            key: "deploy",
            tag: "ops",
            text: "Roll back a release when the health probe fails twice. Restore the previous image and notify the on-call engineer.",
        },
        Note {
            key: "incident",
            tag: "ops",
            text: "During a storage outage, pause writes and contact the incident coordinator. Recover from the latest verified database snapshot.",
        },
        Note {
            key: "backup",
            tag: "ops",
            text: "The recovery exercise restores the database snapshot and its matching objects. Validate the restored records before reopening traffic.",
        },
        Note {
            key: "access",
            tag: "security",
            text: "The security coordinator reviews shared vault membership every month. Remove expired temporary access during that review.",
        },
        Note {
            key: "trip",
            tag: "personal",
            text: "Keep the train reservation in the travel folder and download an offline ticket before leaving home.",
        },
        Note {
            key: "garden",
            tag: "personal",
            text: "Water the balcony tomatoes on Saturday morning. The basil needs shade in the afternoon.",
        },
        Note {
            key: "coffee",
            tag: "personal",
            text: "Weekend visitors prefer filter coffee and oat milk. Keep a pot of herbal tea available too.",
        },
        Note {
            key: "meeting",
            tag: "work",
            text: "The weekly planning meeting moves to Tuesday at ten. Bring a short list of blocked tasks and decisions needed.",
        },
        Note {
            key: "battery",
            tag: "device",
            text: "The wireless keyboard has a fresh battery. The spare camera battery is kept in the blue equipment case.",
        },
    ],
    [
        Note {
            key: "invoice",
            tag: "finance",
            text: "After an invoice is paid, archive the supplier document together with the bank confirmation. Keep its purchase order attached.",
        },
        Note {
            key: "purchase",
            tag: "finance",
            text: "New purchase requests require an approved cost estimate. Requests above the team budget go to the finance coordinator.",
        },
        Note {
            key: "archive",
            tag: "finance",
            text: "A settled invoice archive contains the original invoice, matching order and proof of payment. Name files with the invoice reference.",
        },
        Note {
            key: "deploy",
            tag: "ops",
            text: "The release engineer is on call for deployment failures. Use the rollback checklist and preserve diagnostic logs.",
        },
        Note {
            key: "incident",
            tag: "ops",
            text: "For a broken deployment, page the release engineer first. The incident coordinator handles communication with affected teams.",
        },
        Note {
            key: "backup",
            tag: "ops",
            text: "Allow forty minutes for the restore rehearsal. Check the database and object store together before declaring recovery complete.",
        },
        Note {
            key: "access",
            tag: "security",
            text: "Temporary shared folder membership expires after fourteen days. A team administrator must approve renewal.",
        },
        Note {
            key: "trip",
            tag: "personal",
            text: "The hiking trip starts at the north station on Sunday. Pack waterproof shoes and a paper route map.",
        },
        Note {
            key: "garden",
            tag: "personal",
            text: "Plant the autumn bulbs beside the garden gate. Leave space for the spring herbs.",
        },
        Note {
            key: "coffee",
            tag: "personal",
            text: "Serve decaffeinated coffee and ginger tea to the weekend guests. Buy oat milk before Friday evening.",
        },
        Note {
            key: "meeting",
            tag: "work",
            text: "The design meeting is on Thursday afternoon. Review the accessibility findings before joining.",
        },
        Note {
            key: "battery",
            tag: "device",
            text: "The portable barcode scanner needs a replacement battery. The keyboard and camera batteries are already charged.",
        },
    ],
];

pub struct Query {
    pub owner: usize,
    pub text: &'static str,
    pub tag: &'static str,
    pub relevant: &'static [&'static str],
}

pub const QUERIES: [Query; 12] = [
    Query {
        owner: 0,
        text: "Which documents are required before paying a supplier?",
        tag: "finance",
        relevant: &["invoice", "purchase"],
    },
    Query {
        owner: 0,
        text: "When may we roll back a faulty release?",
        tag: "ops",
        relevant: &["deploy"],
    },
    Query {
        owner: 0,
        text: "How should we recover after a storage outage?",
        tag: "ops",
        relevant: &["incident", "backup"],
    },
    Query {
        owner: 0,
        text: "Who reviews access to the shared vault?",
        tag: "security",
        relevant: &["access"],
    },
    Query {
        owner: 0,
        text: "Where should I keep the train reservation?",
        tag: "personal",
        relevant: &["trip"],
    },
    Query {
        owner: 0,
        text: "What was agreed about the weekly planning meeting?",
        tag: "work",
        relevant: &["meeting"],
    },
    Query {
        owner: 1,
        text: "What paperwork belongs in a settled invoice archive?",
        tag: "finance",
        relevant: &["invoice", "archive"],
    },
    Query {
        owner: 1,
        text: "How long does a restore rehearsal take?",
        tag: "ops",
        relevant: &["backup"],
    },
    Query {
        owner: 1,
        text: "Who is on call for a broken deployment?",
        tag: "ops",
        relevant: &["incident", "deploy"],
    },
    Query {
        owner: 1,
        text: "When does shared folder access expire?",
        tag: "security",
        relevant: &["access"],
    },
    Query {
        owner: 1,
        text: "What drinks are planned for weekend guests?",
        tag: "personal",
        relevant: &["coffee"],
    },
    Query {
        owner: 1,
        text: "Which device needs a replacement battery?",
        tag: "device",
        relevant: &["battery"],
    },
];
