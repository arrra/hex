//! Embedding: wraps `fastembed` (nomic-embed-text-v1.5, ONNX/CPU). One
//! `Embedder` holds a resident model; construction pays the ~1.6 s cold-load
//! once. nomic-v1.5 is an asymmetric model — documents and queries get
//! different task prefixes.

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

// fastembed 4.x does not auto-apply nomic task prefixes — we add them here.
const DOC_PREFIX: &str = "search_document: ";
const QUERY_PREFIX: &str = "search_query: ";

/// Read current resident set size (RSS) in MB on Linux via /proc/self/statm.
/// Returns None on non-Linux or read failure. Used by OBS-019 diagnosis to
/// pinpoint where memory blows up during indexing.
pub fn rss_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // Page size on Linux is typically 4096; query via sysconf if available.
        let page_size: u64 = 4096;
        Some(resident_pages * page_size / (1024 * 1024))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Emit an RSS checkpoint to stderr. Cheap; safe to leave wired in.
pub fn log_rss(label: &str) {
    match rss_mb() {
        Some(mb) => eprintln!("[rss] {label}: {mb} MB"),
        None => eprintln!("[rss] {label}: (unavailable on this platform)"),
    }
}

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Load the model. On first ever run fastembed downloads the ONNX weights
    /// (~hundreds of MB) into its cache; afterwards this is a local load.
    ///
    /// OBS-019 fix: cap ONNX Runtime intra/inter-op threads to 1 BEFORE
    /// loading the model. fastembed 4.x's InitOptions doesn't expose ORT
    /// session options, so we set the env vars ort reads at module init.
    /// Without this cap, ort defaults to `num_cpus`, and a multi-thread
    /// embed of a batch of N chunks allocates N × per-thread × per-layer
    /// activation tensors — easily 2+ GB on the 70-chunk CLAUDE.md batch,
    /// which OOM-killed the process in the 4 GB Docker test container.
    /// See evolution/obs-019-diagnosis.md for the full RSS trace.
    pub fn new() -> anyhow::Result<Self> {
        std::env::set_var("ORT_NUM_THREADS", "1");
        std::env::set_var("OMP_NUM_THREADS", "1");
        log_rss("embedder pre-new");
        let model = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::NomicEmbedTextV15))?;
        log_rss("embedder post-new");
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
