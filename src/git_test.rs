#[cfg(test)]
mod tests {
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
        // Ensure core.quotepath is true for consistent test output
        Command::new("git")
            .args(&["config", "core.quotepath", "true"])
            .current_dir(&path)
            .output()
            .unwrap();
        (path, dir)
    }

    fn create_and_commit_files(repo_path: &PathBuf) {
        fs::create_dir_all(repo_path.join("src")).unwrap();
        fs::write(repo_path.join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(repo_path.join("src/components")).unwrap();
        fs::write(repo_path.join("src/components/mod.rs"), "pub fn foo() {}").unwrap();
        fs::create_dir_all(repo_path.join("docs")).unwrap();
        fs::write(repo_path.join("docs/README.md"), "# Docs").unwrap();
        fs::create_راحfs::create_dir_all(repo_path.join("tests")).unwrap();
        fs::write(repo_path.join("tests/test.rs"), "# Tests").unwrap();
        fs::write(repo_path.join(".gitignore"), "target/").unwrap();
        fs::create_dir_all(repo_path.join("dir with spaces")).unwrap(); // Directory with spaces
        fs::write(repo_path.join("dir with spaces/file with spaces.txt"), "content").unwrap();
        fs::create_dir_all(repo_path.join("日本語ディレクトリ")).unwrap(); // Japanese directory
        fs::write(repo_path.join("日本語ディレクトリ/ファイル.txt"), "japanese content").unwrap();


        Command::new("git")
            .args(&["add", "."])
            .current_dir(repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["commit", "-m", "Initial commit with various paths"])
            .current_dir(repo_path)
            .output()
            .unwrap();
    }

    #[test]
    fn test_find_repo_root() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);
        let root = find_repo_root().unwrap();
        assert_eq!(root, repo_path);
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
        let mut root_dirs = get_dirs_at_path("", &repo_path).unwrap();
        root_dirs.sort(); // Sort for consistent comparison
        let expected_root_dirs = vec![
            "dir\ with\ spaces".to_string(),
            "docs".to_string(),
            "src".to_string(),
            "tests".to_string(),
            "日本語ディレクトリ".to_string(),
        ];
        assert_eq!(root_dirs, expected_root_dirs);

        // Test at a subdirectory
        let mut src_dirs = get_dirs_at_path("src", &repo_path).unwrap();
        src_dirs.sort();
        let expected_src_dirs = vec!["components".to_string()];
        assert_eq!(src_dirs, expected_src_dirs);

        // Test at a directory with no subdirectories
        let docs_dirs = get_dirs_at_path("docs", &repo_path).unwrap();
        assert!(docs_dirs.is_empty());
        
        let components_dirs = get_dirs_at_path("src/components", &repo_path).unwrap();
        assert!(components_dirs.is_empty());

        let mut dir_with_spaces_dirs = get_dirs_at_path("dir with spaces", &repo_path).unwrap();
        dir_with_spaces_dirs.sort();
        assert!(dir_with_spaces_dirs.is_empty());
    }

    #[test]
    fn test_get_sparse_checkout_list() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);

        Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed");

        Command::new("git")
            .args(&["sparse-checkout", "set", "src", "docs", "dir with spaces", "日本語ディレクトリ"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout set failed");
        
        let mut sparse_dirs = get_sparse_checkout_list(&repo_path).unwrap();
        sparse_dirs.sort();
        let expected_sparse_dirs = vec![
            "dir\ with\ spaces".to_string(),
            "docs".to_string(),
            "src".to_string(),
            "日本語ディレクトリ".to_string(),
        ];
        assert_eq!(sparse_dirs, expected_sparse_dirs);
    }

    #[test]
    fn test_get_uncommitted_paths() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);

        // No changes initially
        let changes = get_uncommitted_paths(&repo_path).unwrap();
        assert!(changes.is_empty());

        // Create a new untracked file with spaces and Japanese
        fs::write(repo_path.join("untracked file.txt"), "untracked").unwrap();
        fs::write(repo_path.join("新規ファイル.txt"), "new file content").unwrap();

        // Modify an existing file
        fs::write(repo_path.join("src/main.rs"), "fn main() { /* changed */ }").unwrap();

        let mut changes: Vec<String> = get_uncommitted_paths(&repo_path).unwrap().into_iter().collect();
        changes.sort();
        let expected_changes = vec![
            "src/main.rs".to_string(),
            "untracked\ file.txt".to_string(),
            "新規ファイル.txt".to_string(),
        ];
        assert_eq!(changes, expected_changes);
    }

    #[test]
    fn test_set_sparse_checkout_dirs() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);

        Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed");

        let dirs_to_set = vec![
            "src".to_string(),
            quote_path_string("dir with spaces"),
            quote_path_string("日本語ディレクトリ"),
        ];
        set_sparse_checkout_dirs(dirs_to_set.clone(), &repo_path).unwrap();

        let mut sparse_list = get_sparse_checkout_list(&repo_path).unwrap();
        sparse_list.sort();
        let mut expected_sparse_list = dirs_to_set;
        expected_sparse_list.sort();
        assert_eq!(sparse_list, expected_sparse_list);

        // Verify that the files actually exist (Git unquotes them internally)
        assert!(repo_path.join("src/main.rs").exists());
        assert!(repo_path.join("dir with spaces/file with spaces.txt").exists());
        assert!(repo_path.join("日本語ディレクトリ/ファイル.txt").exists());
        assert!(!repo_path.join("docs/README.md").exists()); // Should not exist
    }

    #[test]
    fn test_unescape_git_path_string_function() {
        assert_eq!(unescape_git_path_string("foo\ bar/baz"), "foo bar/baz");
        assert_eq!(unescape_git_path_string(""foo\ bar/baz""), "foo bar/baz"); // Double quotes
        assert_eq!(unescape_git_path_string("foo\040bar"), "foo bar"); // Octal space
        assert_eq!(unescape_git_path_string("file\ with\ spaces"), "file with spaces");
        assert_eq!(unescape_git_path_string("日本語ディレクトリ"), "日本語ディレクトリ"); // No escaping needed for UTF-8 that's not special
        assert_eq!(unescape_git_path_string(""\343\201\202.txt""), "あ.txt"); // Octal for UTF-8
        assert_eq!(unescape_git_path_string("file
ame"), "file
ame"); // Backslash
        assert_eq!(unescape_git_path_string("file"name"), "file"name"); // Quote
        assert_eq!(unescape_git_path_string("file
name"), "file
name"); // Newline escape
        assert_eq!(unescape_git_path_string(""), ""); // Empty string
        assert_eq!(unescape_git_path_string("not_quoted"), "not_quoted"); // Not quoted
        assert_eq!(unescape_git_path_string("a/b/c"), "a/b/c"); // Normal path
        assert_eq!(unescape_git_path_string("trailing\ "), "trailing "); // Trailing space
    }

    #[test]
    fn test_quote_path_string_function() {
        assert_eq!(quote_path_string("foo bar/baz"), ""foo\ bar/baz"");
        assert_eq!(quote_path_string("file with spaces"), ""file\ with\ spaces"");
        assert_eq!(quote_path_string("日本語ディレクトリ"), ""\346\227\245\346\234\254\348\252\236\343\203\207\343\202\243\343\203\225\343\202\241\343\202\244\343\203\253"");
        assert_eq!(quote_path_string("file
name"), ""file
name"");
        assert_eq!(quote_path_string("a/b/c"), "a/b/c"); // No special chars, no quoting
        assert_eq!(quote_path_string("file"name"), ""file"name"");
        assert_eq!(quote_path_string("file
ame"), ""file
ame"");
        assert_eq!(quote_path_string("foo/bar\ baz"), ""foo/bar\ baz""); // Mixed quoting
    }
}