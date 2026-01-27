use crate::git;
use itertools::Itertools;
use ratatui::style::{Color, Style};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc, // Add this import
    thread,
};

// Define messages that can be sent from background threads to the main thread
pub enum AppMessage {
    ApplyChangesCompleted(Result<(), git::Error>),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ChangeType {
    Add,
    Remove,
}

/// Holds the display-ready information for a single item in the tree view.
#[derive(Debug, Clone)]
pub struct TuiTreeItemViewModel {
    pub display_text: String,
    pub style: Style,
}

#[derive(Debug, Clone, Default)]
pub struct GridViewModel {
    pub name: String,
    pub path: String,
    pub status: String,
    pub uncommitted: String,
    pub subdirectories_total: String,
    pub subdirectories_checked_out: String,
    pub pending_changes: String,
}

#[derive(Debug, Clone)]
pub struct TreeItem {
    pub path: String,
    pub name: String,
    pub children_indices: Vec<usize>, // Indices of direct children in the App's items vec
    pub parent_index: Option<usize>,
    pub is_expanded: bool,
    pub children_loaded: bool, // To support lazy loading
    pub is_checked_out: bool,
    pub pending_change: Option<ChangeType>,
    pub is_locked: bool,                    // If this item cannot be deselected
    pub contains_uncommitted_changes: bool, // For determining `is_locked`
    pub has_checked_out_descendant: bool,
    pub is_implicitly_checked_out: bool,
}

impl TreeItem {
    pub fn new(path: String, name: String, is_checked_out: bool) -> Self {
        TreeItem {
            path,
            name,
            children_indices: Vec::new(),
            parent_index: None,
            is_expanded: false,     // Default to collapsed
            children_loaded: false, // Not loaded by default
            is_checked_out,
            pending_change: None,
            is_locked: false,
            contains_uncommitted_changes: false,
            has_checked_out_descendant: false,
            is_implicitly_checked_out: false, // Initialize new field
        }
    }
}

#[derive(Debug)]
pub struct App {
    #[allow(dead_code)] // Will be used in UI and other places
    pub current_repo_root: PathBuf,
    pub items: Vec<TreeItem>, // Flat list of all directories
    pub path_to_index: HashMap<String, usize>, // For quick lookups
    pub filtered_item_indices: Vec<usize>, // Indices of items currently visible in the TUI
    pub selected_item_index: usize, // Index into `filtered_item_indices`
    #[allow(dead_code)] // Will be used for TUI scrolling
    pub scroll_offset: usize, // For scrolling the TUI view
    pub last_git_error: Option<String>, // To display transient git errors
    pub is_applying_changes: bool, // New field to indicate if changes are being applied
    pub tx: mpsc::Sender<AppMessage>, // Sender for background tasks to send messages to App
    #[allow(dead_code)] // Will be used by the main loop
    pub rx: mpsc::Receiver<AppMessage>, // Receiver for App to get messages from background tasks

    // Cached git state
    pub sparse_checkout_dirs: Vec<String>,
    pub uncommitted_paths: std::collections::HashSet<PathBuf>,
}

impl Default for App {
    fn default() -> Self {
        App {
            current_repo_root: PathBuf::new(),
            items: Vec::new(),
            path_to_index: HashMap::new(),
            filtered_item_indices: Vec::new(),
            selected_item_index: 0,
            scroll_offset: 0,
            last_git_error: None,
            is_applying_changes: false, // Initialize new field
            tx: mpsc::channel().0,      // Initialize sender (dummy, will be replaced in App::new)
            rx: mpsc::channel().1,      // Initialize receiver (dummy, will be replaced in App::new)
            sparse_checkout_dirs: Vec::new(),
            uncommitted_paths: std::collections::HashSet::new(),
        }
    }
}

impl App {
    fn update_tree_item_states(&mut self) {
        // Pass 1: Post-order traversal (from leaves up to the root) to set `has_checked_out_descendant`.
        for i in (0..self.items.len()).rev() {
            let has_descendant = {
                let item = &self.items[i];
                item.children_indices.iter().any(|&child_idx| {
                    let child = &self.items[child_idx];
                    child.is_checked_out || child.has_checked_out_descendant
                })
            };
            self.items[i].has_checked_out_descendant = has_descendant;
        }

        // Pass 2: Pre-order traversal (from root down to leaves) to set `is_implicitly_checked_out`.
        for i in 0..self.items.len() {
            let parent_is_effectively_checked_out = if let Some(parent_idx) = self.items[i].parent_index {
                // Do not consider the root directory for implicit checkout logic
                if parent_idx == 0 {
                    false
                } else {
                    let parent = &self.items[parent_idx];
                    parent.is_checked_out || parent.is_implicitly_checked_out
                }
            } else {
                false
            };

            if parent_is_effectively_checked_out {
                self.items[i].is_implicitly_checked_out = true;
            } else {
                self.items[i].is_implicitly_checked_out = false;
            }
        }
    }

    fn load_initial_tree(&mut self) -> Result<(), git::Error> {
        self.sparse_checkout_dirs = match git::get_sparse_checkout_list(&self.current_repo_root) {
            Ok(list) => list,
            Err(git::Error::GitCommand(err_msg))
                if err_msg.contains("fatal: this worktree is not sparse") =>
            {
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        self.uncommitted_paths = git::get_uncommitted_paths(&self.current_repo_root)?;

        // --- Build Initial Tree ---
        self.items.clear();
        self.path_to_index.clear();

        // 1. Create Root Item
        let root_name = self
            .current_repo_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut root_item = TreeItem::new(".".to_string(), root_name, true);
        root_item.is_locked = true;
        root_item.is_expanded = true;
        root_item.children_loaded = true;
        self.items.push(root_item);
        self.path_to_index.insert(".".to_string(), 0);

        // 2. Load Top-Level Dirs
        let top_level_dirs = git::get_dirs_at_path(".", &self.current_repo_root)?;
        for dir_path_str in top_level_dirs.into_iter().sorted() {
            let dir_path = Path::new(&dir_path_str);
            let name = dir_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let is_checked_out = self.sparse_checkout_dirs.contains(&dir_path_str);

            let contains_uncommitted_changes = self
                .uncommitted_paths
                .iter()
                .any(|p| p.starts_with(&dir_path_str));
            let is_locked = contains_uncommitted_changes;

            let mut item = TreeItem::new(dir_path_str.clone(), name, is_checked_out);
            item.contains_uncommitted_changes = contains_uncommitted_changes;
            item.is_locked = is_locked;
            item.parent_index = Some(0);

            let new_idx = self.items.len();
            self.items[0].children_indices.push(new_idx);
            self.path_to_index.insert(dir_path_str, new_idx);
            self.items.push(item);
        }
        self.update_tree_item_states();
        Ok(())
    }

    /// Applies the pending changes to the git sparse-checkout set in a separate thread.
    pub fn apply_changes(&mut self) {
        self.is_applying_changes = true;
        self.last_git_error = None; // Clear previous errors

        let dirs_to_checkout: Vec<String> = self
            .items
            .iter()
            .filter_map(|item| {
                let is_currently_checked_out = item.is_checked_out;
                let pending_change = item.pending_change;

                // Determine if the item should be in the final set
                let should_be_checked_out = match pending_change {
                    Some(ChangeType::Add) => true,
                    Some(ChangeType::Remove) => false,
                    None => is_currently_checked_out,
                };

                // We only need to include directories that should be checked out
                if should_be_checked_out && item.path != "." {
                    Some(item.path.clone())
                } else {
                    None
                }
            })
            .collect();

        // Capture necessary variables for the thread
        let repo_root = self.current_repo_root.clone();
        let tx_clone = self.tx.clone();

        // Spawn a new thread to perform the potentially long-running git operation
        thread::spawn(move || {
            let result = git::set_sparse_checkout_dirs(dirs_to_checkout, &repo_root);
            // Send the result back to the main thread
            let _ = tx_clone.send(AppMessage::ApplyChangesCompleted(result));
        });

        // The main thread returns immediately, letting the TUI continue to render.
    }

    /// Refreshes the application state by re-reading the git repository.
    pub fn refresh(&mut self) -> Result<(), git::Error> {
        // --- Fetch latest git state ---
        self.sparse_checkout_dirs = match git::get_sparse_checkout_list(&self.current_repo_root) {
            Ok(list) => list,
            Err(git::Error::GitCommand(err_msg))
                if err_msg.contains("fatal: this worktree is not sparse") =>
            {
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        self.uncommitted_paths = git::get_uncommitted_paths(&self.current_repo_root)?;

        // --- Update all loaded items in-place ---
        for i in 0..self.items.len() {
            let item = &mut self.items[i];
            let path_str = &item.path;

            // Root item is special
            if path_str == "." {
                item.is_checked_out = true;
                item.is_locked = true;
                continue;
            }

            // Update checked-out status
            item.is_checked_out = self.sparse_checkout_dirs.contains(path_str);

            // Update lock status
            let contains_uncommitted_changes = self
                .uncommitted_paths
                .iter()
                .any(|p| p.starts_with(path_str));
            item.contains_uncommitted_changes = contains_uncommitted_changes;
            item.is_locked = contains_uncommitted_changes;
        }

        self.update_tree_item_states();

        Ok(())
    }

    /// Counts pending changes recursively starting from a given item index.
    fn count_pending_changes_recursive(&self, item_index: usize) -> u32 {
        let item = &self.items[item_index];
        let mut count = if item.pending_change.is_some() { 1 } else { 0 };

        for &child_index in &item.children_indices {
            count += self.count_pending_changes_recursive(child_index);
        }

        count
    }

    pub fn get_grid_view_model(&self) -> Option<GridViewModel> {
        self.filtered_item_indices
            .get(self.selected_item_index)
            .map(|&global_idx| {
                let item = &self.items[global_idx];

                let status = if item.is_locked {
                    "Locked".to_string()
                } else if item.is_checked_out {
                    "Checked Out".to_string()
                } else {
                    "Not Checked Out".to_string()
                };

                let uncommitted = if item.contains_uncommitted_changes {
                    "Yes".to_string()
                } else {
                    "No".to_string()
                };

                let subdirectories_checked_out = item
                    .children_indices
                    .iter()
                    .filter(|&&child_idx| self.items[child_idx].is_checked_out)
                    .count();

                let pending_changes = self.count_pending_changes_recursive(global_idx);

                GridViewModel {
                    name: item.name.clone(),
                    path: item.path.clone(),
                    status,
                    uncommitted,
                    subdirectories_total: item.children_indices.len().to_string(),
                    subdirectories_checked_out: subdirectories_checked_out.to_string(),
                    pending_changes: pending_changes.to_string(),
                }
            })
    }

    pub fn get_tui_tree_items(&self) -> Vec<TuiTreeItemViewModel> {
        self.filtered_item_indices
            .iter()
            .enumerate()
            .map(|(view_idx, &global_idx)| {
                let item = &self.items[global_idx];

                // 1. Determine Style (Color)
                let mut style = Style::default();
                if item.is_locked {
                    style = style.fg(Color::Red);
                } else if item.pending_change.is_some() {
                    style = style.fg(Color::Yellow);
                } else if item.is_checked_out {
                    style = style.fg(Color::Green);
                } else if item.is_implicitly_checked_out || item.has_checked_out_descendant {
                    style = style.fg(Color::White);
                } else {
                    style = style.fg(Color::DarkGray);
                }


                // Highlight the selected item
                if view_idx == self.selected_item_index {
                    style = style.bg(Color::Blue);
                }

                // 2. Determine Expansion Symbol
                let expansion_symbol = if !item.children_loaded {
                    // Optimistically show expand symbol if we haven't checked for children yet.
                    // The exception is for locked items that are not checked out, which are unlikely to have checked-out children.
                    // This is a heuristic and might be adjusted.
                    "▸ "
                } else if !item.children_indices.is_empty() {
                    if item.is_expanded {
                        "▾ "
                    } else {
                        "▸ "
                    }
                } else {
                    "  " // No children
                };

                // 3. Determine State Symbol
                let state_symbol = if item.is_locked {
                    "🔒 "
                } else {
                    match item.pending_change {
                        Some(ChangeType::Add) => "+ ",
                        Some(ChangeType::Remove) => "- ",
                        None => {
                            if item.is_checked_out {
                                "✔ "
                            } else if item.has_checked_out_descendant {
                                "☐·"
                            } else {
                                "☐ "
                            }
                        }
                    }
                };

                // 4. Determine indentation
                let mut current_idx = global_idx;
                let mut indent = String::new();
                while let Some(parent_idx) = self.items[current_idx].parent_index {
                    indent.insert_str(0, "  ");
                    current_idx = parent_idx;
                }

                let display_text = format!(
                    "{}{}{}{}",
                    indent, expansion_symbol, state_symbol, item.name
                );

                TuiTreeItemViewModel {
                    display_text,
                    style,
                }
            })
            .collect()
    }

    pub fn new(repo_path: Option<&PathBuf>) -> Result<Self, git::Error> {
        let current_repo_root = match repo_path {
            Some(path) => path.clone(),
            None => git::find_repo_root()?,
        };
        let (tx, rx) = mpsc::channel(); // Create the channel
        let mut app = App {
            current_repo_root,
            tx, // Assign the sender
            rx, // Assign the receiver
            ..Default::default()
        };
        app.load_initial_tree()?;
        app.build_visible_items();
        Ok(app)
    }

    fn build_visible_items_recursive(
        items: &Vec<TreeItem>,
        item_idx: usize,
        visible_indices: &mut Vec<usize>,
    ) {
        visible_indices.push(item_idx);

        let item = &items[item_idx];
        if item.is_expanded {
            for &child_idx in &item.children_indices {
                Self::build_visible_items_recursive(items, child_idx, visible_indices);
            }
        }
    }

    // Helper to rebuild the list of currently visible items in the TUI
    fn build_visible_items(&mut self) {
        self.filtered_item_indices.clear();
        if !self.items.is_empty() {
            Self::build_visible_items_recursive(&self.items, 0, &mut self.filtered_item_indices);
        }
    }

    pub fn move_cursor_up(&mut self) {
        if !self.filtered_item_indices.is_empty() {
            self.selected_item_index = self.selected_item_index.saturating_sub(1);
        }
    }

    pub fn move_cursor_down(&mut self) {
        if !self.filtered_item_indices.is_empty() {
            self.selected_item_index = std::cmp::min(
                self.selected_item_index + 1,
                self.filtered_item_indices.len().saturating_sub(1),
            );
        }
    }

    pub fn toggle_expansion(&mut self) -> Result<(), git::Error> {
        if let Some(&global_idx) = self.filtered_item_indices.get(self.selected_item_index) {
            // Load children if they haven't been loaded yet.
            if !self.items[global_idx].children_loaded {
                let item_path = self.items[global_idx].path.clone();
                let sub_dirs = git::get_dirs_at_path(&item_path, &self.current_repo_root)?;

                if !sub_dirs.is_empty() {
                    for dir_path_str in sub_dirs.into_iter().sorted() {
                        // Check if item already exists (can happen on refresh)
                        if self.path_to_index.contains_key(&dir_path_str) {
                            continue;
                        }

                        let dir_path = Path::new(&dir_path_str);
                        let name = dir_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let is_checked_out = self.sparse_checkout_dirs.contains(&dir_path_str);

                        // NOTE: This check is still simplified.
                        let contains_uncommitted_changes = self
                            .uncommitted_paths
                            .iter()
                            .any(|p| p.starts_with(&dir_path_str));
                        let is_locked = contains_uncommitted_changes;

                        let mut item = TreeItem::new(dir_path_str.clone(), name, is_checked_out);
                        item.contains_uncommitted_changes = contains_uncommitted_changes;
                        item.is_locked = is_locked;
                        item.parent_index = Some(global_idx);

                        let new_idx = self.items.len();
                        self.items[global_idx].children_indices.push(new_idx);
                        self.path_to_index.insert(dir_path_str, new_idx);
                        self.items.push(item);
                    }
                }
                self.items[global_idx].children_loaded = true;
                self.update_tree_item_states();
            }

            // Now, toggle the expansion state.
            let item = &mut self.items[global_idx];
            if !item.children_indices.is_empty() {
                item.is_expanded = !item.is_expanded;
            }

            self.build_visible_items();
            // Ensure selected item index remains valid after rebuilding visible items
            // This logic might need adjustment to keep the selection on the same item
            if let Some(new_filtered_idx) = self
                .filtered_item_indices
                .iter()
                .position(|&idx| idx == global_idx)
            {
                self.selected_item_index = new_filtered_idx;
            } else {
                // If the item disappeared (e.g., collapsing its parent), we might lose selection.
                // For now, let's clamp it. A better solution might be to move selection to the parent.
                self.selected_item_index = std::cmp::min(
                    self.selected_item_index,
                    self.filtered_item_indices.len().saturating_sub(1),
                );
            }
        }
        Ok(())
    }

    pub fn toggle_selection(&mut self) {
        if let Some(&global_idx) = self.filtered_item_indices.get(self.selected_item_index) {
            let item = &mut self.items[global_idx];
            if item.is_locked {
                // Cannot toggle selection on locked items
                return;
            }

            // Toggle pending change state
            item.pending_change = match item.pending_change {
                Some(ChangeType::Add) => None,
                Some(ChangeType::Remove) => None,
                None => {
                    if item.is_checked_out {
                        Some(ChangeType::Remove)
                    } else {
                        Some(ChangeType::Add)
                    }
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use ratatui::style::Color;

    #[test]
    fn test_directory_state_coloring() {
        // 1. Setup Repo & Sparse Checkout
        let (repo_path, _temp_dir) = git::tests::setup_git_repo_with_subdirs();
        // Explicitly check out a nested directory.
        // This makes `src` have a checked-out descendant.
        git::set_sparse_checkout_dirs(vec!["src/components".to_string()], &repo_path)
            .expect("Failed to set sparse checkout dirs");

        // 2. Create App and expand 'src' to make the nested dir visible
        let mut app = App::new(Some(&repo_path)).expect("App initialization failed");
        let src_global_idx = app.items.iter().position(|i| i.path == "src").unwrap();
        app.selected_item_index = app
            .filtered_item_indices
            .iter()
            .position(|&i| i == src_global_idx)
            .unwrap();
        app.toggle_expansion()
            .expect("Failed to expand 'src' directory");

        // 3. Get View Models
        let view_models = app.get_tui_tree_items();

        // Helper to find a view model by its name
        let find_vm = |name: &str| {
            view_models
                .iter()
                .find(|vm| vm.display_text.ends_with(name))
                .unwrap_or_else(|| panic!("View model for '{}' not found", name))
        };

        // 4. Assert Colors based on requirements.md
        // `src` is not explicitly checked out but has a checked out descendant. Should be White.
        assert_eq!(
            find_vm("src").style.fg,
            Some(Color::White),
            "'src' should be White"
        );

        // `src/components` is explicitly checked out. Should be Green.
        assert_eq!(
            find_vm("components").style.fg,
            Some(Color::Green),
            "'src/components' should be Green"
        );

        // `docs` is not checked out and has no checked out descendants. Should be DarkGray.
        assert_eq!(
            find_vm("docs").style.fg,
            Some(Color::DarkGray),
            "'docs' should be DarkGray"
        );

        // `tests` is not checked out and has no checked out descendants. Should be DarkGray.
        assert_eq!(
            find_vm("tests").style.fg,
            Some(Color::DarkGray),
            "'tests' should be DarkGray"
        );
    }

    // A basic mock setup for testing `toggle_expansion`
    // In a real-world scenario, we'd mock the git calls.
    // For now, these tests will run against a real git repo.
    #[test]
    fn test_toggle_expansion_loads_children() {
        // This test needs to run in a temporary git repo.
        let (repo_path, _temp_dir) = crate::git::tests::setup_git_repo_with_subdirs();

        let mut app = App::new(Some(&repo_path)).expect("App initialization failed");

        // --- Initial State Checks ---
        // Ensure initial items are loaded and sorted correctly
        let root_idx = app.items.iter().position(|i| i.path == ".").unwrap();
        let docs_idx = app.items.iter().position(|i| i.path == "docs").unwrap();
        let src_idx = app.items.iter().position(|i| i.path == "src").unwrap();
        let tests_idx = app.items.iter().position(|i| i.path == "tests").unwrap(); // Assuming 'tests' is also a top-level dir

        // Initial top-level items are alphabetically sorted in `items`
        // and also in `filtered_item_indices` since no children are expanded yet.
        assert_eq!(
            app.filtered_item_indices,
            vec![root_idx, docs_idx, src_idx, tests_idx]
        );

        // Select 'src' in the filtered list
        app.selected_item_index = app
            .filtered_item_indices
            .iter()
            .position(|&i| i == src_idx)
            .unwrap();

        assert!(
            !app.items[src_idx].children_loaded,
            "Children of 'src' should not be loaded yet"
        );
        assert!(
            app.items[src_idx].children_indices.is_empty(),
            "Children indices of 'src' should be empty before loading"
        );

        // Expand 'src'
        app.toggle_expansion().unwrap();

        // --- After Expansion Checks ---
        let src_item = &app.items[src_idx];
        assert!(
            src_item.children_loaded,
            "Children of 'src' should be loaded after expansion"
        );
        assert!(
            !src_item.children_indices.is_empty(),
            "Children indices of 'src' should not be empty after loading"
        );
        assert!(src_item.is_expanded, "'src' should be marked as expanded");

        // Check if the children (e.g., 'src/components') are now in the items list
        let components_idx = app.items.iter().position(|i| i.path == "src/components");
        assert!(
            components_idx.is_some(),
            "'src/components' should be in the items list after expanding 'src'"
        );
        let components_idx = components_idx.unwrap();

        // Verify the filtered_item_indices order to expose the bug
        // The *correct* order should be: [root, docs, src, src/components, tests]
        let expected_correct_order = vec![root_idx, docs_idx, src_idx, components_idx, tests_idx];
        assert_eq!(
            app.filtered_item_indices, expected_correct_order,
            "Filtered items order is incorrect after fix"
        );

        // The *correct* order should be: [root, docs, src, src/components, tests]
        // The actual assertion for the fix will check for this.
    }

    #[test]
    fn test_apply_changes_progress_flag() {
        // Setup a temporary git repo
        let (repo_path, _temp_dir) = crate::git::tests::setup_git_repo_with_subdirs();

        let mut app = App::new(Some(&repo_path)).expect("App initialization failed");

        // Ensure initially no changes are being applied
        assert!(
            !app.is_applying_changes,
            "Initially, is_applying_changes should be false"
        );

        // Simulate a pending change: mark 'src' for addition
        let src_global_idx = app.items.iter().position(|i| i.path == "src").unwrap();
        app.items[src_global_idx].pending_change = Some(ChangeType::Add);

        // Assert that the pending change is registered
        assert_eq!(
            app.items[src_global_idx].pending_change,
            Some(ChangeType::Add)
        );

        // Apply changes
        app.apply_changes();

        // After apply_changes, app.is_applying_changes is still true
        assert!(
            app.is_applying_changes,
            "Immediately after apply_changes, is_applying_changes should be true"
        );

        // Simulate main loop processing the message
        let app_msg = app
            .rx
            .recv()
            .expect("Should receive a message from apply_changes");
        match app_msg {
            AppMessage::ApplyChangesCompleted(result) => {
                app.is_applying_changes = false; // Manually reset as main loop would
                result.expect("Apply changes should succeed in test");
                // Clear pending changes on all items, as the main loop would
                for item in app.items.iter_mut() {
                    item.pending_change = None;
                }
                // Refresh the app state as the main loop would
                app.refresh().expect("App refresh should succeed");
            }
        }

        // After processing the message, the flag should be reset to false
        assert!(
            !app.is_applying_changes,
            "After processing message, is_applying_changes should be false"
        );

        // Verify that 'src' is now checked out
        let sparse_checkout_list = git::get_sparse_checkout_list(&repo_path).unwrap();
        assert!(
            sparse_checkout_list.contains(&"src".to_string()),
            "'src' should be in the sparse-checkout list"
        );

        // Also verify the pending_change was cleared
        assert_eq!(
            app.items[src_global_idx].pending_change, None,
            "Pending change for 'src' should be cleared"
        );
    }
}
