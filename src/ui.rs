use crate::doc::Document;
use crate::state::{DocumentView, Shared, State};
use anyhow::Result;
use finl_unicode::grapheme_clusters::Graphemes;
use log::warn;
use std::cell::RefCell;
use std::cmp::min;
use std::io::Read;
use std::rc::Rc;
use termwiz::cell::{grapheme_column_width, unicode_column_width, AttributeChange};
use termwiz::color::{AnsiColor, ColorAttribute};
use termwiz::input::Modifiers;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::{Change, Position::Absolute};
use termwiz::surface::{CursorShape, CursorVisibility, Surface};

use crate::widgets::layout::{ChildOrientation, Constraints};
use crate::widgets::{
    ParentRelativeCoords, RenderArgs, Ui, UpdateArgs, Widget, WidgetEvent, WidgetId,
};

pub fn create_ui<'a>(
    input: Box<dyn Read>,
    width: usize,
    height: usize,
    open_link: Box<dyn FnMut(&str) -> Result<()>>,
) -> Result<AteUi<'a>> {
    let doc = Rc::new(Document::new(input)?);
    let state = State::new(doc, open_link, width, height);
    let shared = state.shared.clone();
    let mut ui = Ui::new(state);
    let root_id = ui.set_root(MainWidget {});
    let doc_id = ui.add_child(root_id, DocumentWidget {});
    ui.set_focus(doc_id);
    let search_id = ui.add_child(root_id, SearchWidget {});
    ui.add_child(root_id, StatusWidget {});

    // Send a resize event through to get us to do an initial layout
    ui.queue_event(WidgetEvent::Input(InputEvent::Resized {
        cols: width,
        rows: height,
    }));
    ui.process_event_queue()?;
    Ok(AteUi {
        ui,
        shared,
        doc_id,
        search_id,
    })
}

pub struct AteUi<'a> {
    ui: Ui<'a, crate::state::State>,
    shared: Rc<RefCell<Shared>>,
    doc_id: WidgetId,
    search_id: WidgetId,
}

pub enum StepNext {
    QUIT,
    WAIT,
}

impl<'a> AteUi<'a> {
    pub fn step(&mut self, surface: &mut Surface) -> Result<StepNext> {
        loop {
            self.shared.borrow_mut().term_height = surface.dimensions().1;
            self.ui.process_event_queue()?;
            if self.shared.borrow().quit {
                return Ok(StepNext::QUIT);
            }
            self.ui.set_focus(if self.shared.borrow().searching {
                self.search_id
            } else {
                self.doc_id
            });

            // After updating and processing all of the widgets, compose them
            // and render them to the screen.
            if self.ui.render_to_screen(surface)? {
                // We have more events to process immediately; don't block waiting
                // for input below, but jump to the top of the loop to re-run the
                // updates.
                continue;
            }
            break;
        }
        Ok(StepNext::WAIT)
    }

    pub fn queue_event(&mut self, input: WidgetEvent) {
        self.ui.queue_event(input);
    }
}

fn render_lines(
    doc: &Document,
    view: &DocumentView,
    mut line: usize,
    height: usize,
    highlights: &[(usize, usize)],
    changes: &mut Vec<Change>,
) {
    let mut byte = view.lines()[line].start_byte;
    let line_attrs = view.lines()[line].start_attributes.clone();
    // Tracks the inverse sgr state for byte.
    // We switch it when in a highlight and then go back to the set state when exiting
    // the highlight
    let mut reversed = line_attrs.reverse();
    // Start with our line's state
    changes.push(Change::AllAttributes(line_attrs));

    let mut attr_idx = doc.attrs.partition_point(|(b, _)| *b < byte);
    let mut highlight_idx = highlights.partition_point(|(_, e)| *e <= byte);
    let mut highlight: Option<(usize, usize)> = None;
    let mut cells_in_line = 0;
    let last_displayed_line = min(view.lines().len(), line + height) - 1;
    for (grapheme, cells) in
        Graphemes::new(&doc.text[byte..]).map(|g| (g, grapheme_column_width(g, None)))
    {
        if cells_in_line + cells > view.width() || grapheme == "\n" {
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

struct DocumentWidget {}

impl DocumentWidget {
    fn process_key(&mut self, event: &KeyEvent, state: &mut State) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::UpArrow,
                ..
            } => {
                state.view.backward(1);
                true
            }
            KeyEvent {
                key: KeyCode::DownArrow,
                ..
            } => {
                state.view.forward(1);
                true
            }
            KeyEvent {
                key: KeyCode::Char(' '),
                ..
            } => {
                state.view.forward(state.view.height() - 2);
                true
            }
            KeyEvent {
                key: KeyCode::Char('b'),
                ..
            } => {
                state.view.backward(state.view.height() - 2);
                true
            }
            KeyEvent { .. } => false,
        }
    }
}

impl Widget<State> for DocumentWidget {
    fn render(&mut self, args: &mut RenderArgs, state: &mut State) {
        let (width, height) = args.surface.dimensions();
        assert!(width > 0 && height > 0);
        state.view.set_size(width, height);

        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
        ];
        render_lines(
            &state.doc,
            &state.view,
            state.view.line(),
            height,
            state.view.highlights(),
            &mut changes,
        );
        args.surface.add_changes(changes);
        args.cursor.visibility = CursorVisibility::Hidden;
    }

    fn process_event(
        &mut self,
        event: &WidgetEvent,
        _args: &mut UpdateArgs,
        state: &mut State,
    ) -> bool {
        match event {
            WidgetEvent::Input(i) => match i {
                InputEvent::Key(k) => self.process_key(k, state),
                _ => false,
            },
        }
    }

    fn get_size_constraints(&self, state: &State) -> Constraints {
        let mut c = Constraints::default();
        c.set_fixed_height(state.doc_height());
        c
    }
}

// This is a little status line widget that we render at the bottom
struct StatusWidget {}

impl Widget<State> for StatusWidget {
    fn render(&mut self, args: &mut RenderArgs, state: &mut State) {
        let mut changes = vec![
            Change::ClearScreen(AnsiColor::Grey.into()),
            Change::CursorPosition {
                x: Absolute(0),
                y: Absolute(0),
            },
        ];
        let last_error: &Option<String> = &state.last_error;
        let error_width = if let Some(err) = last_error {
            changes.push(Change::Text(err.clone()));
            unicode_column_width(&err, None)
        } else {
            0
        };
        let progress = match state.view.percent() {
            Some(p) => format!("{}%", p),
            None => "?%".to_string(),
        };
        let progress_width = unicode_column_width(&progress, None);
        let surface_width = args.surface.dimensions().0;
        if surface_width.saturating_sub(error_width + progress_width) >= 1 {
            changes.push(Change::CursorPosition {
                x: Absolute(surface_width.saturating_sub(progress_width)),
                y: Absolute(0),
            });
            changes.push(Change::Text(progress));
        }
        args.surface.add_changes(changes);
    }

    fn get_size_constraints(&self, _state: &State) -> Constraints {
        let mut c = Constraints::default();
        c.set_fixed_height(1);
        c
    }
}

impl SearchWidget {
    fn process_key(&mut self, event: &KeyEvent, state: &mut State) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::Enter,
                ..
            } => {
                state.close_search();
                true
            }
            KeyEvent {
                key: KeyCode::Escape,
                ..
            } => {
                state.cancel_search();
                true
            }
            KeyEvent {
                key: KeyCode::Char(c),
                modifiers: Modifiers::NONE | Modifiers::SHIFT,
            } => {
                state.search_mut().push_query_char(*c);
                true
            }
            KeyEvent {
                key: KeyCode::Backspace,
                ..
            } => {
                Some(state.search_mut().pop_query_char());
                true
            }
            KeyEvent {
                key: KeyCode::UpArrow,
                ..
            } => {
                Some(state.search_mut().select_prev());
                true
            }
            KeyEvent {
                key: KeyCode::DownArrow,
                ..
            } => {
                Some(state.search_mut().select_next());
                true
            }
            _ => false,
        }
    }

    fn render_matches(&mut self, height: usize, changes: &mut Vec<Change>, state: &mut State) {
        if state.search.matches().len() == 0 {
            return;
        }
        let selected_idx = state.search.selected_idx().unwrap_or(0);
        let first_visible_idx = selected_idx.saturating_sub(height - 1);
        let selected = &state.doc.links[state.search.matches()[selected_idx]];
        let highlights = vec![(selected.start, selected.end)];
        for i in first_visible_idx..(first_visible_idx + height) {
            if i >= state.search.matches().len() {
                break;
            }
            let start = state.doc.links[state.search.matches()[i]].start;
            let line = state.view.find_line(start);
            render_lines(&state.doc, &state.view, line, 1, &highlights, changes);
            changes.push(Change::Text("\r\n".to_string()));
        }
    }
}

struct SearchWidget {}

impl Widget<State> for SearchWidget {
    fn render(&mut self, args: &mut RenderArgs, state: &mut State) {
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
        if height > 3 {
            changes.push(Change::Text(format!("{}\r\n", "━".repeat(width))));
            self.render_matches(height - 2, &mut changes, state);
        }
        let search_label = format!("Search: {}", state.search.query());
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

    fn get_size_constraints(&self, state: &State) -> Constraints {
        let mut c = Constraints::default();
        c.set_fixed_height(state.search_height());
        c
    }

    fn process_event(
        &mut self,
        event: &WidgetEvent,
        _args: &mut UpdateArgs,
        state: &mut State,
    ) -> bool {
        match event {
            WidgetEvent::Input(i) => match i {
                InputEvent::Key(k) => self.process_key(k, state),
                InputEvent::Paste(s) => {
                    state.search_mut().push_query_str(&s);
                    true
                }
                _ => false,
            },
        }
    }
}

/// This is the main container widget for the app
struct MainWidget {}

impl MainWidget {
    fn process_key(&mut self, event: &KeyEvent, state: &mut State) -> bool {
        match event {
            KeyEvent {
                key: KeyCode::Char('/'),
                ..
            } => {
                state.open_search();
                true
            }
            KeyEvent {
                key: KeyCode::Char('N'),
                ..
            } => {
                state.search_mut().select_prev();
                true
            }
            KeyEvent {
                key: KeyCode::Char('n'),
                ..
            } => {
                state.search_mut().select_next();
                true
            }
            KeyEvent {
                key: KeyCode::Enter,
                ..
            } => {
                if let Err(e) = state.search_mut().open_selected() {
                    warn!("Opening selection failed with {:?}", e);
                    state.last_error = Some(format!("{}", e));
                }
                true
            }
            KeyEvent {
                key: KeyCode::Char('c'),
                modifiers: Modifiers::CTRL,
            }
            | KeyEvent {
                key: KeyCode::Char('q'),
                ..
            } => {
                // Quit the app when Ctrl-c or q are pressed
                state.shared.borrow_mut().quit = true;
                true
            }
            _ => false,
        }
    }
}

impl Widget<State> for MainWidget {
    fn render(&mut self, _args: &mut RenderArgs, _state: &mut State) {}

    fn get_size_constraints(&self, _state: &State) -> Constraints {
        let mut c = Constraints::default();
        c.child_orientation = ChildOrientation::Vertical;
        c
    }

    fn process_event(
        &mut self,
        event: &WidgetEvent,
        _args: &mut UpdateArgs,
        state: &mut State,
    ) -> bool {
        match event {
            WidgetEvent::Input(i) => match i {
                InputEvent::Key(k) => self.process_key(k, state),
                _ => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, fs, io::Cursor, rc::Rc};

    use termwiz::{color::ColorAttribute, input::Modifiers, surface::Surface};

    use crate::setup_logging;

    use super::*;

    struct Context<'a> {
        ui: AteUi<'a>,
        surface: Surface,
        visited: Rc<RefCell<Vec<String>>>,
    }

    impl<'a> Context<'a> {
        fn press_keys(&mut self, keys: Vec<KeyCode>) {
            for key in keys {
                self.ui
                    .queue_event(WidgetEvent::Input(InputEvent::Key(KeyEvent {
                        key,
                        modifiers: Modifiers::NONE,
                    })));
            }
            self.ui.step(&mut self.surface).unwrap();
        }
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
                Ok(())
            }),
        )
        .unwrap();
        let mut surface = Surface::new(width, height);
        // Render twice to test if we're stepping on ourselves
        ui.step(&mut surface).unwrap();
        ui.step(&mut surface).unwrap();
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
        assert_eq!(ctx.surface.screen_chars_to_string(), "DRD\n 0%\n");
    }

    #[test]
    fn render_short_doc() {
        let ctx = create_test_ui("Hi Bye", 3, 3);
        assert_eq!(ctx.surface.screen_chars_to_string(), "Hi \nBye\n 0%\n");
    }

    #[test]
    fn page() {
        let input = "1\n2\n3\n4\n5\n6";
        let mut ctx = create_test_ui(input, 5, 6);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());

        // Going forward while at the last screen shouldn't keep the whole screen
        ctx.press_keys(vec![KeyCode::Char(' '), KeyCode::Char(' ')]);
        let screen = ctx.surface.screen_chars_to_string();
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("2", cells[0].str(), "{}", screen);
        assert_eq!(
            ctx.surface.screen_chars_to_string(),
            "2    \n3    \n4    \n5    \n6    \n 100%\n"
        );

        // Going back while at the first line should stay at the first line
        ctx.press_keys(vec![KeyCode::Char('b'), KeyCode::Char('b')]);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("1", cells[0].str());
        assert_eq!(
            ctx.surface.screen_chars_to_string(),
            "1    \n2    \n3    \n4    \n5    \n   0%\n"
        );
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

        ctx.press_keys(vec![KeyCode::Enter]);
        let cells = &ctx.surface.screen_cells()[0];
        assert_eq!("B", cells[0].str());
        let cells = &ctx.surface.screen_cells()[1];
        assert_eq!("L", cells[0].str());
        assert!(cells[0].attrs().reverse());
        assert!(cells[1].attrs().reverse());
        assert_eq!(vec!["http://a.b".to_string()], *ctx.visited.borrow());
    }

    fn check_rev(ctx: &mut Context, reversed: usize) {
        let cells = &ctx.surface.screen_cells();
        for i in 0..6 {
            assert_eq!(format!("{}", i), cells[i][0].str());
            assert_eq!(i == reversed, cells[i][0].attrs().reverse(), "i = {}", i);
        }
    }

    #[test]
    fn next_links() {
        setup_logging().unwrap();
        let input = "0
\x1b]8;;1\x1b\\1\x1b]8;;2\x1b\\
2
\x1b]8;;3\x1b\\3
\x1b]8;;\x1b\\4
\x1b]8;;5\x1b\\5";
        fs::write("six_lines", input).unwrap();
        let mut ctx = create_test_ui(input, 10, 13);

        // Nothing should be highlighted
        check_rev(&mut ctx, usize::MAX);

        ctx.press_keys(vec![KeyCode::Char('n'), KeyCode::Char('n')]);
        check_rev(&mut ctx, 2);

        ctx.press_keys(vec![KeyCode::Char('n'), KeyCode::Char('n')]);
        check_rev(&mut ctx, 5);

        // Wrap!
        ctx.press_keys(vec![KeyCode::Char('n')]);
        check_rev(&mut ctx, 1);

        // Wrap back!
        ctx.press_keys(vec![KeyCode::Char('N')]);
        check_rev(&mut ctx, 5);

        // Wrap back!
        ctx.press_keys(vec![KeyCode::Char('N')]);
        check_rev(&mut ctx, 3);

        // Search for missing character doesn't change highlight
        ctx.press_keys(vec![KeyCode::Char('/'), KeyCode::Char('6')]);
        check_rev(&mut ctx, 3);

        // Search for a different character changes highlight
        ctx.press_keys(vec![KeyCode::Backspace, KeyCode::Char('1')]);
        check_rev(&mut ctx, 1);
    }
}
