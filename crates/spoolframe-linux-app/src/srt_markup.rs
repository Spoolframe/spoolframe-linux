//! SRT cues as Pango markup.
//!
//! A text subtitle muxed into the file (Matroska's S_TEXT/UTF8) keeps its
//! SRT tags. matroskademux passes a cue on as Pango markup when it parses
//! as such (`<i>`), and escapes it whole when not: `<font color="white">`
//! is no Pango tag, so the overlay drew it as letters. Other demuxers hand
//! the text on as plain `utf8`. Either way the tags are turned into
//! Pango's here, so they style the cue as the file meant. An external SRT
//! goes through subparse, which does this itself.

/// `cue` with SRT's tags as Pango's: `<i>`, `<b>`, `<u>` and `<s>` kept,
/// `<font color=…>` as a coloured span, everything else of a tag dropped,
/// ASS overrides (`{\an8}`) removed, and the text escaped. Tags left open
/// are closed and stray closers dropped: Pango refuses markup that does
/// not nest, and the overlay then shows nothing.
pub fn to_pango(cue: &str) -> String {
    let mut out = String::with_capacity(cue.len());
    // The Pango element each open tag became, innermost last.
    let mut open: Vec<&'static str> = Vec::new();
    let mut rest = cue;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && let Some(tag) = Tag::parse(&rest[1..end])
        {
            match tag {
                Tag::Open(element, attrs) => {
                    out.push('<');
                    out.push_str(element);
                    out.push_str(&attrs);
                    out.push('>');
                    open.push(element);
                }
                Tag::Close(element) => {
                    // Closes it and anything opened inside it; a closer
                    // with nothing open to close is dropped.
                    if let Some(at) = open.iter().rposition(|e| *e == element) {
                        for inner in open.drain(at..).rev() {
                            out.push_str("</");
                            out.push_str(inner);
                            out.push('>');
                        }
                    }
                }
                Tag::Drop => {}
            }
            rest = &rest[end + 1..];
            continue;
        }
        if c == '{'
            && rest.starts_with("{\\")
            && let Some(end) = rest.find('}')
        {
            rest = &rest[end + 1..];
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
        rest = &rest[c.len_utf8()..];
    }
    for element in open.into_iter().rev() {
        out.push_str("</");
        out.push_str(element);
        out.push('>');
    }
    out
}

/// A cue matroskademux escaped whole because of an SRT tag Pango lacks,
/// converted as [`to_pango`] converts plain text; `None` for markup with
/// no escaped SRT tag in it, which is shown as it is.
pub fn repair_escaped(markup: &str) -> Option<String> {
    const ESCAPED: [&str; 10] = [
        "&lt;font",
        "&lt;/font",
        "&lt;i&gt;",
        "&lt;/i&gt;",
        "&lt;b&gt;",
        "&lt;/b&gt;",
        "&lt;u&gt;",
        "&lt;/u&gt;",
        "&lt;s&gt;",
        "&lt;/s&gt;",
    ];
    let lower = markup.to_ascii_lowercase();
    if !ESCAPED.iter().any(|tag| lower.contains(tag)) {
        return None;
    }
    let plain = markup
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    Some(to_pango(&plain))
}

enum Tag {
    /// The Pango element, and its attributes ready to write.
    Open(&'static str, String),
    Close(&'static str),
    /// A tag SRT players know but Pango has no use for.
    Drop,
}

impl Tag {
    /// The inside of `<…>`; `None` when it is no tag at all ("a < b"),
    /// so it is shown as text.
    fn parse(inside: &str) -> Option<Self> {
        let (closing, body) = match inside.strip_prefix('/') {
            Some(body) => (true, body),
            None => (false, inside),
        };
        let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
        let name = body[..name_end].to_ascii_lowercase();
        let element = match name.as_str() {
            "i" => "i",
            "b" => "b",
            "u" => "u",
            "s" => "s",
            "font" => "span",
            // Tags of other subtitle formats that turn up in SRT files.
            "br" | "c" | "v" | "ruby" | "rt" | "span" => return Some(Self::Drop),
            _ => return None,
        };
        if closing {
            return Some(Self::Close(element));
        }
        let attrs = match element {
            "span" => color(&body[name_end..])
                .map(|c| format!(" foreground=\"{c}\""))
                .unwrap_or_default(),
            _ => String::new(),
        };
        Some(Self::Open(element, attrs))
    }
}

/// The `color` of a `<font>` tag's attributes, as Pango reads colours: a
/// name, or `#` and 3, 6 or 8 hex digits (a bare "FFFFFF" gains its `#`).
/// Anything else is left out, as Pango would refuse the whole cue.
fn color(attrs: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let at = lower.find("color")?;
    let value = attrs[at + "color".len()..].trim_start().strip_prefix('=')?;
    let value = value.trim_start();
    let value = match value.chars().next()? {
        quote @ ('"' | '\'') => value[1..].split(quote).next()?,
        _ => value.split_whitespace().next()?,
    };
    let hex = value.strip_prefix('#').unwrap_or(value);
    if matches!(hex.len(), 3 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("#{hex}"));
    }
    (!value.is_empty() && value.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cue_the_demuxer_escaped_is_marked_up_again() {
        assert_eq!(
            repair_escaped(
                "&lt;font color=&quot;#ffffff&quot;&gt;PB&amp;J under the bridge&lt;/font&gt;"
            )
            .as_deref(),
            Some("<span foreground=\"#ffffff\">PB&amp;J under the bridge</span>")
        );
    }

    #[test]
    fn markup_without_escaped_tags_is_left_alone() {
        assert_eq!(repair_escaped("<i>Fish &amp; chips</i>"), None);
    }

    #[test]
    fn a_font_colour_becomes_a_coloured_span() {
        assert_eq!(
            to_pango("<font color=\"white\">Under the bridge.</font>"),
            "<span foreground=\"white\">Under the bridge.</span>"
        );
    }

    #[test]
    fn a_bare_hex_colour_gains_its_hash() {
        assert_eq!(
            to_pango("<font color=FFFF00>Yes</font>"),
            "<span foreground=\"#FFFF00\">Yes</span>"
        );
    }

    #[test]
    fn italics_are_kept_and_text_is_escaped() {
        assert_eq!(
            to_pango("<i>PB&J</i> for 2 < 3"),
            "<i>PB&amp;J</i> for 2 &lt; 3"
        );
    }

    #[test]
    fn tags_left_open_are_closed_and_stray_closers_dropped() {
        assert_eq!(to_pango("</b><i><b>Run"), "<i><b>Run</b></i>");
    }

    #[test]
    fn closing_an_outer_tag_closes_the_inner_ones_first() {
        assert_eq!(to_pango("<i><b>Hey</i> you"), "<i><b>Hey</b></i> you");
    }

    #[test]
    fn ass_overrides_are_removed() {
        assert_eq!(to_pango("{\\an8}Upstairs"), "Upstairs");
    }

    #[test]
    fn a_colour_pango_cannot_read_is_left_out() {
        assert_eq!(
            to_pango("<font color=\"rgb(1,2,3)\">x</font>"),
            "<span>x</span>"
        );
    }
}
