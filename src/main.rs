mod ast;
mod lsp;
mod mcp;
mod api_client;
mod config;
mod workspace;

use anyhow::Result;
use std::env;
use std::io::{self, BufRead, BufReader, Write};
use tracing_subscriber;

#[derive(Debug, Clone, Copy)]
enum TransportMode {
    Auto,
    Framed,
    Line,
}

#[derive(Debug, Clone, Copy)]
enum ActiveTransport {
    Framed,
    Line,
}

impl ActiveTransport {
    fn write_response<W: Write>(self, writer: &mut W, response: &str) -> io::Result<()> {
        match self {
            ActiveTransport::Framed => {
                write!(writer, "Content-Length: {}\r\n\r\n", response.len())?;
                writer.write_all(response.as_bytes())?;
                writer.flush()
            }
            ActiveTransport::Line => {
                writeln!(writer, "{}", response)?;
                writer.flush()
            }
        }
    }
}

fn parse_transport_mode() -> TransportMode {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--transport=") {
            return parse_transport_value(value);
        }
        if arg == "--transport" {
            if let Some(value) = args.next() {
                return parse_transport_value(&value);
            }
        }
    }
    TransportMode::Auto
}

fn parse_transport_value(value: &str) -> TransportMode {
    match value {
        "auto" => TransportMode::Auto,
        "framed" => TransportMode::Framed,
        "line" => TransportMode::Line,
        other => {
            tracing::warn!(
                "Unknown transport mode '{}', defaulting to auto",
                other
            );
            TransportMode::Auto
        }
    }
}

fn detect_transport<R: BufRead>(reader: &mut R) -> io::Result<Option<ActiveTransport>> {
    let buf = reader.fill_buf()?;
    if buf.is_empty() {
        return Ok(None);
    }

    // MCP stdio framing starts with Content-Length headers.
    if buf.starts_with(b"Content-Length:") || buf.starts_with(b"content-length:") {
        Ok(Some(ActiveTransport::Framed))
    } else {
        Ok(Some(ActiveTransport::Line))
    }
}

fn read_line_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }

        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
}

fn read_framed_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }

        if line == "\r\n" || line == "\n" {
            break;
        }

        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                let len = value.trim().parse::<usize>().map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid Content-Length header: {}", e),
                    )
                })?;
                content_length = Some(len);
            }
        }
    }

    const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024; // 64 MiB

    let len = match content_length {
        Some(v) => v,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing Content-Length header",
            ))
        }
    };

    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Content-Length {} exceeds maximum of {} bytes", len, MAX_MESSAGE_SIZE),
        ));
    }

    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body)?;

    let message = String::from_utf8(body).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid utf-8 body: {}", e),
        )
    })?;

    Ok(Some(message))
}

fn read_message<R: BufRead>(reader: &mut R, mode: ActiveTransport) -> io::Result<Option<String>> {
    match mode {
        ActiveTransport::Framed => read_framed_message(reader),
        ActiveTransport::Line => read_line_message(reader),
    }
}

fn main() -> Result<()> {
    // Initialize logging to stderr (stdout is for MCP protocol)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("Cultra MCP Server (Rust) starting...");

    // Load configuration from ~/.config/cultra/mcp.json
    let config = config::Config::load()?;
    tracing::info!("Configuration loaded from ~/.config/cultra/mcp.json");
    tracing::info!("API URL: {}", config.api.base_url);

    // Initialize API client
    let api_client = api_client::APIClient::new(
        config.api.base_url.clone(),
        config.api.key.clone(),
    )?;
    tracing::info!("API client initialized");

    // Initialize LSP manager with current working directory
    let workspace_root = std::env::current_dir()?;
    let lsp_manager = lsp::LSPManager::new(&workspace_root);
    tracing::info!("LSP manager initialized for workspace: {:?}", workspace_root);

    // Detect default project_id from CLAUDE.md
    let default_project = config::detect_project_id(&workspace_root);
    if let Some(ref pid) = default_project {
        tracing::info!("Default project detected from CLAUDE.md: {}", pid);
    }

    // Create MCP server
    let mut server = mcp::Server::new(api_client, lsp_manager)
        .with_default_project(default_project);
    let selected_transport = parse_transport_mode();
    tracing::info!("Transport mode: {:?}", selected_transport);

    // Run server (stdio mode - synchronous)
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    let mut active_transport = match selected_transport {
        TransportMode::Auto => None,
        TransportMode::Framed => Some(ActiveTransport::Framed),
        TransportMode::Line => Some(ActiveTransport::Line),
    };

    loop {
        if active_transport.is_none() {
            active_transport = detect_transport(&mut reader)?;
            if let Some(mode) = active_transport {
                tracing::info!("Detected transport: {:?}", mode);
            } else {
                break;
            }
        }

        let mode = active_transport.expect("transport mode should be set");
        let request = match read_message(&mut reader, mode) {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(e) => {
                tracing::error!("Error reading request: {}", e);
                let error_response = server.error_response(None, -32700, &e.to_string());
                mode.write_response(&mut writer, &error_response)?;
                continue;
            }
        };

        match server.handle_request(&request) {
            Ok(Some(response)) => {
                mode.write_response(&mut writer, &response)?;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Error handling request: {}", e);
                let error_response = server.error_response(None, -32603, &e.to_string());
                mode.write_response(&mut writer, &error_response)?;
            }
        }
    }

    tracing::info!("EOF reached, shutting down");
    Ok(())
}
