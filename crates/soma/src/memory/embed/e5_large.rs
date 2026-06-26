//! `intfloat/multilingual-e5-large` (1024d, ~94 languages).
//! Studio profile primary embedder — D69 v1.1 candidate.
//!
//! Wired only behind `embed-onnx` (fastembed dep). v1.1 D69
//! activates this when `profile::detect()` returns Studio AND the
//! `cognitive`/`embed-onnx` features are on. v1.2 chunk 2+ will
//! optionally upgrade to a trainable head, but the inference
//! shape matches what `OnnxEmbedder` does for the Mini profile.
//!
//! Cache override matches the Mini path — `~/.soma/models/<id>/`.

use crate::memory::embed::Embedder;

pub struct E5LargeEmbedder {
    #[cfg(feature = "embed-onnx")]
    inner: std::sync::Mutex<fastembed::TextEmbedding>,

    #[cfg(not(feature = "embed-onnx"))]
    _phantom: std::marker::PhantomData<()>,
}

#[derive(Debug)]
pub enum E5Error {
    FeatureOff,
    Init(String),
}

impl std::fmt::Display for E5Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            E5Error::FeatureOff => {
                write!(f, "embed-onnx feature is disabled; rebuild with `--features embed-onnx`")
            }
            E5Error::Init(m) => write!(f, "e5-large ONNX init failed: {m}"),
        }
    }
}

impl std::error::Error for E5Error {}

impl E5LargeEmbedder {
    /// Stable id for `episode_vectors.model_id`. Disjoint from
    /// `OnnxEmbedder::MODEL_ID` so the per-model storage row is
    /// always unambiguous (D69 dual-model coexistence).
    pub const MODEL_ID: &'static str = "multilingual-e5-large-1024d";
    pub const DIM: usize = 1024;

    pub fn cache_dir() -> Result<std::path::PathBuf, E5Error> {
        let home = dirs::home_dir().ok_or_else(|| E5Error::Init("home dir unresolvable".into()))?;
        Ok(home.join(".soma").join("models").join(Self::MODEL_ID))
    }

    pub fn new() -> Result<Self, E5Error> {
        #[cfg(feature = "embed-onnx")]
        {
            use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
            let cache = Self::cache_dir()?;
            std::fs::create_dir_all(&cache)
                .map_err(|e| E5Error::Init(format!("mkdir {}: {e}", cache.display())))?;
            let opts = InitOptions::new(EmbeddingModel::MultilingualE5Large).with_cache_dir(cache);
            let inner = TextEmbedding::try_new(opts)
                .map_err(|e| E5Error::Init(format!("fastembed::try_new: {e}")))?;
            Ok(Self { inner: std::sync::Mutex::new(inner) })
        }
        #[cfg(not(feature = "embed-onnx"))]
        {
            Err(E5Error::FeatureOff)
        }
    }

    /// `soma install --model multilingual-e5-large` backend.
    pub fn ensure_downloaded() -> Result<std::path::PathBuf, E5Error> {
        let cache = Self::cache_dir()?;
        let _e = Self::new()?;
        Ok(cache)
    }
}

impl Embedder for E5LargeEmbedder {
    fn model_id(&self) -> &'static str {
        Self::MODEL_ID
    }

    fn dim(&self) -> usize {
        Self::DIM
    }

    /// Generic [`Embedder::embed`] — back-compat default for callers
    /// that don't know about the ingest / recall asymmetry. Routes to
    /// `embed_query` so symmetric uses (e.g. mock tests) behave the
    /// same as before D138.
    fn embed(&self, text: &str) -> Vec<f32> {
        self.embed_query(text)
    }

    /// D138 — asymmetric ingest path. e5 paper recommends the
    /// `passage: ` prefix on stored corpus so retrieval cosine peaks
    /// when the query (under `query: ` prefix) and the passage live
    /// in their respective halves of the model's joint space.
    fn embed_passage(&self, text: &str) -> Vec<f32> {
        self.embed_with_prefix("passage: ", text)
    }

    /// D138 — asymmetric recall path. See [`Self::embed_passage`] for
    /// the rationale; `query: ` is the recommended retrieval prefix
    /// and is what the original v1 implementation used unilaterally.
    fn embed_query(&self, text: &str) -> Vec<f32> {
        self.embed_with_prefix("query: ", text)
    }
}

impl E5LargeEmbedder {
    /// D138 — shared body of [`Self::embed_passage`] and
    /// [`Self::embed_query`]. The two public methods differ only in
    /// the literal `passage: ` / `query: ` prefix the e5 paper
    /// specifies; routing them through one helper preserves the
    /// fastembed lock contract and the zero-basis fallback under
    /// `try_new` failure.
    // Under `embed-onnx` the body locks `self.inner`; the off-feature
    // stub returns `Self::zero_basis()` and genuinely doesn't read
    // `self`. clippy::pedantic::unused_self flags the off-feature
    // branch — silence at the site rather than refactor away `&self`,
    // because keeping the receiver matches the on-feature shape and
    // any future pre-call invariant (rate-limit, last-prefix cache)
    // wants `self` access without an API churn.
    #[cfg_attr(not(feature = "embed-onnx"), allow(unused_variables))]
    #[allow(clippy::unused_self)]
    fn embed_with_prefix(&self, prefix: &str, text: &str) -> Vec<f32> {
        #[cfg(feature = "embed-onnx")]
        {
            let prompt = format!("{prefix}{text}");
            let mut guard = self.inner.lock().expect("E5LargeEmbedder mutex");
            match guard.embed(vec![prompt], None) {
                Ok(mut batch) => batch.pop().unwrap_or_else(Self::zero_basis),
                Err(e) => {
                    tracing::warn!(error = %e, "fastembed e5-large embed failed; returning zero basis");
                    Self::zero_basis()
                }
            }
        }
        #[cfg(not(feature = "embed-onnx"))]
        {
            Self::zero_basis()
        }
    }

    fn zero_basis() -> Vec<f32> {
        let mut v = vec![0.0_f32; Self::DIM];
        v[0] = 1.0;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_constants_match_design() {
        assert_eq!(E5LargeEmbedder::MODEL_ID, "multilingual-e5-large-1024d");
        assert_eq!(E5LargeEmbedder::DIM, 1024);
        let cache = E5LargeEmbedder::cache_dir().expect("home dir");
        let comps: Vec<_> = cache.components().map(|c| c.as_os_str().to_owned()).collect();
        assert!(comps.iter().any(|c| c == ".soma"));
        assert!(comps.iter().any(|c| c == "models"));
        assert!(cache.ends_with(E5LargeEmbedder::MODEL_ID));
    }

    #[cfg(not(feature = "embed-onnx"))]
    #[test]
    fn new_returns_feature_off_when_disabled() {
        match E5LargeEmbedder::new() {
            Err(E5Error::FeatureOff) => {}
            Err(other) => panic!("expected FeatureOff, got {other}"),
            Ok(_) => panic!("expected FeatureOff, got Ok"),
        }
    }
}
