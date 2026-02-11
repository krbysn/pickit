use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Git command failed: {0}")]
    GitCommand(String),
    #[error("Failed to decode git command output: {0}")]
    OutputDecode(#[from] std::string::FromUtf8Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, Error>;

// Helper function to prepend git config core.quotepath=false
fn git_args_with_quotepath<'a>(args: &'a [&'a str]) -> Vec<&'a str> {
    let mut new_args = Vec::new();
    new_args.push("-c");
    new_args.push("core.quotepath=false"); // Set to false for unescaped output
    new_args.extend_from_slice(args);
    new_args
}

fn run_git_command(args: &[&str], current_dir: Option<&Path>) -> Result<std::process::Output> {
    let mut command = Command::new("git");

    // Always add core.quotepath=false for consistent unescaped output
    let full_args = git_args_with_quotepath(args);
    command.args(&full_args);

    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    // Set environment variables for robust UTF-8 handling as a fallback strategy
    command.env("LANG", "C.UTF-8");
    command.env("LC_ALL", "C.UTF-8");

    let output = command.output()?;
    
    if !output.status.success() {
        return Err(Error::GitCommand(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(output) // Return the full output struct
}



pub fn find_repo_root() -> Result<PathBuf> {
    let output = run_git_command(&["rev-parse", "--show-toplevel"], None)?;
    let s = String::from_utf8(output.stdout)?; // It is expected to be UTF-8
    Ok(PathBuf::from(s.trim()))
}

// Helper to process newline-separated output into a Vec<String> of paths
fn parse_path_lines(output: std::process::Output) -> Result<Vec<String>> {
    let s = String::from_utf8_lossy(&output.stdout).to_string(); // Use lossy for initial String conversion
    Ok(s.lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect())
}

pub fn get_sparse_checkout_list(repo_path: &Path) -> Result<Vec<String>> {
    let output_result = run_git_command(&["sparse-checkout", "list"], Some(repo_path));
    match output_result {
        Ok(output) => {
            // Git's 'sparse-checkout list' now outputs quoted paths when core.quotepath=true.
            // So we can directly parse and use them.
            parse_path_lines(output)
        }
        Err(Error::GitCommand(stderr)) => {
            if stderr.contains("fatal: this worktree is not sparse") {
                Ok(Vec::new())
            } else {
                Err(Error::GitCommand(stderr))
            }
        }
        Err(e) => Err(e),
    }
}

pub fn get_dirs_at_path(path: &str, repo_path: &Path) -> Result<Vec<String>> {
    let target_abs_path = if path.is_empty() || path == "." {
        repo_path.to_path_buf()
    } else {
        repo_path.join(path) // path is already unescaped UTF-8
    };

    let mut direct_children = Vec::new();

    // Strategy 1: Try running ls-tree from the target directory itself (works for physically existing dirs)
    // This is more efficient for checked-out directories.
    if target_abs_path.is_dir() { // Check if the directory physically exists
        let args = vec!["ls-tree", "-r", "--name-only", "-d", "HEAD"];
        let output = run_git_command(&args, Some(&target_abs_path))?;
        let paths_relative_to_target = parse_path_lines(output)?;

        // Filter for direct children (no slashes)
        direct_children = paths_relative_to_target.into_iter().filter(|p| p.find('/').is_none()).collect();
        return Ok(direct_children);
    }

    // Strategy 2: Fallback for virtual directories (not physically checked out)
    // Query all directories recursively from the repository root and filter in Rust.
    // This is necessary because Command::current_dir fails if target_abs_path does not exist.
    let output = run_git_command(&["ls-tree", "-r", "--name-only", "-d", "HEAD"], Some(repo_path))?;
    let all_dirs_from_root = parse_path_lines(output)?; // These are unescaped UTF-8 strings

    let search_prefix = if path.is_empty() || path == "." {
        "".to_string()
    } else {
        format!("{}/", path)
    };

    for dir in all_dirs_from_root {
        if dir.starts_with(&search_prefix) {
            let suffix = &dir[search_prefix.len()..];
            if !suffix.is_empty() && suffix.find('/').is_none() {
                direct_children.push(suffix.to_string());
            }
        }
    }
    Ok(direct_children)
}

#[allow(dead_code)]
pub fn get_all_directories_recursive(repo_path: &Path) -> Result<Vec<String>> {
    let output = run_git_command(&["ls-tree", "-r", "--name-only", "-d", "HEAD"], Some(repo_path))?;
    parse_path_lines(output) // Returns Vec<String> of unquoted paths
}

use std::collections::HashSet;

pub fn get_uncommitted_paths(repo_path: &Path) -> Result<HashSet<String>> {
    let mut uncommitted_paths = HashSet::new();

    // Get modified and staged files using git diff --name-only HEAD
    let output = run_git_command(&["diff", "--name-only", "HEAD"], Some(repo_path))?;
    let modified_paths = parse_path_lines(output)?;
    uncommitted_paths.extend(modified_paths.into_iter());

    // Get untracked files using git ls-files --others --exclude-standard
    let output = run_git_command(&["ls-files", "--others", "--exclude-standard"], Some(repo_path))?;
    let untracked_paths = parse_path_lines(output)?;
    uncommitted_paths.extend(untracked_paths.into_iter());

    Ok(uncommitted_paths)
}

pub fn set_sparse_checkout_dirs(dirs: Vec<String>, repo_path: &Path) -> Result<()> {
    let mut args = vec!["sparse-checkout", "set"];
    
    let dirs_as_strs: Vec<&str> = dirs.iter().map(|s| s.as_str()).collect();
    args.extend(dirs_as_strs);

    run_git_command(&args, Some(repo_path))?;
    Ok(())
}