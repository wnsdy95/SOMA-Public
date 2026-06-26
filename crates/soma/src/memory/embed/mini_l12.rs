//! `paraphrase-multilingual-MiniLM-L12-v2` (384d, 50 languages)
//! via fastembed (ort + tokenizers + hf-hub).
//!
//! v1.1 D70 wire — discussion 0038 §결정 의 A1 fastembed lock
//! 결과. `embed-onnx` cargo feature 가 ON 일 때 만 활성.
//!
//! fastembed 가 ort runtime download + tokenizer + mean-pooling +
//! L2-norm 모두 처리. SOMA 는 cache directory 만 강제
//! (`~/.soma/models/<id>/`) + Embedder trait 의 `&self` 인터페이스 에
//! 맞추기 위한 interior mutability (`Mutex<TextEmbedding>`).

use crate::memory::embed::Embedder;

/// Production embed backend backed by MiniLM-L12 ONNX. v1.1 의
/// `embed-onnx` feature 가 ON 일 때 fastembed 의 lazy download +
/// inference 를 wrap, OFF 일 때 `new()` 가 typed `Err` 반환 (caller
/// 는 `HashEmbedder` 로 fallback).
pub struct OnnxEmbedder {
    /// fastembed handle. `embed` 가 `&mut self` 라 SOMA 의 Embedder
    /// trait (`&self`) 와 맞추기 위해 Mutex.
    #[cfg(feature = "embed-onnx")]
    inner: std::sync::Mutex<fastembed::TextEmbedding>,

    /// off-feature 빌드 의 type-shape 유지 용 zero-size flag.
    #[cfg(not(feature = "embed-onnx"))]
    _phantom: std::marker::PhantomData<()>,
}

#[derive(Debug)]
pub enum OnnxError {
    /// `embed-onnx` cargo feature is off — caller should fall back
    /// to `HashEmbedder`.
    FeatureOff,
    /// Feature on but model load / download failed (network error,
    /// disk full, corrupt cache, ...). Wraps fastembed's error.
    Init(String),
}

impl std::fmt::Display for OnnxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OnnxError::FeatureOff => {
                write!(f, "embed-onnx feature is disabled; rebuild with `--features embed-onnx`")
            }
            OnnxError::Init(m) => write!(f, "MiniLM ONNX init failed: {m}"),
        }
    }
}

impl std::error::Error for OnnxError {}

impl OnnxEmbedder {
    /// Canonical model id for this embedder — matches
    /// `episode_vectors.model_id` discriminator. Stable string,
    /// don't change across versions (would orphan stored vectors).
    pub const MODEL_ID: &'static str = "minilm-l12-v2-384d";

    /// `~/.soma/models/<MODEL_ID>/` — single source of truth for
    /// downloaded ONNX artifacts. fastembed's default cache
    /// (`~/.cache/fastembed/`) overridden so SOMA owns the layout.
    pub fn cache_dir() -> Result<std::path::PathBuf, OnnxError> {
        let home =
            dirs::home_dir().ok_or_else(|| OnnxError::Init("home dir unresolvable".into()))?;
        Ok(home.join(".soma").join("models").join(Self::MODEL_ID))
    }

    /// Feature-aware constructor. ON 시 fastembed lazy download +
    /// runtime init. OFF 시 `Err(FeatureOff)`.
    pub fn new() -> Result<Self, OnnxError> {
        #[cfg(feature = "embed-onnx")]
        {
            use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

            let cache = Self::cache_dir()?;
            std::fs::create_dir_all(&cache)
                .map_err(|e| OnnxError::Init(format!("mkdir {}: {e}", cache.display())))?;

            let opts =
                InitOptions::new(EmbeddingModel::ParaphraseMLMiniLML12V2).with_cache_dir(cache);
            let inner = TextEmbedding::try_new(opts)
                .map_err(|e| OnnxError::Init(format!("fastembed::try_new: {e}")))?;

            Ok(Self { inner: std::sync::Mutex::new(inner) })
        }
        #[cfg(not(feature = "embed-onnx"))]
        {
            Err(OnnxError::FeatureOff)
        }
    }

    /// `soma install --model` 의 backend — first-run download 만
    /// 수행 후 drop. fastembed 의 `try_new` 가 cache miss 시 자동
    /// download 하므로 `new()` 호출 + drop 으로 충분.
    pub fn ensure_downloaded() -> Result<std::path::PathBuf, OnnxError> {
        let cache = Self::cache_dir()?;
        let _embedder = Self::new()?;
        Ok(cache)
    }
}

impl Embedder for OnnxEmbedder {
    fn model_id(&self) -> &'static str {
        Self::MODEL_ID
    }

    fn dim(&self) -> usize {
        384
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        #[cfg(feature = "embed-onnx")]
        {
            let mut guard = self.inner.lock().expect("OnnxEmbedder mutex");
            match guard.embed(vec![text], None) {
                Ok(mut batch) => batch.pop().unwrap_or_else(Self::zero_basis),
                Err(e) => {
                    tracing::warn!(error = %e, "fastembed embed failed; returning zero basis");
                    Self::zero_basis()
                }
            }
        }
        #[cfg(not(feature = "embed-onnx"))]
        {
            let _ = text;
            Self::zero_basis()
        }
    }
}

impl OnnxEmbedder {
    /// Fallback when fastembed errors mid-call — returns the e0
    /// basis vector (`[1.0, 0.0, ...]`). Same convention as the
    /// pre-D70 stub so existing recall paths keep working.
    fn zero_basis() -> Vec<f32> {
        let mut v = vec![0.0_f32; 384];
        v[0] = 1.0;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_constants_match_design() {
        // dim + model_id are stable regardless of feature state.
        assert_eq!(OnnxEmbedder::MODEL_ID, "minilm-l12-v2-384d");
        // cache_dir resolves to ~/.soma/models/<id>/ shape. Use
        // Path::components rather than string matching so the
        // assertion is portable — Windows separates with `\`,
        // unix with `/`.
        let cache = OnnxEmbedder::cache_dir().expect("home dir");
        assert!(cache.ends_with("minilm-l12-v2-384d"));
        let comps: Vec<_> = cache.components().map(|c| c.as_os_str().to_owned()).collect();
        assert!(comps.iter().any(|c| c == ".soma"), "cache path contains `.soma`: {cache:?}");
        assert!(comps.iter().any(|c| c == "models"), "cache path contains `models`: {cache:?}");
    }

    #[cfg(not(feature = "embed-onnx"))]
    #[test]
    fn new_returns_feature_off_when_disabled() {
        match OnnxEmbedder::new() {
            Err(OnnxError::FeatureOff) => {}
            Err(other) => panic!("expected FeatureOff, got {other}"),
            Ok(_) => panic!("expected FeatureOff, got Ok"),
        }
    }
}
