//! Embedding: wraps `fastembed` (nomic-embed-text-v1.5, ONNX/CPU). One
//! `Embedder` holds a resident model; construction pays the ~1.6 s cold-load
//! once. nomic-v1.5 is an asymmetric model — documents and queries get
//! different task prefixes.

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

// fastembed 4.x does not auto-apply nomic task prefixes — we add them here.
const DOC_PREFIX: &str = "search_document: ";
const QUERY_PREFIX: &str = "search_query: ";

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Load the model. On first ever run fastembed downloads the ONNX weights
    /// (~hundreds of MB) into its cache; afterwards this is a local load.
    pub fn new() -> anyhow::Result<Self> {
        let model = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::NomicEmbedTextV15))?;
        Ok(Self { model })
    }

    /// Embed corpus chunks (document side).
    pub fn embed_documents(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{DOC_PREFIX}{t}")).collect();
        Ok(self.model.embed(prefixed, None)?)
    }

    /// Embed a single search query (query side).
    pub fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut out = self.model.embed(vec![format!("{QUERY_PREFIX}{text}")], None)?;
        out.pop()
            .ok_or_else(|| anyhow::anyhow!("fastembed returned no embedding for query"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Model-dependent: requires the nomic ONNX weights. Run explicitly with
    // `cargo test -- --ignored`. CI / the nightly eval also exercise this path.
    #[test]
    #[ignore]
    fn embeds_at_768_dimensions() {
        let e = Embedder::new().unwrap();
        let q = e.embed_query("what did we decide about the memory schema").unwrap();
        assert_eq!(q.len(), super::super::vector::EMBED_DIM);

        let docs = e
            .embed_documents(&["hex is an AI operating layer".to_string()])
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].len(), super::super::vector::EMBED_DIM);
    }
}
