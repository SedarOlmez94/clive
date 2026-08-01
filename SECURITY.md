# Security Policy

## Supported Versions

Clive is currently pre-1.0 and evolving quickly. Security fixes are made
against the latest release on the `main` branch.

| Version | Supported          |
| ------- | ------------------- |
| latest  | :white_check_mark:  |
| < latest | :x:                 |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Instead, report it privately using one of these methods:

1. **GitHub Security Advisories** (preferred): open a
   [private security advisory](https://github.com/SedarOlmez94/clive/security/advisories/new)
   for this repository.
2. **Email**: sedarolmez@users.noreply.github.com — include a description of
   the issue, steps to reproduce, affected version(s), and potential impact.

You should expect an initial response within **5 business days**. We will work
with you to understand and validate the issue, develop a fix, and coordinate
disclosure. Please give us a reasonable amount of time to address the issue
before any public disclosure.

## Scope

Clive runs entirely on the user's local machine and talks to a local Ollama
instance over HTTP. Areas of particular security interest include:

- Command execution paths (e.g. `--allow-agent-commands`, shell invocations
  for `ollama pull`/`serve`)
- File read/write and patch-apply logic (path traversal, unintended
  overwrites)
- Handling of untrusted model output that could influence file edits or
  shell commands
- Configuration and credential handling (Ollama host URLs, env vars)

## Disclosure Policy

Once a fix is available, we will publish a new release and a corresponding
GitHub Security Advisory crediting the reporter (unless anonymity is
requested).
