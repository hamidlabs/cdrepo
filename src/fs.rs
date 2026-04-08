use crate::github::{BlockingGitHubClient, RepoSpec, RepoTree};
use fuser::{
    AccessFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, Request,
};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(300);
const BLOCK_SIZE: u32 = 512;

/// Tree loading state — mounts instantly, tree fetched in background.
enum TreeState {
    Loading,
    Ready {
        tree: RepoTree,
        inode_to_path: HashMap<u64, String>,
        path_to_inode: HashMap<String, u64>,
    },
    Failed(String),
}

/// FUSE filesystem — mounts immediately, fetches tree lazily.
pub struct RepoFs {
    state: Arc<(Mutex<TreeState>, Condvar)>,
    client: Arc<BlockingGitHubClient>,
    spec: RepoSpec,
    blob_cache: Mutex<HashMap<String, Vec<u8>>>,
}

impl RepoFs {
    /// Create a new RepoFs that mounts immediately.
    /// Tree will be fetched in a background thread.
    pub fn new(client: BlockingGitHubClient, spec: RepoSpec) -> Self {
        let client = Arc::new(client);
        let state = Arc::new((Mutex::new(TreeState::Loading), Condvar::new()));

        // Spawn background thread to fetch the tree
        let bg_client = client.clone();
        let bg_spec = spec.clone();
        let bg_state = state.clone();
        std::thread::spawn(move || {
            let result = bg_client.fetch_tree(&bg_spec);
            let (lock, cvar) = &*bg_state;
            let mut state = lock.lock().unwrap();
            match result {
                Ok((_sha, entries)) => {
                    let tree = RepoTree::new(entries);
                    let mut inode_to_path = HashMap::new();
                    let mut path_to_inode = HashMap::new();
                    inode_to_path.insert(1, String::new());
                    path_to_inode.insert(String::new(), 1);
                    let mut next_inode = 2u64;
                    for entry in &tree.entries {
                        inode_to_path.insert(next_inode, entry.path.clone());
                        path_to_inode.insert(entry.path.clone(), next_inode);
                        next_inode += 1;
                    }
                    *state = TreeState::Ready {
                        tree,
                        inode_to_path,
                        path_to_inode,
                    };
                }
                Err(e) => {
                    *state = TreeState::Failed(format!("{e:#}"));
                }
            }
            cvar.notify_all();
        });

        Self {
            state,
            client,
            spec,
            blob_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Wait for tree to be ready, then run a closure with the tree data.
    /// Returns Errno::EIO if tree loading failed.
    fn with_tree<T, F>(&self, f: F) -> Result<T, Errno>
    where
        F: FnOnce(
            &RepoTree,
            &HashMap<u64, String>,
            &HashMap<String, u64>,
        ) -> Result<T, Errno>,
    {
        let (lock, cvar) = &*self.state;
        let state = cvar
            .wait_while(lock.lock().unwrap(), |s| matches!(s, TreeState::Loading))
            .unwrap();
        match &*state {
            TreeState::Ready {
                tree,
                inode_to_path,
                path_to_inode,
            } => f(tree, inode_to_path, path_to_inode),
            TreeState::Failed(_) => Err(Errno::EIO),
            TreeState::Loading => unreachable!(),
        }
    }

    fn make_attr(&self, ino: u64, path: &str, tree: &RepoTree) -> FileAttr {
        let now = SystemTime::now();
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        if path.is_empty() || tree.is_dir(path) {
            FileAttr {
                ino: INodeNo(ino),
                size: 0,
                blocks: 0,
                atime: now,
                mtime: now,
                ctime: now,
                crtime: now,
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid,
                gid,
                rdev: 0,
                blksize: BLOCK_SIZE,
                flags: 0,
            }
        } else {
            let entry = tree.lookup(path);
            let size = entry.and_then(|e| e.size).unwrap_or(0);
            let is_symlink = entry.map(|e| e.mode == "120000").unwrap_or(false);
            let is_exec = entry.map(|e| e.mode == "100755").unwrap_or(false);
            FileAttr {
                ino: INodeNo(ino),
                size,
                blocks: (size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64,
                atime: now,
                mtime: now,
                ctime: now,
                crtime: now,
                kind: if is_symlink {
                    FileType::Symlink
                } else {
                    FileType::RegularFile
                },
                perm: if is_exec { 0o755 } else { 0o644 },
                nlink: 1,
                uid,
                gid,
                rdev: 0,
                blksize: BLOCK_SIZE,
                flags: 0,
            }
        }
    }

    fn fetch_blob_sync(&self, sha: &str) -> Option<Vec<u8>> {
        {
            let cache = self.blob_cache.lock().unwrap();
            if let Some(data) = cache.get(sha) {
                return Some(data.clone());
            }
        }

        let data = self
            .client
            .fetch_blob(&self.spec.owner, &self.spec.repo, sha)
            .ok()?;

        {
            let mut cache = self.blob_cache.lock().unwrap();
            cache.insert(sha.to_string(), data.clone());
        }

        Some(data)
    }

    fn root_attr(&self) -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino: INodeNo(1),
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }
}

impl Filesystem for RepoFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy().to_string();
        let parent_ino: u64 = parent.into();

        match self.with_tree(|tree, i2p, p2i| {
            let parent_path = i2p.get(&parent_ino).ok_or(Errno::ENOENT)?;
            let child_path = if parent_path.is_empty() {
                name_str.clone()
            } else {
                format!("{parent_path}/{name_str}")
            };
            let &ino = p2i.get(&child_path).ok_or(Errno::ENOENT)?;
            Ok((ino, self.make_attr(ino, &child_path, tree)))
        }) {
            Ok((_, attr)) => reply.entry(&TTL, &attr, Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let ino_u64: u64 = ino.into();

        // Root inode — always available, even before tree loads
        if ino_u64 == 1 {
            reply.attr(&TTL, &self.root_attr());
            return;
        }

        match self.with_tree(|tree, i2p, _| {
            let path = i2p.get(&ino_u64).ok_or(Errno::ENOENT)?;
            let mut attr = self.make_attr(ino_u64, path, tree);
            if attr.kind == FileType::RegularFile && attr.size == 0 {
                if let Some(entry) = tree.lookup(path) {
                    let cache = self.blob_cache.lock().unwrap();
                    if let Some(data) = cache.get(&entry.sha) {
                        attr.size = data.len() as u64;
                        attr.blocks = (attr.size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64;
                    }
                }
            }
            Ok(attr)
        }) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(e) => reply.error(e),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let ino_u64: u64 = ino.into();

        let sha = match self.with_tree(|tree, i2p, _| {
            let path = i2p.get(&ino_u64).ok_or(Errno::ENOENT)?;
            let entry = tree.lookup(path).ok_or(Errno::ENOENT)?;
            Ok(entry.sha.clone())
        }) {
            Ok(sha) => sha,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        match self.fetch_blob_sync(&sha) {
            Some(data) => {
                let offset = offset as usize;
                let end = (offset + size as usize).min(data.len());
                if offset >= data.len() {
                    reply.data(&[]);
                } else {
                    reply.data(&data[offset..end]);
                }
            }
            None => reply.error(Errno::EIO),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let ino_u64: u64 = ino.into();

        match self.with_tree(|tree, i2p, p2i| {
            let dir_path = i2p.get(&ino_u64).ok_or(Errno::ENOENT)?;

            let mut entries: Vec<(u64, FileType, String)> = vec![
                (ino_u64, FileType::Directory, ".".to_string()),
                (ino_u64, FileType::Directory, "..".to_string()),
            ];

            for child in tree.list_dir(dir_path) {
                let child_name = child
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&child.path)
                    .to_string();
                let child_ino = p2i.get(&child.path).copied().unwrap_or(0);
                let kind = if child.entry_type == "tree" {
                    FileType::Directory
                } else if child.mode == "120000" {
                    FileType::Symlink
                } else {
                    FileType::RegularFile
                };
                entries.push((child_ino, kind, child_name));
            }

            for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                if reply.add(INodeNo(*ino), (i + 1) as u64, *kind, &name) {
                    break;
                }
            }
            Ok(())
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let ino_u64: u64 = ino.into();

        let sha = match self.with_tree(|tree, i2p, _| {
            let path = i2p.get(&ino_u64).ok_or(Errno::ENOENT)?;
            let entry = tree.lookup(path).ok_or(Errno::ENOENT)?;
            Ok(entry.sha.clone())
        }) {
            Ok(sha) => sha,
            Err(e) => {
                reply.error(e);
                return;
            }
        };

        match self.fetch_blob_sync(&sha) {
            Some(data) => reply.data(&data),
            None => reply.error(Errno::EIO),
        }
    }

    fn access(&self, _req: &Request, ino: INodeNo, _mask: AccessFlags, reply: ReplyEmpty) {
        let ino_u64: u64 = ino.into();
        // Root inode always accessible — makes cd return instantly
        if ino_u64 == 1 {
            reply.ok();
            return;
        }
        match self.with_tree(|_, i2p, _| {
            if i2p.contains_key(&ino_u64) {
                Ok(())
            } else {
                Err(Errno::ENOENT)
            }
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
}
