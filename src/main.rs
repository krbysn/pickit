use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

use ratatui::layout::{Alignment, Rect};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table},
    Terminal,
};
use std::{
    error::Error,
    io,
    io::Stdout,
    path::PathBuf,
    sync::mpsc::{self},
    time::Duration,
};

mod app;
mod git;

/// A TUI for git sparse-checkout.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The path to the git repository.
    #[arg()]
    path: Option<PathBuf>,
}

// Event types for main loop
enum InputEvent {
    Input(Event),

    App(app::AppMessage), // New variant for App-specific messages
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    if let Some(path_ref) = cli.path.as_ref() {
        std::env::set_current_dir(path_ref)?;
    }



    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run it
    let mut app = match app::App::new(cli.path.as_ref()) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("Error initializing application: {}", e);
            restore_terminal(&mut terminal)?;
            return Err(e.into());
        }
    };

    let res = run_app(&mut terminal, &mut app);

    // Restore terminal
    restore_terminal(&mut terminal)?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut app::App,
) -> io::Result<()> {
    let mut list_state = ListState::default();

    loop {
        let event = if event::poll(Duration::from_millis(10))? {
            Some(InputEvent::Input(event::read()?))
        } else {
            // Always try to receive from app.rx first, then file_change_receiver
            match app.rx.try_recv() {
                Ok(app_msg) => Some(InputEvent::App(app_msg)),
                Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("App message sender disconnected.");
                    None
                }
                Err(mpsc::TryRecvError::Empty) => {
                    None // No events from any source
                }
            }
        };

        // Process keyboard input if any
        if let Some(input_event) = event {
            match input_event {

                InputEvent::Input(Event::Key(key)) => {
                    if key.kind == KeyEventKind::Press {
                        // Clear error on any key press
                        app.last_git_error = None;

                        // Normal application key handling
                        match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Up => app.move_cursor_up(),
                            KeyCode::Down => app.move_cursor_down(),
                            KeyCode::PageUp => {
                                let tree_view_height =
                                    terminal.size()?.height.saturating_sub(3).saturating_sub(2);
                                app.move_cursor_page_up(tree_view_height);
                            }
                            KeyCode::PageDown => {
                                let tree_view_height =
                                    terminal.size()?.height.saturating_sub(3).saturating_sub(2);
                                app.move_cursor_page_down(tree_view_height);
                            }
                            KeyCode::Right => {
                                app.expand_selected_item();
                            }
                            KeyCode::Left => {
                                app.handle_left_key();
                            }
                            KeyCode::Char(' ') => app.toggle_selection(),
                            KeyCode::Char('a') => {
                                app.is_applying_changes = true; // Set flag to show loading dialog
                                app.apply_changes(); // Directly call apply_changes
                            }
                            KeyCode::Char('r') => { // New 'r' key handling
                                app.is_refreshing = true;
                                app.refresh();
                            }
                            _ => {}
                        }
                    }
                }
                InputEvent::App(app_msg) => {
                    match app_msg {
                        app::AppMessage::ApplyChangesCompleted(result) => {
                            app.is_applying_changes = false; // Reset the flag
                            match result {
                                Ok(_) => {
                                    // Clear pending changes on all items (this was moved from App::apply_changes)
                                    for item in app.items.iter_mut() {
                                        item.pending_change = None;
                                    }
                                    app.refresh(); // Now asynchronous
                                }
                                Err(e) => {
                                    app.last_git_error = Some(e.to_string());
                                }
                            }
                        }
                        app::AppMessage::ChildrenLoaded(result) => {
                            app.handle_children_loaded(result);
                        }
                        app::AppMessage::RefreshCompleted(result) => {
                            app.handle_refresh_completed(result);
                        }

                    }
                }
                _ => {} // Other events like mouse, resize etc.
            }
        }

        terminal.draw(|f| {
            if app.is_applying_changes {
                // Render a loading screen
                let size = f.area();
                let block = Block::default()
                    .title("Applying Changes")
                    .borders(Borders::ALL);
                let paragraph = Paragraph::new("Applying changes... Please wait.")
                    .style(Style::default().fg(Color::White).bg(Color::Black))
                    .alignment(Alignment::Center)
                    .block(block);

                let area = Rect::new(
                    size.width / 4,
                    size.height / 3,
                    size.width / 2,
                    size.height / 6,
                );
                f.render_widget(paragraph, area);
            } else if app.is_refreshing {
                // Render refresh loading dialog
                let size = f.area();
                let block = Block::default()
                    .title("Refreshing")
                    .borders(Borders::ALL);
                let paragraph = Paragraph::new("Refreshing application state... Please wait.")
                    .style(Style::default().fg(Color::White).bg(Color::Black))
                    .alignment(Alignment::Center)
                    .block(block);

                let area = Rect::new(
                    size.width / 4,
                    size.height / 3,
                    size.width / 2,
                    size.height / 6,
                );
                f.render_widget(paragraph, area);
            } else {
                // Render the main TUI
                let size = f.area();

                // Define main layout (main_area + footer)
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(0), Constraint::Length(3)])
                    .split(size);

                let main_area = chunks[0];
                let footer_area = chunks[1];

                // Split main_area into tree (left) and grid (right)
                let main_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(main_area);

                let tree_area = main_chunks[0];
                let grid_area = main_chunks[1];

                // --- Tree View ---
                let tree_items_vm = app.get_tui_tree_items();
                let tree_items: Vec<ListItem> = tree_items_vm
                    .into_iter()
                    .map(|vm| ListItem::new(Line::from(vm.display_text)).style(vm.style))
                    .collect();

                list_state.select(Some(app.selected_item_index));

                let tree_list = List::new(tree_items)
                    .block(Block::default().borders(Borders::ALL).title(" Tree View "));

                f.render_stateful_widget(tree_list, tree_area, &mut list_state);

                // --- Grid View ---
                let grid_title = " Grid View ";
                if let Some(grid_vm) = app.get_grid_view_model() {
                    let rows = vec![
                        Row::new(vec![Cell::new("Name"), Cell::new(grid_vm.name)]),
                        Row::new(vec![Cell::new("Path"), Cell::new(grid_vm.path)]),
                        Row::new(vec![Cell::new("Status"), Cell::new(grid_vm.status)]),
                        Row::new(vec![
                            Cell::new("Uncommitted"),
                            Cell::new(grid_vm.uncommitted),
                        ]),
                        Row::new(vec![
                            Cell::new("Subdirectories (Total)"),
                            Cell::new(grid_vm.subdirectories_total),
                        ]),
                        Row::new(vec![
                            Cell::new("Subdirectories (Checked Out)"),
                            Cell::new(grid_vm.subdirectories_checked_out),
                        ]),
                        Row::new(vec![
                            Cell::new("Pending Changes"),
                            Cell::new(grid_vm.pending_changes),
                        ]),
                    ];

                    let table = Table::new(
                        rows,
                        &[Constraint::Percentage(50), Constraint::Percentage(50)],
                    )
                    .block(Block::default().borders(Borders::ALL).title(grid_title));
                    f.render_widget(table, grid_area);
                } else {
                    let grid_block = Block::default().borders(Borders::ALL).title(grid_title);
                    f.render_widget(grid_block, grid_area);
                }

                // --- Footer ---
                let footer_text = if let Some(err) = &app.last_git_error {
                    err.clone()
                } else {
                    " [q] Quit [Space] Toggle [a] Apply [r] Refresh [↑/↓] Navigate [→] Expand [←] Coll/Parent [PgUp/Dn] Scroll "
                        .to_string()
                };
                let footer_block = Block::default().borders(Borders::ALL).title(footer_text);
                f.render_widget(footer_block, footer_area);
            }
        })?; // Correctly closes the terminal.draw call
    }
}
