//! Lexical lowering of explicit subprocess forms; Monty still parses Python.

use crate::shell;

/// Lower the complete source before any interpreter or host effect can start.
pub(crate) fn lower(code: &str) -> Result<String, String> {
    if code.len() > 48 * 1024 {
        return Err("xonsh source exceeds 48 KiB".to_owned());
    }
    if code.contains('\0') {
        return Err("xonsh source cannot contain NUL".to_owned());
    }
    let mut output = String::with_capacity(code.len());
    let mut cursor = 0;
    let mut copied = 0;
    while cursor < code.len() {
        if let Some(end) = string_end(code, cursor)? {
            cursor = end;
            continue;
        }
        match code.as_bytes()[cursor] {
            b'#' => cursor = comment_end(code, cursor),
            b'$' => {
                if !matches!(code.as_bytes().get(cursor + 1), Some(b'(' | b'[')) {
                    return Err(at(
                        cursor,
                        "only explicit $(...) and $[...] forms are supported",
                    ));
                }
                let (form, end) = shell::form(code, cursor)?;
                output.push_str(&code[copied..cursor]);
                output.push_str(&form);
                cursor = end;
                copied = end;
            }
            b'!' if matches!(code.as_bytes().get(cursor + 1), Some(b'(' | b'[')) => {
                return Err(at(cursor, "rich subprocess forms are unsupported"));
            }
            b'`' => return Err(at(cursor, "backtick syntax is unsupported")),
            _ => cursor = next_char(code, cursor),
        }
    }
    output.push_str(&code[copied..]);
    Ok(output)
}

/// Find a parenthesized Python interpolation, including its closing delimiter.
/// The returned range is borrowed from source until the caller emits Python.
pub(super) fn expression_end(source: &str, opening: usize) -> Result<usize, String> {
    let mut expected = vec![b')'];
    let mut cursor = opening + 1;
    while cursor < source.len() {
        if let Some(end) = string_end(source, cursor)? {
            cursor = end;
            continue;
        }
        match source.as_bytes()[cursor] {
            b'#' => cursor = comment_end(source, cursor),
            b'(' | b'[' | b'{' => {
                if expected.len() >= 128 {
                    return Err(at(cursor, "interpolation nesting exceeds 128 delimiters"));
                }
                expected.push(match source.as_bytes()[cursor] {
                    b'(' => b')',
                    b'[' => b']',
                    _ => b'}',
                });
                cursor += 1;
            }
            closing @ (b')' | b']' | b'}') => {
                if expected.pop() != Some(closing) {
                    return Err(at(cursor, "mismatched Python interpolation delimiter"));
                }
                cursor += 1;
                if expected.is_empty() {
                    return Ok(cursor);
                }
            }
            b'$' | b'`' => {
                return Err(at(
                    cursor,
                    "xonsh syntax inside Python interpolation is unsupported",
                ));
            }
            b'!' if matches!(source.as_bytes().get(cursor + 1), Some(b'(' | b'[')) => {
                return Err(at(cursor, "nested subprocess forms are unsupported"));
            }
            b'@' if source[cursor..].starts_with("@(") => {
                return Err(at(cursor, "nested xonsh interpolation is unsupported"));
            }
            _ => cursor = next_char(source, cursor),
        }
    }
    Err(at(opening, "unclosed Python interpolation"))
}

// Python prefixes are recognized only at a token boundary. Strings are copied,
// not evaluated here; both raw and ordinary strings escape their quote with '\'.
fn string_end(source: &str, start: usize) -> Result<Option<usize>, String> {
    let bytes = source.as_bytes();
    let mut quote = start;
    if !matches!(bytes[start], b'\'' | b'"') {
        if start > 0
            && source[..start]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        {
            return Ok(None);
        }
        let rest = &source[start..];
        let prefix = [2, 1].into_iter().find_map(|length| {
            let text = rest.get(..length)?;
            if !text.is_ascii() || !matches!(bytes.get(start + length), Some(b'\'' | b'"')) {
                return None;
            }
            matches!(
                text.to_ascii_lowercase().as_str(),
                "r" | "u" | "b" | "br" | "rb" | "f" | "fr" | "rf" | "t" | "tr" | "rt"
            )
            .then_some(text)
        });
        let Some(prefix) = prefix else {
            return Ok(None);
        };
        if prefix
            .bytes()
            .any(|byte| matches!(byte.to_ascii_lowercase(), b'f' | b't'))
        {
            return Err(at(
                start,
                "Python f-strings and template strings are unsupported",
            ));
        }
        quote += prefix.len();
    }
    let delimiter = bytes[quote];
    let triple = bytes
        .get(quote..quote + 3)
        .is_some_and(|part| part.iter().all(|&byte| byte == delimiter));
    let width = if triple { 3 } else { 1 };
    let mut cursor = quote + width;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor += 1;
            if cursor < bytes.len() {
                cursor = if bytes[cursor..].starts_with(b"\r\n") {
                    cursor + 2
                } else {
                    next_char(source, cursor)
                };
            }
        } else if bytes[cursor] == delimiter
            && (!triple
                || bytes
                    .get(cursor..cursor + 3)
                    .is_some_and(|part| part.iter().all(|&byte| byte == delimiter)))
        {
            return Ok(Some(cursor + width));
        } else if !triple && matches!(bytes[cursor], b'\n' | b'\r') {
            return Err(at(quote, "unclosed Python string before newline"));
        } else {
            cursor = next_char(source, cursor);
        }
    }
    Err(at(quote, "unclosed Python string"))
}

fn comment_end(source: &str, start: usize) -> usize {
    source[start..]
        .find('\n')
        .map_or(source.len(), |offset| start + offset)
}

pub(super) fn next_char(source: &str, cursor: usize) -> usize {
    cursor
        + source[cursor..]
            .chars()
            .next()
            .expect("cursor is before source end")
            .len_utf8()
}

pub(super) fn at(offset: usize, message: &str) -> String {
    format!("xonsh source byte {offset}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_python_literals_and_comments_while_lowering_explicit_forms() {
        let source = concat!(
            "text = r'''$(literal) \\\n $[literal]'''\n",
            "# $(comment)\n",
            "value = $(printf '%s' '雪 )' | cat)\n",
            r#"$[printf '' "a\"b" escaped\ word @(values[0:2])]"#,
            "\n",
        );
        let lowered = lower(source).unwrap();
        assert!(lowered.starts_with("text = r'''$(literal) \\\n $[literal]'''\n# $(comment)\n"));
        assert!(lowered.contains("['literal', \"雪 )\"]"));
        assert!(lowered.contains("['splice', (\nvalues[0:2]\n)]"));
        assert!(lowered.contains(r#"['literal', "a\"b"]"#));
        assert!(lowered.contains("['literal', \"escaped word\"]"));
        assert!(lowered.contains("], True)"));
        assert!(lowered.contains("], False)"));
    }

    #[test]
    fn rejects_unsupported_or_ambiguous_source_before_execution() {
        for source in [
            "$(echo ok); $HOME",
            "f'{value}'",
            "rf'$(not a form)'",
            "$(echo @(value)suffix)",
            "$(echo prefix@(value))",
            "$(echo @($(nested)))",
            "$(echo @(other@(nested)))",
            "$(echo @(items[0)))",
            "$(echo ok &)",
            "$(echo ok; echo bad)",
            "$(cat <<EOF)",
            "$(echo *)",
            "!(echo rich)",
        ] {
            assert!(lower(source).is_err(), "accepted {source:?}");
        }
    }
}
