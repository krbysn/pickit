use std::path::PathBuf;
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

fn run_git_command(args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).output()?;

    if !output.status.success() {
        return Err(Error::GitCommand(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(String::from_utf8(output.stdout)?)
}

pub fn find_repo_root() -> Result<PathBuf> {
    let output = run_git_command(&["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(output.trim()))
}

pub fn get_sparse_checkout_list() -> Result<Vec<String>> {
    let output = run_git_command(&["sparse-checkout", "list"])?;
    Ok(output
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

pub fn get_all_dirs() -> Result<Vec<String>> {
    let output = run_git_command(&["ls-tree", "-r", "--name-only", "-d", "HEAD"])?;
    Ok(output
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

pub fn has_uncommitted_changes(path: &PathBuf) -> Result<bool> {
    let path_str = path.to_string_lossy();
    let output = dbg!(run_git_command(&["status", "--porcelain", path_str.as_ref()]))?;
    Ok(dbg!(!output.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn setup_git_repo() -> (PathBuf, tempfile::TempDir) {
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

    #[test]
    fn test_get_all_dirs() {
        let (repo_path, _temp_dir) = setup_git_repo(); // Capture _temp_dir
        create_and_commit_files(&repo_path);

        let output = Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        assert!(output.status.success());
        
        // Temporarily change directory for the test
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&repo_path).unwrap();

        let dirs = get_all_dirs().unwrap();
        assert!(dirs.contains(&"src".to_string()));
        assert!(dirs.contains(&"docs".to_string()));
        assert!(dirs.contains(&"tests".to_string()));
        assert!(!dirs.contains(&"src/main.rs".to_string())); // Should not contain files
        assert_eq!(dirs.len(), 3); // src, docs, tests

        std::env::set_current_dir(&original_dir).unwrap(); // Restore original directory
    }

    #[test]
    fn test_get_sparse_checkout_list() {
        let (repo_path, _temp_dir) = setup_git_repo(); // Capture _temp_dir
        create_and_commit_files(&repo_path);

        let output = Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        assert!(output.status.success());

        Command::new("git")
            .args(&["sparse-checkout", "set", "src", "docs"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        
        // Temporarily change directory for the test
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&repo_path).unwrap();

        let sparse_dirs = get_sparse_checkout_list().unwrap();
        assert!(sparse_dirs.contains(&"src".to_string()));
        assert!(sparse_dirs.contains(&"docs".to_string()));
        assert!(!sparse_dirs.contains(&"tests".to_string()));
        assert_eq!(sparse_dirs.len(), 2);

        std::env::set_current_dir(&original_dir).unwrap(); // Restore original directory
    }

    #[test]
    fn test_has_uncommitted_changes() {
        let (repo_path, _temp_dir) = setup_git_repo(); // Capture _temp_dir
        create_and_commit_files(&repo_path);

        // No changes
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&repo_path).unwrap();
        dbg!(has_uncommitted_changes(&PathBuf::from(".")).unwrap());
        assert!(!has_uncommitted_changes(&PathBuf::from(".")).unwrap());
        dbg!(has_uncommitted_changes(&PathBuf::from("src")).unwrap());
        assert!(!has_uncommitted_changes(&PathBuf::from("src")).unwrap());
        std::env::set_current_dir(&original_dir).unwrap();


        // With changes
        fs::write(repo_path.join("src/main.rs"), "fn main() { println!(\"Hello\"); }").unwrap();
        std::env::set_current_dir(&repo_path).unwrap();
        dbg!(has_uncommitted_changes(&PathBuf::from(".")).unwrap());
        assert!(has_uncommitted_changes(&PathBuf::from(".")).unwrap());
        dbg!(has_uncommitted_changes(&PathBuf::from("src")).unwrap());
        assert!(has_uncommitted_changes(&PathBuf::from("src")).unwrap());
        dbg!(has_uncommitted_changes(&PathBuf::from("docs")).unwrap());
        assert!(!has_uncommitted_changes(&PathBuf::from("docs")).unwrap()); // No changes in docs
        std::env::set_current_dir(&original_dir).unwrap();
    }
}


