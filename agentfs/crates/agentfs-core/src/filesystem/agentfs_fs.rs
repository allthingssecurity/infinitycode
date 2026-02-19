use std::sync::Arc;

use rusqlite::Connection;

use crate::config::AgentFSConfig;
use crate::connection::pool::{ReaderPool, WriterHandle};
use crate::error::{AgentFSError, Result};
use crate::filesystem::cache::DentryCache;
use crate::filesystem::file_handle::{read_file_data, write_file_data};
use crate::filesystem::{DirEntry, SearchResult, Stat, TreeNode};
use crate::schema::get_chunk_size;

/// Root inode number.
const ROOT_INO: i64 = 1;

/// POSIX mode bits.
const S_IFDIR: i64 = 0o040000;
const S_IFREG: i64 = 0o100000;

/// SQLite-backed filesystem implementation.
pub struct AgentFSFileSystem {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
    cache: Arc<DentryCache>,
    verify_checksums: bool,
    chunk_size: usize,
}

impl AgentFSFileSystem {
    pub fn new(
        writer: Arc<WriterHandle>,
        readers: Arc<ReaderPool>,
        config: &AgentFSConfig,
    ) -> Result<Self> {
        let chunk_size = {
            let conn = rusqlite::Connection::open(&config.db_path)?;
            get_chunk_size(&conn)?
        };

        Ok(Self {
            writer,
            readers,
            cache: Arc::new(DentryCache::new(4096)),
            verify_checksums: config.verify_checksums,
            chunk_size,
        })
    }

    /// Resolve a POSIX path to an inode number.
    fn resolve_path(conn: &Connection, path: &str, cache: &DentryCache) -> Result<i64> {
        if path == "/" {
            return Ok(ROOT_INO);
        }

        let path = path.strip_prefix('/').unwrap_or(path);
        let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();

        let mut current_ino = ROOT_INO;
        for component in &components {
            if let Some(ino) = cache.get(current_ino, component) {
                current_ino = ino;
                continue;
            }

            let ino: i64 = conn
                .query_row(
                    "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                    rusqlite::params![current_ino, component],
                    |row| row.get(0),
                )
                .map_err(|_| AgentFSError::FileNotFound {
                    path: format!("/{}", components.join("/")),
                })?;

            cache.insert(current_ino, component.to_string(), ino);
            current_ino = ino;
        }

        Ok(current_ino)
    }

    /// Split a path into (parent_path, basename) — both owned.
    fn split_path(path: &str) -> Result<(String, String)> {
        if path == "/" {
            return Err(AgentFSError::InvalidPath {
                path: path.to_string(),
            });
        }
        let path = path.strip_suffix('/').unwrap_or(path);
        match path.rfind('/') {
            Some(0) => Ok(("/".to_string(), path[1..].to_string())),
            Some(i) => Ok((path[..i].to_string(), path[i + 1..].to_string())),
            None => Err(AgentFSError::InvalidPath {
                path: path.to_string(),
            }),
        }
    }

    /// Get inode metadata.
    fn stat_ino(conn: &Connection, ino: i64) -> Result<Stat> {
        conn.query_row(
            "SELECT ino, mode, size, nlink, ctime, mtime, atime FROM fs_inode WHERE ino = ?1",
            [ino],
            |row| {
                Ok(Stat {
                    ino: row.get(0)?,
                    mode: row.get(1)?,
                    size: row.get(2)?,
                    nlink: row.get(3)?,
                    ctime: row.get(4)?,
                    mtime: row.get(5)?,
                    atime: row.get(6)?,
                })
            },
        )
        .map_err(|_| AgentFSError::FileNotFound {
            path: format!("<ino:{ino}>"),
        })
    }

    // ── Public API ──────────────────────────────────────────────────

    /// Stat a path.
    pub async fn stat(&self, path: &str) -> Result<Stat> {
        let cache = self.cache.clone();
        let reader = self.readers.acquire().await?;
        let ino = Self::resolve_path(reader.conn(), path, &cache)?;
        Self::stat_ino(reader.conn(), ino)
    }

    /// List directory entries.
    pub async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let cache = self.cache.clone();
        let reader = self.readers.acquire().await?;
        let ino = Self::resolve_path(reader.conn(), path, &cache)?;

        let st = Self::stat_ino(reader.conn(), ino)?;
        if !st.is_dir() {
            return Err(AgentFSError::NotADirectory {
                path: path.to_string(),
            });
        }

        let mut stmt = reader.conn().prepare_cached(
            "SELECT d.name, d.ino, i.mode FROM fs_dentry d JOIN fs_inode i ON d.ino = i.ino WHERE d.parent_ino = ?1 ORDER BY d.name",
        )?;

        let entries = stmt
            .query_map([ino], |row| {
                Ok(DirEntry {
                    name: row.get(0)?,
                    ino: row.get(1)?,
                    mode: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    /// Read file contents.
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let cache = self.cache.clone();
        let verify = self.verify_checksums;
        let reader = self.readers.acquire().await?;
        let ino = Self::resolve_path(reader.conn(), path, &cache)?;

        let st = Self::stat_ino(reader.conn(), ino)?;
        if !st.is_file() {
            return Err(AgentFSError::NotAFile {
                path: path.to_string(),
            });
        }

        read_file_data(reader.conn(), ino, verify)
    }

    /// Write file contents. Creates parent directories and file if needed.
    pub async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        let cache = self.cache.clone();
        let chunk_size = self.chunk_size;
        let path = path.to_string();
        let data = data.to_vec();
        let (parent_path, name) = Self::split_path(&path)?;

        self.writer
            .with_conn(move |conn| {
                let parent_ino = ensure_parents(conn, &parent_path, &cache)?;

                let existing: Option<i64> = conn
                    .query_row(
                        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![parent_ino, &name],
                        |row| row.get(0),
                    )
                    .ok();

                let ino = if let Some(ino) = existing {
                    let st = Self::stat_ino(conn, ino)?;
                    if !st.is_file() {
                        return Err(AgentFSError::NotAFile { path });
                    }
                    ino
                } else {
                    let mode = S_IFREG | 0o644;
                    conn.execute(
                        "INSERT INTO fs_inode (mode, nlink) VALUES (?1, 1)",
                        [mode],
                    )?;
                    let ino = conn.last_insert_rowid();

                    conn.execute(
                        "INSERT INTO fs_dentry (parent_ino, name, ino) VALUES (?1, ?2, ?3)",
                        rusqlite::params![parent_ino, &name, ino],
                    )?;
                    cache.insert(parent_ino, name, ino);
                    ino
                };

                write_file_data(conn, ino, &data, chunk_size)?;
                Ok(())
            })
            .await
    }

    /// Create a directory (and intermediate parents).
    pub async fn mkdir(&self, path: &str) -> Result<()> {
        let cache = self.cache.clone();
        let path = path.to_string();
        self.writer
            .with_conn(move |conn| {
                ensure_parents(conn, &path, &cache)?;
                Ok(())
            })
            .await
    }

    /// Remove a file.
    pub async fn remove_file(&self, path: &str) -> Result<()> {
        let cache = self.cache.clone();
        let path_owned = path.to_string();
        let (parent_path, name) = Self::split_path(path)?;

        self.writer
            .with_conn(move |conn| {
                let parent_ino = Self::resolve_path(conn, &parent_path, &cache)?;

                let ino: i64 = conn
                    .query_row(
                        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![parent_ino, &name],
                        |row| row.get(0),
                    )
                    .map_err(|_| AgentFSError::FileNotFound {
                        path: path_owned.clone(),
                    })?;

                let st = Self::stat_ino(conn, ino)?;
                if st.is_dir() {
                    return Err(AgentFSError::NotAFile {
                        path: path_owned,
                    });
                }

                conn.execute(
                    "DELETE FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                    rusqlite::params![parent_ino, &name],
                )?;
                cache.remove(parent_ino, &name);

                conn.execute(
                    "UPDATE fs_inode SET nlink = nlink - 1 WHERE ino = ?1",
                    [ino],
                )?;
                let nlink: i64 = conn.query_row(
                    "SELECT nlink FROM fs_inode WHERE ino = ?1",
                    [ino],
                    |r| r.get(0),
                )?;
                if nlink <= 0 {
                    conn.execute("DELETE FROM fs_data WHERE ino = ?1", [ino])?;
                    conn.execute("DELETE FROM fs_symlink WHERE ino = ?1", [ino])?;
                    conn.execute("DELETE FROM fs_inode WHERE ino = ?1", [ino])?;
                }

                Ok(())
            })
            .await
    }

    /// Remove an empty directory.
    pub async fn rmdir(&self, path: &str) -> Result<()> {
        if path == "/" {
            return Err(AgentFSError::InvalidPath {
                path: path.to_string(),
            });
        }

        let cache = self.cache.clone();
        let path_owned = path.to_string();
        let (parent_path, name) = Self::split_path(path)?;

        self.writer
            .with_conn(move |conn| {
                let parent_ino = Self::resolve_path(conn, &parent_path, &cache)?;

                let ino: i64 = conn
                    .query_row(
                        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![parent_ino, &name],
                        |row| row.get(0),
                    )
                    .map_err(|_| AgentFSError::FileNotFound {
                        path: path_owned.clone(),
                    })?;

                let st = Self::stat_ino(conn, ino)?;
                if !st.is_dir() {
                    return Err(AgentFSError::NotADirectory {
                        path: path_owned.clone(),
                    });
                }

                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM fs_dentry WHERE parent_ino = ?1",
                    [ino],
                    |r| r.get(0),
                )?;
                if count > 0 {
                    return Err(AgentFSError::DirectoryNotEmpty {
                        path: path_owned,
                    });
                }

                conn.execute(
                    "DELETE FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                    rusqlite::params![parent_ino, &name],
                )?;
                cache.remove(parent_ino, &name);
                conn.execute("DELETE FROM fs_inode WHERE ino = ?1", [ino])?;

                Ok(())
            })
            .await
    }

    /// Recursive tree listing.
    pub async fn tree(&self, path: &str) -> Result<TreeNode> {
        let cache = self.cache.clone();
        let reader = self.readers.acquire().await?;
        let ino = Self::resolve_path(reader.conn(), path, &cache)?;
        let st = Self::stat_ino(reader.conn(), ino)?;

        let name = if path == "/" {
            "/".to_string()
        } else {
            path.rsplit('/').next().unwrap_or(path).to_string()
        };

        build_tree(reader.conn(), name, ino, &st)
    }

    /// Check whether a path exists.
    pub async fn exists(&self, path: &str) -> Result<bool> {
        let cache = self.cache.clone();
        let reader = self.readers.acquire().await?;
        match Self::resolve_path(reader.conn(), path, &cache) {
            Ok(_) => Ok(true),
            Err(AgentFSError::FileNotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Append data to a file. Creates the file if it doesn't exist.
    pub async fn append_file(&self, path: &str, data: &[u8]) -> Result<()> {
        let cache = self.cache.clone();
        let chunk_size = self.chunk_size;
        let verify = self.verify_checksums;
        let path = path.to_string();
        let data = data.to_vec();
        let (parent_path, name) = Self::split_path(&path)?;

        self.writer
            .with_conn(move |conn| {
                let parent_ino = ensure_parents(conn, &parent_path, &cache)?;

                let existing: Option<i64> = conn
                    .query_row(
                        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![parent_ino, &name],
                        |row| row.get(0),
                    )
                    .ok();

                let ino = if let Some(ino) = existing {
                    let st = Self::stat_ino(conn, ino)?;
                    if !st.is_file() {
                        return Err(AgentFSError::NotAFile { path });
                    }
                    // Read existing data and append
                    let mut existing_data = read_file_data(conn, ino, verify)?;
                    existing_data.extend_from_slice(&data);
                    write_file_data(conn, ino, &existing_data, chunk_size)?;
                    return Ok(());
                } else {
                    // Create new file
                    let mode = S_IFREG | 0o644;
                    conn.execute(
                        "INSERT INTO fs_inode (mode, nlink) VALUES (?1, 1)",
                        [mode],
                    )?;
                    let ino = conn.last_insert_rowid();
                    conn.execute(
                        "INSERT INTO fs_dentry (parent_ino, name, ino) VALUES (?1, ?2, ?3)",
                        rusqlite::params![parent_ino, &name, ino],
                    )?;
                    cache.insert(parent_ino, name, ino);
                    ino
                };

                write_file_data(conn, ino, &data, chunk_size)?;
                Ok(())
            })
            .await
    }

    /// Rename (move) a file or directory from one path to another.
    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let cache = self.cache.clone();
        let from = from.to_string();
        let to = to.to_string();
        let (from_parent_path, from_name) = Self::split_path(&from)?;
        let (to_parent_path, to_name) = Self::split_path(&to)?;

        self.writer
            .with_conn(move |conn| {
                let from_parent_ino = Self::resolve_path(conn, &from_parent_path, &cache)?;

                // Resolve source
                let src_ino: i64 = conn
                    .query_row(
                        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![from_parent_ino, &from_name],
                        |row| row.get(0),
                    )
                    .map_err(|_| AgentFSError::FileNotFound {
                        path: from.clone(),
                    })?;

                // Ensure destination parent exists
                let to_parent_ino = ensure_parents(conn, &to_parent_path, &cache)?;

                // Check if destination already exists — overwrite (POSIX semantics)
                let existing_dest: Option<i64> = conn
                    .query_row(
                        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![to_parent_ino, &to_name],
                        |row| row.get(0),
                    )
                    .ok();

                if let Some(dest_ino) = existing_dest {
                    let dest_st = Self::stat_ino(conn, dest_ino)?;
                    let src_st = Self::stat_ino(conn, src_ino)?;

                    // Can't overwrite a non-empty directory
                    if dest_st.is_dir() {
                        let count: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM fs_dentry WHERE parent_ino = ?1",
                            [dest_ino],
                            |r| r.get(0),
                        )?;
                        if count > 0 {
                            return Err(AgentFSError::DirectoryNotEmpty { path: to.clone() });
                        }
                    }

                    // Can't rename dir over file or file over dir
                    if src_st.is_dir() != dest_st.is_dir() {
                        if src_st.is_dir() {
                            return Err(AgentFSError::NotADirectory { path: to.clone() });
                        } else {
                            return Err(AgentFSError::NotAFile { path: to.clone() });
                        }
                    }

                    // Remove destination dentry and clean up inode
                    conn.execute(
                        "DELETE FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![to_parent_ino, &to_name],
                    )?;
                    conn.execute(
                        "UPDATE fs_inode SET nlink = nlink - 1 WHERE ino = ?1",
                        [dest_ino],
                    )?;
                    let nlink: i64 = conn.query_row(
                        "SELECT nlink FROM fs_inode WHERE ino = ?1",
                        [dest_ino],
                        |r| r.get(0),
                    )?;
                    if nlink <= 0 {
                        conn.execute("DELETE FROM fs_data WHERE ino = ?1", [dest_ino])?;
                        conn.execute("DELETE FROM fs_symlink WHERE ino = ?1", [dest_ino])?;
                        conn.execute("DELETE FROM fs_inode WHERE ino = ?1", [dest_ino])?;
                    }
                    cache.remove(to_parent_ino, &to_name);
                }

                // Remove old dentry
                conn.execute(
                    "DELETE FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                    rusqlite::params![from_parent_ino, &from_name],
                )?;
                cache.remove(from_parent_ino, &from_name);

                // Create new dentry
                conn.execute(
                    "INSERT INTO fs_dentry (parent_ino, name, ino) VALUES (?1, ?2, ?3)",
                    rusqlite::params![to_parent_ino, &to_name, src_ino],
                )?;
                cache.insert(to_parent_ino, to_name, src_ino);

                Ok(())
            })
            .await
    }

    /// Recursively remove a directory and all its contents.
    pub async fn remove_tree(&self, path: &str) -> Result<()> {
        if path == "/" {
            return Err(AgentFSError::InvalidPath {
                path: path.to_string(),
            });
        }

        let cache = self.cache.clone();
        let path_owned = path.to_string();
        let (parent_path, name) = Self::split_path(path)?;

        self.writer
            .with_conn(move |conn| {
                let parent_ino = Self::resolve_path(conn, &parent_path, &cache)?;

                let root_ino: i64 = conn
                    .query_row(
                        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                        rusqlite::params![parent_ino, &name],
                        |row| row.get(0),
                    )
                    .map_err(|_| AgentFSError::FileNotFound {
                        path: path_owned.clone(),
                    })?;

                // Collect all descendant inodes via DFS
                let mut descendants = Vec::new();
                collect_descendants(conn, root_ino, &mut descendants)?;

                // All inodes to remove (descendants + root)
                let mut all_inodes = descendants;
                all_inodes.push(root_ino);

                // Phase 1: Delete ALL dentries referencing these inodes
                // (both as parent and as child, except the root's parent link)
                for ino in &all_inodes {
                    conn.execute("DELETE FROM fs_dentry WHERE parent_ino = ?1", [ino])?;
                    conn.execute("DELETE FROM fs_dentry WHERE ino = ?1", [ino])?;
                }

                // Phase 2: Delete data, symlinks, inodes
                for ino in &all_inodes {
                    conn.execute("DELETE FROM fs_data WHERE ino = ?1", [ino])?;
                    conn.execute("DELETE FROM fs_symlink WHERE ino = ?1", [ino])?;
                    conn.execute("DELETE FROM fs_inode WHERE ino = ?1", [ino])?;
                }

                // Clear entire cache after tree removal
                cache.clear();

                Ok(())
            })
            .await
    }

    /// Search for files/directories matching a glob pattern.
    /// Supports `*` (any chars) and `?` (single char).
    pub async fn search(&self, pattern: &str) -> Result<Vec<SearchResult>> {
        let reader = self.readers.acquire().await?;
        let sql_pattern = glob_to_sql(pattern);

        let mut stmt = reader.conn().prepare(
            "SELECT d.ino, d.name, d.parent_ino, i.mode, i.size \
             FROM fs_dentry d JOIN fs_inode i ON d.ino = i.ino \
             WHERE d.name LIKE ?1",
        )?;

        let rows: Vec<(i64, String, i64, i64, i64)> = stmt
            .query_map([&sql_pattern], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut results = Vec::new();
        for (ino, _name, parent_ino, mode, size) in rows {
            let path = reconstruct_path(reader.conn(), ino, parent_ino)?;
            let is_dir = (mode & 0o170000) == 0o040000;
            results.push(SearchResult {
                path,
                ino,
                is_dir,
                size,
            });
        }

        Ok(results)
    }
}

/// Recursively build a tree from a directory inode.
fn build_tree(conn: &Connection, name: String, ino: i64, st: &Stat) -> Result<TreeNode> {
    let mut children = Vec::new();

    if st.is_dir() {
        let mut stmt = conn.prepare_cached(
            "SELECT d.name, d.ino, i.mode, i.size, i.nlink, i.ctime, i.mtime, i.atime \
             FROM fs_dentry d JOIN fs_inode i ON d.ino = i.ino \
             WHERE d.parent_ino = ?1 ORDER BY d.name",
        )?;

        let rows = stmt.query_map([ino], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                Stat {
                    ino: row.get(1)?,
                    mode: row.get(2)?,
                    size: row.get(3)?,
                    nlink: row.get(4)?,
                    ctime: row.get(5)?,
                    mtime: row.get(6)?,
                    atime: row.get(7)?,
                },
            ))
        })?;

        for row in rows {
            let (child_name, child_ino, child_stat) = row?;
            children.push(build_tree(conn, child_name, child_ino, &child_stat)?);
        }
    }

    Ok(TreeNode {
        name,
        stat: st.clone(),
        children,
    })
}

/// Ensure all parent directories for a path exist, creating them if needed.
/// Returns the inode of the leaf directory.
fn ensure_parents(conn: &Connection, path: &str, cache: &DentryCache) -> Result<i64> {
    if path == "/" {
        return Ok(ROOT_INO);
    }

    let path = path.strip_prefix('/').unwrap_or(path);
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();

    let mut current_ino = ROOT_INO;
    for component in &components {
        if let Some(ino) = cache.get(current_ino, component) {
            current_ino = ino;
            continue;
        }

        let existing: Option<i64> = conn
            .query_row(
                "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
                rusqlite::params![current_ino, component],
                |row| row.get(0),
            )
            .ok();

        if let Some(ino) = existing {
            cache.insert(current_ino, component.to_string(), ino);
            current_ino = ino;
        } else {
            let mode = S_IFDIR | 0o755;
            conn.execute(
                "INSERT INTO fs_inode (mode, nlink) VALUES (?1, 2)",
                [mode],
            )?;
            let new_ino = conn.last_insert_rowid();

            conn.execute(
                "INSERT INTO fs_dentry (parent_ino, name, ino) VALUES (?1, ?2, ?3)",
                rusqlite::params![current_ino, component, new_ino],
            )?;

            cache.insert(current_ino, component.to_string(), new_ino);
            current_ino = new_ino;
        }
    }

    Ok(current_ino)
}

/// Collect all descendant inodes via DFS (for remove_tree).
fn collect_descendants(conn: &Connection, ino: i64, result: &mut Vec<i64>) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT ino FROM fs_dentry WHERE parent_ino = ?1",
    )?;
    let children: Vec<i64> = stmt
        .query_map([ino], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for child_ino in children {
        collect_descendants(conn, child_ino, result)?;
        result.push(child_ino);
    }
    Ok(())
}

/// Convert a glob pattern to SQL LIKE pattern.
fn glob_to_sql(pattern: &str) -> String {
    let mut sql = String::with_capacity(pattern.len());
    for ch in pattern.chars() {
        match ch {
            '*' => sql.push('%'),
            '?' => sql.push('_'),
            '%' => sql.push_str("\\%"),
            '_' => sql.push_str("\\_"),
            _ => sql.push(ch),
        }
    }
    sql
}

/// Reconstruct full path for an inode by walking parent chain.
fn reconstruct_path(conn: &Connection, ino: i64, parent_ino: i64) -> Result<String> {
    let mut components = Vec::new();

    // Get the name of the matched entry
    let name: String = conn
        .query_row(
            "SELECT name FROM fs_dentry WHERE parent_ino = ?1 AND ino = ?2 LIMIT 1",
            rusqlite::params![parent_ino, ino],
            |row| row.get(0),
        )
        .map_err(|_| AgentFSError::FileNotFound {
            path: format!("<ino:{ino}>"),
        })?;
    components.push(name);

    // Walk up to root
    let mut current_ino = parent_ino;
    while current_ino != ROOT_INO {
        let (pname, pparent): (String, i64) = conn
            .query_row(
                "SELECT name, parent_ino FROM fs_dentry WHERE ino = ?1 LIMIT 1",
                [current_ino],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| AgentFSError::FileNotFound {
                path: format!("<ino:{current_ino}>"),
            })?;
        components.push(pname);
        current_ino = pparent;
    }

    components.reverse();
    Ok(format!("/{}", components.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentFSConfig;
    use crate::connection::pool::{ReaderPool, WriterHandle};
    use crate::schema::init_schema;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    async fn setup() -> (AgentFSFileSystem, tempfile::NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path())
            .chunk_size(64)
            .reader_count(2)
            .build();

        {
            let conn = Connection::open(tmp.path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            init_schema(&conn, cfg.chunk_size).unwrap();
        }

        let writer = Arc::new(WriterHandle::open(&cfg).unwrap());
        let readers = Arc::new(ReaderPool::open(&cfg).unwrap());
        let fs = AgentFSFileSystem::new(writer, readers, &cfg).unwrap();
        (fs, tmp)
    }

    #[tokio::test]
    async fn stat_root() {
        let (fs, _tmp) = setup().await;
        let st = fs.stat("/").await.unwrap();
        assert!(st.is_dir());
        assert_eq!(st.ino, 1);
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/hello.txt", b"Hello, world!").await.unwrap();

        let data = fs.read_file("/hello.txt").await.unwrap();
        assert_eq!(data, b"Hello, world!");

        let st = fs.stat("/hello.txt").await.unwrap();
        assert!(st.is_file());
        assert_eq!(st.size, 13);
    }

    #[tokio::test]
    async fn write_nested_creates_parents() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/a/b/c.txt", b"deep").await.unwrap();

        let data = fs.read_file("/a/b/c.txt").await.unwrap();
        assert_eq!(data, b"deep");

        let st = fs.stat("/a").await.unwrap();
        assert!(st.is_dir());
        let st = fs.stat("/a/b").await.unwrap();
        assert!(st.is_dir());
    }

    #[tokio::test]
    async fn readdir() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/a.txt", b"a").await.unwrap();
        fs.write_file("/b.txt", b"b").await.unwrap();
        fs.mkdir("/subdir").await.unwrap();

        let entries = fs.readdir("/").await.unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "subdir"]);
    }

    #[tokio::test]
    async fn remove_file() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/rm_me.txt", b"bye").await.unwrap();
        fs.remove_file("/rm_me.txt").await.unwrap();

        let err = fs.stat("/rm_me.txt").await.unwrap_err();
        assert!(matches!(err, AgentFSError::FileNotFound { .. }));
    }

    #[tokio::test]
    async fn rmdir_empty() {
        let (fs, _tmp) = setup().await;
        fs.mkdir("/empty_dir").await.unwrap();
        fs.rmdir("/empty_dir").await.unwrap();

        let err = fs.stat("/empty_dir").await.unwrap_err();
        assert!(matches!(err, AgentFSError::FileNotFound { .. }));
    }

    #[tokio::test]
    async fn rmdir_nonempty_fails() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/dir/file.txt", b"x").await.unwrap();

        let err = fs.rmdir("/dir").await.unwrap_err();
        assert!(matches!(err, AgentFSError::DirectoryNotEmpty { .. }));
    }

    #[tokio::test]
    async fn overwrite_file() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/f.txt", b"version 1").await.unwrap();
        fs.write_file("/f.txt", b"version 2").await.unwrap();

        let data = fs.read_file("/f.txt").await.unwrap();
        assert_eq!(data, b"version 2");
    }

    #[tokio::test]
    async fn tree_listing() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/a/x.txt", b"x").await.unwrap();
        fs.write_file("/a/y.txt", b"y").await.unwrap();
        fs.write_file("/b.txt", b"b").await.unwrap();

        let tree = fs.tree("/").await.unwrap();
        assert_eq!(tree.name, "/");
        assert_eq!(tree.children.len(), 2);
    }

    #[tokio::test]
    async fn exists() {
        let (fs, _tmp) = setup().await;
        assert!(fs.exists("/").await.unwrap());
        assert!(!fs.exists("/nope.txt").await.unwrap());
        fs.write_file("/yes.txt", b"y").await.unwrap();
        assert!(fs.exists("/yes.txt").await.unwrap());
    }

    #[tokio::test]
    async fn append_file_new() {
        let (fs, _tmp) = setup().await;
        fs.append_file("/log.txt", b"line1\n").await.unwrap();
        let data = fs.read_file("/log.txt").await.unwrap();
        assert_eq!(data, b"line1\n");
    }

    #[tokio::test]
    async fn append_file_existing() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/log.txt", b"aaa").await.unwrap();
        fs.append_file("/log.txt", b"bbb").await.unwrap();
        let data = fs.read_file("/log.txt").await.unwrap();
        assert_eq!(data, b"aaabbb");
    }

    #[tokio::test]
    async fn rename_file() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/old.txt", b"data").await.unwrap();
        fs.rename("/old.txt", "/new.txt").await.unwrap();

        assert!(!fs.exists("/old.txt").await.unwrap());
        let data = fs.read_file("/new.txt").await.unwrap();
        assert_eq!(data, b"data");
    }

    #[tokio::test]
    async fn rename_with_overwrite() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/a.txt", b"aaa").await.unwrap();
        fs.write_file("/b.txt", b"bbb").await.unwrap();
        fs.rename("/a.txt", "/b.txt").await.unwrap();

        let data = fs.read_file("/b.txt").await.unwrap();
        assert_eq!(data, b"aaa");
        assert!(!fs.exists("/a.txt").await.unwrap());
    }

    #[tokio::test]
    async fn remove_tree() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/dir/a/b.txt", b"b").await.unwrap();
        fs.write_file("/dir/c.txt", b"c").await.unwrap();

        fs.remove_tree("/dir").await.unwrap();
        assert!(!fs.exists("/dir").await.unwrap());
        assert!(!fs.exists("/dir/a").await.unwrap());
    }

    #[tokio::test]
    async fn search_by_pattern() {
        let (fs, _tmp) = setup().await;
        fs.write_file("/readme.md", b"r").await.unwrap();
        fs.write_file("/docs/guide.md", b"g").await.unwrap();
        fs.write_file("/code.rs", b"c").await.unwrap();

        let results = fs.search("*.md").await.unwrap();
        assert_eq!(results.len(), 2);
        let paths: Vec<&str> = results.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"/readme.md"));
        assert!(paths.contains(&"/docs/guide.md"));
    }
}
