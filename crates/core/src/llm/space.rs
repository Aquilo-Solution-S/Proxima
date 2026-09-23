//! Where vectors live: the widths the store indexes, the space a client
//! embeds into, and vectors checked against it.

use super::{EmbeddingClient, LlmError};

/// Declares [`EmbeddingDim`] and its width codec from one list, so a width
/// cannot be added to the enum and missed by [`EmbeddingDim::ALL`],
/// [`EmbeddingDim::from_width`] or the refusal message.
macro_rules! embedding_dims {
    ($($variant:ident = $width:literal),+ $(,)?) => {
        /// A vector width the store can index.
        ///
        /// Closed on purpose. pgvector indexes one width per HNSW index, so
        /// the store keeps one partial index per width over a single
        /// `embeddings` table. A width outside this set has no index to be
        /// searched through; it is refused where a client is bound
        /// ([`BoundEmbeddingClient::bind`]), not discovered on the first
        /// write. Widths above pgvector's 2,000-dimension HNSW cap for
        /// `vector` are indexed as `halfvec`; the stored vector keeps full
        /// precision.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum EmbeddingDim {
            $($variant),+
        }

        impl EmbeddingDim {
            /// Every supported width, narrowest first.
            pub const ALL: [Self; [$($width),+].len()] = [$(Self::$variant),+];

            /// Number of components in a vector of this width.
            #[must_use]
            pub const fn width(self) -> usize {
                match self {
                    $(Self::$variant => $width),+
                }
            }

            /// The supported width with exactly `width` components, if any.
            #[must_use]
            pub const fn from_width(width: usize) -> Option<Self> {
                match width {
                    $($width => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

embedding_dims! {
    D384 = 384,
    D768 = 768,
    D1024 = 1024,
    D1536 = 1536,
    D2048 = 2048,
    D3072 = 3072,
}

impl TryFrom<usize> for EmbeddingDim {
    type Error = UnsupportedEmbeddingWidth;

    fn try_from(width: usize) -> Result<Self, Self::Error> {
        Self::from_width(width).ok_or(UnsupportedEmbeddingWidth { width })
    }
}

impl std::fmt::Display for EmbeddingDim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.width())
    }
}

/// A client width no [`EmbeddingDim`] matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedEmbeddingWidth {
    pub width: usize,
}

impl std::fmt::Display for UnsupportedEmbeddingWidth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "embedding width {} is not supported; the store indexes",
            self.width
        )?;
        for (index, dim) in EmbeddingDim::ALL.iter().enumerate() {
            let sep = if index == 0 { " " } else { ", " };
            write!(f, "{sep}{dim}")?;
        }
        f.write_str(" (a Matryoshka model can request one of these)")
    }
}

impl std::error::Error for UnsupportedEmbeddingWidth {}

/// Where vectors live: the model that produced them and their width.
///
/// Vectors are comparable only within one space. The width is part of the
/// identity, so the same model re-embedded at another Matryoshka width is a
/// different space, not a collision.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmbeddingSpace {
    model_id: String,
    dim: EmbeddingDim,
}

impl EmbeddingSpace {
    #[must_use]
    pub fn new(model_id: impl Into<String>, dim: EmbeddingDim) -> Self {
        Self {
            model_id: model_id.into(),
            dim,
        }
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub const fn dim(&self) -> EmbeddingDim {
        self.dim
    }
}

impl std::fmt::Display for EmbeddingSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.model_id, self.dim)
    }
}

/// A vector checked against the [`EmbeddingSpace`] it lives in.
///
/// [`Self::new`] is the only way to make one and compares the width once,
/// so a write or a search takes space and vector as one value and nothing
/// downstream checks the pair again.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceVector {
    space: EmbeddingSpace,
    values: Vec<f32>,
}

impl SpaceVector {
    /// # Errors
    ///
    /// [`VectorWidthMismatch`] when `values` is not `space`'s width.
    pub fn new(space: EmbeddingSpace, values: Vec<f32>) -> Result<Self, VectorWidthMismatch> {
        if values.len() != space.dim().width() {
            return Err(VectorWidthMismatch {
                space,
                got: values.len(),
            });
        }
        Ok(Self { space, values })
    }

    #[must_use]
    pub const fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

/// A vector whose length is not its space's width.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("embedding dim mismatch: {space} needs {} components, got {got}", .space.dim())]
pub struct VectorWidthMismatch {
    pub space: EmbeddingSpace,
    pub got: usize,
}

/// An embedding client bound to the [`EmbeddingSpace`] its vectors live in.
///
/// The engine installs only bound clients. Binding checks the client's
/// width once, so every downstream write and query names a width the store
/// indexes. Dereferences to the client.
#[derive(Debug, Clone)]
pub struct BoundEmbeddingClient {
    client: std::sync::Arc<dyn EmbeddingClient>,
    space: EmbeddingSpace,
    /// The client as the host bound it, kept through engine wrapping so two
    /// bindings of one host client still compare as the same endpoint.
    origin: std::sync::Arc<dyn EmbeddingClient>,
}

impl BoundEmbeddingClient {
    /// Bind `client` to its space.
    ///
    /// # Errors
    ///
    /// [`UnsupportedEmbeddingWidth`] when `client.dim()` is not an
    /// [`EmbeddingDim`].
    pub fn bind(
        client: std::sync::Arc<dyn EmbeddingClient>,
    ) -> Result<Self, UnsupportedEmbeddingWidth> {
        let dim = EmbeddingDim::try_from(client.dim())?;
        let space = EmbeddingSpace::new(client.model_id(), dim);
        Ok(Self {
            origin: std::sync::Arc::clone(&client),
            client,
            space,
        })
    }

    #[must_use]
    pub const fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    /// `values`, produced by this client, as a vector in its space.
    ///
    /// # Errors
    ///
    /// [`VectorWidthMismatch`] when the client returned a vector of another
    /// width than it declared.
    pub fn vector(&self, values: Vec<f32>) -> Result<SpaceVector, VectorWidthMismatch> {
        SpaceVector::new(self.space.clone(), values)
    }

    /// Embed `text` in this client's space.
    ///
    /// # Errors
    ///
    /// The client's [`LlmError`]; [`LlmError::Embed`] when the vector is not
    /// the client's declared width.
    pub async fn embed_vector(&self, text: &str) -> Result<SpaceVector, LlmError> {
        let values = self.client.embed(text).await?;
        self.vector(values)
            .map_err(|err| LlmError::Embed(err.to_string()))
    }

    /// The same binding with its client replaced by `wrap(client)`. Space
    /// and origin are kept, so a wrapper cannot move the binding.
    pub(super) fn wrapped(
        self,
        wrap: impl Fn(std::sync::Arc<dyn EmbeddingClient>) -> std::sync::Arc<dyn EmbeddingClient>,
    ) -> Self {
        Self {
            client: wrap(self.client),
            ..self
        }
    }

    /// Whether both bindings serve the same host client, so one embedding
    /// of a text may stand for both.
    #[must_use]
    pub fn same_client(&self, other: &Self) -> bool {
        std::ptr::addr_eq(
            std::sync::Arc::as_ptr(&self.origin),
            std::sync::Arc::as_ptr(&other.origin),
        )
    }
}

impl AsRef<dyn EmbeddingClient> for BoundEmbeddingClient {
    fn as_ref(&self) -> &(dyn EmbeddingClient + 'static) {
        self.client.as_ref()
    }
}

impl std::ops::Deref for BoundEmbeddingClient {
    type Target = dyn EmbeddingClient;

    fn deref(&self) -> &Self::Target {
        self.client.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::super::LlmError;
    use super::EmbeddingClient;
    use async_trait::async_trait;

    #[test]
    fn every_width_round_trips_and_only_supported_widths_bind() {
        for dim in super::EmbeddingDim::ALL {
            assert_eq!(super::EmbeddingDim::try_from(dim.width()), Ok(dim));
        }
        for width in [0, 4, 383, 1023, 1025, 4096] {
            assert_eq!(
                super::EmbeddingDim::try_from(width),
                Err(super::UnsupportedEmbeddingWidth { width })
            );
        }
    }

    #[test]
    fn binding_refuses_an_unsupported_width_and_records_the_space() {
        #[derive(Debug)]
        struct Width(usize);

        #[async_trait]
        impl EmbeddingClient for Width {
            async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
                Ok(vec![0.0; self.0])
            }

            fn model_id(&self) -> &'static str {
                "m"
            }

            fn dim(&self) -> usize {
                self.0
            }
        }

        let bound = super::BoundEmbeddingClient::bind(std::sync::Arc::new(Width(768)))
            .expect("768 is a lane");
        assert_eq!(
            bound.space(),
            &super::EmbeddingSpace::new("m", super::EmbeddingDim::D768)
        );
        assert_eq!(
            super::BoundEmbeddingClient::bind(std::sync::Arc::new(Width(1000))).unwrap_err(),
            super::UnsupportedEmbeddingWidth { width: 1000 }
        );
    }

    #[test]
    fn a_space_vector_has_its_space_width() {
        let space = super::EmbeddingSpace::new("m", super::EmbeddingDim::D384);
        let vector = super::SpaceVector::new(space.clone(), vec![0.0; 384]).expect("384 wide");
        assert_eq!((vector.space(), vector.values().len()), (&space, 384));
        assert_eq!(
            super::SpaceVector::new(space.clone(), vec![0.0; 383]),
            Err(super::VectorWidthMismatch { space, got: 383 })
        );
    }
}
