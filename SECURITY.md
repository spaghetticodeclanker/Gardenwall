# Gardenwall AI Security Policy

## Security Philosophy

Gardenwall AI is designed under a zero-trust approach to model output and external content.

No model, prompt, retrieved webpage, document, or generated tool request is considered a trusted source of authorization.

Security controls must be enforced by trusted application components and, where necessary, operating-system and network isolation.

## Threats in Scope

- Malicious or poisoned language models
- Compromised model artifacts or inference runtimes
- Prompt injection through retrieved content
- Unauthorized command execution
- Credential and confidential-data exfiltration
- Unauthorized outbound network access
- Server-side request forgery (SSRF)
- Unauthorized application access
- Dependency and software supply-chain compromise

## Security Requirements

1. Models must not directly possess privileged execution or network capabilities.
2. All privileged operations must pass independent authorization.
3. External search queries and page requests require explicit user approval of each request, unless the user chooses a research grant for one research session. A research grant hands over permission to search and read public pages for that session; it is not approval of each request's exact contents.
4. Private conversation history must not automatically be transmitted to external services.
5. Secrets must not be embedded in source code, frontend assets, or Git history.
6. Security audit logs must minimize sensitive information.
7. Production inference must operate within a verified restricted environment.
8. Remote access must require authentication and protected transport.
9. Security claims must be supported by tests and deployment verification.

## Current Security Status

Gardenwall AI 0.1.0 is an early **preview** for Windows.

What is implemented and tested is listed in the README ("Status and features") and in the [security model](docs/public/SECURITY-MODEL.md), which explains each protection and its limits. The OS sandbox has escape test suites on Windows and Linux, a red-team suite runs scripted malicious models against the real agent, and the parsers of untrusted data are fuzzed.

The project does not yet claim:

- hardened protection against every malicious model or a compromised Gardenwall window: approvals are shown and decided in the main window, which renders no HTML or remote content but is not a separate trusted process (a separate approval window is planned for 0.2);
- that secret-file detection is complete: it is pattern-based, and secret files created while a session runs are found at the next check;
- that lockdown stops everything instantly: agent commands are killed at once, but it waits up to 10 seconds for agent turns to end and says so if one hasn't, and downloads stop at their next progress report;
- protection on macOS (unsupported, fails closed) or model loading on Linux (not finished);
- secure remote access (not built).

An outside review of the source (2026-11-23) found no critical issues; its concrete findings were fixed before release and its architectural recommendations are planned for a security hardening release, 0.2.

## Vulnerability Reporting

Please report security problems privately through GitHub: on the repository's **Security** tab, choose **Report a vulnerability**. The report is seen only by the maintainer and you. Please don't open a public issue, discussion or pull request for a security problem.

A useful report includes:

- the Gardenwall version (shown on the Updates page);
- what an attacker controls (a model, a model file, a web page, a document, a tool server…) and what they gain;
- steps to reproduce, ideally with a minimal example;
- a Copy diagnostics report if it helps (it is redacted, and you see all of it before you share it).

You can expect a first reply within a week. Please keep the details private until a fixed version is released; the fix's release notes will credit you if you'd like.

## Supported Versions

While Gardenwall is at 0.x, only the latest release gets security fixes. A fix ships as a new version through the signed updater; a published file is never replaced.

## Security Limitations

No application can guarantee protection against all malicious model artifacts, unknown software vulnerabilities, or compromised operating systems.

Gardenwall AI aims to reduce risk through independently enforced permissions, isolation, least privilege, and verifiable security controls.