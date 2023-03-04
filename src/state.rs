use std::{
    cell::RefCell,
    cmp::{max, min},
    rc::Rc,
};

use crate::doc::Document;
use anyhow::Result;
use finl_unicode::grapheme_clusters::Graphemes;
use log::{debug, info};
use termwiz::{
    cell::{grapheme_column_width, CellAttributes},
    surface::Change,
};

// Anything we need to share with the surrounding application goes in here
// It's in a RefCell so we can mutate from either side
pub struct Shared {
    pub searching: bool,
    // We keep the raw term height to be able to do fixed size layout.
    // TODO - fix termwiz layout to get rid of this:
    // https://github.com/wez/wezterm/issues/2543
    pub term_height: usize,
    pub quit: bool,
}

impl Shared {
    fn new(term_height: usize) -> Self {
        Self {
            searching: false,
            term_height,
            quit: false,
        }
    }
}

pub struct State {
    pub doc: Rc<Document>,
    pub view: DocumentView,
    pub search: Search,
    pub last_error: Option<String>,
    pub shared: Rc<RefCell<Shared>>,

    // TODO - store the byte in case width changes and keep track of the selected search, too
    search_activate_line: usize,
}

impl State {
    pub fn new(
        doc: Rc<Document>,
        open_link: Box<dyn FnMut(&str) -> Result<()>>,
        width: usize,
        height: usize,
    ) -> Self {
        let search = Search::new(Rc::clone(&doc), open_link);
        let view = DocumentView::new(Rc::clone(&doc), width, height);
        Self {
            doc,
            view,
            search,
            last_error: None,
            shared: Rc::new(RefCell::new(Shared::new(height))),
            search_activate_line: 0,
        }
    }

    pub fn search_mut(&mut self) -> SearchMutator {
        SearchMutator {
            search: &mut self.search,
            view: &mut self.view,
        }
    }

    pub fn open_search(&mut self) {
        self.shared.borrow_mut().searching = true;
        self.search_activate_line = self.view.line;
        self.search_mut().activate();
    }

    pub fn close_search(&self) {
        self.shared.borrow_mut().searching = false;
    }

    pub fn cancel_search(&mut self) {
        self.close_search();
        self.view.line = self.search_activate_line;
    }

    fn all_but_status_height(&self) -> u16 {
        self.shared.borrow().term_height as u16 - 1
    }

    pub fn search_height(&self) -> u16 {
        if self.shared.borrow().searching {
            // 1 line each for separator and search entry
            let chrome_height = 2;
            (10 + chrome_height) // Show at most 10 matches
                // but show fewer if that would take more than half the screen
                .min(self.all_but_status_height() / 2)
                // and only take up as many lines as we have matches
                .min(self.search.matches.len() as u16 + chrome_height)
                // but at least take 1 for search entry if that's all there is
                .max(1)
        } else {
            0
        }
    }

    pub fn doc_height(&self) -> u16 {
        self.all_but_status_height() - self.search_height()
    }
}

// Only valid for a particular text width due to reflowing
pub struct Line {
    pub start_byte: usize,
    // The full set of active attributes to let set up this line for rendering.
    pub start_attributes: CellAttributes,
}

pub struct DocumentView {
    // Reverses the reverse display of bytes in these ranges.
    // If reverse is off for a byte, flips it on and vice versa.
    highlights: Vec<(usize, usize)>,
    // First displayed line
    // Used for paging forward and backwards.
    // In reflow, the start_byte of this line is kept in the first_displayed_line of the reflowed
    // lines
    line: usize,
    width: usize,

    doc: Rc<Document>,

    height: usize,
    // Cache of text flown at width.
    // Will be recalculated if width changes.
    // We cache the start point of the line so that when we go backwards, we can rerender without
    // having to start from the start of the document to find where long lines break.
    lines: Vec<Line>,
}

impl DocumentView {
    fn new(doc: Rc<Document>, width: usize, height: usize) -> Self {
        let lines = Self::flow(width, &doc.text, &doc.attrs);
        Self {
            doc,
            width,
            height,
            line: 0,
            highlights: vec![],
            lines,
        }
    }

    pub fn highlight(&mut self, start: usize, end: usize) {
        self.highlights = vec![(start, end)];
        self.make_line_visible(self.find_line(start));
    }

    pub fn backward(&mut self, lines: usize) {
        self.line = self.line.saturating_sub(lines);
    }

    pub fn forward(&mut self, lines: usize) {
        self.line = min(
            self.lines.len().saturating_sub(self.height),
            self.line + max(1, lines),
        );
    }

    fn make_line_visible(&mut self, line: usize) {
        debug!("Current {} New {}, End {}", self.line, line, self.height);
        if line.saturating_sub(3) < self.line || (line + 3) > (self.line + self.height) {
            self.line = line.saturating_sub(3);
        }
    }

    pub fn set_size(&mut self, width: usize, height: usize) {
        self.height = height;
        if width == self.width {
            return;
        }
        // TODO - update line to keep current view position
        self.width = width;
        self.lines = Self::flow(width, &self.doc.text, &self.doc.attrs);
    }

    pub fn highlights(&self) -> &[(usize, usize)] {
        &self.highlights
    }

    pub fn line(&self) -> usize {
        self.line
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn find_line(&self, byte: usize) -> usize {
        self.lines.partition_point(|l| l.start_byte <= byte) - 1
    }

    pub fn percent(&self) -> Option<u8> {
        if self.line == 0 || self.lines.len() < self.height {
            return Some(0);
        }
        let final_page_line = self.lines.len() - self.height;
        if final_page_line == self.line {
            Some(100)
        } else {
            let percent = (self.line as f64 / (final_page_line as f64)) * 100.0;
            Some(percent.floor() as u8)
        }
    }

    fn flow(width: usize, text: &str, attrs: &[(usize, Change)]) -> Vec<Line> {
        // TODO - Only flow the lines necessary to render the screen.
        // Read from the underlying stream if at the point of flowing.
        let mut lines = vec![];

        let mut byte = 0;
        let graphemes = Graphemes::new(text).map(|g| (g, grapheme_column_width(g, None)));
        let mut attr_idx = 0;
        let mut cells_in_line = 0;
        let mut attributes = CellAttributes::default();
        lines.push(Line {
            start_byte: byte,
            start_attributes: attributes.clone(),
        });
        for (grapheme, cells) in graphemes {
            if cells_in_line + cells > width || grapheme == "\n" {
                lines.push(Line {
                    start_byte: if grapheme == "\n" { byte + 1 } else { byte },
                    start_attributes: attributes.clone(),
                });
                cells_in_line = 0;
            }
            if grapheme != "\n" {
                while attr_idx < attrs.len() && byte >= attrs[attr_idx].0 {
                    match &attrs[attr_idx].1 {
                        Change::AllAttributes(_) => {
                            attributes = CellAttributes::default();
                        }
                        Change::Attribute(a) => {
                            attributes.apply_change(a);
                        }
                        _ => unreachable!(),
                    }
                    attr_idx += 1;
                }
                cells_in_line += cells;
            }
            byte += grapheme.len();
        }
        lines
    }
}

pub struct Search {
    doc: Rc<Document>,
    query: String,
    selected_idx: Option<usize>,
    open_link: Box<dyn FnMut(&str) -> Result<()>>,
    matches: Vec<usize>,
}

impl Search {
    fn new(doc: Rc<Document>, open_link: Box<dyn FnMut(&str) -> Result<()>>) -> Search {
        let matches = doc.links.iter().enumerate().map(|(i, _)| i).collect();
        Search {
            doc,
            open_link,
            query: String::new(),
            selected_idx: None,
            matches,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected_idx(&self) -> Option<usize> {
        self.selected_idx
    }

    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    fn set_selected_idx(&mut self, selected_idx: usize, view: &mut DocumentView) {
        if selected_idx < self.matches.len() {
            self.selected_idx = Some(selected_idx);
            let link = &self.doc.links[self.matches[selected_idx]];
            view.highlight(link.start, link.end)
        }
    }

    fn update_matches(&mut self, view: &mut DocumentView) {
        let previous_link_idx = if self.matches.len() > 0 {
            self.matches[self.selected_idx.unwrap_or(0)]
        } else {
            0
        };
        self.matches = self
            .doc
            .links
            .iter()
            .enumerate()
            .filter(|(_, l)| self.doc.text[l.start..l.end].contains(&self.query))
            .map(|(i, _)| i)
            .collect();
        let mut new_selected_idx = self
            .matches
            .partition_point(|link_idx| link_idx < &previous_link_idx);
        if new_selected_idx == self.matches.len() {
            new_selected_idx = self.matches.len().saturating_sub(1);
        }
        self.set_selected_idx(new_selected_idx, view);
    }
}

// Changing search requires updating view. This joins them to make it more straightforward
pub struct SearchMutator<'a> {
    search: &'a mut Search,
    view: &'a mut DocumentView,
}

impl<'a> SearchMutator<'a> {
    fn activate(&mut self) {
        self.search
            .set_selected_idx(self.search.selected_idx.unwrap_or(0), self.view)
    }

    pub fn open_selected(&mut self) -> Result<()> {
        if self.search.matches.len() == 0 {
            return Ok(());
        }
        let selected_idx = match self.search.selected_idx {
            Some(idx) => idx,
            None => {
                self.search.set_selected_idx(0, self.view);
                0
            }
        };
        let link = &self.search.doc.links[self.search.matches[selected_idx]];
        let addr = link.link.uri();
        info!("Opening {}", addr);
        (self.search.open_link)(addr)
    }

    pub fn select_next(&mut self) {
        self.search.set_selected_idx(
            match self.search.selected_idx {
                Some(idx) if idx < self.search.matches.len().saturating_sub(1) => idx + 1,
                _ => 0,
            },
            self.view,
        );
    }

    pub fn select_prev(&mut self) {
        self.search.set_selected_idx(
            match self.search.selected_idx {
                Some(0) | None => self.search.matches.len().saturating_sub(1),
                Some(idx) => idx - 1,
            },
            self.view,
        );
    }

    pub(crate) fn push_query_char(&mut self, c: char) {
        self.search.query.push(c);
        self.search.update_matches(self.view);
    }

    pub(crate) fn pop_query_char(&mut self) {
        self.search.query.pop();
        self.search.update_matches(self.view);
    }

    pub(crate) fn push_query_str(&mut self, s: &str) {
        self.search.query.push_str(s);
        self.search.update_matches(self.view);
    }
}
