use crate::git;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct App {
    pub current_repo_root: PathBuf,
    pub all_dirs: Vec<String>,
    pub sparse_checkout_dirs: Vec<String>,
    // Add other application state here as needed
}

impl App {
    pub fn new() -> Result<Self, git::Error> {
        let current_repo_root = git::find_repo_root()?;
        let all_dirs = git::get_all_dirs()?;
        
        let sparse_checkout_dirs = match git::get_sparse_checkout_list() {
            Ok(list) => list,
            Err(git::Error::GitCommand(err_msg)) if err_msg.contains("fatal: this worktree is not sparse") => {
                // Temporary workaround for PR#2: if sparse-checkout is not enabled, treat it as empty
                Vec::new()
            },
            Err(e) => return Err(e), // Propagate other errors
        };

        Ok(App {
            current_repo_root,
            all_dirs,
            sparse_checkout_dirs,
            // Initialize other fields
        })
    }
}

