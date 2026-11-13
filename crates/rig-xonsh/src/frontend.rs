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
    lower_paths(&output)
}

// Subprocess lowering leaves interpolation expressions as Python source. Doing
// path lowering afterward handles those expressions as well as ordinary Python,
// while quoted argv data, Python strings and comments stay literal. This adds
// lexical Path values only: neither ~ nor environment variables are expanded.
pub(crate) fn lower_paths(code: &str) -> Result<String, String> {
    let mut output = String::with_capacity(code.len());
    let mut cursor = 0;
    let mut copied = 0;
    let mut after_dot = false;
    let mut depth = 0_usize;
    while cursor < code.len() {
        if let Some(end) = string_end(code, cursor)? {
            if matches!(code.as_bytes()[cursor], b'p' | b'P')
                && matches!(code.as_bytes().get(cursor + 1), Some(b'\'' | b'"'))
            {
                if after_dot {
                    return Err(at(cursor, "a path literal cannot follow attribute access"));
                }
                output.push_str(&code[copied..cursor]);
                // Start with an identifier, not a parenthesized expression:
                // malformed adjacency such as 'prefix' p'path' must not become
                // an implicit function call that fails only after earlier effects.
                output.push_str("__rig_xonsh_path(");
                output.push_str(&code[cursor + 1..end]);
                output.push(')');
                copied = end;
            }
            after_dot = false;
            cursor = end;
            continue;
        }
        match code.as_bytes()[cursor] {
            b'#' => cursor = comment_end(code, cursor),
            b'\\' if matches!(code.as_bytes().get(cursor + 1), Some(b'\n' | b'\r')) => {
                // Explicit continuation preserves attribute context, even at
                // top level. Its newline does not start another statement.
                cursor += 2;
                if code.as_bytes()[cursor - 1] == b'\r'
                    && code.as_bytes().get(cursor) == Some(&b'\n')
                {
                    cursor += 1;
                }
            }
            b'\n' | b'\r' => {
                // Inside delimiters a newline is only whitespace; outside,
                // dots in numeric/ellipsis literals belong to the prior statement.
                if depth == 0 {
                    after_dot = false;
                }
                cursor += 1;
            }
            byte if byte.is_ascii_whitespace() => cursor += 1,
            byte => {
                match byte {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                after_dot = byte == b'.';
                cursor = next_char(code, cursor);
            }
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

// Python/path prefixes are recognized only at a token boundary. Strings are
// not evaluated here; both raw and ordinary strings escape their quote with '\'.
pub(super) fn string_end(source: &str, start: usize) -> Result<Option<usize>, String> {
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
        let length = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        if length == 0 || !matches!(bytes.get(start + length), Some(b'\'' | b'"')) {
            return Ok(None);
        }
        let prefix = &rest[..length];
        let normalized = prefix.to_ascii_lowercase();
        let path_prefix = normalized.contains('p')
            && normalized
                .bytes()
                .all(|byte| matches!(byte, b'p' | b'r' | b'u' | b'b' | b'f' | b't'));
        if path_prefix && normalized != "p" {
            return Err(at(
                start,
                "only plain p-prefixed path literals are supported; combined prefixes are unsupported",
            ));
        }
        if !matches!(
            normalized.as_str(),
            "p" | "r" | "u" | "b" | "br" | "rb" | "f" | "fr" | "rf" | "t" | "tr" | "rt"
        ) {
            return Ok(None);
        }
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
    fn lowers_lexical_paths_in_python_and_interpolated_arguments() {
        let source = concat!(
            "root = p'~/$HOME/雪'\n",
            "child = P\"file.txt\"\n",
            "multi = p'''one\n$[literal]\n'''\n",
            "value = $(printf '%s' @(root / p'leaf'))\n",
            "commented = str(\n # comment.\n p'leaf'\n)\n",
        );
        let lowered = lower(source).unwrap();
        assert!(lowered.contains("root = __rig_xonsh_path('~/$HOME/雪')"));
        assert!(lowered.contains("child = __rig_xonsh_path(\"file.txt\")"));
        assert!(lowered.contains("multi = __rig_xonsh_path('''one\n$[literal]\n''')"));
        assert!(lowered.contains("root / __rig_xonsh_path('leaf')"));
        assert!(lowered.contains("commented = str(\n # comment.\n __rig_xonsh_path('leaf')\n)"));
    }

    #[test]
    fn path_lowering_preserves_strings_comments_and_identifier_boundaries() {
        let source = concat!(
            "text = \"p'not a path'\"\n",
            "raw = r'''p'literal' $(literal)'''\n",
            "# p'comment'\n",
            "named = p\n",
            // This stays invalid Python, rather than becoming a helper call.
            "xp'not a prefix'\n",
        );
        assert_eq!(lower(source).unwrap(), source);
        for source in [
            "obj.p'leaf'",
            "obj. p'leaf'",
            "(obj.\n p'leaf')",
            "obj.\\\n p'leaf'",
            "obj.\\\r\n p'leaf'",
            "(obj. # comment.\n p'leaf')",
        ] {
            assert!(
                lower(source).is_err(),
                "accepted attribute literal {source:?}"
            );
        }
        // Bare helper emission preserves malformed expression adjacency for
        // Monty's whole-program compilation, rather than creating valid calls.
        assert_eq!(
            lower("'prefix' p'leaf'").unwrap(),
            "'prefix' __rig_xonsh_path('leaf')",
        );
        assert_eq!(
            lower("func() p'leaf'").unwrap(),
            "func() __rig_xonsh_path('leaf')",
        );
        assert_eq!(
            lower("1.\np'leaf'\n...\np'other'").unwrap(),
            "1.\n__rig_xonsh_path('leaf')\n...\n__rig_xonsh_path('other')",
        );
        let shell_literal = lower("$(printf '%s' \"p'not a path'\")").unwrap();
        assert!(shell_literal.contains("['literal', \"p'not a path'\"]"));
        assert!(!shell_literal.contains("__rig_xonsh_path"));
    }

    #[test]
    fn rejects_combined_or_unclosed_path_literals_before_execution() {
        for prefix in ["pr", "rp", "pf", "fp", "pfr", "frp", "pp", "pt"] {
            let source = format!("$(printf first)\n{prefix}'unsupported'");
            let error = lower(&source).unwrap_err();
            assert!(error.contains("combined prefixes"), "{error}");
        }
        for source in ["p'unclosed", "p'line\nbreak'", "$(printf '%s' @(p'bad))"] {
            assert!(lower(source).is_err(), "accepted {source:?}");
        }
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
