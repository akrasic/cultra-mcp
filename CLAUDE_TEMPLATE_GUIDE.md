# How to Use the CLAUDE.md Template

This guide explains how to adapt `CLAUDE.md.TEMPLATE` for your own projects.

---

## Quick Start (5 minutes)

1. **Copy the template:**
   ```bash
   cp CLAUDE.md.TEMPLATE /path/to/your/project/CLAUDE.md
   ```

2. **Replace all placeholders:**
   - Search for `{{` in your editor
   - Replace each placeholder with actual values
   - Use your editor's find/replace feature

3. **Choose your feature set:**
   - Decide which optional sections apply to your project
   - Remove sections marked `[REMOVE IF NOT USING]`
   - Keep `[CORE]` sections (always applicable)

4. **Customize examples:**
   - Update code examples to match your tech stack
   - Replace generic API/framework references

5. **Test it:**
   - Start a Claude Code session in your project
   - Ask Claude to read CLAUDE.md
   - Give it a task and verify it follows the workflow

---

## Feature Decision Tree

Use this to determine which sections to keep:

### Core Workflows (ALWAYS KEEP)
✅ **Keep these sections:**
- Work Classification (3-task rule)
- Workflow Timing
- Documenting Plans
- Common Questions
- Quick Reference Card

**Why:** Universal best practices that work with any tooling.

---

### MCP Session Management (OPTIONAL)

**Do you have an MCP server with session/plan/task management tools?**

✅ **YES - Keep these sections:**
- Load/save session state
- MCP plan management tools
- MCP task management tools
- Document management tools
- Batch operations (reduces round-trips for multi-tool calls)
- Engine V3 features (if applicable)

❌ **NO - Remove and replace with:**
- Built-in Claude Code tools (`TaskCreate`, `TaskUpdate`, `EnterPlanMode`)
- Markdown-based session notes
- Git-based continuity (commits as boundaries)

**Example replacement:**
```markdown
## Session Start

Instead of:
load_session_state({project_id: "..."})

Use:
# Check git log and read SESSION_NOTES.md
git log --oneline -10
cat docs/SESSION_NOTES.md
```

---

### Code Intelligence (OPTIONAL)

**Do you want Claude to use AST/LSP tools for code understanding?**

✅ **YES - Keep these sections:**
- Code Intelligence Quick Start
- AST Workflow
- LSP Workflow
- LSP Prerequisites

**Prerequisites:**
- MCP server with AST parsing (for AST tools)
- Language servers installed (for LSP tools)

❌ **NO - Remove and rely on:**
- `Read` tool for reading files
- `Grep` tool for searching
- `Glob` tool for finding files

---

## Placeholder Reference

### Required Replacements

| Placeholder | Example | Description |
|-------------|---------|-------------|
| `{{PROJECT_NAME}}` | MyApp | Human-readable project name |
| `{{PROJECT_ID}}` | proj-myapp | Unique identifier (lowercase, hyphenated) |
| `{{PROJECT_TAGLINE}}` | Fast REST API | Short project description |
| `{{DATE}}` | Feb 5, 2026 | Current date |
| `{{MAIN_LANGUAGE}}` | go | Primary programming language |

### Optional Replacements (in examples)

| Placeholder | Example | Where Used |
|-------------|---------|-----------|
| `{{FRAMEWORK}}` | Fiber | Web framework |
| `{{DATABASE}}` | PostgreSQL | Database system |
| `{{FILE}}` | auth.go | File name |
| `{{LINE}}` | 45 | Line number |
| `{{FEATURE}}` | authentication | Feature name |
| `{{COMPONENT}}` | API | Component name |
| `{{EXT}}` | go | File extension |

---

## Customization Checklist

### Phase 1: Basic Setup (5 minutes)
- [ ] Copy template to your project as `CLAUDE.md`
- [ ] Replace all `{{PROJECT_*}}` placeholders
- [ ] Update `{{DATE}}`
- [ ] Set `{{MAIN_LANGUAGE}}`

### Phase 2: Feature Selection (10 minutes)
- [ ] Decide: Using MCP session tools? (YES/NO)
  - If NO: Remove MCP sections, add alternatives
- [ ] Decide: Using code intelligence tools? (YES/NO)
  - If NO: Remove AST/LSP sections
- [ ] Remove all `[REMOVE IF NOT USING]` sections you don't need
- [ ] Delete empty optional sections

### Phase 3: Examples (15 minutes)
- [ ] Update plan document structure for your domain
- [ ] Replace tech stack examples (Go → your language)
- [ ] Update framework/database references
- [ ] Add project-specific patterns

### Phase 4: Project Specifics (20 minutes)
- [ ] Fill in "Project-Specific Customizations" section
- [ ] Document your tech stack
- [ ] List coding standards
- [ ] Define domain terminology
- [ ] Add common patterns (how to add endpoint, component, etc.)

### Phase 5: Tune Thresholds (5 minutes)
- [ ] Review the "3-task rule" - adjust if needed (default: 3+)
- [ ] Adjust protocol modes (Lite vs Full) for your team
- [ ] Customize document types if needed

---

## Configuration Scenarios

### Scenario 1: Basic Project (No MCP Tools)

**What to keep:**
- Core workflows (3-task rule, timing, documentation)
- Claude Code built-in tools
- Markdown-based session notes

**What to remove:**
- All MCP tool sections
- Code intelligence sections
- Engine V3 features

**Setup time:** 15 minutes

**Example projects:** Small apps, personal projects, simple CLIs

---

### Scenario 2: Full MCP Integration

**What to keep:**
- Everything in the template
- MCP session management
- MCP plan/task/document tools
- Code intelligence (if language servers installed)

**What to remove:**
- Alternative methods (git-based notes, etc.)

**Setup time:** 10 minutes (mostly just placeholder replacement)

**Example projects:** This project (claude-session-manager), complex multi-session work

---

### Scenario 3: Hybrid Approach

**What to keep:**
- Core workflows
- Built-in Claude Code tools
- Code intelligence (LSP) for cross-file navigation
- Markdown docs for plans

**What to remove:**
- MCP session/plan/task tools
- AST tools (use LSP only)

**Setup time:** 20 minutes

**Example projects:** Medium complexity projects with good editor setup

---

## Tips for Customization

### 1. Start Minimal

Don't try to use every feature immediately. Start with:
- Core workflows (3-task rule)
- Basic task tracking (built-in tools or MCP)
- Session continuity (notes or MCP)

Add more features as you need them.

---

### 2. Match Your Team's Style

**Formal team?**
- Keep detailed documentation requirements
- Enforce plan templates strictly
- Require progress logs

**Casual team?**
- Simplify plan templates
- Make progress logs optional
- Focus on core workflow

---

### 3. Adjust Thresholds

The "3-task rule" is a guideline, not a law:
- **Increase to 4-5 tasks** if your team prefers less planning overhead
- **Decrease to 2 tasks** if continuity is critical and sessions are frequently interrupted
- **Keep at 3** for balanced approach (recommended)

---

### 4. Add Domain-Specific Patterns

Include common workflows for your domain:

**Web API project:**
```markdown
## Adding a New Endpoint

1. Define route in routes.go
2. Create handler in handlers/{{feature}}.go
3. Add request/response types in types/{{feature}}.go
4. Write tests in handlers/{{feature}}_test.go
5. Update API documentation
```

**React app:**
```markdown
## Adding a New Component

1. Create component file in src/components/{{Name}}.tsx
2. Add prop types interface
3. Implement component logic
4. Add Storybook story in {{Name}}.stories.tsx
5. Write tests in {{Name}}.test.tsx
```

---

### 5. Keep Examples Relevant

Replace generic examples with actual patterns from your codebase:

**Instead of:**
```javascript
// Generic example
task = save_task({title: "Fix bug"})
```

**Use:**
```javascript
// Your actual pattern
task = save_task({
  title: "Fix validation in UserService",
  type: "bug",
  component: "backend/services",
  priority: "high"
})
```

---

## Testing Your CLAUDE.md

After customization, test it:

1. **Start fresh session:**
   ```bash
   cd your-project
   claude
   ```

2. **Ask Claude to read the guide:**
   ```
   Read the CLAUDE.md file and summarize the workflow
   ```

3. **Give a test task:**
   ```
   Implement 5 new API endpoints for user management
   ```

4. **Verify Claude:**
   - ✅ Recognizes this as 3+ tasks (should create plan)
   - ✅ Uses correct tools (MCP or built-in)
   - ✅ Follows the plan → document → tasks sequence
   - ✅ Uses your project's naming conventions

5. **Check end of session:**
   - ✅ Saves state correctly (MCP or markdown notes)
   - ✅ Includes file paths in logs
   - ✅ Provides clear next action

---

## Common Mistakes to Avoid

### ❌ Leaving Too Many Optional Sections

**Problem:** Claude gets confused by sections that don't apply
**Fix:** Remove entire sections you don't use (not just placeholders)

### ❌ Generic Examples

**Problem:** Examples don't match your actual workflow
**Fix:** Replace ALL examples with your project's patterns

### ❌ Inconsistent Tool References

**Problem:** Mixing MCP tools and built-in tools incorrectly
**Fix:** Pick one approach and update all examples consistently

### ❌ Forgetting Prerequisites

**Problem:** References to tools that aren't installed
**Fix:** Remove code intelligence sections if language servers aren't installed

### ❌ Overly Complex Plans

**Problem:** Plan template is too detailed for your needs
**Fix:** Simplify to what you actually use (maybe just Goal + Tasks + Criteria)

---

## Maintenance

### When to Update CLAUDE.md

- ✅ Tech stack changes (new framework, database, etc.)
- ✅ Workflow improvements discovered
- ✅ Team feedback on what works/doesn't work
- ✅ New tools added (MCP server, language servers)
- ✅ Threshold adjustments (3-task rule, coverage targets)

### Keep a Changelog

Add to the Changelog section:
```markdown
# Changelog

**2026-02-05** - Initial setup from template
**2026-02-10** - Adjusted 3-task rule to 4+ for our team
**2026-02-15** - Added API endpoint pattern
**2026-03-01** - Enabled LSP tools (installed gopls)
```

---

## Getting Help

**If Claude isn't following the guide:**
1. Check that CLAUDE.md is in the project root
2. Ask Claude: "Have you read the CLAUDE.md file?"
3. Simplify the guide (might be too complex)
4. Remove conflicting instructions

**If tools aren't working:**
1. MCP tools: Verify MCP server is running
2. LSP tools: Check language server installation
3. Fall back to built-in tools

**If workflow feels wrong:**
1. Start with just the core workflows
2. Add features incrementally
3. Customize thresholds for your team

---

## Examples from Real Projects

### Example 1: Simple CLI Tool

**Removed:**
- All MCP tools
- All code intelligence
- Engine V3 features

**Kept:**
- 3-task rule (adjusted to 2+ for this small project)
- TaskCreate/TaskUpdate built-in tools
- Markdown session notes

**Customizations:**
- Simplified plan template (just Goal + Tasks)
- Added CLI testing pattern
- Removed database references

---

### Example 2: Large Web App with MCP

**Removed:**
- Nothing (using all features)

**Kept:**
- Everything

**Customizations:**
- Added React component patterns
- Defined API endpoint workflow
- Added accessibility checklist to plan template
- Adjusted coverage target to 85%

---

### Example 3: Data Science Project

**Removed:**
- MCP session tools (using Jupyter notebooks)
- Plan workflow (experiments are exploratory)

**Kept:**
- Basic task tracking (built-in tools)
- Code intelligence (Python LSP)

**Customizations:**
- Changed "3-task rule" to "3+ experiments"
- Added notebook naming conventions
- Added data validation patterns
- Plan template focuses on hypothesis/results

---

## Quick Reference: Template Sections

| Section | Type | Keep? |
|---------|------|-------|
| Philosophy | Core | ✅ Always |
| Quick Start | Core | ✅ Always (customize) |
| Work Classification | Core | ✅ Always |
| Protocol Modes | Core | ✅ Always |
| Workflow Timing | Core | ✅ Always |
| Documenting Plans | Core | ✅ Always |
| Session Workflow | Conditional | ✅ If using MCP, ⚠️ simplify otherwise |
| Code Intelligence | Optional | ✅ If using AST/LSP, ❌ remove otherwise |
| Batch Operations | Optional | ✅ If using MCP tools, ❌ remove otherwise |
| Tool Reference | Mixed | ✅ Keep Core, customize Optional |
| Common Questions | Core | ✅ Always |
| Quick Reference Card | Core | ✅ Always |
| Plan & Document Quality | Core | ✅ Always |
| Test-Driven Development | Core | ✅ Always |
| Project Customizations | Required | ✅ Always (MUST fill in) |

---

## Final Checklist

Before committing your CLAUDE.md:

- [ ] All `{{PLACEHOLDERS}}` replaced
- [ ] No `[REMOVE IF NOT USING]` tags left (unless intentional)
- [ ] All examples use your actual tech stack
- [ ] MCP tool sections match your setup (present or removed)
- [ ] Code intelligence sections match your setup (present or removed)
- [ ] Project-Specific Customizations section filled in
- [ ] Tested with Claude in a real session
- [ ] Team reviewed and approved (if applicable)
- [ ] Changelog updated with initial date

---

**You're ready!** Commit your CLAUDE.md and start using it.

**Pro tip:** After your first few sessions, review and refine. The best CLAUDE.md evolves with your project.
