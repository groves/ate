use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;
use backtrace::Backtrace;
use log::error;
use log::{debug, info};
use std::env;
use std::env::VarError;
use std::io::stdin;
use std::panic;
use std::process;
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use termwiz::caps::Capabilities;
use termwiz::input::InputEvent;
use termwiz::input::KeyCode;
use termwiz::input::KeyEvent;
use termwiz::input::Modifiers;
use termwiz::surface::Change;
use termwiz::terminal::SystemTerminal;
use ui::AteUi;
use ui::StepNext;

use crate::widgets::WidgetEvent;
use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::Terminal;
mod doc;
mod state;
mod ui;
mod widgets;

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

struct Ate<'a> {
    term: BufferedTerminal<SystemTerminal>,
    ui: AteUi<'a>,
    // Fields are dropped in declaration order.
    // Sticking this here gets it to be dropped after term.
    _dl: DropLast,
}

impl<'a> Ate<'a> {
    fn run(&mut self) -> Result<()> {
        while let StepNext::WAIT = self.ui.step(&mut self.term)? {
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

// A message created by our panic hook if it ran
static PANIC_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

struct DropLast {}
impl Drop for DropLast {
    fn drop(&mut self) {
        // If our hook set a message, print it here.
        // We're running after SystemTerminal has been dropped.
        // That means we'll be out of the alternate screen.
        if let Some(message) = PANIC_MESSAGE.lock().unwrap().take() {
            eprintln!("{message}");
        }
    }
}

fn setup_logging() -> Result<()> {
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
    Ok(())
}
fn main() -> Result<()> {
    setup_logging()?;
    info!("ate started");
    if atty::is(atty::Stream::Stdin) {
        eprintln!("ate displays data from stdin i.e. pipe or redirect to ate");
        process::exit(1);
    }

    let caps = Capabilities::new_from_env()?;
    let underlying_term = SystemTerminal::new(caps)?;
    let mut term = BufferedTerminal::new(underlying_term)?;
    term.terminal().set_raw_mode()?;
    term.terminal().enter_alternate_screen()?;

    // We can't use the default panic handler.
    // It prints to stderr immediately aand we'll be in the alternate screen after this.
    // Printing to stderr is lost when returning to the main screen.
    panic::set_hook(Box::new(|info| {
        // Build a description of the panic.
        // This is essentially https://github.com/sfackler/rust-log-panics/blob/e1b352b61c03a8a87cc647815d5eddb98829882f/src/lib.rs#L115
        // That doesn't allow calling to get the message that would be logged.
        // We want to print the description later, so we need to create the value ourselves here.
        let bt = Backtrace::new();
        let thread = thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let msg = match info.payload().downcast_ref::<&'static str>() {
            Some(s) => *s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => &**s,
                None => "Box<Any>",
            },
        };
        let formatted = match info.location() {
            Some(location) => {
                format!(
                    "thread '{}' panicked at '{}': {}:{}\n{:?}",
                    thread,
                    msg,
                    location.file(),
                    location.line(),
                    bt,
                )
            }
            None => {
                format!("thread '{}' panicked at '{}'\n{:?}", thread, msg, bt,)
            }
        };
        // Log the panic message we created
        error!("{formatted}");
        // Store the message for to print to stderr after exiting the alternate screen
        let mut pmsg = PANIC_MESSAGE.lock().unwrap();
        *pmsg = Some(formatted);
    }));

    let size = term.terminal().get_screen_size()?;

    let mut ui = ui::create_ui(Box::new(stdin()), size.cols, size.rows, Box::new(open))?;

    if env::var("ATE_OPEN_FIRST").is_ok() {
        debug!("Opening first link");
        ui.queue_event(WidgetEvent::Input(InputEvent::Key(KeyEvent {
            key: KeyCode::Enter,
            modifiers: Modifiers::NONE,
        })));
    }

    if env::var("ATE_GOTO_LAST").is_ok() {
        debug!("Going to last link");
        ui.queue_event(WidgetEvent::Input(InputEvent::Key(KeyEvent {
            key: KeyCode::Char('N'),
            modifiers: Modifiers::NONE,
        })));
    }

    Ate {
        term,
        ui,
        _dl: DropLast {},
    }
    .run()
}
