mod app;
mod git;

use std::{
    io::{self, Stdout},
    time::Duration,
};

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
    widgets::{Block, Borders, List, ListItem},
    Terminal,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run it
    let app = match app::App::new() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("Error initializing application: {}", e);
            restore_terminal(&mut terminal)?;
            return Err(e.into());
        }
    };

    let res = run_app(&mut terminal, app);

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
    app: app::App,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| {
            let size = f.area();

            // Define main layout (main_area + footer)
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)]) // Main area, then 3 lines for footer
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
            let tree_items: Vec<ListItem> = app.filtered_item_indices
                .iter()
                .map(|&idx| {
                    let item = &app.items[idx];
                    ListItem::new(item.name.clone())
                })
                .collect();
            let tree_list = List::new(tree_items)
                .block(Block::default().borders(Borders::ALL).title(" Tree View "));
            f.render_widget(tree_list, tree_area);


            // --- Grid View ---
            let grid_block = Block::default()
                .borders(Borders::ALL)
                .title(" Grid View ");
            f.render_widget(grid_block, grid_area);

            // --- Footer ---
            let footer_block = Block::default()
                .borders(Borders::ALL)
                .title(" Footer - Press 'q' to quit ");
            f.render_widget(footer_block, footer_area);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if KeyCode::Char('q') == key.code {
                    return Ok(());
                }
            }
        }
    }
}