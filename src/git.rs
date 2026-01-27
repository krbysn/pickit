use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Git command failed: {0}")]
    GitCommand(String),
    #[error("Failed to decode git command output")]
    OutputDecode(#[from] std::string::FromUtf8Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, Error>;

fn run_git_command(args: &[&str], current_dir: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    let output = command.output()?;
    
    if !output.status.success() {
        return Err(Error::GitCommand(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(String::from_utf8(output.stdout)?)
}

pub fn find_repo_root() -> Result<PathBuf> {
    let output = run_git_command(&["rev-parse", "--show-toplevel"], None)?;
    Ok(PathBuf::from(output.trim()))
}

pub fn get_sparse_checkout_list(repo_path: &Path) -> Result<Vec<String>> {
    let output = run_git_command(&["sparse-checkout", "list"], Some(repo_path))?;
    Ok(output
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

pub fn get_dirs_at_path(path: &str, repo_path: &Path) -> Result<Vec<String>> {
    let tree_ish = if path.is_empty() || path == "." {
        "HEAD".to_string()
    } else {
        format!("HEAD:{}", path)
    };

    let output = run_git_command(&["ls-tree", "--name-only", "-d", &tree_ish], Some(repo_path));

    // ls-tree returns a non-zero exit code if the path doesn't exist or has no directories,
    // which is not a "real" error for us. We just want to return an empty Vec.
    let output = match output {
        Ok(out) => out,
        Err(Error::GitCommand(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    Ok(output
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| {
            if path.is_empty() || path == "." {
                s.to_string()
            } else {
                // ls-tree with `HEAD:path` returns just the basename, so we prepend the path
                format!("{}/{}", path, s)
            }
        })
        .collect())
}

use std::collections::HashSet;

pub fn get_uncommitted_paths(repo_path: &Path) -> Result<HashSet<PathBuf>> {
    let output = run_git_command(&["status", "--porcelain=v1"], Some(repo_path))?;
    let paths = output
        .lines()
        .filter_map(|line| {
            // Each line is like "XY <path>" or "?? <path>"
            // We need to grab the path part after the status codes
            line.split_whitespace().last().map(PathBuf::from)
        })
        .collect();
    Ok(paths)
}

pub fn set_sparse_checkout_dirs(dirs: Vec<String>, repo_path: &Path) -> Result<()> {
    let mut args = vec!["sparse-checkout", "set"];
    // This is a workaround because `dirs` is Vec<String> and `args` is Vec<&str>
    let dirs_as_strs: Vec<&str> = dirs.iter().map(|s| s.as_str()).collect();
    args.extend(dirs_as_strs);

    run_git_command(&args, Some(repo_path))?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    pub fn setup_git_repo() -> (PathBuf, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        Command::new("git")
            .arg("init")
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["config", "user.email", "test@example.com"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()
            .unwrap();
        (path, dir)
    }

    fn create_and_commit_files(repo_path: &PathBuf) {
        fs::create_dir_all(repo_path.join("src")).unwrap();
        fs::write(repo_path.join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(repo_path.join("src/components")).unwrap(); // Add src/components
        fs::write(repo_path.join("src/components/mod.rs"), "pub fn foo() {}").unwrap(); // Add a file in src/components
        fs::create_dir_all(repo_path.join("docs")).unwrap();
        fs::write(repo_path.join("docs/README.md"), "# Docs").unwrap();
        fs::create_dir_all(repo_path.join("tests")).unwrap();
        fs::write(repo_path.join("tests/test.rs"), "# Tests").unwrap();
        fs::write(repo_path.join(".gitignore"), "target/").unwrap();

        Command::new("git")
            .args(&["add", "."])
            .current_dir(repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["commit", "-m", "Initial commit"])
            .current_dir(repo_path)
            .output()
            .unwrap();
    }

    #[test]
    fn test_find_repo_root() {
        // This test runs in the actual pickit repo
        let root = find_repo_root().unwrap();
        assert!(root.to_string_lossy().ends_with("pickit")); // Assuming 'pickit' is the repo name
        assert!(root.is_dir());
    }

    pub fn setup_git_repo_with_subdirs() -> (PathBuf, tempfile::TempDir) {
        let (repo_path, temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path); // Use the helper that creates src, docs, tests, and src/components
        // No need for separate add/commit here, as create_and_commit_files handles it
        (repo_path, temp_dir)
    }

    #[test]
    fn test_get_dirs_at_path() {
        let (repo_path, _temp_dir) = setup_git_repo_with_subdirs();

        // Test at root
        let root_dirs = get_dirs_at_path("", &repo_path).unwrap();
        assert_eq!(root_dirs, vec!["docs", "src", "tests"]);

        // Test at a subdirectory
        let src_dirs = get_dirs_at_path("src", &repo_path).unwrap();
        assert_eq!(src_dirs, vec!["src/components"]);

        // Test at a directory with no subdirectories
        let docs_dirs = get_dirs_at_path("docs", &repo_path).unwrap();
        assert!(docs_dirs.is_empty());
        
        let components_dirs = get_dirs_at_path("src/components", &repo_path).unwrap();
        assert!(components_dirs.is_empty());
    }

    #[test]
    fn test_get_sparse_checkout_list() {
        let (repo_path, _temp_dir) = setup_git_repo(); // Capture _temp_dir
        create_and_commit_files(&repo_path);

        let _ = Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed")
            .status
            .success();

        let _ = Command::new("git")
            .args(&["sparse-checkout", "set", "src", "docs"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout set failed")
            .status
            .success();
        
        let sparse_dirs = get_sparse_checkout_list(&repo_path).unwrap();
        assert!(sparse_dirs.contains(&"src".to_string()));
        assert!(sparse_dirs.contains(&"docs".to_string()));
        assert!(!sparse_dirs.contains(&"tests".to_string()));
        assert_eq!(sparse_dirs.len(), 2);
    }

    #[test]
    fn test_get_uncommitted_paths() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);

        // No changes initially
        let changes = get_uncommitted_paths(&repo_path).unwrap();
        assert!(changes.is_empty());

        // Create a new untracked file
        fs::write(repo_path.join("untracked.txt"), "untracked").unwrap();
        // Modify an existing file
        fs::write(repo_path.join("src/main.rs"), "fn main() { /* changed */ }").unwrap();

        let changes = get_uncommitted_paths(&repo_path).unwrap();
        assert_eq!(changes.len(), 2);
        assert!(changes.contains(&PathBuf::from("untracked.txt")));
        assert!(changes.contains(&PathBuf::from("src/main.rs")));
    }
}


