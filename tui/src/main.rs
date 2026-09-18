//! The `forge-tui` binary: a thin shell over the `forge_tui` library, which
//! holds the `App` state and the `draw` rendering that `tui/tests/` also
//! drives.

use anyhow::Result;
use forge_client::Forge;
use forge_tui::{App, draw, render_text};

fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--dump") {
        // One frame, no terminal: for a script or a smoke test.
        let mut app = App::new(Forge::new());
        app.snapshot();
        app.shutdown();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|f| draw(f, &app))?;
        print!("{}", render_text(terminal.backend()));
        return Ok(());
    }
    forge_tui::run()
}
