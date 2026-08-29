use regex::Regex;

pub fn is_safe_command(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty()
        || command.contains('\n')
        || ['>', '|', ';', '`']
            .iter()
            .any(|character| command.contains(*character))
        || command.contains("&&")
        || command.contains("$(")
    {
        return false;
    }

    let arguments = command.split_whitespace().collect::<Vec<_>>();
    if arguments.iter().any(|argument| {
        matches!(
            *argument,
            "--delete" | "-delete" | "--force" | "-w" | "--output"
        ) || argument.starts_with("--output=")
    }) {
        return false;
    }

    const SAFE_PREFIXES: &[&str] = &[
        "ls",
        "dir",
        "pwd",
        "echo",
        "cat",
        "head",
        "tail",
        "wc",
        "which",
        "whereis",
        "whoami",
        "hostname",
        "uname",
        "date",
        "cal",
        "uptime",
        "df",
        "du",
        "free",
        "printenv",
        "file",
        "stat",
        "readlink",
        "realpath",
        "basename",
        "dirname",
        "sort",
        "uniq",
        "tr",
        "cut",
        "diff",
        "comm",
        "true",
        "false",
        "test",
        "git status",
        "git log",
        "git diff",
        "git show",
        "git rev-parse",
        "git ls-files",
        "git blame",
        "git stash list",
        "go version",
        "go env",
        "node -v",
        "npm -v",
        "python --version",
        "pip list",
        "cargo --version",
        "rustc --version",
    ];

    SAFE_PREFIXES.iter().any(|prefix| {
        command == *prefix
            || (command.starts_with(prefix)
                && command[prefix.len()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace))
    })
}

pub fn detect_dangerous_command(command: &str) -> Option<&'static str> {
    const DANGEROUS: &[(&str, &str)] = &[
        (
            r"rm\s+(-[a-z]*r[a-z]*f[a-z]*|-[a-z]*f[a-z]*r[a-z]*)\s+/\s*$",
            "recursive force delete of root",
        ),
        (r"mkfs\.", "format disk"),
        (r"dd\s+if=.*of=/dev/", "direct write to a disk device"),
        (r"chmod\s+-R\s+777\s+/", "recursive root permission change"),
        (r":\(\)\{\s*:\|:&\s*\};:", "fork bomb"),
        (
            r"curl\s+.*\|\s*(ba)?sh",
            "pipe a remote script into a shell",
        ),
        (
            r"wget\s+.*\|\s*(ba)?sh",
            "pipe a remote script into a shell",
        ),
        (r">\s*/dev/sd", "overwrite a disk device"),
    ];
    for (pattern, reason) in DANGEROUS {
        if Regex::new(pattern)
            .expect("dangerous command regex")
            .is_match(command)
        {
            return Some(reason);
        }
    }
    None
}
