//! Literal argv and pipeline grammar for an explicit xonsh subprocess form.

use crate::frontend::{at, expression_end, next_char};

enum Word<'source> {
    Literal(String),
    Splice(&'source str),
}

/// Return a lowered helper call and the byte after this form's closing delimiter.
pub(super) fn form(source: &str, start: usize) -> Result<(String, usize), String> {
    let (closing, capture) = match source.as_bytes().get(start + 1) {
        Some(b'(') => (b')', true),
        Some(b'[') => (b']', false),
        _ => return Err(at(start, "expected an explicit subprocess form")),
    };
    let mut cursor = start + 2;
    let mut pipeline = Vec::new();
    let mut command = Vec::new();
    loop {
        while source
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        let Some(&byte) = source.as_bytes().get(cursor) else {
            return Err(at(start, "unclosed subprocess form"));
        };
        if byte == closing || byte == b'|' {
            if command.is_empty() {
                return Err(at(
                    cursor,
                    "a subprocess pipeline cannot contain an empty command",
                ));
            }
            pipeline.push(std::mem::take(&mut command));
            cursor += 1;
            if byte == closing {
                return Ok((render(&pipeline, capture), cursor));
            }
            continue;
        }
        if let Some(word) = word(source, &mut cursor, closing)? {
            command.push(word);
        }
    }
}

fn word<'source>(
    source: &'source str,
    cursor: &mut usize,
    closing: u8,
) -> Result<Option<Word<'source>>, String> {
    if source[*cursor..].starts_with("@(") {
        let start = *cursor + 2;
        let end = expression_end(source, *cursor + 1)?;
        let expression = source[start..end - 1].trim();
        if expression.is_empty() {
            return Err(at(start, "Python interpolation cannot be empty"));
        }
        *cursor = end;
        if source
            .as_bytes()
            .get(end)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != closing && *byte != b'|')
        {
            return Err(at(end, "interpolation must occupy an entire shell word"));
        }
        return Ok(Some(Word::Splice(expression)));
    }

    let mut literal = String::new();
    let mut started = false;
    while let Some(&byte) = source.as_bytes().get(*cursor) {
        if byte.is_ascii_whitespace() || byte == closing || byte == b'|' {
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
                if escaped != b'\n' {
                    literal.push_str(&source[*cursor..next_char(source, *cursor)]);
                    started = true;
                }
                *cursor = next_char(source, *cursor);
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
