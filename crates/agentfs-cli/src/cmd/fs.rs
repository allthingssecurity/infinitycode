use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use clap::Subcommand;
use comfy_table::{Table, presets::UTF8_FULL_CONDENSED};

#[derive(Subcommand)]
pub enum FsCommands {
    /// List directory contents
    Ls {
        /// Path to the database
        db: PathBuf,
        /// Directory path (default: /)
        #[arg(default_value = "/")]
        path: String,
    },
    /// Print file contents
    Cat {
        /// Path to the database
        db: PathBuf,
        /// File path
        path: String,
    },
    /// Write data to a file
    Write {
        /// Path to the database
        db: PathBuf,
        /// File path
        path: String,
        /// Content to write (use - for stdin)
        content: String,
    },
    /// Append data to a file
    Append {
        /// Path to the database
        db: PathBuf,
        /// File path
        path: String,
        /// Content to append
        content: String,
    },
    /// Remove a file
    Rm {
        /// Path to the database
        db: PathBuf,
        /// File path
        path: String,
    },
    /// Create a directory
    Mkdir {
        /// Path to the database
        db: PathBuf,
        /// Directory path
        path: String,
    },
    /// Show file/directory metadata
    Stat {
        /// Path to the database
        db: PathBuf,
        /// Path to stat
        path: String,
    },
    /// Recursive directory tree
    Tree {
        /// Path to the database
        db: PathBuf,
        /// Root path (default: /)
        #[arg(default_value = "/")]
        path: String,
    },
    /// Move/rename a file or directory
    Mv {
        /// Path to the database
        db: PathBuf,
        /// Source path
        from: String,
        /// Destination path
        to: String,
    },
    /// Recursively remove a directory and all contents
    Rmtree {
        /// Path to the database
        db: PathBuf,
        /// Directory path
        path: String,
    },
    /// Search for files matching a glob pattern
    Search {
        /// Path to the database
        db: PathBuf,
        /// Glob pattern (e.g., *.rs, config*)
        pattern: String,
    },
}

pub async fn run(cmd: FsCommands, json: bool) -> anyhow::Result<()> {
    match cmd {
        FsCommands::Ls { db, path } => {
            let afs = open_db(&db).await?;
            let entries = afs.fs.readdir(&path).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["Name", "Ino", "Type"]);

                for entry in &entries {
                    let ftype = if (entry.mode & 0o170000) == 0o040000 {
                        "dir"
                    } else if (entry.mode & 0o170000) == 0o120000 {
                        "link"
                    } else {
                        "file"
                    };
                    table.add_row(vec![&entry.name, &entry.ino.to_string(), ftype]);
                }

                println!("{table}");
            }
            afs.close().await?;
        }
        FsCommands::Cat { db, path } => {
            let afs = open_db(&db).await?;
            let data = afs.fs.read_file(&path).await?;
            if json {
                let text = String::from_utf8_lossy(&data);
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "content": text }))?);
            } else {
                let text = String::from_utf8_lossy(&data);
                print!("{text}");
            }
            afs.close().await?;
        }
        FsCommands::Write { db, path, content } => {
            let afs = open_db(&db).await?;
            let data = if content == "-" {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                content
            };
            afs.fs.write_file(&path, data.as_bytes()).await?;
            if json {
                println!("{}", serde_json::json!({ "written": data.len(), "path": path }));
            } else {
                println!("Wrote {} bytes to {path}", data.len());
            }
            afs.close().await?;
        }
        FsCommands::Append { db, path, content } => {
            let afs = open_db(&db).await?;
            afs.fs.append_file(&path, content.as_bytes()).await?;
            if json {
                println!("{}", serde_json::json!({ "appended": content.len(), "path": path }));
            } else {
                println!("Appended {} bytes to {path}", content.len());
            }
            afs.close().await?;
        }
        FsCommands::Rm { db, path } => {
            let afs = open_db(&db).await?;
            afs.fs.remove_file(&path).await?;
            if json {
                println!("{}", serde_json::json!({ "removed": path }));
            } else {
                println!("Removed {path}");
            }
            afs.close().await?;
        }
        FsCommands::Mkdir { db, path } => {
            let afs = open_db(&db).await?;
            afs.fs.mkdir(&path).await?;
            if json {
                println!("{}", serde_json::json!({ "created": path }));
            } else {
                println!("Created directory {path}");
            }
            afs.close().await?;
        }
        FsCommands::Stat { db, path } => {
            let afs = open_db(&db).await?;
            let st = afs.fs.stat(&path).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!("  Path:    {path}");
                println!("  Ino:     {}", st.ino);
                println!("  Mode:    {} ({:#o})", st.mode_string(), st.mode);
                println!("  Size:    {}", st.size);
                println!("  Nlink:   {}", st.nlink);
                println!("  Ctime:   {}", st.ctime);
                println!("  Mtime:   {}", st.mtime);
                println!("  Atime:   {}", st.atime);
            }
            afs.close().await?;
        }
        FsCommands::Tree { db, path } => {
            let afs = open_db(&db).await?;
            let tree = afs.fs.tree(&path).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&tree)?);
            } else {
                print_tree(&tree, "", true);
            }
            afs.close().await?;
        }
        FsCommands::Mv { db, from, to } => {
            let afs = open_db(&db).await?;
            afs.fs.rename(&from, &to).await?;
            if json {
                println!("{}", serde_json::json!({ "renamed": { "from": from, "to": to } }));
            } else {
                println!("Moved {from} → {to}");
            }
            afs.close().await?;
        }
        FsCommands::Rmtree { db, path } => {
            let afs = open_db(&db).await?;
            afs.fs.remove_tree(&path).await?;
            if json {
                println!("{}", serde_json::json!({ "removed_tree": path }));
            } else {
                println!("Removed tree {path}");
            }
            afs.close().await?;
        }
        FsCommands::Search { db, pattern } => {
            let afs = open_db(&db).await?;
            let results = afs.fs.search(&pattern).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec!["Path", "Type", "Size"]);
                for r in &results {
                    let ftype = if r.is_dir { "dir" } else { "file" };
                    table.add_row(vec![&r.path, ftype, &r.size.to_string()]);
                }
                println!("{table}");
            }
            afs.close().await?;
        }
    }
    Ok(())
}

fn print_tree(node: &agentfs_core::filesystem::TreeNode, prefix: &str, is_last: bool) {
    let connector = if prefix.is_empty() {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };

    let type_indicator = if node.stat.is_dir() { "/" } else { "" };
    println!("{prefix}{connector}{}{type_indicator}", node.name);

    let child_prefix = if prefix.is_empty() {
        if is_last { "    ".to_string() } else { "│   ".to_string() }
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    for (i, child) in node.children.iter().enumerate() {
        let last = i == node.children.len() - 1;
        print_tree(child, &child_prefix, last);
    }
}

async fn open_db(path: &PathBuf) -> anyhow::Result<agentfs_core::AgentFS> {
    let config = AgentFSConfig::builder(path)
        .checkpoint_interval_secs(0)
        .build();
    Ok(agentfs_core::AgentFS::open(config).await?)
}
