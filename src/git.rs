use std::path::{Path, PathBuf};
use std::process::Command;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt; // Add this import
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

fn run_git_command(args: &[&str], current_dir: Option<&Path>) -> Result<std::process::Output> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    // Set environment variables for robust UTF-8 handling as a fallback strategy
    command.env("LANG", "C.UTF-8");
    command.env("LC_ALL", "C.UTF-8");

    let output = command.output()?;
    
    if !output.status.success() {
        return Err(Error::GitCommand(
            // Always use lossy conversion for error messages as they might not be critical data
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

pub fn get_sparse_checkout_list(repo_path: &Path) -> Result<Vec<String>> {
    let output_result = run_git_command(&["sparse-checkout", "list"], Some(repo_path));
    match output_result {
        Ok(output) => {
            let s = String::from_utf8(output.stdout).map_err(Error::OutputDecode)?;
            Ok(s.lines()
                .filter(|s| !s.is_empty())
                .map(unescape_git_path_string) // Apply unescaping here
                .collect())
        }
        Err(Error::GitCommand(stderr)) => {
            if stderr.contains("fatal: this worktree is not sparse") {
                // If sparse-checkout is not initialized, return an empty list
                Ok(Vec::new())
            } else {
                // Otherwise, propagate the error
                Err(Error::GitCommand(stderr))
            }
        }
        Err(e) => Err(e), // Propagate other types of errors
    }
}

pub fn get_dirs_at_path(path: &str, repo_path: &Path) -> Result<Vec<PathBuf>> {
    let tree_ish = if path.is_empty() || path == "." {
        "HEAD".to_string()
    } else {
        format!("HEAD:{}", path)
    };

    let output = run_git_command(&["ls-tree", "-z", "--name-only", "-d", &tree_ish], Some(repo_path))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("fatal: Path") && stderr.contains("does not exist") {
            return Ok(Vec::new());
        } else {
            return Err(Error::GitCommand(stderr.to_string()));
        }
    }

    let paths = output.stdout
        .split(|&b| b == 0)
        .filter(|&s| !s.is_empty())
        .map(|s| PathBuf::from(OsString::from_vec(s.to_vec())))
        .map(|pb| {
            if path.is_empty() || path == "." {
                pb
            } else {
                let mut full_path = PathBuf::from(path);
                full_path.push(pb);
                full_path
            }
        })
        .collect();
    Ok(paths)
}

pub fn get_all_directories_recursive(repo_path: &Path) -> Result<Vec<PathBuf>> {
    let output = run_git_command(&["ls-tree", "-r", "-z", "--name-only", "-d", "HEAD"], Some(repo_path))?;
    let paths = output.stdout
        .split(|&b| b == 0)
        .filter(|&s| !s.is_empty())
        .map(|s| PathBuf::from(OsString::from_vec(s.to_vec())))
        .collect();
    Ok(paths)
}

use std::collections::HashSet;

pub fn get_uncommitted_paths(repo_path: &Path) -> Result<HashSet<PathBuf>> {
    let mut uncommitted_paths = HashSet::new();

    // Get modified and staged files using git diff --name-only -z HEAD
    let modified_output = run_git_command(&["diff", "--name-only", "-z", "HEAD"], Some(repo_path))?;
    modified_output.stdout
        .split(|&b| b == 0)
        .filter(|&s| !s.is_empty())
        .map(|s| PathBuf::from(OsString::from_vec(s.to_vec())))
        .for_each(|p| {
            uncommitted_paths.insert(p);
        });

    // Get untracked files using git ls-files -z --others --exclude-standard
    let untracked_output = run_git_command(&["ls-files", "-z", "--others", "--exclude-standard"], Some(repo_path))?;
    untracked_output.stdout
        .split(|&b| b == 0)
        .filter(|&s| !s.is_empty())
        .map(|s| PathBuf::from(OsString::from_vec(s.to_vec())))
        .for_each(|p| {
            uncommitted_paths.insert(p);
        });

    Ok(uncommitted_paths)
}

pub fn set_sparse_checkout_dirs(dirs: Vec<String>, repo_path: &Path) -> Result<()> {
    let mut args = vec!["sparse-checkout", "set"];
    let dirs_as_strs: Vec<&str> = dirs.iter().map(|s| s.as_str()).collect();
    args.extend(dirs_as_strs);

    run_git_command(&args, Some(repo_path))?; // We just care about status, not stdout
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
        // Prevent git from quoting paths, which simplifies parsing.
        Command::new("git")
            .args(&["config", "core.quotePath", "false"])
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
        let expected_root_dirs: Vec<PathBuf> = vec!["docs", "src", "tests"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(root_dirs, expected_root_dirs);

        // Test at a subdirectory
        let src_dirs = get_dirs_at_path("src", &repo_path).unwrap();
        let expected_src_dirs: Vec<PathBuf> = vec!["src/components"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(src_dirs, expected_src_dirs);

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

    #[test]
    fn test_japanese_filenames() {
        let (repo_path, _temp_dir) = setup_git_repo();

        // Create directories and files with Japanese names
        fs::create_dir_all(repo_path.join("日本語ディレクトリ")).unwrap();
        fs::write(repo_path.join("日本語ディレクトリ/ファイル.txt"), "japanese content").unwrap();
        fs::create_dir_all(repo_path.join("別のフォルダ")).unwrap();
        fs::write(repo_path.join("別のフォルダ/テスト.md"), "test content").unwrap();

        // Add and commit
        Command::new("git")
            .args(&["add", "."])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["commit", "-m", "Add Japanese files"])
            .current_dir(&repo_path)
            .output()
            .unwrap();

        // Test get_dirs_at_path with Japanese directory
        let root_dirs = get_dirs_at_path("", &repo_path).unwrap();
        let expected_root_dirs: HashSet<PathBuf> = [
            PathBuf::from("日本語ディレクトリ"),
            PathBuf::from("別のフォルダ"),
        ].iter().cloned().collect();
        let actual_root_dirs: HashSet<PathBuf> = root_dirs.into_iter().collect();
        assert_eq!(actual_root_dirs, expected_root_dirs);

        // Test get_all_directories_recursive with Japanese directories
        let all_dirs = get_all_directories_recursive(&repo_path).unwrap();
        let expected_all_dirs: HashSet<PathBuf> = [
            PathBuf::from("日本語ディレクトリ"),
            PathBuf::from("別のフォルダ"),
        ].iter().cloned().collect();
        let actual_all_dirs: HashSet<PathBuf> = all_dirs.into_iter().collect();
        assert_eq!(actual_all_dirs, expected_all_dirs);

        // Test get_uncommitted_paths after modifying a Japanese named file
        fs::write(repo_path.join("日本語ディレクトリ/ファイル.txt"), "modified content").unwrap();
        let uncommitted = get_uncommitted_paths(&repo_path).unwrap();
        assert!(uncommitted.contains(&PathBuf::from("日本語ディレクトリ/ファイル.txt")));
        assert_eq!(uncommitted.len(), 1);

        // Test sparse-checkout with Japanese path
        Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed");

        let sparse_checkout_set_dirs = vec![PathBuf::from("日本語ディレクトリ").to_string_lossy().to_string()];
        set_sparse_checkout_dirs(sparse_checkout_set_dirs.clone(), &repo_path).unwrap();
        let sparse_list = get_sparse_checkout_list(&repo_path).unwrap();
        assert!(sparse_list.contains(&"日本語ディレクトリ".to_string()));

        // Verify the file exists after sparse checkout
        assert!(repo_path.join("日本語ディレクトリ/ファイル.txt").exists());
        assert!(!repo_path.join("別のフォルダ/テスト.md").exists()); // Should not exist
    }
}


