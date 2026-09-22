use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct FileView {
    pub path: PathBuf,
    lines: Vec<Line<'static>>,
    wrapped: Vec<Line<'static>>,
    width: usize,
    height: usize,
    scroll: usize,
}

impl FileView {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
        if !path.metadata()?.is_file() {
            return Err(invalid("selected path is not a regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        // A regular file can be replaced by a FIFO between stat and open.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("selected path is not a regular file"));
        }
        if metadata.len() > MAX_BYTES as u64 {
            return Err(invalid(
                "file exceeds the 1 MiB viewer limit; use e to edit",
            ));
        }
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid(
                "file exceeds the 1 MiB viewer limit; use e to edit",
            ));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid("viewer supports UTF-8 text files only; use e to edit"))?;
        if text.contains('\0') {
            return Err(invalid("binary files cannot be viewed; use e to edit"));
        }
        let text = sanitize(text);
        let lines = if path.extension().is_some_and(|extension| extension == "md") {
            markdown(&text)
        } else {
            text.split('\n')
                .map(|line| Line::raw(line.to_owned()))
                .collect()
        };
        Ok(Self {
            path,
            lines,
            wrapped: Vec::new(),
            width: 0,
            height: 1,
            scroll: 0,
        })
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let half = (self.height / 2).max(1);
        let delta = if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => half as isize,
                KeyCode::Char('u') => -(half as isize),
                _ => 0,
            }
        } else {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => 1,
                KeyCode::Char('k') | KeyCode::Up => -1,
                KeyCode::PageDown => self.height as isize,
                KeyCode::PageUp => -(self.height as isize),
                KeyCode::Char('g') => {
                    self.scroll = 0;
                    0
                }
                KeyCode::Char('G') => {
                    self.scroll = self.max_scroll();
                    0
                }
                _ => 0,
            }
        };
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
    }

    fn max_scroll(&self) -> usize {
        self.wrapped.len().saturating_sub(self.height)
    }

    fn resize(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        if width != self.width {
            self.width = width;
            self.wrapped = wrap_lines(&self.lines, width);
        }
        self.height = height.max(1);
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", sanitize(&self.path.to_string_lossy())))
            .title_bottom(" j/k ↑/↓ scroll · PgUp/PgDn Ctrl-u/d page · g/G ends · Esc back ");
        let inner = block.inner(area);
        self.resize(inner.width as usize, inner.height as usize);
        frame.render_widget(block, area);
        let visible: Vec<_> = self
            .wrapped
            .iter()
            .skip(self.scroll)
            .take(self.height)
            .cloned()
            .collect();
        frame.render_widget(Paragraph::new(visible), inner);
    }
}

fn sanitize(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for character in text.replace("\r\n", "\n").chars() {
        match character {
            '\n' => result.push('\n'),
            '\t' => result.push_str("    "),
            c if c.is_control() => result.push('�'),
            c => result.push(c),
        }
    }
    result
}

fn wrap_lines(lines: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    for line in lines {
        let mut spans = Vec::new();
        let mut used = 0;
        for span in &line.spans {
            let mut chunk = String::new();
            for grapheme in span.content.graphemes(true) {
                let size = grapheme.width();
                if used + size > width && used > 0 {
                    if !chunk.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut chunk), span.style));
                    }
                    result.push(Line::from(std::mem::take(&mut spans)));
                    used = 0;
                }
                // A wide grapheme cannot fit in a one-column terminal.
                if size > width {
                    chunk.push('�');
                    used += 1;
                } else {
                    chunk.push_str(grapheme);
                    used += size;
                }
            }
            if !chunk.is_empty() {
                spans.push(Span::styled(chunk, span.style));
            }
        }
        result.push(Line::from(spans));
    }
    result
}

fn markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut styles = vec![Style::default()];
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut links = Vec::new();
    let flush = |lines: &mut Vec<Line<'static>>, current: &mut Vec<Span<'static>>| {
        if !current.is_empty() {
            lines.push(Line::from(std::mem::take(current)));
        }
    };
    for event in Parser::new_ext(
        text,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        let style = *styles.last().unwrap();
        match event {
            Event::Start(tag) => {
                let mut next = style;
                match tag {
                    Tag::Heading { .. } => {
                        flush(&mut lines, &mut current);
                        next = style.fg(Color::Cyan).add_modifier(Modifier::BOLD);
                    }
                    Tag::Strong => next = style.add_modifier(Modifier::BOLD),
                    Tag::Emphasis => next = style.add_modifier(Modifier::ITALIC),
                    Tag::Strikethrough => next = style.add_modifier(Modifier::CROSSED_OUT),
                    Tag::CodeBlock(_) => {
                        flush(&mut lines, &mut current);
                        next = style.fg(Color::Yellow);
                    }
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        links.push(dest_url.to_string());
                        next = style
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED);
                    }
                    Tag::List(start) => {
                        flush(&mut lines, &mut current);
                        lists.push(start);
                    }
                    Tag::Item => {
                        flush(&mut lines, &mut current);
                        let indent = "  ".repeat(lists.len().saturating_sub(1));
                        let marker = match lists.last_mut() {
                            Some(Some(number)) => {
                                let marker = format!("{number}. ");
                                *number = number.saturating_add(1);
                                marker
                            }
                            _ => "• ".to_owned(),
                        };
                        current.push(Span::raw(format!("{indent}{marker}")));
                    }
                    Tag::BlockQuote(_) => {
                        flush(&mut lines, &mut current);
                        current.push(Span::raw("│ "));
                        next = style.fg(Color::Gray);
                    }
                    _ => {}
                }
                styles.push(next);
            }
            Event::End(tag) => {
                styles.pop();
                match tag {
                    TagEnd::Link | TagEnd::Image => {
                        if let Some(url) = links.pop() {
                            current.push(Span::styled(format!(" ({url})"), style));
                        }
                    }
                    TagEnd::List(_) => {
                        flush(&mut lines, &mut current);
                        lists.pop();
                    }
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::CodeBlock
                    | TagEnd::Item
                    | TagEnd::BlockQuote(_) => {
                        flush(&mut lines, &mut current);
                        if matches!(
                            tag,
                            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock
                        ) {
                            lines.push(Line::default());
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(value) | Event::Html(value) | Event::InlineHtml(value) => {
                for (index, part) in value.split('\n').enumerate() {
                    if index > 0 {
                        lines.push(Line::from(std::mem::take(&mut current)));
                    }
                    if !part.is_empty() {
                        current.push(Span::styled(part.to_owned(), style));
                    }
                }
            }
            Event::Code(value) => {
                current.push(Span::styled(value.to_string(), style.fg(Color::Yellow)))
            }
            Event::SoftBreak => current.push(Span::raw(" ")),
            Event::HardBreak => lines.push(Line::from(std::mem::take(&mut current))),
            Event::Rule => {
                flush(&mut lines, &mut current);
                lines.push(Line::raw("────────"));
            }
            Event::TaskListMarker(checked) => {
                current.push(Span::raw(if checked { "[x] " } else { "[ ] " }))
            }
            _ => {}
        }
    }
    flush(&mut lines, &mut current);
    if lines.is_empty() {
        lines.push(Line::default());
    }
    // Entity decoding can introduce controls after the source was sanitized.
    for line in &mut lines {
        for span in &mut line.spans {
            span.content = sanitize(&span.content).into();
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn rendered(view: &mut FileView, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn markdown_formats_blocks_and_inline_styles() {
        let lines = markdown(
            "# Heading\n\n**bold** and *italic* [link](https://example.com)\n\n- item\n\n3. third\n\n```rust\nlet x = 1;\n```\n",
        );
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        for expected in [
            "Heading",
            "bold and italic link (https://example.com)",
            "• item",
            "3. third",
            "let x = 1;",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("```"));
        assert!(!text.contains("**"));
        let span = |text: &str| {
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content == text)
                .unwrap()
        };
        assert!(span("Heading").style.add_modifier.contains(Modifier::BOLD));
        assert!(span("bold").style.add_modifier.contains(Modifier::BOLD));
        assert!(span("italic").style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(span("let x = 1;").style.fg, Some(Color::Yellow));
    }

    #[test]
    fn markdown_entity_controls_are_sanitized() {
        let lines = markdown("&#27;[2J [label](https://example.com/&#27;) &#7;");
        let text: String = lines.iter().map(ToString::to_string).collect();
        assert!(!text.chars().any(char::is_control));
    }

    #[test]
    fn loading_formats_only_markdown_and_sanitizes_terminal_controls() {
        let dir = tempfile::tempdir().unwrap();
        for extension in ["md", "txt"] {
            let path = dir.path().join(format!("notes.{extension}"));
            std::fs::write(&path, "# Heading\r\n\ttext\x1b[2J\x07").unwrap();
            let mut view = FileView::load(path).unwrap();
            let display = rendered(&mut view, 80, 12);
            assert!(display.contains("Heading"));
            assert_eq!(display.contains("# Heading"), extension == "txt");
            assert!(display.contains("text�[2J�"));
            if extension == "txt" {
                assert!(display.contains("    text"));
            }
            assert!(display.contains("Esc back"));
            assert!(!display.contains('\x1b'));
        }
    }

    #[test]
    fn empty_missing_binary_large_and_non_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        assert!(FileView::load(path.clone()).is_err());
        assert!(FileView::load(dir.path().to_owned()).is_err());
        for content in [vec![0], vec![0xff], vec![b'a'; MAX_BYTES + 1]] {
            std::fs::write(&path, content).unwrap();
            assert!(FileView::load(path.clone()).is_err());
        }
        for extension in ["txt", "md"] {
            let path = path.with_extension(extension);
            std::fs::write(&path, "").unwrap();
            let mut view = FileView::load(path).unwrap();
            rendered(&mut view, 20, 5);
            assert_eq!(view.max_scroll(), 0);
        }
    }

    #[test]
    fn scrolling_wraps_unicode_and_clamps_after_resize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text");
        let content = format!("{}\nTHE END", "é界👩‍💻e\u{301} ".repeat(20));
        std::fs::write(&path, &content).unwrap();
        let mut view = FileView::load(path).unwrap();
        rendered(&mut view, 12, 5);
        assert!(view.wrapped.iter().all(|line| line.width() <= 10));
        let reconstructed: String = view.wrapped.iter().map(ToString::to_string).collect();
        assert_eq!(reconstructed, content.replace('\n', ""));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.scroll, 1);
        view.handle_key(key(KeyCode::PageDown));
        assert_eq!(view.scroll, 4);
        view.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(view.scroll, 3);
        view.handle_key(key(KeyCode::Char('G')));
        assert_eq!(view.scroll, view.max_scroll());
        assert!(rendered(&mut view, 12, 5).contains("THE END"));
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.scroll, view.max_scroll());
        rendered(&mut view, 80, 30);
        assert_eq!(view.scroll, 0);
        view.handle_key(key(KeyCode::Char('g')));
        view.handle_key(key(KeyCode::Up));
        assert_eq!(view.scroll, 0);
        rendered(&mut view, 1, 1);
        rendered(&mut view, 0, 0);
    }

    #[test]
    fn scroll_offsets_are_not_limited_to_u16() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many-lines");
        std::fs::write(&path, format!("{}LAST", "a\n".repeat(70_000))).unwrap();
        let mut view = FileView::load(path).unwrap();
        rendered(&mut view, 40, 5);
        view.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert!(view.scroll > u16::MAX as usize);
        assert!(rendered(&mut view, 40, 5).contains("LAST"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_and_regular_symlinks_are_supported() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'f', 0xff]));
        std::fs::write(&path, "content").unwrap();
        assert_eq!(FileView::load(path.clone()).unwrap().path, path);
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(FileView::load(link).is_ok());
    }
}
