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
use std::path::{Path, PathBuf};

// ANSI escape codes
const BOLD: &str = "\x1b[1m";
const BLUE: &str = "\x1b[34m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";
const RESET: &str = "\x1b[0m";
const STRIKETHROUGH: &str = "\x1b[9m";
const UNDERLINE: &str = "\x1b[4m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
#[allow(dead_code)]
const GREEN: &str = "\x1b[32m";
#[allow(dead_code)]
const MAGENTA: &str = "\x1b[35m";

// ANSI cursor control
const CLEAR_LINE: &str = "\x1b[2K";
const CURSOR_UP: &str = "\x1b[A";
const SAVE_CURSOR: &str = "\x1b7";
const RESTORE_CURSOR: &str = "\x1b8";
const DELETE_LINE: &str = "\x1b[M";

/// Path to store the last thinking content.
pub fn thinking_file_path() -> PathBuf {
    thinking_file_path_in(&data_dir_or_default())
}

fn thinking_file_path_in(root: &Path) -> PathBuf {
    root.join(".last_thinking")
}

fn data_dir_or_default() -> PathBuf {
    if let Some(dir) = dirs::data_dir() {
        dir.join("chat-cli")
    } else {
        PathBuf::from(".local/share/chat-cli")
    }
}

fn config_dir_or_default() -> PathBuf {
    if let Some(dir) = dirs::config_dir() {
        dir.join("chat-cli")
    } else {
        PathBuf::from(".config/chat-cli")
    }
}

fn legacy_thinking_file_path() -> PathBuf {
    thinking_file_path_in(&config_dir_or_default())
}

fn migrate_legacy_thinking_file(path: &Path, legacy_path: &Path) {
    if path.exists() || !legacy_path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::rename(legacy_path, path).is_ok() {
        return;
    }
    if let Ok(content) = std::fs::read_to_string(legacy_path)
        && std::fs::write(path, content).is_ok()
    {
        let _ = std::fs::remove_file(legacy_path);
    }
}

/// Save thinking content to the persistent file.
pub fn save_thinking(content: &str) {
    let path = thinking_file_path();
    migrate_legacy_thinking_file(&path, &legacy_thinking_file_path());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, content);
}

/// Load the last saved thinking content.
pub fn load_thinking() -> Option<String> {
    let path = thinking_file_path();
    migrate_legacy_thinking_file(&path, &legacy_thinking_file_path());
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
            let lang = line.trim_start().trim_start_matches('`').trim();
            let normalized_lang = normalize_code_lang(lang);
            i += 1;
            while i < lines.len() {
                let code_line = lines[i];
                if code_line.trim_start().starts_with("```") {
                    i += 1;
                    break;
                }
                output.push_str(&render_code_block_line_with_lang(
                    code_line,
                    normalized_lang,
                ));
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

        output.push_str(&render_single_markdown_line(line));
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

        if chars[i] == '[' {
            if i + 1 < len && chars[i + 1] == '^' {
                if let Some(end) = find_closing(&chars, i + 2, ']') {
                    let label: String = chars[i + 2..end].iter().collect();
                    result.push_str(&render_footnote_marker(&label));
                    i = end + 1;
                    continue;
                }
            } else if let Some(label_end) = find_closing(&chars, i + 1, ']')
                && chars.get(label_end + 1) == Some(&'(')
                && let Some(url_end) = find_closing(&chars, label_end + 2, ')')
            {
                let label: String = chars[i + 1..label_end].iter().collect();
                let url: String = chars[label_end + 2..url_end].iter().collect();
                result.push_str(&render_link(&label, &url));
                i = url_end + 1;
                continue;
            }
        }

        if i + 2 < len && chars[i] == '*' && chars[i + 1] == '*' && chars[i + 2] == '*' {
            if let Some(end) = find_triple_closing(&chars, i + 3, '*') {
                let inner: String = chars[i + 3..end].iter().collect();
                result.push_str(&format!("{BOLD}{ITALIC}{inner}{RESET}"));
                i = end + 3;
                continue;
            }
        }

        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            if let Some(end) = find_double_closing(&chars, i + 2, '~') {
                let inner: String = chars[i + 2..end].iter().collect();
                result.push_str(&format!("{STRIKETHROUGH}{inner}{RESET}"));
                i = end + 2;
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

fn render_list_item(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    if let Some(content) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        if let Some(rendered) = render_task_list_item(indent, None, content) {
            return Some(rendered);
        }
        return Some(format!("{indent}{DIM}•{RESET} {}", render_inline(content)));
    }

    let digit_count = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0
        && trimmed.as_bytes().get(digit_count) == Some(&b'.')
        && trimmed.as_bytes().get(digit_count + 1) == Some(&b' ')
    {
        let marker = &trimmed[..digit_count + 1];
        let content = &trimmed[digit_count + 2..];
        if let Some(rendered) = render_task_list_item(indent, Some(marker), content) {
            return Some(rendered);
        }
        return Some(format!(
            "{indent}{DIM}{marker}{RESET} {}",
            render_inline(content)
        ));
    }

    None
}

fn render_blockquote_line(line: &str) -> Option<String> {
    let (indent, depth, content) = parse_blockquote_line(line)?;

    let rendered = if content.is_empty() {
        String::new()
    } else {
        sticky_style(DIM, &render_single_markdown_line(content))
    };
    let bars = render_blockquote_bars(depth);

    if rendered.is_empty() {
        Some(format!("{indent}{bars}"))
    } else {
        Some(format!("{indent}{bars} {rendered}"))
    }
}

fn render_single_markdown_line(line: &str) -> String {
    if let Some(rendered) = render_blockquote_line(line) {
        return rendered;
    }
    if let Some(rendered) = render_footnote_definition(line) {
        return rendered;
    }
    if let Some(rendered) = render_header(line) {
        return rendered;
    }
    if let Some(rendered) = render_list_item(line) {
        return rendered;
    }
    render_inline(line)
}

fn render_task_list_item(
    indent: &str,
    ordered_marker: Option<&str>,
    content: &str,
) -> Option<String> {
    let (checked, rest) = parse_task_marker(content)?;
    let marker = if checked {
        format!("{GREEN}☑{RESET}")
    } else {
        format!("{DIM}☐{RESET}")
    };
    let rendered = render_inline(rest);
    Some(match ordered_marker {
        Some(prefix) => format!("{indent}{DIM}{prefix}{RESET} {marker} {rendered}"),
        None => format!("{indent}{marker} {rendered}"),
    })
}

fn render_link(label: &str, url: &str) -> String {
    let rendered_label = sticky_style(&format!("{UNDERLINE}{CYAN}"), &render_inline(label));
    if label == url || rendered_label.is_empty() {
        return rendered_label;
    }
    format!("{rendered_label} {DIM}{CYAN}<{url}>{RESET}")
}

fn render_code_block_line_with_lang(code_line: &str, lang: &str) -> String {
    let highlighted = highlight_code_content(lang, code_line);
    format!("{}\n", sticky_style(DIM, &highlighted))
}

fn parse_task_marker(content: &str) -> Option<(bool, &str)> {
    if let Some(rest) = content
        .strip_prefix("[x] ")
        .or_else(|| content.strip_prefix("[X] "))
    {
        return Some((true, rest));
    }
    if let Some(rest) = content.strip_prefix("[ ] ") {
        return Some((false, rest));
    }
    None
}

fn render_footnote_definition(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let body = trimmed.strip_prefix("[^")?;
    let (label, content) = body.split_once("]:")?;
    let rendered_content = sticky_style(DIM, &render_inline(content.trim_start()));
    if rendered_content.is_empty() {
        Some(format!("{indent}{}", render_footnote_marker(label)))
    } else {
        Some(format!(
            "{indent}{} {rendered_content}",
            render_footnote_marker(label)
        ))
    }
}

fn render_footnote_marker(label: &str) -> String {
    let display = footnote_display_label(label);
    if label.chars().all(|c| c.is_ascii_digit()) {
        format!("{DIM}{CYAN}⁽{display}⁾{RESET}")
    } else {
        format!("{DIM}{CYAN}[^{display}]{RESET}")
    }
}

fn parse_blockquote_line(line: &str) -> Option<(&str, usize, &str)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('>') {
        return None;
    }
    let indent = &line[..line.len() - trimmed.len()];
    let mut depth = 0usize;
    let mut content = trimmed;
    while let Some(rest) = content.strip_prefix('>') {
        depth += 1;
        content = rest.trim_start();
    }
    Some((indent, depth, content))
}

fn render_blockquote_bars(depth: usize) -> String {
    (0..depth)
        .map(|_| format!("{BLUE}│{RESET}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn footnote_display_label(label: &str) -> String {
    if label.chars().all(|c| c.is_ascii_digit()) {
        return label.chars().map(to_superscript_digit).collect();
    }
    label.to_string()
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

fn find_triple_closing(chars: &[char], start: usize, marker: char) -> Option<usize> {
    let len = chars.len();
    if len < 3 {
        return None;
    }
    for i in start..len - 2 {
        if chars[i] == marker && chars[i + 1] == marker && chars[i + 2] == marker {
            return Some(i);
        }
    }
    None
}

fn to_superscript_digit(ch: char) -> char {
    match ch {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        _ => ch,
    }
}

fn highlight_code_content(lang: &str, line: &str) -> String {
    let normalized = normalize_code_lang(lang);
    if normalized.is_empty() {
        return line.to_string();
    }

    let mut output = String::new();
    let mut index = 0usize;

    while index < line.len() {
        if line_comment_prefix_at(line, index, normalized).is_some() {
            output.push_str(&format!("{GREEN}{}{RESET}", &line[index..]));
            break;
        }

        let ch = line[index..].chars().next().unwrap();
        let ch_len = ch.len_utf8();

        if ch == '"' || ch == '\'' {
            let end = string_end_offset(line, index, ch);
            output.push_str(&format!("{YELLOW}{}{RESET}", &line[index..end]));
            index = end;
            continue;
        }

        if ch.is_ascii_digit() {
            let end = number_end_offset(line, index);
            output.push_str(&format!("{MAGENTA}{}{RESET}", &line[index..end]));
            index = end;
            continue;
        }

        if is_identifier_start(ch) {
            let end = identifier_end_offset(line, index);
            let token = &line[index..end];
            if keyword_set(normalized).contains(&token) {
                output.push_str(&format!("{BOLD}{CYAN}{token}{RESET}"));
            } else if boolean_literal_set(normalized).contains(&token) {
                output.push_str(&format!("{BOLD}{MAGENTA}{token}{RESET}"));
            } else {
                output.push_str(token);
            }
            index = end;
            continue;
        }

        output.push(ch);
        index += ch_len;
    }

    output
}

fn normalize_code_lang(lang: &str) -> &str {
    lang.split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()
        .unwrap_or("")
        .trim()
}

fn line_comment_prefix_at<'a>(line: &'a str, index: usize, lang: &str) -> Option<&'a str> {
    let rest = &line[index..];
    comment_prefixes(lang)
        .iter()
        .copied()
        .find(|prefix| rest.starts_with(prefix))
}

fn comment_prefixes(lang: &str) -> &'static [&'static str] {
    match lang {
        "rs" | "rust" | "js" | "jsx" | "ts" | "tsx" | "c" | "cpp" | "cc" | "cxx" | "go"
        | "java" | "kt" | "kotlin" | "swift" | "scala" => &["//"],
        "py" | "python" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "rb" | "ruby"
        | "perl" | "pl" | "ini" | "conf" => &["#"],
        "sql" | "lua" => &["--"],
        _ => &[],
    }
}

fn keyword_set(lang: &str) -> &'static [&'static str] {
    match lang {
        "rs" | "rust" => &[
            "fn", "let", "mut", "pub", "impl", "struct", "enum", "trait", "use", "mod", "match",
            "if", "else", "loop", "while", "for", "in", "return", "async", "await", "const",
            "static", "crate", "super", "Self", "self",
        ],
        "js" | "jsx" | "ts" | "tsx" => &[
            "function", "const", "let", "var", "return", "if", "else", "for", "while", "switch",
            "case", "break", "continue", "class", "new", "import", "from", "export", "async",
            "await", "try", "catch", "finally",
        ],
        "py" | "python" => &[
            "def", "class", "import", "from", "return", "if", "elif", "else", "for", "while", "in",
            "async", "await", "with", "as", "try", "except", "finally", "lambda", "pass",
        ],
        "sh" | "bash" | "zsh" => &[
            "if", "then", "else", "fi", "for", "do", "done", "case", "esac", "function", "local",
            "export", "in",
        ],
        "toml" => &[],
        "json" => &[],
        "yaml" | "yml" => &[],
        _ => &[],
    }
}

fn boolean_literal_set(lang: &str) -> &'static [&'static str] {
    match lang {
        "rs" | "rust" => &["true", "false", "None", "Some", "Ok", "Err"],
        "js" | "jsx" | "ts" | "tsx" => &["true", "false", "null", "undefined"],
        "py" | "python" => &["True", "False", "None"],
        "toml" | "yaml" | "yml" => &["true", "false"],
        "json" => &["true", "false", "null"],
        "sh" | "bash" | "zsh" => &["true", "false"],
        _ => &[],
    }
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn identifier_end_offset(line: &str, start: usize) -> usize {
    let mut index = start;
    while index < line.len() {
        let ch = line[index..].chars().next().unwrap();
        if !is_identifier_continue(ch) {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn number_end_offset(line: &str, start: usize) -> usize {
    let mut index = start;
    while index < line.len() {
        let ch = line[index..].chars().next().unwrap();
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | 'x' | 'X')) {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn string_end_offset(line: &str, start: usize, quote: char) -> usize {
    let mut escaped = false;
    let mut index = start + quote.len_utf8();
    while index < line.len() {
        let ch = line[index..].chars().next().unwrap();
        index += ch.len_utf8();
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            break;
        }
    }
    index
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
    code_block_lang: String,
    table_buffer: Vec<String>,
    in_table: bool,
}

impl LineRenderer {
    fn process_line(&mut self, line: &str) -> String {
        if line.trim_start().starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                self.code_block_lang.clear();
                return String::new();
            }

            self.in_code_block = true;
            let lang = line.trim_start().trim_start_matches('`').trim();
            self.code_block_lang = normalize_code_lang(lang).to_string();
            return String::new();
        }

        if self.in_code_block {
            return render_code_block_line_with_lang(line, &self.code_block_lang);
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

    fn can_render_partial_inline(&self, partial: &str) -> bool {
        !self.in_code_block
            && !self.in_table
            && !partial.trim_start().starts_with("```")
            && !partial.trim_start().starts_with('>')
    }

    fn render_single_line(&self, line: &str) -> String {
        if is_horizontal_rule(line) {
            return format!("{DIM}{}{RESET}\n", "─".repeat(48));
        }
        format!("{}\n", render_single_markdown_line(line))
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
    Waiting,
    Thinking,
    Answering,
}

impl StreamPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
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
                    self.note_phase(StreamPhase::Answering);
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
                    // Don't output partial lines — wait for complete lines
                    // so markdown markers like **bold** aren't fragmented across chunks
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
                    // Don't output partial lines — wait for complete lines
                    // so markdown markers like **bold** aren't fragmented across chunks
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
            self.note_phase(StreamPhase::Answering);
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
            let rendered = self.normal_renderer.render_single_line(&remaining);
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

    fn push_partial_thinking_inline(&mut self, output: &mut String) {
        if self.collapse_thinking
            || self.buffer.is_empty()
            || !self
                .thinking_renderer
                .can_render_partial_inline(&self.buffer)
        {
            return;
        }

        let partial = std::mem::take(&mut self.buffer);
        self.thinking_content.push_str(&partial);
        let rendered = render_inline(&partial);
        if !rendered.is_empty() {
            output.push_str(&sticky_style(DIM, &rendered));
        }
    }

    fn push_partial_normal_inline(&mut self, output: &mut String) {
        if self.buffer.is_empty() || !self.normal_renderer.can_render_partial_inline(&self.buffer) {
            return;
        }

        let partial = std::mem::take(&mut self.buffer);
        let rendered = render_inline(&partial);
        if !rendered.is_empty() {
            self.note_phase(StreamPhase::Answering);
            output.push_str(&rendered);
        }
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

// ─── Stream Status Bar ───

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const WHITE: &str = "\x1b[37m";
const BRIGHT_WHITE: &str = "\x1b[97m";

pub fn format_status_bar(provider: &str, model: &str, session_id: &str) -> String {
    let badge = if is_temp_session(session_id) {
        format!("{YELLOW}[temp]{RESET}")
    } else {
        format!("{DIM}[saved]{RESET}")
    };
    format!(
        "{DIM}{provider}{RESET} {BOLD}{CYAN}{model}{RESET} {badge} {DIM}{}{RESET}",
        short_id(session_id)
    )
}

/// Print the static message bar shown above streaming output.
pub fn print_status_bar(provider: &str, model: &str, session_id: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    writeln!(stdout, "{}", format_status_bar(provider, model, session_id))?;
    stdout.flush()
}

#[derive(Debug, Clone, Copy)]
struct StatusFrame {
    phase: StreamPhase,
    tick: usize,
    inline: bool,
}

#[derive(Debug)]
struct StreamStatusState {
    phase: StreamPhase,
    tick: usize,
    has_output: bool,
}

fn status_frame(state: &StreamStatusState) -> StatusFrame {
    StatusFrame {
        phase: state.phase,
        tick: state.tick,
        inline: !state.has_output,
    }
}

fn animated_phase_label(phase: StreamPhase, tick: usize) -> String {
    let chars: Vec<char> = phase.label().chars().collect();
    if chars.is_empty() {
        return String::new();
    }

    let lead = tick % chars.len();
    let trail = if lead == 0 { chars.len() - 1 } else { lead - 1 };

    let mut output = String::new();
    for (idx, ch) in chars.iter().enumerate() {
        if idx == lead {
            output.push_str(BRIGHT_WHITE);
            output.push_str(BOLD);
        } else if idx == trail {
            output.push_str(WHITE);
        } else {
            output.push_str(DIM);
        }
        output.push(*ch);
        output.push_str(RESET);
    }

    output
}

fn draw_status_footer<W: Write>(writer: &mut W, frame: StatusFrame) -> io::Result<()> {
    if frame.inline {
        write!(
            writer,
            "{SAVE_CURSOR}\r{CLEAR_LINE}\r{}{RESTORE_CURSOR}",
            animated_phase_label(frame.phase, frame.tick)
        )
    } else {
        write!(
            writer,
            "{SAVE_CURSOR}\r\n{CLEAR_LINE}\r{}{RESTORE_CURSOR}",
            animated_phase_label(frame.phase, frame.tick)
        )
    }
}

fn clear_status_footer<W: Write>(writer: &mut W, frame: StatusFrame) -> io::Result<()> {
    if frame.inline {
        write!(writer, "{SAVE_CURSOR}\r{CLEAR_LINE}\r{RESTORE_CURSOR}")
    } else {
        write!(
            writer,
            "{SAVE_CURSOR}\r\n{CLEAR_LINE}\r{DELETE_LINE}{RESTORE_CURSOR}"
        )
    }
}

/// Animated footer fixed to the last terminal line while streaming.
pub struct StreamStatus {
    state: Arc<Mutex<StreamStatusState>>,
    write_lock: Arc<Mutex<()>>,
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StreamStatus {
    pub fn start(initial_phase: StreamPhase) -> Self {
        let state = Arc::new(Mutex::new(StreamStatusState {
            phase: initial_phase,
            tick: 0,
            has_output: false,
        }));
        let write_lock = Arc::new(Mutex::new(()));
        let running = Arc::new(AtomicBool::new(true));
        let thread_state = state.clone();
        let thread_write_lock = write_lock.clone();
        let thread_running = running.clone();
        let handle = std::thread::spawn(move || {
            while thread_running.load(Ordering::Relaxed) {
                let frame = {
                    let mut state = thread_state.lock().unwrap();
                    state.tick = state.tick.wrapping_add(1);
                    status_frame(&state)
                };

                // Only draw when inline (no content output yet).
                // Non-inline footer uses \r\n which breaks with terminal scrolling.
                if frame.inline {
                    let _guard = thread_write_lock.lock().unwrap();
                    let mut stdout = io::stdout();
                    let _ = draw_status_footer(&mut stdout, frame);
                    let _ = stdout.flush();
                }

                std::thread::sleep(std::time::Duration::from_millis(90));
            }
        });

        let status = Self {
            state,
            write_lock,
            running,
            handle: Some(handle),
        };
        let _ = status.redraw();
        status
    }

    pub fn set_phase(&self, phase: StreamPhase) -> io::Result<()> {
        let frame = {
            let mut state = self.state.lock().unwrap();
            if state.phase != phase {
                state.phase = phase;
                state.tick = 0;
            }
            status_frame(&state)
        };
        // Only redraw inline; once content is flowing, skip to avoid scroll artifacts
        if frame.inline {
            self.redraw_frame(frame)
        } else {
            Ok(())
        }
    }

    pub fn write_output(&self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        let previous = self.snapshot();
        {
            let mut state = self.state.lock().unwrap();
            state.has_output = true;
        }

        let _guard = self.write_lock.lock().unwrap();
        let mut stdout = io::stdout();
        // Only clear inline footer; non-inline was never drawn
        if previous.inline {
            clear_status_footer(&mut stdout, previous)?;
        }
        write!(stdout, "{text}")?;
        // Don't redraw footer — content itself is the visual feedback
        stdout.flush()
    }

    pub fn stop(&mut self) -> io::Result<()> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let frame = self.snapshot();
        // Only clear inline footer; non-inline was never drawn
        if frame.inline {
            let _guard = self.write_lock.lock().unwrap();
            let mut stdout = io::stdout();
            clear_status_footer(&mut stdout, frame)?;
            stdout.flush()
        } else {
            Ok(())
        }
    }

    fn redraw(&self) -> io::Result<()> {
        self.redraw_frame(self.snapshot())
    }

    fn redraw_frame(&self, frame: StatusFrame) -> io::Result<()> {
        let _guard = self.write_lock.lock().unwrap();
        let mut stdout = io::stdout();
        draw_status_footer(&mut stdout, frame)?;
        stdout.flush()
    }

    fn snapshot(&self) -> StatusFrame {
        let state = self.state.lock().unwrap();
        status_frame(&state)
    }
}

impl Drop for StreamStatus {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
        assert!(rendered.contains("This is the overwritten content using overwrite mode."));
        assert!(rendered.contains("Line "));
        assert!(rendered.contains(&format!("{MAGENTA}2{RESET}")));
        assert!(!rendered.contains("```"));
    }

    #[test]
    fn render_markdown_renders_markdown_list_items() {
        let rendered = render_markdown("- *第一项*\n1. 第二项", false);
        assert!(rendered.contains("•"));
        assert!(rendered.contains("第一项"));
        assert!(rendered.contains("1."));
        assert!(!rendered.contains("- *"));
    }

    #[test]
    fn render_markdown_renders_task_list_items_with_checkboxes() {
        let rendered = render_markdown("- [x] 已完成任务\n- [ ] 未完成任务", false);
        assert!(rendered.contains("☑"));
        assert!(rendered.contains("☐"));
        assert!(rendered.contains("已完成任务"));
        assert!(rendered.contains("未完成任务"));
        assert!(!rendered.contains("[x]"));
        assert!(!rendered.contains("[ ]"));
    }

    #[test]
    fn render_markdown_renders_strikethrough() {
        let rendered = render_markdown("~~删除线文本~~", false);
        assert!(rendered.contains(&format!("{STRIKETHROUGH}删除线文本{RESET}")));
        assert!(!rendered.contains("~~删除线文本~~"));
    }

    #[test]
    fn render_markdown_renders_blockquotes_with_blue_bar() {
        let rendered = render_markdown("> 这是一段引用文本。\n> 可以包含多行。", false);
        assert!(rendered.contains(&format!("{BLUE}│{RESET}")));
        assert!(rendered.contains("这是一段引用文本。"));
        assert!(rendered.contains("可以包含多行。"));
        assert!(!rendered.contains("> 这是一段引用文本。"));
    }

    #[test]
    fn render_markdown_renders_nested_blockquotes_with_multiple_bars() {
        let rendered = render_markdown(">> 第二层引用", false);
        assert!(rendered.contains(&format!("{BLUE}│{RESET} {BLUE}│{RESET}")));
        assert!(rendered.contains("第二层引用"));
        assert!(!rendered.contains(">> 第二层引用"));
    }

    #[test]
    fn render_markdown_renders_bold_italic_inside_blockquote() {
        let rendered = render_markdown("> ***粗斜体***", false);
        assert!(rendered.contains(&format!("{BLUE}│{RESET}")));
        assert!(rendered.contains(&format!("{BOLD}{ITALIC}粗斜体{RESET}")));
        assert!(!rendered.contains("***粗斜体***"));
    }

    #[test]
    fn render_markdown_renders_footnote_references_and_definitions() {
        let rendered = render_markdown("这是一段带有脚注的文本[^1]\n\n[^1]: 这是脚注内容", false);
        assert!(rendered.contains("这是一段带有脚注的文本"));
        assert!(rendered.contains("这是脚注内容"));
        assert!(rendered.contains("⁽¹⁾"));
        assert!(!rendered.contains("[^1]"));
        assert!(!rendered.contains("[^1]:"));
    }

    #[test]
    fn render_markdown_renders_links_with_styled_label_and_url() {
        let rendered = render_markdown("[OpenAI](https://openai.com)", false);
        assert!(rendered.contains("OpenAI"));
        assert!(rendered.contains("<https://openai.com>"));
        assert!(!rendered.contains("[OpenAI](https://openai.com)"));
    }

    #[test]
    fn render_markdown_renders_code_block_with_gutter() {
        let rendered = render_markdown("```rs\nfn main() {}\n```", false);
        assert!(!rendered.contains("▎"));
        assert!(rendered.contains(&format!("{BOLD}{CYAN}fn{RESET}")));
        assert!(rendered.contains("main() {}"));
    }

    #[test]
    fn render_markdown_renders_basic_code_highlighting() {
        let rendered = render_markdown(
            "```rs\nfn main() { let value = \"hi\"; // note\n}\n```",
            false,
        );
        assert!(rendered.contains(&format!("{BOLD}{CYAN}fn{RESET}")));
        assert!(rendered.contains(&format!("{BOLD}{CYAN}let{RESET}")));
        assert!(rendered.contains(&format!("{YELLOW}\"hi\"")));
        assert!(rendered.contains(&format!("{GREEN}// note")));
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

    #[test]
    fn stream_renderer_buffers_partial_answer_without_newline() {
        let mut renderer = StreamRenderer::new(false);
        let rendered = renderer.push("hello");
        assert!(rendered.is_empty(), "partial line should be buffered");
        let flushed = renderer.flush();
        assert!(
            flushed.contains("hello"),
            "flush should output buffered content"
        );
    }

    #[test]
    fn stream_renderer_buffers_partial_thinking_without_newline() {
        let mut renderer = StreamRenderer::new(false);
        let rendered = renderer.push("<think>plan");
        assert!(rendered.is_empty(), "partial thinking should be buffered");
        let flushed = renderer.flush();
        assert!(
            flushed.contains("plan"),
            "flush should output thinking content"
        );
    }

    #[test]
    fn stream_renderer_flushes_blockquote_with_blue_bar() {
        let mut renderer = StreamRenderer::new(false);
        let rendered = renderer.push("> 这是引用");
        assert!(rendered.is_empty(), "partial quote should be buffered");
        let flushed = renderer.flush();
        assert!(flushed.contains(&format!("{BLUE}│{RESET}")));
        assert!(flushed.contains("这是引用"));
        assert!(!flushed.contains("> 这是引用"));
    }

    #[test]
    fn stream_renderer_renders_code_block_with_gutter() {
        let mut renderer = StreamRenderer::new(false);
        let rendered = renderer.push("```rs\nfn main() {}\n```\n");
        assert!(!rendered.contains("▎"));
        assert!(rendered.contains(&format!("{BOLD}{CYAN}fn{RESET}")));
        assert!(rendered.contains("main() {}"));
    }

    #[test]
    fn stream_renderer_renders_basic_code_highlighting() {
        let mut renderer = StreamRenderer::new(false);
        let rendered = renderer.push("```rs\nfn main() { let value = \"hi\"; }\n```\n");
        assert!(rendered.contains(&format!("{BOLD}{CYAN}fn{RESET}")));
        assert!(rendered.contains(&format!("{BOLD}{CYAN}let{RESET}")));
        assert!(rendered.contains(&format!("{YELLOW}\"hi\"")));
    }

    #[test]
    fn format_status_bar_marks_temp_and_saved_sessions() {
        let saved = format_status_bar("deepseek", "deepseek-chat", "sess_01ABCDEF12345678");
        let temp = format_status_bar("deepseek", "deepseek-chat", "tmp_01ABCDEF12345678");
        assert!(saved.contains("[saved]"));
        assert!(temp.contains("[temp]"));
        assert!(saved.contains("01ABCDEF"));
        assert!(temp.contains("tmp_01ABCDEF"));
    }

    #[test]
    fn animated_phase_label_moves_highlight() {
        let first = animated_phase_label(StreamPhase::Thinking, 0);
        let second = animated_phase_label(StreamPhase::Thinking, 1);
        assert_ne!(first, second);
        let plain_first = first
            .replace(BRIGHT_WHITE, "")
            .replace(WHITE, "")
            .replace(BOLD, "")
            .replace(DIM, "")
            .replace(RESET, "");
        let plain_second = second
            .replace(BRIGHT_WHITE, "")
            .replace(WHITE, "")
            .replace(BOLD, "")
            .replace(DIM, "")
            .replace(RESET, "");
        assert_eq!(plain_first, "thinking");
        assert_eq!(plain_second, "thinking");
    }

    #[test]
    fn migrate_legacy_thinking_file_moves_config_file_to_data_dir() {
        let base = std::env::temp_dir().join(format!("chat-cli-render-test-{}", ulid::Ulid::new()));
        let config_dir = base.join("config");
        let data_dir = base.join("data");
        let legacy_path = thinking_file_path_in(&config_dir);
        let current_path = thinking_file_path_in(&data_dir);

        fs::create_dir_all(&config_dir).unwrap();
        fs::write(&legacy_path, "legacy thinking").unwrap();
        migrate_legacy_thinking_file(&current_path, &legacy_path);

        assert_eq!(
            fs::read_to_string(&current_path).unwrap(),
            "legacy thinking"
        );
        assert!(!legacy_path.exists());

        let _ = fs::remove_dir_all(base);
    }
}
