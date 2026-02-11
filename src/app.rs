use crate::git;
use ratatui::style::{Color, Style};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

// Define messages that can be sent from background threads to the main thread
#[derive(Debug)] // Add this line
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
    pub path: String, // Changed from PathBuf to String (unescaped path)
    pub name: String, // Unescaped name for display
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

// Helper to check if a string path starts with another string path as a component
// e.g., "foo/bar" starts with "foo" -> true
//       "foo/bar" starts with "foobar" -> false
fn path_starts_with_component(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix) && (path.len() == prefix.len() || path.as_bytes()[prefix.len()] == b'/')
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
    // Helper to extract the parent path from a path string
    // e.g., "foo bar/baz" -> "foo bar"
    //       "foo" -> ""
    #[allow(dead_code)]
    fn path_parent(path: &str) -> Option<String> {
        path.rfind('/').map(|i| path[..i].to_string())
    }

    // Helper to extract the file/directory name from a path string
    // e.g., "foo bar/baz" -> "baz"
    //       "foo" -> "foo"
    #[allow(dead_code)]
    fn path_file_name(path: &str) -> Option<String> {
        path.rfind('/').map(|i| path[i + 1..].to_string())
    }

    // Helper to join two path strings
    // This assumes `other` is a simple component or relative path
    #[allow(dead_code)]
    fn path_join(base: &str, other: &str) -> String {
        if base.is_empty() {
            other.to_string()
        } else {
            format!("{}/{}", base, other)
        }
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
                    let mut sorted_sub_dirs = sub_dirs;
                    sorted_sub_dirs.sort();

                    let parent_item_path = self.items[parent_idx].path.clone(); // Get the full path of the parent

                    for dir_name in sorted_sub_dirs { // dir_name is now the simple name of the directory
                        let full_child_path = if parent_item_path == "." {
                            dir_name.clone() // If parent is root, child path is just its name
                        } else {
                            format!("{}/{}", parent_item_path, dir_name)
                        };
                        let name = dir_name.clone(); // Name for display remains just the component name

                        if self.path_to_index.contains_key(&full_child_path) {
                            continue;
                        }

                        let is_checked_out = self.sparse_checkout_dirs.contains(&full_child_path); // Compare String with String

                        let contains_uncommitted_changes = self
                            .uncommitted_paths
                            .iter()
                            .any(|p| path_starts_with_component(p, &full_child_path)); // Use custom helper
                        let is_locked = contains_uncommitted_changes;

                        let mut item = TreeItem::new(full_child_path.clone(), name, is_checked_out); // Pass String for path
                        item.contains_uncommitted_changes = contains_uncommitted_changes;
                        item.is_locked = is_locked;
                        item.parent_index = Some(parent_idx);
                        item.indentation_level = self.items[parent_idx].indentation_level + 1;
                        item.cached_pending_changes = 0;
                        
                        let new_idx = self.items.len();
                        self.items[parent_idx].children_indices.push(new_idx);
                        self.path_to_index.insert(full_child_path, new_idx); // Insert String
                        self.items.push(item);
                    }
                }
                
                let parent_item = &mut self.items[parent_idx];
                parent_item.is_expanded = true; // Always set to expanded if children were loaded (even if empty)

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
            let item_path = &self.items[i].path; // item_path is now &String (unescaped path)
            let has_descendant = self
                .sparse_checkout_dirs
                .iter()
                .any(|sco_path| { // sco_path is now &String (unescaped path)
                    if item_path == "." { // Compare String with "." (root path)
                        // For the root item, any sparse checkout path that is not "." itself
                        // indicates a checked out descendant.
                        sco_path != "."
                    } else {
                        // For other items, check for true descendant paths.
                        // `sco_path` must start with `item_path` as a component.
                        path_starts_with_component(sco_path, item_path) &&
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
        let root_path = ".".to_string(); // Path is now unescaped, use "."
        let root_name = self
            .current_repo_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut root_item = TreeItem::new(root_path.clone(), root_name, true); // Pass String
        root_item.is_locked = true;
        root_item.is_expanded = true;
        root_item.children_loaded = true;
        root_item.indentation_level = 0; // Root is at level 0
        root_item.cached_pending_changes = 0;
        self.items.push(root_item);
        self.path_to_index.insert(root_path, 0); // Insert String

        // 2. Load Top-Level Dirs
        let top_level_dirs = git::get_dirs_at_path(".", &self.current_repo_root)?; // Returns Vec<String> (unescaped paths)
        let mut sorted_top_level_dirs = top_level_dirs;
        sorted_top_level_dirs.sort();

        for dir_path in sorted_top_level_dirs { // dir_path is now String (unescaped path)
            let name = dir_path.clone(); // Path is now unescaped, use directly

            if self.path_to_index.contains_key(&dir_path) {
                continue;
            }

            let is_checked_out = self.sparse_checkout_dirs.contains(&dir_path); // Compare String with String

            let contains_uncommitted_changes = self
                .uncommitted_paths
                .iter()
                .any(|p| path_starts_with_component(p, &dir_path)); // Use custom helper
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
        // Convert Vec<String> (unescaped paths) to HashSet<String>
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
        
        // Convert the final set to a Vec<String> for the git command (already unescaped paths)
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
                .any(|p| path_starts_with_component(p, &item.path)); // Use custom helper
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
                    path: item.path.clone(), // Path is now unescaped
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
        
        // Corrected: item.path already stores the full path relative to repo root
        let full_path_to_expand = self.items[global_idx].path.clone();

        let repo_root = self.current_repo_root.clone();
        let tx_clone = self.tx.clone();

        thread::spawn(move || {
            // Pass the reconstructed full path to get_dirs_at_path
            let result = git::get_dirs_at_path(&full_path_to_expand, &repo_root).map(|dirs| (global_idx, dirs));
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
            // Check if the current item is expanded
            if self.items[global_idx].is_expanded {
                // If expanded, collapse it
                self.items[global_idx].is_expanded = false;
                self.build_visible_items();
            } else {
                // If collapsed, move to parent
                if let Some(parent_idx) = self.items[global_idx].parent_index {
                    // Update selected_item_index to point to the parent in filtered_item_indices
                    if let Some(parent_filtered_idx) = self.filtered_item_indices.iter().position(|&idx| idx == parent_idx) {
                        self.selected_item_index = parent_filtered_idx;
                    }
                }
            }
        }
    }

    pub fn move_cursor_page_up(&mut self, _tree_view_height: u16) { // Marked as unused
        if self.filtered_item_indices.is_empty() {
            return;
        }

        let page_size = _tree_view_height as usize;
        let target_index = self.selected_item_index.saturating_sub(page_size);

        self.selected_item_index = target_index;
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);

        // Ensure scroll_offset doesn't go below the selected_item_index's visible start
        if self.selected_item_index < self.scroll_offset {
            self.scroll_offset = self.selected_item_index;
        }
    }

    pub fn move_cursor_page_down(&mut self, _tree_view_height: u16) { // Marked as unused
        if self.filtered_item_indices.is_empty() {
            return;
        }

        let page_size = _tree_view_height as usize;
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
mod app_tests {
    use super::*;
    use std::fs;
    use std::time::Duration;
    use std::process::Command; // Import Command for tests
    use tempfile::tempdir;

    // --- Helper functions copied from git_test.rs for self-containment ---
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
        // Ensure core.quotepath is false for consistent unescaped test output
        Command::new("git")
            .args(&["config", "core.quotepath", "false"])
            .current_dir(&path)
            .output()
            .unwrap();
        (path, dir)
    }

    fn create_and_commit_files(repo_path: &PathBuf) {
        fs::create_dir_all(&repo_path.join("dir1/subdir1")).unwrap();
        fs::write(&repo_path.join("dir1/subdir1/file1.txt"), "content").unwrap();
        fs::create_dir_all(&repo_path.join("dir1/subdir2")).unwrap();
        fs::write(&repo_path.join("dir1/subdir2/file2.txt"), "content").unwrap();
        fs::create_dir_all(&repo_path.join("dir2/subdir3/subdir4")).unwrap();
        fs::write(&repo_path.join("dir2/subdir3/subdir4/file3.txt"), "content").unwrap();
        fs::create_dir_all(&repo_path.join("dir3")).unwrap(); // Empty dir
        fs::write(&repo_path.join("dir3/.gitkeep"), "").unwrap(); // Add .gitkeep to track empty dir

        // Add a Japanese directory
        fs::create_dir_all(&repo_path.join("日本語ディレクトリ/サブディレクトリ")).unwrap();
        fs::write(&repo_path.join("日本語ディレクトリ/サブディレクトリ/.gitkeep"), "").unwrap(); // Add .gitkeep to track empty dir
        fs::write(&repo_path.join("日本語ディレクトリ/ファイル.txt"), "content").unwrap();


        Command::new("git")
            .args(&["add", "."])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["commit", "-m", "Test commit with nested dirs"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        
        // Initialize sparse-checkout
        Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed");
    }
    // --- End Helper functions ---

    #[test]
    fn test_expand_multiple_subtrees() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);

        let (test_thread_tx, test_thread_rx) = mpsc::channel(); // Channel for App's spawned threads to send to test
        let (app_tx_dummy, app_rx_dummy) = mpsc::channel(); // Dummy channel for App's rx, since App's tx is what matters for tests
        let mut app = App { tx: test_thread_tx, rx: app_rx_dummy, ..Default::default() };
        
        // Simulate app initialization
        app.current_repo_root = repo_path.clone();
        app.sparse_checkout_dirs = vec![]; // Initially nothing checked out
        app.uncommitted_paths = HashSet::new();

        app.load_initial_tree().unwrap(); // Load root and first level

        // --- Expand "dir1" ---
        // Find "dir1" item index
        let dir1_global_idx = app.items.iter().position(|item| item.name == "dir1").expect("dir1 not found");

        // Simulate expansion
        app.load_children_and_expand(dir1_global_idx);
        
        // Await the async message from App's spawned thread
        let app_message = test_thread_rx.recv_timeout(Duration::from_secs(5)).expect("Did not receive AppMessage for dir1");
        match app_message {
            AppMessage::ChildrenLoaded(result) => app.handle_children_loaded(result),
            _ => panic!("Unexpected AppMessage received for dir1: {:?}", app_message),
        }

        // Assert dir1 state
        let dir1_item = &app.items[dir1_global_idx];
        assert!(dir1_item.is_expanded, "dir1 should be expanded");
        assert!(dir1_item.children_loaded, "dir1 children should be loaded");
        assert!(!dir1_item.children_indices.is_empty(), "dir1 should have children");
        assert_eq!(dir1_item.children_indices.len(), 2, "dir1 should have 2 children");

        // Assert subdir1 and subdir2 exist as children of dir1
        let subdir1_idx = dir1_item.children_indices.iter().find(|&&idx| app.items[idx].name == "subdir1").expect("subdir1 not found");
        let subdir2_idx = dir1_item.children_indices.iter().find(|&&idx| app.items[idx].name == "subdir2").expect("subdir2 not found");
        assert_eq!(app.items[*subdir1_idx].name, "subdir1");
        assert_eq!(app.items[*subdir2_idx].name, "subdir2");


        // --- Expand "dir2" ---
        // Find "dir2" item index
        let dir2_global_idx = app.items.iter().position(|item| item.name == "dir2").expect("dir2 not found");

        // Simulate expansion
        app.load_children_and_expand(dir2_global_idx);

        // Await the async message
        let app_message = test_thread_rx.recv_timeout(Duration::from_secs(5)).expect("Did not receive AppMessage for dir2");
        match app_message {
            AppMessage::ChildrenLoaded(result) => app.handle_children_loaded(result),
            _ => panic!("Unexpected AppMessage received for dir2: {:?}", app_message),
        }

        // Assert dir2 state
        let dir2_item = &app.items[dir2_global_idx];
        assert!(dir2_item.is_expanded, "dir2 should be expanded");
        assert!(dir2_item.children_loaded, "dir2 children should be loaded");
        assert!(!dir2_item.children_indices.is_empty(), "dir2 should have children");
        assert_eq!(dir2_item.children_indices.len(), 1, "dir2 should have 1 child");

        // Assert subdir3 exists as a child of dir2
        let subdir3_idx = dir2_item.children_indices.iter().find(|&&idx| app.items[idx].name == "subdir3").expect("subdir3 not found");
        assert_eq!(app.items[*subdir3_idx].name, "subdir3");


        // --- Expand "日本語ディレクトリ" ---
        let jp_dir_global_idx = app.items.iter().position(|item| item.name == "日本語ディレクトリ").expect("日本語ディレクトリ not found");
        app.load_children_and_expand(jp_dir_global_idx);

        let app_message = test_thread_rx.recv_timeout(Duration::from_secs(5)).expect("Did not receive AppMessage for 日本語ディレクトリ");
        match app_message {
            AppMessage::ChildrenLoaded(result) => app.handle_children_loaded(result),
            _ => panic!("Unexpected AppMessage received for 日本語ディレクトリ: {:?}", app_message),
        }

        let jp_dir_item = &app.items[jp_dir_global_idx];
        assert!(jp_dir_item.is_expanded, "日本語ディレクトリ should be expanded");
        assert!(jp_dir_item.children_loaded, "日本語ディレクトリ children should be loaded");
        assert!(!jp_dir_item.children_indices.is_empty(), "日本語ディレクトリ should have children");
        assert_eq!(jp_dir_item.children_indices.len(), 1, "日本語ディレクトリ should have 1 child");
        let jp_subdir_idx = jp_dir_item.children_indices.iter().find(|&&idx| app.items[idx].name == "サブディレクトリ").expect("サブディレクトリ not found");
        assert_eq!(app.items[*jp_subdir_idx].name, "サブディレクトリ");
    }

    #[test]
    fn test_expand_empty_directory() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);

        let (test_thread_tx, test_thread_rx) = mpsc::channel(); // Channel for App's spawned threads to send to test
        let (app_tx_dummy, app_rx_dummy) = mpsc::channel(); // Dummy channel for App's rx
        let mut app = App { tx: test_thread_tx, rx: app_rx_dummy, ..Default::default() };

        app.current_repo_root = repo_path.clone();
        app.load_initial_tree().unwrap();

        let dir3_global_idx = app.items.iter().position(|item| item.name == "dir3").expect("dir3 not found");
        app.load_children_and_expand(dir3_global_idx);

        let app_message = test_thread_rx.recv_timeout(Duration::from_secs(5)).expect("Did not receive AppMessage for dir3");
        match app_message {
            AppMessage::ChildrenLoaded(result) => app.handle_children_loaded(result),
            _ => panic!("Unexpected AppMessage received for dir3: {:?}", app_message),
        }

        let dir3_item = &app.items[dir3_global_idx];
        assert!(dir3_item.is_expanded, "dir3 should be expanded");
        assert!(dir3_item.children_loaded, "dir3 children should be loaded");
        assert!(dir3_item.children_indices.is_empty(), "dir3 should have no children");
    }

    // This test ensures that if a directory is already expanded and children loaded,
    // calling expand_selected_item again just rebuilds visible items without re-querying git.
    #[test]
    fn test_expand_already_expanded_item() {
        let (repo_path, _temp_dir) = setup_git_repo();
        create_and_commit_files(&repo_path);

        let (test_thread_tx, test_thread_rx) = mpsc::channel(); // Channel for App's spawned threads to send to test
        let (app_tx_dummy, app_rx_dummy) = mpsc::channel(); // Dummy channel for App's rx
        let mut app = App { tx: test_thread_tx, rx: app_rx_dummy, ..Default::default() };

        app.current_repo_root = repo_path.clone();
        app.load_initial_tree().unwrap();

        let dir1_global_idx = app.items.iter().position(|item| item.name == "dir1").expect("dir1 not found");

        // Initial expansion
        app.load_children_and_expand(dir1_global_idx);
        let app_message = test_thread_rx.recv_timeout(Duration::from_secs(5)).expect("Did not receive AppMessage for dir1 (first expand)");
        match app_message {
            AppMessage::ChildrenLoaded(result) => app.handle_children_loaded(result),
            _ => panic!("Unexpected AppMessage received for dir1 (first expand): {:?}", app_message),
        }

        // Call expand_selected_item again (which should just expand and rebuild)
        app.expand_selected_item(); // Simulate selecting dir1 and pressing right key

        let dir1_item = &app.items[dir1_global_idx];
        assert!(dir1_item.is_expanded, "dir1 should still be expanded");
        assert!(dir1_item.children_loaded, "dir1 children should still be loaded");
        assert!(!dir1_item.children_indices.is_empty(), "dir1 should still have children");

        // Ensure no new message was sent, indicating git was not re-queried
        assert!(test_thread_rx.try_recv().is_err(), "No new AppMessage should be sent");
    }

    #[test]
    fn test_expand_non_checked_out_directory() {
        let (repo_path, _temp_dir) = setup_git_repo();
        
        // Create a directory structure that is NOT checked out
        fs::create_dir_all(&repo_path.join("virtual_dir/virtual_subdir1")).unwrap();
        fs::write(&repo_path.join("virtual_dir/virtual_subdir1/file.txt"), "content").unwrap();
        fs::create_dir_all(&repo_path.join("virtual_dir/virtual_subdir2")).unwrap();
        fs::write(&repo_path.join("virtual_dir/virtual_subdir2/file.txt"), "content").unwrap();
        
        Command::new("git")
            .args(&["add", "."])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(&["commit", "-m", "Add virtual dirs"])
            .current_dir(&repo_path)
            .output()
            .unwrap();

        // Initialize sparse-checkout but DO NOT CHECK OUT "virtual_dir"
        Command::new("git")
            .args(&["sparse-checkout", "init", "--cone"])
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout init --cone failed");
        
        // Explicitly set sparse-checkout to something else, ensuring virtual_dir is NOT checked out
        Command::new("git")
            .args(&["sparse-checkout", "set", "dir1"]) // Check out 'dir1' if it exists, but not 'virtual_dir'
            .current_dir(&repo_path)
            .output()
            .expect("git sparse-checkout set failed");

        // Verify "virtual_dir" does NOT exist physically
        assert!(!repo_path.join("virtual_dir").is_dir(), "virtual_dir should not exist physically");


        let (test_thread_tx, test_thread_rx) = mpsc::channel();
        let (app_tx_dummy, app_rx_dummy) = mpsc::channel();
        let mut app = App { tx: test_thread_tx, rx: app_rx_dummy, ..Default::default() };
        
        app.current_repo_root = repo_path.clone();
        app.sparse_checkout_dirs = git::get_sparse_checkout_list(&repo_path).unwrap(); // Load sparse checkout info
        app.uncommitted_paths = HashSet::new();

        app.load_initial_tree().unwrap(); // Load root and first level


        // --- Expand "virtual_dir" ---
        let virtual_dir_global_idx = app.items.iter().position(|item| item.name == "virtual_dir").expect("virtual_dir not found");

        app.load_children_and_expand(virtual_dir_global_idx);

        let app_message = test_thread_rx.recv_timeout(Duration::from_secs(5)).expect("Did not receive AppMessage for virtual_dir");
        match app_message {
            AppMessage::ChildrenLoaded(result) => app.handle_children_loaded(result),
            _ => panic!("Unexpected AppMessage received for virtual_dir: {:?}", app_message),
        }

        let virtual_dir_item = &app.items[virtual_dir_global_idx];
        assert!(virtual_dir_item.is_expanded, "virtual_dir should be expanded");
        assert!(virtual_dir_item.children_loaded, "virtual_dir children should be loaded");
        assert!(!virtual_dir_item.children_indices.is_empty(), "virtual_dir should have children");
        assert_eq!(virtual_dir_item.children_indices.len(), 2, "virtual_dir should have 2 children");

        let virtual_subdir1_idx = virtual_dir_item.children_indices.iter().find(|&&idx| app.items[idx].name == "virtual_subdir1").expect("virtual_subdir1 not found");
        let virtual_subdir2_idx = virtual_dir_item.children_indices.iter().find(|&&idx| app.items[idx].name == "virtual_subdir2").expect("virtual_subdir2 not found");
        assert_eq!(app.items[*virtual_subdir1_idx].name, "virtual_subdir1");
        assert_eq!(app.items[*virtual_subdir2_idx].name, "virtual_subdir2");
    }
}
