---
name: code-review
description: Comprehensive code review combining AST analysis, security scanning, and complexity metrics. Use when reviewing PRs, before merging, or when asked to review code changes. Triggers on "review this", "code review", "review the PR", "look at these changes".
allowed-tools: Read, Grep, Glob, Bash, mcp__cultra__analyze_file, mcp__cultra__parse_file_ast, mcp__cultra__lsp, mcp__cultra__lsp_document_symbols, mcp__cultra__find_interface_implementations, mcp__cultra__save_decision
argument-hint: "[file_or_directory]"
---

# Code Review Mode

Systematic code review that goes beyond "looks good to me."

## Workflow

### 1. Understand the Change
```bash
# If reviewing a PR or branch
git diff main...HEAD --stat
git log main..HEAD --oneline
```
Read the commit messages. Understand the *intent* before reviewing the *code*.

### 2. Structural Analysis
For each changed file, run AST parsing to understand the shape:
```
parse_file_ast(file_path: "<path>")
```

This gives you: symbols, functions, imports, complexity metrics. Look for:
- New public API surface
- Changed function signatures
- New dependencies/imports

### 3. Security Scan
Run security analysis on changed files:
```
analyze_file(analyzer: "security", file_path: "<path>")
```

### 4. Complexity Check
Run complexity analysis:
```
analyze_file(analyzer: "complexity", file_path: "<path>")
```

Flag:
- Functions with cyclomatic complexity > 10
- Cognitive complexity > 15
- Functions longer than 50 lines

### 5. Language-Specific Analysis
- **Go files**: Run `analyze_file(analyzer: "concurrency")` if goroutines/channels present
- **React files**: Run `analyze_file(analyzer: "react")` for component structure
- **CSS files**: Run `analyze_file(analyzer: "css")` + `find_unused_selectors`

### 6. Manual Review Checklist
- [ ] Error handling: are errors propagated correctly?
- [ ] Edge cases: nil/null/empty/boundary conditions handled?
- [ ] Naming: are names clear and consistent with the codebase?
- [ ] Tests: are there tests for new/changed behavior?
- [ ] Backwards compatibility: does this break existing consumers?
- [ ] Performance: any obvious N+1 queries, unbounded loops, missing pagination?
- [ ] Documentation: is the public API documented where needed?

### 7. Report Format
```markdown
## Code Review: [scope]

### Summary
[1-2 sentence overview of the change]

### Findings

#### Must Fix
[issues that must be addressed before merge]

#### Should Fix
[improvements that are strongly recommended]

#### Consider
[suggestions for improvement, not blocking]

### Security
[results from security scan, or "No findings"]

### Complexity
[any functions above threshold]

### Verdict
[APPROVE / REQUEST_CHANGES / NEEDS_DISCUSSION]
```

## Principles
- Review the design, not just the syntax
- If the approach is wrong, say so early — don't nitpick formatting on a doomed PR
- Be specific: "this could NPE because X" not "watch out for nil"
- Suggest alternatives when rejecting an approach
- Acknowledge good decisions, not just problems
