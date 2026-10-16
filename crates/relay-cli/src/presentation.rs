//! Human presentation only. Domain results and JSON never pass through this module's renderer.
//! Build plain text first; apply terminal styling at the final output boundary.
use std::io::IsTerminal as _;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Terminal {
    pub color: bool,
    pub width: usize,
}

impl Terminal {
    pub fn stdout() -> Self {
        Self::detect(std::io::stdout().is_terminal())
    }

    pub fn stderr() -> Self {
        Self::detect(std::io::stderr().is_terminal())
    }

    fn detect(tty: bool) -> Self {
        Self {
            color: color_allowed(
                tty,
                false,
                std::env::var_os("NO_COLOR").is_some(),
                std::env::var("TERM").ok().as_deref(),
            ),
            // A conservative default also works when a terminal does not export COLUMNS.
            width: std::env::var("COLUMNS")
                .ok()
                .and_then(|v| v.parse().ok())
                .or_else(|| terminal_size::terminal_size().map(|(width, _)| usize::from(width.0)))
                .unwrap_or(80usize)
                .clamp(40, 120),
        }
    }
}

pub(crate) fn color_allowed(tty: bool, json: bool, no_color: bool, term: Option<&str>) -> bool {
    tty && !json && !no_color && term != Some("dumb")
}

/// Strip terminal/bidi controls; preserve line boundaries only when explicitly requested.
pub(crate) fn clean(text: &str, multiline: bool) -> String {
    text.replace('\t', "  ").chars().filter(|c| {
        (!c.is_control() || (multiline && *c == '\n'))
            && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }).collect()
}

pub(crate) fn bounded(text: &str, limit: usize) -> String {
    let text = clean(text, false);
    if text.chars().count() <= limit {
        return text;
    }
    let mut result: String = text.chars().take(limit.saturating_sub(1)).collect();
    result.push('…');
    result
}

pub(crate) fn provider(kind: relay_core::ProviderKind) -> &'static str {
    match kind {
        relay_core::ProviderKind::Claude => "Claude",
        relay_core::ProviderKind::Codex => "Codex",
        relay_core::ProviderKind::Fake => "Fake",
    }
}

pub(crate) fn section(title: &str, body: &str) -> String {
    format!(
        "{title}\n{}",
        body.lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Commands are always plain, on their own lines, with each continuation indented equally.
pub(crate) fn recovery(name: &str, commands: &str) -> String {
    format!(
        "  {name}\n{}",
        commands
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub(crate) fn table(headers: &[&str], rows: &[Vec<String>], caps: &[usize]) -> String {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .map(|row| row[i].chars().count())
                .max()
                .unwrap_or(0)
                .max(h.len())
                .min(caps[i])
        })
        .collect();
    let row = |values: &[String]| {
        values
            .iter()
            .enumerate()
            .map(|(i, value)| {
                let value = bounded(value, widths[i]);
                if i + 1 == values.len() {
                    value
                } else {
                    format!(
                        "{value}{}",
                        " ".repeat(widths[i].saturating_sub(value.chars().count()) + 2)
                    )
                }
            })
            .collect::<String>()
    };
    std::iter::once(row(&headers
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<_>>()))
    .chain(rows.iter().map(|values| row(values)))
    .collect::<Vec<_>>()
    .join("\n")
}

/// Keep full diagnostics outside a table. Recovery text is rendered separately, never embedded.
pub(crate) fn diagnostic(detail: &str) -> String {
    bounded(detail.split("\nRun:").next().unwrap_or(detail), 360)
}

pub(crate) fn auth_label(state: &str, detail: Option<&str>) -> &'static str {
    match state {
        "authenticated" => "✓ Authenticated",
        "needs login" if detail.is_some_and(|d| d.starts_with("authentication_revoked:")) => {
            "✗ Credentials revoked"
        }
        "needs login" if detail.is_some_and(|d| d.starts_with("authentication_invalid:")) => {
            "✗ Credentials invalid"
        }
        "needs login" => "! Needs login",
        "usage unverified" => "! Usage unverified",
        _ => "✗ Unavailable",
    }
}

pub(crate) fn render(text: &str, terminal: Terminal) -> String {
    let plain = clean(text, true);
    let lines: Vec<&str> = plain.lines().collect();
    let mut rendered = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trim = line.trim_start();
        let command = trim.starts_with("relay ")
            || trim.starts_with("relay-dev ")
            || trim.starts_with("brew ")
            || trim.starts_with("cd ")
            || trim.starts_with("env ")
            || trim.starts_with("claude ")
            || trim.starts_with("codex ");
        let heading = !line.is_empty()
            && !line.starts_with(' ')
            && lines
                .get(index + 1)
                .is_some_and(|next| next.is_empty() || next.starts_with(' '))
            && !line.contains(':');
        let code = if command {
            None
        } else if trim.starts_with('✗') || trim.starts_with("error [") {
            Some("31")
        } else if trim.starts_with('!') || trim.starts_with('⚠') {
            Some("33")
        } else if trim.starts_with('✓') {
            Some("32")
        } else if heading || trim.starts_with("> ") {
            Some("1")
        } else if trim.starts_with("Session ") || trim.starts_with("Herdr:") {
            Some("2")
        } else {
            None
        };
        // Do not insert hard breaks into copyable commands or table rows. Long prose gets a
        // hanging indent; full opaque identifiers remain intact rather than being truncated.
        let wrapped = if command || is_table_row(line) {
            (*line).to_owned()
        } else {
            wrap(line, terminal.width)
        };
        if terminal.color && is_table_row(line) {
            if let Some((offset, code)) = [("✓ ", "32"), ("! ", "33"), ("✗ ", "31")]
                .iter()
                .find_map(|(marker, code)| line.find(marker).map(|offset| (offset, *code)))
            {
                rendered.push(format!(
                    "{}\x1b[{code}m{}\x1b[0m",
                    &line[..offset],
                    &line[offset..]
                ));
            } else if line.starts_with("PROFILE") {
                rendered.push(format!("\x1b[1m{line}\x1b[0m"));
            } else {
                rendered.push(wrapped);
            }
            continue;
        }
        rendered.push(match code.filter(|_| terminal.color) {
            Some(code) => format!("\x1b[{code}m{wrapped}\x1b[0m"),
            None => wrapped,
        });
    }
    rendered.join("\n")
}

fn is_table_row(line: &str) -> bool {
    !line.starts_with(' ') && line.split("  ").filter(|cell| !cell.is_empty()).count() >= 3
}

fn wrap(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        return line.to_owned();
    }
    let indent = line.len() - line.trim_start().len();
    let continuation = " ".repeat((indent + 2).min(12));
    let mut result = String::new();
    let mut count = 0;
    for word in line.split_whitespace() {
        let length = word.chars().count();
        if count == 0 {
            result.push_str(&" ".repeat(indent));
            count = indent;
        }
        if count > indent && count + 1 + length > width {
            result.push('\n');
            result.push_str(&continuation);
            count = continuation.len();
        } else if count > indent {
            result.push(' ');
            count += 1;
        }
        result.push_str(word);
        count += length;
    }
    result
}

pub(crate) fn error(error: &relay_core::Error) -> String {
    // Keep the existing support code and first-line contract. Only separate recovery prose.
    if let relay_core::Error::ProfileAuthenticationRequired(name) = error {
        return format!(
            "error [{}]: profile '{name}' is logged out\n\nRecovery\n  relay login {name}",
            error.code()
        );
    }
    let text = error.to_string();
    let (problem, recovery) = text
        .split_once("Run:\n")
        .map_or((text.as_str(), None), |(p, r)| (p.trim_end(), Some(r)));
    let mut lines = problem.lines();
    let mut result = format!(
        "error [{}]: {}",
        error.code(),
        lines.next().unwrap_or("Command failed")
    );
    for line in lines {
        result.push_str(&format!("\n  {line}"));
    }
    if let Some(recovery) = recovery {
        result.push_str("\n\nRecovery\n");
        result.push_str(
            &recovery
                .lines()
                .map(|line| format!("  {}", line.trim()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styling_policy_is_explicit_and_plain_by_default() {
        assert!(color_allowed(true, false, false, Some("xterm-256color")));
        for (tty, json, no_color, term) in [
            (false, false, false, None),
            (true, true, false, None),
            (true, false, true, None),
            (true, false, false, Some("dumb")),
        ] {
            assert!(!color_allowed(tty, json, no_color, term));
        }
        let plain =
            "Report\n\n✓ Ready\n! Needs login\n✗ Unavailable\n\nRecovery\n  relay login main";
        let styled = render(
            plain,
            Terminal {
                color: true,
                width: 80,
            },
        );
        assert!(styled.contains("\x1b[32m✓ Ready"));
        assert!(styled.contains("\x1b[33m! Needs login"));
        assert!(styled.contains("\x1b[31m✗ Unavailable"));
        assert!(styled.contains("\n  relay login main"));
        assert!(
            !render(
                plain,
                Terminal {
                    color: false,
                    width: 80
                }
            )
            .contains('\x1b')
        );
    }

    #[test]
    fn controls_bidi_bounds_and_narrow_prose() {
        let text = format!("\x1b\u{202e}{}", "long diagnostic ".repeat(100));
        let bounded = diagnostic(&text);
        assert!(bounded.chars().count() <= 360);
        assert!(!bounded.contains('\x1b') && !bounded.contains('\u{202e}'));
        let rendered = render(
            &format!("Warnings\n  {bounded}"),
            Terminal {
                color: false,
                width: 60,
            },
        );
        assert!(rendered.lines().all(|line| line.chars().count() <= 60));
        assert!(
            rendered
                .lines()
                .skip(2)
                .all(|line| line.starts_with("    "))
        );
    }
    #[test]
    fn auth_errors_keep_codes_and_plain_multiline_recovery_without_guessing() {
        let revoked = relay_core::Error::CodexUsageUnverified {
            profile:"codex-main".into(),detail:"authentication_revoked: HTTP 401; token_revoked\nThis isolated profile needs its own login. Run:\nrelay logout codex-main\nrelay login codex-main".into()};
        let report = error(&revoked);
        assert!(report.starts_with("error [codex_usage_unverified]:"));
        assert!(
            report.contains("\n\nRecovery\n  relay logout codex-main\n  relay login codex-main")
        );
        let styled = render(
            &report,
            Terminal {
                color: true,
                width: 60,
            },
        );
        for line in styled
            .lines()
            .filter(|line| line.trim_start().starts_with("relay "))
        {
            assert!(!line.contains('\x1b'));
        }
        let timeout = relay_core::Error::CodexUsageUnverified {
            profile: "main".into(),
            detail: "timeout: no structured response".into(),
        };
        assert!(!error(&timeout).contains("Recovery") && !error(&timeout).contains("relay login"));
        let shell = "Recovery\n  cd '/a long directory' && env CODEX_HOME='/a long isolated directory' codex";
        assert_eq!(
            render(
                shell,
                Terminal {
                    color: false,
                    width: 40
                }
            ),
            shell
        );
    }
}
