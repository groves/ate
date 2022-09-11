use std::io::{stdin, Read};

use termwiz::caps::Capabilities;
use termwiz::cell::{grapheme_column_width, AttributeChange, CellAttributes};
use termwiz::escape::csi::Sgr;
use termwiz::escape::parser::Parser;
use termwiz::escape::Action::{self, Control, Print};
use termwiz::escape::ControlCode::LineFeed;
use termwiz::escape::CSI;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::Change;
use termwiz::terminal::buffered::BufferedTerminal;
use termwiz::terminal::{new_terminal, Terminal};
use termwiz::Error;

struct Document {
    text: String,
    attrs: Vec<(usize, Change)>,
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
        Ok(Document { text, attrs })
    }

    // Takes a start line, a number of lines to render, and the width in character cells of those
    // lines.
    // Returns the changes necessary to render those lines assuming a terminal cursor at the
    // start of the first line and the lines rendered.
    // TODO - Reads from the underlying stream if it hasn't been exhausted and more data is needed to fill
    // the lines.
    // May return an error if reading produced an error.
    // If the returned lines are fewer than requested, EOF has been reached.
    fn render(
        &mut self,
        width: usize,
        start: usize,
        lines: usize,
    ) -> Result<(Vec<Change>, usize), Error> {
        assert!(width > 0);
        assert!(lines > 0);
        let mut changes = vec![];
        use unicode_segmentation::UnicodeSegmentation;
        let mut graphemes = self
            .text
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));
        let mut text_idx = 0;
        let mut attr_index = 0;
        let mut cells_in_line = 0;
        let mut line = 0;
        let end = start + lines;
        while line < end {
            if let Some((grapheme, cells)) = graphemes.next() {
                if cells_in_line + cells > width || grapheme == "\n" {
                    line += 1;
                    cells_in_line = 0;
                    if line >= start && line < end {
                        changes.push(Change::Text("\r\n".to_string()));
                    }
                }
                if grapheme != "\n" {
                    while attr_index < self.attrs.len() && text_idx >= self.attrs[attr_index].0 {
                        changes.push(self.attrs[attr_index].1.clone());
                        attr_index += 1;
                    }
                    if line >= start {
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
                line += 1;
                break;
            }
        }
        Ok((changes, line - start))
    }
}

fn main() -> Result<(), Error> {
    let caps = Capabilities::new_from_env()?;
    let underlying_term = new_terminal(caps)?;
    let mut term = BufferedTerminal::new(underlying_term)?;
    term.terminal().set_raw_mode()?;

    let size = term.terminal().get_screen_size()?;
    let mut doc = Document::new(Box::new(stdin()))?;
    let (changes, mut last_rendered) = doc.render(size.cols, 0, size.rows - 2)?;
    term.add_changes(changes);
    term.flush()?;

    loop {
        match term.terminal().poll_input(None) {
            Ok(Some(input)) => match input {
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                }) => {
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(' '),
                    ..
                }) => {
                    let (changes, amount_rendered) =
                        doc.render(size.cols, last_rendered, last_rendered + size.rows - 2)?;
                    last_rendered += amount_rendered;
                    term.add_changes(changes);
                    term.flush()?;
                }
                _ => {
                    print!("{:?}\r\n", input);
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
    use termwiz::{
        color::ColorAttribute,
        surface::{Position::Absolute, Surface},
    };

    use super::*;

    #[test]
    fn parse_color_output() -> Result<(), Error> {
        let input = "D\x1b[31mR\x1b[mD";
        let mut doc = Document::new(Box::new(input.as_bytes()))?;
        let (changes, _) = doc.render(10, 0, 1)?;
        let mut screen = Surface::new(10, 1);
        screen.add_changes(changes);
        let cells = &screen.screen_cells()[0];
        assert_eq!(ColorAttribute::Default, cells[0].attrs().foreground());
        assert_eq!(
            ColorAttribute::PaletteIndex(1),
            cells[1].attrs().foreground()
        );
        assert_eq!(ColorAttribute::Default, cells[2].attrs().foreground());
        Ok(())
    }

    #[test]
    fn render_short_doc() -> Result<(), Error> {
        let mut d = Document::new(Box::new("Hi Bye".as_bytes()))?;
        assert_eq!(
            (
                vec![
                    Change::AllAttributes(CellAttributes::default()),
                    Change::Text("H".to_string()),
                    Change::Text("i".to_string()),
                    Change::Text(" ".to_string()),
                    Change::Text("\r\n".to_string()),
                    Change::Text("B".to_string()),
                    Change::Text("y".to_string()),
                    Change::Text("e".to_string()),
                ],
                2
            ),
            d.render(3, 0, 2)?
        );
        assert_eq!(
            (vec![Change::AllAttributes(CellAttributes::default()),], 0),
            d.render(3, 2, 2)?
        );
        Ok(())
    }

    #[test]
    fn render_backwards() -> Result<(), Error> {
        let input = "1\n2\n3\n";
        let mut doc = Document::new(Box::new(input.as_bytes()))?;
        let (changes, lines) = doc.render(1, 0, 1)?;
        let mut screen = Surface::new(1, 1);
        screen.add_changes(changes);
        let cells = &screen.screen_cells()[0];
        assert_eq!(1, lines);
        assert_eq!("1", cells[0].str());
        let (changes, _) = doc.render(1, 1, 1)?;
        screen.add_changes(changes);
        let cell = {
            let cells = screen.screen_cells();
            cells[0][0].str().to_string()
        };
        assert_eq!(
            "2",
            cell,
            "Expected screen to just be '2' but got '{}'",
            screen.screen_chars_to_string(),
        );
        let (changes, _) = doc.render(1, 0, 1)?;
        screen.add_change(Change::CursorPosition {
            x: Absolute(0),
            y: Absolute(0),
        });
        screen.add_changes(changes);
        let cells = &screen.screen_cells()[0];
        assert_eq!("1", cells[0].str());
        Ok(())
    }
}
