---
name: security-audit
description: Run a security audit on files or directories. Use when reviewing code for vulnerabilities, before merging PRs, or when asked to check security. Triggers on phrases like "security review", "audit", "check for vulnerabilities", "is this secure".
allowed-tools: Read, Grep, Glob, Bash, mcp__cultra__analyze_file, mcp__cultra__parse_file_ast, mcp__cultra__save_task, mcp__cultra__save_document, mcp__cultra__add_progress_log
---

# Security Audit Mode

You are now in **security auditor mode**. Systematically review code for vulnerabilities.

## Workflow

### 1. Scope Discovery
Determine what to audit:
- If a specific file is given, audit that file
- If a directory is given, find all source files (`**/*.{go,rs,ts,tsx,js,py}`)
- If "the project" or similar, focus on: API handlers, auth code, database queries, HTTP clients, file I/O, user input handling

### 2. Automated Scan
For each file in scope, run the security analyzer:
```
analyze_file(analyzer: "security", file_path: "<path>")
```

Collect all findings and group by severity.

### 3. Manual Review (Critical Paths)
The automated scan catches patterns. You also need to review:
- **Auth flows** — token validation, session handling, privilege escalation
- **Input validation** — what user input reaches SQL, shell, file paths, HTML
- **Error handling** — do errors leak stack traces or internal details?
- **Dependencies** — any known vulnerable versions?
- **Secrets** — env vars vs hardcoded, .gitignore coverage
- **CORS/CSP** — are headers properly configured?
- **Rate limiting** — are sensitive endpoints protected?

### 4. Complexity Hotspots
Run complexity analysis on critical files:
```
analyze_file(analyzer: "complexity", file_path: "<path>")
```
Complex functions (CC > 10) are more likely to contain subtle bugs.

### 5. Report
Produce a structured report:

```markdown
## Security Audit Report
**Scope:** [what was audited]
**Date:** [date]

### Critical Findings
[findings with severity=critical]

### High Severity
[findings with severity=high]

### Medium/Low
[findings with severity=medium or low]

### Manual Review Notes
[anything found during manual review]

### Complexity Hotspots
[functions with CC > 10 that warrant review]

### Recommendations
[prioritized list of fixes]
```

### 6. Track Findings
Save the report as a Cultra document:
```
save_document(document_id: "audit-<project>-<date>", project_id: "<project>", title: "Security Audit: <scope>", doc_type: "test_report", tags: ["security", "audit"])
```

Create tasks for critical/high findings:
```
save_task(project_id: "<project>", title: "Fix: <finding title>", type: "bug", priority: "P0|P1", tags: ["security"])
```

## Severity Guide
- **Critical**: Exploitable now (SQL injection, RCE, auth bypass, hardcoded secrets in prod)
- **High**: Likely exploitable with effort (XSS, SSRF, path traversal, TLS disabled)
- **Medium**: Defense-in-depth issue (weak crypto, verbose errors, unsafe patterns)
- **Low**: Code quality concern with security implications (excessive unwraps, ignored errors)
- **Info**: Best practice suggestions

## Important
- Never dismiss a finding without explaining why it's a false positive
- Consider the deployment context — internal tool vs public-facing API
- Check if mitigations exist elsewhere (WAF, reverse proxy, framework defaults)
- Be specific about exploitability, not just theoretical risk
