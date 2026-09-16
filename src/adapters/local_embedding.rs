//! Keyless, on-device embeddings for the opt-in `--semantic` pass, backing the
//! [`Embedder`] port with a local ONNX model via `fastembed`. No network call at
//! query time and no API key: the model weights download once to the fastembed
//! cache on first use, then every embedding runs on the CPU. This is the
//! alternative to [`crate::adapters::openai_compatible::EmbeddingClient`] for
//! users without an OpenAI-compatible `/embeddings` provider. Compiled only
//! under the `local-embed` feature, since it pulls the ONNX Runtime.

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::application::ports::Embedder;

/// A locally executing sentence-embedding model, loaded once and reused for
/// every batch. `fastembed` fetches the weights into its own on-disk cache the
/// first time a given model is requested, so construction may hit the network
/// once; subsequent runs are fully offline.
pub struct LocalEmbedder {
    model: TextEmbedding,
}

impl LocalEmbedder {
    /// Loads the named model. `model_name` is the short config value (e.g.
    /// `bge-small-en-v1.5`); an unrecognized name fails with the supported set
    /// rather than silently substituting a different model.
    pub fn new(model_name: &str) -> Result<Self> {
        let embedding_model = resolve_model(model_name)?;
        let model = TextEmbedding::try_new(
            InitOptions::new(embedding_model).with_show_download_progress(false),
        )
        .with_context(|| format!("failed to load local embedding model {model_name:?}"))?;
        Ok(Self { model })
    }
}

impl Embedder for LocalEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // fastembed batches internally; passing `None` lets it pick a batch size.
        self.model
            .embed(texts.to_vec(), None)
            .context("local embedding computation failed")
    }
}

/// Maps a short, provider-neutral model name to a fastembed model. The default
/// (`bge-small-en-v1.5`) is a small, fast English model that fits a modest
/// on-device budget; larger variants trade size for recall.
fn resolve_model(name: &str) -> Result<EmbeddingModel> {
    match name.trim().to_ascii_lowercase().as_str() {
        // The default when no `embeddingModel` is set for the local provider.
        "" | "bge-small-en-v1.5" | "bge-small" => Ok(EmbeddingModel::BGESmallENV15),
        "bge-base-en-v1.5" | "bge-base" => Ok(EmbeddingModel::BGEBaseENV15),
        "bge-large-en-v1.5" | "bge-large" => Ok(EmbeddingModel::BGELargeENV15),
        "all-minilm-l6-v2" | "all-minilm" => Ok(EmbeddingModel::AllMiniLML6V2),
        "nomic-embed-text-v1.5" | "nomic" => Ok(EmbeddingModel::NomicEmbedTextV15),
        other => anyhow::bail!(
            "unknown local embeddingModel {other:?}; supported: bge-small-en-v1.5, \
             bge-base-en-v1.5, bge-large-en-v1.5, all-minilm-l6-v2, nomic-embed-text-v1.5"
        ),
    }
}
