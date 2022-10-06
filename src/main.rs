use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;
use log::{debug, info};
use std::env;
use std::env::VarError;
use std::io::stdin;
use std::process;
use std::process::Command;
use termwiz::caps::Capabilities;
use termwiz::input::InputEvent;
use termwiz::surface::Change;
use termwiz::widgets::Ui;

use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::{new_terminal, Terminal};
use termwiz::widgets::WidgetEvent;
mod doc;
mod ui;

fn open(uri: &str) -> Result<()> {
    let opener = match env::var("ATE_OPENER") {
        Ok(val) => val,
        Err(e) => match e {
            VarError::NotPresent => bail!("ATE_OPENER must be defined to open links"),
            _ => bail!(e),
        },
    };
    info!("Using ATE_OPENER {}", opener);
    // TODO - don't block forever waiting on this, complain if it takes too long
    let output = match Command::new(&opener).arg(uri).output() {
        Ok(o) => o,
        // Don't use anyhow::context as it adds newlines
        Err(e) => bail!("Failed to run ATE_OPENER {}: {}", opener, e),
    };
    info!("ATE_OPENER stdout={}", String::from_utf8(output.stdout)?);
    let stderr = String::from_utf8(output.stderr)?;
    info!("ATE_OPENER stderr={}", stderr);
    match output.status.code() {
        Some(0) | None => Ok(()),
        Some(c) => {
            bail!(
                "ATE_OPENER {} failed with code={} stderr={}",
                opener,
                c,
                stderr
            );
        }
    }
}

struct Ate<'a, T: Terminal> {
    ctx: ui::RefCtx<'a>,
    term: BufferedTerminal<T>,
    ui: Ui<'a>,
}

impl<'a, T: Terminal> Ate<'a, T> {
    fn run(&mut self) -> Result<()> {
        let mut open_first = env::var("ATE_OPEN_FIRST").is_ok();
        debug!("Open first link?{}", open_first);
        loop {
            let size = self.term.terminal().get_screen_size()?;
            self.ctx.term_height.set(size.rows);
            self.ui.process_event_queue()?;
            if self.ctx.quit.get() {
                break;
            }
            self.ui.set_focus(self.ctx.focus.get());

            // After updating and processing all of the widgets, compose them
            // and render them to the screen.
            if self.ui.render_to_screen(&mut self.term)? {
                // We have more events to process immediately; don't block waiting
                // for input below, but jump to the top of the loop to re-run the
                // updates.
                continue;
            } else if open_first {
                open_first = false;
                self.ctx.open_selected();
                continue;
            }
            // Compute an optimized delta to apply to the terminal and display it
            self.term.flush()?;

            // Wait for user input
            match self.term.terminal().poll_input(None) {
                Ok(Some(input)) => match input {
                    InputEvent::Resized { rows, cols } => {
                        // FIXME: this is working around a bug where we don't realize
                        // that we should redraw everything on resize in BufferedTerminal.
                        self.term
                            .add_change(Change::ClearScreen(Default::default()));
                        self.term.resize(cols, rows);
                        self.ctx.term_height.set(rows);
                        self.ui.queue_event(WidgetEvent::Input(input));
                    }
                    _ => {
                        // Feed input into the Ui
                        self.ui.queue_event(WidgetEvent::Input(input));
                    }
                },
                Ok(None) => {}
                Err(e) => {
                    return Err(anyhow!(e));
                }
            }
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix("ate")?;
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {} {} {}",
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
                record.target(),
                record.level(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .chain(fern::log_file(xdg_dirs.place_state_file("log")?)?)
        .apply()?;
    info!("ate started");
    if atty::is(atty::Stream::Stdin) {
        eprintln!("ate displays data from stdin i.e. pipe or redirect to ate");
        process::exit(1);
    }

    let caps = Capabilities::new_from_env()?;
    let underlying_term = new_terminal(caps)?;
    let mut term = BufferedTerminal::new(underlying_term)?;
    term.terminal().set_raw_mode()?;
    term.terminal().enter_alternate_screen()?;
    let size = term.terminal().get_screen_size()?;

    let (ui, ctx) = ui::create_ui(Box::new(stdin()), size.cols, size.rows, Box::new(open))?;

    Ate { ctx, term, ui }.run()
}
