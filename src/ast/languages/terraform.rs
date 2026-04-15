use crate::ast::types::Symbol;
use crate::mcp::types::{Scope, SymbolType};
use anyhow::Result;

/// Extract symbols from Terraform/HCL source code
///
/// Terraform files consist of blocks like:
///   resource "aws_instance" "web" { ... }
///   variable "name" { ... }
///   module "vpc" { ... }
///   output "id" { ... }
///   data "aws_ami" "latest" { ... }
///   locals { ... }
///   provider "aws" { ... }
///   terraform { ... }
pub fn extract_terraform_symbols(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();
    extract_blocks(root_node, content, &mut symbols);
    Ok(symbols)
}

/// Extract module source references as "imports"
pub fn extract_terraform_imports(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<String>> {
    let mut imports = Vec::new();
    collect_module_sources(root_node, content, &mut imports);
    Ok(imports)
}

/// Walk the tree and extract top-level blocks
/// HCL tree: config_file > body > block
fn extract_blocks(node: &tree_sitter::Node, content: &[u8], symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "block" => {
                if let Some(sym) = parse_block(&child, content) {
                    symbols.push(sym);
                }
            }
            "body" | "config_file" => {
                extract_blocks(&child, content, symbols);
            }
            _ => {}
        }
    }
}

/// Parse a single HCL block into a Symbol
fn parse_block(node: &tree_sitter::Node, content: &[u8]) -> Option<Symbol> {
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();

    // First child should be the block type identifier
    let block_type_node = children.first()?;
    if block_type_node.kind() != "identifier" {
        return None;
    }
    let block_type = node_text(block_type_node, content)?;

    // Collect string labels (e.g., "aws_instance" "web")
    let labels: Vec<String> = children
        .iter()
        .filter(|c| c.kind() == "string_lit")
        .filter_map(|c| {
            let text = node_text(c, content)?;
            Some(text.trim_matches('"').to_string())
        })
        .collect();

    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    let (symbol_type, name, signature) = match block_type.as_str() {
        "resource" => {
            let resource_type = labels.first().cloned().unwrap_or_default();
            let resource_name = labels.get(1).cloned().unwrap_or_default();
            (
                SymbolType::Type,
                format!("{}.{}", resource_type, resource_name),
                format!("resource \"{}\" \"{}\"", resource_type, resource_name),
            )
        }
        "data" => {
            let data_type = labels.first().cloned().unwrap_or_default();
            let data_name = labels.get(1).cloned().unwrap_or_default();
            (
                SymbolType::Type,
                format!("data.{}.{}", data_type, data_name),
                format!("data \"{}\" \"{}\"", data_type, data_name),
            )
        }
        "variable" => {
            let var_name = labels.first().cloned().unwrap_or_default();
            (
                SymbolType::Variable,
                var_name.clone(),
                format!("variable \"{}\"", var_name),
            )
        }
        "output" => {
            let out_name = labels.first().cloned().unwrap_or_default();
            (
                SymbolType::Variable,
                format!("output.{}", out_name),
                format!("output \"{}\"", out_name),
            )
        }
        "module" => {
            let mod_name = labels.first().cloned().unwrap_or_default();
            (
                SymbolType::Struct,
                mod_name.clone(),
                format!("module \"{}\"", mod_name),
            )
        }
        "locals" => (
            SymbolType::Variable,
            "locals".to_string(),
            "locals".to_string(),
        ),
        "provider" => {
            let provider_name = labels.first().cloned().unwrap_or_default();
            (
                SymbolType::Struct,
                format!("provider.{}", provider_name),
                format!("provider \"{}\"", provider_name),
            )
        }
        "terraform" => (
            SymbolType::Struct,
            "terraform".to_string(),
            "terraform".to_string(),
        ),
        // Unknown block type — still capture it
        _ => {
            let name = if labels.is_empty() {
                block_type.clone()
            } else {
                labels.join(".")
            };
            (
                SymbolType::Other,
                name.clone(),
                format!("{} {}", block_type, labels.iter().map(|l| format!("\"{}\"", l)).collect::<Vec<_>>().join(" ")),
            )
        }
    };

    Some(Symbol {
        symbol_type,
        name,
        line: start_line,
        end_line,
        scope: Scope::Public,
        signature,
        parent: None,
        receiver: None,
        calls: Vec::new(),
        documentation: None,
        parameters: Vec::new(),
        return_type: None,
    })
}

/// Collect module source attributes as imports
fn collect_module_sources(node: &tree_sitter::Node, content: &[u8], imports: &mut Vec<String>) {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "body" | "config_file" => {
                collect_module_sources(&child, content, imports);
            }
            "block" => {
                let mut block_cursor = child.walk();
                let children: Vec<_> = child.children(&mut block_cursor).collect();

                // Check if this is a module block
                if let Some(first) = children.first() {
                    if first.kind() == "identifier" && node_text(first, content).as_deref() == Some("module") {
                        // Look for source attribute in the body
                        if let Some(body) = children.iter().find(|c| c.kind() == "body") {
                            let mut body_cursor = body.walk();
                            for attr in body.children(&mut body_cursor) {
                                if attr.kind() == "attribute" {
                                    let mut attr_cursor = attr.walk();
                                    let attr_children: Vec<_> = attr.children(&mut attr_cursor).collect();
                                    if let Some(key) = attr_children.first() {
                                        if key.kind() == "identifier" && node_text(key, content).as_deref() == Some("source") {
                                            // Value may be nested: attribute > expr_term > template_expr > string_lit
                                            if let Some(text) = find_nested_string(&attr, content) {
                                                imports.push(text);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn node_text(node: &tree_sitter::Node, content: &[u8]) -> Option<String> {
    node.utf8_text(content).ok().map(|s| s.to_string())
}

/// Recursively find the first string_lit node's text value (trimmed of quotes)
fn find_nested_string(node: &tree_sitter::Node, content: &[u8]) -> Option<String> {
    if node.kind() == "string_lit" {
        return node_text(node, content).map(|s| s.trim_matches('"').to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(s) = find_nested_string(&child, content) {
            return Some(s);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_tf(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_hcl::LANGUAGE.into())
            .expect("Failed to set HCL language");
        let tree = parser.parse(source, None).expect("Failed to parse");
        (tree, source.as_bytes().to_vec())
    }

    #[test]
    fn test_extract_resource() {
        let (tree, content) = parse_tf(r#"
resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = "t3.micro"
}
"#);
        let symbols = extract_terraform_symbols(&tree.root_node(), &content).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "aws_instance.web");
        assert_eq!(symbols[0].symbol_type, SymbolType::Type);
        assert_eq!(symbols[0].signature, "resource \"aws_instance\" \"web\"");
    }

    #[test]
    fn test_extract_variable() {
        let (tree, content) = parse_tf(r#"
variable "region" {
  type    = string
  default = "us-east-1"
}
"#);
        let symbols = extract_terraform_symbols(&tree.root_node(), &content).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "region");
        assert_eq!(symbols[0].symbol_type, SymbolType::Variable);
    }

    #[test]
    fn test_extract_module() {
        let (tree, content) = parse_tf(r#"
module "vpc" {
  source = "terraform-aws-modules/vpc/aws"
  cidr   = "10.0.0.0/16"
}
"#);
        let symbols = extract_terraform_symbols(&tree.root_node(), &content).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "vpc");
        assert_eq!(symbols[0].symbol_type, SymbolType::Struct);
    }

    #[test]
    fn test_extract_module_imports() {
        let (tree, content) = parse_tf(r#"
module "vpc" {
  source = "terraform-aws-modules/vpc/aws"
}

module "eks" {
  source = "./modules/eks"
}
"#);
        let imports = extract_terraform_imports(&tree.root_node(), &content).unwrap();
        assert_eq!(imports.len(), 2);
        assert!(imports.contains(&"terraform-aws-modules/vpc/aws".to_string()));
        assert!(imports.contains(&"./modules/eks".to_string()));
    }

    #[test]
    fn test_extract_multiple_block_types() {
        let (tree, content) = parse_tf(r#"
terraform {
  required_version = ">= 1.0"
}

provider "aws" {
  region = "us-east-1"
}

resource "aws_s3_bucket" "data" {
  bucket = "my-bucket"
}

data "aws_ami" "ubuntu" {
  most_recent = true
}

output "bucket_arn" {
  value = aws_s3_bucket.data.arn
}

locals {
  env = "production"
}
"#);
        let symbols = extract_terraform_symbols(&tree.root_node(), &content).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"terraform"));
        assert!(names.contains(&"provider.aws"));
        assert!(names.contains(&"aws_s3_bucket.data"));
        assert!(names.contains(&"data.aws_ami.ubuntu"));
        assert!(names.contains(&"output.bucket_arn"));
        assert!(names.contains(&"locals"));
        assert_eq!(symbols.len(), 6);
    }
}
