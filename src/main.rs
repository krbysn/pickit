use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    text::Line,
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Row, Table},
    Terminal,
};
use std::{error::Error, io, io::Stdout, path::PathBuf, time::Duration};

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

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    if let Some(path) = cli.path {
        std::env::set_current_dir(path)?;
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run it
    let mut app = match app::App::new() {
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

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), Box<dyn std::error::Error>> {
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
        terminal.draw(|f| {
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

            let tree_list =
                List::new(tree_items).block(Block::default().borders(Borders::ALL).title(" Tree View "));

            f.render_stateful_widget(tree_list, tree_area, &mut list_state);

            // --- Grid View ---
            let grid_title = " Grid View ";
            if let Some(grid_vm) = app.get_grid_view_model() {
                let rows = vec![
                    Row::new(vec![Cell::new("Name"), Cell::new(grid_vm.name)]),
                    Row::new(vec![Cell::new("Path"), Cell::new(grid_vm.path)]),
                    Row::new(vec![Cell::new("Status"), Cell::new(grid_vm.status)]),
                    Row::new(vec![Cell::new("Uncommitted"), Cell::new(grid_vm.uncommitted)]),
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
                " [↑/↓] Navigate [→/←] Expand/Collapse [Space] Toggle [a] Apply [q] Quit ".to_string()
            };
            let footer_block = Block::default()
                .borders(Borders::ALL)
                .title(footer_text);
            f.render_widget(footer_block, footer_area);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // It's important to clear the error on any key press
                app.last_git_error = None;
                
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Up => app.move_cursor_up(),
                    KeyCode::Down => app.move_cursor_down(),
                    KeyCode::Left => app.toggle_expansion(),
                    KeyCode::Right => app.toggle_expansion(),
                    KeyCode::Char(' ') => app.toggle_selection(),
                    KeyCode::Char('a') => app.apply_changes(),
                    _ => {}
                }
            }
        }
    }
}