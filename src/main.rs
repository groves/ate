use std::cell::RefCell;
use std::io::{stdin, Read};
use std::rc::Rc;

use termwiz::caps::Capabilities;
use termwiz::cell::{grapheme_column_width, AttributeChange, CellAttributes};
use termwiz::color::ColorAttribute;
use termwiz::escape::csi::Sgr;
use termwiz::escape::parser::Parser;
use termwiz::escape::Action::{self, Control, Print};
use termwiz::escape::ControlCode::LineFeed;
use termwiz::escape::CSI;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::{Change, Position::Absolute};

use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::{new_terminal, Terminal};
use termwiz::widgets::{RenderArgs, Ui, UpdateArgs, Widget, WidgetEvent};
use termwiz::Error;

struct Document {
    text: String,
    attrs: Vec<(usize, Change)>,
    state: Rc<RefCell<DocState>>,
}

impl Document {
    fn new(mut input: Box<dyn Read>) -> Result<Document, Error> {
        // TODO - lazily read and parse in Document::render
        let mut buf = vec![];
        let read = input.read_to_end(&mut buf)?;
        let mut text = String::new();
        let mut attrs = vec![(0, Change::AllAttributes(CellAttributes::default()))];
        Parser::new().parse(&buf[0..read], |a| {
            match a {
                Print(c) => text.push(c),
                Control(LineFeed) => text.push('\n'),
                // TODO - hyperlinks
                Action::CSI(CSI::Sgr(s)) => {
                    let change = match s {
                        Sgr::Reset => Change::AllAttributes(CellAttributes::default()),
                        ac => Change::Attribute(match ac {
                            Sgr::Intensity(i) => AttributeChange::Intensity(i),
                            Sgr::Background(b) => AttributeChange::Background(b.into()),
                            Sgr::Underline(u) => AttributeChange::Underline(u),
                            Sgr::Blink(b) => AttributeChange::Blink(b),
                            Sgr::Italic(i) => AttributeChange::Italic(i),
                            Sgr::Invisible(i) => AttributeChange::Invisible(i),
                            Sgr::StrikeThrough(s) => AttributeChange::StrikeThrough(s),
                            Sgr::Foreground(f) => AttributeChange::Foreground(f.into()),
                            Sgr::Inverse(_) => todo!(),
                            Sgr::UnderlineColor(_) => todo!(),
                            Sgr::Font(_) => todo!(),
                            Sgr::Overline(_) => todo!(),
                            Sgr::Reset => unreachable!(),
                        }),
                    };
                    attrs.push((text.len(), change));
                }
                _ => (),
            };
        });
        Ok(Document {
            text,
            attrs,
            state: Rc::new(RefCell::new(DocState {
                line: 0,
                height: 0,
                width: 0,
            })),
        })
    }
}

impl Widget for Document {
    // Takes a start line, a number of lines to render, and the width in character cells of those
    // lines.
    // Returns the changes necessary to render those lines assuming a terminal cursor at the
    // start of the first line and the lines rendered.
    // TODO - Reads from the underlying stream if it hasn't been exhausted and more data is needed to fill
    // the lines.
    // May return an error if reading produced an error.
    // If the returned lines are fewer than requested, EOF has been reached.
    fn render(&mut self, args: &mut RenderArgs) {
        let (width, height) = args.surface.dimensions();
        assert!(width > 0);
        assert!(height > 0);
        let state_line = {
            let mut state = self.state.borrow_mut();
            state.height = height;
            state.width = width;
            state.line
        };
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
        ];
        use unicode_segmentation::UnicodeSegmentation;
        let mut graphemes = self
            .text
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));
        let mut text_idx = 0;
        let mut attr_index = 0;
        let mut cells_in_line = 0;
        let end = state_line + height;
        let mut line = 0;
        while line < end {
            if let Some((grapheme, cells)) = graphemes.next() {
                if cells_in_line + cells > width || grapheme == "\n" {
                    line += 1;
                    cells_in_line = 0;
                    if line >= state_line && line < end {
                        changes.push(Change::Text("\r\n".to_string()));
                    }
                }
                if grapheme != "\n" {
                    while attr_index < self.attrs.len() && text_idx >= self.attrs[attr_index].0 {
                        changes.push(self.attrs[attr_index].1.clone());
                        attr_index += 1;
                    }
                    if line >= state_line {
                        changes.push(Change::Text(grapheme.to_string()));
                    }
                    cells_in_line += cells;
                }
                text_idx += grapheme.len();
            } else {
                // TODO - read more out of input. Simpler thing will be if graphemes is empty.
                // More correct thing will be if we're within a grapheme of the end to see if
                // there are any zwjs that need to be added to what's in the buffer.
                // Maybe call next twice so we're two out?
                break;
            }
        }
        args.surface.add_changes(changes);
    }

    fn process_event(&mut self, event: &WidgetEvent, _args: &mut UpdateArgs) -> bool {
        return match event {
            WidgetEvent::Input(i) => match i {
                InputEvent::Key(k) => match k {
                    KeyEvent {
                        key: KeyCode::Char(' '),
                        ..
                    } => {
                        let mut state = self.state.borrow_mut();
                        state.line += state.height - 2;
                        true
                    }
                    KeyEvent {
                        key: KeyCode::Char('b'),
                        ..
                    } => {
                        let mut state = self.state.borrow_mut();
                        state.line -= state.height - 2;
                        true
                    }
                    KeyEvent { .. } => false,
                },
                _ => false,
            },
        };
    }
}

fn main() -> Result<(), Error> {
    let caps = Capabilities::new_from_env()?;
    let underlying_term = new_terminal(caps)?;
    let mut term = BufferedTerminal::new(underlying_term)?;
    term.terminal().set_raw_mode()?;
    let mut ui = Ui::new();
    let doc = Document::new(Box::new(stdin()))?;

    let root_id = ui.set_root(doc);

    ui.set_focus(root_id);

    loop {
        ui.process_event_queue()?;

        // After updating and processing all of the widgets, compose them
        // and render them to the screen.
        if ui.render_to_screen(&mut term)? {
            // We have more events to process immediately; don't block waiting
            // for input below, but jump to the top of the loop to re-run the
            // updates.
            continue;
        }
        // Compute an optimized delta to apply to the terminal and display it
        term.flush()?;

        // Wait for user input
        match term.terminal().poll_input(None) {
            Ok(Some(InputEvent::Resized { rows, cols })) => {
                // FIXME: this is working around a bug where we don't realize
                // that we should redraw everything on resize in BufferedTerminal.
                term.add_change(Change::ClearScreen(Default::default()));
                term.resize(cols, rows);
            }
            Ok(Some(input)) => match input {
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('q'),
                    ..
                }) => {
                    // Quit the app when escape or qis pressed
                    break;
                }
                _ => {
                    // Feed input into the Ui
                    ui.queue_event(WidgetEvent::Input(input));
                }
            },
            Ok(None) => {}
            Err(e) => {
                print!("{:?}\r\n", e);
                break;
            }
        }
    }

    Ok(())
}

struct DocState {
    line: usize,
    height: usize,
    width: usize,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use termwiz::{color::ColorAttribute, surface::Surface};

    use super::*;

    struct Context<'a> {
        ui: Ui<'a>,
        state: Rc<RefCell<DocState>>,
        surface: Surface,
    }

    fn create_ui(input: &str, width: usize, height: usize) -> Context {
        let doc = Document::new(Box::new(Cursor::new(input.to_string()))).unwrap();
        let mut ctx = Context {
            ui: Ui::new(),
            state: doc.state.clone(),
            surface: Surface::new(width, height),
        };
        ctx.ui.set_root(doc);
        ctx.ui.queue_event(WidgetEvent::Input(InputEvent::Resized {
            cols: width,
            rows: height,
        }));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        ctx
    }

    #[test]
    fn parse_color_output() {
        let input = "D\x1b[31mR\x1b[mD";
        let mut ctx = create_ui(input, 3, 1);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!(ColorAttribute::Default, cells[0].attrs().foreground());
        assert_eq!(
            ColorAttribute::PaletteIndex(1),
            cells[1].attrs().foreground()
        );
        assert_eq!(ColorAttribute::Default, cells[2].attrs().foreground());
        assert_eq!(ctx.surface.screen_chars_to_string(), "DRD\n");
    }

    #[test]
    fn render_short_doc() {
        let ctx = create_ui("Hi Bye", 3, 2);
        assert_eq!(ctx.surface.screen_chars_to_string(), "Hi \nBye\n");
    }

    #[test]
    fn render_backwards() {
        let input = "1\n2\n3\n";
        let mut ctx = create_ui(input, 1, 1);
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());
        ctx.state.borrow_mut().line = 1;
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cell = {
            let cells = ctx.surface.screen_cells();
            cells[0][0].str().to_string()
        };
        assert_eq!(
            "2",
            cell,
            "Expected screen to just be '2' but got '{}'",
            ctx.surface.screen_chars_to_string(),
        );
        ctx.state.borrow_mut().line = 0;
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());
    }
}
