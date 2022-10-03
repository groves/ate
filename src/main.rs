use std::cell::{Cell, RefCell};
use std::cmp::{max, min};
use std::io::{stdin, Read};
use std::process::Command;
use std::rc::{Rc, Weak};

use crate::doc::Document;
use doc::LinkRange;
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

#[derive(Clone)]
struct DocumentView {
    // Reverses the reverse display of bytes in these ranges.
    // If reverse is off for a byte, flips it on and vice versa.
    highlights: Vec<(usize, usize)>,
    // First displayed line
    // Used for paging forward and backwards.
    // In reflow, the start_byte of this line is kept in the first_displayed_line of the reflowed
    // lines
    line: usize,
    total_lines: usize,
    page_height: usize,
}

impl DocumentView {
    fn backward(&mut self, lines: usize) {
        self.line = self.line.saturating_sub(lines);
    }

    fn forward(&mut self, lines: usize) {
        self.line = min(
            self.total_lines.saturating_sub(self.page_height),
            self.line + max(1, lines),
        );
    }

    fn set_line(&mut self, line: usize) {
        self.line = line;
    }

    fn percent(&self) -> Option<u8> {
        if self.line == 0 || self.total_lines < self.page_height {
            return Some(0);
        }
        let final_page_line = self.total_lines - self.page_height;
        if final_page_line == self.line {
            Some(100)
        } else {
            let percent = (self.line as f64 / (final_page_line as f64)) * 100.0;
            Some(percent.floor() as u8)
        }
    }
}

struct DocumentFlow<'a> {
    ctx: Weak<Ctx<'a>>,
    width: usize,
    // Cache of doc.text flown at the last_render_size. Will be cleared if the size changes.
    // We cache the start point of the line so that when we go backwards, we can rerender without
    // having to start from the start of the document to find where long lines break.
    lines: Vec<Line>,
}

impl<'a> DocumentFlow<'a> {
    fn new(width: usize) -> DocumentFlow<'a> {
        DocumentFlow {
            width,
            ctx: Weak::new(),
            lines: vec![],
        }
    }

    fn set_width(&mut self, width: usize) -> bool {
        if width == self.width {
            return false;
        }
        // TODO - update ctx.view.line to keep current view position
        self.width = width;
        self.flow();
        true
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

    fn ctx(&self) -> Rc<Ctx<'a>> {
        self.ctx
            .upgrade()
            .expect("Ctx shouldn't go away since it owns flow")
    }

    fn flow(&mut self) {
        // TODO - Only flow the lines necessary to render the screen.
        // Read from the underlying stream if at the point of flowing.
        self.lines.clear();

        let doc = &self.ctx().doc;
        let width = self.width;
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
        self.ctx().view.borrow_mut().total_lines = self.lines.len();
    }
}

fn render_lines(
    doc: &Document,
    lines: &[Line],
    mut byte: usize,
    width: usize,
    height: usize,
    highlights: &[(usize, usize)],
    changes: &mut Vec<Change>,
) {
    use unicode_segmentation::UnicodeSegmentation;
    let graphemes = doc.text[byte..]
        .graphemes(true)
        .map(|g| (g, grapheme_column_width(g, None)));
    let mut line = lines.partition_point(|l| l.start_byte < byte);
    let mut attr_idx = doc.attrs.partition_point(|(b, _)| *b < byte);
    let mut highlight_idx = highlights.partition_point(|(_, e)| *e <= byte);
    let mut highlight: Option<(usize, usize)> = None;
    // Tracks the inverse sgr state for byte.
    // We switch it when in a highlight and then go back to the set state when exiting
    // the highlight
    let mut reversed = lines[line].start_attributes.reverse();
    let mut cells_in_line = 0;
    let last_displayed_line = min(lines.len(), line + height) - 1;
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
            } else if highlight_idx < highlights.len() && highlights[highlight_idx].0 <= byte {
                highlight = Some(highlights[highlight_idx]);
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
                    reversed = attr.reverse();
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
}

struct DocumentWidget<'a> {
    ctx: RefCtx<'a>,
    last_render_height: Option<usize>,
}

impl<'a> DocumentWidget<'a> {
    fn height(&self) -> usize {
        self.last_render_height
            .expect("Must render before accessing height")
    }

    fn process_key(&mut self, event: &KeyEvent) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::UpArrow,
                ..
            } => {
                self.ctx.view.borrow_mut().backward(1);
                true
            }
            KeyEvent {
                key: KeyCode::DownArrow,
                ..
            } => {
                self.ctx.view.borrow_mut().forward(1);
                true
            }
            KeyEvent {
                key: KeyCode::Char(' '),
                ..
            } => {
                self.ctx.view.borrow_mut().forward(self.height() - 2);
                true
            }
            KeyEvent {
                key: KeyCode::Char('b'),
                ..
            } => {
                self.ctx.view.borrow_mut().backward(self.height() - 2);
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
        self.last_render_height = Some(height);

        self.ctx.flow.borrow_mut().set_width(width);
        self.ctx.view.borrow_mut().page_height = height;

        let first_line = &self.ctx.flow.borrow().lines[self.ctx.view.borrow().line];
        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
            Change::AllAttributes(first_line.start_attributes.clone()),
        ];
        render_lines(
            &self.ctx.doc,
            &self.ctx.flow.borrow().lines,
            first_line.start_byte,
            width,
            height,
            &self.ctx.view.borrow().highlights,
            &mut changes,
        );
        // TODO - add visibility setting to cursor in termwiz.
        // This is the best I can do to make it less visible now.
        // Changing the shape doesn't seem to work, either :(
        args.cursor.coords = ParentRelativeCoords {
            x: width + 1,
            y: height - 1,
        };

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
struct StatusLine<'a> {
    ctx: RefCtx<'a>,
}

impl<'a> Widget for StatusLine<'a> {
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
        let progress = match self.ctx.view.borrow().percent() {
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

struct Search<'a> {
    ctx: Weak<Ctx<'a>>,
    search: String,
    selected_idx: Option<usize>,
    open_link: Box<dyn FnMut(&str) + 'a>,
    view_at_activation: Option<DocumentView>,
    matches: Vec<usize>,
    links: Vec<LinkRange>,
}

impl<'a> Search<'a> {
    fn new(open_link: Box<dyn FnMut(&str) + 'a>) -> Search<'a> {
        Search {
            open_link,
            ctx: Weak::new(),
            search: String::new(),
            selected_idx: None,
            view_at_activation: None,
            matches: vec![],
            links: vec![],
        }
    }

    fn set_ctx(&mut self, ctx: &Rc<Ctx<'a>>) {
        self.ctx = Rc::downgrade(ctx);
        self.links = self.ctx().doc.links.to_vec();
        self.matches = self.links.iter().enumerate().map(|(i, _)| i).collect();
    }

    fn ctx(&self) -> Rc<Ctx<'a>> {
        self.ctx
            .upgrade()
            .expect("Ctx shouldn't go away since it owns flow")
    }

    fn activate(&mut self) {
        self.view_at_activation = Some(self.ctx().view.borrow().clone());
        self.set_selected_idx(self.selected_idx.unwrap_or(0));
    }

    fn set_selected_idx(&mut self, selected_idx: usize) {
        if selected_idx >= self.matches.len() {
            return;
        }
        self.selected_idx = Some(selected_idx);
        let link = &self.links[self.matches[selected_idx]];
        self.ctx().highlight(link.start, link.end);
    }

    fn open_selected(&mut self) {
        if self.matches.len() == 0 {
            return;
        }
        let selected_idx = match self.selected_idx {
            Some(idx) => idx,
            None => {
                self.set_selected_idx(0);
                0
            }
        };
        let link = &self.links[self.matches[selected_idx]];
        let addr = link.link.uri();
        (self.open_link)(addr);
    }

    fn select_next(&mut self) {
        self.set_selected_idx(if let Some(idx) = self.selected_idx {
            idx + 1
        } else {
            0
        });
    }

    fn select_prev(&mut self) {
        self.set_selected_idx(if let Some(idx) = self.selected_idx {
            idx.saturating_sub(1)
        } else {
            0
        });
    }

    fn update_matches(&mut self) {
        let previous_link_idx = if self.matches.len() > 0 {
            self.matches[self.selected_idx.unwrap_or(0)]
        } else {
            0
        };
        self.matches = self
            .links
            .iter()
            .enumerate()
            .filter(|(_, l)| self.ctx().doc.text[l.start..l.end].contains(&self.search))
            .map(|(i, _)| i)
            .collect();
        let mut new_selected_idx = self
            .matches
            .partition_point(|link_idx| link_idx < &previous_link_idx);
        if new_selected_idx == self.matches.len() {
            new_selected_idx = self.matches.len().saturating_sub(1);
        }
        self.set_selected_idx(new_selected_idx);
    }

    fn process_key(&mut self, event: &KeyEvent) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::Enter,
                ..
            } => {
                self.ctx().deactivate_search();
                true
            }
            KeyEvent {
                key: KeyCode::Escape,
                ..
            } => {
                self.ctx().deactivate_search();
                self.ctx()
                    .view
                    .replace(self.view_at_activation.take().unwrap());
                true
            }
            KeyEvent {
                key: KeyCode::Char(c),
                ..
            } => {
                self.search.push(*c);
                self.update_matches();
                true
            }
            KeyEvent {
                key: KeyCode::Backspace,
                ..
            } => {
                self.search.pop();
                self.update_matches();
                true
            }
            KeyEvent {
                key: KeyCode::UpArrow,
                ..
            } => {
                self.select_prev();
                true
            }
            KeyEvent {
                key: KeyCode::DownArrow,
                ..
            } => {
                self.select_next();
                true
            }
            _ => false,
        }
    }

    fn render(&mut self, width: usize, height: usize, changes: &mut Vec<Change>) {
        if self.matches.len() == 0 {
            return;
        }
        let selected_idx = self.selected_idx.unwrap_or(0);
        let first_visible_idx = selected_idx.saturating_sub(height);
        let selected = &self.links[self.matches[selected_idx]];
        let highlights = vec![(selected.start, selected.end)];
        for i in first_visible_idx..(first_visible_idx + height) {
            if i >= self.matches.len() {
                break;
            }
            changes.push(Change::Attribute(AttributeChange::Reverse(
                i == selected_idx,
            )));
            render_lines(
                &self.ctx().doc,
                &self.ctx().flow.borrow().lines,
                self.links[self.matches[i]].start,
                width,
                1,
                &highlights,
                changes,
            );
            changes.push(Change::Text("\r\n".to_string()));
        }
    }
}

struct SearchWidget<'a> {
    ctx: RefCtx<'a>,
}

impl<'a> Widget for SearchWidget<'a> {
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
        ];
        if height > 1 {
            changes.push(Change::Text(format!("{}\r\n", "-".repeat(width))));
            self.ctx
                .search
                .borrow_mut()
                .render(width, height - 2, &mut changes);
        }
        let search_label = format!("Search: {}", self.ctx.search.borrow().search);
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
                InputEvent::Key(k) => self.ctx.search.borrow_mut().process_key(k),
                InputEvent::Paste(s) => {
                    self.ctx.search.borrow_mut().search.push_str(&s);
                    true
                }
                _ => false,
            },
        }
    }
}

/// This is the main container widget for the app
struct MainScreen<'a> {
    ctx: RefCtx<'a>,
}

impl<'a> MainScreen<'a> {
    fn process_key(&mut self, event: &KeyEvent) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::Char('/'),
                ..
            } => {
                self.ctx.activate_search();
                true
            }
            KeyEvent {
                key: KeyCode::Char('N'),
                ..
            } => {
                self.ctx.search.borrow_mut().select_prev();
                true
            }
            KeyEvent {
                key: KeyCode::Char('n'),
                ..
            } => {
                self.ctx.search.borrow_mut().select_next();
                true
            }
            KeyEvent {
                key: KeyCode::Enter,
                ..
            } => {
                self.ctx.search.borrow_mut().open_selected();
                true
            }
            _ => false,
        }
    }
}

impl<'a> Widget for MainScreen<'a> {
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

struct Ctx<'a> {
    doc: Document,
    doc_name: String,
    search_id: Cell<WidgetId>,
    doc_widget: Cell<WidgetId>,
    flow: RefCell<DocumentFlow<'a>>,
    view: RefCell<DocumentView>,
    search: RefCell<Search<'a>>,

    focus: Cell<WidgetId>,
    term_height: Cell<usize>,
}

impl<'a> Ctx<'a> {
    fn highlight(&self, start: usize, end: usize) {
        self.view.borrow_mut().highlights = vec![(start, end)];
        for (idx, line) in self.flow.borrow().lines.iter().enumerate() {
            if line.start_byte > start {
                self.view.borrow_mut().set_line(idx.saturating_sub(3));
                break;
            }
        }
    }

    fn search_visible(&self) -> bool {
        self.focus.get() == self.search_id.get()
    }

    fn activate_search(&self) {
        self.focus.set(self.search_id.get());
        self.search.borrow_mut().activate();
    }

    fn deactivate_search(&self) {
        self.focus.set(self.doc_widget.get());
    }

    fn all_but_status_height(&self) -> u16 {
        self.term_height.get() as u16 - 1
    }

    fn search_height(&self) -> u16 {
        if self.search_visible() {
            let chrome_height = 2;
            // Show at most 10 matches
            (10 + chrome_height)
                // but show fewer if that would take more than half the screen
                .min(self.all_but_status_height() / 2)
                // and only take up as many lines as we have matches
                .min(self.search.borrow().matches.len() as u16 + chrome_height)
                // but at least take 1 for search entry if that's all there is
                .max(1)
        } else {
            0
        }
    }

    fn doc_height(&self) -> u16 {
        self.all_but_status_height() - self.search_height()
    }
}

type RefCtx<'a> = Rc<Ctx<'a>>;

struct Ate<'a, T: Terminal> {
    ctx: RefCtx<'a>,
    term: BufferedTerminal<T>,
    ui: Ui<'a>,
}

impl<'a, T: Terminal> Ate<'a, T> {
    fn run(&mut self) -> Result<(), Error> {
        loop {
            let size = self.term.terminal().get_screen_size()?;
            self.ctx.term_height.set(size.rows);
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
                        self.ctx.term_height.set(rows);
                        self.ui.queue_event(WidgetEvent::Input(input));
                    }
                    InputEvent::Key(KeyEvent {
                        key: KeyCode::Char('q'),
                        ..
                    }) => {
                        // Quit the app when q is pressed
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
) -> Result<(Ui<'a>, RefCtx<'a>), Error> {
    let doc = Document::new(input)?;
    let placeholder_id = Cell::new(WidgetId::new());
    let ctx = Rc::new(Ctx {
        doc,
        doc_name,
        search_id: placeholder_id.clone(),
        doc_widget: placeholder_id.clone(),
        focus: Cell::new(WidgetId::new()),
        term_height: Cell::new(height),
        view: RefCell::new(DocumentView {
            highlights: vec![],
            line: 0,
            page_height: 0,
            total_lines: 0,
        }),
        flow: RefCell::new(DocumentFlow::new(width)),
        search: RefCell::new(Search::new(open_link)),
    });
    ctx.flow.borrow_mut().ctx = Rc::downgrade(&ctx);
    ctx.flow.borrow_mut().flow();
    ctx.search.borrow_mut().set_ctx(&ctx);

    let mut ui = Ui::new();
    let root_id = ui.set_root(MainScreen { ctx: ctx.clone() });
    let doc_id = ui.add_child(
        root_id,
        DocumentWidget {
            ctx: ctx.clone(),
            last_render_height: None,
        },
    );
    ctx.doc_widget.set(doc_id);
    ctx.focus.set(doc_id);
    ui.set_focus(doc_id);

    ctx.search_id
        .set(ui.add_child(root_id, SearchWidget { ctx: ctx.clone() }));
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
        let cells = &ctx.surface.screen_cells()[1];
        assert_eq!("L", cells[0].str());
        assert!(!cells[0].attrs().reverse());
        assert!(!cells[1].attrs().reverse());
        assert_eq!(0, ctx.visited.borrow().len());

        ctx.ui
            .queue_event(WidgetEvent::Input(InputEvent::Key(KeyEvent {
                key: KeyCode::Enter,
                modifiers: Modifiers::NONE,
            })));
        ctx.ui.process_event_queue().unwrap();
        ctx.ui.render_to_screen(&mut ctx.surface).unwrap();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("B", cells[0].str());
        let cells = &ctx.surface.screen_cells()[1];
        assert_eq!("L", cells[0].str());
        assert!(cells[0].attrs().reverse());
        assert!(cells[1].attrs().reverse());
        assert_eq!(vec!["http://a.b".to_string()], *ctx.visited.borrow());
    }
}
