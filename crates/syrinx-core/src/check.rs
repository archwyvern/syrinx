//! Static determinism check.
//!
//! Sound sources must produce the same bytes every time they are compiled, on every machine.
//! Anything that reads the wall clock, the process, or entropy is rejected before the source
//! ever reaches V8, with a located error. This is a lexical scan, not a parser: comments and
//! string literals are skipped so that a banned name in a comment does not trip it, but a
//! determined author can still smuggle one through (`globalThis["Ma"+"th"]`). The purpose is
//! to catch honest mistakes early, not to sandbox hostile code.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
    pub message: String,
}

/// Banned names and why. Order matters only for the message.
const BANNED: &[(&str, &str)] = &[
    ("Math.random", "unseeded randomness; use `new Random(seed)` from the prelude"),
    ("Date", "wall clock"),
    ("performance", "wall clock"),
    ("setTimeout", "timers do not exist in a sound source"),
    ("setInterval", "timers do not exist in a sound source"),
    ("queueMicrotask", "scheduling does not exist in a sound source"),
    ("eval", "dynamic code defeats the determinism check"),
    ("Function", "dynamic code defeats the determinism check"),
    ("globalThis", "reaching the realm's globals defeats the determinism check"),
    ("Intl", "locale-dependent"),
    ("crypto", "entropy"),
    ("require", "use `import` from \"syrinx\" or a relative path"),
    ("fetch", "no I/O in a sound source"),
    ("console", "no I/O in a sound source"),
];

/// The exponentiation operator compiles to the engine's own pow, which the standard math cannot
/// replace the way it replaces `Math.pow`. It is the one operator in the language that reaches
/// transcendental arithmetic.
const EXPONENT_WHY: &str = "exponentiation uses the engine's own pow, not the standard's; call Math.pow(x, y)";

/// Scan `source` and report every use of a banned name, and every `**`.
pub fn check(source: &str) -> Vec<Diagnostic> {
    let stripped = strip(source);
    let mut out = Vec::new();
    for (name, why) in BANNED {
        for (line, column) in find_identifier(&stripped, name) {
            out.push(Diagnostic { line, column, message: format!("`{name}` is not allowed in a sound source: {why}") });
        }
    }
    for (line, column) in find_exponent(&stripped) {
        out.push(Diagnostic {
            line,
            column,
            message: format!("`**` is not allowed in a sound source: {EXPONENT_WHY}"),
        });
    }
    out.sort_by_key(|d| (d.line, d.column));
    out
}

/// Replace comments and string/template literal bodies with spaces, preserving every
/// newline and every column so positions in the result map 1:1 onto the source.
fn strip(source: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        Str(char),
        Template,
    }

    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut state = State::Code;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match state {
            State::Code => match c {
                '/' if next == Some('/') => {
                    state = State::LineComment;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                '/' if next == Some('*') => {
                    state = State::BlockComment;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                '"' | '\'' => {
                    state = State::Str(c);
                    out.push(c);
                }
                '`' => {
                    state = State::Template;
                    out.push(c);
                }
                _ => out.push(c),
            },
            State::LineComment => {
                if c == '\n' {
                    state = State::Code;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
            State::BlockComment => {
                if c == '*' && next == Some('/') {
                    state = State::Code;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                out.push(if c == '\n' { '\n' } else { ' ' });
            }
            State::Str(quote) => {
                if c == '\\' {
                    out.push(' ');
                    if let Some(n) = next {
                        out.push(if n == '\n' { '\n' } else { ' ' });
                        i += 2;
                        continue;
                    }
                } else if c == quote || c == '\n' {
                    state = State::Code;
                    out.push(c);
                } else {
                    out.push(' ');
                }
            }
            State::Template => {
                // Template substitutions (`${...}`) are code, but nesting them properly needs
                // a real parser; treat the whole literal as text. A banned name inside a
                // substitution therefore slips through this check and fails at run time.
                if c == '\\' {
                    out.push(' ');
                    if let Some(n) = next {
                        out.push(if n == '\n' { '\n' } else { ' ' });
                        i += 2;
                        continue;
                    }
                } else if c == '`' {
                    state = State::Code;
                    out.push(c);
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                }
            }
        }
        i += 1;
    }
    out
}

fn is_ident(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric()
}

/// Every occurrence of `name` (which may be dotted) as a whole identifier path.
fn find_identifier(text: &str, name: &str) -> Vec<(u32, u32)> {
    let mut hits = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut from = 0;
        while let Some(pos) = line[from..].find(name) {
            let start = from + pos;
            let end = start + name.len();
            let before_ok = start == 0 || {
                let prev = line[..start].chars().next_back().unwrap();
                !is_ident(prev) && prev != '.'
            };
            let after_ok = end >= bytes.len() || !is_ident(line[end..].chars().next().unwrap());
            if before_ok && after_ok {
                let column = line[..start].chars().count() as u32 + 1;
                hits.push((line_index as u32 + 1, column));
            }
            from = end;
        }
    }
    hits
}

/// Every `**` (which covers `**=`) in the stripped text. Comments and literals are already
/// blanks, and no other JavaScript token contains two adjacent asterisks.
fn find_exponent(text: &str) -> Vec<(u32, u32)> {
    let mut hits = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let mut from = 0;
        while let Some(pos) = line[from..].find("**") {
            let start = from + pos;
            let column = line[..start].chars().count() as u32 + 1;
            hits.push((line_index as u32 + 1, column));
            from = start + 2;
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_the_exponent_operator() {
        let d = check("const a = 2 ** 10;\nlet b = 1;\nb **= 2;\n/** doc */ const c = '**';\n");
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].line, d[0].column), (1, 13));
        assert_eq!((d[1].line, d[1].column), (3, 3));
        assert!(d[0].message.contains("Math.pow"));
    }

    #[test]
    fn catches_math_random_with_position() {
        let d = check("const x = 1;\nlet y = Math.random();\n");
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].line, d[0].column), (2, 9));
    }

    #[test]
    fn ignores_comments_and_strings() {
        let src = "// Date is fine here\n/* Math.random too */\nconst s = 'Date';\nconst t = `performance`;\n";
        assert!(check(src).is_empty());
    }

    #[test]
    fn does_not_match_inside_other_identifiers() {
        assert!(check("const update = 1; const myDate = 2; const evalue = 3;").is_empty());
        assert!(check("obj.Date").is_empty());
    }

    #[test]
    fn positions_survive_stripping() {
        let src = "const a = 'x'; // c\nconst b = Date.now();";
        let d = check(src);
        assert_eq!((d[0].line, d[0].column), (2, 11));
    }
}
