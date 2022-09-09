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

    // Takes a start byte offset into text, a number of lines to render, and the width in character
    // cells of those lines.
    // Returns the changes necessary to render those lines assuming a terminal cursor at the
    // start of the first line and the bytes consumed.
    // Pass start + the consumed bytes to future render calls to continue from the last displayed
    // character in the changes.
    // Reads from the underlying stream if it hasn't been exhausted and more data is needed to fill
    // the lines.
    // May return an error if reading produced an error.
    // If the returned bytes are 0, EOF has been reached.
    fn render(
        &mut self,
        start: usize,
        mut lines: usize,
        width: usize,
    ) -> Result<(Vec<Change>, usize), Error> {
        assert!(width > 0);
        assert!(lines > 0);
        let mut changes = vec![];
        use unicode_segmentation::UnicodeSegmentation;
        let mut graphemes = self.text[start..]
            .graphemes(true)
            .map(|g| (g, grapheme_column_width(g, None)));
        let mut text_idx = start;
        let mut attr_index = self.attrs.partition_point(|item| item.0 < start);
        let mut cells_in_line = 0;
        loop {
            if let Some((grapheme, cells)) = graphemes.next() {
                if cells_in_line + cells > width || grapheme == "\n" {
                    if lines > 1 {
                        changes.push(Change::Text("\r\n".to_string()));
                        lines -= 1;
                        cells_in_line = 0;
                    } else {
                        break;
                    }
                }
                // LATER - accumulate multiple cells into a string rather than a change per cell
                if grapheme != "\n" {
                    while attr_index < self.attrs.len() && text_idx >= self.attrs[attr_index].0 {
                        changes.push(self.attrs[attr_index].1.clone());
                        attr_index += 1;
                    }
                    changes.push(Change::Text(grapheme.to_string()));
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
        Ok((changes, text_idx - start))
    }
}

fn main() -> Result<(), Error> {
    let caps = Capabilities::new_from_env()?;
    let underlying_term = new_terminal(caps)?;
    let mut term = BufferedTerminal::new(underlying_term)?;
    term.terminal().set_raw_mode()?;

    let size = term.terminal().get_screen_size()?;
    let mut doc = Document::new(Box::new(stdin()))?;
    let (changes, mut last_rendered) = doc.render(0, size.rows - 2, size.cols)?;
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
                        doc.render(last_rendered, size.rows - 2, size.cols)?;
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

    use termwiz::{color::ColorAttribute, surface::Surface};

    use super::*;

    #[test]
    fn parse_color_output() -> Result<(), Error> {
        let input = "D\x1b[31mR\x1b[mD";
        let mut doc = Document::new(Box::new(input.as_bytes()))?;
        let (changes, _) = doc.render(0, 1, 10)?;
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
                6
            ),
            d.render(0, 2, 3)?
        );
        assert_eq!((vec![], 0), d.render(6, 2, 3)?);
        Ok(())
    }
}
