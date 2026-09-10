use super::shell_single_quote;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// Values enter a separate shell through its environment, never as script text.
/// Support ordinary shell words and quoting; reject syntax whose nested parsing
/// would require a full shell parser to identify placeholder quote contexts.
pub(super) fn substitute(template: &str, path: &str, base: Option<&str>) -> Result<String, String> {
    if !template.contains("{path}") && !template.contains("{base}") {
        return Ok(template.to_string());
    }
    let mut script = String::new();
    let mut quote = Quote::None;
    let mut rest = template;
    while !rest.is_empty() {
        let placeholder = if rest.starts_with("{path}") {
            Some(("{path}", "GITPANE_LAUNCH_PATH"))
        } else if rest.starts_with("{base}") {
            if base.is_none() {
                return Err("command uses {base} but no review base is available".into());
            }
            Some(("{base}", "GITPANE_LAUNCH_BASE"))
        } else {
            None
        };
        if let Some((token, variable)) = placeholder {
            let expansion = format!("${{{variable}}}");
            match quote {
                Quote::None => script.push_str(&format!("\"{expansion}\"")),
                Quote::Double => script.push_str(&expansion),
                Quote::Single => script.push_str(&format!("'\"{expansion}\"'")),
            }
            rest = &rest[token.len()..];
            continue;
        }
        let ch = rest.chars().next().expect("nonempty template remainder");
        if quote == Quote::None
            && ch == '#'
            && script
                .chars()
                .last()
                .is_none_or(|c| c.is_whitespace() || ";|&()".contains(c))
        {
            let end = rest.find('\n').unwrap_or(rest.len());
            script.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if quote != Quote::Single {
            if rest.starts_with("$(")
                || rest.starts_with("${")
                || ch == '`'
                || quote == Quote::None && rest.starts_with("<<")
                || quote == Quote::None && (rest.starts_with("$'") || rest.starts_with("$\""))
            {
                return Err("commands with placeholders do not support nested shell substitutions, heredocs, or extended quoting; use a wrapper script".into());
            }
            if ch == '\\' {
                let escaped = &rest[1..];
                if escaped.starts_with("{path}") || escaped.starts_with("{base}") {
                    return Err("command placeholders cannot be backslash-escaped".into());
                }
                let Some(next) = escaped.chars().next() else {
                    return Err("command ends with an incomplete shell escape".into());
                };
                script.push(ch);
                script.push(next);
                rest = &escaped[next.len_utf8()..];
                continue;
            }
        }
        quote = match (quote, ch) {
            (Quote::None, '\'') => Quote::Single,
            (Quote::None, '"') => Quote::Double,
            (Quote::Single, '\'') | (Quote::Double, '"') => Quote::None,
            (current, _) => current,
        };
        script.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    if quote != Quote::None {
        return Err("command has an unterminated shell quote".into());
    }
    Ok(format!(
        "GITPANE_LAUNCH_PATH={} GITPANE_LAUNCH_BASE={} sh -c {}",
        shell_single_quote(path),
        shell_single_quote(base.unwrap_or_default()),
        shell_single_quote(&script),
    ))
}
