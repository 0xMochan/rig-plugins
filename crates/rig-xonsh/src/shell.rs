//! Literal argv and pipelines for explicit subprocess forms and command blocks.

use crate::frontend::{at, expression_end, next_char, string_end};

enum Word<'source> {
    Literal(String),
    Splice(&'source str),
}

#[derive(Clone, Copy)]
enum Terminator {
    Delimiter(u8),
    Statement,
}

impl Terminator {
    fn closes(self, byte: u8) -> bool {
        match self {
            Self::Delimiter(closing) => byte == closing,
            Self::Statement => matches!(byte, b'\n' | b'\r'),
        }
    }
}

/// Return a lowered helper call and the byte after this form's closing delimiter.
pub(super) fn form(source: &str, start: usize) -> Result<(String, usize), String> {
    let (closing, capture) = match source.as_bytes().get(start + 1) {
        Some(b'(') => (b')', true),
        Some(b'[') => (b']', false),
        _ => return Err(at(start, "expected an explicit subprocess form")),
    };
    pipeline(
        source,
        start,
        start + 2,
        Terminator::Delimiter(closing),
        capture,
    )
}

/// Lower an entire command block before any interpreter or host effect starts.
/// Bare pipelines use the same argv grammar as explicit forms, not a Bash eval.
pub(crate) fn commands(source: &str) -> Result<String, String> {
    if source.len() > 48 * 1024 {
        return Err("xonsh source exceeds 48 KiB".to_owned());
    }
    if source.contains('\0') {
        return Err("xonsh source cannot contain NUL".to_owned());
    }
    let mut output = String::new();
    let mut cursor = 0;
    while cursor < source.len() {
        while source
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if source.as_bytes().get(cursor) == Some(&b'#') {
            while source
                .as_bytes()
                .get(cursor)
                .is_some_and(|byte| !matches!(*byte, b'\n' | b'\r'))
            {
                cursor = next_char(source, cursor);
            }
            continue;
        }
        if cursor == source.len() {
            break;
        }
        let (lowered, end) = pipeline(source, cursor, cursor, Terminator::Statement, false)?;
        output.push_str(&lowered);
        output.push('\n');
        cursor = end;
    }
    Ok(output)
}

fn pipeline(
    source: &str,
    start: usize,
    mut cursor: usize,
    terminator: Terminator,
    capture: bool,
) -> Result<(String, usize), String> {
    let mut pipeline = Vec::new();
    let mut command = Vec::new();
    loop {
        while source
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace() && !terminator.closes(*byte))
        {
            cursor += 1;
        }
        let byte = source.as_bytes().get(cursor).copied();
        let closed = byte.is_some_and(|byte| terminator.closes(byte))
            || (byte.is_none() && matches!(terminator, Terminator::Statement));
        if closed || byte == Some(b'|') {
            if command.is_empty() {
                return Err(at(
                    cursor,
                    "a subprocess pipeline cannot contain an empty command",
                ));
            }
            pipeline.push(std::mem::take(&mut command));
            if byte.is_some() {
                cursor += 1;
                if matches!(terminator, Terminator::Statement)
                    && byte == Some(b'\r')
                    && source.as_bytes().get(cursor) == Some(&b'\n')
                {
                    cursor += 1;
                }
            }
            if closed {
                return Ok((render(&pipeline, capture), cursor));
            }
            continue;
        }
        if byte.is_none() {
            return Err(at(start, "unclosed subprocess form"));
        }
        if let Some(word) = word(source, &mut cursor, terminator)? {
            command.push(word);
        }
    }
}

fn word<'source>(
    source: &'source str,
    cursor: &mut usize,
    terminator: Terminator,
) -> Result<Option<Word<'source>>, String> {
    if source[*cursor..].starts_with("@(") {
        let start = *cursor + 2;
        let end = expression_end(source, *cursor + 1)?;
        let expression = source[start..end - 1].trim();
        if expression.is_empty() {
            return Err(at(start, "Python interpolation cannot be empty"));
        }
        require_word_end(
            source,
            end,
            terminator,
            "interpolation must occupy an entire shell word",
        )?;
        *cursor = end;
        return Ok(Some(Word::Splice(expression)));
    }

    let start = *cursor;
    let rest = &source[start..];
    let prefix_length = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    if matches!(
        source.as_bytes().get(start + prefix_length),
        Some(b'\'' | b'"')
    ) {
        let prefix = rest[..prefix_length].to_ascii_lowercase();
        if prefix.contains('p')
            && prefix
                .bytes()
                .all(|byte| matches!(byte, b'p' | b'r' | b'u' | b'b' | b'f' | b't'))
        {
            if prefix != "p" {
                return Err(at(
                    start,
                    "only plain p-prefixed path literals are supported; combined prefixes are unsupported",
                ));
            }
            // A standalone shell word has no Python attribute context. Scan
            // its quote directly, then preserve the p expression for path lowering.
            let end = string_end(source, start + 1)?.expect("path prefix has a quote");
            require_word_end(
                source,
                end,
                terminator,
                "path literals must occupy an entire shell word",
            )?;
            *cursor = end;
            return Ok(Some(Word::Splice(&source[start..end])));
        }
    }

    let mut literal = String::new();
    let mut started = false;
    while let Some(&byte) = source.as_bytes().get(*cursor) {
        if byte.is_ascii_whitespace() || terminator.closes(byte) || byte == b'|' {
            break;
        }
        match byte {
            b'\'' | b'"' => {
                quoted(source, cursor, &mut literal)?;
                started = true;
            }
            b'\\' => {
                *cursor += 1;
                let Some(&escaped) = source.as_bytes().get(*cursor) else {
                    return Err(at(
                        *cursor - 1,
                        "a shell escape needs a following character",
                    ));
                };
                if source.as_bytes()[*cursor..].starts_with(b"\r\n") {
                    *cursor += 2;
                } else {
                    if escaped != b'\n' {
                        literal.push_str(&source[*cursor..next_char(source, *cursor)]);
                        started = true;
                    }
                    *cursor = next_char(source, *cursor);
                }
            }
            b'@' if source[*cursor..].starts_with("@(") => {
                return Err(at(
                    *cursor,
                    "interpolation must occupy an entire shell word",
                ));
            }
            b'$' => {
                return Err(at(
                    *cursor,
                    "environment and nested subprocess syntax are unsupported",
                ));
            }
            b';' | b'&' | b'<' | b'>' => {
                return Err(at(
                    *cursor,
                    "shell control operators, background jobs and redirections are unsupported",
                ));
            }
            b'*' | b'?' | b'[' | b']' | b'{' | b'}' => {
                return Err(at(
                    *cursor,
                    "unquoted glob and brace syntax are unsupported",
                ));
            }
            b'(' | b')' | b'!' | b'`' | b'#' => {
                return Err(at(
                    *cursor,
                    "shell grouping, macros and comments are unsupported; quote literal characters",
                ));
            }
            _ => {
                let end = next_char(source, *cursor);
                literal.push_str(&source[*cursor..end]);
                *cursor = end;
                started = true;
            }
        }
    }
    Ok(started.then_some(Word::Literal(literal)))
}

fn require_word_end(
    source: &str,
    end: usize,
    terminator: Terminator,
    message: &str,
) -> Result<(), String> {
    if source.as_bytes().get(end).is_some_and(|byte| {
        !byte.is_ascii_whitespace() && !terminator.closes(*byte) && *byte != b'|'
    }) {
        return Err(at(end, message));
    }
    Ok(())
}

// Single quotes preserve every character. Double-quoted backslashes escape
// shell metacharacters; other backslashes remain data (e.g. printf's "\n").
// There is no expansion inside either kind of quoted literal.
fn quoted(source: &str, cursor: &mut usize, output: &mut String) -> Result<(), String> {
    let start = *cursor;
    let quote = source.as_bytes()[start];
    *cursor += 1;
    while let Some(&byte) = source.as_bytes().get(*cursor) {
        if byte == quote {
            *cursor += 1;
            return Ok(());
        }
        if quote == b'"' && byte == b'\\' {
            let escaped = source.as_bytes().get(*cursor + 1).copied();
            if matches!(escaped, Some(b'$' | b'`' | b'"' | b'\\' | b'\n')) {
                *cursor += 1;
                if escaped != Some(b'\n') {
                    let end = next_char(source, *cursor);
                    output.push_str(&source[*cursor..end]);
                }
                *cursor = next_char(source, *cursor);
                continue;
            }
        }
        let end = next_char(source, *cursor);
        output.push_str(&source[*cursor..end]);
        *cursor = end;
    }
    Err(at(start, "unclosed shell quote"))
}

fn render(pipeline: &[Vec<Word<'_>>], capture: bool) -> String {
    let mut output = String::from("__rig_xonsh_run([");
    for (index, command) in pipeline.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        output.push('[');
        for (index, word) in command.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            match word {
                Word::Literal(value) => {
                    output.push_str("['literal', ");
                    output.push_str(&serde_json::to_string(value).expect("strings serialize"));
                    output.push(']');
                }
                Word::Splice(expression) => {
                    // A newline prevents a final Python comment from consuming
                    // the generated closing delimiters.
                    output.push_str("['splice', (\n");
                    output.push_str(expression);
                    output.push_str("\n)]");
                }
            }
        }
        output.push(']');
    }
    output.push_str("], ");
    output.push_str(if capture { "True" } else { "False" });
    output.push(')');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_blocks_share_explicit_argv_and_pipeline_lowering() {
        let source =
            "\n # ignored $(not a command)\r\npwd\r\nprintf '%s\\n' 'a; b' @(values) | cat\n\n";
        let expected = format!(
            "{}\n{}\n",
            form("$[pwd]", 0).unwrap().0,
            form("$[printf '%s\\n' 'a; b' @(values) | cat]", 0)
                .unwrap()
                .0,
        );
        assert_eq!(commands(source).unwrap(), expected);
        assert_eq!(commands("\t\n# only a comment\r\n ").unwrap(), "");
    }

    #[test]
    fn quoted_newlines_and_multiline_interpolation_do_not_split_commands() {
        let command =
            "printf '%s' '雪; not code' 'first\nsecond' @(\nvalues[0:2] # Python comment\n)";
        let explicit = format!("$[{command}]");
        assert_eq!(
            commands(&format!("{command}\n")).unwrap(),
            format!("{}\n", form(&explicit, 0).unwrap().0),
        );
        let lowered = commands(command).unwrap();
        assert_eq!(lowered.matches("__rig_xonsh_run(").count(), 1);
        assert!(lowered.contains("['literal', \"雪; not code\"]"));
        assert!(lowered.contains("['literal', \"first\\nsecond\"]"));
        assert!(lowered.contains("['splice', (\nvalues[0:2] # Python comment\n)]"));
    }

    #[test]
    fn escaped_lf_and_crlf_continue_a_word_without_starting_another_command() {
        let lowered = commands("printf joined\\\nword\nprintf joined\\\r\nword").unwrap();
        let expected = form("$[printf joinedword]", 0).unwrap().0;
        assert_eq!(lowered, format!("{expected}\n{expected}\n"));
        assert_eq!(
            form("$[\npwd\n]", 0).unwrap().0,
            form("$[pwd]", 0).unwrap().0,
        );
        assert!(
            form("$(printf capture)", 0)
                .unwrap()
                .0
                .ends_with("], True)")
        );
    }

    #[test]
    fn standalone_path_words_are_borrowed_expressions_not_shell_source() {
        let command = "printf '%s' . p'a; b' P\"雪\" p'''first\nsecond''' | cat";
        let lowered = commands(command).unwrap();
        assert!(lowered.contains("['literal', \".\"]"));
        assert!(lowered.contains("['splice', (\np'a; b'\n)]"));
        assert!(lowered.contains("['splice', (\nP\"雪\"\n)]"));
        let paths = crate::frontend::lower_paths(&lowered).unwrap();
        assert!(paths.contains("__rig_xonsh_path('a; b')"));
        assert!(paths.contains("__rig_xonsh_path(\"雪\")"));
        assert!(paths.contains("__rig_xonsh_path('''first\nsecond''')"));
        let explicit = format!("$[{command}]");
        assert_eq!(lowered, format!("{}\n", form(&explicit, 0).unwrap().0));
    }

    #[test]
    fn malformed_later_commands_refuse_the_entire_block() {
        for command in [
            "printf bad; touch injected",
            "printf bad && touch injected",
            "printf *",
            "printf text |",
            "| cat",
            "printf 'unclosed",
            "printf @(value",
            "printf @(value)suffix",
            "printf p'path'suffix",
            "printf pr'path'",
            "printf PF'path'",
            "printf > output",
            "printf $(nested)",
            "printf text # inline comments are unsupported",
            "printf null\0",
        ] {
            let source = format!("printf first\n{command}");
            assert!(commands(&source).is_err(), "accepted {source:?}");
        }
    }
}
