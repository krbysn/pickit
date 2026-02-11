use crate::git;
use itertools::Itertools;
use ratatui::style::{Color, Style};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use crate::git::{unescape_git_path_string, quote_path_string}; // Import for display and path manipulation

// Define messages that can be sent from background threads to the main thread
pub enum AppMessage {
    ApplyChangesCompleted(Result<(), git::Error>),
    ChildrenLoaded(Result<(usize, Vec<String>), git::Error>), // Changed Vec<PathBuf> to Vec<String>
    RefreshCompleted(Result<(Vec<String>, HashSet<String>), git::Error>), // Changed Vec<PathBuf>, HashSet<PathBuf> to Vec<String>, HashSet<String>
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
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
    pub path: String, // Keep as String for display purposes (unescaped)
    pub status: String,
    pub uncommitted: String,
    pub subdirectories_total: String,
    pub subdirectories_checked_out: String,
    pub pending_changes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TreeItem {
    pub path: String, // Changed from PathBuf to String (quoted path)
    pub name: String, // Unquoted name for display
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
    pub is_loading: bool,
    pub indentation_level: u16,
    pub cached_pending_changes: u32,
}

impl TreeItem {
    pub fn new(path: String, name: String, is_checked_out: bool) -> Self { // Changed path to String
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
            is_implicitly_checked_out: false,
            is_loading: false,
            indentation_level: 0,
            cached_pending_changes: 0,
        }
    }
}

#[derive(Debug)]
pub struct App {
    #[allow(dead_code)] // Will be used in UI and other places
    pub current_repo_root: PathBuf, // Remains PathBuf
    pub items: Vec<TreeItem>, // Flat list of all directories
    pub path_to_index: HashMap<String, usize>, // Changed key from PathBuf to String
    pub filtered_item_indices: Vec<usize>, // Indices of items currently visible in the TUI
    pub selected_item_index: usize, // Index into `filtered_item_indices`
    #[allow(dead_code)] // Will be used for TUI scrolling
    pub scroll_offset: usize, // For scrolling the TUI view
    pub last_git_error: Option<String>, // To display transient git errors
    pub is_applying_changes: bool, // New field to indicate if changes are being applied

    pub is_refreshing: bool, // New field to indicate if a refresh is in progress
    pub tx: mpsc::Sender<AppMessage>, // Sender for background tasks to send messages to App
    #[allow(dead_code)] // Will be used by the main loop
    pub rx: mpsc::Receiver<AppMessage>, // Receiver for App to get messages from background tasks

    // Cached git state
    pub sparse_checkout_dirs: Vec<String>, // Changed from Vec<PathBuf> to Vec<String>
    pub uncommitted_paths: HashSet<String>, // Changed from HashSet<PathBuf> to HashSet<String>
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
            is_applying_changes: false,

            is_refreshing: false, // Initialize new field
            tx: mpsc::channel().0,      // Initialize sender (dummy, will be replaced in App::new)
            rx: mpsc::channel().1,      // Initialize receiver (dummy, will be replaced in App::new)
            sparse_checkout_dirs: Vec::new(),
            uncommitted_paths: HashSet::new(),
        }
    }
}

impl App {
    // Helper to extract the parent path from a quoted string path
    // e.g., "foo\\ bar/baz" -> "foo\\ bar"
    //       "foo" -> ""
    fn path_parent(path: &str) -> Option<String> {
        path.rfind('/').map(|i| path[..i].to_string())
    }

    // Helper to extract the file/directory name from a quoted string path
    // e.g., "foo\\ bar/baz" -> "baz"
    //       "foo" -> "foo"
    fn path_file_name(path: &str) -> Option<String> {
        path.rfind('/').map(|i| path[i + 1..].to_string())
    }

    // Helper to join two quoted string paths
    // This assumes `other` is a simple component or relative path
    fn path_join(base: &str, other: &str) -> String {
        if base.is_empty() {
            other.to_string()
        } else {
            format!("{}/{}", base, other)
        }
    }

    // Helper to check if a quoted string path starts with another quoted string path as a component
    // e.g., "foo/bar" starts with "foo" -> true
    //       "foo/bar" starts with "foobar" -> false
    fn path_starts_with_component(path: &str, prefix: &str) -> bool {
        path.starts_with(prefix) && (path.len() == prefix.len() || path.as_bytes()[prefix.len()] == b'/')
    }

    // New helper function to recursively update cached_pending_changes
    fn update_pending_changes_cache(&mut self, item_idx: usize) {
        let mut current_pending_changes = 0;
        let item = &self.items[item_idx];

        // Add 1 if this item itself has a pending change
        if item.pending_change.is_some() {
            current_pending_changes += 1;
        }

        // Recursively sum up children's cached_pending_changes
        for &child_idx in &item.children_indices {
            // Children's cache must be up-to-date before it is summed here.
            // This is ensured by calling this function bottom-up in the hierarchy.
            current_pending_changes += self.items[child_idx].cached_pending_changes;
        }

        // Update the item's cache if it has changed
        if self.items[item_idx].cached_pending_changes != current_pending_changes {
            self.items[item_idx].cached_pending_changes = current_pending_changes;

            // Propagate the update to the parent
            if let Some(parent_idx) = self.items[item_idx].parent_index {
                self.update_pending_changes_cache(parent_idx);
            }
        }
    }

    pub fn handle_refresh_completed(&mut self, result: Result<(Vec<String>, HashSet<String>), git::Error>) {
        self.is_refreshing = false; // Refresh is complete
        match result {
            Ok((sparse_checkout_dirs, uncommitted_paths)) => {
                self.update_state_from_git_info(sparse_checkout_dirs, uncommitted_paths);
                self.build_visible_items(); // Rebuild visible items after state update
            }
            Err(e) => {
                self.last_git_error = Some(e.to_string());
            }
        }
    }



    pub fn handle_children_loaded(
        &mut self,
        result: Result<(usize, Vec<String>), git::Error>, // Changed Vec<PathBuf> to Vec<String>
    ) {
        match result {
            Ok((parent_idx, sub_dirs)) => {
                let parent_item = &mut self.items[parent_idx];
                parent_item.is_loading = false;
                parent_item.children_loaded = true;

                if !sub_dirs.is_empty() {
                    for dir_path in sub_dirs.into_iter().sorted() { // dir_path is now String (quoted path)
                        if self.path_to_index.contains_key(&dir_path) {
                            continue;
                        }

                        // dir_path is already String (quoted path)
                        let name = unescape_git_path_string(&dir_path); // Unescape for display name
                        let is_checked_out = self.sparse_checkout_dirs.contains(&dir_path); // Compare String with String

                        let contains_uncommitted_changes = self
                            .uncommitted_paths
                            .iter()
                            .any(|p| self.path_starts_with_component(p, &dir_path)); // Use custom helper
                        let is_locked = contains_uncommitted_changes;

                        let mut item = TreeItem::new(dir_path.clone(), name, is_checked_out); // Pass String for path
                        item.contains_uncommitted_changes = contains_uncommitted_changes;
                        item.is_locked = is_locked;
                        item.parent_index = Some(parent_idx);
                        item.indentation_level = self.items[parent_idx].indentation_level + 1;
                        item.cached_pending_changes = 0;
                        
                        let new_idx = self.items.len();
                        self.items[parent_idx].children_indices.push(new_idx);
                        self.path_to_index.insert(dir_path, new_idx); // Insert String
                        self.items.push(item);
                    }
                }
                
                let parent_item = &mut self.items[parent_idx];
                if !parent_item.children_indices.is_empty() {
                    parent_item.is_expanded = true;
                }

                self.update_tree_item_states();
                self.build_visible_items();
                self.update_pending_changes_cache(parent_idx); // Update cache after adding children
            }
            Err(e) => {
                // Find which item was loading and set its state back
                if let Some(loading_item) = self.items.iter_mut().find(|i| i.is_loading) {
                    loading_item.is_loading = false;
                }
                self.last_git_error = Some(e.to_string());
            }
        }
    }
    fn update_tree_item_states(&mut self) {
        // Pass 1: Determine `has_checked_out_descendant` by checking `sparse_checkout_dirs`.
        // This is done once all items are loaded and `sparse_checkout_dirs` is up-to-date.
        for i in 0..self.items.len() {
            let item_path = &self.items[i].path; // item_path is now &String (quoted path)
            let has_descendant = self
                .sparse_checkout_dirs
                .iter()
                .any(|sco_path| { // sco_path is now &String (quoted path)
                    if item_path == "." { // Compare String with "." (root path)
                        // For the root item, any sparse checkout path that is not "." itself
                        // indicates a checked out descendant.
                        sco_path != "."
                    } else {
                        // For other items, check for true descendant paths.
                        // `sco_path` must start with `item_path` as a component.
                        self.path_starts_with_component(sco_path, item_path) &&
                        sco_path.len() > item_path.len() // Ensure it's longer
                    }
                });
            self.items[i].has_checked_out_descendant = has_descendant;
        }

        // Pass 2: Post-order traversal (from leaves up to the root) to aggregate `has_checked_out_descendant`.
        // This ensures that if a checked-out descendant is not in `sparse_checkout_dirs` (e.g., implicitly added by git),
        // it still propagates up. Or if an item was already checked out but then removed.
        for i in (0..self.items.len()).rev() {
            let item_has_explicit_checkout = self.items[i].is_checked_out;
            let children_have_checked_out_descendant = {
                let item = &self.items[i];
                item.children_indices.iter().any(|&child_idx| {
                    let child = &self.items[child_idx];
                    child.is_checked_out || child.has_checked_out_descendant
                })
            };
            self.items[i].has_checked_out_descendant = self.items[i].has_checked_out_descendant || children_have_checked_out_descendant || item_has_explicit_checkout;
        }


        // Pass 3: Pre-order traversal (from root down to leaves) to set `is_implicitly_checked_out`.
        for i in 0..self.items.len() {
            let parent_is_effectively_checked_out =
                if let Some(parent_idx) = self.items[i].parent_index {
                    // Do not consider the root directory for implicit checkout logic
                    if parent_idx == 0 { // Root item has index 0
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
        // self.sparse_checkout_dirs is now loaded asynchronously in App::new
        self.uncommitted_paths = git::get_uncommitted_paths(&self.current_repo_root)?;

        // --- Build Initial Tree ---
        self.items.clear();
        self.path_to_index.clear();

        // 1. Create Root Item
        // For the root, the path is represented as "." internally.
        let root_quoted_path = ".".to_string();
        let root_name = self
            .current_repo_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut root_item = TreeItem::new(root_quoted_path.clone(), root_name, true); // Pass String
        root_item.is_locked = true;
        root_item.is_expanded = true;
        root_item.children_loaded = true;
        root_item.indentation_level = 0; // Root is at level 0
        root_item.cached_pending_changes = 0;
        self.items.push(root_item);
        self.path_to_index.insert(root_quoted_path, 0); // Insert String

        // 2. Load Top-Level Dirs
        let top_level_dirs = git::get_dirs_at_path(".", &self.current_repo_root)?; // Returns Vec<String> (quoted paths)
        for dir_path in top_level_dirs.into_iter().sorted() { // dir_path is now String (quoted path)
            if self.path_to_index.contains_key(&dir_path) {
                continue;
            }

            // dir_path is already String (quoted path)
            let name = unescape_git_path_string(&dir_path); // Unescape for display name
            let is_checked_out = self.sparse_checkout_dirs.contains(&dir_path); // Compare String with String

            let contains_uncommitted_changes = self
                .uncommitted_paths
                .iter()
                .any(|p| self.path_starts_with_component(p, &dir_path)); // Use custom helper
            let is_locked = contains_uncommitted_changes;

            let mut item = TreeItem::new(dir_path.clone(), name, is_checked_out); // Pass String
            item.contains_uncommitted_changes = contains_uncommitted_changes;
            item.is_locked = is_locked;
            item.parent_index = Some(0);
            item.indentation_level = 1; // Direct children of root are at level 1
            item.cached_pending_changes = 0; // Initialize to 0
            
            let new_idx = self.items.len();
            self.items[0].children_indices.push(new_idx);
            self.path_to_index.insert(dir_path, new_idx); // Insert String
            self.items.push(item);
        }
        self.update_tree_item_states();
        
        // After all initial items are loaded, update the cache from leaves up.
        // This needs to be done once all items are in `self.items`.
        for i in (0..self.items.len()).rev() {
            self.update_pending_changes_cache(i);
        }
        
        Ok(())
    }

    /// Applies the pending changes to the git sparse-checkout set in a separate thread.
    pub fn apply_changes(&mut self) {
        self.is_applying_changes = true;
        self.last_git_error = None; // Clear previous errors

        let repo_root = self.current_repo_root.clone();
        let tx_clone = self.tx.clone();

        let current_actual_sparse_list = match git::get_sparse_checkout_list(&repo_root) {
            Ok(list) => list,
            Err(e) => {
                let _ = tx_clone.send(AppMessage::ApplyChangesCompleted(Err(e)));
                return;
            }
        };
        // Convert Vec<String> (quoted paths) to HashSet<String>
        let mut final_sparse_checkout_set: HashSet<String> = current_actual_sparse_list.into_iter().collect();

        // Apply pending changes from self.items on top of the actual git state
        for item in self.items.iter() {
            if item.path == "." { continue; } // Root is always implicitly checked out and cannot be changed

            match item.pending_change {
                Some(ChangeType::Add) => {
                    final_sparse_checkout_set.insert(item.path.clone());
                }
                Some(ChangeType::Remove) => {
                    final_sparse_checkout_set.remove(&item.path);
                }
                None => { /* No pending change, its state is already reflected in `final_sparse_checkout_set` */ }
            }
        }
        
        // Convert the final set to a Vec<String> for the git command (already quoted paths)
        let dirs_to_checkout: Vec<String> = final_sparse_checkout_set.into_iter().collect();

        // Spawn a new thread to perform the potentially long-running git operation
        thread::spawn(move || {
            let result = git::set_sparse_checkout_dirs(dirs_to_checkout, &repo_root);
            // Send the result back to the main thread
            let _ = tx_clone.send(AppMessage::ApplyChangesCompleted(result));
        });

        // The main thread returns immediately, letting the TUI continue to render.
    }

    /// Refreshes the application state by re-reading the git repository.
    /// Initiates an asynchronous refresh of the application state by re-reading the git repository.
    pub fn refresh(&mut self) {
        self.last_git_error = None; // Clear previous errors
        let repo_root_clone = self.current_repo_root.clone();
        let tx_clone = self.tx.clone();

        thread::spawn(move || {
            let result: Result<(Vec<String>, HashSet<String>), git::Error> = (|| {
                let sparse_checkout_dirs = git::get_sparse_checkout_list(&repo_root_clone)?;
                let uncommitted_paths = git::get_uncommitted_paths(&repo_root_clone)?;
                Ok((sparse_checkout_dirs, uncommitted_paths))
            })();
            // Send the result back to the main thread
            let _ = tx_clone.send(AppMessage::RefreshCompleted(result));
        });
    }

    // Helper function to update app state based on fetched git info
    fn update_state_from_git_info(&mut self, new_sparse_checkout_dirs: Vec<String>, new_uncommitted_paths: HashSet<String>) {
        self.sparse_checkout_dirs = new_sparse_checkout_dirs;
        self.uncommitted_paths = new_uncommitted_paths;

        // Update all loaded items in-place
        for i in 0..self.items.len() {
            let item = &mut self.items[i];

            // Root item is special
            if item.path == "." { // Compare String with "."
                item.is_checked_out = true;
                item.is_locked = true; // Root is always locked
                continue;
            }

            // Update checked-out status
            item.is_checked_out = self.sparse_checkout_dirs.contains(&item.path);

            // Update lock status
            let contains_uncommitted_changes = self
                .uncommitted_paths
                .iter()
                .any(|p| self.path_starts_with_component(p, &item.path)); // Use custom helper
            item.contains_uncommitted_changes = contains_uncommitted_changes;
            item.is_locked = contains_uncommitted_changes;
        }

        // Update tree item states and pending changes cache
        self.update_tree_item_states();
        for i in (0..self.items.len()).rev() {
            self.update_pending_changes_cache(i);
        }
        
        // After refreshing, clear any pending changes that might have been applied externally
        for item in self.items.iter_mut() {
            if let Some(change_type) = &item.pending_change {
                let current_checked_out = self.sparse_checkout_dirs.contains(&item.path);
                let needs_removal = match change_type {
                    ChangeType::Add => current_checked_out, // If added and now checked out, clear pending
                    ChangeType::Remove => !current_checked_out, // If removed and now not checked out, clear pending
                };
                if needs_removal {
                    item.pending_change = None;
                }
            }
        }
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

                let pending_changes = self.items[global_idx].cached_pending_changes;

                GridViewModel {
                    name: item.name.clone(),
                    path: unescape_git_path_string(&item.path), // Unescape for display
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
                let expansion_symbol = if item.is_loading {
                    "◌ " // Spinner for loading state
                } else if !item.children_loaded {
                    "▸ "
                } else if !item.children_indices.is_empty() {
                    if item.is_expanded {
                        "▾ "
                    } else {
                        "▸ "
                    }
                } else {
                    "  " // No children
                }
                ;

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
                let indent = "  ".repeat(item.indentation_level as usize);

                let display_text = format!(
                    "{indent}{expansion_symbol}{state_symbol}{}",
                    item.name
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
        
        // Synchronously load sparse checkout list at startup
        let initial_sparse_checkout_dirs = git::get_sparse_checkout_list(&current_repo_root)?;

        let mut app = App {
            current_repo_root,
            tx, // Assign the sender
            rx, // Assign the receiver
            sparse_checkout_dirs: initial_sparse_checkout_dirs, // Populated synchronously
            is_refreshing: false, // Initialize new field
            ..Default::default()
        };

        // No need to start asynchronous loading here, as it's done synchronously above.


        app.load_initial_tree()?; // Now sparse_checkout_dirs is populated here
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

    // Helper to start loading children and expand the item
    fn load_children_and_expand(&mut self, global_idx: usize) {
        let item = &mut self.items[global_idx];

        // If children are already loaded, just expand and rebuild
        if item.children_loaded {
            if !item.children_indices.is_empty() {
                item.is_expanded = true; // Always set to true for expansion
                self.build_visible_items();
            }
            return;
        }

        // If already loading, do nothing
        if item.is_loading {
            return;
        }

        // Start loading children
        item.is_loading = true;
        let item_path = item.path.clone(); // item_path is String (quoted path)
        let repo_root = self.current_repo_root.clone();
        let tx_clone = self.tx.clone();

        thread::spawn(move || {
            // item_path is already a String (quoted path), directly pass it
            let result = git::get_dirs_at_path(&item_path, &repo_root).map(|dirs| (global_idx, dirs));
            let _ = tx_clone.send(AppMessage::ChildrenLoaded(result));
        });
    }

    pub fn expand_selected_item(&mut self) {
        if let Some(&global_idx) = self.filtered_item_indices.get(self.selected_item_index) {
            let item = &mut self.items[global_idx];
            if !item.is_expanded { // Only expand if not already expanded
                self.load_children_and_expand(global_idx);
            }
        }
    }

    pub fn handle_left_key(&mut self) {
        if let Some(&global_idx) = self.filtered_item_indices.get(self.selected_item_index) {
            // _item_path was PathBuf, now removed as not used
            // let _item_path = self.items[global_idx].path.clone();

            // Check if the current item is expanded
            if self.items[global_idx].is_expanded {
                // If expanded, collapse it
                self.items[global_idx].is_expanded = false;
                self.build_visible_items();
            } else {
                // If collapsed, move to parent and collapse parent if it's open
                if let Some(parent_idx) = self.items[global_idx].parent_index {
                    // Update selected_item_index to point to the parent in filtered_item_indices
                    if let Some(parent_filtered_idx) = self.filtered_item_indices.iter().position(|&idx| idx == parent_idx) {
                        self.selected_item_index = parent_filtered_idx;
                    }
                    // No longer collapse the parent, just move to it.
                }
            }
        }
    }

    pub fn move_cursor_page_up(&mut self, tree_view_height: u16) {
        if self.filtered_item_indices.is_empty() {
            return;
        }

        let page_size = tree_view_height as usize;
        let target_index = self.selected_item_index.saturating_sub(page_size);

        self.selected_item_index = target_index;
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);

        // Ensure scroll_offset doesn't go below the selected_item_index's visible start
        if self.selected_item_index < self.scroll_offset {
            self.scroll_offset = self.selected_item_index;
        }
    }

    pub fn move_cursor_page_down(&mut self, tree_view_height: u16) {
        if self.filtered_item_indices.is_empty() {
            return;
        }

        let page_size = tree_view_height as usize;
        let max_index = self.filtered_item_indices.len().saturating_sub(1);
        let target_index = std::cmp::min(self.selected_item_index.saturating_add(page_size), max_index);

        self.selected_item_index = target_index;
        self.scroll_offset = std::cmp::min(self.scroll_offset.saturating_add(page_size), max_index.saturating_sub(page_size).max(0));

        // Ensure selected item is always visible after calculation
        if self.selected_item_index >= self.scroll_offset + page_size {
            self.scroll_offset = self.selected_item_index.saturating_sub(page_size).saturating_add(1);
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
            self.update_pending_changes_cache(global_idx); // Update cache after toggling selection
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use ratatui::style::Color;
    use std::fs; // Added fs import
    use std::process::Command;
    // Added git function imports
    use crate::git::tests::setup_git_repo;
    use crate::git::{get_dirs_at_path, get_all_directories_recursive, get_uncommitted_paths, set_sparse_checkout_dirs, get_sparse_checkout_list};


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
        
        let src_global_idx = app.items.iter().position(|i| i.path == PathBuf::from("src")).unwrap();
        app.selected_item_index = app
            .filtered_item_indices
            .iter()
            .position(|&i| i == src_global_idx)
            .unwrap();
        app.expand_selected_item(); // Changed from toggle_expansion()
        // Since expansion is now async, we need to process the message.
        // In a test, we can do this manually.
        let msg = app.rx.recv().unwrap();
        if let AppMessage::ChildrenLoaded(result) = msg {
            app.handle_children_loaded(result);
        } else {
            panic!("Expected ChildrenLoaded message");
        }

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
        let root_idx = app.items.iter().position(|i| i.path == PathBuf::from(".")).unwrap();
        let docs_idx = app.items.iter().position(|i| i.path == PathBuf::from("docs")).unwrap();
        let src_idx = app.items.iter().position(|i| i.path == PathBuf::from("src")).unwrap();
        let tests_idx = app.items.iter().position(|i| i.path == PathBuf::from("tests")).unwrap(); // Assuming 'tests' is also a top-level dir

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
        app.expand_selected_item();
        // Since expansion is now async, we need to process the message.
        let msg = app.rx.recv().unwrap();
        if let AppMessage::ChildrenLoaded(result) = msg {
            app.handle_children_loaded(result);
        } else {
            panic!("Expected ChildrenLoaded message");
        }


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
        let components_idx = app
            .items
            .iter()
            .position(|i| i.path == PathBuf::from("src/components"));
        assert!(
            components_idx.is_some(),
            "'src/components' should be in the items list after expanding 'src'"
        );
        let components_idx = components_idx.unwrap();

        // Verify the filtered_item_indices order to expose the bug
        // The *correct* order should be: [root, docs, src, src/components, tests]
        let expected_correct_order = vec![root_idx, docs_idx, src_idx, components_idx, tests_idx];
        assert_eq!(
            app.filtered_item_indices,
            expected_correct_order,
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
        let src_global_idx = app.items.iter().position(|i| i.path == PathBuf::from("src")).unwrap();
        app.items[src_global_idx].pending_change = Some(ChangeType::Add);

        // Assert that the pending change is registered
        assert_eq!(app.items[src_global_idx].pending_change, Some(ChangeType::Add));

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
            AppMessage::ChildrenLoaded(_) => {
                panic!("Expected ApplyChangesCompleted message");
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
        assert_eq!(app.items[src_global_idx].pending_change, None, "Pending change for 'src' should be cleared");
    }
        
    #[test]
    fn test_toggle_selection_locked_item() {
        // Setup a temporary git repo with uncommitted changes in 'src'
        let (repo_path, _temp_dir) = crate::git::tests::setup_git_repo_with_subdirs();
        // Modify a file to create uncommitted changes in 'src'
        std::fs::write(repo_path.join("src/main.rs"), "fn main() { println!(\"Hello\"); }").unwrap();

        let mut app = App::new(Some(&repo_path)).expect("App initialization failed");

        // Find the 'src' item and ensure it's locked
        let src_global_idx = app.items.iter().position(|i| i.path == PathBuf::from("src")).unwrap();
        assert!(app.items[src_global_idx].is_locked, "'src' should be locked due to uncommitted changes");
        assert_eq!(app.items[src_global_idx].pending_change, None, "'src' should have no pending change initially");

        // Select 'src'
        app.selected_item_index = app.filtered_item_indices.iter().position(|&i| i == src_global_idx).unwrap();

        // Attempt to toggle selection on the locked item
        app.toggle_selection();

        // Assert that the pending_change state has NOT changed
        assert_eq!(app.items[src_global_idx].pending_change, None, "Toggle selection on a locked item should have no effect on pending_change");

        // Ensure no error message was set
        assert!(app.last_git_error.is_none(), "No git error should occur for locked item toggle");
    }

    #[test]
    fn test_apply_changes_unloaded_node() {
        // 1. Setup Repo with subdirs and initialize sparse-checkout
        let (repo_path, _temp_dir) = crate::git::tests::setup_git_repo_with_subdirs();
        let _ = Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed")
            .status
            .success();

        // 2. Create App instance
        let mut app = App::new(Some(&repo_path)).expect("App initialization failed");

        // Find the 'docs' item (a top-level directory)
        let docs_global_idx = app.items.iter().position(|i| i.path == PathBuf::from("docs")).unwrap();
        assert!(!app.items[docs_global_idx].is_checked_out, "'docs' should not be checked out initially by git");
        assert_eq!(app.items[docs_global_idx].pending_change, None, "'docs' should have no pending change initially");

        // 3. Simulate selecting 'docs' for addition
        app.items[docs_global_idx].pending_change = Some(ChangeType::Add);

        // 4. Call app.apply_changes()
        app.apply_changes();

        // 5. Process the ApplyChangesCompleted message
        let app_msg = app
            .rx
            .recv()
            .expect("Should receive a message from apply_changes");
        match app_msg {
            AppMessage::ApplyChangesCompleted(result) => {
                result.expect("Apply changes should succeed in test");
            }
            AppMessage::ChildrenLoaded(_) => {
                panic!("Expected ApplyChangesCompleted message");
            }

        }

        // 6. Verify that git sparse-checkout list includes 'docs'
        let sparse_checkout_list = git::get_sparse_checkout_list(&repo_path).unwrap();
        assert!(sparse_checkout_list.contains(&"docs".to_string()), "'docs' should be in the sparse-checkout list after apply");
    }

    #[test]
    fn test_directory_name_inclusion_coloring() {
        // 1. Setup Repo & Sparse Checkout
        let (repo_path, _temp_dir) = git::tests::setup_git_repo();
        
        // Create directories: dir1, dir100, dir2
        std::fs::create_dir_all(repo_path.join("dir1")).unwrap();
        std::fs::write(repo_path.join("dir1/file1.txt"), "content").unwrap();
        std::fs::create_dir_all(repo_path.join("dir100")).unwrap();
        std::fs::write(repo_path.join("dir100/file100.txt"), "content").unwrap();
        std::fs::create_dir_all(repo_path.join("dir2")).unwrap();
        std::fs::write(repo_path.join("dir2/file2.txt"), "content").unwrap();

        // Commit these files
        Command::new("git")
            .args(&["add", "."])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["commit", "-m", "Add test dirs"])
            .current_dir(&repo_path)
            .output()
            .unwrap();

        // Initialize sparse checkout and set dir100
        git::set_sparse_checkout_dirs(vec!["dir100".to_string()], &repo_path)
            .expect("Failed to set sparse checkout dirs");

        // 2. Create App
        let app = App::new(Some(&repo_path)).expect("App initialization failed");
        
        // 3. Get View Models
        let view_models = app.get_tui_tree_items();

        // Helper to find a view model by its name
        let find_vm = |name: &str| {
            view_models
                .iter()
                .find(|vm| vm.display_text.ends_with(name))
                .unwrap_or_else(|| panic!("View model for '{}' not found", name))
        };

        // 4. Assert Colors
        // `dir100` is explicitly checked out. Should be Green.
        assert_eq!(
            find_vm("dir100").style.fg,
            Some(Color::Green),
            "'dir100' should be Green"
        );

        // `dir1` is NOT checked out and NOT a descendant of `dir100`. Should be DarkGray.
        assert_eq!(
            find_vm("dir1").style.fg,
            Some(Color::DarkGray),
            "'dir1' should be DarkGray"
        );

        // `dir2` is not checked out and has no checked out descendants. Should be DarkGray.
        assert_eq!(
            find_vm("dir2").style.fg,
            Some(Color::DarkGray),
            "'dir2' should be DarkGray"
        );
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

    #[test]
    fn test_japanese_folder_display_bug_reproduction() {
        let (repo_path, _temp_dir) = setup_git_repo();

        // Create Japanese directory and file
        fs::create_dir_all(repo_path.join("表示バグフォルダ")).unwrap();
        fs::write(repo_path.join("表示バグフォルダ/ファイル.txt"), "content").unwrap();
        Command::new("git")
            .args(&["add", "."])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["commit", "-m", "Add Japanese bug folder"])
            .current_dir(&repo_path)
            .output()
            .unwrap();

        // Manually check out the Japanese folder using Git (simulating the user's initial action)
        Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed");

        let bug_folder_path = PathBuf::from("表示バグフォルダ");
        let dirs_to_set = vec![bug_folder_path.to_string_lossy().to_string()];
        set_sparse_checkout_dirs(dirs_to_set, &repo_path).unwrap();

        // --- Simulate pickit restart and initial load ---
        let app = App::new(Some(&repo_path)).expect("App initialization failed"); // Changed mut to non-mut

        // --- Critical part: Assert the state *before* async message is processed ---
        // At this point, app.load_initial_tree() has run.
        // So, app.sparse_checkout_dirs is still empty, and is_checked_out should be false for the folder.
        let bug_folder_global_idx = app.items.iter().position(|i| i.path == bug_folder_path).expect("Bug folder not found in app items");
        
        println!("DEBUG (Bug Repro Test): After App::new() and load_initial_tree():");
        println!("DEBUG (Bug Repro Test): app.sparse_checkout_dirs: {:?}", app.sparse_checkout_dirs); // Should be empty
        println!("DEBUG (Bug Repro Test): app.items[{:?}].path: {:?}", bug_folder_global_idx, app.items[bug_folder_global_idx].path);
        println!("DEBUG (Bug Repro Test): app.items[{:?}].is_checked_out: {:?}", bug_folder_global_idx, app.items[bug_folder_global_idx].is_checked_out);

        // This assertion *should* FAIL to reproduce the bug.
        // We expect it to be TRUE (because it's checked out in Git), but it will be FALSE (due to race condition).
        assert!(app.items[bug_folder_global_idx].is_checked_out, "BUG REPRODUCED: Japanese folder should be checked out, but internal state is FALSE after initial load.");

        // Clean up: Process messages to avoid test interference in other tests, though not strictly needed for this bug repro.
        // Process any other potential messages if they exist to clear the channel, for other tests.
        while let Ok(_) = app.rx.try_recv() {}
    }
}