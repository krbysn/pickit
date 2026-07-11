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
            if stderr.contains("this worktree is not sparse") {
                Ok(Vec::new())
            } else {
                Err(Error::GitCommand(stderr))
            }
        }
        Err(e) => Err(e),
    }
}

pub fn is_sparse_checkout_enabled(repo_path: &Path) -> Result<bool> {
    let output = run_git_command(&["config", "--get", "core.sparseCheckout"], Some(repo_path));
    match output {
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).trim() == "true"),
        Err(_) => Ok(false), // If the config doesn't exist, it's not enabled
    }
}

pub fn get_top_level_directories(repo_path: &Path) -> Result<Vec<String>> {
    // Get all directories at the root level using ls-tree
    let output = run_git_command(&["ls-tree", "--name-only", "-d", "HEAD"], Some(repo_path))?;
    parse_path_lines(output)
}

pub fn init_sparse_checkout_cone(repo_path: &Path) -> Result<()> {
    run_git_command(&["sparse-checkout", "init", "--cone"], Some(repo_path))?;
    Ok(())
}

pub fn get_dirs_at_path(path: &str, repo_path: &Path) -> Result<Vec<String>> {
    let mut args = vec!["ls-tree", "-d", "--name-only", "HEAD"];
    
    // If we're looking at a subdirectory, query only its immediate contents
    let path_arg;
    if !path.is_empty() && path != "." {
        path_arg = if path.ends_with('/') {
            path.to_string()
        } else {
            format!("{}/", path)
        };
        args.push(&path_arg);
    }

    let output = run_git_command(&args, Some(repo_path))?;
    let lines = parse_path_lines(output)?;
    
    let mut direct_children = Vec::new();
    for line in lines {
        // Extract the simple directory name (strip parent prefix if present)
        let simple_name = if !path.is_empty() && path != "." {
            let prefix = if path.ends_with('/') {
                path.to_string()
            } else {
                format!("{}/", path)
            };
            if line.starts_with(&prefix) {
                line[prefix.len()..].to_string()
            } else {
                // Fallback: take part after the last slash
                if let Some(idx) = line.rfind('/') {
                    line[idx + 1..].to_string()
                } else {
                    line
                }
            }
        } else {
            line
        };
        
        if !simple_name.is_empty() {
            direct_children.push(simple_name);
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
    uncommitted_paths.extend(modified_paths);

    // Get untracked files using git ls-files --others --exclude-standard
    let output = run_git_command(&["ls-files", "--others", "--exclude-standard"], Some(repo_path))?;
    let untracked_paths = parse_path_lines(output)?;
    uncommitted_paths.extend(untracked_paths);

    Ok(uncommitted_paths)
}

pub fn set_sparse_checkout_dirs(dirs: Vec<String>, repo_path: &Path) -> Result<()> {
    let mut args = vec!["sparse-checkout", "set"];
    
    let dirs_as_strs: Vec<&str> = dirs.iter().map(|s| s.as_str()).collect();
    args.extend(dirs_as_strs);

    run_git_command(&args, Some(repo_path))?;
    Ok(())
}