use std::cell::Cell;
use std::cmp::{max, min};
use std::io::{stdin, Read};
use std::process::Command;
use std::rc::Rc;

use crate::doc::Document;
use termwiz::caps::Capabilities;
use termwiz::cell::{grapheme_column_width, unicode_column_width, AttributeChange, CellAttributes};
use termwiz::color::{AnsiColor, ColorAttribute};
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::CursorShape;
use termwiz::surface::{Change, Position::Absolute};

use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::{new_terminal, Terminal};
use termwiz::widgets::layout::{ChildOrientation, Constraints};
use termwiz::widgets::{
    ParentRelativeCoords, RenderArgs, Ui, UpdateArgs, Widget, WidgetEvent, WidgetId,
};
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
    ctx: RefCtx,
    open_link: Box<dyn FnMut(&str) + 'a>,
    last_render_size: Option<Dimensions>,
    // The last link we opened
    link_idx: Option<usize>,
    // Cache of doc.text flown at the last_render_size. Will be cleared if the size changes.
    lines: Vec<Line>,
    // Reverses the reverse display of bytes in these ranges.
    // If reverse is off for a byte, flips it on and vice versa.
    highlights: Vec<(usize, usize)>,
    // First displayed line
    // Used for paging forward and backwards.
    // In reflow, the start_byte of this line is kept in the first_displayed_line of the reflowed
    // lines
    line: usize,
    // Total lines to display if the length of the file is known
    total_lines: Option<usize>,
    // Lines shown in a single page
    page_lines: Option<usize>,
}

impl<'a> DocumentWidget<'a> {
    fn new(ctx: RefCtx, open_link: Box<dyn FnMut(&str) + 'a>) -> DocumentWidget<'a> {
        DocumentWidget {
            open_link,
            ctx,
            line: 0,
            total_lines: None,
            page_lines: None,
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

        let doc = &self.ctx.doc;
        let width = self.width();
        let mut byte = 0;
        use unicode_segmentation::UnicodeSegmentation;
        let graphemes = doc
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
                while attr_idx < doc.attrs.len() && byte >= doc.attrs[attr_idx].0 {
                    match &doc.attrs[attr_idx].1 {
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
        self.total_lines = Some(self.lines.len())
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
        self.set_line(self.line.saturating_sub(lines));
    }

    fn forward(&mut self, lines: usize) {
        self.set_line(min(
            self.lines.len().saturating_sub(self.height()),
            self.line + max(1, lines),
        ));
    }

    fn set_line(&mut self, line: usize) {
        self.line = line;
        self.ctx.percent.set(self.percent());
    }

    fn percent(&self) -> Option<u8> {
        if self.line == 0 {
            return Some(0);
        }
        let total_lines = match self.total_lines {
            None => {
                return None;
            }
            Some(tl) => tl,
        };
        let final_page_line = total_lines - self.page_lines.unwrap_or(0);
        if final_page_line == self.line {
            Some(100)
        } else {
            let percent = (self.line as f64 / (final_page_line as f64)) * 100.0;
            Some(percent.floor() as u8)
        }
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
                if self.ctx.doc.links.len() == 0 {
                    return true;
                }
                let link_idx = self.link_idx.unwrap_or(0);
                let x = &self.ctx.doc.links[link_idx];
                self.highlights = vec![(x.start, x.end)];
                self.link_idx = Some(link_idx + 1);
                (self.open_link)(x.link.uri());
                for (idx, line) in self.lines.iter().enumerate() {
                    if line.start_byte > x.start {
                        self.set_line(idx.saturating_sub(1));
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
        self.page_lines = Some(height);
        self.ctx.percent.set(self.percent());

        let mut line = self.line;
        let first_line = &self.lines[line];
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
        let doc = &self.ctx.doc;
        let graphemes = doc.text[byte..]
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));
        let mut attr_idx = doc.attrs.partition_point(|(b, _)| *b < byte);
        let mut highlight_idx = self.highlights.partition_point(|(_, e)| *e <= byte);
        let mut highlight: Option<(usize, usize)> = None;
        // Tracks the inverse sgr state for byte.
        // We switch it when in a highlight and then go back to the set state when exiting
        // the highlight
        let mut reversed = first_line.start_attributes.reverse();
        let mut cells_in_line = 0;
        let last_displayed_line = min(self.lines.len(), line + height) - 1;
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
                while attr_idx < doc.attrs.len() && byte >= doc.attrs[attr_idx].0 {
                    let mut change = doc.attrs[attr_idx].1.clone();
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

    fn get_size_constraints(&self) -> Constraints {
        let mut c = Constraints::default();
        c.set_fixed_height(self.ctx.doc_height());
        c
    }
}

// This is a little status line widget that we render at the bottom
struct StatusLine {
    ctx: RefCtx,
}

impl Widget for StatusLine {
    fn render(&mut self, args: &mut RenderArgs) {
        args.surface
            .add_change(Change::ClearScreen(AnsiColor::Grey.into()));
        args.surface.add_change(Change::CursorPosition {
            x: Absolute(0),
            y: Absolute(0),
        });
        let doc_name = &self.ctx.doc_name;
        let doc_name_width = unicode_column_width(&doc_name, None);
        args.surface.add_change(Change::Text(doc_name.to_string()));
        let progress = match self.ctx.percent.get() {
            Some(p) => format!("{}%", p),
            None => "?%".to_string(),
        };
        let progress_width = unicode_column_width(&progress, None);
        let surface_width = args.surface.dimensions().0;
        if surface_width.saturating_sub(doc_name_width + progress_width) < 1 {
            return;
        }
        args.surface.add_change(Change::CursorPosition {
            x: Absolute(surface_width - progress_width),
            y: Absolute(0),
        });
        args.surface.add_change(Change::Text(progress));
    }

    fn get_size_constraints(&self) -> Constraints {
        let mut c = Constraints::default();
        c.set_fixed_height(1);
        c
    }
}

struct Search {
    ctx: RefCtx,
    search: String,
}

impl Search {
    fn process_key(&mut self, event: &KeyEvent) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::Char('/'),
                ..
            } => {
                self.ctx.deactivate_search();
                true
            }
            KeyEvent {
                key: KeyCode::Char(c),
                ..
            } => {
                self.search.push(*c);
                true
            }
            KeyEvent {
                key: KeyCode::Backspace,
                ..
            } => {
                self.search.pop();
                true
            }
            KeyEvent {
                key: KeyCode::DownArrow,
                ..
            } => true,
            _ => false,
        }
    }
}

impl Widget for Search {
    fn render(&mut self, args: &mut RenderArgs) {
        let (width, height) = args.surface.dimensions();
        if height == 0 {
            return;
        }
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
            Change::Text("-".repeat(width)),
        ];
        for link in &self.ctx.doc.links[..height - 2] {
            changes.push(Change::Text(format!("{}\r\n", link.link.uri())));
        }
        let search_label = format!("Search: {}", self.search);
        args.cursor.coords = ParentRelativeCoords {
            x: search_label.len(),
            y: height - 1,
        };
        args.cursor.shape = CursorShape::BlinkingBar;
        changes.extend(vec![
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(height - 1),
            },
            Change::Text(search_label),
        ]);
        args.surface.add_changes(changes);
    }

    fn get_size_constraints(&self) -> Constraints {
        let mut c = Constraints::default();
        c.set_fixed_height(self.ctx.search_height());
        c
    }

    fn process_event(&mut self, event: &WidgetEvent, _args: &mut UpdateArgs) -> bool {
        match event {
            WidgetEvent::Input(i) => match i {
                InputEvent::Key(k) => self.process_key(k),
                InputEvent::Paste(s) => {
                    self.search.push_str(&s);
                    true
                }
                _ => false,
            },
        }
    }
}

/// This is the main container widget for the app
struct MainScreen {
    ctx: RefCtx,
}

impl MainScreen {
    fn process_key(&mut self, event: &KeyEvent) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::Char('/'),
                ..
            } => {
                self.ctx.activate_search();
                true
            }
            _ => false,
        }
    }
}

impl Widget for MainScreen {
    fn render(&mut self, _args: &mut RenderArgs) {}

    fn get_size_constraints(&self) -> Constraints {
        // Switch from default horizontal layout to vertical layout
        let mut c = Constraints::default();
        c.child_orientation = ChildOrientation::Vertical;
        c
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

struct Ctx {
    doc: Document,
    doc_name: String,
    search: Cell<WidgetId>,
    doc_widget: Cell<WidgetId>,

    percent: Cell<Option<u8>>,
    focus: Cell<WidgetId>,
    term_dims: Cell<Dimensions>,
}

impl Ctx {
    fn search_visible(&self) -> bool {
        self.focus.get() == self.search.get()
    }

    fn activate_search(&self) {
        self.focus.set(self.search.get());
    }

    fn deactivate_search(&self) {
        self.focus.set(self.doc_widget.get());
    }

    fn all_but_status_height(&self) -> u16 {
        self.term_dims.get().height as u16 - 1
    }

    fn half_height(&self) -> u16 {
        self.all_but_status_height() / 2
    }

    fn search_height(&self) -> u16 {
        if self.search_visible() {
            self.half_height()
        } else {
            0
        }
    }

    fn doc_height(&self) -> u16 {
        if self.search_visible() {
            self.half_height() + (self.all_but_status_height() % 2)
        } else {
            self.all_but_status_height()
        }
    }
}

type RefCtx = Rc<Ctx>;

struct Ate<'a, T: Terminal> {
    ctx: RefCtx,
    term: BufferedTerminal<T>,
    ui: Ui<'a>,
}

impl<'a, T: Terminal> Ate<'a, T> {
    fn run(&mut self) -> Result<(), Error> {
        loop {
            let size = self.term.terminal().get_screen_size()?;
            self.ctx.term_dims.set(Dimensions {
                width: size.cols,
                height: size.rows,
            });
            self.ui.process_event_queue()?;
            self.ui.set_focus(self.ctx.focus.get());

            // After updating and processing all of the widgets, compose them
            // and render them to the screen.
            if self.ui.render_to_screen(&mut self.term)? {
                // We have more events to process immediately; don't block waiting
                // for input below, but jump to the top of the loop to re-run the
                // updates.
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
                        self.ctx.term_dims.set(Dimensions {
                            width: cols,
                            height: rows,
                        });
                        self.ui.queue_event(WidgetEvent::Input(input));
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
                        self.ui.queue_event(WidgetEvent::Input(input));
                    }
                },
                Ok(None) => {}
                Err(e) => {
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}

fn create_ui<'a>(
    input: Box<dyn Read + 'a>,
    doc_name: String,
    width: usize,
    height: usize,
    open_link: Box<dyn FnMut(&str) + 'a>,
) -> Result<(Ui<'a>, RefCtx), Error> {
    let doc = Document::new(input)?;
    let placeholder_id = Cell::new(WidgetId::new());
    let ctx = Rc::new(Ctx {
        doc,
        doc_name,
        percent: Cell::new(None),
        search: placeholder_id.clone(),
        doc_widget: placeholder_id.clone(),
        focus: Cell::new(WidgetId::new()),
        term_dims: Cell::new(Dimensions { width, height }),
    });

    let mut ui = Ui::new();
    let root_id = ui.set_root(MainScreen { ctx: ctx.clone() });
    let doc_id = ui.add_child(root_id, DocumentWidget::new(ctx.clone(), open_link));
    ctx.doc_widget.set(doc_id);
    ctx.focus.set(doc_id);
    ui.set_focus(doc_id);

    ctx.search.set(ui.add_child(
        root_id,
        Search {
            ctx: ctx.clone(),
            search: String::new(),
        },
    ));
    ui.add_child(root_id, StatusLine { ctx: ctx.clone() });

    // Send a resize event through to get us to do an initial layout
    ui.queue_event(WidgetEvent::Input(InputEvent::Resized {
        cols: width,
        rows: height,
    }));
    ui.process_event_queue()?;
    Ok((ui, ctx))
}

fn main() -> Result<(), Error> {
    let caps = Capabilities::new_from_env()?;
    let underlying_term = new_terminal(caps)?;
    let mut term = BufferedTerminal::new(underlying_term)?;
    term.terminal().set_raw_mode()?;
    term.terminal().enter_alternate_screen()?;
    let size = term.terminal().get_screen_size()?;

    let (ui, ctx) = create_ui(
        Box::new(stdin()),
        "stdin".to_string(),
        size.cols,
        size.rows,
        Box::new(|uri| {
            let output = Command::new(",edit").arg(uri).output().unwrap();
            println!("{}", String::from_utf8(output.stdout).unwrap());
        }),
    )?;

    Ate { ctx, term, ui }.run()?;
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
        let (mut ui, _) = create_ui(
            Box::new(Cursor::new(input.to_string())),
            "tst".to_string(),
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
        let mut ctx = create_test_ui(input, 3, 2);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!(ColorAttribute::Default, cells[0].attrs().foreground());
        assert_eq!(
            ColorAttribute::PaletteIndex(1),
            cells[1].attrs().foreground()
        );
        assert_eq!(ColorAttribute::Default, cells[2].attrs().foreground());
        assert_eq!(ctx.surface.screen_chars_to_string(), "DRD\ntst\n");
    }

    #[test]
    fn render_short_doc() {
        let ctx = create_test_ui("Hi Bye", 3, 3);
        assert_eq!(ctx.surface.screen_chars_to_string(), "Hi \nBye\ntst\n");
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
        let mut ctx = create_test_ui(input, 3, 6);
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());

        ctx.ui.queue_event(press_char_event(' '));
        // Going forward while at the last screen shouldn't keep the whole screen
        ctx.ui.queue_event(press_char_event(' '));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let screen = ctx.surface.screen_chars_to_string();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("2", cells[0].str(), "{}", screen);

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
        let mut ctx = create_test_ui(input, 10, 4);
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
