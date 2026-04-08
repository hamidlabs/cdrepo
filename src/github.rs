use anyhow::{Context, Result};
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::debug;

const API_BASE: &str = "https://api.github.com";

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: String, // "blob" or "tree"
    pub sha: String,
    #[serde(default)]
    pub size: Option<u64>,
    pub mode: String,
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    sha: String,
    tree: Vec<TreeEntry>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct RefResponse {
    object: RefObject,
}

#[derive(Debug, Deserialize)]
struct RefObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct CommitResponse {
    tree: TreeRef,
}

#[derive(Debug, Deserialize)]
struct TreeRef {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct RepoResponse {
    default_branch: String,
}

/// Parsed repo identifier.
#[derive(Debug, Clone)]
pub struct RepoSpec {
    pub owner: String,
    pub repo: String,
    pub git_ref: Option<String>,
    pub subpath: Option<String>,
}

impl RepoSpec {
    /// Parse various formats:
    /// - owner/repo
    /// - github.com/owner/repo
    /// - https://github.com/owner/repo
    /// - owner/repo/tree/branch/path
    /// - owner/repo@branch
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim().trim_end_matches('/');

        // Handle git@github.com:owner/repo.git (SSH format)
        if let Some(rest) = input.strip_prefix("git@github.com:") {
            let rest = rest.strip_suffix(".git").unwrap_or(rest);
            let parts: Vec<&str> = rest.splitn(2, '/').collect();
            if parts.len() == 2 {
                return Ok(Self {
                    owner: parts[0].to_string(),
                    repo: parts[1].to_string(),
                    git_ref: None,
                    subpath: None,
                });
            }
        }

        // Strip URL prefix
        let input = input
            .strip_prefix("https://")
            .or_else(|| input.strip_prefix("http://"))
            .unwrap_or(input);
        let input = input.strip_prefix("github.com/").unwrap_or(input);

        // Strip trailing .git
        let input = input.strip_suffix(".git").unwrap_or(input);

        // Handle owner/repo@ref
        if let Some((repo_part, git_ref)) = input.split_once('@') {
            let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
            if parts.len() == 2 {
                return Ok(Self {
                    owner: parts[0].to_string(),
                    repo: parts[1].to_string(),
                    git_ref: Some(git_ref.to_string()),
                    subpath: None,
                });
            }
        }

        let parts: Vec<&str> = input.splitn(5, '/').collect();
        match parts.len() {
            2 => Ok(Self {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
                git_ref: None,
                subpath: None,
            }),
            // owner/repo/tree/branch or owner/repo/tree/branch/path
            4 | 5 if parts[2] == "tree" || parts[2] == "blob" => Ok(Self {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
                git_ref: Some(parts[3].to_string()),
                subpath: parts.get(4).map(|s| s.to_string()),
            }),
            _ => anyhow::bail!("Invalid repo format: {input}\nExpected: owner/repo or https://github.com/owner/repo"),
        }
    }

}

#[derive(serde::Serialize, Deserialize)]
struct CachedTree {
    sha: String,
    entries: Vec<TreeEntry>,
}

/// Blocking (synchronous) GitHub client for use inside FUSE handlers.
/// No tokio runtime needed — safe to use in forked/spawned daemon processes.
pub struct BlockingGitHubClient {
    client: reqwest::blocking::Client,
    cache_dir: PathBuf,
}

impl BlockingGitHubClient {
    pub fn new(token: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static("cdrepo/0.1.0"),
        );

        let client = reqwest::blocking::Client::builder()
            .default_headers(headers)
            .build()
            .context("failed to build HTTP client")?;

        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("cdrepo");

        Ok(Self { client, cache_dir })
    }

    /// Get the default branch for a repo (blocking).
    fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        let url = format!("{API_BASE}/repos/{owner}/{repo}");
        let resp: RepoResponse = self
            .client
            .get(&url)
            .send()?
            .error_for_status()
            .context(format!("repo not found: {owner}/{repo}"))?
            .json()?;
        Ok(resp.default_branch)
    }

    /// Resolve a ref to a tree SHA (blocking).
    fn resolve_tree_sha(&self, owner: &str, repo: &str, git_ref: &str) -> Result<String> {
        let url = format!("{API_BASE}/repos/{owner}/{repo}/git/ref/heads/{git_ref}");
        let resp = self.client.get(&url).send()?;

        let commit_sha = if resp.status().is_success() {
            let ref_resp: RefResponse = resp.json()?;
            ref_resp.object.sha
        } else {
            let url = format!("{API_BASE}/repos/{owner}/{repo}/git/ref/tags/{git_ref}");
            let ref_resp: RefResponse = self
                .client
                .get(&url)
                .send()?
                .error_for_status()
                .context(format!("ref not found: {git_ref}"))?
                .json()?;
            ref_resp.object.sha
        };

        let url = format!("{API_BASE}/repos/{owner}/{repo}/git/commits/{commit_sha}");
        let commit: CommitResponse = self
            .client
            .get(&url)
            .send()?
            .error_for_status()?
            .json()?;
        Ok(commit.tree.sha)
    }

    /// Fetch the full repo tree in a single API call (blocking).
    pub fn fetch_tree(&self, spec: &RepoSpec) -> Result<(String, Vec<TreeEntry>)> {
        let git_ref = match &spec.git_ref {
            Some(r) => r.clone(),
            None => self.default_branch(&spec.owner, &spec.repo)?,
        };

        let cache_file = self.tree_cache_path(&spec.owner, &spec.repo, &git_ref);
        if let Some(cached) = self.load_tree_cache(&cache_file) {
            debug!("tree cache hit for {}/{} @ {}", spec.owner, spec.repo, git_ref);
            return Ok(cached);
        }

        debug!("fetching tree for {}/{} @ {}", spec.owner, spec.repo, git_ref);

        let tree_sha = self.resolve_tree_sha(&spec.owner, &spec.repo, &git_ref)?;
        let url = format!(
            "{API_BASE}/repos/{}/{}/git/trees/{tree_sha}?recursive=1",
            spec.owner, spec.repo
        );

        let resp: TreeResponse = self
            .client
            .get(&url)
            .send()?
            .error_for_status()
            .context("failed to fetch repo tree")?
            .json()?;

        if resp.truncated {
            debug!("warning: tree response was truncated (very large repo)");
        }

        self.save_tree_cache(&cache_file, &resp.sha, &resp.tree)?;
        Ok((resp.sha, resp.tree))
    }

    /// Fetch raw file content by blob SHA (blocking).
    pub fn fetch_blob(&self, owner: &str, repo: &str, sha: &str) -> Result<Vec<u8>> {
        let cache_file = self.blob_cache_path(sha);
        if let Ok(data) = std::fs::read(&cache_file) {
            debug!("blob cache hit: {sha}");
            return Ok(data);
        }

        debug!("fetching blob: {sha}");
        let url = format!("{API_BASE}/repos/{owner}/{repo}/git/blobs/{sha}");
        let resp = self
            .client
            .get(&url)
            .header(header::ACCEPT, "application/vnd.github.raw+json")
            .send()?
            .error_for_status()
            .context("failed to fetch blob")?;

        let bytes = resp.bytes()?.to_vec();

        if let Some(parent) = cache_file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&cache_file, &bytes).ok();

        Ok(bytes)
    }

    fn tree_cache_path(&self, owner: &str, repo: &str, git_ref: &str) -> PathBuf {
        self.cache_dir
            .join("trees")
            .join(owner)
            .join(repo)
            .join(format!("{git_ref}.json"))
    }

    fn blob_cache_path(&self, sha: &str) -> PathBuf {
        let (prefix, rest) = sha.split_at(2.min(sha.len()));
        self.cache_dir.join("blobs").join(prefix).join(rest)
    }

    fn load_tree_cache(&self, path: &PathBuf) -> Option<(String, Vec<TreeEntry>)> {
        let data = std::fs::read_to_string(path).ok()?;
        let cached: CachedTree = serde_json::from_str(&data).ok()?;
        Some((cached.sha, cached.entries))
    }

    fn save_tree_cache(
        &self,
        path: &PathBuf,
        sha: &str,
        entries: &[TreeEntry],
    ) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let cached = CachedTree {
            sha: sha.to_string(),
            entries: entries.to_vec(),
        };
        let data = serde_json::to_string(&cached)?;
        std::fs::write(path, data)?;
        Ok(())
    }
}

/// Build an in-memory directory tree from flat tree entries.
pub struct RepoTree {
    pub entries: Vec<TreeEntry>,
    dir_children: HashMap<String, Vec<usize>>, // dir_path -> indices into entries
}

impl RepoTree {
    pub fn new(entries: Vec<TreeEntry>) -> Self {
        let mut dir_children: HashMap<String, Vec<usize>> = HashMap::new();

        // Root children
        dir_children.insert(String::new(), Vec::new());

        for (i, entry) in entries.iter().enumerate() {
            let parent = match entry.path.rfind('/') {
                Some(pos) => &entry.path[..pos],
                None => "",
            };
            dir_children
                .entry(parent.to_string())
                .or_default()
                .push(i);

            // Ensure directory entries exist for tree types
            if entry.entry_type == "tree" {
                dir_children.entry(entry.path.clone()).or_default();
            }
        }

        Self {
            entries,
            dir_children,
        }
    }

    /// List children of a directory path (empty string = root).
    pub fn list_dir(&self, dir_path: &str) -> Vec<&TreeEntry> {
        self.dir_children
            .get(dir_path)
            .map(|indices| indices.iter().map(|&i| &self.entries[i]).collect())
            .unwrap_or_default()
    }

    /// Lookup a specific path.
    pub fn lookup(&self, path: &str) -> Option<&TreeEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Check if a path is a directory.
    pub fn is_dir(&self, path: &str) -> bool {
        if path.is_empty() {
            return true; // root
        }
        self.entries
            .iter()
            .any(|e| e.path == path && e.entry_type == "tree")
    }
}
