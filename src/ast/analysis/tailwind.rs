use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

// ========== Public output structs ==========

/// Top-level result from resolving Tailwind classes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailwindResolution {
    pub classes: Vec<ResolvedClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme_source: Option<String>,
    pub stats: ResolutionStats,
}

/// A single resolved Tailwind utility class
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedClass {
    pub class_name: String,
    pub declarations: Vec<CssDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub resolved: bool,
    pub category: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub negative: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub important: bool,
}

/// A CSS property: value pair
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CssDeclaration {
    pub property: String,
    pub value: String,
}

/// Summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionStats {
    pub total: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub with_variants: usize,
}

// ========== Internal theme context ==========

#[derive(Debug, Default)]
struct ThemeContext {
    colors: HashMap<String, String>,
    spacing: HashMap<String, String>,
    fonts: HashMap<String, String>,
    text_sizes: HashMap<String, String>,
    custom_vars: HashMap<String, String>,
}

// ========== Entry point ==========

/// Resolve a list of Tailwind utility class names to CSS declarations.
/// Optionally reads @theme block from a CSS file for custom theme values (Tailwind v4).
pub fn resolve_tailwind_classes(classes: &[&str], css_path: Option<&str>) -> Result<TailwindResolution> {
    let theme = if let Some(path) = css_path {
        let content = fs::read_to_string(path)?;
        parse_theme_block(&content)
    } else {
        ThemeContext::default()
    };

    let mut resolved_classes = Vec::with_capacity(classes.len());
    let mut with_variants = 0usize;

    for &class in classes {
        // Strip important modifier (trailing or leading !)
        let (clean, important) = strip_important(class);
        let (variants, core, negative) = strip_variants_and_negative(clean);
        if variants.is_some() {
            with_variants += 1;
        }
        let mut rc = resolve_core_class(core, negative, &theme);
        rc.class_name = class.to_string();
        rc.variant = variants;
        rc.negative = negative;
        rc.important = important;
        resolved_classes.push(rc);
    }

    let resolved_count = resolved_classes.iter().filter(|c| c.resolved).count();

    Ok(TailwindResolution {
        classes: resolved_classes,
        theme_source: css_path.map(|p| p.to_string()),
        stats: ResolutionStats {
            total: classes.len(),
            resolved: resolved_count,
            unresolved: classes.len() - resolved_count,
            with_variants,
        },
    })
}

// ========== @theme parser ==========

fn parse_theme_block(content: &str) -> ThemeContext {
    let mut ctx = ThemeContext::default();
    let mut in_theme = false;
    let mut brace_depth = 0u32;

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect @theme block start (with optional modifiers like `default`)
        if !in_theme {
            if trimmed.starts_with("@theme") && trimmed.contains('{') {
                in_theme = true;
                brace_depth = 1;
                // Handle vars on the same line as the brace (unlikely but safe)
                if let Some(after_brace) = trimmed.split('{').nth(1) {
                    parse_theme_line(after_brace, &mut ctx);
                }
                continue;
            }
            continue;
        }

        // Inside @theme block
        for ch in trimmed.chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => {
                    brace_depth -= 1;
                    if brace_depth == 0 {
                        in_theme = false;
                        break;
                    }
                }
                _ => {}
            }
        }

        // Skip nested blocks (e.g. @keyframes inside @theme — unusual but defensive)
        if brace_depth == 1 && in_theme {
            parse_theme_line(trimmed, &mut ctx);
        }
    }

    ctx
}

fn parse_theme_line(line: &str, ctx: &mut ThemeContext) {
    let trimmed = line.trim().trim_end_matches(';').trim();
    if !trimmed.starts_with("--") {
        return;
    }
    if let Some(colon) = trimmed.find(':') {
        let name = trimmed[..colon].trim();
        let value = trimmed[colon + 1..].trim();
        if name.is_empty() || value.is_empty() {
            return;
        }

        let name_str = name.to_string();
        let value_str = value.to_string();

        if name.starts_with("--color-") {
            ctx.colors.insert(name_str, value_str);
        } else if name.starts_with("--font-") {
            ctx.fonts.insert(name_str, value_str);
        } else if name.starts_with("--text-") {
            ctx.text_sizes.insert(name_str, value_str);
        } else if name == "--spacing" || name.starts_with("--spacing-") {
            ctx.spacing.insert(name_str, value_str);
        } else {
            ctx.custom_vars.insert(name_str, value_str);
        }
    }
}

// ========== Important / variant / negative stripping ==========

/// Strip important modifier: leading `!` (Tailwind v4) or trailing `!` (v3)
fn strip_important(class: &str) -> (&str, bool) {
    if class.starts_with('!') {
        (&class[1..], true)
    } else if class.ends_with('!') {
        (&class[..class.len() - 1], true)
    } else {
        (class, false)
    }
}

/// Returns (combined_variant, core_class, is_negative)
fn strip_variants_and_negative(class: &str) -> (Option<String>, &str, bool) {
    // Split on ':' — all but last segment are variants
    let parts: Vec<&str> = class.split(':').collect();
    let (variants, core) = if parts.len() > 1 {
        let variant_str = parts[..parts.len() - 1].join(":");
        (Some(variant_str), parts[parts.len() - 1])
    } else {
        (None, class)
    };

    // Check for negative prefix: leading '-' followed by alphabetic char
    let negative = core.starts_with('-')
        && core.len() > 1
        && core.as_bytes()[1].is_ascii_alphabetic();
    let core = if negative { &core[1..] } else { core };

    (variants, core, negative)
}

// ========== Core resolution ==========

fn resolve_core_class(core: &str, negative: bool, theme: &ThemeContext) -> ResolvedClass {
    // Priority order: static → arbitrary → spacing → color → sizing → typography → border/radius → opacity/z → grid → unresolved

    if let Some(rc) = resolve_static(core) {
        return rc;
    }
    if let Some(rc) = resolve_arbitrary(core) {
        return rc;
    }
    if let Some(rc) = resolve_spacing(core, negative, theme) {
        return rc;
    }
    if let Some(rc) = resolve_color(core, theme) {
        return rc;
    }
    if let Some(rc) = resolve_sizing(core, negative) {
        return rc;
    }
    if let Some(rc) = resolve_typography(core, theme) {
        return rc;
    }
    if let Some(rc) = resolve_border_radius(core) {
        return rc;
    }
    if let Some(rc) = resolve_opacity_z(core) {
        return rc;
    }
    if let Some(rc) = resolve_grid(core) {
        return rc;
    }
    if let Some(rc) = resolve_filters(core) {
        return rc;
    }
    if let Some(rc) = resolve_translate(core, negative) {
        return rc;
    }

    // Unresolved
    ResolvedClass {
        class_name: core.to_string(),
        declarations: vec![],
        variant: None,
        resolved: false,
        category: "unresolved".to_string(),
        negative: false,
        important: false,
    }
}

// ========== Resolver 1: Static utility map ==========

fn resolve_static(core: &str) -> Option<ResolvedClass> {
    let (props, cat) = static_map(core)?;
    Some(ResolvedClass {
        class_name: core.to_string(),
        declarations: props
            .iter()
            .map(|(p, v)| CssDeclaration {
                property: p.to_string(),
                value: v.to_string(),
            })
            .collect(),
        variant: None,
        resolved: true,
        category: cat.to_string(),
        negative: false,
        important: false,
    })
}

fn static_map(class: &str) -> Option<(Vec<(&'static str, &'static str)>, &'static str)> {
    let result: (Vec<(&str, &str)>, &str) = match class {
        // Display
        "block" => (vec![("display", "block")], "layout"),
        "inline-block" => (vec![("display", "inline-block")], "layout"),
        "inline" => (vec![("display", "inline")], "layout"),
        "flex" => (vec![("display", "flex")], "layout"),
        "inline-flex" => (vec![("display", "inline-flex")], "layout"),
        "grid" => (vec![("display", "grid")], "layout"),
        "inline-grid" => (vec![("display", "inline-grid")], "layout"),
        "contents" => (vec![("display", "contents")], "layout"),
        "hidden" => (vec![("display", "none")], "layout"),
        "table" => (vec![("display", "table")], "layout"),
        "table-row" => (vec![("display", "table-row")], "layout"),
        "table-cell" => (vec![("display", "table-cell")], "layout"),
        "list-item" => (vec![("display", "list-item")], "layout"),

        // Position
        "static" => (vec![("position", "static")], "layout"),
        "fixed" => (vec![("position", "fixed")], "layout"),
        "absolute" => (vec![("position", "absolute")], "layout"),
        "relative" => (vec![("position", "relative")], "layout"),
        "sticky" => (vec![("position", "sticky")], "layout"),

        // Overflow
        "overflow-auto" => (vec![("overflow", "auto")], "layout"),
        "overflow-hidden" => (vec![("overflow", "hidden")], "layout"),
        "overflow-visible" => (vec![("overflow", "visible")], "layout"),
        "overflow-scroll" => (vec![("overflow", "scroll")], "layout"),
        "overflow-x-auto" => (vec![("overflow-x", "auto")], "layout"),
        "overflow-y-auto" => (vec![("overflow-y", "auto")], "layout"),
        "overflow-x-hidden" => (vec![("overflow-x", "hidden")], "layout"),
        "overflow-y-hidden" => (vec![("overflow-y", "hidden")], "layout"),

        // Flexbox
        "flex-row" => (vec![("flex-direction", "row")], "flexbox"),
        "flex-row-reverse" => (vec![("flex-direction", "row-reverse")], "flexbox"),
        "flex-col" => (vec![("flex-direction", "column")], "flexbox"),
        "flex-col-reverse" => (vec![("flex-direction", "column-reverse")], "flexbox"),
        "flex-wrap" => (vec![("flex-wrap", "wrap")], "flexbox"),
        "flex-wrap-reverse" => (vec![("flex-wrap", "wrap-reverse")], "flexbox"),
        "flex-nowrap" => (vec![("flex-wrap", "nowrap")], "flexbox"),
        "flex-1" => (vec![("flex", "1 1 0%")], "flexbox"),
        "flex-auto" => (vec![("flex", "1 1 auto")], "flexbox"),
        "flex-initial" => (vec![("flex", "0 1 auto")], "flexbox"),
        "flex-none" => (vec![("flex", "none")], "flexbox"),
        "grow" => (vec![("flex-grow", "1")], "flexbox"),
        "grow-0" => (vec![("flex-grow", "0")], "flexbox"),
        "shrink" | "flex-shrink" => (vec![("flex-shrink", "1")], "flexbox"),
        "shrink-0" | "flex-shrink-0" => (vec![("flex-shrink", "0")], "flexbox"),

        // Alignment
        "items-start" => (vec![("align-items", "flex-start")], "alignment"),
        "items-end" => (vec![("align-items", "flex-end")], "alignment"),
        "items-center" => (vec![("align-items", "center")], "alignment"),
        "items-baseline" => (vec![("align-items", "baseline")], "alignment"),
        "items-stretch" => (vec![("align-items", "stretch")], "alignment"),
        "justify-start" => (vec![("justify-content", "flex-start")], "alignment"),
        "justify-end" => (vec![("justify-content", "flex-end")], "alignment"),
        "justify-center" => (vec![("justify-content", "center")], "alignment"),
        "justify-between" => (vec![("justify-content", "space-between")], "alignment"),
        "justify-around" => (vec![("justify-content", "space-around")], "alignment"),
        "justify-evenly" => (vec![("justify-content", "space-evenly")], "alignment"),
        "self-auto" => (vec![("align-self", "auto")], "alignment"),
        "self-start" => (vec![("align-self", "flex-start")], "alignment"),
        "self-end" => (vec![("align-self", "flex-end")], "alignment"),
        "self-center" => (vec![("align-self", "center")], "alignment"),
        "self-stretch" => (vec![("align-self", "stretch")], "alignment"),
        "place-content-center" => (vec![("place-content", "center")], "alignment"),
        "place-items-center" => (vec![("place-items", "center")], "alignment"),

        // Cursor
        "cursor-pointer" => (vec![("cursor", "pointer")], "interactivity"),
        "cursor-default" => (vec![("cursor", "default")], "interactivity"),
        "cursor-wait" => (vec![("cursor", "wait")], "interactivity"),
        "cursor-text" => (vec![("cursor", "text")], "interactivity"),
        "cursor-move" => (vec![("cursor", "move")], "interactivity"),
        "cursor-not-allowed" => (vec![("cursor", "not-allowed")], "interactivity"),
        "cursor-grab" => (vec![("cursor", "grab")], "interactivity"),
        "cursor-grabbing" => (vec![("cursor", "grabbing")], "interactivity"),

        // Pointer events / user select
        "pointer-events-none" => (vec![("pointer-events", "none")], "interactivity"),
        "pointer-events-auto" => (vec![("pointer-events", "auto")], "interactivity"),
        "select-none" => (vec![("user-select", "none")], "interactivity"),
        "select-text" => (vec![("user-select", "text")], "interactivity"),
        "select-all" => (vec![("user-select", "all")], "interactivity"),
        "select-auto" => (vec![("user-select", "auto")], "interactivity"),

        // Text utilities
        "truncate" => (vec![
            ("overflow", "hidden"),
            ("text-overflow", "ellipsis"),
            ("white-space", "nowrap"),
        ], "typography"),
        "text-left" => (vec![("text-align", "left")], "typography"),
        "text-center" => (vec![("text-align", "center")], "typography"),
        "text-right" => (vec![("text-align", "right")], "typography"),
        "text-justify" => (vec![("text-align", "justify")], "typography"),
        "uppercase" => (vec![("text-transform", "uppercase")], "typography"),
        "lowercase" => (vec![("text-transform", "lowercase")], "typography"),
        "capitalize" => (vec![("text-transform", "capitalize")], "typography"),
        "normal-case" => (vec![("text-transform", "none")], "typography"),
        "italic" => (vec![("font-style", "italic")], "typography"),
        "not-italic" => (vec![("font-style", "normal")], "typography"),
        "underline" => (vec![("text-decoration-line", "underline")], "typography"),
        "overline" => (vec![("text-decoration-line", "overline")], "typography"),
        "line-through" => (vec![("text-decoration-line", "line-through")], "typography"),
        "no-underline" => (vec![("text-decoration-line", "none")], "typography"),
        "antialiased" => (vec![
            ("-webkit-font-smoothing", "antialiased"),
            ("-moz-osx-font-smoothing", "grayscale"),
        ], "typography"),
        "subpixel-antialiased" => (vec![
            ("-webkit-font-smoothing", "auto"),
            ("-moz-osx-font-smoothing", "auto"),
        ], "typography"),
        "break-words" => (vec![("overflow-wrap", "break-word")], "typography"),
        "break-all" => (vec![("word-break", "break-all")], "typography"),
        "break-keep" => (vec![("word-break", "keep-all")], "typography"),
        "break-normal" => (vec![("overflow-wrap", "normal"), ("word-break", "normal")], "typography"),
        "whitespace-normal" => (vec![("white-space", "normal")], "typography"),
        "whitespace-nowrap" => (vec![("white-space", "nowrap")], "typography"),
        "whitespace-pre" => (vec![("white-space", "pre")], "typography"),
        "whitespace-pre-line" => (vec![("white-space", "pre-line")], "typography"),
        "whitespace-pre-wrap" => (vec![("white-space", "pre-wrap")], "typography"),

        // Accessibility
        "sr-only" => (vec![
            ("position", "absolute"),
            ("width", "1px"),
            ("height", "1px"),
            ("padding", "0"),
            ("margin", "-1px"),
            ("overflow", "hidden"),
            ("clip", "rect(0, 0, 0, 0)"),
            ("white-space", "nowrap"),
            ("border-width", "0"),
        ], "accessibility"),
        "not-sr-only" => (vec![
            ("position", "static"),
            ("width", "auto"),
            ("height", "auto"),
            ("padding", "0"),
            ("margin", "0"),
            ("overflow", "visible"),
            ("clip", "auto"),
            ("white-space", "normal"),
        ], "accessibility"),

        // Visibility
        "visible" => (vec![("visibility", "visible")], "layout"),
        "invisible" => (vec![("visibility", "hidden")], "layout"),
        "collapse" => (vec![("visibility", "collapse")], "layout"),

        // Object fit/position
        "object-contain" => (vec![("object-fit", "contain")], "layout"),
        "object-cover" => (vec![("object-fit", "cover")], "layout"),
        "object-fill" => (vec![("object-fit", "fill")], "layout"),
        "object-none" => (vec![("object-fit", "none")], "layout"),
        "object-scale-down" => (vec![("object-fit", "scale-down")], "layout"),

        // Aspect ratio
        "aspect-auto" => (vec![("aspect-ratio", "auto")], "layout"),
        "aspect-square" => (vec![("aspect-ratio", "1 / 1")], "layout"),
        "aspect-video" => (vec![("aspect-ratio", "16 / 9")], "layout"),

        // Isolation
        "isolate" => (vec![("isolation", "isolate")], "layout"),
        "isolation-auto" => (vec![("isolation", "auto")], "layout"),

        // Box sizing
        "box-border" => (vec![("box-sizing", "border-box")], "layout"),
        "box-content" => (vec![("box-sizing", "content-box")], "layout"),

        // Float / clear
        "float-right" => (vec![("float", "right")], "layout"),
        "float-left" => (vec![("float", "left")], "layout"),
        "float-none" => (vec![("float", "none")], "layout"),
        "clear-left" => (vec![("clear", "left")], "layout"),
        "clear-right" => (vec![("clear", "right")], "layout"),
        "clear-both" => (vec![("clear", "both")], "layout"),
        "clear-none" => (vec![("clear", "none")], "layout"),

        // Transitions / transforms
        "transition" => (vec![("transition-property", "color, background-color, border-color, text-decoration-color, fill, stroke, opacity, box-shadow, transform, filter, backdrop-filter"), ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"), ("transition-duration", "150ms")], "transition"),
        "transition-none" => (vec![("transition-property", "none")], "transition"),
        "transition-all" => (vec![("transition-property", "all"), ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"), ("transition-duration", "150ms")], "transition"),
        "transition-colors" => (vec![("transition-property", "color, background-color, border-color, text-decoration-color, fill, stroke"), ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"), ("transition-duration", "150ms")], "transition"),
        "transition-opacity" => (vec![("transition-property", "opacity"), ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"), ("transition-duration", "150ms")], "transition"),
        "transition-shadow" => (vec![("transition-property", "box-shadow"), ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"), ("transition-duration", "150ms")], "transition"),
        "transition-transform" => (vec![("transition-property", "transform"), ("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)"), ("transition-duration", "150ms")], "transition"),
        "transform" => (vec![("transform", "translate(var(--tw-translate-x), var(--tw-translate-y)) rotate(var(--tw-rotate)) skewX(var(--tw-skew-x)) skewY(var(--tw-skew-y)) scaleX(var(--tw-scale-x)) scaleY(var(--tw-scale-y))")], "transform"),
        "transform-none" => (vec![("transform", "none")], "transform"),
        "transform-gpu" => (vec![("transform", "translate3d(var(--tw-translate-x), var(--tw-translate-y), 0) rotate(var(--tw-rotate)) skewX(var(--tw-skew-x)) skewY(var(--tw-skew-y)) scaleX(var(--tw-scale-x)) scaleY(var(--tw-scale-y))")], "transform"),

        // Duration / delay / ease
        "ease-linear" => (vec![("transition-timing-function", "linear")], "transition"),
        "ease-in" => (vec![("transition-timing-function", "cubic-bezier(0.4, 0, 1, 1)")], "transition"),
        "ease-out" => (vec![("transition-timing-function", "cubic-bezier(0, 0, 0.2, 1)")], "transition"),
        "ease-in-out" => (vec![("transition-timing-function", "cubic-bezier(0.4, 0, 0.2, 1)")], "transition"),

        // Shadow
        "shadow" => (vec![("box-shadow", "0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1)")], "effect"),
        "shadow-sm" => (vec![("box-shadow", "0 1px 2px 0 rgb(0 0 0 / 0.05)")], "effect"),
        "shadow-md" => (vec![("box-shadow", "0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1)")], "effect"),
        "shadow-lg" => (vec![("box-shadow", "0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1)")], "effect"),
        "shadow-xl" => (vec![("box-shadow", "0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1)")], "effect"),
        "shadow-2xl" => (vec![("box-shadow", "0 25px 50px -12px rgb(0 0 0 / 0.25)")], "effect"),
        "shadow-inner" => (vec![("box-shadow", "inset 0 2px 4px 0 rgb(0 0 0 / 0.05)")], "effect"),
        "shadow-none" => (vec![("box-shadow", "0 0 #0000")], "effect"),

        // Ring
        "ring" => (vec![("box-shadow", "var(--tw-ring-inset) 0 0 0 3px var(--tw-ring-color)")], "effect"),
        "ring-0" => (vec![("box-shadow", "var(--tw-ring-inset) 0 0 0 0px var(--tw-ring-color)")], "effect"),
        "ring-1" => (vec![("box-shadow", "var(--tw-ring-inset) 0 0 0 1px var(--tw-ring-color)")], "effect"),
        "ring-2" => (vec![("box-shadow", "var(--tw-ring-inset) 0 0 0 2px var(--tw-ring-color)")], "effect"),
        "ring-4" => (vec![("box-shadow", "var(--tw-ring-inset) 0 0 0 4px var(--tw-ring-color)")], "effect"),
        "ring-8" => (vec![("box-shadow", "var(--tw-ring-inset) 0 0 0 8px var(--tw-ring-color)")], "effect"),
        "ring-inset" => (vec![("--tw-ring-inset", "inset")], "effect"),

        // Container
        "container" => (vec![("width", "100%")], "layout"),

        // Animations
        "animate-spin" => (vec![("animation", "spin 1s linear infinite")], "animation"),
        "animate-ping" => (vec![("animation", "ping 1s cubic-bezier(0, 0, 0.2, 1) infinite")], "animation"),
        "animate-pulse" => (vec![("animation", "pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite")], "animation"),
        "animate-bounce" => (vec![("animation", "bounce 1s infinite")], "animation"),
        "animate-none" => (vec![("animation", "none")], "animation"),

        // Gradients
        "bg-gradient-to-t" => (vec![("background-image", "linear-gradient(to top, var(--tw-gradient-stops))")], "gradient"),
        "bg-gradient-to-tr" => (vec![("background-image", "linear-gradient(to top right, var(--tw-gradient-stops))")], "gradient"),
        "bg-gradient-to-r" => (vec![("background-image", "linear-gradient(to right, var(--tw-gradient-stops))")], "gradient"),
        "bg-gradient-to-br" => (vec![("background-image", "linear-gradient(to bottom right, var(--tw-gradient-stops))")], "gradient"),
        "bg-gradient-to-b" => (vec![("background-image", "linear-gradient(to bottom, var(--tw-gradient-stops))")], "gradient"),
        "bg-gradient-to-bl" => (vec![("background-image", "linear-gradient(to bottom left, var(--tw-gradient-stops))")], "gradient"),
        "bg-gradient-to-l" => (vec![("background-image", "linear-gradient(to left, var(--tw-gradient-stops))")], "gradient"),
        "bg-gradient-to-tl" => (vec![("background-image", "linear-gradient(to top left, var(--tw-gradient-stops))")], "gradient"),

        // Vertical align
        "align-baseline" => (vec![("vertical-align", "baseline")], "typography"),
        "align-top" => (vec![("vertical-align", "top")], "typography"),
        "align-middle" => (vec![("vertical-align", "middle")], "typography"),
        "align-bottom" => (vec![("vertical-align", "bottom")], "typography"),
        "align-text-top" => (vec![("vertical-align", "text-top")], "typography"),
        "align-text-bottom" => (vec![("vertical-align", "text-bottom")], "typography"),
        "align-sub" => (vec![("vertical-align", "sub")], "typography"),
        "align-super" => (vec![("vertical-align", "super")], "typography"),

        // Table
        "border-collapse" => (vec![("border-collapse", "collapse")], "border"),
        "border-separate" => (vec![("border-collapse", "separate")], "border"),

        // Touch
        "touch-auto" => (vec![("touch-action", "auto")], "interactivity"),
        "touch-none" => (vec![("touch-action", "none")], "interactivity"),
        "touch-pan-x" => (vec![("touch-action", "pan-x")], "interactivity"),
        "touch-pan-y" => (vec![("touch-action", "pan-y")], "interactivity"),
        "touch-pan-left" => (vec![("touch-action", "pan-left")], "interactivity"),
        "touch-pan-right" => (vec![("touch-action", "pan-right")], "interactivity"),
        "touch-pan-up" => (vec![("touch-action", "pan-up")], "interactivity"),
        "touch-pan-down" => (vec![("touch-action", "pan-down")], "interactivity"),
        "touch-pinch-zoom" => (vec![("touch-action", "pinch-zoom")], "interactivity"),
        "touch-manipulation" => (vec![("touch-action", "manipulation")], "interactivity"),

        // Fill / Stroke
        "fill-none" => (vec![("fill", "none")], "svg"),
        "fill-current" => (vec![("fill", "currentColor")], "svg"),
        "stroke-none" => (vec![("stroke", "none")], "svg"),
        "stroke-current" => (vec![("stroke", "currentColor")], "svg"),

        // Typography extras
        "tabular-nums" => (vec![("font-variant-numeric", "tabular-nums")], "typography"),
        "oldstyle-nums" => (vec![("font-variant-numeric", "oldstyle-nums")], "typography"),
        "lining-nums" => (vec![("font-variant-numeric", "lining-nums")], "typography"),
        "proportional-nums" => (vec![("font-variant-numeric", "proportional-nums")], "typography"),
        "slashed-zero" => (vec![("font-variant-numeric", "slashed-zero")], "typography"),
        "normal-nums" => (vec![("font-variant-numeric", "normal")], "typography"),
        "ordinal" => (vec![("font-variant-numeric", "ordinal")], "typography"),

        // Background clip
        "bg-clip-text" => (vec![("-webkit-background-clip", "text"), ("background-clip", "text")], "effect"),
        "bg-clip-border" => (vec![("background-clip", "border-box")], "effect"),
        "bg-clip-padding" => (vec![("background-clip", "padding-box")], "effect"),
        "bg-clip-content" => (vec![("background-clip", "content-box")], "effect"),

        // Group / peer markers (no CSS output, but valid Tailwind classes)
        "group" => (vec![], "marker"),
        "peer" => (vec![], "marker"),

        // Will-change
        "will-change-auto" => (vec![("will-change", "auto")], "effect"),
        "will-change-scroll" => (vec![("will-change", "scroll-position")], "effect"),
        "will-change-contents" => (vec![("will-change", "contents")], "effect"),
        "will-change-transform" => (vec![("will-change", "transform")], "effect"),

        // Misc
        "outline-none" => (vec![("outline", "2px solid transparent"), ("outline-offset", "2px")], "effect"),
        "outline-hidden" => (vec![("outline", "2px solid transparent"), ("outline-offset", "2px")], "effect"),
        "resize-none" => (vec![("resize", "none")], "interactivity"),
        "resize" => (vec![("resize", "both")], "interactivity"),
        "resize-x" => (vec![("resize", "horizontal")], "interactivity"),
        "resize-y" => (vec![("resize", "vertical")], "interactivity"),
        "appearance-none" => (vec![("appearance", "none")], "interactivity"),
        "appearance-auto" => (vec![("appearance", "auto")], "interactivity"),
        "list-none" => (vec![("list-style-type", "none")], "typography"),
        "list-disc" => (vec![("list-style-type", "disc")], "typography"),
        "list-decimal" => (vec![("list-style-type", "decimal")], "typography"),
        "list-inside" => (vec![("list-style-position", "inside")], "typography"),
        "list-outside" => (vec![("list-style-position", "outside")], "typography"),

        _ => return None,
    };
    Some(result)
}

// ========== Resolver 2: Arbitrary values ==========

fn resolve_arbitrary(core: &str) -> Option<ResolvedClass> {
    // Pattern: prefix-[value]
    let bracket_start = core.find('[')?;
    let bracket_end = core.rfind(']')?;
    if bracket_end <= bracket_start {
        return None;
    }

    let prefix = core[..bracket_start].trim_end_matches('-');
    let value = &core[bracket_start + 1..bracket_end];

    // Replace underscores with spaces (Tailwind convention)
    let value = value.replace('_', " ");

    let (property, cat) = match prefix {
        "w" => ("width", "sizing"),
        "h" => ("height", "sizing"),
        "min-w" => ("min-width", "sizing"),
        "min-h" => ("min-height", "sizing"),
        "max-w" => ("max-width", "sizing"),
        "max-h" => ("max-height", "sizing"),
        "size" => ("width", "sizing"), // size sets both, we note width
        "p" => ("padding", "spacing"),
        "px" => ("padding-inline", "spacing"),
        "py" => ("padding-block", "spacing"),
        "pt" => ("padding-top", "spacing"),
        "pr" => ("padding-right", "spacing"),
        "pb" => ("padding-bottom", "spacing"),
        "pl" => ("padding-left", "spacing"),
        "m" => ("margin", "spacing"),
        "mx" => ("margin-inline", "spacing"),
        "my" => ("margin-block", "spacing"),
        "mt" => ("margin-top", "spacing"),
        "mr" => ("margin-right", "spacing"),
        "mb" => ("margin-bottom", "spacing"),
        "ml" => ("margin-left", "spacing"),
        "gap" => ("gap", "spacing"),
        "gap-x" => ("column-gap", "spacing"),
        "gap-y" => ("row-gap", "spacing"),
        "top" => ("top", "spacing"),
        "right" => ("right", "spacing"),
        "bottom" => ("bottom", "spacing"),
        "left" => ("left", "spacing"),
        "inset" => ("inset", "spacing"),
        "inset-x" => ("inset-inline", "spacing"),
        "inset-y" => ("inset-block", "spacing"),
        "text" => ("font-size", "typography"),
        "bg" => ("background-color", "color"),
        "border" => ("border-width", "border"),
        "rounded" => ("border-radius", "border"),
        "opacity" => ("opacity", "effect"),
        "z" => ("z-index", "layout"),
        "grid-cols" => ("grid-template-columns", "grid"),
        "grid-rows" => ("grid-template-rows", "grid"),
        "col-span" => ("grid-column", "grid"),
        "row-span" => ("grid-row", "grid"),
        "translate-x" => ("--tw-translate-x", "transform"),
        "translate-y" => ("--tw-translate-y", "transform"),
        "rotate" => ("--tw-rotate", "transform"),
        "scale" => ("--tw-scale-x", "transform"),
        "duration" => ("transition-duration", "transition"),
        "delay" => ("transition-delay", "transition"),
        "blur" => ("filter", "filter"),
        "backdrop-blur" => ("backdrop-filter", "filter"),
        "brightness" => ("filter", "filter"),
        "contrast" => ("filter", "filter"),
        "saturate" => ("filter", "filter"),
        "hue-rotate" => ("filter", "filter"),
        "shadow" => ("box-shadow", "effect"),
        "tracking" => ("letter-spacing", "typography"),
        "leading" => ("line-height", "typography"),
        _ => return None,
    };

    let mut declarations = vec![CssDeclaration {
        property: property.to_string(),
        value: value.clone(),
    }];

    // For `size-[x]`, also set height
    if prefix == "size" {
        declarations.push(CssDeclaration {
            property: "height".to_string(),
            value,
        });
    }

    Some(ResolvedClass {
        class_name: core.to_string(),
        declarations,
        variant: None,
        resolved: true,
        category: cat.to_string(),
        negative: false,
        important: false,
    })
}

// ========== Resolver 3: Spacing ==========

/// Spacing prefixes and their CSS properties
const SPACING_MAP: &[(&str, &[&str])] = &[
    ("p", &["padding"]),
    ("px", &["padding-left", "padding-right"]),
    ("py", &["padding-top", "padding-bottom"]),
    ("pt", &["padding-top"]),
    ("pr", &["padding-right"]),
    ("pb", &["padding-bottom"]),
    ("pl", &["padding-left"]),
    ("ps", &["padding-inline-start"]),
    ("pe", &["padding-inline-end"]),
    ("m", &["margin"]),
    ("mx", &["margin-left", "margin-right"]),
    ("my", &["margin-top", "margin-bottom"]),
    ("mt", &["margin-top"]),
    ("mr", &["margin-right"]),
    ("mb", &["margin-bottom"]),
    ("ml", &["margin-left"]),
    ("ms", &["margin-inline-start"]),
    ("me", &["margin-inline-end"]),
    ("gap", &["gap"]),
    ("gap-x", &["column-gap"]),
    ("gap-y", &["row-gap"]),
    ("top", &["top"]),
    ("right", &["right"]),
    ("bottom", &["bottom"]),
    ("left", &["left"]),
    ("inset", &["inset"]),
    ("inset-x", &["left", "right"]),
    ("inset-y", &["top", "bottom"]),
    ("space-x", &["margin-left"]),  // simplified — applies to children
    ("space-y", &["margin-top"]),   // simplified — applies to children
];

fn resolve_spacing(core: &str, negative: bool, _theme: &ThemeContext) -> Option<ResolvedClass> {
    for &(prefix, properties) in SPACING_MAP {
        if let Some(suffix) = strip_prefix_dash(core, prefix) {
            if let Some(value) = spacing_value(suffix) {
                let value = if negative { format!("-{}", value) } else { value };
                let declarations = properties
                    .iter()
                    .map(|p| CssDeclaration {
                        property: p.to_string(),
                        value: value.clone(),
                    })
                    .collect();
                return Some(ResolvedClass {
                    class_name: core.to_string(),
                    declarations,
                    variant: None,
                    resolved: true,
                    category: "spacing".to_string(),
                    negative,
                    important: false,
                });
            }
        }
    }
    None
}

fn spacing_value(suffix: &str) -> Option<String> {
    match suffix {
        "0" => Some("0px".to_string()),
        "px" => Some("1px".to_string()),
        "auto" => Some("auto".to_string()),
        "full" => Some("100%".to_string()),
        _ => {
            // Try numeric: n → n * 0.25rem
            if let Ok(n) = suffix.parse::<f64>() {
                let rem = n * 0.25;
                if rem == rem.trunc() {
                    Some(format!("{}rem", rem as i64))
                } else {
                    Some(format!("{}rem", rem))
                }
            }
            // Fractions: 1/2 → 50%
            else if let Some(slash) = suffix.find('/') {
                let num: f64 = suffix[..slash].parse().ok()?;
                let den: f64 = suffix[slash + 1..].parse().ok()?;
                if den != 0.0 {
                    Some(format!("{:.6}%", (num / den) * 100.0))
                } else {
                    None
                }
            } else {
                None
            }
        }
    }
}

// ========== Resolver 4: Colors ==========

/// Color utility prefixes and their CSS properties
const COLOR_MAP: &[(&str, &str)] = &[
    ("bg", "background-color"),
    ("text", "color"),
    ("border", "border-color"),
    ("border-t", "border-top-color"),
    ("border-r", "border-right-color"),
    ("border-b", "border-bottom-color"),
    ("border-l", "border-left-color"),
    ("outline", "outline-color"),
    ("ring", "box-shadow"),  // ring color — simplified
    ("accent", "accent-color"),
    ("caret", "caret-color"),
    ("fill", "fill"),
    ("stroke", "stroke"),
    ("decoration", "text-decoration-color"),
    ("placeholder", "color"),  // ::placeholder — simplified
    ("divide", "border-color"),  // > * + * — simplified
    ("from", "--tw-gradient-from"),
    ("via", "--tw-gradient-via"),
    ("to", "--tw-gradient-to"),
];

/// Known text size names — used to disambiguate text-sm (typography) from text-red-500 (color)
const TEXT_SIZE_NAMES: &[&str] = &[
    "xs", "sm", "base", "lg", "xl", "2xl", "3xl", "4xl", "5xl", "6xl", "7xl", "8xl", "9xl",
];

fn resolve_color(core: &str, theme: &ThemeContext) -> Option<ResolvedClass> {
    for &(prefix, property) in COLOR_MAP {
        if let Some(suffix) = strip_prefix_dash(core, prefix) {
            // Disambiguation: "text-sm" etc. are typography, not color
            if prefix == "text" && TEXT_SIZE_NAMES.contains(&suffix) {
                return None; // let typography resolver handle it
            }

            // Check for opacity modifier: color/opacity (e.g., "red-500/50")
            let (color_part, opacity) = if let Some(slash_pos) = suffix.find('/') {
                (&suffix[..slash_pos], Some(&suffix[slash_pos + 1..]))
            } else {
                (suffix, None)
            };

            if let Some(color_val) = resolve_color_value(color_part, theme) {
                let value = if let Some(op) = opacity {
                    format!("{} / {}%", color_val, op)
                } else {
                    color_val
                };
                return Some(ResolvedClass {
                    class_name: core.to_string(),
                    declarations: vec![CssDeclaration {
                        property: property.to_string(),
                        value,
                    }],
                    variant: None,
                    resolved: true,
                    category: "color".to_string(),
                    negative: false,
                    important: false,
                });
            }
        }
    }
    None
}

fn resolve_color_value(name: &str, theme: &ThemeContext) -> Option<String> {
    match name {
        "black" => Some("#000".to_string()),
        "white" => Some("#fff".to_string()),
        "transparent" => Some("transparent".to_string()),
        "current" => Some("currentColor".to_string()),
        "inherit" => Some("inherit".to_string()),
        _ => {
            // Check custom theme colors first
            let theme_key = format!("--color-{}", name);
            if let Some(val) = theme.colors.get(&theme_key) {
                return Some(val.clone());
            }

            // Lenient fallback: resolve as var(--color-{name}) — covers standard palette,
            // DaisyUI, shadcn/ui, and any custom theme colors.
            // Reject pure numbers, border keywords, and sizing keywords to avoid
            // stealing from later resolvers (e.g. border-2, border-solid, border-t)
            let first_char = name.chars().next().unwrap_or('0');
            if first_char.is_ascii_digit()
                || matches!(name, "solid" | "dashed" | "dotted" | "double" | "none"
                    | "collapse" | "separate"
                    | "t" | "r" | "b" | "l" | "x" | "y")
            {
                None
            } else {
                Some(format!("var(--color-{})", name))
            }
        }
    }
}

// ========== Resolver 5: Sizing ==========

const SIZING_MAP: &[(&str, &[&str])] = &[
    ("w", &["width"]),
    ("h", &["height"]),
    ("min-w", &["min-width"]),
    ("min-h", &["min-height"]),
    ("max-w", &["max-width"]),
    ("max-h", &["max-height"]),
    ("size", &["width", "height"]),
];

fn resolve_sizing(core: &str, negative: bool) -> Option<ResolvedClass> {
    for &(prefix, properties) in SIZING_MAP {
        if let Some(suffix) = strip_prefix_dash(core, prefix) {
            if let Some(value) = sizing_value(suffix, prefix) {
                let value = if negative { format!("-{}", value) } else { value };
                let declarations = properties
                    .iter()
                    .map(|p| CssDeclaration {
                        property: p.to_string(),
                        value: value.clone(),
                    })
                    .collect();
                return Some(ResolvedClass {
                    class_name: core.to_string(),
                    declarations,
                    variant: None,
                    resolved: true,
                    category: "sizing".to_string(),
                    negative: false,
                    important: false,
                });
            }
        }
    }
    None
}

fn sizing_value(suffix: &str, prefix: &str) -> Option<String> {
    match suffix {
        "full" => Some("100%".to_string()),
        "screen" => {
            if prefix == "w" || prefix == "min-w" || prefix == "max-w" || prefix == "size" {
                Some("100vw".to_string())
            } else {
                Some("100vh".to_string())
            }
        }
        "svh" => Some("100svh".to_string()),
        "lvh" => Some("100lvh".to_string()),
        "dvh" => Some("100dvh".to_string()),
        "svw" => Some("100svw".to_string()),
        "lvw" => Some("100lvw".to_string()),
        "dvw" => Some("100dvw".to_string()),
        "min" => Some("min-content".to_string()),
        "max" => Some("max-content".to_string()),
        "fit" => Some("fit-content".to_string()),
        "auto" => Some("auto".to_string()),
        "px" => Some("1px".to_string()),
        "0" => Some("0px".to_string()),
        "prose" if prefix == "max-w" => Some("65ch".to_string()),
        "xs" if prefix == "max-w" => Some("20rem".to_string()),
        "sm" if prefix == "max-w" => Some("24rem".to_string()),
        "md" if prefix == "max-w" => Some("28rem".to_string()),
        "lg" if prefix == "max-w" => Some("32rem".to_string()),
        "xl" if prefix == "max-w" => Some("36rem".to_string()),
        "2xl" if prefix == "max-w" => Some("42rem".to_string()),
        "3xl" if prefix == "max-w" => Some("48rem".to_string()),
        "4xl" if prefix == "max-w" => Some("56rem".to_string()),
        "5xl" if prefix == "max-w" => Some("64rem".to_string()),
        "6xl" if prefix == "max-w" => Some("72rem".to_string()),
        "7xl" if prefix == "max-w" => Some("80rem".to_string()),
        "none" if prefix.starts_with("max") => Some("none".to_string()),
        _ => {
            // Fraction: 1/2 → 50%, 1/3 → 33.333333%
            if let Some(slash) = suffix.find('/') {
                let num: f64 = suffix[..slash].parse().ok()?;
                let den: f64 = suffix[slash + 1..].parse().ok()?;
                if den != 0.0 {
                    let pct = (num / den) * 100.0;
                    return Some(format!("{:.6}%", pct));
                }
            }
            // Numeric spacing: n → n * 0.25rem
            if let Ok(n) = suffix.parse::<f64>() {
                let rem = n * 0.25;
                if rem == rem.trunc() {
                    Some(format!("{}rem", rem as i64))
                } else {
                    Some(format!("{}rem", rem))
                }
            } else {
                None
            }
        }
    }
}

// ========== Resolver 6: Typography ==========

fn resolve_typography(core: &str, theme: &ThemeContext) -> Option<ResolvedClass> {
    // text-{size}
    if let Some(suffix) = strip_prefix_dash(core, "text") {
        if let Some(decls) = text_size_declarations(suffix, theme) {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: decls,
                variant: None,
                resolved: true,
                category: "typography".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // font-{weight} or font-{family}
    if let Some(suffix) = strip_prefix_dash(core, "font") {
        if let Some(decls) = font_declarations(suffix, theme) {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: decls,
                variant: None,
                resolved: true,
                category: "typography".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // leading-{value} (line-height)
    if let Some(suffix) = strip_prefix_dash(core, "leading") {
        if let Some(val) = leading_value(suffix) {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "line-height".to_string(),
                    value: val,
                }],
                variant: None,
                resolved: true,
                category: "typography".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // tracking-{value} (letter-spacing)
    if let Some(suffix) = strip_prefix_dash(core, "tracking") {
        if let Some(val) = tracking_value(suffix) {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "letter-spacing".to_string(),
                    value: val,
                }],
                variant: None,
                resolved: true,
                category: "typography".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    None
}

fn text_size_declarations(suffix: &str, theme: &ThemeContext) -> Option<Vec<CssDeclaration>> {
    // Check theme first
    let theme_key = format!("--text-{}", suffix);
    if let Some(val) = theme.text_sizes.get(&theme_key) {
        return Some(vec![CssDeclaration {
            property: "font-size".to_string(),
            value: val.clone(),
        }]);
    }

    let (size, line_height) = match suffix {
        "xs" => ("0.75rem", "1rem"),
        "sm" => ("0.875rem", "1.25rem"),
        "base" => ("1rem", "1.5rem"),
        "lg" => ("1.125rem", "1.75rem"),
        "xl" => ("1.25rem", "1.75rem"),
        "2xl" => ("1.5rem", "2rem"),
        "3xl" => ("1.875rem", "2.25rem"),
        "4xl" => ("2.25rem", "2.5rem"),
        "5xl" => ("3rem", "1"),
        "6xl" => ("3.75rem", "1"),
        "7xl" => ("4.5rem", "1"),
        "8xl" => ("6rem", "1"),
        "9xl" => ("8rem", "1"),
        _ => return None,
    };

    Some(vec![
        CssDeclaration {
            property: "font-size".to_string(),
            value: size.to_string(),
        },
        CssDeclaration {
            property: "line-height".to_string(),
            value: line_height.to_string(),
        },
    ])
}

fn font_declarations(suffix: &str, theme: &ThemeContext) -> Option<Vec<CssDeclaration>> {
    // Check weights first
    let weight = match suffix {
        "thin" => Some("100"),
        "extralight" => Some("200"),
        "light" => Some("300"),
        "normal" => Some("400"),
        "medium" => Some("500"),
        "semibold" => Some("600"),
        "bold" => Some("700"),
        "extrabold" => Some("800"),
        "black" => Some("900"),
        _ => None,
    };

    if let Some(w) = weight {
        return Some(vec![CssDeclaration {
            property: "font-weight".to_string(),
            value: w.to_string(),
        }]);
    }

    // Font families
    // Check theme first
    let theme_key = format!("--font-{}", suffix);
    if let Some(val) = theme.fonts.get(&theme_key) {
        return Some(vec![CssDeclaration {
            property: "font-family".to_string(),
            value: val.clone(),
        }]);
    }

    let family = match suffix {
        "sans" => Some("ui-sans-serif, system-ui, sans-serif, \"Apple Color Emoji\", \"Segoe UI Emoji\", \"Segoe UI Symbol\", \"Noto Color Emoji\""),
        "serif" => Some("ui-serif, Georgia, Cambria, \"Times New Roman\", Times, serif"),
        "mono" => Some("ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, \"Liberation Mono\", \"Courier New\", monospace"),
        // Lenient fallback: any other font name → var(--font-{name})
        _ => return Some(vec![CssDeclaration {
            property: "font-family".to_string(),
            value: format!("var(--font-{})", suffix),
        }]),
    };

    family.map(|f| {
        vec![CssDeclaration {
            property: "font-family".to_string(),
            value: f.to_string(),
        }]
    })
}

fn leading_value(suffix: &str) -> Option<String> {
    match suffix {
        "none" => Some("1".to_string()),
        "tight" => Some("1.25".to_string()),
        "snug" => Some("1.375".to_string()),
        "normal" => Some("1.5".to_string()),
        "relaxed" => Some("1.625".to_string()),
        "loose" => Some("2".to_string()),
        _ => {
            // Numeric: leading-6 → 1.5rem (spacing scale)
            if let Ok(n) = suffix.parse::<f64>() {
                let rem = n * 0.25;
                if rem == rem.trunc() {
                    Some(format!("{}rem", rem as i64))
                } else {
                    Some(format!("{}rem", rem))
                }
            } else {
                None
            }
        }
    }
}

fn tracking_value(suffix: &str) -> Option<String> {
    match suffix {
        "tighter" => Some("-0.05em".to_string()),
        "tight" => Some("-0.025em".to_string()),
        "normal" => Some("0em".to_string()),
        "wide" => Some("0.025em".to_string()),
        "wider" => Some("0.05em".to_string()),
        "widest" => Some("0.1em".to_string()),
        _ => None,
    }
}

// ========== Resolver 7: Border / Radius ==========

fn resolve_border_radius(core: &str) -> Option<ResolvedClass> {
    // rounded-{size} variants
    if core == "rounded" || core.starts_with("rounded-") {
        let suffix = if core == "rounded" {
            ""
        } else {
            &core["rounded-".len()..]
        };
        return resolve_rounded(suffix);
    }

    // border-{width} — only the width patterns, not color (color handled by color resolver)
    if core == "border" {
        return Some(ResolvedClass {
            class_name: core.to_string(),
            declarations: vec![CssDeclaration {
                property: "border-width".to_string(),
                value: "1px".to_string(),
            }],
            variant: None,
            resolved: true,
            category: "border".to_string(),
            negative: false,
            important: false,
        });
    }

    if let Some(suffix) = strip_prefix_dash(core, "border") {
        // border-{n} where n is a number → width
        if let Ok(n) = suffix.parse::<u32>() {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "border-width".to_string(),
                    value: format!("{}px", n),
                }],
                variant: None,
                resolved: true,
                category: "border".to_string(),
                negative: false,
                important: false,
            });
        }

        // border-{side}-{n} patterns
        let sides = [
            ("t", "border-top-width"),
            ("r", "border-right-width"),
            ("b", "border-bottom-width"),
            ("l", "border-left-width"),
            ("x", "border-inline-width"),
            ("y", "border-block-width"),
        ];
        for (side, prop) in &sides {
            if suffix == *side {
                return Some(ResolvedClass {
                    class_name: core.to_string(),
                    declarations: vec![CssDeclaration {
                        property: prop.to_string(),
                        value: "1px".to_string(),
                    }],
                    variant: None,
                    resolved: true,
                    category: "border".to_string(),
                    negative: false,
                    important: false,
                });
            }
            let side_prefix = format!("{}-", side);
            if let Some(width_str) = suffix.strip_prefix(side_prefix.as_str()) {
                if let Ok(n) = width_str.parse::<u32>() {
                    return Some(ResolvedClass {
                        class_name: core.to_string(),
                        declarations: vec![CssDeclaration {
                            property: prop.to_string(),
                            value: format!("{}px", n),
                        }],
                        variant: None,
                        resolved: true,
                        category: "border".to_string(),
                        negative: false,
                        important: false,
                    });
                }
            }
        }

        // Border style
        let styles = ["solid", "dashed", "dotted", "double", "none"];
        if styles.contains(&suffix) {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "border-style".to_string(),
                    value: suffix.to_string(),
                }],
                variant: None,
                resolved: true,
                category: "border".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // divide-{side}-{width} — simplified
    if let Some(suffix) = strip_prefix_dash(core, "divide") {
        if suffix == "x" || suffix == "y" {
            let prop = if suffix == "x" {
                "border-inline-width"
            } else {
                "border-block-width"
            };
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: prop.to_string(),
                    value: "1px".to_string(),
                }],
                variant: None,
                resolved: true,
                category: "border".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    None
}

fn resolve_rounded(suffix: &str) -> Option<ResolvedClass> {
    // Corner-specific: rounded-{corner}-{size}
    let corners = [
        ("t", &["border-top-left-radius", "border-top-right-radius"][..]),
        ("r", &["border-top-right-radius", "border-bottom-right-radius"]),
        ("b", &["border-bottom-right-radius", "border-bottom-left-radius"]),
        ("l", &["border-top-left-radius", "border-bottom-left-radius"]),
        ("tl", &["border-top-left-radius"][..]),
        ("tr", &["border-top-right-radius"][..]),
        ("br", &["border-bottom-right-radius"][..]),
        ("bl", &["border-bottom-left-radius"][..]),
    ];

    for (corner, props) in &corners {
        if suffix == *corner {
            let declarations = props
                .iter()
                .map(|p| CssDeclaration {
                    property: p.to_string(),
                    value: "0.25rem".to_string(),
                })
                .collect();
            return Some(ResolvedClass {
                class_name: format!("rounded-{}", suffix),
                declarations,
                variant: None,
                resolved: true,
                category: "border".to_string(),
                negative: false,
                important: false,
            });
        }
        let corner_prefix = format!("{}-", corner);
        if let Some(size_suffix) = suffix.strip_prefix(corner_prefix.as_str()) {
            if let Some(val) = radius_value(size_suffix) {
                let declarations = props
                    .iter()
                    .map(|p| CssDeclaration {
                        property: p.to_string(),
                        value: val.clone(),
                    })
                    .collect();
                return Some(ResolvedClass {
                    class_name: format!("rounded-{}", suffix),
                    declarations,
                    variant: None,
                    resolved: true,
                    category: "border".to_string(),
                    negative: false,
                    important: false,
                });
            }
        }
    }

    // General rounded: rounded or rounded-{size}
    let val = if suffix.is_empty() {
        "0.25rem".to_string()
    } else {
        radius_value(suffix)?
    };

    Some(ResolvedClass {
        class_name: if suffix.is_empty() {
            "rounded".to_string()
        } else {
            format!("rounded-{}", suffix)
        },
        declarations: vec![CssDeclaration {
            property: "border-radius".to_string(),
            value: val,
        }],
        variant: None,
        resolved: true,
        category: "border".to_string(),
        negative: false,
        important: false,
    })
}

fn radius_value(suffix: &str) -> Option<String> {
    match suffix {
        "none" => Some("0px".to_string()),
        "xs" => Some("0.0625rem".to_string()),
        "sm" => Some("0.125rem".to_string()),
        "md" => Some("0.375rem".to_string()),
        "lg" => Some("0.5rem".to_string()),
        "xl" => Some("0.75rem".to_string()),
        "2xl" => Some("1rem".to_string()),
        "3xl" => Some("1.5rem".to_string()),
        "full" => Some("9999px".to_string()),
        _ => None,
    }
}

// ========== Resolver 8: Opacity / z-index ==========

fn resolve_opacity_z(core: &str) -> Option<ResolvedClass> {
    // opacity-{n}
    if let Some(suffix) = strip_prefix_dash(core, "opacity") {
        if let Ok(n) = suffix.parse::<u32>() {
            let val = n as f64 / 100.0;
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "opacity".to_string(),
                    value: format!("{}", val),
                }],
                variant: None,
                resolved: true,
                category: "effect".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // z-{n}
    if let Some(suffix) = strip_prefix_dash(core, "z") {
        match suffix {
            "auto" => {
                return Some(ResolvedClass {
                    class_name: core.to_string(),
                    declarations: vec![CssDeclaration {
                        property: "z-index".to_string(),
                        value: "auto".to_string(),
                    }],
                    variant: None,
                    resolved: true,
                    category: "layout".to_string(),
                    negative: false,
                    important: false,
                });
            }
            _ => {
                if let Ok(n) = suffix.parse::<i32>() {
                    return Some(ResolvedClass {
                        class_name: core.to_string(),
                        declarations: vec![CssDeclaration {
                            property: "z-index".to_string(),
                            value: n.to_string(),
                        }],
                        variant: None,
                        resolved: true,
                        category: "layout".to_string(),
                        negative: false,
                        important: false,
                    });
                }
            }
        }
    }

    // Duration
    if let Some(suffix) = strip_prefix_dash(core, "duration") {
        if let Ok(n) = suffix.parse::<u32>() {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "transition-duration".to_string(),
                    value: format!("{}ms", n),
                }],
                variant: None,
                resolved: true,
                category: "transition".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // Delay
    if let Some(suffix) = strip_prefix_dash(core, "delay") {
        if let Ok(n) = suffix.parse::<u32>() {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "transition-delay".to_string(),
                    value: format!("{}ms", n),
                }],
                variant: None,
                resolved: true,
                category: "transition".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // Scale
    if let Some(suffix) = strip_prefix_dash(core, "scale") {
        if let Ok(n) = suffix.parse::<u32>() {
            let val = n as f64 / 100.0;
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![
                    CssDeclaration {
                        property: "--tw-scale-x".to_string(),
                        value: format!("{}", val),
                    },
                    CssDeclaration {
                        property: "--tw-scale-y".to_string(),
                        value: format!("{}", val),
                    },
                ],
                variant: None,
                resolved: true,
                category: "transform".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // Rotate
    if let Some(suffix) = strip_prefix_dash(core, "rotate") {
        if let Ok(n) = suffix.parse::<i32>() {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "--tw-rotate".to_string(),
                    value: format!("{}deg", n),
                }],
                variant: None,
                resolved: true,
                category: "transform".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    None
}

// ========== Resolver 9: Grid ==========

fn resolve_grid(core: &str) -> Option<ResolvedClass> {
    // grid-cols-{n}
    if let Some(suffix) = strip_prefix_dash(core, "grid-cols") {
        if suffix == "none" {
            return Some(make_grid_class(core, "grid-template-columns", "none"));
        }
        if suffix == "subgrid" {
            return Some(make_grid_class(core, "grid-template-columns", "subgrid"));
        }
        if let Ok(n) = suffix.parse::<u32>() {
            let val = format!("repeat({}, minmax(0, 1fr))", n);
            return Some(make_grid_class(core, "grid-template-columns", &val));
        }
    }

    // grid-rows-{n}
    if let Some(suffix) = strip_prefix_dash(core, "grid-rows") {
        if suffix == "none" {
            return Some(make_grid_class(core, "grid-template-rows", "none"));
        }
        if suffix == "subgrid" {
            return Some(make_grid_class(core, "grid-template-rows", "subgrid"));
        }
        if let Ok(n) = suffix.parse::<u32>() {
            let val = format!("repeat({}, minmax(0, 1fr))", n);
            return Some(make_grid_class(core, "grid-template-rows", &val));
        }
    }

    // col-span-{n}
    if let Some(suffix) = strip_prefix_dash(core, "col-span") {
        if suffix == "full" {
            return Some(make_grid_class(core, "grid-column", "1 / -1"));
        }
        if let Ok(n) = suffix.parse::<u32>() {
            let val = format!("span {} / span {}", n, n);
            return Some(make_grid_class(core, "grid-column", &val));
        }
    }

    // col-start-{n} / col-end-{n}
    if let Some(suffix) = strip_prefix_dash(core, "col-start") {
        if suffix == "auto" {
            return Some(make_grid_class(core, "grid-column-start", "auto"));
        }
        if let Ok(n) = suffix.parse::<u32>() {
            return Some(make_grid_class(core, "grid-column-start", &n.to_string()));
        }
    }
    if let Some(suffix) = strip_prefix_dash(core, "col-end") {
        if suffix == "auto" {
            return Some(make_grid_class(core, "grid-column-end", "auto"));
        }
        if let Ok(n) = suffix.parse::<u32>() {
            return Some(make_grid_class(core, "grid-column-end", &n.to_string()));
        }
    }

    // row-span-{n}
    if let Some(suffix) = strip_prefix_dash(core, "row-span") {
        if suffix == "full" {
            return Some(make_grid_class(core, "grid-row", "1 / -1"));
        }
        if let Ok(n) = suffix.parse::<u32>() {
            let val = format!("span {} / span {}", n, n);
            return Some(make_grid_class(core, "grid-row", &val));
        }
    }

    // auto-cols / auto-rows
    if let Some(suffix) = strip_prefix_dash(core, "auto-cols") {
        let val = match suffix {
            "auto" => "auto",
            "min" => "min-content",
            "max" => "max-content",
            "fr" => "minmax(0, 1fr)",
            _ => return None,
        };
        return Some(make_grid_class(core, "grid-auto-columns", val));
    }
    if let Some(suffix) = strip_prefix_dash(core, "auto-rows") {
        let val = match suffix {
            "auto" => "auto",
            "min" => "min-content",
            "max" => "max-content",
            "fr" => "minmax(0, 1fr)",
            _ => return None,
        };
        return Some(make_grid_class(core, "grid-auto-rows", val));
    }

    // grid-flow
    if let Some(suffix) = strip_prefix_dash(core, "grid-flow") {
        let val = match suffix {
            "row" => "row",
            "col" => "column",
            "dense" => "dense",
            "row-dense" => "row dense",
            "col-dense" => "column dense",
            _ => return None,
        };
        return Some(make_grid_class(core, "grid-auto-flow", val));
    }

    None
}

fn make_grid_class(core: &str, property: &str, value: &str) -> ResolvedClass {
    ResolvedClass {
        class_name: core.to_string(),
        declarations: vec![CssDeclaration {
            property: property.to_string(),
            value: value.to_string(),
        }],
        variant: None,
        resolved: true,
        category: "grid".to_string(),
        negative: false,
        important: false,
    }
}

// ========== Resolver 10: Filters (blur, backdrop-blur, brightness, etc.) ==========

fn resolve_filters(core: &str) -> Option<ResolvedClass> {
    // backdrop-blur-{size}
    if let Some(suffix) = strip_prefix_dash(core, "backdrop-blur") {
        let val = blur_value(suffix)?;
        return Some(ResolvedClass {
            class_name: core.to_string(),
            declarations: vec![CssDeclaration {
                property: "backdrop-filter".to_string(),
                value: format!("blur({})", val),
            }],
            variant: None,
            resolved: true,
            category: "filter".to_string(),
            negative: false,
            important: false,
        });
    }
    // backdrop-blur (default)
    if core == "backdrop-blur" {
        return Some(ResolvedClass {
            class_name: core.to_string(),
            declarations: vec![CssDeclaration {
                property: "backdrop-filter".to_string(),
                value: "blur(8px)".to_string(),
            }],
            variant: None,
            resolved: true,
            category: "filter".to_string(),
            negative: false,
            important: false,
        });
    }

    // blur-{size}
    if let Some(suffix) = strip_prefix_dash(core, "blur") {
        let val = blur_value(suffix)?;
        return Some(ResolvedClass {
            class_name: core.to_string(),
            declarations: vec![CssDeclaration {
                property: "filter".to_string(),
                value: format!("blur({})", val),
            }],
            variant: None,
            resolved: true,
            category: "filter".to_string(),
            negative: false,
            important: false,
        });
    }
    // blur (default)
    if core == "blur" {
        return Some(ResolvedClass {
            class_name: core.to_string(),
            declarations: vec![CssDeclaration {
                property: "filter".to_string(),
                value: "blur(8px)".to_string(),
            }],
            variant: None,
            resolved: true,
            category: "filter".to_string(),
            negative: false,
            important: false,
        });
    }

    // brightness-{n}, contrast-{n}, saturate-{n} — percentage-based
    let pct_filters = [
        ("brightness", "brightness"),
        ("contrast", "contrast"),
        ("saturate", "saturate"),
    ];
    for (prefix, fn_name) in &pct_filters {
        if let Some(suffix) = strip_prefix_dash(core, prefix) {
            if let Ok(n) = suffix.parse::<u32>() {
                let val = n as f64 / 100.0;
                return Some(ResolvedClass {
                    class_name: core.to_string(),
                    declarations: vec![CssDeclaration {
                        property: "filter".to_string(),
                        value: format!("{}({})", fn_name, val),
                    }],
                    variant: None,
                    resolved: true,
                    category: "filter".to_string(),
                    negative: false,
                    important: false,
                });
            }
        }
    }

    // grayscale, invert, sepia — toggle filters
    let toggle_filters = [
        ("grayscale", "grayscale"),
        ("invert", "invert"),
        ("sepia", "sepia"),
    ];
    for (name, fn_name) in &toggle_filters {
        if core == *name {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "filter".to_string(),
                    value: format!("{}(100%)", fn_name),
                }],
                variant: None,
                resolved: true,
                category: "filter".to_string(),
                negative: false,
                important: false,
            });
        }
        let with_zero = format!("{}-0", name);
        if core == with_zero {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "filter".to_string(),
                    value: format!("{}(0)", fn_name),
                }],
                variant: None,
                resolved: true,
                category: "filter".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // hue-rotate-{deg}
    if let Some(suffix) = strip_prefix_dash(core, "hue-rotate") {
        if let Ok(n) = suffix.parse::<i32>() {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "filter".to_string(),
                    value: format!("hue-rotate({}deg)", n),
                }],
                variant: None,
                resolved: true,
                category: "filter".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // drop-shadow-{size}
    if core == "drop-shadow" || core.starts_with("drop-shadow-") {
        let suffix = if core == "drop-shadow" {
            ""
        } else {
            &core["drop-shadow-".len()..]
        };
        let val = match suffix {
            "" => "drop-shadow(0 1px 2px rgb(0 0 0 / 0.1)) drop-shadow(0 1px 1px rgb(0 0 0 / 0.06))",
            "sm" => "drop-shadow(0 1px 1px rgb(0 0 0 / 0.05))",
            "md" => "drop-shadow(0 4px 3px rgb(0 0 0 / 0.07)) drop-shadow(0 2px 2px rgb(0 0 0 / 0.06))",
            "lg" => "drop-shadow(0 10px 8px rgb(0 0 0 / 0.04)) drop-shadow(0 4px 3px rgb(0 0 0 / 0.1))",
            "xl" => "drop-shadow(0 20px 13px rgb(0 0 0 / 0.03)) drop-shadow(0 8px 5px rgb(0 0 0 / 0.08))",
            "2xl" => "drop-shadow(0 25px 25px rgb(0 0 0 / 0.15))",
            "none" => "drop-shadow(0 0 #0000)",
            _ => return None,
        };
        return Some(ResolvedClass {
            class_name: core.to_string(),
            declarations: vec![CssDeclaration {
                property: "filter".to_string(),
                value: val.to_string(),
            }],
            variant: None,
            resolved: true,
            category: "filter".to_string(),
            negative: false,
            important: false,
        });
    }

    // line-clamp-{n}
    if let Some(suffix) = strip_prefix_dash(core, "line-clamp") {
        if suffix == "none" {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![
                    CssDeclaration { property: "overflow".to_string(), value: "visible".to_string() },
                    CssDeclaration { property: "display".to_string(), value: "block".to_string() },
                    CssDeclaration { property: "-webkit-box-orient".to_string(), value: "horizontal".to_string() },
                    CssDeclaration { property: "-webkit-line-clamp".to_string(), value: "unset".to_string() },
                ],
                variant: None,
                resolved: true,
                category: "typography".to_string(),
                negative: false,
                important: false,
            });
        }
        if let Ok(n) = suffix.parse::<u32>() {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![
                    CssDeclaration { property: "overflow".to_string(), value: "hidden".to_string() },
                    CssDeclaration { property: "display".to_string(), value: "-webkit-box".to_string() },
                    CssDeclaration { property: "-webkit-box-orient".to_string(), value: "vertical".to_string() },
                    CssDeclaration { property: "-webkit-line-clamp".to_string(), value: n.to_string() },
                ],
                variant: None,
                resolved: true,
                category: "typography".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    // stroke-{width}
    if let Some(suffix) = strip_prefix_dash(core, "stroke") {
        if let Ok(n) = suffix.parse::<u32>() {
            return Some(ResolvedClass {
                class_name: core.to_string(),
                declarations: vec![CssDeclaration {
                    property: "stroke-width".to_string(),
                    value: n.to_string(),
                }],
                variant: None,
                resolved: true,
                category: "svg".to_string(),
                negative: false,
                important: false,
            });
        }
    }

    None
}

fn blur_value(suffix: &str) -> Option<String> {
    match suffix {
        "none" => Some("0".to_string()),
        "sm" => Some("4px".to_string()),
        "md" => Some("12px".to_string()),
        "lg" => Some("16px".to_string()),
        "xl" => Some("24px".to_string()),
        "2xl" => Some("40px".to_string()),
        "3xl" => Some("64px".to_string()),
        _ => None,
    }
}

// ========== Resolver 11: Translate ==========

fn resolve_translate(core: &str, negative: bool) -> Option<ResolvedClass> {
    let axes = [
        ("translate-x", "--tw-translate-x"),
        ("translate-y", "--tw-translate-y"),
    ];

    for (prefix, property) in &axes {
        if let Some(suffix) = strip_prefix_dash(core, prefix) {
            if let Some(value) = spacing_value(suffix) {
                let value = if negative { format!("-{}", value) } else { value };
                return Some(ResolvedClass {
                    class_name: core.to_string(),
                    declarations: vec![CssDeclaration {
                        property: property.to_string(),
                        value,
                    }],
                    variant: None,
                    resolved: true,
                    category: "transform".to_string(),
                    negative,
                    important: false,
                });
            }
            // Fractions: translate-x-1/2 → 50%
            if let Some(slash) = suffix.find('/') {
                let num: f64 = suffix[..slash].parse().ok()?;
                let den: f64 = suffix[slash + 1..].parse().ok()?;
                if den != 0.0 {
                    let pct = (num / den) * 100.0;
                    let value = if negative {
                        format!("-{:.6}%", pct)
                    } else {
                        format!("{:.6}%", pct)
                    };
                    return Some(ResolvedClass {
                        class_name: core.to_string(),
                        declarations: vec![CssDeclaration {
                            property: property.to_string(),
                            value,
                        }],
                        variant: None,
                        resolved: true,
                        category: "transform".to_string(),
                        negative,
                        important: false,
                    });
                }
            }
        }
    }

    None
}

// ========== Helpers ==========

/// Strip a prefix followed by a dash: "p" from "p-4" → "4", "bg" from "bg-red-500" → "red-500"
fn strip_prefix_dash<'a>(class: &'a str, prefix: &str) -> Option<&'a str> {
    let with_dash = class.strip_prefix(prefix)?;
    with_dash.strip_prefix('-')
}

// ========== Tests ==========

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, content: &str) -> String {
        let path = std::env::temp_dir().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn resolve(classes: &[&str]) -> TailwindResolution {
        resolve_tailwind_classes(classes, None).unwrap()
    }

    fn resolve_one(class: &str) -> ResolvedClass {
        resolve(&[class]).classes.into_iter().next().unwrap()
    }

    fn prop_value(rc: &ResolvedClass, property: &str) -> String {
        rc.declarations
            .iter()
            .find(|d| d.property == property)
            .map(|d| d.value.clone())
            .unwrap_or_default()
    }

    // ===== @theme parser =====

    #[test]
    fn test_theme_parsing_colors() {
        let css = r#"
@theme {
    --color-kb-primary: #3b82f6;
    --color-kb-secondary: #64748b;
}
"#;
        let ctx = parse_theme_block(css);
        assert_eq!(ctx.colors.get("--color-kb-primary").unwrap(), "#3b82f6");
        assert_eq!(ctx.colors.get("--color-kb-secondary").unwrap(), "#64748b");
    }

    #[test]
    fn test_theme_parsing_spacing() {
        let css = "@theme {\n    --spacing: 0.25rem;\n    --spacing-lg: 2rem;\n}\n";
        let ctx = parse_theme_block(css);
        assert_eq!(ctx.spacing.get("--spacing").unwrap(), "0.25rem");
        assert_eq!(ctx.spacing.get("--spacing-lg").unwrap(), "2rem");
    }

    #[test]
    fn test_theme_parsing_fonts() {
        let css = "@theme {\n    --font-display: 'Inter', sans-serif;\n}\n";
        let ctx = parse_theme_block(css);
        assert_eq!(
            ctx.fonts.get("--font-display").unwrap(),
            "'Inter', sans-serif"
        );
    }

    #[test]
    fn test_theme_parsing_empty() {
        let ctx = parse_theme_block("@theme {}\n");
        assert!(ctx.colors.is_empty());
        assert!(ctx.spacing.is_empty());
    }

    #[test]
    fn test_theme_with_modifiers() {
        let css = "@theme default {\n    --color-brand: blue;\n}\n";
        let ctx = parse_theme_block(css);
        assert_eq!(ctx.colors.get("--color-brand").unwrap(), "blue");
    }

    #[test]
    fn test_theme_skips_non_theme_blocks() {
        let css = r#"
:root { --foo: bar; }
@theme { --color-x: red; }
.class { color: blue; }
"#;
        let ctx = parse_theme_block(css);
        assert_eq!(ctx.colors.len(), 1);
        assert!(ctx.colors.contains_key("--color-x"));
    }

    // ===== Static utilities =====

    #[test]
    fn test_static_flex() {
        let rc = resolve_one("flex");
        assert!(rc.resolved);
        assert_eq!(rc.category, "layout");
        assert_eq!(prop_value(&rc, "display"), "flex");
    }

    #[test]
    fn test_static_hidden() {
        let rc = resolve_one("hidden");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "display"), "none");
    }

    #[test]
    fn test_static_truncate() {
        let rc = resolve_one("truncate");
        assert!(rc.resolved);
        assert_eq!(rc.declarations.len(), 3);
        assert_eq!(prop_value(&rc, "overflow"), "hidden");
        assert_eq!(prop_value(&rc, "text-overflow"), "ellipsis");
        assert_eq!(prop_value(&rc, "white-space"), "nowrap");
    }

    #[test]
    fn test_static_sr_only() {
        let rc = resolve_one("sr-only");
        assert!(rc.resolved);
        assert_eq!(rc.category, "accessibility");
        assert_eq!(rc.declarations.len(), 9);
    }

    #[test]
    fn test_static_cursor_pointer() {
        let rc = resolve_one("cursor-pointer");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "cursor"), "pointer");
    }

    // ===== Spacing =====

    #[test]
    fn test_spacing_p4() {
        let rc = resolve_one("p-4");
        assert!(rc.resolved);
        assert_eq!(rc.category, "spacing");
        assert_eq!(prop_value(&rc, "padding"), "1rem");
    }

    #[test]
    fn test_spacing_mx2() {
        let rc = resolve_one("mx-2");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "margin-left"), "0.5rem");
        assert_eq!(prop_value(&rc, "margin-right"), "0.5rem");
    }

    #[test]
    fn test_spacing_gap3() {
        let rc = resolve_one("gap-3");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "gap"), "0.75rem");
    }

    #[test]
    fn test_spacing_p0() {
        let rc = resolve_one("p-0");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "padding"), "0px");
    }

    #[test]
    fn test_spacing_p_px() {
        let rc = resolve_one("p-px");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "padding"), "1px");
    }

    #[test]
    fn test_spacing_p_half() {
        let rc = resolve_one("p-0.5");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "padding"), "0.125rem");
    }

    #[test]
    fn test_spacing_negative_mt4() {
        let rc = resolve_one("-mt-4");
        assert!(rc.resolved);
        assert!(rc.negative);
        assert_eq!(prop_value(&rc, "margin-top"), "-1rem");
    }

    #[test]
    fn test_spacing_m_auto() {
        let rc = resolve_one("m-auto");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "margin"), "auto");
    }

    // ===== Colors =====

    #[test]
    fn test_color_bg_red_500() {
        let rc = resolve_one("bg-red-500");
        assert!(rc.resolved);
        assert_eq!(rc.category, "color");
        assert_eq!(prop_value(&rc, "background-color"), "var(--color-red-500)");
    }

    #[test]
    fn test_color_text_white() {
        let rc = resolve_one("text-white");
        assert!(rc.resolved);
        assert_eq!(rc.category, "color");
        assert_eq!(prop_value(&rc, "color"), "#fff");
    }

    #[test]
    fn test_color_text_transparent() {
        let rc = resolve_one("text-transparent");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "color"), "transparent");
    }

    #[test]
    fn test_color_border_blue_200() {
        let rc = resolve_one("border-blue-200");
        assert!(rc.resolved);
        assert_eq!(rc.category, "color");
        assert_eq!(
            prop_value(&rc, "border-color"),
            "var(--color-blue-200)"
        );
    }

    #[test]
    fn test_color_custom_theme() {
        let css = "@theme {\n    --color-kb-primary: #3b82f6;\n}\n";
        let path = write_temp("test_tw_theme.css", css);
        let result = resolve_tailwind_classes(&["bg-kb-primary"], Some(&path)).unwrap();
        let rc = &result.classes[0];
        assert!(rc.resolved);
        assert_eq!(prop_value(rc, "background-color"), "#3b82f6");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_color_with_opacity() {
        let rc = resolve_one("bg-red-500/50");
        assert!(rc.resolved);
        assert_eq!(
            prop_value(&rc, "background-color"),
            "var(--color-red-500) / 50%"
        );
    }

    // ===== Sizing =====

    #[test]
    fn test_sizing_w_full() {
        let rc = resolve_one("w-full");
        assert!(rc.resolved);
        assert_eq!(rc.category, "sizing");
        assert_eq!(prop_value(&rc, "width"), "100%");
    }

    #[test]
    fn test_sizing_w_screen() {
        let rc = resolve_one("w-screen");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "width"), "100vw");
    }

    #[test]
    fn test_sizing_h_half() {
        let rc = resolve_one("h-1/2");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "height"), "50.000000%");
    }

    #[test]
    fn test_sizing_w4() {
        let rc = resolve_one("w-4");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "width"), "1rem");
    }

    #[test]
    fn test_sizing_max_w_prose() {
        let rc = resolve_one("max-w-prose");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "max-width"), "65ch");
    }

    #[test]
    fn test_sizing_w_auto() {
        let rc = resolve_one("w-auto");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "width"), "auto");
    }

    #[test]
    fn test_sizing_w_fit() {
        let rc = resolve_one("w-fit");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "width"), "fit-content");
    }

    #[test]
    fn test_sizing_size_4() {
        let rc = resolve_one("size-4");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "width"), "1rem");
        assert_eq!(prop_value(&rc, "height"), "1rem");
    }

    // ===== Typography =====

    #[test]
    fn test_typography_text_sm() {
        let rc = resolve_one("text-sm");
        assert!(rc.resolved);
        assert_eq!(rc.category, "typography");
        assert_eq!(prop_value(&rc, "font-size"), "0.875rem");
        assert_eq!(prop_value(&rc, "line-height"), "1.25rem");
    }

    #[test]
    fn test_typography_font_bold() {
        let rc = resolve_one("font-bold");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "font-weight"), "700");
    }

    #[test]
    fn test_typography_font_sans() {
        let rc = resolve_one("font-sans");
        assert!(rc.resolved);
        assert!(prop_value(&rc, "font-family").contains("sans-serif"));
    }

    #[test]
    fn test_typography_leading_tight() {
        let rc = resolve_one("leading-tight");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "line-height"), "1.25");
    }

    #[test]
    fn test_typography_tracking_wide() {
        let rc = resolve_one("tracking-wide");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "letter-spacing"), "0.025em");
    }

    // ===== Border / Radius =====

    #[test]
    fn test_rounded_lg() {
        let rc = resolve_one("rounded-lg");
        assert!(rc.resolved);
        assert_eq!(rc.category, "border");
        assert_eq!(prop_value(&rc, "border-radius"), "0.5rem");
    }

    #[test]
    fn test_rounded_full() {
        let rc = resolve_one("rounded-full");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "border-radius"), "9999px");
    }

    #[test]
    fn test_border_2() {
        let rc = resolve_one("border-2");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "border-width"), "2px");
    }

    #[test]
    fn test_border_default() {
        let rc = resolve_one("border");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "border-width"), "1px");
    }

    // ===== Arbitrary values =====

    #[test]
    fn test_arbitrary_w() {
        let rc = resolve_one("w-[200px]");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "width"), "200px");
    }

    #[test]
    fn test_arbitrary_bg_hex() {
        let rc = resolve_one("bg-[#ff0]");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "background-color"), "#ff0");
    }

    #[test]
    fn test_arbitrary_grid_cols() {
        let rc = resolve_one("grid-cols-[1fr_2fr]");
        assert!(rc.resolved);
        assert_eq!(
            prop_value(&rc, "grid-template-columns"),
            "1fr 2fr"
        );
    }

    // ===== Variants =====

    #[test]
    fn test_variant_hover() {
        let rc = resolve_one("hover:text-white");
        assert!(rc.resolved);
        assert_eq!(rc.variant, Some("hover".to_string()));
        assert_eq!(prop_value(&rc, "color"), "#fff");
    }

    #[test]
    fn test_variant_md() {
        let rc = resolve_one("md:flex");
        assert!(rc.resolved);
        assert_eq!(rc.variant, Some("md".to_string()));
        assert_eq!(prop_value(&rc, "display"), "flex");
    }

    #[test]
    fn test_variant_dark() {
        let rc = resolve_one("dark:bg-gray-900");
        assert!(rc.resolved);
        assert_eq!(rc.variant, Some("dark".to_string()));
    }

    #[test]
    fn test_variant_stacked() {
        let rc = resolve_one("md:hover:text-blue-500");
        assert!(rc.resolved);
        assert_eq!(rc.variant, Some("md:hover".to_string()));
    }

    // ===== Opacity / z-index =====

    #[test]
    fn test_opacity_50() {
        let rc = resolve_one("opacity-50");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "opacity"), "0.5");
    }

    #[test]
    fn test_z_10() {
        let rc = resolve_one("z-10");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "z-index"), "10");
    }

    #[test]
    fn test_z_auto() {
        let rc = resolve_one("z-auto");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "z-index"), "auto");
    }

    // ===== Grid =====

    #[test]
    fn test_grid_cols_3() {
        let rc = resolve_one("grid-cols-3");
        assert!(rc.resolved);
        assert_eq!(rc.category, "grid");
        assert_eq!(
            prop_value(&rc, "grid-template-columns"),
            "repeat(3, minmax(0, 1fr))"
        );
    }

    #[test]
    fn test_grid_rows_2() {
        let rc = resolve_one("grid-rows-2");
        assert!(rc.resolved);
        assert_eq!(
            prop_value(&rc, "grid-template-rows"),
            "repeat(2, minmax(0, 1fr))"
        );
    }

    #[test]
    fn test_col_span_2() {
        let rc = resolve_one("col-span-2");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "grid-column"), "span 2 / span 2");
    }

    #[test]
    fn test_col_span_full() {
        let rc = resolve_one("col-span-full");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "grid-column"), "1 / -1");
    }

    // ===== Unresolved =====

    #[test]
    fn test_unresolved() {
        let rc = resolve_one("fancy-unknown-class");
        assert!(!rc.resolved);
        assert_eq!(rc.category, "unresolved");
    }

    // ===== Mixed batch =====

    #[test]
    fn test_mixed_batch() {
        let result = resolve(&["flex", "p-4", "bg-red-500", "hover:text-white", "w-[200px]"]);
        assert_eq!(result.stats.total, 5);
        assert_eq!(result.stats.resolved, 5);
        assert_eq!(result.stats.unresolved, 0);
        assert_eq!(result.stats.with_variants, 1);
    }

    // ===== Empty input =====

    #[test]
    fn test_empty_input() {
        let result = resolve(&[]);
        assert_eq!(result.stats.total, 0);
        assert!(result.classes.is_empty());
    }

    // ===== Stats =====

    #[test]
    fn test_stats() {
        let result = resolve(&["flex", "p-4", "unknown-thing", "hover:bg-white"]);
        assert_eq!(result.stats.total, 4);
        assert_eq!(result.stats.resolved, 3);
        assert_eq!(result.stats.unresolved, 1);
        assert_eq!(result.stats.with_variants, 1);
    }

    // ===== Negative variant edge case =====

    #[test]
    fn test_negative_with_variant() {
        let rc = resolve_one("md:hover:-mt-4");
        assert!(rc.resolved);
        assert!(rc.negative);
        assert_eq!(rc.variant, Some("md:hover".to_string()));
        assert_eq!(prop_value(&rc, "margin-top"), "-1rem");
    }

    // ===== Duration / scale / rotate =====

    #[test]
    fn test_duration_300() {
        let rc = resolve_one("duration-300");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "transition-duration"), "300ms");
    }

    #[test]
    fn test_scale_150() {
        let rc = resolve_one("scale-150");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "--tw-scale-x"), "1.5");
    }

    #[test]
    fn test_rotate_45() {
        let rc = resolve_one("rotate-45");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "--tw-rotate"), "45deg");
    }

    // ===== Important modifier =====

    #[test]
    fn test_important_leading() {
        let rc = resolve_one("!p-0");
        assert!(rc.resolved);
        assert!(rc.important);
        assert_eq!(prop_value(&rc, "padding"), "0px");
    }

    #[test]
    fn test_important_trailing() {
        let rc = resolve_one("bg-red-500!");
        assert!(rc.resolved);
        assert!(rc.important);
        assert_eq!(rc.category, "color");
    }

    #[test]
    fn test_important_not_set_normally() {
        let rc = resolve_one("flex");
        assert!(!rc.important);
    }

    // ===== Lenient color fallback =====

    #[test]
    fn test_lenient_color_base_100() {
        let rc = resolve_one("bg-base-100");
        assert!(rc.resolved);
        assert_eq!(rc.category, "color");
        assert_eq!(prop_value(&rc, "background-color"), "var(--color-base-100)");
    }

    #[test]
    fn test_lenient_color_muted_foreground() {
        let rc = resolve_one("text-muted-foreground");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "color"), "var(--color-muted-foreground)");
    }

    #[test]
    fn test_lenient_color_danger() {
        let rc = resolve_one("border-danger-600");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "border-color"), "var(--color-danger-600)");
    }

    #[test]
    fn test_lenient_doesnt_steal_border_width() {
        // border-2 should still resolve as border-width, not color
        let rc = resolve_one("border-2");
        assert!(rc.resolved);
        assert_eq!(rc.category, "border");
        assert_eq!(prop_value(&rc, "border-width"), "2px");
    }

    #[test]
    fn test_lenient_doesnt_steal_border_style() {
        let rc = resolve_one("border-solid");
        assert!(rc.resolved);
        assert_eq!(rc.category, "border");
        assert_eq!(prop_value(&rc, "border-style"), "solid");
    }

    // ===== Backdrop / Blur filters =====

    #[test]
    fn test_backdrop_blur_md() {
        let rc = resolve_one("backdrop-blur-md");
        assert!(rc.resolved);
        assert_eq!(rc.category, "filter");
        assert_eq!(prop_value(&rc, "backdrop-filter"), "blur(12px)");
    }

    #[test]
    fn test_backdrop_blur_default() {
        let rc = resolve_one("backdrop-blur");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "backdrop-filter"), "blur(8px)");
    }

    #[test]
    fn test_blur_lg() {
        let rc = resolve_one("blur-lg");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "filter"), "blur(16px)");
    }

    #[test]
    fn test_blur_default() {
        let rc = resolve_one("blur");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "filter"), "blur(8px)");
    }

    #[test]
    fn test_brightness_150() {
        let rc = resolve_one("brightness-150");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "filter"), "brightness(1.5)");
    }

    #[test]
    fn test_grayscale() {
        let rc = resolve_one("grayscale");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "filter"), "grayscale(100%)");
    }

    #[test]
    fn test_invert_0() {
        let rc = resolve_one("invert-0");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "filter"), "invert(0)");
    }

    #[test]
    fn test_drop_shadow_lg() {
        let rc = resolve_one("drop-shadow-lg");
        assert!(rc.resolved);
        assert!(prop_value(&rc, "filter").contains("drop-shadow"));
    }

    // ===== Translate =====

    #[test]
    fn test_translate_x_4() {
        let rc = resolve_one("translate-x-4");
        assert!(rc.resolved);
        assert_eq!(rc.category, "transform");
        assert_eq!(prop_value(&rc, "--tw-translate-x"), "1rem");
    }

    #[test]
    fn test_translate_y_half() {
        let rc = resolve_one("-translate-y-1/2");
        assert!(rc.resolved);
        assert!(rc.negative);
        assert_eq!(prop_value(&rc, "--tw-translate-y"), "-50.000000%");
    }

    #[test]
    fn test_translate_x_px() {
        let rc = resolve_one("-translate-y-px");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "--tw-translate-y"), "-1px");
    }

    // ===== Animations =====

    #[test]
    fn test_animate_spin() {
        let rc = resolve_one("animate-spin");
        assert!(rc.resolved);
        assert_eq!(rc.category, "animation");
        assert!(prop_value(&rc, "animation").contains("spin"));
    }

    #[test]
    fn test_animate_none() {
        let rc = resolve_one("animate-none");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "animation"), "none");
    }

    // ===== Gradients =====

    #[test]
    fn test_bg_gradient_to_r() {
        let rc = resolve_one("bg-gradient-to-r");
        assert!(rc.resolved);
        assert_eq!(rc.category, "gradient");
        assert!(prop_value(&rc, "background-image").contains("to right"));
    }

    // ===== Misc new statics =====

    #[test]
    fn test_group_marker() {
        let rc = resolve_one("group");
        assert!(rc.resolved);
        assert_eq!(rc.category, "marker");
    }

    #[test]
    fn test_tabular_nums() {
        let rc = resolve_one("tabular-nums");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "font-variant-numeric"), "tabular-nums");
    }

    #[test]
    fn test_touch_none() {
        let rc = resolve_one("touch-none");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "touch-action"), "none");
    }

    #[test]
    fn test_align_middle() {
        let rc = resolve_one("align-middle");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "vertical-align"), "middle");
    }

    #[test]
    fn test_border_collapse() {
        let rc = resolve_one("border-collapse");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "border-collapse"), "collapse");
    }

    #[test]
    fn test_fill_current() {
        let rc = resolve_one("fill-current");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "fill"), "currentColor");
    }

    #[test]
    fn test_outline_hidden() {
        let rc = resolve_one("outline-hidden");
        assert!(rc.resolved);
    }

    #[test]
    fn test_bg_clip_text() {
        let rc = resolve_one("bg-clip-text");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "background-clip"), "text");
    }

    #[test]
    fn test_line_clamp_2() {
        let rc = resolve_one("line-clamp-2");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "-webkit-line-clamp"), "2");
    }

    #[test]
    fn test_stroke_2() {
        let rc = resolve_one("stroke-2");
        assert!(rc.resolved);
        assert_eq!(prop_value(&rc, "stroke-width"), "2");
    }
}
