//! Shared streaming argv lexer; neither dialect evaluates shell syntax.
#[derive(Clone, Copy)]
pub(crate) enum Dialect {
    HookShlex,
    ClangGnu22,
}
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub input_bytes: usize,
    pub arguments: usize,
    pub token_bytes: usize,
    pub decoded_bytes: usize,
}
struct Output {
    limits: Option<Limits>,
    arguments: Vec<String>,
    token: String,
    bytes: usize,
}
impl Output {
    fn push(&mut self, ch: char) -> Result<(), String> {
        if let Some(limits) = self.limits {
            if self.token.len().saturating_add(ch.len_utf8()) > limits.token_bytes {
                return Err("compilation argument exceeds token byte limit".into());
            }
            if self.bytes.saturating_add(ch.len_utf8()) > limits.decoded_bytes {
                return Err("compilation command exceeds decoded byte limit".into());
            }
        }
        self.bytes = self.bytes.saturating_add(ch.len_utf8());
        self.token.push(ch);
        Ok(())
    }
    fn emit(&mut self) -> Result<(), String> {
        if self
            .limits
            .is_some_and(|limits| self.arguments.len() >= limits.arguments)
        {
            return Err("compilation command exceeds argument count limit".into());
        }
        self.arguments.push(std::mem::take(&mut self.token));
        Ok(())
    }
}
fn whitespace(ch: char, dialect: Dialect) -> bool {
    match dialect {
        Dialect::HookShlex => matches!(ch, ' ' | '\t' | '\r' | '\n'),
        Dialect::ClangGnu22 => ch == ' ',
    }
}
/// Hook semantics are unchanged. ClangGnu22 follows LLVM 22's GNU database
/// quoting, but rejects unfinished quotes/escapes instead of partial recovery.
/// Controls are admitted/rejected by the database layer, not this generic lexer.
pub(crate) fn split(
    command: &str,
    dialect: Dialect,
    limits: Option<Limits>,
) -> Result<Vec<String>, String> {
    if limits.is_some_and(|limits| command.len() > limits.input_bytes) {
        return Err("compilation command exceeds input byte limit".into());
    }
    let mut output = Output {
        limits,
        arguments: Vec::new(),
        token: String::new(),
        bytes: 0,
    };
    let mut have_token = false;
    let mut chars = command.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if q == '"' && ch == '\\' {
                let escapes_next = matches!(dialect, Dialect::ClangGnu22)
                    || chars.peek().is_some_and(|next| matches!(*next, '"' | '\\'));
                if escapes_next {
                    output.push(chars.next().ok_or("No escaped character")?)?;
                } else {
                    output.push(ch)?;
                }
            } else {
                output.push(ch)?;
            }
            have_token = true;
        } else if whitespace(ch, dialect) {
            if have_token {
                output.emit()?;
                have_token = false;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            have_token = true;
        } else if ch == '\\' {
            output.push(chars.next().ok_or("No escaped character")?)?;
            have_token = true;
        } else {
            output.push(ch)?;
            have_token = true;
        }
    }
    if quote.is_some() {
        return Err("No closing quotation".into());
    }
    if have_token {
        output.emit()?;
    }
    Ok(output.arguments)
}
#[cfg(test)]
#[path = "command_lex_tests.rs"]
mod tests;
