use std::cmp::{max, min};
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

struct Document {
    text: String,
    attrs: Vec<(usize, Change)>,
    links: Vec<LinkRange>,
}

impl Document {
    fn new<'a>(mut input: Box<dyn Read + 'a>) -> Result<Document, Error> {
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
        Ok(Document { text, attrs, links })
    }
}

// Only valid for a particular text width due to reflowing
struct Line {
    start_byte: usize,
    // The changes to render this line assuming the cursor is at the start of it.
    // Starts with the full set of active attributes to let it be used
    // without the Surface containing the active render state
    changes: Vec<Change>,
}

#[derive(Clone, Copy)]
struct Dimensions {
    width: usize,
    height: usize,
}

struct DocumentWidget<'a> {
    doc: Document,
    open_link: Box<dyn FnMut(&str) + 'a>,
    // Used for paging forward and backwards.
    // In reflow, the start_byte of this line is kept in the first_displayed_line of the reflowed
    // lines
    first_displayed_line: usize,
    last_render_size: Option<Dimensions>,
    // The last link we opened
    link_idx: Option<usize>,
    // Cache of doc.text flown at the last_render_size. Will be cleared if the size changes.
    lines: Vec<Line>,
}

impl<'a> DocumentWidget<'a> {
    fn new(doc: Document, open_link: Box<dyn FnMut(&str) + 'a>) -> DocumentWidget<'a> {
        DocumentWidget {
            doc,
            open_link,
            first_displayed_line: 0,
            last_render_size: None,
            link_idx: None,
            lines: vec![],
        }
    }

    fn flow(&mut self) {
        let width = self.width();
        use unicode_segmentation::UnicodeSegmentation;
        let graphemes = self
            .doc
            .text
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));

        self.lines.clear();
        let mut start_byte = 0;
        let mut changes = vec![Change::AllAttributes(CellAttributes::default())];

        let mut attr_idx = 0;
        let mut cells_in_line = 0;
        let mut byte = 0;
        let mut attributes = CellAttributes::default();
        for (grapheme, cells) in graphemes {
            if cells_in_line + cells > width || grapheme == "\n" {
                self.lines.push(Line {
                    start_byte,
                    changes,
                });
                changes = vec![Change::AllAttributes(attributes.clone())];
                start_byte = byte;
                cells_in_line = 0;
            }
            if grapheme != "\n" {
                while attr_idx < self.doc.attrs.len() && byte >= self.doc.attrs[attr_idx].0 {
                    use termwiz::cell::AttributeChange::*;
                    match &self.doc.attrs[attr_idx].1 {
                        Change::Attribute(a) => match a {
                            Intensity(value) => {
                                attributes.set_intensity(*value);
                            }
                            Underline(value) => {
                                attributes.set_underline(*value);
                            }
                            Italic(value) => {
                                attributes.set_italic(*value);
                            }
                            Blink(value) => {
                                attributes.set_blink(*value);
                            }
                            Reverse(value) => {
                                attributes.set_reverse(*value);
                            }
                            StrikeThrough(value) => {
                                attributes.set_strikethrough(*value);
                            }
                            Invisible(value) => {
                                attributes.set_invisible(*value);
                            }
                            Foreground(value) => {
                                attributes.set_foreground(*value);
                            }
                            Background(value) => {
                                attributes.set_background(*value);
                            }
                            Hyperlink(value) => {
                                attributes.set_hyperlink(value.clone());
                            }
                        },
                        Change::AllAttributes(_) => {
                            attributes = CellAttributes::default();
                        }
                        _ => unreachable!(),
                    }
                    changes.push(self.doc.attrs[attr_idx].1.clone());
                    attr_idx += 1;
                }
                changes.push(Change::Text(grapheme.to_string()));
                cells_in_line += cells;
            }
            byte += grapheme.len();
        }
        if changes.len() > 1 {
            self.lines.push(Line {
                start_byte,
                changes,
            });
        }
    }

    fn width(&self) -> usize {
        self.last_render_size
            .expect("Must render before accessing width")
            .width
    }

    fn height(&self) -> usize {
        self.last_render_size
            .expect("Must render before accessing height")
            .height
    }

    fn backward(&mut self, lines: usize) {
        self.first_displayed_line = self.first_displayed_line.saturating_sub(lines);
    }

    fn forward(&mut self, lines: usize) {
        self.first_displayed_line = min(
            self.lines.len().saturating_sub(self.height()),
            self.first_displayed_line + max(1, lines),
        );
    }

    fn process_key(&mut self, event: &KeyEvent) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::UpArrow,
                ..
            } => {
                self.backward(1);
                true
            }
            KeyEvent {
                key: KeyCode::DownArrow,
                ..
            } => {
                self.forward(1);
                true
            }
            KeyEvent {
                key: KeyCode::Char(' '),
                ..
            } => {
                self.forward(self.height() - 2);
                true
            }
            KeyEvent {
                key: KeyCode::Char('b'),
                ..
            } => {
                self.backward(self.height() - 2);
                true
            }
            KeyEvent {
                key: KeyCode::Char('n'),
                ..
            } => {
                if self.doc.links.len() == 0 {
                    return true;
                }
                let link_idx = self.link_idx.unwrap_or(0);
                let x = &self.doc.links[link_idx];
                self.link_idx = Some(link_idx + 1);
                (self.open_link)(x.link.uri());
                for (idx, line) in self.lines.iter().enumerate() {
                    if line.start_byte > x.start {
                        self.first_displayed_line = idx.saturating_sub(1);
                        break;
                    }
                }
                true
            }
            KeyEvent { .. } => false,
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
        self.last_render_size = Some(Dimensions { width, height });
        if self.lines.len() == 0 {
            self.flow();
        }
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
        ];
        let last_displayed_line = min(self.lines.len(), self.first_displayed_line + height) - 1;
        for line in &self.lines[self.first_displayed_line..last_displayed_line] {
            changes.extend_from_slice(&line.changes);
            changes.push(Change::Text("\r\n".to_string()));
        }
        changes.extend_from_slice(&self.lines[last_displayed_line].changes);
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
    fn render_color() {
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

    #[test]
    fn render_short_doc() {
        let ctx = create_test_ui("Hi Bye", 3, 2);
        assert_eq!(ctx.surface.screen_chars_to_string(), "Hi \nBye\n");
    }

    fn press_char_event(c: char) -> WidgetEvent {
        WidgetEvent::Input(InputEvent::Key(KeyEvent {
            key: KeyCode::Char(c),
            modifiers: Modifiers::NONE,
        }))
    }

    #[test]
    fn page() {
        let input = "1\n2\n3\n4\n5\n6\n";
        let mut ctx = create_test_ui(input, 1, 5);
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());

        ctx.ui.queue_event(press_char_event(' '));
        // Going forward while at the last screen shouldn't keep the whole screen
        ctx.ui.queue_event(press_char_event(' '));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("2", cells[0].str());

        ctx.ui.queue_event(press_char_event('b'));
        // Going back while at the first line should stay at the first line
        ctx.ui.queue_event(press_char_event('b'));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());
    }
    #[test]
    fn visit_link() {
        let input = "Before\n\x1b]8;;http://a.b\x1b\\Linked\x1b]8;;\x1b\\\nNot Linked";
        let mut ctx = create_test_ui(input, 10, 3);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("B", cells[0].str());
        ctx.ui.queue_event(press_char_event('n'));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("L", cells[0].str());
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
}
