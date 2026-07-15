use dashmap::DashMap;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tower_lsp::lsp_types::{CompletionItem, Diagnostic, DocumentSymbolResponse, SemanticToken};

const SESSION_CACHE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct QueryKey {
    schema: u32,
    toolchain: &'static str,
    host: String,
    namespace: &'static str,
    identity: String,
    fingerprint: [u8; 32],
}

impl QueryKey {
    pub(crate) fn for_document(
        namespace: &'static str,
        identity: impl Into<String>,
        path: &Path,
        text: &str,
        overrides: &[(PathBuf, String)],
    ) -> Self {
        let mut hasher = Sha256::new();
        add_bytes(&mut hasher, path.to_string_lossy().as_bytes());
        add_bytes(&mut hasher, text.as_bytes());
        let mut overrides = overrides.iter().collect::<Vec<_>>();
        overrides.sort_by(|left, right| left.0.cmp(&right.0));
        for (override_path, source) in overrides {
            add_bytes(&mut hasher, override_path.to_string_lossy().as_bytes());
            add_bytes(&mut hasher, source.as_bytes());
        }
        if let Some(manifest) = nearest_manifest(path) {
            add_bytes(&mut hasher, manifest.to_string_lossy().as_bytes());
            match std::fs::read(&manifest) {
                Ok(contents) => add_bytes(&mut hasher, &contents),
                Err(error) => add_bytes(&mut hasher, error.to_string().as_bytes()),
            }
        }
        Self {
            schema: SESSION_CACHE_SCHEMA,
            toolchain: env!("CARGO_PKG_VERSION"),
            host: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            namespace,
            identity: identity.into(),
            fingerprint: hasher.finalize().into(),
        }
    }
}

fn add_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn nearest_manifest(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start
        .ancestors()
        .map(|ancestor| ancestor.join("nomo.toml"))
        .find(|candidate| candidate.is_file())
}

#[derive(Debug, Clone)]
struct CacheEntry<V> {
    value: V,
    dependencies: BTreeSet<PathBuf>,
}

#[derive(Debug)]
struct QueryCache<V> {
    entries: DashMap<QueryKey, CacheEntry<V>>,
    hits: AtomicU64,
    misses: AtomicU64,
    invalidations: AtomicU64,
}

impl<V> Default for QueryCache<V> {
    fn default() -> Self {
        Self {
            entries: DashMap::new(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
        }
    }
}

impl<V: Clone> QueryCache<V> {
    fn get_or_compute(
        &self,
        key: QueryKey,
        dependencies: impl IntoIterator<Item = PathBuf>,
        compute: impl FnOnce() -> V,
    ) -> V {
        if let Some(entry) = self.entries.get(&key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return entry.value.clone();
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let value = compute();
        let entry = CacheEntry {
            value: value.clone(),
            dependencies: dependencies.into_iter().collect(),
        };
        match self.entries.entry(key) {
            dashmap::mapref::entry::Entry::Occupied(existing) => existing.get().value.clone(),
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(entry);
                value
            }
        }
    }

    fn invalidate_path(&self, path: &Path) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| !entry.dependencies.contains(path));
        let removed = before.saturating_sub(self.entries.len());
        self.invalidations
            .fetch_add(removed as u64, Ordering::Relaxed);
        removed
    }

    fn clear(&self) -> usize {
        let removed = self.entries.len();
        self.entries.clear();
        self.invalidations
            .fetch_add(removed as u64, Ordering::Relaxed);
        removed
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
            entries: self.entries.len() as u64,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CacheStats {
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) invalidations: u64,
    pub(crate) entries: u64,
}

impl CacheStats {
    fn add(self, other: Self) -> Self {
        Self {
            hits: self.hits + other.hits,
            misses: self.misses + other.misses,
            invalidations: self.invalidations + other.invalidations,
            entries: self.entries + other.entries,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct IncrementalSession {
    diagnostics: QueryCache<Vec<Diagnostic>>,
    completions: QueryCache<Vec<CompletionItem>>,
    document_symbols: QueryCache<Option<DocumentSymbolResponse>>,
    semantic_tokens: QueryCache<Vec<SemanticToken>>,
}

impl IncrementalSession {
    pub(crate) fn diagnostics(
        &self,
        key: QueryKey,
        dependencies: impl IntoIterator<Item = PathBuf>,
        compute: impl FnOnce() -> Vec<Diagnostic>,
    ) -> Vec<Diagnostic> {
        self.diagnostics.get_or_compute(key, dependencies, compute)
    }

    pub(crate) fn completions(
        &self,
        key: QueryKey,
        dependencies: impl IntoIterator<Item = PathBuf>,
        compute: impl FnOnce() -> Vec<CompletionItem>,
    ) -> Vec<CompletionItem> {
        self.completions.get_or_compute(key, dependencies, compute)
    }

    pub(crate) fn document_symbols(
        &self,
        key: QueryKey,
        dependencies: impl IntoIterator<Item = PathBuf>,
        compute: impl FnOnce() -> Option<DocumentSymbolResponse>,
    ) -> Option<DocumentSymbolResponse> {
        self.document_symbols
            .get_or_compute(key, dependencies, compute)
    }

    pub(crate) fn semantic_tokens(
        &self,
        key: QueryKey,
        dependencies: impl IntoIterator<Item = PathBuf>,
        compute: impl FnOnce() -> Vec<SemanticToken>,
    ) -> Vec<SemanticToken> {
        self.semantic_tokens
            .get_or_compute(key, dependencies, compute)
    }

    pub(crate) fn invalidate_path(&self, path: &Path) -> usize {
        self.diagnostics.invalidate_path(path)
            + self.completions.invalidate_path(path)
            + self.document_symbols.invalidate_path(path)
            + self.semantic_tokens.invalidate_path(path)
    }

    pub(crate) fn clear(&self) -> usize {
        self.diagnostics.clear()
            + self.completions.clear()
            + self.document_symbols.clear()
            + self.semantic_tokens.clear()
    }

    pub(crate) fn stats(&self) -> CacheStats {
        self.diagnostics
            .stats()
            .add(self.completions.stats())
            .add(self.document_symbols.stats())
            .add(self.semantic_tokens.stats())
    }
}

pub(crate) fn dependency_paths(path: &Path, overrides: &[(PathBuf, String)]) -> BTreeSet<PathBuf> {
    let mut paths = overrides
        .iter()
        .map(|(override_path, _)| override_path.clone())
        .collect::<BTreeSet<_>>();
    paths.insert(path.to_path_buf());
    if let Some(manifest) = nearest_manifest(path) {
        paths.insert(manifest);
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn query_fingerprint_is_independent_of_overlay_order() {
        let path = PathBuf::from("src/main.nomo");
        let left = vec![
            (PathBuf::from("src/a.nomo"), "a".to_string()),
            (PathBuf::from("src/b.nomo"), "b".to_string()),
        ];
        let mut right = left.clone();
        right.reverse();
        assert_eq!(
            QueryKey::for_document("diagnostics", "main", &path, "source", &left),
            QueryKey::for_document("diagnostics", "main", &path, "source", &right)
        );
    }

    #[test]
    fn cache_hits_and_invalidates_declared_document_dependencies() {
        let cache = QueryCache::default();
        let path = PathBuf::from("src/main.nomo");
        let key = QueryKey::for_document("test", "main", &path, "source", &[]);
        let calls = AtomicUsize::new(0);
        let first = cache.get_or_compute(key.clone(), [path.clone()], || {
            calls.fetch_add(1, Ordering::SeqCst);
            vec![1_u8]
        });
        let second = cache.get_or_compute(key, [path.clone()], || {
            calls.fetch_add(1, Ordering::SeqCst);
            vec![2_u8]
        });
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(cache.invalidate_path(&path), 1);
        assert_eq!(cache.stats().entries, 0);
    }
}
