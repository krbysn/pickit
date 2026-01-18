use crate::git;
use ratatui::style::{Color, Style};
use std::{collections::HashMap, path::{Path, PathBuf}};

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
    pub is_checked_out: bool,
    pub pending_change: Option<ChangeType>,
    pub is_locked: bool,                      // If this item cannot be deselected
    pub contains_uncommitted_changes: bool, // For determining `is_locked`
}

impl TreeItem {
    pub fn new(path: String, name: String, is_checked_out: bool) -> Self {
        TreeItem {
            path,
            name,
            children_indices: Vec::new(),
            parent_index: None,
            is_expanded: false, // Default to collapsed
            is_checked_out,
            pending_change: None,
            is_locked: false,
            contains_uncommitted_changes: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct App {
    #[allow(dead_code)] // Will be used in UI and other places
    pub current_repo_root: PathBuf,
    pub items: Vec<TreeItem>, // Flat list of all directories
    pub filtered_item_indices: Vec<usize>, // Indices of items currently visible in the TUI
    pub selected_item_index: usize, // Index into `filtered_item_indices`
    #[allow(dead_code)] // Will be used for TUI scrolling
    pub scroll_offset: usize, // For scrolling the TUI view
    #[allow(dead_code)] // Will be used to display errors in TUI
    pub last_git_error: Option<String>, // To display transient git errors
}

impl App {
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
                // Clear any previous error and refresh the state
                self.last_git_error = None;
                if self.refresh().is_err() {
                    // If refreshing fails, we should probably note that
                    self.last_git_error = Some("Failed to refresh state after applying changes.".to_string());
                }
            }
            Err(e) => {
                self.last_git_error = Some(e.to_string());
            }
        }
    }

    /// Refreshes the application state by re-reading the git repository.
    fn refresh(&mut self) -> Result<(), git::Error> {
        let all_dirs = git::get_all_dirs()?;
        let sparse_checkout_dirs = match git::get_sparse_checkout_list() {
            Ok(list) => list,
            Err(git::Error::GitCommand(err_msg)) if err_msg.contains("fatal: this worktree is not sparse") => Vec::new(),
            Err(e) => return Err(e),
        };

        let mut items = Vec::new();
        let mut path_to_index: HashMap<String, usize> = HashMap::new();

        // Ensure root is always present
        let mut sorted_all_dirs = all_dirs;
        sorted_all_dirs.sort_unstable();
        if !sorted_all_dirs.contains(&".".to_string()) {
            sorted_all_dirs.insert(0, ".".to_string());
        }

        for (i, dir_path) in sorted_all_dirs.into_iter().enumerate() {
            let is_checked_out = sparse_checkout_dirs.contains(&dir_path);
            let name = if dir_path == "." {
                self.current_repo_root.file_name().unwrap_or_default().to_string_lossy().to_string()
            } else {
                PathBuf::from(&dir_path).file_name().unwrap_or_default().to_string_lossy().to_string()
            };

            let contains_uncommitted_changes = git::has_uncommitted_changes(Path::new(&dir_path))?;
            let is_locked = contains_uncommitted_changes || dir_path == ".";

            let mut item = TreeItem::new(dir_path.clone(), name, is_checked_out);
            item.contains_uncommitted_changes = contains_uncommitted_changes;
            item.is_locked = is_locked;

            if dir_path != "." {
                let parent_path = PathBuf::from(&dir_path).parent().map(|p| p.to_string_lossy().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| ".".to_string());
                if let Some(&parent_idx) = path_to_index.get(&parent_path) {
                    item.parent_index = Some(parent_idx);
                }
            }

            path_to_index.insert(dir_path, i);
            items.push(item);
        }

        let mut final_items = items;
        for i in 0..final_items.len() {
            if let Some(parent_idx) = final_items[i].parent_index {
                final_items[parent_idx].children_indices.push(i);
            }
        }
        
        if let Some(root_item) = final_items.get_mut(0) {
            if root_item.path == "." {
                root_item.is_expanded = true;
            }
        }

        self.items = final_items;
        self.build_visible_items();
        self.selected_item_index = 0; // Reset cursor
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
                }
                
                // Highlight the selected item
                if view_idx == self.selected_item_index {
                    style = style.bg(Color::Blue);
                }

                // 2. Determine Expansion Symbol
                let expansion_symbol = if !item.children_indices.is_empty() {
                    if item.is_expanded { "▾ " } else { "▸ " }
                } else {
                    "  " // No children, so no symbol
                };

                // 3. Determine State Symbol
                let state_symbol = if item.is_locked {
                    "🔒 "
                } else {
                    match item.pending_change {
                        Some(ChangeType::Add) => "+ ",
                        Some(ChangeType::Remove) => "- ",
                        None => {
                            if item.is_checked_out { "✔ " } else { "☐ " }
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
                
                let display_text = format!("{}{}{}{}", indent, expansion_symbol, state_symbol, item.name);

                TuiTreeItemViewModel { display_text, style }
            })
            .collect()
    }

    pub fn new() -> Result<Self, git::Error> {
        let current_repo_root = git::find_repo_root()?;
        let mut app = App {
            current_repo_root,
            ..Default::default()
        };
        app.refresh()?;
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

    pub fn toggle_expansion(&mut self) {
        if let Some(&global_idx) = self.filtered_item_indices.get(self.selected_item_index) {
            let item = &mut self.items[global_idx];
            if !item.children_indices.is_empty() {
                item.is_expanded = !item.is_expanded;
                self.build_visible_items();
                // Ensure selected item index remains valid after rebuilding visible items
                self.selected_item_index = std::cmp::min(
                    self.selected_item_index,
                    self.filtered_item_indices.len().saturating_sub(1),
                );
            }
        }
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

    // Helper to create a basic App for testing
    // This mocks the git calls by taking pre-defined data
    fn create_mock_app(
        repo_root: &str,
        all_dirs: Vec<String>,
        sparse_checkout_dirs: Vec<String>,
        uncommitted_dirs: Vec<String>,
    ) -> App {
        let current_repo_root = PathBuf::from(repo_root);
        let mut items = Vec::new();
        let mut path_to_index: HashMap<String, usize> = HashMap::new();

        let mut sorted_all_dirs = all_dirs.clone();
        sorted_all_dirs.sort_unstable(); // Ensure consistent order for tree building

        // Root is always present
        if !sorted_all_dirs.contains(&".".to_string()) {
            sorted_all_dirs.insert(0, ".".to_string());
        }

        for (i, dir_path) in sorted_all_dirs.into_iter().enumerate() {
            let is_checked_out = sparse_checkout_dirs.contains(&dir_path);
            let name = if dir_path == "." {
                current_repo_root.file_name().unwrap_or_default().to_string_lossy().to_string()
            } else {
                PathBuf::from(&dir_path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            };

            let contains_uncommitted_changes = uncommitted_dirs.contains(&dir_path);
            let is_locked = contains_uncommitted_changes || dir_path == "."; // Lock root and any dir with changes

            let mut item = TreeItem::new(dir_path.clone(), name, is_checked_out);
            item.contains_uncommitted_changes = contains_uncommitted_changes;
            item.is_locked = is_locked;

            path_to_index.insert(dir_path, i);
            items.push(item);
        }

        // Second pass to populate children_indices and parent_index
        let mut final_items = items.clone(); 
        for (i, item) in items.iter().enumerate() {
            if item.path != "." {
                 let parent_path = PathBuf::from(&item.path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| ".".to_string()); // Default parent to root

                if let Some(&parent_idx) = path_to_index.get(&parent_path) {
                    final_items[parent_idx].children_indices.push(i);
                    final_items[i].parent_index = Some(parent_idx);
                }
            }
        }
        items = final_items;

        if let Some(root_item) = items.get_mut(0) {
            if root_item.path == "." {
                root_item.is_expanded = true;
            }
        }

        let mut app = App {
            current_repo_root,
            items,
            filtered_item_indices: Vec::new(),
            selected_item_index: 0,
            scroll_offset: 0,
            last_git_error: None,
        };

        app.build_visible_items();
        app
    }

    // --- Tests for App::new() ---
    // This test still relies on actual git commands, due to App::new() signature
    // A more robust solution for App::new() test would involve modifying App::new()
    // to accept a trait for git operations.
    #[test]
    fn test_app_new_initial_state() {
        let app = App::new().expect("App initialization failed");
        
        // Assert on the structure and some properties
        assert!(!app.items.is_empty());
        assert_eq!(app.selected_item_index, 0);
        assert!(!app.filtered_item_indices.is_empty());
        
        let root_item = &app.items[app.filtered_item_indices[0]];
        assert_eq!(root_item.path, ".");
        assert!(root_item.is_expanded);
        assert!(root_item.is_locked); // Root is always locked
    }

    // --- Tests for Navigation ---
    #[test]
    fn test_move_cursor_down() {
        let app_root = "/test/repo";
        let all_dirs = vec![".".to_string(), "src".to_string(), "docs".to_string()];
        let sparse_checkout = vec![];
        let uncommitted = vec![];

        let mut app = create_mock_app(app_root, all_dirs, sparse_checkout, uncommitted);
        assert_eq!(app.selected_item_index, 0);
        
        app.move_cursor_down();
        assert_eq!(app.selected_item_index, 1);
        
        app.move_cursor_down();
        assert_eq!(app.selected_item_index, 2);

        // Should not go past the last item
        app.move_cursor_down();
        assert_eq!(app.selected_item_index, 2);
    }

    #[test]
    fn test_move_cursor_up() {
        let app_root = "/test/repo";
        let all_dirs = vec![".".to_string(), "src".to_string(), "docs".to_string()];
        let sparse_checkout = vec![];
        let uncommitted = vec![];

        let mut app = create_mock_app(app_root, all_dirs, sparse_checkout, uncommitted);
        app.selected_item_index = 2; // Start at the last item
        
        app.move_cursor_up();
        assert_eq!(app.selected_item_index, 1);
        
        app.move_cursor_up();
        assert_eq!(app.selected_item_index, 0);

        // Should not go above the first item
        app.move_cursor_up();
        assert_eq!(app.selected_item_index, 0);
    }

    // --- Tests for Expansion ---
    #[test]
    fn test_toggle_expansion() {
        let app_root = "/test/repo";
        let all_dirs = vec![
            ".".to_string(),
            "src".to_string(),
            "src/components".to_string(),
            "docs".to_string(),
        ];
        let sparse_checkout = vec![];
        let uncommitted = vec![];

        let mut app = create_mock_app(app_root, all_dirs, sparse_checkout, uncommitted);
        
        // Initial state: root expanded, others collapsed
        // Filtered: [0 (.), 1 (src), 3 (docs)]
        assert_eq!(app.filtered_item_indices.len(), 3); 
        let src_global_idx = app.items.iter().position(|i| i.path == "src").unwrap();
        let components_global_idx = app.items.iter().position(|i| i.path == "src/components").unwrap();

        // Select 'src' (filtered index 1 -> global index of 'src')
        app.selected_item_index = app.filtered_item_indices.iter().position(|&idx| idx == src_global_idx).unwrap();
        let current_item_global_idx = app.filtered_item_indices[app.selected_item_index];
        assert_eq!(app.items[current_item_global_idx].path, "src");
        assert!(!app.items[current_item_global_idx].is_expanded);

        // Toggle expansion of 'src'
        app.toggle_expansion();
        assert!(app.items[current_item_global_idx].is_expanded);
        // After expansion, 'src/components' should be visible
        assert_eq!(app.filtered_item_indices.len(), 4); // ., src, src/components, docs
        assert!(app.filtered_item_indices.contains(&components_global_idx));

        // Toggle back
        app.toggle_expansion();
        assert!(!app.items[current_item_global_idx].is_expanded);
        assert_eq!(app.filtered_item_indices.len(), 3); // ., src, docs
        assert!(!app.filtered_item_indices.contains(&components_global_idx));
    }

    // --- Tests for Selection ---
    #[test]
    fn test_toggle_selection_add() {
        let app_root = "/test/repo";
        let all_dirs = vec![".".to_string(), "src".to_string()];
        let sparse_checkout = vec![]; // src not checked out initially
        let uncommitted = vec![];

        let mut app = create_mock_app(app_root, all_dirs, sparse_checkout, uncommitted);
        app.selected_item_index = 1; // Select 'src' (filtered index 1 -> global index of 'src')
        let src_global_idx = app.filtered_item_indices[app.selected_item_index];
        
        assert_eq!(app.items[src_global_idx].path, "src");
        assert!(!app.items[src_global_idx].is_checked_out);
        assert_eq!(app.items[src_global_idx].pending_change, None);

        app.toggle_selection(); // Should mark 'src' for addition
        assert_eq!(app.items[src_global_idx].pending_change, Some(ChangeType::Add));

        app.toggle_selection(); // Should clear pending change
        assert_eq!(app.items[src_global_idx].pending_change, None);
    }

    #[test]
    fn test_toggle_selection_remove() {
        let app_root = "/test/repo";
        let all_dirs = vec![".".to_string(), "src".to_string()];
        let sparse_checkout = vec!["src".to_string()]; // src checked out initially
        let uncommitted = vec![];

        let mut app = create_mock_app(app_root, all_dirs, sparse_checkout, uncommitted);
        app.selected_item_index = 1; // Select 'src' (filtered index 1 -> global index of 'src')
        let src_global_idx = app.filtered_item_indices[app.selected_item_index];

        assert_eq!(app.items[src_global_idx].path, "src");
        assert!(app.items[src_global_idx].is_checked_out);
        assert_eq!(app.items[src_global_idx].pending_change, None);

        app.toggle_selection(); // Should mark 'src' for removal
        assert_eq!(app.items[src_global_idx].pending_change, Some(ChangeType::Remove));

        app.toggle_selection(); // Should clear pending change
        assert_eq!(app.items[src_global_idx].pending_change, None);
    }

    #[test]
    fn test_toggle_selection_locked_item() {
        let app_root = "/test/repo";
        let all_dirs = vec![".".to_string(), "src".to_string()];
        let sparse_checkout = vec![];
        let uncommitted = vec!["src".to_string()]; // Mark 'src' as having uncommitted changes

        let mut app = create_mock_app(app_root, all_dirs, sparse_checkout, uncommitted);
        app.selected_item_index = 1; // Select 'src' (filtered index 1 -> global index of 'src')
        let src_global_idx = app.filtered_item_indices[app.selected_item_index];

        assert!(app.items[src_global_idx].is_locked);
        assert_eq!(app.items[src_global_idx].pending_change, None);

        app.toggle_selection(); // Attempt to toggle selection on locked item
        assert_eq!(app.items[src_global_idx].pending_change, None); // Should remain None
    }
}