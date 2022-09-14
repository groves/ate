use std::io::{stdin, Read};
use std::process::Command;

use termwiz::caps::Capabilities;
use termwiz::cell::{grapheme_column_width, AttributeChange, CellAttributes};
use termwiz::color::ColorAttribute;
use termwiz::escape::csi::Sgr;
use termwiz::escape::parser::Parser;
use termwiz::escape::Action::{self, Control, Print};
use termwiz::escape::ControlCode::LineFeed;
use termwiz::escape::{OperatingSystemCommand, CSI};
use termwiz::hyperlink::Hyperlink;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::{Change, Position::Absolute};

use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::{new_terminal, Terminal};
use termwiz::widgets::{RenderArgs, Ui, UpdateArgs, Widget, WidgetEvent};
use termwiz::Error;

struct LinkRange {
    start: usize,
    end: usize,
    link: Hyperlink,
}

struct Document<'a> {
    text: String,
    attrs: Vec<(usize, Change)>,
    links: Vec<LinkRange>,
    input: Box<dyn Read + 'a>,
}

impl<'a> Document<'a> {
    fn new(mut input: Box<dyn Read + 'a>) -> Result<Document<'a>, Error> {
        // TODO - lazily read and parse in Document::render
        let mut buf = vec![];
        let read = input.read_to_end(&mut buf)?;
        let mut text = String::new();
        let mut links = vec![];
        let mut attrs = vec![(0, Change::AllAttributes(CellAttributes::default()))];
        let mut partial_link: Option<(usize, Hyperlink)> = None;
        let mut complete_link = |start, link, end| links.push(LinkRange { start, link, end });
        Parser::new().parse(&buf[0..read], |a| {
            match a {
                Print(c) => text.push(c),
                Control(LineFeed) => text.push('\n'),
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
                Action::OperatingSystemCommand(osc) => {
                    match *osc {
                        OperatingSystemCommand::SetHyperlink(parsed_link) => {
                            // SetHyperlink may have the current partial link in it.
                            // We may have just ended the link that's in there, too.
                            // We don't try to collapse repeated links into a single range.
                            // Instead we assume the output repeated links for some reason and
                            // faithfully recreate it.
                            if let Some((start, link)) = partial_link.take() {
                                complete_link(start, link, text.len());
                            }
                            partial_link = parsed_link.map(|l| (text.len(), l));
                        }
                        _ => {}
                    };
                }
                _ => (),
            };
        });
        if let Some((start, link)) = partial_link {
            complete_link(start, link, text.len());
        }
        Ok(Document {
            text,
            attrs,
            links,
            input,
        })
    }
}

struct DocumentWidget<'a> {
    doc: Document<'a>,
    line: usize,
    height: usize,
    width: usize,
    link_idx: usize,
    open_link: Box<dyn FnMut(&str) + 'a>,
}

impl<'a> DocumentWidget<'a> {
    fn process_key(&mut self, event: &KeyEvent) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::Char(' '),
                ..
            } => {
                self.line += self.height - 2;
                true
            }
            KeyEvent {
                key: KeyCode::Char('b'),
                ..
            } => {
                self.line -= self.height - 2;
                true
            }
            KeyEvent {
                key: KeyCode::Char('n'),
                ..
            } => {
                let x = &self.doc.links[self.link_idx];
                self.link_idx += 1;
                (self.open_link)(x.link.uri());
                true
            }
            KeyEvent { .. } => false,
        }
    }

    fn new(doc: Document<'a>, open_link: Box<dyn FnMut(&str) + 'a>) -> DocumentWidget<'a> {
        DocumentWidget {
            doc,
            open_link,
            line: 0,
            height: 0,
            width: 0,
            link_idx: 0,
        }
    }
}

impl<'a> Widget for DocumentWidget<'a> {
    // TODO - Reads from the underlying stream if it hasn't been exhausted and more data is needed to fill
    // the lines.
    fn render(&mut self, args: &mut RenderArgs) {
        let (width, height) = args.surface.dimensions();
        assert!(width > 0);
        assert!(height > 0);
        self.height = height;
        self.width = width;
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
        ];
        use unicode_segmentation::UnicodeSegmentation;
        let mut graphemes = self
            .doc
            .text
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));
        let mut text_idx = 0;
        let mut attr_index = 0;
        let mut cells_in_line = 0;
        let end = self.line + height;
        let mut line = 0;
        while line < end {
            if let Some((grapheme, cells)) = graphemes.next() {
                if cells_in_line + cells > width || grapheme == "\n" {
                    line += 1;
                    cells_in_line = 0;
                    if line >= self.line && line < end {
                        changes.push(Change::Text("\r\n".to_string()));
                    }
                }
                if grapheme != "\n" {
                    while attr_index < self.doc.attrs.len()
                        && text_idx >= self.doc.attrs[attr_index].0
                    {
                        changes.push(self.doc.attrs[attr_index].1.clone());
                        attr_index += 1;
                    }
                    if line >= self.line {
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
        match event {
            WidgetEvent::Input(i) => match i {
                InputEvent::Key(k) => self.process_key(k),
                _ => false,
            },
        }
    }
}

fn create_ui<'a>(
    input: Box<dyn Read + 'a>,
    width: usize,
    height: usize,
    open_link: Box<dyn FnMut(&str) + 'a>,
) -> Result<Ui<'a>, Error> {
    let doc = Document::new(input)?;
    let widget = DocumentWidget::new(doc, open_link);
    let mut ui = Ui::new();
    let root_id = ui.set_root(widget);
    ui.set_focus(root_id);

    // Send a resize event through to get us to do an initial layout
    ui.queue_event(WidgetEvent::Input(InputEvent::Resized {
        cols: width,
        rows: height,
    }));
    ui.process_event_queue()?;
    Ok(ui)
}

fn main() -> Result<(), Error> {
    let caps = Capabilities::new_from_env()?;
    let underlying_term = new_terminal(caps)?;
    let mut term = BufferedTerminal::new(underlying_term)?;
    term.terminal().set_raw_mode()?;
    term.terminal().enter_alternate_screen()?;
    let size = term.terminal().get_screen_size()?;

    let mut ui = create_ui(
        Box::new(stdin()),
        size.cols,
        size.rows,
        Box::new(|uri| {
            let output = Command::new(",edit").arg(uri).output().unwrap();
            println!("{}", String::from_utf8(output.stdout).unwrap());
        }),
    )?;

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
            Ok(Some(input)) => match input {
                InputEvent::Resized { rows, cols } => {
                    // FIXME: this is working around a bug where we don't realize
                    // that we should redraw everything on resize in BufferedTerminal.
                    term.add_change(Change::ClearScreen(Default::default()));
                    term.resize(cols, rows);
                    ui.queue_event(WidgetEvent::Input(input));
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('q'),
                    ..
                }) => {
                    // Quit the app when escape or q is pressed
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use termwiz::{color::ColorAttribute, input::Modifiers, surface::Surface};

    use super::*;

    struct Context<'a> {
        ui: Ui<'a>,
        surface: Surface,
    }

    fn create_test_ui(input: &str, width: usize, height: usize) -> Context {
        let mut ui = create_ui(
            Box::new(Cursor::new(input.to_string())),
            width,
            height,
            Box::new(|_uri| {}),
        )
        .unwrap();
        let mut surface = Surface::new(width, height);
        // Render twice to test if we're stepping on ourselves
        ui.render_to_screen(&mut surface).unwrap();
        ui.render_to_screen(&mut surface).unwrap();
        Context { ui, surface }
    }

    #[test]
    fn parse_color_output() {
        let input = "D\x1b[31mR\x1b[mD";
        let mut ctx = create_test_ui(input, 3, 1);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!(ColorAttribute::Default, cells[0].attrs().foreground());
        assert_eq!(
            ColorAttribute::PaletteIndex(1),
            cells[1].attrs().foreground()
        );
        assert_eq!(ColorAttribute::Default, cells[2].attrs().foreground());
        assert_eq!(ctx.surface.screen_chars_to_string(), "DRD\n");
    }
    fn parse_links(input: &str) -> Vec<LinkRange> {
        let doc = Document::new(Box::new(Cursor::new(input.to_string()))).unwrap();
        doc.links
    }

    #[test]
    fn parse_zero_length_link() {
        let links = parse_links("\x1b]8;;http://a.b\x1b\\\x1b]8;;\x1b\\After zero length link");
        assert_eq!(1, links.len());
        assert_eq!(0, links[0].start);
        assert_eq!(0, links[0].end);
    }
    #[test]
    fn parse_continued_link() {
        let links = parse_links("Before\x1b]8;;http://a.b\x1b\\\x1b]8;;http://a.b\x1b\\After");
        assert_eq!(2, links.len());
        assert_eq!(6, links[0].start);
        assert_eq!(6, links[0].end);
        assert_eq!(6, links[1].start);
        assert_eq!(11, links[1].end);
    }

    #[test]
    fn parse_a_link() {
        let input = "Before\x1b]8;;http://example.com\x1b\\Link to example";
        let links = parse_links(input);
        assert_eq!(1, links.len());
        assert_eq!(6, links[0].start);
        assert_eq!(21, links[0].end);
        assert_eq!(links[0].link.uri(), "http://example.com");
    }

    #[test]
    fn render_short_doc() {
        let ctx = create_test_ui("Hi Bye", 3, 2);
        assert_eq!(ctx.surface.screen_chars_to_string(), "Hi \nBye\n");
    }

    #[test]
    fn render_backwards() {
        let input = "1\n2\n3\n4\n5\n6\n";
        let mut ctx = create_test_ui(input, 1, 3);
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());
        ctx.ui
            .queue_event(WidgetEvent::Input(InputEvent::Key(KeyEvent {
                key: KeyCode::Char(' '),
                modifiers: Modifiers::NONE,
            })));
        ctx.ui.process_event_queue().unwrap();
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
        ctx.ui
            .queue_event(WidgetEvent::Input(InputEvent::Key(KeyEvent {
                key: KeyCode::Char('b'),
                modifiers: Modifiers::NONE,
            })));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());
    }
}
