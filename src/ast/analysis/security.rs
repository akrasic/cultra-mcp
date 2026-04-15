use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;

use crate::ast::util::detect_language;

/// Complete security analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAnalysis {
    pub file_path: String,
    pub language: String,
    pub findings: Vec<SecurityFinding>,
    pub summary: SecuritySummary,
}

/// Individual security finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityFinding {
    pub rule_id: String,
    pub category: String,
    pub severity: String, // "critical", "high", "medium", "low", "info"
    pub title: String,
    pub description: String,
    pub location: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    pub cwe: String,
}

/// Summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecuritySummary {
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub info: usize,
    pub categories: Vec<String>,
}

/// Pattern rule for security scanning
struct SecurityRule {
    id: &'static str,
    category: &'static str,
    severity: &'static str,
    title: &'static str,
    description: &'static str,
    cwe: &'static str,
    fix: &'static str,
    languages: &'static [&'static str], // empty = all languages
    patterns: &'static [&'static str],
    /// If true, finding requires the pattern to NOT be in a comment or string
    code_only: bool,
}

/// Analyze a source file for security vulnerabilities
pub fn analyze_security(file_path: &str) -> Result<SecurityAnalysis> {
    let content = fs::read_to_string(file_path)?;
    let raw_language = detect_language(file_path)
        .unwrap_or("unknown")
        .to_string();
    // Svelte scripts are TypeScript — match TypeScript security rules
    let language = if raw_language == "svelte" {
        "typescript".to_string()
    } else {
        raw_language
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();

    let rules = get_rules();

    for rule in &rules {
        // Check language filter
        if !rule.languages.is_empty() && !rule.languages.contains(&language.as_str()) {
            continue;
        }

        for pattern in rule.patterns {
            for (line_idx, line) in lines.iter().enumerate() {
                let line_num = (line_idx + 1) as u32;
                let trimmed = line.trim();

                // Skip comments
                if rule.code_only && is_comment(trimmed, &language) {
                    continue;
                }

                if line_contains_pattern(trimmed, pattern) {
                    findings.push(SecurityFinding {
                        rule_id: rule.id.to_string(),
                        category: rule.category.to_string(),
                        severity: rule.severity.to_string(),
                        title: rule.title.to_string(),
                        description: rule.description.to_string(),
                        location: format!("{}:{}", file_path, line_num),
                        line: line_num,
                        snippet: Some(trimmed.chars().take(200).collect()),
                        fix: if rule.fix.is_empty() {
                            None
                        } else {
                            Some(rule.fix.to_string())
                        },
                        cwe: rule.cwe.to_string(),
                    });
                    // Only report first occurrence per pattern per rule
                    break;
                }
            }
        }
    }

    // Deduplicate findings by rule_id + line
    findings.sort_by(|a, b| a.line.cmp(&b.line));
    findings.dedup_by(|a, b| a.rule_id == b.rule_id && a.line == b.line);

    // CULTRA-901: data-flow post-filter for SQL findings.
    // Drop SEC-SQL-001 / SEC-SQL-002 findings where the Sprintf result is
    // provably never used in a SQL execution sink (e.g., logged, returned
    // as JSON, used in error messages). Conservative: when uncertain, keep.
    findings = filter_sql_findings_by_dataflow(findings, &content, &language);

    // Run structural analysis (tree-sitter based)
    let structural_findings = structural_analysis(file_path, &content, &language)?;
    findings.extend(structural_findings);

    // Sort by severity (critical first)
    findings.sort_by(|a, b| severity_rank(&a.severity).cmp(&severity_rank(&b.severity)));

    let summary = build_summary(&findings);

    Ok(SecurityAnalysis {
        file_path: file_path.to_string(),
        language,
        findings,
        summary,
    })
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        "info" => 4,
        _ => 5,
    }
}

fn build_summary(findings: &[SecurityFinding]) -> SecuritySummary {
    let mut categories: Vec<String> = findings
        .iter()
        .map(|f| f.category.clone())
        .collect();
    categories.sort();
    categories.dedup();

    SecuritySummary {
        total_findings: findings.len(),
        critical: findings.iter().filter(|f| f.severity == "critical").count(),
        high: findings.iter().filter(|f| f.severity == "high").count(),
        medium: findings.iter().filter(|f| f.severity == "medium").count(),
        low: findings.iter().filter(|f| f.severity == "low").count(),
        info: findings.iter().filter(|f| f.severity == "info").count(),
        categories,
    }
}

fn is_comment(line: &str, language: &str) -> bool {
    match language {
        "go" | "rust" | "typescript" | "tsx" | "javascript" => {
            line.starts_with("//") || line.starts_with("/*") || line.starts_with('*')
        }
        "python" => line.starts_with('#'),
        _ => line.starts_with("//") || line.starts_with('#'),
    }
}

fn line_contains_pattern(line: &str, pattern: &str) -> bool {
    let lower = line.to_lowercase();
    let pat = pattern.to_lowercase();
    lower.contains(&pat)
}

/// Get all security rules
fn get_rules() -> Vec<SecurityRule> {
    vec![
        // === SQL Injection ===
        SecurityRule {
            id: "SEC-SQL-001",
            category: "sql-injection",
            severity: "critical",
            title: "Potential SQL injection via string concatenation",
            description: "SQL query built with string concatenation or formatting. User input may be interpolated directly into the query.",
            cwe: "CWE-89",
            fix: "Use parameterized queries or prepared statements instead of string concatenation.",
            languages: &[],
            patterns: &[
                // Trailing space anchors SQL keywords so English words like
                // "Deleted"/"Selected"/"Updates" don't match as substrings.
                "fmt.Sprintf(\"SELECT ",
                "fmt.Sprintf(\"INSERT ",
                "fmt.Sprintf(\"UPDATE ",
                "fmt.Sprintf(\"DELETE ",
                "fmt.Sprintf(\"DROP ",
                "f\"SELECT ",
                "f\"INSERT ",
                "f\"UPDATE ",
                "f\"DELETE ",
                "f'SELECT ",
                "f'INSERT ",
                "f'UPDATE ",
                "f'DELETE ",
                "+ \"SELECT ",
                "+ \"INSERT ",
                "+ \"UPDATE ",
                "+ \"DELETE ",
                "`SELECT ${",
                "`INSERT ${",
                "`UPDATE ${",
                "`DELETE ${",
            ],
            code_only: true,
        },
        SecurityRule {
            id: "SEC-SQL-002",
            category: "sql-injection",
            severity: "high",
            title: "Raw SQL query execution",
            description: "Direct execution of raw SQL strings. Ensure all parameters are properly escaped.",
            cwe: "CWE-89",
            fix: "Use an ORM or query builder with parameter binding.",
            languages: &[],
            patterns: &[
                ".Exec(fmt.",
                ".Query(fmt.",
                ".QueryRow(fmt.",
                "execute(f\"",
                "execute(f'",
                ".raw(\"SELECT ",
                ".raw(`SELECT ",
            ],
            code_only: true,
        },

        // === Command Injection ===
        SecurityRule {
            id: "SEC-CMD-001",
            category: "command-injection",
            severity: "critical",
            title: "Potential command injection",
            description: "Shell command built with string interpolation. User input may be injected into command execution.",
            cwe: "CWE-78",
            fix: "Use exec.Command with separate arguments instead of shell string interpolation. Never pass user input to shell commands.",
            languages: &[],
            patterns: &[
                "exec.Command(\"bash\", \"-c\"",
                "exec.Command(\"sh\", \"-c\"",
                "exec.Command(\"/bin/sh\"",
                "exec.Command(\"/bin/bash\"",
                "os.system(",
                "subprocess.call(f",
                "subprocess.run(f",
                "subprocess.Popen(f",
                "child_process.exec(",
                "child_process.execSync(",
                "shell=True",
            ],
            code_only: true,
        },

        // === Path Traversal ===
        SecurityRule {
            id: "SEC-PATH-001",
            category: "path-traversal",
            severity: "high",
            title: "Potential path traversal vulnerability",
            description: "File path constructed from user input without sanitization. Attackers may access files outside intended directory.",
            cwe: "CWE-22",
            fix: "Validate and sanitize file paths. Use filepath.Clean() and verify the resolved path stays within the expected directory.",
            languages: &[],
            patterns: &[
                "filepath.Join(r.",
                "filepath.Join(req.",
                "os.Open(r.",
                "os.Open(req.",
                "os.ReadFile(r.",
                "path.join(req.",
                "path.resolve(req.",
                "open(request.",
            ],
            code_only: true,
        },

        // === XSS ===
        SecurityRule {
            id: "SEC-XSS-001",
            category: "xss",
            severity: "high",
            title: "Potential cross-site scripting (XSS)",
            description: "User input rendered directly in HTML without escaping. May allow script injection.",
            cwe: "CWE-79",
            fix: "Use template auto-escaping or explicitly escape HTML output. Use textContent instead of innerHTML.",
            languages: &["typescript", "tsx", "javascript"],
            patterns: &[
                "innerHTML",
                "outerHTML",
                "document.write(",
                "dangerouslySetInnerHTML",
                ".insertAdjacentHTML(",
            ],
            code_only: true,
        },
        SecurityRule {
            id: "SEC-XSS-002",
            category: "xss",
            severity: "medium",
            title: "Unsafe HTML rendering in template",
            description: "Template renders raw/unescaped HTML content.",
            cwe: "CWE-79",
            fix: "Use safe rendering methods or sanitize HTML before rendering.",
            languages: &["go"],
            patterns: &[
                "template.HTML(",
                "template.JS(",
                "template.CSS(",
            ],
            code_only: true,
        },

        // === Hardcoded Secrets ===
        SecurityRule {
            id: "SEC-SECRET-001",
            category: "hardcoded-secrets",
            severity: "critical",
            title: "Potential hardcoded secret or credential",
            description: "What appears to be a secret, API key, or password is hardcoded in source code.",
            cwe: "CWE-798",
            fix: "Use environment variables or a secret management system. Never commit secrets to source code.",
            languages: &[],
            patterns: &[
                "password = \"",
                "password = '",
                "api_key = \"",
                "api_key = '",
                "apiKey = \"",
                "apiKey = '",
                "secret = \"",
                "secret = '",
                "token = \"",
                "token = '",
                "API_KEY = \"",
                "API_SECRET = \"",
                "SECRET_KEY = \"",
                "PRIVATE_KEY = \"",
                "AWS_ACCESS_KEY",
                "AWS_SECRET_KEY",
            ],
            code_only: true,
        },
        SecurityRule {
            id: "SEC-SECRET-002",
            category: "hardcoded-secrets",
            severity: "high",
            title: "Potential hardcoded connection string",
            description: "Database or service connection string with credentials appears hardcoded.",
            cwe: "CWE-798",
            fix: "Use environment variables for connection strings.",
            languages: &[],
            patterns: &[
                "postgres://",
                "mysql://",
                "mongodb://",
                "redis://",
                "amqp://",
            ],
            code_only: true,
        },

        // === Insecure Crypto ===
        SecurityRule {
            id: "SEC-CRYPTO-001",
            category: "insecure-crypto",
            severity: "high",
            title: "Use of weak or deprecated cryptographic algorithm",
            description: "MD5 and SHA1 are cryptographically broken and should not be used for security purposes.",
            cwe: "CWE-327",
            fix: "Use SHA-256 or stronger for hashing. Use bcrypt/scrypt/argon2 for passwords.",
            languages: &[],
            patterns: &[
                "crypto/md5",
                "md5.New(",
                "md5.Sum(",
                "hashlib.md5(",
                "crypto/sha1",
                "sha1.New(",
                "sha1.Sum(",
                "hashlib.sha1(",
                "createHash('md5')",
                "createHash(\"md5\")",
                "createHash('sha1')",
                "createHash(\"sha1\")",
            ],
            code_only: true,
        },

        // === Insecure TLS ===
        SecurityRule {
            id: "SEC-TLS-001",
            category: "insecure-tls",
            severity: "critical",
            title: "TLS certificate verification disabled",
            description: "TLS certificate verification is disabled, enabling man-in-the-middle attacks.",
            cwe: "CWE-295",
            fix: "Enable TLS certificate verification. Only disable for development/testing with clear flags.",
            languages: &[],
            patterns: &[
                "InsecureSkipVerify: true",
                "InsecureSkipVerify:true",
                "verify=False",
                "verify = False",
                "NODE_TLS_REJECT_UNAUTHORIZED",
                "rejectUnauthorized: false",
                "rejectUnauthorized:false",
                "danger_accept_invalid_certs(true)",
            ],
            code_only: true,
        },

        // === SSRF ===
        SecurityRule {
            id: "SEC-SSRF-001",
            category: "ssrf",
            severity: "high",
            title: "Potential server-side request forgery (SSRF)",
            description: "HTTP request URL constructed from user input. Attacker may redirect requests to internal services.",
            cwe: "CWE-918",
            fix: "Validate and whitelist allowed URLs/hosts. Block requests to internal networks (127.0.0.1, 10.x, 192.168.x, etc.).",
            languages: &[],
            patterns: &[
                "http.Get(r.",
                "http.Get(req.",
                "http.Post(r.",
                "http.Post(req.",
                "fetch(req.body",
                "fetch(req.query",
                "fetch(req.params",
                "requests.get(request.",
                "requests.post(request.",
                "urllib.request.urlopen(request.",
            ],
            code_only: true,
        },

        // === Unsafe Deserialization ===
        SecurityRule {
            id: "SEC-DESER-001",
            category: "unsafe-deserialization",
            severity: "high",
            title: "Unsafe deserialization of untrusted data",
            description: "Deserializing data from untrusted sources can lead to remote code execution.",
            cwe: "CWE-502",
            fix: "Validate and sanitize input before deserialization. Use safe deserialization methods.",
            languages: &["python"],
            patterns: &[
                "pickle.loads(",
                "pickle.load(",
                "yaml.load(",
                "marshal.loads(",
                "shelve.open(",
            ],
            code_only: true,
        },

        // === Information Exposure ===
        SecurityRule {
            id: "SEC-INFO-001",
            category: "information-exposure",
            severity: "medium",
            title: "Verbose error information exposed",
            description: "Stack traces or detailed error messages may be sent to clients, leaking internal implementation details.",
            cwe: "CWE-209",
            fix: "Log detailed errors server-side but return generic error messages to clients.",
            languages: &[],
            patterns: &[
                "debug.PrintStack()",
                "traceback.print_exc()",
                "e.printStackTrace()",
                "console.error(err.stack)",
            ],
            code_only: true,
        },

        // === CORS ===
        SecurityRule {
            id: "SEC-CORS-001",
            category: "cors-misconfiguration",
            severity: "high",
            title: "Overly permissive CORS configuration",
            description: "Allowing all origins (*) with credentials can expose the application to cross-origin attacks.",
            cwe: "CWE-942",
            fix: "Restrict Access-Control-Allow-Origin to specific trusted domains.",
            languages: &[],
            patterns: &[
                "Access-Control-Allow-Origin\", \"*\"",
                "Access-Control-Allow-Origin', '*'",
                "AllowAllOrigins: true",
                "AllowAllOrigins:true",
                "cors({origin: '*'",
                "cors({ origin: '*'",
                "cors({origin: true",
                "cors({ origin: true",
            ],
            code_only: true,
        },

        // === Rust-specific ===
        SecurityRule {
            id: "SEC-RUST-001",
            category: "unsafe-code",
            severity: "medium",
            title: "Unsafe Rust code block",
            description: "Unsafe blocks bypass Rust's safety guarantees. Review carefully for memory safety issues.",
            cwe: "CWE-119",
            fix: "Minimize unsafe code. Document safety invariants. Consider safe alternatives.",
            languages: &["rust"],
            patterns: &[
                "unsafe {",
                "unsafe fn ",
            ],
            code_only: true,
        },

        // === Go-specific ===
        SecurityRule {
            id: "SEC-GO-001",
            category: "error-handling",
            severity: "medium",
            title: "Ignored error return value",
            description: "Error return value is discarded. This may hide failures that have security implications.",
            cwe: "CWE-252",
            fix: "Always check error return values, especially for I/O, crypto, and auth operations.",
            languages: &["go"],
            patterns: &[
                "_ = http.",
                "_ = os.",
                "_ = io.",
                "_ = crypto.",
            ],
            code_only: true,
        },

        // === JWT/Auth ===
        SecurityRule {
            id: "SEC-AUTH-001",
            category: "authentication",
            severity: "critical",
            title: "JWT signed with none algorithm or weak key",
            description: "JWT using 'none' algorithm or hardcoded signing key. Tokens can be forged.",
            cwe: "CWE-347",
            fix: "Use RS256/ES256 with proper key management. Never use 'none' algorithm in production.",
            languages: &[],
            patterns: &[
                "\"alg\":\"none\"",
                "\"alg\": \"none\"",
                "algorithm=\"none\"",
                "algorithm='none'",
                "SigningMethodNone",
            ],
            code_only: true,
        },
        // === Terraform: Hardcoded Secrets ===
        SecurityRule {
            id: "SEC-TF-001",
            category: "hardcoded-secret",
            severity: "high",
            title: "Hardcoded secret in Terraform variable default",
            description: "Variable default value may contain a hardcoded password, key, or token. Secrets should come from environment variables, Vault, or SSM Parameter Store.",
            cwe: "CWE-798",
            fix: "Remove the default value and pass the secret via TF_VAR_ environment variable, terraform.tfvars (gitignored), or a secrets manager.",
            languages: &["terraform"],
            patterns: &[
                "default = \"AKIA",
                "default = \"sk-",
                "default = \"ghp_",
                "default = \"glpat-",
                "default = \"xoxb-",
                "default = \"xoxp-",
            ],
            code_only: true,
        },
        // === Terraform: Public Access ===
        SecurityRule {
            id: "SEC-TF-002",
            category: "network-exposure",
            severity: "high",
            title: "Overly permissive security group or network ACL",
            description: "Security group or NACL rule allows traffic from 0.0.0.0/0 (the entire internet). This may expose internal services.",
            cwe: "CWE-284",
            fix: "Restrict CIDR blocks to known IP ranges. Use VPN or bastion hosts for administrative access.",
            languages: &["terraform"],
            patterns: &[
                "cidr_blocks = [\"0.0.0.0/0\"]",
                "cidr_blocks      = [\"0.0.0.0/0\"]",
                "ipv6_cidr_blocks = [\"::/0\"]",
                "source_ranges = [\"0.0.0.0/0\"]",
            ],
            code_only: true,
        },
        // === Terraform: Public S3 / Storage ===
        SecurityRule {
            id: "SEC-TF-003",
            category: "storage-exposure",
            severity: "high",
            title: "Public ACL on storage bucket",
            description: "S3 bucket or storage resource configured with public ACL. Data may be exposed to the internet.",
            cwe: "CWE-284",
            fix: "Set acl to \"private\" and use bucket policies for controlled access. Enable S3 Block Public Access.",
            languages: &["terraform"],
            patterns: &[
                "acl = \"public-read\"",
                "acl    = \"public-read\"",
                "acl = \"public-read-write\"",
                "acl    = \"public-read-write\"",
                "acl = \"authenticated-read\"",
            ],
            code_only: true,
        },
        // === Terraform: Overly Permissive IAM ===
        SecurityRule {
            id: "SEC-TF-004",
            category: "iam-misconfiguration",
            severity: "critical",
            title: "Overly permissive IAM policy with wildcard actions or resources",
            description: "IAM policy grants wildcard (*) permissions on actions or resources. This violates the principle of least privilege.",
            cwe: "CWE-250",
            fix: "Scope actions and resources to the minimum required. Use specific service:Action patterns and resource ARNs.",
            languages: &["terraform"],
            patterns: &[
                "actions = [\"*\"]",
                "actions   = [\"*\"]",
                "resources = [\"*\"]",
                "resources   = [\"*\"]",
                "\"Action\": \"*\"",
                "\"Action\": [\"*\"]",
                "\"Resource\": \"*\"",
                "\"Resource\": [\"*\"]",
            ],
            code_only: true,
        },
        // === Terraform: Unencrypted Storage ===
        SecurityRule {
            id: "SEC-TF-005",
            category: "encryption",
            severity: "medium",
            title: "Storage encryption explicitly disabled",
            description: "Storage resource has encryption explicitly set to false. Data at rest should be encrypted.",
            cwe: "CWE-311",
            fix: "Set storage_encrypted = true or enable server-side encryption configuration.",
            languages: &["terraform"],
            patterns: &[
                "storage_encrypted = false",
                "storage_encrypted  = false",
                "encrypted = false",
                "encrypted  = false",
                "kms_encryption = false",
            ],
            code_only: true,
        },
    ]
}

/// CULTRA-901: filter SQL findings based on whether the Sprintf result
/// flows into a SQL execution sink. Currently Go-only (the dominant language
/// for false positives in this codebase). Other languages pass through unchanged.
///
/// Algorithm:
///   1. Parse the file with tree-sitter Go.
///   2. Find every `name := fmt.Sprintf(...)` short-var-declaration.
///   3. For each, locate the enclosing function and scan its body for any
///      call_expression matching a known SQL sink method (Exec, Query,
///      QueryRow, ExecContext, etc.) where the assigned variable name
///      appears as an argument identifier.
///   4. Build line → flow-status map for the Sprintf assignments.
///   5. Drop SQL findings whose line maps to NoSinkUsage. Keep findings
///      whose line is unknown (inline expressions, complex assignments)
///      so we err on the side of false positives.
fn filter_sql_findings_by_dataflow(
    findings: Vec<SecurityFinding>,
    content: &str,
    language: &str,
) -> Vec<SecurityFinding> {
    match language {
        "go" => filter_sql_findings_by_dataflow_go(findings, content),
        "rust" => filter_sql_findings_by_dataflow_rust(findings, content),
        _ => findings,
    }
}

fn filter_sql_findings_by_dataflow_go(
    findings: Vec<SecurityFinding>,
    content: &str,
) -> Vec<SecurityFinding> {
    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return findings;
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return findings,
    };

    let bytes = content.as_bytes();
    let mut line_status: std::collections::HashMap<u32, bool> = std::collections::HashMap::new();
    walk_for_sprintf_dataflow(tree.root_node(), bytes, &mut line_status);

    findings
        .into_iter()
        .filter(|f| {
            // Only the regex-based SQL rules participate in this filter.
            // Structural findings (SEC-SQL-AST-*) and other categories pass through.
            if f.rule_id != "SEC-SQL-001" && f.rule_id != "SEC-SQL-002" {
                return true;
            }
            match line_status.get(&f.line) {
                // Confirmed flows to a SQL sink → keep.
                Some(true) => true,
                // Assigned but no sink usage → drop (proven safe).
                Some(false) => false,
                // Unknown (inline, multi-assign, parser miss) → conservative keep.
                None => true,
            }
        })
        .collect()
}

/// CULTRA-942: Rust-side dataflow filter for SQL findings. Uses a different
/// heuristic than the Go version: rather than tracking variable bindings to
/// sinks (which wouldn't catch the security.rs self-scan case — pattern
/// literals live inside an array initializer, not a variable), we ask the
/// simpler question "does the enclosing function ever call a SQL sink?"
///
/// Algorithm:
///   1. Parse the file with tree-sitter-rust.
///   2. Walk every function_item, recording (line_range, contains_sql_sink).
///   3. For each SEC-SQL-001/002 finding:
///        - Find the function containing the finding line.
///        - If the function exists AND contains no SQL sink → DROP (proven safe).
///        - If line is module-level (outside any function) → DROP (can't execute).
///        - Otherwise (function contains a sink) → KEEP.
///
/// Known SQL sink method names (Rust ecosystem):
///   execute, execute_batch, query, query_row, query_map, query_one, query_opt,
///   query_scalar, fetch_one, fetch_all, fetch_optional, prepare, prepare_cached,
///   sql_query, raw_sql, exec, bind.
///
/// This is more aggressive than the Go version — it's willing to drop findings
/// inside functions that don't call SQL at all — because Rust's stronger typing
/// and the structure of the false-positive cases we've seen make the trade-off
/// favorable. Real SQL injection in Rust almost always co-locates the string
/// construction with the sink call in the same function body.
fn filter_sql_findings_by_dataflow_rust(
    findings: Vec<SecurityFinding>,
    content: &str,
) -> Vec<SecurityFinding> {
    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return findings;
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return findings,
    };

    let bytes = content.as_bytes();
    // (start_line, end_line, contains_sink)
    let mut function_sinks: Vec<(u32, u32, bool)> = Vec::new();
    collect_rust_function_sinks(tree.root_node(), bytes, &mut function_sinks);

    findings
        .into_iter()
        .filter(|f| {
            // Only SQL rules participate.
            if f.rule_id != "SEC-SQL-001" && f.rule_id != "SEC-SQL-002" {
                return true;
            }
            // Find the innermost function containing this line.
            let containing = function_sinks
                .iter()
                .filter(|(s, e, _)| f.line >= *s && f.line <= *e)
                .min_by_key(|(s, e, _)| e - s);
            match containing {
                // Inside a function that calls a sink → real injection risk, keep.
                Some((_, _, true)) => true,
                // Inside a function that NEVER calls a SQL sink → drop.
                Some((_, _, false)) => false,
                // Module-level (not inside any function) → drop.
                // Pattern literals, const declarations, static arrays, etc.
                None => false,
            }
        })
        .collect()
}

/// Recursively collect every function_item / closure_expression in the tree.
/// For each, record (start_line, end_line, whether_any_SQL_sink_call_appears_in_body).
fn collect_rust_function_sinks(
    node: tree_sitter::Node,
    bytes: &[u8],
    out: &mut Vec<(u32, u32, bool)>,
) {
    if node.kind() == "function_item" || node.kind() == "closure_expression" {
        let start = (node.start_position().row + 1) as u32;
        let end = (node.end_position().row + 1) as u32;
        let contains_sink = function_body_contains_rust_sql_sink(node, bytes);
        out.push((start, end, contains_sink));
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_function_sinks(child, bytes, out);
    }
}

/// True if the subtree rooted at `node` contains any call_expression whose
/// function path's final segment matches a known Rust SQL sink method.
fn function_body_contains_rust_sql_sink(node: tree_sitter::Node, bytes: &[u8]) -> bool {
    if node.kind() == "call_expression" {
        if call_expr_is_rust_sql_sink(node, bytes) {
            return true;
        }
    }
    // Macro invocations can also be sinks: sqlx::query!, diesel::sql_query!, etc.
    if node.kind() == "macro_invocation" {
        if let Some(name_node) = node.child_by_field_name("macro") {
            let name = node_text_rust(name_node, bytes);
            if is_rust_sql_sink_macro(&name) {
                return true;
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if function_body_contains_rust_sql_sink(child, bytes) {
            return true;
        }
    }
    false
}

/// True if a call_expression's callee is a known SQL sink. Matches by final
/// method name only (any receiver) — catches `tx.execute`, `conn.query_row`,
/// `pool.fetch_one`, etc.
fn call_expr_is_rust_sql_sink(call: tree_sitter::Node, bytes: &[u8]) -> bool {
    let func = match call.child_by_field_name("function") {
        Some(f) => f,
        None => return false,
    };
    // Method call: (call_expression function: (field_expression field: (field_identifier)))
    // Freestanding call: (call_expression function: (scoped_identifier name: (identifier)))
    // Bare call: (call_expression function: (identifier))
    let final_name = match func.kind() {
        "field_expression" => func
            .child_by_field_name("field")
            .map(|n| node_text_rust(n, bytes)),
        "scoped_identifier" => func
            .child_by_field_name("name")
            .map(|n| node_text_rust(n, bytes)),
        "identifier" => Some(node_text_rust(func, bytes)),
        _ => None,
    };
    final_name.map(|n| is_rust_sql_sink_method(&n)).unwrap_or(false)
}

fn is_rust_sql_sink_method(name: &str) -> bool {
    matches!(
        name,
        "execute"
            | "execute_batch"
            | "query"
            | "query_row"
            | "query_map"
            | "query_one"
            | "query_opt"
            | "query_scalar"
            | "fetch_one"
            | "fetch_all"
            | "fetch_optional"
            | "prepare"
            | "prepare_cached"
            | "sql_query"
            | "raw_sql"
            | "exec"
            | "exec_batch"
    )
}

fn is_rust_sql_sink_macro(name: &str) -> bool {
    // Match "query", "query!", "sql_query", etc. — also qualified forms like "sqlx::query".
    let bare = name.rsplit("::").next().unwrap_or(name);
    matches!(bare, "query" | "query_as" | "query_scalar" | "sql_query")
}

fn node_text_rust(n: tree_sitter::Node, bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[n.byte_range()]).into_owned()
}

/// Recursively walks a Go AST node looking for `name := fmt.Sprintf(...)`
/// short-var-declarations. For each match, records (line, reaches_sink) in
/// `line_status`. Sink lookup is scoped to the enclosing function body.
fn walk_for_sprintf_dataflow(
    node: tree_sitter::Node,
    bytes: &[u8],
    line_status: &mut std::collections::HashMap<u32, bool>,
) {
    // function_declaration / method_declaration are the scopes for sink scans.
    if node.kind() == "function_declaration" || node.kind() == "method_declaration" {
        if let Some(body) = node.child_by_field_name("body") {
            collect_sprintf_assignments_in_body(body, bytes, line_status);
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_sprintf_dataflow(child, bytes, line_status);
    }
}

/// Within a single function body, find all `name := fmt.Sprintf(...)` and
/// determine whether `name` flows into a SQL sink elsewhere in the same body.
fn collect_sprintf_assignments_in_body(
    body: tree_sitter::Node,
    bytes: &[u8],
    line_status: &mut std::collections::HashMap<u32, bool>,
) {
    // Pass 1: collect Sprintf assignments (line, var_name).
    let mut sprintfs: Vec<(u32, String)> = Vec::new();
    collect_sprintf_assigns(body, bytes, &mut sprintfs);

    if sprintfs.is_empty() {
        return;
    }

    // Pass 2: collect every SQL sink call in the body and the set of
    // identifier names that appear in their argument lists.
    let mut sink_arg_idents: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_sink_arg_idents(body, bytes, &mut sink_arg_idents);

    // For each Sprintf assignment, check whether the assigned name appears
    // as an argument to any sink call in this function.
    for (line, var_name) in sprintfs {
        let reaches_sink = sink_arg_idents.contains(&var_name);
        line_status.insert(line, reaches_sink);
    }
}

/// Recursively find `name := fmt.Sprintf(...)` patterns. Records (line, name).
/// Only handles single-LHS short_var_declaration. Multi-LHS or
/// long-form assignments are skipped (treated as unknown by the filter).
fn collect_sprintf_assigns(
    node: tree_sitter::Node,
    bytes: &[u8],
    out: &mut Vec<(u32, String)>,
) {
    if node.kind() == "short_var_declaration" {
        if let Some(var_name) = extract_single_lhs_name(node, bytes) {
            if let Some(rhs) = node.child_by_field_name("right") {
                if rhs_is_fmt_sprintf(rhs, bytes) {
                    let line = (node.start_position().row + 1) as u32;
                    out.push((line, var_name));
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_sprintf_assigns(child, bytes, out);
    }
}

/// Extract the LHS identifier of a `name := ...` short_var_declaration.
/// Returns None for multi-LHS (`a, b := ...`) or non-identifier targets.
fn extract_single_lhs_name(
    decl: tree_sitter::Node,
    bytes: &[u8],
) -> Option<String> {
    let lhs = decl.child_by_field_name("left")?;
    // expression_list with exactly one identifier child
    let mut named_count = 0;
    let mut name = None;
    let mut cursor = lhs.walk();
    for child in lhs.named_children(&mut cursor) {
        named_count += 1;
        if child.kind() == "identifier" {
            name = Some(node_text(child, bytes));
        }
    }
    if named_count == 1 {
        name
    } else {
        None
    }
}

/// Check whether a `right` expression_list contains exactly one
/// `fmt.Sprintf(...)` call_expression.
fn rhs_is_fmt_sprintf(rhs: tree_sitter::Node, bytes: &[u8]) -> bool {
    let mut cursor = rhs.walk();
    let mut named: Vec<tree_sitter::Node> = rhs.named_children(&mut cursor).collect();
    if named.len() != 1 {
        return false;
    }
    let call = named.remove(0);
    if call.kind() != "call_expression" {
        return false;
    }
    let func = match call.child_by_field_name("function") {
        Some(f) => f,
        None => return false,
    };
    if func.kind() != "selector_expression" {
        return false;
    }
    let pkg = func
        .child_by_field_name("operand")
        .map(|n| node_text(n, bytes));
    let method = func
        .child_by_field_name("field")
        .map(|n| node_text(n, bytes));
    matches!((pkg.as_deref(), method.as_deref()), (Some("fmt"), Some("Sprintf")))
}

/// Recursively walks a function body collecting identifier names that
/// appear as arguments to known SQL sink calls.
fn collect_sink_arg_idents(
    node: tree_sitter::Node,
    bytes: &[u8],
    out: &mut std::collections::HashSet<String>,
) {
    if node.kind() == "call_expression" {
        if call_is_sql_sink(node, bytes) {
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for arg in args.named_children(&mut cursor) {
                    collect_identifiers(arg, bytes, out);
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_sink_arg_idents(child, bytes, out);
    }
}

/// Known SQL sink methods. Receivers are intentionally not checked — any
/// method on any value with one of these names is treated as a sink. This
/// catches db.Exec, pool.Query, tx.QueryRow, conn.ExecContext, dbq.Exec, etc.
fn call_is_sql_sink(call: tree_sitter::Node, bytes: &[u8]) -> bool {
    let func = match call.child_by_field_name("function") {
        Some(f) => f,
        None => return false,
    };
    if func.kind() != "selector_expression" {
        return false;
    }
    let method = match func.child_by_field_name("field") {
        Some(f) => node_text(f, bytes),
        None => return false,
    };
    matches!(
        method.as_str(),
        "Exec"
            | "ExecContext"
            | "Query"
            | "QueryContext"
            | "QueryRow"
            | "QueryRowContext"
    )
}

/// Recursively collect identifier names from an argument subtree.
fn collect_identifiers(
    node: tree_sitter::Node,
    bytes: &[u8],
    out: &mut std::collections::HashSet<String>,
) {
    if node.kind() == "identifier" {
        out.insert(node_text(node, bytes));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifiers(child, bytes, out);
    }
}

fn node_text(n: tree_sitter::Node, bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[n.byte_range()]).into_owned()
}

/// Structural analysis using tree-sitter for patterns that need AST context
fn structural_analysis(
    file_path: &str,
    content: &str,
    language: &str,
) -> Result<Vec<SecurityFinding>> {
    let mut findings = Vec::new();

    match language {
        "go" => {
            findings.extend(go_structural_analysis(file_path, content)?);
        }
        "typescript" | "tsx" | "javascript" => {
            findings.extend(js_structural_analysis(file_path, content)?);
        }
        "python" => {
            findings.extend(python_structural_analysis(file_path, content)?);
        }
        "rust" => {
            findings.extend(rust_structural_analysis(file_path, content)?);
        }
        "terraform" => {
            findings.extend(terraform_structural_analysis(file_path, content)?);
        }
        _ => {}
    }

    Ok(findings)
}

/// Go-specific structural security analysis
fn go_structural_analysis(file_path: &str, content: &str) -> Result<Vec<SecurityFinding>> {
    let mut findings = Vec::new();
    let content_bytes = content.as_bytes();

    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    parser.set_language(&lang)?;

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Ok(findings),
    };

    let root = tree.root_node();

    // Detect sql.DB.Exec/Query with string concatenation
    let query_src = r#"
    (call_expression
        function: (selector_expression
            field: (field_identifier) @method (#match? @method "^(Exec|Query|QueryRow)$"))
        arguments: (argument_list
            (binary_expression
                operator: "+" ) @concat))
    "#;

    if let Ok(query) = tree_sitter::Query::new(&lang, query_src) {
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, content_bytes);
        use streaming_iterator::StreamingIterator;
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = &query.capture_names()[cap.index as usize];
                if *name == "concat" {
                    let line = cap.node.start_position().row + 1;
                    findings.push(SecurityFinding {
                        rule_id: "SEC-SQL-AST-001".to_string(),
                        category: "sql-injection".to_string(),
                        severity: "critical".to_string(),
                        title: "SQL query with string concatenation in arguments".to_string(),
                        description: "Database query method called with concatenated string argument. This is a strong indicator of SQL injection vulnerability.".to_string(),
                        location: format!("{}:{}", file_path, line),
                        line: line as u32,
                        snippet: Some(
                            content_bytes[cap.node.byte_range()]
                                .iter()
                                .map(|&b| b as char)
                                .take(200)
                                .collect(),
                        ),
                        fix: Some("Use parameterized queries: db.Query(\"SELECT * FROM users WHERE id = $1\", id)".to_string()),
                        cwe: "CWE-89".to_string(),
                    });
                }
            }
        }
    }

    Ok(findings)
}

/// JavaScript/TypeScript structural security analysis
fn js_structural_analysis(file_path: &str, content: &str) -> Result<Vec<SecurityFinding>> {
    let mut findings = Vec::new();
    let content_bytes = content.as_bytes();

    let mut parser = tree_sitter::Parser::new();
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("js");

    let lang: tree_sitter::Language = match ext {
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => tree_sitter_javascript::LANGUAGE.into(),
    };
    parser.set_language(&lang)?;

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Ok(findings),
    };

    let root = tree.root_node();

    // Detect eval() usage
    let query_src = r#"
    (call_expression
        function: (identifier) @fn (#eq? @fn "eval")
        arguments: (arguments) @args)
    "#;

    if let Ok(query) = tree_sitter::Query::new(&lang, query_src) {
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, content_bytes);
        use streaming_iterator::StreamingIterator;
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = &query.capture_names()[cap.index as usize];
                if *name == "fn" {
                    let line = cap.node.start_position().row + 1;
                    findings.push(SecurityFinding {
                        rule_id: "SEC-EVAL-001".to_string(),
                        category: "code-injection".to_string(),
                        severity: "critical".to_string(),
                        title: "Use of eval()".to_string(),
                        description: "eval() executes arbitrary code. If user input reaches eval(), it enables remote code execution.".to_string(),
                        location: format!("{}:{}", file_path, line),
                        line: line as u32,
                        snippet: None,
                        fix: Some("Replace eval() with safe alternatives like JSON.parse() for data or a sandboxed execution environment.".to_string()),
                        cwe: "CWE-94".to_string(),
                    });
                }
            }
        }
    }

    // Detect Function constructor (equivalent to eval)
    let fn_constructor_query = r#"
    (new_expression
        constructor: (identifier) @ctor (#eq? @ctor "Function"))
    "#;

    if let Ok(query) = tree_sitter::Query::new(&lang, fn_constructor_query) {
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, content_bytes);
        use streaming_iterator::StreamingIterator;
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = &query.capture_names()[cap.index as usize];
                if *name == "ctor" {
                    let line = cap.node.start_position().row + 1;
                    findings.push(SecurityFinding {
                        rule_id: "SEC-EVAL-002".to_string(),
                        category: "code-injection".to_string(),
                        severity: "high".to_string(),
                        title: "Use of Function constructor".to_string(),
                        description: "new Function() is equivalent to eval() and can execute arbitrary code.".to_string(),
                        location: format!("{}:{}", file_path, line),
                        line: line as u32,
                        snippet: None,
                        fix: Some("Avoid dynamic code generation. Use static function definitions.".to_string()),
                        cwe: "CWE-94".to_string(),
                    });
                }
            }
        }
    }

    Ok(findings)
}

/// Python structural security analysis
fn python_structural_analysis(file_path: &str, content: &str) -> Result<Vec<SecurityFinding>> {
    let mut findings = Vec::new();
    let content_bytes = content.as_bytes();

    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    parser.set_language(&lang)?;

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Ok(findings),
    };

    let root = tree.root_node();

    // Detect eval/exec usage
    let query_src = r#"
    (call
        function: (identifier) @fn (#match? @fn "^(eval|exec|compile)$")
        arguments: (argument_list) @args)
    "#;

    if let Ok(query) = tree_sitter::Query::new(&lang, query_src) {
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, content_bytes);
        use streaming_iterator::StreamingIterator;
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = &query.capture_names()[cap.index as usize];
                if *name == "fn" {
                    let fn_name = std::str::from_utf8(
                        &content_bytes[cap.node.byte_range()],
                    )
                    .unwrap_or("eval");
                    let line = cap.node.start_position().row + 1;
                    findings.push(SecurityFinding {
                        rule_id: "SEC-PYEVAL-001".to_string(),
                        category: "code-injection".to_string(),
                        severity: "critical".to_string(),
                        title: format!("Use of {}()", fn_name),
                        description: format!(
                            "{}() executes arbitrary Python code. If user input reaches it, attackers can execute arbitrary code on the server.",
                            fn_name
                        ),
                        location: format!("{}:{}", file_path, line),
                        line: line as u32,
                        snippet: None,
                        fix: Some("Use ast.literal_eval() for data parsing, or avoid dynamic code execution entirely.".to_string()),
                        cwe: "CWE-94".to_string(),
                    });
                }
            }
        }
    }

    Ok(findings)
}

/// Rust structural security analysis
fn rust_structural_analysis(file_path: &str, content: &str) -> Result<Vec<SecurityFinding>> {
    let mut findings = Vec::new();
    let content_bytes = content.as_bytes();

    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser.set_language(&lang)?;

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Ok(findings),
    };

    let root = tree.root_node();

    // CULTRA-957: detect unwrap()/expect() calls in PRODUCTION code only.
    // Walks up from each match to skip anything inside a `#[cfg(test)]`
    // mod or `#[test]`/`#[tokio::test]` function. Reports per-location
    // (one finding per call site) instead of one aggregate at line 1, so
    // the user can see exactly which production unwraps to fix without
    // re-grepping the file.
    let query_src = r#"
    (call_expression
        function: (field_expression
            field: (field_identifier) @method (#match? @method "^(unwrap|expect)$"))) @call
    "#;

    if let Ok(query) = tree_sitter::Query::new(&lang, query_src) {
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, content_bytes);
        use streaming_iterator::StreamingIterator;
        while let Some(m) = matches.next() {
            // Find the @call capture (the whole call_expression node) so we
            // can ask its parent chain about test context.
            let call_node = m.captures.iter()
                .find(|cap| query.capture_names()[cap.index as usize] == "call")
                .map(|cap| cap.node);
            let method_node = m.captures.iter()
                .find(|cap| query.capture_names()[cap.index as usize] == "method")
                .map(|cap| cap.node);
            let (Some(call), Some(method)) = (call_node, method_node) else { continue };

            if rust_node_is_in_test_context(call, content_bytes) {
                continue;
            }

            let line: u32 = (method.start_position().row + 1) as u32;
            let method_name = node_text(method, content_bytes);
            findings.push(SecurityFinding {
                rule_id: "SEC-RUST-002".to_string(),
                category: "robustness".to_string(),
                severity: "low".to_string(),
                title: format!("unwrap/expect in production code ({}())", method_name),
                description: format!(
                    "{}() in production code is a potential panic point. \
                     Test-context calls are excluded from this finding (CULTRA-957) — \
                     this is a production-code instance.",
                    method_name
                ),
                location: format!("{}:{}", file_path, line),
                line,
                snippet: None,
                fix: Some("Use proper error handling with ? operator or match. Reserve unwrap() for cases where failure is logically impossible.".to_string()),
                cwe: "CWE-248".to_string(),
            });
        }
    }

    Ok(findings)
}

/// CULTRA-957: walk up from `node` looking for an enclosing function_item
/// or mod_item that has a test-context attribute (`#[cfg(test)]`,
/// `#[test]`, `#[tokio::test]`, etc). Returns true if any ancestor matches.
///
/// Detection uses source-text lookback rather than walking attribute_item
/// siblings because tree-sitter-rust attaches outer attributes as preceding
/// siblings of the item, not as children — and structured sibling-walking
/// requires more book-keeping than the lookback covers in practice.
fn rust_node_is_in_test_context(node: tree_sitter::Node, content_bytes: &[u8]) -> bool {
    let mut current = node;
    loop {
        let parent = match current.parent() {
            Some(p) => p,
            None => return false,
        };
        if parent.kind() == "function_item" || parent.kind() == "mod_item" {
            if rust_item_has_test_attribute(parent, content_bytes) {
                return true;
            }
        }
        current = parent;
    }
}

/// CULTRA-957: source-text lookback to detect a test attribute attached to
/// `item`. Looks at up to 200 bytes preceding the item's start byte for
/// any of the recognized test-attribute patterns. Conservative: only
/// matches whole `#[...]` lines, ignores comments, bails on the first
/// non-attribute non-blank line.
fn rust_item_has_test_attribute(item: tree_sitter::Node, content_bytes: &[u8]) -> bool {
    let start = item.start_byte();
    let lookback_start = start.saturating_sub(400);
    let preceding = match std::str::from_utf8(&content_bytes[lookback_start..start]) {
        Ok(s) => s,
        Err(_) => return false,
    };
    // Walk preceding lines in reverse, accept blank/comment/attribute,
    // bail on anything else.
    for line in preceding.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        if !trimmed.starts_with("#[") && !trimmed.starts_with("#![") {
            return false;
        }
        // Found an attribute. Check if it's a test attribute.
        if trimmed.contains("cfg(test)")
            || trimmed.contains("#[test]")
            || trimmed.contains("test_case")
            || trimmed.contains("::test")
            || trimmed == "#[test]"
        {
            return true;
        }
        // Some other attribute (e.g. #[derive(...)]) — keep walking back
        // because the test attribute might be on an earlier line.
    }
    false
}

/// Terraform structural analysis — checks that need AST context beyond pattern matching
fn terraform_structural_analysis(file_path: &str, content: &str) -> Result<Vec<SecurityFinding>> {
    let mut findings = Vec::new();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_hcl::LANGUAGE.into())?;

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return Ok(findings),
    };

    // Check for missing versioning on S3 buckets
    // Look for aws_s3_bucket resources without a corresponding aws_s3_bucket_versioning
    let root = tree.root_node();
    let mut s3_buckets: Vec<(String, u32)> = Vec::new(); // (name, line)
    let mut has_versioning: std::collections::HashSet<String> = std::collections::HashSet::new();

    collect_tf_blocks(&root, content.as_bytes(), &mut |block_type, labels, line| {
        if block_type == "resource" {
            if let Some(rtype) = labels.first() {
                if rtype == "aws_s3_bucket" {
                    if let Some(name) = labels.get(1) {
                        s3_buckets.push((name.clone(), line));
                    }
                }
                if rtype == "aws_s3_bucket_versioning" {
                    if let Some(name) = labels.get(1) {
                        has_versioning.insert(name.clone());
                    }
                }
            }
        }
    });

    for (bucket_name, line) in &s3_buckets {
        // Heuristic: versioning resource often named with the bucket name
        let has_it = has_versioning.iter().any(|v| v.contains(bucket_name));
        if !has_it {
            findings.push(SecurityFinding {
                rule_id: "SEC-TF-006".to_string(),
                category: "storage-config".to_string(),
                severity: "medium".to_string(),
                title: "S3 bucket may be missing versioning".to_string(),
                description: format!(
                    "S3 bucket '{}' has no corresponding aws_s3_bucket_versioning resource. Versioning protects against accidental deletion.",
                    bucket_name
                ),
                location: file_path.to_string(),
                line: *line,
                snippet: None,
                cwe: "CWE-693".to_string(),
                fix: Some("Add an aws_s3_bucket_versioning resource with versioning_configuration { status = \"Enabled\" }.".to_string()),
            });
        }
    }

    Ok(findings)
}

/// Walk HCL tree and call callback for each top-level block
fn collect_tf_blocks(
    node: &tree_sitter::Node,
    content: &[u8],
    callback: &mut dyn FnMut(&str, Vec<String>, u32),
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "body" | "config_file" => collect_tf_blocks(&child, content, callback),
            "block" => {
                let mut block_cursor = child.walk();
                let children: Vec<_> = child.children(&mut block_cursor).collect();
                if let Some(first) = children.first() {
                    if first.kind() == "identifier" {
                        if let Ok(block_type) = first.utf8_text(content) {
                            let labels: Vec<String> = children
                                .iter()
                                .filter(|c| c.kind() == "string_lit")
                                .filter_map(|c| c.utf8_text(content).ok().map(|s| s.trim_matches('"').to_string()))
                                .collect();
                            let line = child.start_position().row as u32 + 1;
                            callback(block_type, labels, line);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_basic_pattern_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "import \"crypto/md5\"").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    query := fmt.Sprintf(\"SELECT * FROM users WHERE id = %s\", id)").unwrap();
        writeln!(f, "    db.Query(query)").unwrap();
        writeln!(f, "    password = \"hunter2\"").unwrap();
        writeln!(f, "}}").unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        assert!(result.findings.len() >= 2, "Expected at least 2 findings, got {}", result.findings.len());

        let categories: Vec<&str> = result.findings.iter().map(|f| f.category.as_str()).collect();
        assert!(categories.contains(&"sql-injection"), "Expected sql-injection finding");
        assert!(categories.contains(&"hardcoded-secrets"), "Expected hardcoded-secrets finding");
    }

    #[test]
    fn test_comment_skipping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "// password = \"not-a-real-password\"").unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        let secret_findings: Vec<&SecurityFinding> = result
            .findings
            .iter()
            .filter(|f| f.category == "hardcoded-secrets")
            .collect();
        assert!(secret_findings.is_empty(), "Comments should not trigger findings");
    }

    #[test]
    fn test_js_eval_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.js");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "const result = eval(userInput);").unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        let eval_findings: Vec<&SecurityFinding> = result
            .findings
            .iter()
            .filter(|f| f.rule_id == "SEC-EVAL-001")
            .collect();
        assert!(!eval_findings.is_empty(), "Expected eval() finding");
    }

    #[test]
    fn test_python_eval_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.py");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "result = eval(user_input)").unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        let eval_findings: Vec<&SecurityFinding> = result
            .findings
            .iter()
            .filter(|f| f.category == "code-injection")
            .collect();
        assert!(!eval_findings.is_empty(), "Expected code-injection finding");
    }

    #[test]
    fn test_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.rs");
        fs::File::create(&path).unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        assert_eq!(result.findings.len(), 0);
        assert_eq!(result.summary.total_findings, 0);
    }

    #[test]
    fn test_tls_skip_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "var tlsConfig = &tls.Config{{InsecureSkipVerify: true}}").unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        let tls_findings: Vec<&SecurityFinding> = result
            .findings
            .iter()
            .filter(|f| f.category == "insecure-tls")
            .collect();
        assert!(!tls_findings.is_empty(), "Expected TLS finding");
    }

    #[test]
    fn test_terraform_public_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.tf");
        std::fs::write(
            &path,
            r#"
resource "aws_security_group" "open" {
  ingress {
    cidr_blocks = ["0.0.0.0/0"]
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
  }
}
"#,
        )
        .unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        let net_findings: Vec<&SecurityFinding> = result
            .findings
            .iter()
            .filter(|f| f.category == "network-exposure")
            .collect();
        assert!(!net_findings.is_empty(), "Expected network-exposure finding for 0.0.0.0/0");
    }

    #[test]
    fn test_terraform_iam_wildcard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("iam.tf");
        std::fs::write(
            &path,
            r#"
resource "aws_iam_policy" "admin" {
  policy = jsonencode({
    Statement = [{
      Effect   = "Allow"
      "Action": "*"
      "Resource": "*"
    }]
  })
}
"#,
        )
        .unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        let iam_findings: Vec<&SecurityFinding> = result
            .findings
            .iter()
            .filter(|f| f.category == "iam-misconfiguration")
            .collect();
        assert!(!iam_findings.is_empty(), "Expected IAM wildcard finding");
    }

    #[test]
    fn test_terraform_s3_versioning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage.tf");
        std::fs::write(
            &path,
            r#"
resource "aws_s3_bucket" "data" {
  bucket = "my-data-bucket"
}
"#,
        )
        .unwrap();

        let result = analyze_security(path.to_str().unwrap()).unwrap();
        let versioning_findings: Vec<&SecurityFinding> = result
            .findings
            .iter()
            .filter(|f| f.rule_id == "SEC-TF-006")
            .collect();
        assert!(!versioning_findings.is_empty(), "Expected missing versioning finding");
    }

    fn sql_findings(path: &std::path::Path) -> Vec<SecurityFinding> {
        analyze_security(path.to_str().unwrap())
            .unwrap()
            .findings
            .into_iter()
            .filter(|f| f.rule_id == "SEC-SQL-001" || f.rule_id == "SEC-SQL-002")
            .collect()
    }

    #[test]
    fn test_deleted_english_word_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    msg := fmt.Sprintf(\"Deleted client %{}s\", id)", "").unwrap();
        writeln!(f, "    _ = msg").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(sql_findings(&path).is_empty(), "English word 'Deleted' must not flag SQL injection");
    }

    #[test]
    fn test_updated_english_word_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    msg := fmt.Sprintf(\"Updated record %{}s\", id)", "").unwrap();
        writeln!(f, "    _ = msg").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(sql_findings(&path).is_empty(), "English word 'Updated' must not flag SQL injection");
    }

    #[test]
    fn test_selected_english_word_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    msg := fmt.Sprintf(\"Selected option %{}s\", id)", "").unwrap();
        writeln!(f, "    _ = msg").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(sql_findings(&path).is_empty(), "English word 'Selected' must not flag SQL injection");
    }

    #[test]
    fn test_inserted_english_word_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    msg := fmt.Sprintf(\"Inserted row %{}s\", id)", "").unwrap();
        writeln!(f, "    _ = msg").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(sql_findings(&path).is_empty(), "English word 'Inserted' must not flag SQL injection");
    }

    #[test]
    fn test_dropped_english_word_not_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    msg := fmt.Sprintf(\"Dropped packet %{}s\", id)", "").unwrap();
        writeln!(f, "    _ = msg").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(sql_findings(&path).is_empty(), "English word 'Dropped' must not flag SQL injection");
    }

    #[test]
    fn test_real_delete_from_still_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    q := fmt.Sprintf(\"DELETE FROM users WHERE id = %{}d\", id)", "").unwrap();
        writeln!(f, "    db.Exec(q)").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(!sql_findings(&path).is_empty(), "Real DELETE FROM must still flag");
    }

    #[test]
    fn test_real_insert_into_still_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    q := fmt.Sprintf(\"INSERT INTO users VALUES (%{}s)\", v)", "").unwrap();
        writeln!(f, "    db.Exec(q)").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(!sql_findings(&path).is_empty(), "Real INSERT INTO must still flag");
    }

    #[test]
    fn test_real_update_set_still_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    q := fmt.Sprintf(\"UPDATE users SET name = %{}s\", v)", "").unwrap();
        writeln!(f, "    db.Exec(q)").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(!sql_findings(&path).is_empty(), "Real UPDATE must still flag");
    }

    #[test]
    fn test_real_select_from_still_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func main() {{").unwrap();
        writeln!(f, "    q := fmt.Sprintf(\"SELECT * FROM users WHERE id = %{}s\", id)", "").unwrap();
        writeln!(f, "    db.Query(q)").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(!sql_findings(&path).is_empty(), "Real SELECT must still flag");
    }

    // CULTRA-901: data-flow post-filter tests.

    #[test]
    fn test_dataflow_drops_sql_used_in_json_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func handler() {{").unwrap();
        writeln!(f, "    msg := fmt.Sprintf(\"DELETE FROM ghost rows = %{}d\", id)", "").unwrap();
        writeln!(f, "    c.JSON(map[string]string{{\"message\": msg}})").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(
            sql_findings(&path).is_empty(),
            "Sprintf result used only in JSON response should be dropped by dataflow filter"
        );
    }

    #[test]
    fn test_dataflow_drops_sql_used_in_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func handler() {{").unwrap();
        writeln!(f, "    line := fmt.Sprintf(\"INSERT INTO audit %{}s\", action)", "").unwrap();
        writeln!(f, "    log.Printf(line)").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(
            sql_findings(&path).is_empty(),
            "Sprintf result used only in log should be dropped"
        );
    }

    #[test]
    fn test_dataflow_drops_unused_sql_var() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func handler() {{").unwrap();
        writeln!(f, "    q := fmt.Sprintf(\"DELETE FROM ghosts WHERE id = %{}d\", id)", "").unwrap();
        writeln!(f, "    return").unwrap();
        writeln!(f, "}}").unwrap();
        // Variable q is never referenced anywhere in the body — proven safe.
        // (The Go compiler would normally reject this; some tests/codegen patterns produce it.)
        assert!(
            sql_findings(&path).is_empty(),
            "Sprintf assigned to var that's never used should be dropped"
        );
    }

    #[test]
    fn test_dataflow_keeps_sql_passed_to_dbq_exec() {
        // Sink lookup matches by method name only (Exec, Query, etc.) so it
        // catches arbitrary receivers like dbq, conn, tx, pool.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func handler() {{").unwrap();
        writeln!(f, "    q := fmt.Sprintf(\"DELETE FROM users WHERE id = %{}d\", id)", "").unwrap();
        writeln!(f, "    dbq.ExecContext(ctx, q)").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(
            !sql_findings(&path).is_empty(),
            "Sprintf flowing to dbq.ExecContext must be retained"
        );
    }

    #[test]
    fn test_dataflow_keeps_inline_sprintf() {
        // Inline (no assignment) — conservative behavior keeps the finding.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func handler() {{").unwrap();
        writeln!(f, "    return fmt.Sprintf(\"DELETE FROM x WHERE id = %{}d\", id)", "").unwrap();
        writeln!(f, "}}").unwrap();
        assert!(
            !sql_findings(&path).is_empty(),
            "Inline Sprintf with no assignment must be kept conservatively"
        );
    }

    // CULTRA-942: Rust dataflow filter tests.

    #[test]
    fn test_rust_dataflow_drops_pattern_literals_in_static_array() {
        // Mirrors the security.rs self-scan false-positive case: a function
        // that returns a static array of pattern strings. The function never
        // calls a SQL sink, so every string literal finding in it should drop.
        // Use write! with \" escapes so the file text contains raw quotes
        // (the regex layer looks for `fmt.Sprintf("DELETE `, not escaped).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pats.rs");
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "pub fn get_patterns() -> &'static [&'static str] {{\n").unwrap();
        write!(f, "    &[\n").unwrap();
        write!(f, "        \"fmt.Sprintf(\"DELETE \",\n").unwrap();
        write!(f, "        \"fmt.Sprintf(\"INSERT \",\n").unwrap();
        write!(f, "    ]\n").unwrap();
        write!(f, "}}\n").unwrap();
        drop(f);

        let findings: Vec<_> = analyze_security(path.to_str().unwrap())
            .unwrap()
            .findings
            .into_iter()
            .filter(|f| f.rule_id == "SEC-SQL-001" || f.rule_id == "SEC-SQL-002")
            .collect();
        assert!(
            findings.is_empty(),
            "expected all SQL findings dropped (no sink in function), got {:?}",
            findings
        );
    }

    #[test]
    fn test_rust_dataflow_drops_module_level_literal() {
        // const declarations / module-level string literals can't execute queries.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("constpat.rs");
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "const DANGEROUS: &str = \"fmt.Sprintf(\"DELETE \";\n").unwrap();
        drop(f);

        let findings: Vec<_> = analyze_security(path.to_str().unwrap())
            .unwrap()
            .findings
            .into_iter()
            .filter(|f| f.rule_id == "SEC-SQL-001")
            .collect();
        assert!(findings.is_empty(), "module-level string literal must be dropped");
    }

    #[test]
    fn test_rust_dataflow_keeps_literal_in_sink_calling_function() {
        // A function that calls tx.execute() on a SQL-looking string must
        // still flag the finding.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("risky.rs");
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "fn delete_user(tx: &mut Tx) {{\n").unwrap();
        write!(f, "    let q = \"fmt.Sprintf(\"DELETE FROM users \";\n").unwrap();
        write!(f, "    tx.execute(q, ()).unwrap();\n").unwrap();
        write!(f, "}}\n").unwrap();
        drop(f);

        let findings: Vec<_> = analyze_security(path.to_str().unwrap())
            .unwrap()
            .findings
            .into_iter()
            .filter(|f| f.rule_id == "SEC-SQL-001")
            .collect();
        assert!(
            !findings.is_empty(),
            "function with tx.execute() sink must keep SQL findings, got []"
        );
    }

    #[test]
    fn test_rust_dataflow_keeps_literal_in_query_macro_function() {
        // sqlx::query(...).execute(pool) — the execute() terminator is a sink
        // method; the filter treats the enclosing function as sink-carrying.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("macro_sink.rs");
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "async fn danger(pool: &Pool) {{\n").unwrap();
        write!(f, "    let q = \"fmt.Sprintf(\"DELETE FROM users \";\n").unwrap();
        write!(f, "    sqlx::query(q).execute(pool).await.unwrap();\n").unwrap();
        write!(f, "}}\n").unwrap();
        drop(f);

        let findings: Vec<_> = analyze_security(path.to_str().unwrap())
            .unwrap()
            .findings
            .into_iter()
            .filter(|f| f.rule_id == "SEC-SQL-001")
            .collect();
        assert!(
            !findings.is_empty(),
            "function with execute() sink must keep SQL findings, got {:?}",
            findings
        );
    }

    #[test]
    fn test_rust_dataflow_drops_literal_in_log_only_function() {
        // A function that logs a SQL-looking string but never executes it
        // against a database. Must drop.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log_only.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "fn log_template() {{").unwrap();
        writeln!(f, "    let msg = \"fmt.Sprintf(\\\"DELETE FROM ghost\";").unwrap();
        writeln!(f, "    println!(\"{{}}\", msg);").unwrap();
        writeln!(f, "}}").unwrap();

        let findings: Vec<_> = analyze_security(path.to_str().unwrap())
            .unwrap()
            .findings
            .into_iter()
            .filter(|f| f.rule_id == "SEC-SQL-001")
            .collect();
        assert!(findings.is_empty(), "log-only function must drop SQL findings");
    }

    #[test]
    fn test_dataflow_isolated_per_function() {
        // Two functions: one safe (logged), one dangerous (db.Exec).
        // Use different SQL keywords so each function hits a distinct pattern,
        // because the regex layer emits at most one finding per pattern.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.go");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func safe() {{").unwrap();
        writeln!(f, "    msg := fmt.Sprintf(\"DELETE FROM logs WHERE id = %{}d\", id)", "").unwrap();
        writeln!(f, "    log.Println(msg)").unwrap();
        writeln!(f, "}}").unwrap();
        writeln!(f, "func dangerous() {{").unwrap();
        writeln!(f, "    q := fmt.Sprintf(\"INSERT INTO users VALUES (%{}s)\", v)", "").unwrap();
        writeln!(f, "    db.Exec(q)").unwrap();
        writeln!(f, "}}").unwrap();
        let findings = sql_findings(&path);
        assert_eq!(findings.len(), 1, "expected exactly 1 SQL finding (dangerous), got {:?}", findings);
    }

    // CULTRA-957: SEC-RUST-002 (unwrap/expect) — test-context gating + per-location reporting.

    fn unwrap_findings(path: &std::path::Path) -> Vec<SecurityFinding> {
        analyze_security(path.to_str().unwrap())
            .unwrap()
            .findings
            .into_iter()
            .filter(|f| f.rule_id == "SEC-RUST-002")
            .collect()
    }

    #[test]
    fn test_rust_unwrap_in_production_function_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prod.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "pub fn parse() -> i32 {{").unwrap();
        writeln!(f, "    let s = std::env::var(\"X\").unwrap();").unwrap();
        writeln!(f, "    s.parse().unwrap()").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let findings = unwrap_findings(&path);
        assert_eq!(findings.len(), 2,
            "expected 2 production unwrap findings, got: {:?}", findings);
        // Per-location reporting: each finding has its OWN line, not all line 1.
        let lines: Vec<u32> = findings.iter().map(|f| f.line).collect();
        assert!(lines.contains(&2) && lines.contains(&3),
            "findings should be at lines 2 and 3, got: {:?}", lines);
        assert!(findings.iter().all(|f| f.line != 1),
            "no finding should default to line 1 (the old aggregate behavior)");
    }

    #[test]
    fn test_rust_unwrap_inside_cfg_test_mod_is_skipped() {
        // CULTRA-957: classic `#[cfg(test)] mod tests { ... }` pattern.
        // The 5 unwraps inside the mod are test-context and must NOT
        // produce any findings.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("withtests.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "pub fn add(a: i32, b: i32) -> i32 {{ a + b }}").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "#[cfg(test)]").unwrap();
        writeln!(f, "mod tests {{").unwrap();
        writeln!(f, "    use super::*;").unwrap();
        writeln!(f, "    #[test]").unwrap();
        writeln!(f, "    fn t1() {{ \"x\".parse::<i32>().unwrap(); }}").unwrap();
        writeln!(f, "    #[test]").unwrap();
        writeln!(f, "    fn t2() {{ \"y\".parse::<i32>().unwrap(); }}").unwrap();
        writeln!(f, "    #[test]").unwrap();
        writeln!(f, "    fn t3() {{ \"z\".parse::<i32>().unwrap(); }}").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let findings = unwrap_findings(&path);
        assert!(findings.is_empty(),
            "all unwraps in #[cfg(test)] mod must be filtered out, got: {:?}", findings);
    }

    #[test]
    fn test_rust_unwrap_inside_test_attributed_function_is_skipped() {
        // Single function annotated with #[test] (no surrounding mod).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("singletest.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "#[test]").unwrap();
        writeln!(f, "fn one_test() {{").unwrap();
        writeln!(f, "    \"x\".parse::<i32>().unwrap();").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let findings = unwrap_findings(&path);
        assert!(findings.is_empty(),
            "unwrap in #[test] function must be filtered, got: {:?}", findings);
    }

    #[test]
    fn test_rust_unwrap_mixed_production_and_test_only_flags_production() {
        // The whole point of CULTRA-957: a file with BOTH production and
        // test unwraps reports only the production ones.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "pub fn live() {{").unwrap();
        writeln!(f, "    std::env::var(\"REAL\").unwrap();").unwrap();
        writeln!(f, "}}").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "#[cfg(test)]").unwrap();
        writeln!(f, "mod tests {{").unwrap();
        writeln!(f, "    #[test]").unwrap();
        writeln!(f, "    fn t() {{").unwrap();
        writeln!(f, "        \"a\".parse::<i32>().unwrap();").unwrap();
        writeln!(f, "        \"b\".parse::<i32>().unwrap();").unwrap();
        writeln!(f, "    }}").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let findings = unwrap_findings(&path);
        assert_eq!(findings.len(), 1,
            "expected exactly 1 finding (the production unwrap on line 2), got: {:?}", findings);
        assert_eq!(findings[0].line, 2,
            "the production unwrap is on line 2");
    }

    #[test]
    fn test_rust_unwrap_tokio_test_attribute_is_skipped() {
        // #[tokio::test] should be recognized as a test attribute too.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokio_test.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "#[tokio::test]").unwrap();
        writeln!(f, "async fn integration() {{").unwrap();
        writeln!(f, "    fetch_thing().await.unwrap();").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let findings = unwrap_findings(&path);
        assert!(findings.is_empty(),
            "unwrap in #[tokio::test] function must be filtered, got: {:?}", findings);
    }

    #[test]
    fn test_rust_unwrap_expect_also_per_location() {
        // expect() should be treated identically to unwrap().
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("expect.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "pub fn parse() {{").unwrap();
        writeln!(f, "    std::env::var(\"X\").expect(\"missing X\");").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let findings = unwrap_findings(&path);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].title.contains("expect"),
            "title should mention the actual method name (expect), got: {}", findings[0].title);
        assert_eq!(findings[0].line, 2);
    }

    #[test]
    fn test_rust_unwrap_no_threshold_flags_even_one_production_unwrap() {
        // Pre-fix the rule had a >10 threshold ('Excessive unwrap usage').
        // Per-location reporting drops the threshold so even ONE production
        // unwrap is a finding worth flagging — at severity low so it's
        // easily filterable.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "pub fn x() {{ \"y\".parse::<i32>().unwrap(); }}").unwrap();
        drop(f);

        let findings = unwrap_findings(&path);
        assert_eq!(findings.len(), 1, "single production unwrap must produce 1 finding");
        assert_eq!(findings[0].severity, "low");
    }
}
