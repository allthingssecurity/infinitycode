pub mod agentfs_fs;
pub mod cache;
pub mod file_handle;

use serde::Serialize;

/// Metadata for an inode.
#[derive(Debug, Clone, Serialize)]
pub struct Stat {
    pub ino: i64,
    pub mode: i64,
    pub size: i64,
    pub nlink: i64,
    pub ctime: String,
    pub mtime: String,
    pub atime: String,
}

impl Stat {
    /// Is this a directory? (mode & 0o170000 == 0o040000)
    pub fn is_dir(&self) -> bool {
        (self.mode & 0o170000) == 0o040000
    }

    /// Is this a regular file? (mode & 0o170000 == 0o100000)
    pub fn is_file(&self) -> bool {
        (self.mode & 0o170000) == 0o100000
    }

    /// Is this a symlink? (mode & 0o170000 == 0o120000)
    pub fn is_symlink(&self) -> bool {
        (self.mode & 0o170000) == 0o120000
    }

    /// Format mode as a POSIX-style string (e.g., "drwxr-xr-x").
    pub fn mode_string(&self) -> String {
        let ft = match self.mode & 0o170000 {
            0o040000 => 'd',
            0o120000 => 'l',
            _ => '-',
        };
        let perms = self.mode & 0o777;
        let mut s = String::with_capacity(10);
        s.push(ft);
        for shift in [6, 3, 0] {
            let bits = (perms >> shift) & 7;
            s.push(if bits & 4 != 0 { 'r' } else { '-' });
            s.push(if bits & 2 != 0 { 'w' } else { '-' });
            s.push(if bits & 1 != 0 { 'x' } else { '-' });
        }
        s
    }
}

/// A directory entry.
#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub ino: i64,
    pub mode: i64,
}

/// A tree node for recursive directory listing.
#[derive(Debug, Clone, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub stat: Stat,
    pub children: Vec<TreeNode>,
}

/// A search result entry.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub path: String,
    pub ino: i64,
    pub is_dir: bool,
    pub size: i64,
}

pub use agentfs_fs::AgentFSFileSystem;
