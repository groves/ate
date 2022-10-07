use std::io::Read;

use termwiz::cell::{AttributeChange, CellAttributes};
use termwiz::escape::csi::Sgr;
use termwiz::escape::parser::Parser;
use termwiz::escape::Action::{self, Control, Print};
use termwiz::escape::ControlCode::LineFeed;
use termwiz::escape::{OperatingSystemCommand, CSI};
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::Change;
use termwiz::Error;

#[derive(Clone)]
pub(crate) struct LinkRange {
    pub start: usize,
    pub end: usize,
    pub link: Hyperlink,
}

pub(crate) struct Document {
    // The displayed characters of the input
    // i.e. input bytes with control characters stripped out
    pub text: String,
    // Display attrs to apply by byte index into text.
    // Before the characters in text at .0 have been drawn,
    // all changes with that offset should be applied.
    // Stored in ascending order of .0
    pub attrs: Vec<(usize, Change)>,
    pub links: Vec<LinkRange>,
}

impl Document {
    pub fn new<'a>(mut input: Box<dyn Read + 'a>) -> Result<Document, Error> {
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
                            Sgr::Inverse(i) => AttributeChange::Reverse(i),
                            // TODO - add an Attribute change to termwiz for vertical align
                            Sgr::VerticalAlign(_) => todo!(),
                            Sgr::UnderlineColor(_) => todo!(),
                            Sgr::Font(_) => todo!(),
                            Sgr::Overline(_) => todo!(),
                            Sgr::Reset => unreachable!(),
                        }),
                    };
                    // This isn't parsing by grapheme, which may put this change in the middle of one.
                    // We render by grapheme and changes in the middle of one will be applied
                    // afterwards.
                    // It's nonsensical to change graphical representation in the middle of a
                    // grapheme, so I don't think that's an issue.
                    // We do need to make sure to apply all graphical changes, not just those
                    // that land on grapheme boundaries
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
#[cfg(test)]
mod tests {

    use std::io::Cursor;

    use super::*;

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
