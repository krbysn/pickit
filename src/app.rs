use crate::git;
use itertools::Itertools;
use ratatui::style::{Color, Style};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

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
            is_expanded: false, // Default to collapsed
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
    pub items: Vec<TreeItem>,              // Flat list of all directories
    pub path_to_index: HashMap<String, usize>, // For quick lookups
    pub filtered_item_indices: Vec<usize>, // Indices of items currently visible in the TUI
    pub selected_item_index: usize,        // Index into `filtered_item_indices`
    #[allow(dead_code)] // Will be used for TUI scrolling
    pub scroll_offset: usize, // For scrolling the TUI view
    pub last_git_error: Option<String>,    // To display transient git errors
    
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
            sparse_checkout_dirs: Vec::new(),
            uncommitted_paths: std::collections::HashSet::new(),
        }
    }
}

impl App {
    fn load_initial_tree(&mut self) -> Result<(), git::Error> {
        // --- Fetch Git Data ---
        self.sparse_checkout_dirs = match git::get_sparse_checkout_list() {
            Ok(list) => list,
            Err(git::Error::GitCommand(err_msg))
                if err_msg.contains("fatal: this worktree is not sparse") =>
            {
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        self.uncommitted_paths = git::get_uncommitted_paths()?;

        // --- Build Initial Tree ---
        self.items.clear();
        self.path_to_index.clear();

        // 1. Create Root Item
        let root_name = self.current_repo_root
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
        let top_level_dirs = git::get_dirs_at_path(".")?;
        for dir_path_str in top_level_dirs.into_iter().sorted() {
            let dir_path = Path::new(&dir_path_str);
            let name = dir_path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let is_checked_out = self.sparse_checkout_dirs.contains(&dir_path_str);
            
            let contains_uncommitted_changes = self.uncommitted_paths.iter().any(|p| p.starts_with(&dir_path_str));
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
        Ok(())
    }

    /// Applies the pending changes to the git sparse-checkout set.
    pub fn apply_changes(&mut self) {
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

        match git::set_sparse_checkout_dirs(dirs_to_checkout) {
            Ok(_) => {
                // Clear pending changes on all items
                for item in self.items.iter_mut() {
                    item.pending_change = None;
                }

                // Clear any previous error and refresh the state
                self.last_git_error = None;
                if self.refresh().is_err() {
                    // If refreshing fails, we should probably note that
                    self.last_git_error =
                        Some("Failed to refresh state after applying changes.".to_string());
                }
            }
            Err(e) => {
                self.last_git_error = Some(e.to_string());
            }
        }
    }

    /// Refreshes the application state by re-reading the git repository.
    pub fn refresh(&mut self) -> Result<(), git::Error> {
        // --- Fetch latest git state ---
        self.sparse_checkout_dirs = match git::get_sparse_checkout_list() {
            Ok(list) => list,
            Err(git::Error::GitCommand(err_msg))
                if err_msg.contains("fatal: this worktree is not sparse") =>
            {
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        self.uncommitted_paths = git::get_uncommitted_paths()?;

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
            let contains_uncommitted_changes = self.uncommitted_paths.iter().any(|p| p.starts_with(path_str));
            item.contains_uncommitted_changes = contains_uncommitted_changes;
            item.is_locked = contains_uncommitted_changes;
        }

        // The tree structure hasn't changed, so we don't need to rebuild visible items or touch selection.
        // The UI will be redrawn in the next loop iteration with the updated state.
        
        // We might need to recalculate implicit/descendant checkout states here
        // but for now, this is omitted as it was in the previous implementation.

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
                } else if item.is_implicitly_checked_out {
                    style = style.fg(Color::White); // New: implicitly checked out
                } else if item.has_checked_out_descendant {
                    style = style.fg(Color::White); // Still White for explicit descendant
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

    pub fn new() -> Result<Self, git::Error> {
        let current_repo_root = git::find_repo_root()?;
        let mut app = App {
            current_repo_root,
            ..Default::default()
        };
        app.load_initial_tree()?;
        app.build_visible_items();
        Ok(app)
    }

    // Helper to rebuild the list of currently visible items in the TUI
    fn build_visible_items(&mut self) {
        self.filtered_item_indices.clear();
        for i in 0..self.items.len() {
            let _item = &self.items[i]; // Changed 'item' to '_item'

            // Corrected logic: Check if all its ancestors are expanded
            let mut current_idx = i;
            let mut is_visible = true;
            while let Some(parent_idx) = self.items[current_idx].parent_index {
                if !self.items[parent_idx].is_expanded {
                    is_visible = false;
                    break;
                }
                current_idx = parent_idx;
            }
            if is_visible {
                self.filtered_item_indices.push(i);
            }
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
                let sub_dirs = git::get_dirs_at_path(&item_path)?;

                if !sub_dirs.is_empty() {
                    for dir_path_str in sub_dirs.into_iter().sorted() {
                        // Check if item already exists (can happen on refresh)
                        if self.path_to_index.contains_key(&dir_path_str) {
                            continue;
                        }

                        let dir_path = Path::new(&dir_path_str);
                        let name = dir_path.file_name().unwrap_or_default().to_string_lossy().to_string();
                        let is_checked_out = self.sparse_checkout_dirs.contains(&dir_path_str);

                        // NOTE: This check is still simplified.
                        let contains_uncommitted_changes = self.uncommitted_paths.iter().any(|p| p.starts_with(&dir_path_str));
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

    // A basic mock setup for testing `toggle_expansion`
    // In a real-world scenario, we'd mock the git calls.
    // For now, these tests will run against a real git repo.
    #[test]
    fn test_toggle_expansion_loads_children() {
        // This test needs to run in a temporary git repo.
        let (repo_path, _temp_dir) = crate::git::tests::setup_git_repo_with_subdirs();
        std::env::set_current_dir(&repo_path).unwrap();

        let mut app = App::new().expect("App initialization failed");

        // Initially, we should have the root and top-level dirs.
        // The exact number depends on the test repo setup.
        // Let's find 'src' and expand it.
        let src_initial_index = app.items.iter().position(|i| i.path == "src");
        assert!(src_initial_index.is_some(), "'src' directory not found in initial items");
        let src_initial_index = src_initial_index.unwrap();

        assert!(!app.items[src_initial_index].children_loaded, "Children of 'src' should not be loaded yet");
        assert!(app.items[src_initial_index].children_indices.is_empty(), "Children indices of 'src' should be empty before loading");

        // Select 'src' in the filtered list
        app.selected_item_index = app.filtered_item_indices.iter().position(|&i| i == src_initial_index).unwrap();

        // Expand 'src'
        app.toggle_expansion().unwrap();

        // After expansion, children should be loaded
        let src_item = &app.items[src_initial_index];
        assert!(src_item.children_loaded, "Children of 'src' should be loaded after expansion");
        assert!(!src_item.children_indices.is_empty(), "Children indices of 'src' should not be empty after loading");
        assert!(src_item.is_expanded, "'src' should be marked as expanded");

        // Check if the children (e.g., 'src/components') are now in the items list
        let has_components = app.items.iter().any(|i| i.path == "src/components");
        assert!(has_components, "'src/components' should be in the items list after expanding 'src'");

        // Check if 'src/components' is now visible in the filtered list
        let components_global_idx = app.items.iter().position(|i| i.path == "src/components").unwrap();
        let is_visible = app.filtered_item_indices.contains(&components_global_idx);
        assert!(is_visible, "'src/components' should be visible after expanding 'src'");
    }
}
    