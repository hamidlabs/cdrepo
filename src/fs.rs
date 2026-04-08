use crate::github::{BlockingGitHubClient, RepoSpec, RepoTree};
use fuser::{
    AccessFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, Request,
};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::debug;

const TTL: Duration = Duration::from_secs(300);
const BLOCK_SIZE: u32 = 512;

/// FUSE filesystem — fully synchronous, no tokio dependency.
pub struct RepoFs {
    tree: Arc<RepoTree>,
    client: Arc<BlockingGitHubClient>,
    spec: RepoSpec,
    inode_to_path: HashMap<u64, String>,
    path_to_inode: HashMap<String, u64>,
    blob_cache: std::sync::Mutex<HashMap<String, Vec<u8>>>,
}

impl RepoFs {
    pub fn new(
        tree: RepoTree,
        client: BlockingGitHubClient,
        spec: RepoSpec,
    ) -> Self {
        let tree = Arc::new(tree);
        let client = Arc::new(client);

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

        Self {
            tree,
            client,
            spec,
            inode_to_path,
            path_to_inode,
            blob_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn make_attr(&self, ino: u64, path: &str) -> FileAttr {
        let now = SystemTime::now();
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        if path.is_empty() || self.tree.is_dir(path) {
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
            let size = self
                .tree
                .lookup(path)
                .and_then(|e| e.size)
                .unwrap_or(0);
            FileAttr {
                ino: INodeNo(ino),
                size,
                blocks: (size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64,
                atime: now,
                mtime: now,
                ctime: now,
                crtime: now,
                kind: if self.is_symlink(path) {
                    FileType::Symlink
                } else {
                    FileType::RegularFile
                },
                perm: if self.is_executable(path) { 0o755 } else { 0o644 },
                nlink: 1,
                uid,
                gid,
                rdev: 0,
                blksize: BLOCK_SIZE,
                flags: 0,
            }
        }
    }

    fn is_executable(&self, path: &str) -> bool {
        self.tree
            .lookup(path)
            .map(|e| e.mode == "100755")
            .unwrap_or(false)
    }

    fn is_symlink(&self, path: &str) -> bool {
        self.tree
            .lookup(path)
            .map(|e| e.mode == "120000")
            .unwrap_or(false)
    }

    /// Fetch blob content synchronously using the blocking HTTP client.
    fn fetch_blob_sync(&self, path: &str) -> Option<Vec<u8>> {
        {
            let cache = self.blob_cache.lock().unwrap();
            if let Some(data) = cache.get(path) {
                return Some(data.clone());
            }
        }

        let entry = self.tree.lookup(path)?;
        let sha = entry.sha.clone();

        let data = self
            .client
            .fetch_blob(&self.spec.owner, &self.spec.repo, &sha)
            .ok()?;

        {
            let mut cache = self.blob_cache.lock().unwrap();
            cache.insert(path.to_string(), data.clone());
        }

        Some(data)
    }
}

impl Filesystem for RepoFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let parent_ino: u64 = parent.into();
        let parent_path = match self.inode_to_path.get(&parent_ino) {
            Some(p) => p.clone(),
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let name_str = name.to_string_lossy();
        let child_path = if parent_path.is_empty() {
            name_str.to_string()
        } else {
            format!("{parent_path}/{name_str}")
        };

        if let Some(&ino) = self.path_to_inode.get(&child_path) {
            let attr = self.make_attr(ino, &child_path);
            reply.entry(&TTL, &attr, Generation(0));
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let ino_u64: u64 = ino.into();
        if let Some(path) = self.inode_to_path.get(&ino_u64).cloned() {
            let mut attr = self.make_attr(ino_u64, &path);
            if attr.kind == FileType::RegularFile && attr.size == 0 {
                let cache = self.blob_cache.lock().unwrap();
                if let Some(data) = cache.get(&path) {
                    attr.size = data.len() as u64;
                    attr.blocks = (attr.size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64;
                }
            }
            reply.attr(&TTL, &attr);
        } else {
            reply.error(Errno::ENOENT);
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
        let path = match self.inode_to_path.get(&ino_u64) {
            Some(p) => p.clone(),
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        debug!("read: {path} offset={offset} size={size}");

        match self.fetch_blob_sync(&path) {
            Some(data) => {
                let offset = offset as usize;
                let end = (offset + size as usize).min(data.len());
                if offset >= data.len() {
                    reply.data(&[]);
                } else {
                    reply.data(&data[offset..end]);
                }
            }
            None => {
                reply.error(Errno::EIO);
            }
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
        let dir_path = match self.inode_to_path.get(&ino_u64) {
            Some(p) => p.clone(),
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let mut entries: Vec<(u64, FileType, String)> = vec![
            (ino_u64, FileType::Directory, ".".to_string()),
            (ino_u64, FileType::Directory, "..".to_string()),
        ];

        for child in self.tree.list_dir(&dir_path) {
            let child_name = child
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&child.path)
                .to_string();
            let child_ino = self.path_to_inode.get(&child.path).copied().unwrap_or(0);
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
        reply.ok();
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let ino_u64: u64 = ino.into();
        let path = match self.inode_to_path.get(&ino_u64) {
            Some(p) => p.clone(),
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        match self.fetch_blob_sync(&path) {
            Some(data) => reply.data(&data),
            None => reply.error(Errno::EIO),
        }
    }

    fn access(&self, _req: &Request, ino: INodeNo, _mask: AccessFlags, reply: ReplyEmpty) {
        let ino_u64: u64 = ino.into();
        if self.inode_to_path.contains_key(&ino_u64) {
            reply.ok();
        } else {
            reply.error(Errno::ENOENT);
        }
    }
}
