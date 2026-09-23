//! Server rendering: puts the page's `<head>` content, and the `<html>` and `<body>`
//! attributes, into the first chunk of the page.

/// The comment that `<MetaTags/>` renders where the `<head>` content goes.
const HEAD_MARKER: &str = "<!--HEAD-->";

/// Where [`insert_head_content`] put the `<head>` content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadPlacement {
    /// After the `<!--HEAD-->` marker that `<MetaTags/>` renders.
    AfterMarker,
    /// Before `</head>`: the shell has no `<MetaTags/>`.
    BeforeHeadEnd,
    /// Before `<body`: the first chunk has no `</head>`.
    BeforeBody,
    /// At the start of the document, after its doctype if it has one: the first chunk has
    /// no `</head>` and no `<body`.
    DocumentStart,
}

/// Inserts `head` (the page's meta tags and `<title>`) into `chunk`, the start of the page,
/// and says where it went.
///
/// In order of preference: after the `<!--HEAD-->` marker of `<MetaTags/>`, before
/// `</head>`, before `<body`, or at the start of the document (after its doctype: before
/// it, the browser would render the page in quirks mode). The last two are for a first
/// chunk without `</head>`: a shell without a `<head>`, a page that is not a whole
/// document, or a first chunk that ends inside the `<head>`. Either way, the HTML parser
/// puts the `<title>`, `<meta>`, `<link>`, `<style>` and `<script>` elements that come
/// before `<body>` in the document's head.
pub(crate) fn insert_head_content(
    chunk: &str,
    head: &str,
) -> (String, HeadPlacement) {
    if let Some((before, after)) = chunk.split_once(HEAD_MARKER) {
        let page = [before, HEAD_MARKER, head, after].concat();
        return (page, HeadPlacement::AfterMarker);
    }
    if let Some((before, after)) = chunk.split_once("</head>") {
        let page = [before, head, "</head>", after].concat();
        return (page, HeadPlacement::BeforeHeadEnd);
    }
    if let Some((before, after)) = chunk.split_once("<body") {
        let page = [before, head, "<body", after].concat();
        return (page, HeadPlacement::BeforeBody);
    }
    let rest = chunk.trim_start();
    let starts_with_doctype = rest
        .get(.."<!doctype".len())
        .is_some_and(|start| start.eq_ignore_ascii_case("<!doctype"));
    if starts_with_doctype {
        if let Some((doctype, after)) = rest.split_once('>') {
            let leading_space = chunk.strip_suffix(rest).unwrap_or_default();
            let page = [leading_space, doctype, ">", head, after].concat();
            return (page, HeadPlacement::DocumentStart);
        }
    }
    ([head, chunk].concat(), HeadPlacement::DocumentStart)
}

/// Inserts `attributes` (each with its leading space) into the first `<tag` of `chunk`, where
/// `tag` is `html` or `body`. `None` if `chunk` has no such tag.
pub(crate) fn insert_attributes(
    chunk: &str,
    tag: &str,
    attributes: &str,
) -> Option<String> {
    let open = format!("<{tag}");
    let (before, after) = chunk.split_once(&open)?;
    Some([before, &open, attributes, after].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_content_goes_after_the_marker_even_after_head_end() {
        // a `<MetaTags/>` in the `<body>` is still where the content goes, as it was
        assert_eq!(
            insert_head_content(
                "<head></head><body><!--HEAD--></body>",
                "<title>t</title>"
            ),
            (
                "<head></head><body><!--HEAD--><title>t</title></body>"
                    .to_owned(),
                HeadPlacement::AfterMarker
            )
        );
    }

    #[test]
    fn head_content_goes_after_leading_space_and_the_doctype() {
        assert_eq!(
            insert_head_content(
                "\n <!DocType html><p>x</p>",
                "<title>t</title>"
            ),
            (
                "\n <!DocType html><title>t</title><p>x</p>".to_owned(),
                HeadPlacement::DocumentStart
            )
        );
    }

    #[test]
    fn an_unterminated_doctype_gets_the_content_before_it() {
        assert_eq!(
            insert_head_content("<!DOCTYPE", "<title>t</title>").0,
            "<title>t</title><!DOCTYPE"
        );
    }

    #[test]
    fn attributes_go_on_the_first_tag_only() {
        assert_eq!(
            insert_attributes("<body><body>", "body", " class=\"a\"")
                .as_deref(),
            Some("<body class=\"a\"><body>")
        );
        assert_eq!(insert_attributes("<main>", "body", " class=\"a\""), None);
    }
}
