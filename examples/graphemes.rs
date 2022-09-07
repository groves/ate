use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

fn max_unicode_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            if c == '\u{fe0f}' {
                // If fe0f is included, it turns a text default presentation into an emoji
                // presentation.
                // That means this grapheme will have at least the 2-width of an emoji
                // https://emojipedia.org/variation-selector-16/
                2
            } else {
                // width is None for control characters other than \x00.
                // Treat Nones as 0 width.
                UnicodeWidthChar::width(c).unwrap_or(0)
            }
        })
        .fold(0, |acc, x| if x > acc { x } else { acc })
}

fn main() {
    let cases: HashMap<&str, (String, usize)> = HashMap::from([
        ("multi combined emoji", ("👨‍👩‍👧‍👧".to_string(), 2)),
        ("one char two cell character", ("宽".to_string(), 2)),
        ("text default as two cell emoji", ("🏳️‍⚧️".to_string(), 2)),
        ("two char one cell character", ("é".to_string(), 1)),
    ]);
    for (name, (s, expected_width)) in &cases {
        println!("\n{}\n{}", name, "=".repeat(name.len()));
        println!("{}", s);
        let mut graphemes = UnicodeSegmentation::graphemes(s.as_str(), true);
        let grapheme = graphemes.next().expect("Should be a grapheme");
        assert!(graphemes.next().is_none());
        let widest_codepoint = max_unicode_width(grapheme);
        println!(
            "unicode_width={} widest_codepoint={} chars={:#?}",
            UnicodeWidthStr::width(s.as_str()),
            widest_codepoint,
            s.chars(),
        );
        assert_eq!(widest_codepoint, *expected_width);
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    macro_rules! max_unicode_width_tests {
    ($($name:ident: $value:expr,)*) => {
    $(
        #[test]
        fn $name() {
            let (input, expected) = $value;
        let mut graphemes = UnicodeSegmentation::graphemes(input, true);
        let grapheme = graphemes.next().expect("Should be a grapheme");
        assert!(graphemes.next().is_none());
            assert_eq!(expected, max_unicode_width(grapheme));
        }
    )*
    }
}

    max_unicode_width_tests! {
        multi_combined_emoji: ("👨‍👩‍👧‍👧", 2),
    one_char_two_cell_character: ("宽", 2),
    text_default_as_two_cell_emoji: ("🏳️‍⚧️", 2),
    two_char_one_cell_character: ("é", 1),
    }
}
