use std::cmp::{max, min};
use std::io::{stdin, Read};
use std::process::Command;

use crate::doc::Document;
use termwiz::caps::Capabilities;
use termwiz::cell::{grapheme_column_width, AttributeChange, CellAttributes};
use termwiz::color::ColorAttribute;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::{Change, Position::Absolute};

use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::{new_terminal, Terminal};
use termwiz::widgets::{RenderArgs, Ui, UpdateArgs, Widget, WidgetEvent};
use termwiz::Error;
mod doc;

// Only valid for a particular text width due to reflowing
struct Line {
    start_byte: usize,
    // The full set of active attributes to let set up this line for rendering.
    start_attributes: CellAttributes,
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
    // Reverses the reverse display of bytes in these ranges.
    // If reverse is off for a byte, flips it on and vice versa.
    highlights: Vec<(usize, usize)>,
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
            highlights: vec![],
        }
    }

    fn apply(attributes: &mut CellAttributes, a: &AttributeChange) {
        use termwiz::cell::AttributeChange::*;
        match a {
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
        };
    }

    fn flow(&mut self) {
        self.lines.clear();

        let width = self.width();
        let mut byte = 0;
        use unicode_segmentation::UnicodeSegmentation;
        let graphemes = self
            .doc
            .text
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));
        let mut attr_idx = 0;
        let mut cells_in_line = 0;
        let mut attributes = CellAttributes::default();
        self.lines.push(Line {
            start_byte: byte,
            start_attributes: attributes.clone(),
        });
        for (grapheme, cells) in graphemes {
            if cells_in_line + cells > width || grapheme == "\n" {
                self.lines.push(Line {
                    start_byte: if grapheme == "\n" { byte + 1 } else { byte },
                    start_attributes: attributes.clone(),
                });
                cells_in_line = 0;
            }
            if grapheme != "\n" {
                while attr_idx < self.doc.attrs.len() && byte >= self.doc.attrs[attr_idx].0 {
                    match &self.doc.attrs[attr_idx].1 {
                        Change::AllAttributes(_) => {
                            attributes = CellAttributes::default();
                        }
                        Change::Attribute(a) => {
                            Self::apply(&mut attributes, a);
                        }
                        _ => unreachable!(),
                    }
                    attr_idx += 1;
                }
                cells_in_line += cells;
            }
            byte += grapheme.len();
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
                self.highlights = vec![(x.start, x.end)];
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
    fn render(&mut self, args: &mut RenderArgs) {
        let (width, height) = args.surface.dimensions();
        assert!(width > 0);
        assert!(height > 0);
        self.last_render_size = Some(Dimensions { width, height });
        if self.lines.len() == 0 {
            // TODO - Only flow the lines necessary to render this screen.
            // Read from the underlying stream if at the point of flowing.
            self.flow();
        }
        let first_line = &self.lines[self.first_displayed_line];
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
            Change::AllAttributes(first_line.start_attributes.clone()),
        ];

        let width = self.width();
        let mut byte = first_line.start_byte;
        use unicode_segmentation::UnicodeSegmentation;
        let graphemes = self.doc.text[byte..]
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));
        let mut attr_idx = self.doc.attrs.partition_point(|(b, _)| *b < byte);
        let mut highlight_idx = self.highlights.partition_point(|(_, e)| *e <= byte);
        let mut highlight: Option<(usize, usize)> = None;
        // Tracks the inverse sgr state for byte.
        // We switch it when in a highlight and then go back to the set state when exiting
        // the highlight
        let mut reversed = first_line.start_attributes.reverse();
        let mut cells_in_line = 0;
        let mut line = self.first_displayed_line;
        let last_displayed_line = min(self.lines.len(), self.first_displayed_line + height) - 1;
        for (grapheme, cells) in graphemes {
            if cells_in_line + cells > width || grapheme == "\n" {
                if line == last_displayed_line {
                    break;
                }
                changes.push(Change::Text("\r\n".to_string()));
                line += 1;
                cells_in_line = 0;
            }
            if grapheme != "\n" {
                if let Some(active_highlight) = highlight {
                    if active_highlight.1 <= byte {
                        highlight = None;
                        changes.push(Change::Attribute(AttributeChange::Reverse(reversed)));
                        highlight_idx += 1;
                    }
                } else if highlight_idx < self.highlights.len()
                    && self.highlights[highlight_idx].0 <= byte
                {
                    highlight = Some(self.highlights[highlight_idx]);
                    changes.push(Change::Attribute(AttributeChange::Reverse(!reversed)));
                }
                while attr_idx < self.doc.attrs.len() && byte >= self.doc.attrs[attr_idx].0 {
                    let mut change = self.doc.attrs[attr_idx].1.clone();
                    attr_idx += 1;
                    if let Change::Attribute(AttributeChange::Reverse(new_reverse)) = change {
                        reversed = new_reverse;
                        if highlight.is_some() {
                            change = Change::Attribute(AttributeChange::Reverse(!new_reverse));
                        }
                    }
                    if let Change::AllAttributes(attr) = &mut change {
                        reversed = false;
                        if highlight.is_some() {
                            attr.set_reverse(!attr.reverse());
                        }
                    }
                    changes.push(change);
                }
                changes.push(Change::Text(grapheme.to_string()));
                cells_in_line += cells;
            }
            byte += grapheme.len();
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
    use std::{cell::RefCell, io::Cursor, rc::Rc};

    use termwiz::{color::ColorAttribute, input::Modifiers, surface::Surface};

    use super::*;

    struct Context<'a> {
        ui: Ui<'a>,
        surface: Surface,
        visited: Rc<RefCell<Vec<String>>>,
    }

    fn create_test_ui(input: &str, width: usize, height: usize) -> Context {
        let visited = Rc::new(RefCell::new(vec![]));
        let ctx_visited = visited.clone();
        let mut ui = create_ui(
            Box::new(Cursor::new(input.to_string())),
            width,
            height,
            Box::new(move |uri| {
                visited.borrow_mut().push(uri.to_string());
            }),
        )
        .unwrap();
        let mut surface = Surface::new(width, height);
        // Render twice to test if we're stepping on ourselves
        ui.render_to_screen(&mut surface).unwrap();
        ui.render_to_screen(&mut surface).unwrap();
        Context {
            ui,
            visited: ctx_visited,
            surface,
        }
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
        let input = "1\n2\n3\n4\n5\n6";
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
        let input =
            "Before\n\x1b]8;;http://a.b\x1b\\\x1b[31mL\x1b[m\x1binked\x1b]8;;\x1b\\\nNot Linked";
        let mut ctx = create_test_ui(input, 10, 3);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("B", cells[0].str());
        assert_eq!(0, ctx.visited.borrow().len());
        ctx.ui.queue_event(press_char_event('n'));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("L", cells[0].str());
        assert!(cells[0].attrs().reverse());
        assert!(cells[1].attrs().reverse());
        assert_eq!(vec!["http://a.b".to_string()], *ctx.visited.borrow());
    }
}
