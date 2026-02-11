use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Git command failed: {0}")]
    GitCommand(String),
    #[error("Failed to decode git command output: {0}")]
    OutputDecode(#[from] std::string::FromUtf8Error),
    #[error("Invalid path string: {0}")]
    InvalidPath(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, Error>;

// Helper function to prepend git config core.quotepath=true
fn git_args_with_quotepath<'a>(args: &'a [&'a str]) -> Vec<&'a str> {
    let mut new_args = Vec::new();
    new_args.push("-c");
    new_args.push("core.quotepath=true");
    new_args.extend_from_slice(args);
    new_args
}

fn run_git_command(args: &[&str], current_dir: Option<&Path>) -> Result<std::process::Output> {
    let mut command = Command::new("git");

    // Only add core.quotepath=true for commands that benefit from it and where we parse output as quoted lines.
    // `rev-parse` outputs absolute system paths, which shouldn't be quoted for our purposes.
    let full_args = if args.first() == Some(&"rev-parse") {
        args.to_vec()
    } else {
        git_args_with_quotepath(args)
    };
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

// Helper function to unescape Git's quoted paths.
// Git output can be complex:
// - It might be double-quoted (e.g., `""path""`).
// - It might contain octal escapes (`\ddd`).
// - It might contain C-style escapes (`\n`, `\t`, `\\`, `\"`).
// This function aims to robustly convert such a string back to its original form.
fn unescape_git_path_string(s: &str) -> String {
    let mut unescape_target = s.trim();

    // Remove outer quotes if present. Git sometimes double-quotes.
    if unescape_target.starts_with('"') && unescape_target.ends_with('"') && unescape_target.len() >= 2 {
        unescape_target = &unescape_target[1..unescape_target.len() - 1];
    }

    let mut result_bytes = Vec::new();
    let mut chars = unescape_target.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            // Check for escape sequences
            if let Some(&next_c) = chars.peek() {
                match next_c {
                    'n' => { result_bytes.push(b'\n'); chars.next(); },
                    't' => { result_bytes.push(b'\t'); chars.next(); },
                    'r' => { result_bytes.push(b'\r'); chars.next(); },
                    '\\' => { result_bytes.push(b'\\'); chars.next(); },
                    '"' => { result_bytes.push(b'"'); chars.next(); },
                    // Handle octal escapes: \ddd
                    d1 @ '0'..='7' => {
                        chars.next(); // Consume d1
                        let d2_opt = chars.next();
                        let d3_opt = chars.next();

                        if let (Some(d2), Some(d3)) = (d2_opt, d3_opt) {
                            if d2.is_ascii_digit() && d3.is_ascii_digit() {
                                let octal_str = format!("{}{}{}", d1, d2, d3);
                                if let Ok(byte_val) = u8::from_str_radix(&octal_str, 8) {
                                    result_bytes.push(byte_val);
                                    continue; // Go to next char in outer loop
                                }
                            }
                        }
                        // If not a valid 3-digit octal, push '\' and the digits literally.
                        result_bytes.push(b'\\');
                        result_bytes.extend_from_slice(d1.to_string().as_bytes());
                        if let Some(d2_val) = d2_opt {
                            result_bytes.extend_from_slice(d2_val.to_string().as_bytes());
                        }
                        if let Some(d3_val) = d3_opt {
                            result_bytes.extend_from_slice(d3_val.to_string().as_bytes());
                        }
                    },
                    _ => {
                        // Unrecognized escape sequence, treat as literal backslash and the character after it
                        result_bytes.push(b'\\');
                        result_bytes.extend_from_slice(next_c.to_string().as_bytes());
                        chars.next();
                    }
                }
            } else {
                // Backslash at the end of the string, treat as literal
                result_bytes.push(b'\\');
            }
        } else {
            // Not an escape sequence, push the UTF-8 bytes of the character
            let mut buf = [0; 4];
            let encoded = c.encode_utf8(&mut buf);
            result_bytes.extend_from_slice(encoded.as_bytes());
        }
    }
    
    // Attempt to interpret the bytes as UTF-8. Use lossy conversion for robustness.
    String::from_utf8_lossy(&result_bytes).to_string()
}

pub fn find_repo_root() -> Result<PathBuf> {
    let output = run_git_command(&["rev-parse", "--show-toplevel"], None)?;
    let s = String::from_utf8(output.stdout)?; // It is expected to be UTF-8
    Ok(PathBuf::from(s.trim()))
}

// Helper to process newline-separated output into a Vec<String> of quoted paths
fn parse_quoted_path_lines(output: std::process::Output) -> Result<Vec<String>> {
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
            let unquoted_paths = parse_quoted_path_lines(output)?;
            // Manually quote the paths to store them in the internal quoted format
            Ok(unquoted_paths.into_iter().map(|p| quote_path_string(&p)).collect())
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
    let mut args = vec!["ls-tree", "-r", "--name-only", "-d"];
    let tree_ish = if path.is_empty() || path == "." {
        "HEAD".to_string()
    } else {
        format!("HEAD:{}", path)
    };
    args.push(&tree_ish);

    let output = run_git_command(&args, Some(repo_path))?;
    let mut paths = parse_quoted_path_lines(output)?;

    // Filter to only include direct children, and ensure they are relative to the current path
    // git ls-tree -r always gives paths relative to repo root.
    // We need to filter for direct children of 'path'
    let prefix = if path.is_empty() || path == "." {
        "".to_string()
    } else {
        format!("{}/", path)
    };

    paths.retain(|p| {
        p.starts_with(&prefix) &&
        p.len() > prefix.len() && // Ensure it's not the prefix itself
        p[prefix.len()..].find('/').is_none() // Ensure no further slashes
    });

    // Remove the prefix from the paths, so they are just the direct directory names (quoted)
    paths.iter_mut().for_each(|p| {
        *p = p[prefix.len()..].to_string();
    });

    Ok(paths)
}

pub fn get_all_directories_recursive(repo_path: &Path) -> Result<Vec<String>> {
    let output = run_git_command(&["ls-tree", "-r", "--name-only", "-d", "HEAD"], Some(repo_path))?;
    parse_quoted_path_lines(output) // Returns Vec<String> of quoted paths
}

use std::collections::HashSet;

pub fn get_uncommitted_paths(repo_path: &Path) -> Result<HashSet<String>> {
    let mut uncommitted_paths = HashSet::new();

    // Get modified and staged files using git diff --name-only HEAD
    let output = run_git_command(&["diff", "--name-only", "HEAD"], Some(repo_path))?;
    let modified_paths = parse_quoted_path_lines(output)?;
    uncommitted_paths.extend(modified_paths.into_iter());

    // Get untracked files using git ls-files --others --exclude-standard
    let output = run_git_command(&["ls-files", "--others", "--exclude-standard"], Some(repo_path))?;
    let untracked_paths = parse_quoted_path_lines(output)?;
    uncommitted_paths.extend(untracked_paths.into_iter());

    Ok(uncommitted_paths)
}

pub fn set_sparse_checkout_dirs(dirs: Vec<String>, repo_path: &Path) -> Result<()> {
    let mut args = vec!["sparse-checkout", "set"];
    let dirs_as_strs: Vec<&str> = dirs.iter().map(|s| s.as_str()).collect();
    args.extend(dirs_as_strs);

    // Pass `dirs` directly (these are already quoted strings from internal representation).
    // Git is expected to unquote them as needed when processing the command.
    run_git_command(&args, Some(repo_path))?;
    Ok(())
}




