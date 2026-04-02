/// Terminal markdown renderer using ANSI escape codes.
///
/// Supports:
/// - Headers (bold + cyan)
/// - **bold** / *italic*
/// - `inline code` (yellow)
/// - Fenced code blocks (dimmed, with language label)
/// - Tables with box-drawing characters
/// - Horizontal rules
/// - <think>...</think> tag collapsing
use crate::session::{is_temp_session, short_id};
use std::path::PathBuf;

// ANSI escape codes
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";
const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
#[allow(dead_code)]
const GREEN: &str = "\x1b[32m";
#[allow(dead_code)]
const MAGENTA: &str = "\x1b[35m";

// ANSI cursor control
const CLEAR_LINE: &str = "\x1b[2K";
const CURSOR_UP: &str = "\x1b[A";

/// Path to store the last thinking content.
pub fn thinking_file_path() -> PathBuf {
    let config_dir = dirs_or_default();
    config_dir.join(".last_thinking")
}

fn dirs_or_default() -> PathBuf {
    if let Some(dir) = dirs::config_dir() {
        dir.join("chat-cli")
    } else {
        PathBuf::from(".config/chat-cli")
    }
}

/// Save thinking content to the persistent file.
pub fn save_thinking(content: &str) {
    let path = thinking_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, content);
}

/// Load the last saved thinking content.
pub fn load_thinking() -> Option<String> {
    let path = thinking_file_path();
    std::fs::read_to_string(path).ok().filter(|s| !s.is_empty())
}

/// Strip <think>...</think> blocks from text, returning (rendered_content, thinking_content).
fn strip_thinking(input: &str) -> (String, String) {
    let mut output = String::new();
    let mut thinking = String::new();
    let mut remaining = input;

    while let Some(start) = remaining.find("<think>") {
        output.push_str(&remaining[..start]);
        remaining = &remaining[start + 7..]; // skip "<think>"
        if let Some(end) = remaining.find("</think>") {
            thinking.push_str(&remaining[..end]);
            remaining = &remaining[end + 8..]; // skip "</think>"
            // Skip leading newline after closing tag
            if remaining.starts_with('\n') {
                remaining = &remaining[1..];
            }
        } else {
            // Unclosed tag: treat rest as thinking
            thinking.push_str(remaining);
            remaining = "";
        }
    }
    output.push_str(remaining);

    // Trim leading/trailing whitespace from both
    (output.trim().to_string(), thinking.trim().to_string())
}

/// Render markdown text to ANSI-formatted terminal output (non-streaming).
/// Strips thinking blocks and saves them.
pub fn render_markdown(input: &str, _collapse_thinking: bool) -> String {
    let (content, thinking) = strip_thinking(input);
    if !thinking.is_empty() {
        save_thinking(&thinking);
    }

    let mut sections = Vec::new();
    if !thinking.is_empty() {
        sections.push(render_thinking_content(&thinking));
    }
    if !content.is_empty() {
        sections.push(render_markdown_inner(&content));
    }
    sections.join("\n\n")
}

fn render_markdown_inner(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut output = String::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Fenced code block
        if line.trim_start().starts_with("```") {
            let indent = line.len() - line.trim_start().len();
            let prefix = &line[..indent];
            let lang = line.trim_start().trim_start_matches('`').trim();
            if !lang.is_empty() {
                output.push_str(&format!("{prefix}{DIM}{CYAN}  {lang}{RESET}\n"));
            }
            i += 1;
            while i < lines.len() {
                let code_line = lines[i];
                if code_line.trim_start().starts_with("```") {
                    i += 1;
                    break;
                }
                output.push_str(&format!("{DIM}  {code_line}{RESET}\n"));
                i += 1;
            }
            continue;
        }

        // Table detection
        if line.contains('|') && is_table_start(&lines, i) {
            let table_end = find_table_end(&lines, i);
            let table_lines = &lines[i..table_end];
            output.push_str(&render_table(table_lines));
            i = table_end;
            continue;
        }

        // Horizontal rule
        if is_horizontal_rule(line) {
            output.push_str(&format!("{DIM}{}{RESET}\n", "─".repeat(48)));
            i += 1;
            continue;
        }

        // Header
        if let Some(rendered) = render_header(line) {
            output.push_str(&rendered);
            output.push('\n');
            i += 1;
            continue;
        }

        // Normal line
        output.push_str(&render_inline(line));
        output.push('\n');
        i += 1;
    }

    if output.ends_with('\n') {
        output.pop();
    }
    output
}

fn render_header(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    if trimmed.len() <= level || trimmed.as_bytes()[level] != b' ' {
        return None;
    }
    let content = &trimmed[level + 1..];
    let content = render_inline(content);
    Some(format!("{BOLD}{CYAN}{content}{RESET}"))
}

fn render_inline(text: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '`' {
            if let Some(end) = find_closing(&chars, i + 1, '`') {
                let code: String = chars[i + 1..end].iter().collect();
                result.push_str(&format!("{YELLOW}{code}{RESET}"));
                i = end + 1;
                continue;
            }
        }

        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_double_closing(&chars, i + 2, '*') {
                let inner: String = chars[i + 2..end].iter().collect();
                result.push_str(&format!("{BOLD}{inner}{RESET}"));
                i = end + 2;
                continue;
            }
        }

        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            if let Some(end) = find_closing(&chars, i + 1, '*') {
                let inner: String = chars[i + 1..end].iter().collect();
                result.push_str(&format!("{ITALIC}{inner}{RESET}"));
                i = end + 1;
                continue;
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

fn find_closing(chars: &[char], start: usize, marker: char) -> Option<usize> {
    for i in start..chars.len() {
        if chars[i] == marker {
            return Some(i);
        }
    }
    None
}

fn find_double_closing(chars: &[char], start: usize, marker: char) -> Option<usize> {
    let len = chars.len();
    if len < 2 {
        return None;
    }
    for i in start..len - 1 {
        if chars[i] == marker && chars[i + 1] == marker {
            return Some(i);
        }
    }
    None
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let ch = trimmed.chars().next().unwrap();
    (ch == '-' || ch == '*' || ch == '_') && trimmed.chars().all(|c| c == ch || c == ' ')
}

fn is_table_start(lines: &[&str], idx: usize) -> bool {
    if idx + 1 >= lines.len() {
        return false;
    }
    let next = lines[idx + 1].trim();
    next.contains('|')
        && next
            .chars()
            .all(|c| c == '|' || c == '-' || c == ':' || c == ' ')
}

fn find_table_end(lines: &[&str], start: usize) -> usize {
    let mut i = start;
    while i < lines.len() && lines[i].contains('|') {
        i += 1;
    }
    i
}

fn render_table(lines: &[&str]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut separator_idx: Option<usize> = None;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
        let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);

        if trimmed
            .chars()
            .all(|c| c == '-' || c == ':' || c == '|' || c == ' ')
            && trimmed.contains('-')
        {
            separator_idx = Some(idx);
            continue;
        }

        let cells: Vec<String> = trimmed.split('|').map(|s| s.trim().to_string()).collect();
        rows.push(cells);
    }

    if rows.is_empty() {
        return String::new();
    }

    // Pre-render all cells so we can measure their actual display width
    let rendered_rows: Vec<Vec<String>> = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            row.iter()
                .map(|cell| {
                    if idx == 0 && separator_idx.is_some() {
                        // Header: bold, no inline markdown processing
                        format!("{BOLD}{cell}{RESET}")
                    } else {
                        render_inline(cell)
                    }
                })
                .collect()
        })
        .collect();

    // Calculate column widths from rendered cells
    let col_count = rendered_rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut col_widths = vec![0usize; col_count];
    for row in &rendered_rows {
        for (j, cell) in row.iter().enumerate() {
            if j < col_count {
                let display_len = strip_ansi_len(cell);
                col_widths[j] = col_widths[j].max(display_len);
            }
        }
    }

    let mut output = String::new();

    output.push_str(&format!("{DIM}"));
    output.push_str(&table_border(&col_widths, '┌', '┬', '┐'));
    output.push_str(&format!("{RESET}\n"));

    for row in &rendered_rows {
        output.push_str(&format!("{DIM}│{RESET}"));
        for (j, width) in col_widths.iter().enumerate() {
            let cell = row.get(j).map(|s| s.as_str()).unwrap_or("");
            let display_len = strip_ansi_len(cell);
            let padding = width.saturating_sub(display_len);
            output.push_str(&format!(" {cell}"));
            output.push_str(&" ".repeat(padding));
            output.push_str(&format!(" {DIM}│{RESET}"));
        }
        output.push('\n');

        // After the first row (header), draw separator if present
        if std::ptr::eq(row, &rendered_rows[0]) && separator_idx.is_some() {
            output.push_str(&format!("{DIM}"));
            output.push_str(&table_border(&col_widths, '├', '┼', '┤'));
            output.push_str(&format!("{RESET}\n"));
        }
    }

    output.push_str(&format!("{DIM}"));
    output.push_str(&table_border(&col_widths, '└', '┴', '┘'));
    output.push_str(&format!("{RESET}\n"));

    output
}

fn table_border(widths: &[usize], left: char, mid: char, right: char) -> String {
    let mut s = String::new();
    s.push(left);
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < widths.len() {
            s.push(mid);
        }
    }
    s.push(right);
    s
}

fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else {
            if is_wide_char(c) {
                len += 2;
            } else {
                len += 1;
            }
        }
    }
    len
}

fn is_wide_char(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        0x1100..=0x115F |
        0x2E80..=0x303E |
        0x3041..=0x33BF |
        0x3400..=0x4DBF |
        0x4E00..=0x9FFF |
        0xA000..=0xA4CF |
        0xAC00..=0xD7AF |
        0xF900..=0xFAFF |
        0xFE30..=0xFE4F |
        0xFF01..=0xFF60 |
        0xFFE0..=0xFFE6 |
        0x20000..=0x2FA1F
    )
}

#[derive(Default)]
struct LineRenderer {
    in_code_block: bool,
    table_buffer: Vec<String>,
    in_table: bool,
}

impl LineRenderer {
    fn process_line(&mut self, line: &str) -> String {
        if line.trim_start().starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                return String::new();
            }

            self.in_code_block = true;
            let lang = line.trim_start().trim_start_matches('`').trim();
            if !lang.is_empty() {
                return format!("{DIM}{CYAN}  {lang}{RESET}\n");
            }
            return String::new();
        }

        if self.in_code_block {
            return format!("{DIM}  {line}{RESET}\n");
        }

        if line.contains('|') && !self.in_table {
            self.in_table = true;
            self.table_buffer.push(line.to_string());
            return String::new();
        }

        if self.in_table {
            if line.contains('|') {
                self.table_buffer.push(line.to_string());
                return String::new();
            }

            let mut output = self.flush();
            output.push_str(&self.render_single_line(line));
            return output;
        }

        self.render_single_line(line)
    }

    fn flush(&mut self) -> String {
        if !self.in_table {
            return String::new();
        }

        self.in_table = false;
        let lines: Vec<&str> = self.table_buffer.iter().map(|s| s.as_str()).collect();
        let result = render_table(&lines);
        self.table_buffer.clear();
        result
    }

    fn render_single_line(&self, line: &str) -> String {
        if is_horizontal_rule(line) {
            return format!("{DIM}{}{RESET}\n", "─".repeat(48));
        }
        if let Some(rendered) = render_header(line) {
            return format!("{rendered}\n");
        }
        format!("{}\n", render_inline(line))
    }
}

fn sticky_style(style: &str, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let persisted = text.replace(RESET, &format!("{RESET}{style}"));
    format!("{style}{persisted}{RESET}")
}

fn render_thinking_content(content: &str) -> String {
    let rendered = render_markdown_inner(content);
    sticky_style(DIM, &rendered)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPhase {
    Thinking,
    Answering,
}

impl StreamPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Answering => "answering",
        }
    }
}

/// Streaming-friendly markdown renderer with thinking tag support.
///
/// During streaming:
/// - `<think>` content is shown inline in dim style
/// - Phase changes are surfaced separately by the caller (`thinking` / `answering`)
/// - When collapsed, previewed thinking lines are erased without extra markers
pub struct StreamRenderer {
    buffer: String,
    normal_renderer: LineRenderer,
    thinking_renderer: LineRenderer,
    // Thinking state
    in_thinking: bool,
    thinking_content: String,
    thinking_lines_shown: usize,
    collapse_thinking: bool,
    phase_transitions: Vec<StreamPhase>,
    current_phase: Option<StreamPhase>,
    // Tag detection buffer
    tag_buffer: String,
}

impl StreamRenderer {
    pub fn new(collapse_thinking: bool) -> Self {
        Self {
            buffer: String::new(),
            normal_renderer: LineRenderer::default(),
            thinking_renderer: LineRenderer::default(),
            in_thinking: false,
            thinking_content: String::new(),
            thinking_lines_shown: 0,
            collapse_thinking,
            phase_transitions: Vec::new(),
            current_phase: None,
            tag_buffer: String::new(),
        }
    }

    /// Push a text delta, returns rendered output.
    pub fn push(&mut self, delta: &str) -> String {
        // Append to our working buffer, handling think tags
        self.tag_buffer.push_str(delta);
        let mut output = String::new();

        loop {
            if self.in_thinking {
                // Look for </think> closing tag
                if let Some(pos) = self.tag_buffer.find("</think>") {
                    // Content before closing tag is thinking content
                    let think_part = self.tag_buffer[..pos].to_string();
                    self.tag_buffer = self.tag_buffer[pos + 8..].to_string();

                    // Process remaining thinking lines
                    self.buffer.push_str(&think_part);
                    // Drain all complete lines from buffer
                    while let Some(nl) = self.buffer.find('\n') {
                        let line = self.buffer[..nl].to_string();
                        self.buffer = self.buffer[nl + 1..].to_string();
                        self.thinking_content.push_str(&line);
                        self.thinking_content.push('\n');
                        self.push_thinking_line(&mut output, &line);
                    }
                    // Flush remaining partial line
                    if !self.buffer.is_empty() {
                        let partial = std::mem::take(&mut self.buffer);
                        self.thinking_content.push_str(&partial);
                        self.push_thinking_line(&mut output, &partial);
                    }

                    // Collapse or keep thinking output
                    if self.collapse_thinking {
                        for _ in 0..self.thinking_lines_shown {
                            output.push_str(&format!("{CURSOR_UP}{CLEAR_LINE}"));
                        }
                    } else {
                        output.push_str(&self.flush_thinking_renderer());
                    }

                    // Save thinking content
                    save_thinking(self.thinking_content.trim());

                    self.in_thinking = false;
                    self.thinking_lines_shown = 0;
                    self.thinking_renderer = LineRenderer::default();
                    // Skip newline right after </think>
                    if self.tag_buffer.starts_with('\n') {
                        self.tag_buffer = self.tag_buffer[1..].to_string();
                    }
                    continue; // Process remaining content after </think>
                } else {
                    // Check for partial "</think" at end
                    let could_be_tag = could_be_partial_tag(&self.tag_buffer, "</think>");
                    if could_be_tag > 0 {
                        // Move safe content to buffer, keep potential tag suffix
                        let safe_end = self.tag_buffer.len() - could_be_tag;
                        let safe = self.tag_buffer[..safe_end].to_string();
                        self.tag_buffer = self.tag_buffer[safe_end..].to_string();
                        self.buffer.push_str(&safe);
                    } else {
                        let all = std::mem::take(&mut self.tag_buffer);
                        self.buffer.push_str(&all);
                    }

                    // Render buffered thinking lines (dimmed)
                    while let Some(nl) = self.buffer.find('\n') {
                        let line = self.buffer[..nl].to_string();
                        self.buffer = self.buffer[nl + 1..].to_string();
                        self.thinking_content.push_str(&line);
                        self.thinking_content.push('\n');
                        self.push_thinking_line(&mut output, &line);
                    }
                    break;
                }
            } else {
                // Not in thinking: look for <think> opening tag
                if let Some(pos) = self.tag_buffer.find("<think>") {
                    // Content before <think> is normal content
                    let before = self.tag_buffer[..pos].to_string();
                    self.tag_buffer = self.tag_buffer[pos + 7..].to_string();

                    // Process normal content
                    self.buffer.push_str(&before);
                    if !before.is_empty() {
                        self.note_phase(StreamPhase::Answering);
                    }
                    while let Some(nl) = self.buffer.find('\n') {
                        let line = self.buffer[..nl].to_string();
                        self.buffer = self.buffer[nl + 1..].to_string();
                        let rendered = self.normal_renderer.process_line(&line);
                        if !rendered.is_empty() {
                            self.note_phase(StreamPhase::Answering);
                            output.push_str(&rendered);
                        }
                    }

                    // Enter thinking mode
                    self.in_thinking = true;
                    self.thinking_content.clear();
                    self.thinking_lines_shown = 0;
                    self.thinking_renderer = LineRenderer::default();
                    self.note_phase(StreamPhase::Thinking);
                    // Skip newline right after <think>
                    if self.tag_buffer.starts_with('\n') {
                        self.tag_buffer = self.tag_buffer[1..].to_string();
                    }
                    continue; // Process content after <think>
                } else {
                    // Check for partial "<think" at end
                    let could_be_tag = could_be_partial_tag(&self.tag_buffer, "<think>");
                    if could_be_tag > 0 {
                        let safe_end = self.tag_buffer.len() - could_be_tag;
                        let safe = self.tag_buffer[..safe_end].to_string();
                        self.tag_buffer = self.tag_buffer[safe_end..].to_string();
                        if !safe.is_empty() {
                            self.note_phase(StreamPhase::Answering);
                        }
                        self.buffer.push_str(&safe);
                    } else {
                        let all = std::mem::take(&mut self.tag_buffer);
                        if !all.is_empty() {
                            self.note_phase(StreamPhase::Answering);
                        }
                        self.buffer.push_str(&all);
                    }

                    // Render buffered normal lines
                    while let Some(nl) = self.buffer.find('\n') {
                        let line = self.buffer[..nl].to_string();
                        self.buffer = self.buffer[nl + 1..].to_string();
                        let rendered = self.normal_renderer.process_line(&line);
                        if !rendered.is_empty() {
                            self.note_phase(StreamPhase::Answering);
                            output.push_str(&rendered);
                        }
                    }
                    break;
                }
            }
        }

        output
    }

    /// Flush remaining buffer content.
    pub fn flush(&mut self) -> String {
        let mut output = String::new();

        // Flush any remaining tag buffer
        if !self.tag_buffer.is_empty() {
            let remaining_tag = std::mem::take(&mut self.tag_buffer);
            self.buffer.push_str(&remaining_tag);
        }

        // If still in thinking, collapse it
        if self.in_thinking {
            // Flush remaining buffer as thinking
            if !self.buffer.is_empty() {
                let partial = std::mem::take(&mut self.buffer);
                self.thinking_content.push_str(&partial);
                self.push_thinking_line(&mut output, &partial);
            }
            // Collapse if configured
            if self.collapse_thinking {
                for _ in 0..self.thinking_lines_shown {
                    output.push_str(&format!("{CURSOR_UP}{CLEAR_LINE}"));
                }
            } else {
                output.push_str(&self.flush_thinking_renderer());
            }
            save_thinking(self.thinking_content.trim());
            self.in_thinking = false;
            self.thinking_lines_shown = 0;
            self.thinking_renderer = LineRenderer::default();
        }

        // Flush table if pending
        let table_output = self.normal_renderer.flush();
        if !table_output.is_empty() {
            self.note_phase(StreamPhase::Answering);
            output.push_str(&table_output);
        }

        // Flush remaining text
        if !self.buffer.is_empty() {
            let remaining = std::mem::take(&mut self.buffer);
            let rendered = render_inline(&remaining);
            if !rendered.is_empty() {
                self.note_phase(StreamPhase::Answering);
                output.push_str(&rendered);
            }
        }

        output
    }

    fn push_thinking_line(&mut self, output: &mut String, line: &str) {
        let rendered = self.thinking_renderer.process_line(line);
        self.push_thinking_rendered(output, &rendered);
    }

    fn flush_thinking_renderer(&mut self) -> String {
        let rendered = self.thinking_renderer.flush();
        let mut output = String::new();
        self.push_thinking_rendered(&mut output, &rendered);
        output
    }

    fn push_thinking_rendered(&mut self, output: &mut String, rendered: &str) {
        if !rendered.is_empty() {
            output.push_str(&sticky_style(DIM, rendered));
            self.thinking_lines_shown += rendered.lines().count();
        }
    }

    pub fn drain_phase_transitions(&mut self) -> Vec<StreamPhase> {
        std::mem::take(&mut self.phase_transitions)
    }

    fn note_phase(&mut self, phase: StreamPhase) {
        if self.current_phase != Some(phase) {
            self.phase_transitions.push(phase);
            self.current_phase = Some(phase);
        }
    }
}

/// Check how many characters at the end of `text` could be
/// the start of `tag` (partial match).
fn could_be_partial_tag(text: &str, tag: &str) -> usize {
    let text_bytes = text.as_bytes();
    let tag_bytes = tag.as_bytes();
    for len in (1..tag_bytes.len()).rev() {
        if text_bytes.len() >= len && &text_bytes[text_bytes.len() - len..] == &tag_bytes[..len] {
            return len;
        }
    }
    0
}

// ─── Spinner + Status Bar ───

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Print a status bar line to stderr.
pub fn print_status_bar(provider: &str, model: &str, session_id: &str) {
    let badge = if is_temp_session(session_id) {
        format!("{YELLOW}[temp]{RESET}")
    } else {
        format!("{DIM}[saved]{RESET}")
    };
    eprintln!(
        "{DIM}{provider}{RESET} {BOLD}{CYAN}{model}{RESET} {badge} {DIM}{}{RESET}",
        short_id(session_id)
    );
}

pub fn print_stream_phase(phase: StreamPhase) {
    eprintln!("{DIM}{}{RESET}", phase.label());
}

/// A loading spinner that runs on a background thread.
/// Call `stop()` when the first token arrives.
pub struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub fn start(message: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let msg = message.to_string();
        let handle = std::thread::spawn(move || {
            let mut i = 0;
            let mut stderr = io::stderr();
            while r.load(Ordering::Relaxed) {
                let frame = SPINNER_FRAMES[i % SPINNER_FRAMES.len()];
                let _ = write!(stderr, "\r{DIM}{frame} {msg}{RESET}  ");
                let _ = stderr.flush();
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            // Clear spinner line
            let _ = write!(stderr, "\r{CLEAR_LINE}");
            let _ = stderr.flush();
        });
        Self {
            running,
            handle: Some(handle),
        }
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_markdown_keeps_thinking_without_markers() {
        let rendered = render_markdown("answer\n<think>\nhello\n</think>\nworld", false);
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("world"));
        assert!(!rendered.contains("Thinking"));
        assert!(!rendered.contains("╭"));
    }

    #[test]
    fn render_markdown_renders_fenced_code_inside_thinking() {
        let rendered = render_markdown(
            "<think>\n```txt\nThis is the overwritten content using overwrite mode.\nLine 2\nLine 3\n```\n</think>\nDone",
            false,
        );
        assert!(rendered.contains("txt"));
        assert!(rendered.contains("This is the overwritten content using overwrite mode."));
        assert!(!rendered.contains("```"));
    }

    #[test]
    fn stream_renderer_reports_phase_transitions() {
        let mut renderer = StreamRenderer::new(false);
        let rendered = renderer.push("<think>\nplan\n</think>\nanswer");
        let phases = renderer.drain_phase_transitions();
        assert_eq!(phases, vec![StreamPhase::Thinking, StreamPhase::Answering]);
        assert!(rendered.contains("plan"));
        assert!(!rendered.contains("Thinking"));
    }
}
