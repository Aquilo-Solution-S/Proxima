//! How a search query is worded before it is embedded.
//!
//! Instruction-tuned embedding models embed a document as it is and a query
//! inside a task instruction. The instruction belongs to the model, so it
//! rides on the [`BoundEmbeddingClient`] a route carries, and only
//! [`BoundEmbeddingClient::embed_query`] applies it. Stored content is
//! embedded through the client itself and never sees one, so setting or
//! changing an instruction re-embeds nothing.
//!
//! [`BoundEmbeddingClient`]: super::BoundEmbeddingClient
//! [`BoundEmbeddingClient::embed_query`]: super::BoundEmbeddingClient::embed_query

use std::collections::BTreeMap;

/// The placeholder a [`QueryInstruction`] replaces with the query.
pub const QUERY_PLACEHOLDER: &str = "{query}";

/// What a search is looking for, so a route can word the query for it.
///
/// Substrate searches embed under [`QueryTask::DEFAULT`]; a flavor names its
/// own task, which falls back to the default instruction when the route has
/// none for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryTask(Option<&'static str>);

impl QueryTask {
    /// A search with no task of its own.
    pub const DEFAULT: Self = Self(None);

    /// A search a flavor names, e.g. code search.
    #[must_use]
    pub const fn named(name: &'static str) -> Self {
        Self(Some(name))
    }

    /// The task's name; `None` for [`Self::DEFAULT`].
    #[must_use]
    pub const fn name(self) -> Option<&'static str> {
        self.0
    }
}

/// A query wrapped in a model's task instruction, as a template holding
/// [`QUERY_PLACEHOLDER`], e.g.
/// `Instruct: Given a question about a code repository, retrieve the code snippet that answers it\nQuery:{query}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryInstruction(String);

impl QueryInstruction {
    /// # Errors
    ///
    /// [`QueryInstructionError`] when `template` has no [`QUERY_PLACEHOLDER`]:
    /// every query would embed to the same vector.
    pub fn new(template: impl Into<String>) -> Result<Self, QueryInstructionError> {
        let template = template.into();
        if template.contains(QUERY_PLACEHOLDER) {
            Ok(Self(template))
        } else {
            Err(QueryInstructionError { template })
        }
    }

    #[must_use]
    pub fn template(&self) -> &str {
        &self.0
    }

    /// The template with every placeholder replaced by `query`.
    #[must_use]
    pub fn render(&self, query: &str) -> String {
        self.0.replace(QUERY_PLACEHOLDER, query)
    }
}

/// A query instruction template without a [`QUERY_PLACEHOLDER`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("query instruction {template:?} has no {{query}} placeholder")]
pub struct QueryInstructionError {
    pub template: String,
}

/// A hybrid search's weight on its semantic ranking, in `0.0..=1.0`; the
/// lexical ranking gets the complement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticWeight(f32);

impl SemanticWeight {
    /// Both rankings weighed alike.
    pub const EVEN: Self = Self(0.5);

    /// # Errors
    ///
    /// [`SemanticWeightError`] when `weight` is not a finite value in
    /// `0.0..=1.0`.
    pub fn new(weight: f32) -> Result<Self, SemanticWeightError> {
        if weight.is_finite() && (0.0..=1.0).contains(&weight) {
            Ok(Self(weight))
        } else {
            Err(SemanticWeightError)
        }
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// A semantic weight outside `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("semantic_weight must be within 0.0..=1.0")]
pub struct SemanticWeightError;

/// The instructions a bound client wraps search queries in: one per named
/// [`QueryTask`], over a default. Empty embeds every query as sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryInstructions {
    default: Option<QueryInstruction>,
    by_task: BTreeMap<&'static str, QueryInstruction>,
}

impl QueryInstructions {
    /// Word `task`'s queries by `instruction`; [`QueryTask::DEFAULT`] sets the
    /// fallback every task without its own instruction uses.
    #[must_use]
    pub fn with_instruction(mut self, task: QueryTask, instruction: QueryInstruction) -> Self {
        match task.name() {
            Some(name) => {
                self.by_task.insert(name, instruction);
            }
            None => self.default = Some(instruction),
        }
        self
    }

    /// The instruction `task`'s queries are wrapped in: its own, else the
    /// default, else none.
    #[must_use]
    pub fn for_task(&self, task: QueryTask) -> Option<&QueryInstruction> {
        task.name()
            .and_then(|name| self.by_task.get(name))
            .or(self.default.as_ref())
    }

    /// `query` as `task`'s embedding input.
    #[must_use]
    pub fn render(&self, query: &str, task: QueryTask) -> String {
        self.for_task(task)
            .map_or_else(|| query.to_owned(), |instruction| instruction.render(query))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.by_task.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{QueryInstruction, QueryInstructions, QueryTask};

    const CODE: QueryTask = QueryTask::named("code");

    fn instruction(template: &str) -> QueryInstruction {
        QueryInstruction::new(template).expect("template holds the placeholder")
    }

    #[test]
    fn a_template_needs_the_placeholder_and_renders_every_occurrence() {
        let err = QueryInstruction::new("Instruct: find code").expect_err("no placeholder");
        assert!(err.to_string().contains("{query}"), "{err}");
        assert_eq!(
            instruction("Q:{query}|{query}").render("parse args"),
            "Q:parse args|parse args"
        );
    }

    #[test]
    fn a_task_uses_its_own_instruction_then_the_default_then_none() {
        let none = QueryInstructions::default();
        assert!(none.is_empty());
        assert_eq!(none.render("q", QueryTask::DEFAULT), "q");
        assert_eq!(none.render("q", CODE), "q");

        let code_only =
            QueryInstructions::default().with_instruction(CODE, instruction("code: {query}"));
        assert_eq!(code_only.render("q", CODE), "code: q");
        assert_eq!(code_only.render("q", QueryTask::DEFAULT), "q");

        let both = code_only.with_instruction(QueryTask::DEFAULT, instruction("any: {query}"));
        assert_eq!(both.render("q", CODE), "code: q");
        assert_eq!(both.render("q", QueryTask::DEFAULT), "any: q");
        assert_eq!(both.render("q", QueryTask::named("other")), "any: q");
    }
}
