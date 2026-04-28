//! yq — YAML/XML/TOML/JSON processor using jaq filter engine.
//!
//! Converts input to JSON, runs jaq filter, converts output to requested format.
//! Reuses jaq-core/jaq-std/jaq-json (same engine as jq command).

use std::ffi::OsString;
use std::io::{self, Read, Write};

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, RcIter};
use jaq_json::Val;

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Yaml,
    Json,
    Toml,
    Xml,
}

struct YqOptions {
    filter: String,
    input_format: Option<Format>,
    output_format: Option<Format>,
    raw_output: bool,
    compact: bool,
    null_input: bool,
    slurp: bool,
}

/// Entry point for yq command.
pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    match run_yq(&str_args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("yq: {}", msg);
            2
        }
    }
}

fn parse_format(s: &str) -> Result<Format, String> {
    match s {
        "yaml" | "y" => Ok(Format::Yaml),
        "json" | "j" => Ok(Format::Json),
        "toml" | "t" => Ok(Format::Toml),
        "xml" | "x" => Ok(Format::Xml),
        _ => Err(format!(
            "unknown format: {} (expected yaml, json, toml, xml)",
            s
        )),
    }
}

fn parse_args(args: &[String]) -> Result<YqOptions, String> {
    let mut opts = YqOptions {
        filter: String::new(),
        input_format: None,
        output_format: None,
        raw_output: false,
        compact: false,
        null_input: false,
        slurp: false,
    };

    let mut filter_set = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            break;
        }

        if arg == "-p" || arg == "--input-format" {
            i += 1;
            if i >= args.len() {
                return Err("-p requires a format argument".to_string());
            }
            opts.input_format = Some(parse_format(&args[i])?);
        } else if arg == "-o" || arg == "--output-format" {
            i += 1;
            if i >= args.len() {
                return Err("-o requires a format argument".to_string());
            }
            opts.output_format = Some(parse_format(&args[i])?);
        } else if arg == "-r" || arg == "--raw-output" {
            opts.raw_output = true;
        } else if arg == "-c" || arg == "--compact-output" {
            opts.compact = true;
        } else if arg == "-n" || arg == "--null-input" {
            opts.null_input = true;
        } else if arg == "-s" || arg == "--slurp" {
            opts.slurp = true;
        } else if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            // Combined short flags like -rc
            let flags = &arg[1..];
            let mut chars = flags.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    'r' => opts.raw_output = true,
                    'c' => opts.compact = true,
                    'n' => opts.null_input = true,
                    's' => opts.slurp = true,
                    'p' => {
                        let rest: String = chars.collect();
                        if !rest.is_empty() {
                            opts.input_format = Some(parse_format(&rest)?);
                        } else {
                            i += 1;
                            if i >= args.len() {
                                return Err("-p requires a format argument".to_string());
                            }
                            opts.input_format = Some(parse_format(&args[i])?);
                        }
                        break;
                    }
                    'o' => {
                        let rest: String = chars.collect();
                        if !rest.is_empty() {
                            opts.output_format = Some(parse_format(&rest)?);
                        } else {
                            i += 1;
                            if i >= args.len() {
                                return Err("-o requires a format argument".to_string());
                            }
                            opts.output_format = Some(parse_format(&args[i])?);
                        }
                        break;
                    }
                    _ => return Err(format!("unknown option: -{}", c)),
                }
            }
        } else if !filter_set {
            opts.filter = arg.clone();
            filter_set = true;
        } else {
            return Err(format!("unexpected argument: {}", arg));
        }

        i += 1;
    }

    if !filter_set {
        opts.filter = ".".to_string();
    }

    Ok(opts)
}

fn detect_format(input: &str) -> Format {
    let trimmed = input.trim_start();

    // JSON: starts with { or [
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
            return Format::Json;
        }
    }

    // XML: starts with < (including <?xml declaration)
    if trimmed.starts_with('<') {
        return Format::Xml;
    }

    // TOML: has key = value syntax or [section] headers, but not YAML's key: value
    if !trimmed.contains(": ") && !trimmed.starts_with("---") {
        if (trimmed.contains(" = ") || trimmed.contains("= \"") || trimmed.starts_with('['))
            && toml::from_str::<toml::Value>(trimmed).is_ok()
        {
            return Format::Toml;
        }
    }

    // Default: YAML
    Format::Yaml
}

fn parse_input(input: &str, format: Format) -> Result<serde_json::Value, String> {
    match format {
        Format::Json => serde_json::from_str(input).map_err(|e| format!("invalid JSON: {}", e)),
        Format::Yaml => serde_yaml::from_str(input).map_err(|e| format!("invalid YAML: {}", e)),
        Format::Toml => {
            let toml_val: toml::Value =
                toml::from_str(input).map_err(|e| format!("invalid TOML: {}", e))?;
            toml_to_json(toml_val)
        }
        Format::Xml => xml_to_json(input),
    }
}

fn toml_to_json(val: toml::Value) -> Result<serde_json::Value, String> {
    Ok(match val {
        toml::Value::String(s) => serde_json::Value::String(s),
        toml::Value::Integer(i) => serde_json::Value::Number(serde_json::Number::from(i)),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(arr) => {
            let items: Result<Vec<_>, _> = arr.into_iter().map(toml_to_json).collect();
            serde_json::Value::Array(items?)
        }
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (k, v) in table {
                map.insert(k, toml_to_json(v)?);
            }
            serde_json::Value::Object(map)
        }
    })
}

fn json_to_toml(val: &serde_json::Value) -> Result<toml::Value, String> {
    Ok(match val {
        serde_json::Value::Null => return Err("TOML does not support null values".to_string()),
        serde_json::Value::Bool(b) => toml::Value::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                return Err("unsupported number for TOML".to_string());
            }
        }
        serde_json::Value::String(s) => toml::Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            let items: Result<Vec<_>, _> = arr.iter().map(json_to_toml).collect();
            toml::Value::Array(items?)
        }
        serde_json::Value::Object(map) => {
            let mut table = toml::map::Map::new();
            for (k, v) in map {
                table.insert(k.clone(), json_to_toml(v)?);
            }
            toml::Value::Table(table)
        }
    })
}

// --- XML parsing ---

fn xml_to_json(input: &str) -> Result<serde_json::Value, String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    struct StackEntry {
        name: String,
        children: serde_json::Map<String, serde_json::Value>,
        text: String,
    }

    let mut reader = Reader::from_str(input);
    let mut stack: Vec<StackEntry> = Vec::new();
    let mut root = serde_json::Map::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let mut children = serde_json::Map::new();
                for attr in e.attributes().flatten() {
                    let key = format!("@{}", String::from_utf8_lossy(attr.key.as_ref()));
                    let val = String::from_utf8_lossy(&attr.value).to_string();
                    children.insert(key, serde_json::Value::String(val));
                }
                stack.push(StackEntry {
                    name,
                    children,
                    text: String::new(),
                });
            }
            Ok(Event::End(_)) => {
                let entry = stack.pop().ok_or("unexpected closing tag")?;
                let text = entry.text.trim().to_string();

                let value = if entry.children.is_empty() && text.is_empty() {
                    serde_json::Value::Null
                } else if entry.children.is_empty() {
                    serde_json::Value::String(text)
                } else {
                    let mut obj = entry.children;
                    if !text.is_empty() {
                        obj.insert("#text".to_string(), serde_json::Value::String(text));
                    }
                    serde_json::Value::Object(obj)
                };

                let target = if let Some(parent) = stack.last_mut() {
                    &mut parent.children
                } else {
                    &mut root
                };

                insert_or_array(target, entry.name, value);
            }
            Ok(Event::Empty(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let mut attrs = serde_json::Map::new();
                for attr in e.attributes().flatten() {
                    let key = format!("@{}", String::from_utf8_lossy(attr.key.as_ref()));
                    let val = String::from_utf8_lossy(&attr.value).to_string();
                    attrs.insert(key, serde_json::Value::String(val));
                }

                let value = if attrs.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::Object(attrs)
                };

                let target = if let Some(parent) = stack.last_mut() {
                    &mut parent.children
                } else {
                    &mut root
                };

                insert_or_array(target, name, value);
            }
            Ok(Event::Text(ref e)) => {
                if let Some(entry) = stack.last_mut() {
                    if let Ok(text) = e.unescape() {
                        entry.text.push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {} // Skip PI, Comment, Decl, DocType, CData
            Err(e) => return Err(format!("invalid XML: {}", e)),
        }
    }

    Ok(serde_json::Value::Object(root))
}

fn insert_or_array(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: String,
    value: serde_json::Value,
) {
    if let Some(existing) = map.get_mut(&key) {
        match existing {
            serde_json::Value::Array(arr) => arr.push(value),
            _ => {
                let prev = existing.clone();
                *existing = serde_json::Value::Array(vec![prev, value]);
            }
        }
    } else {
        map.insert(key, value);
    }
}

// --- XML output ---

fn json_to_xml(val: &serde_json::Value) -> Result<String, String> {
    use quick_xml::Writer;

    let mut writer = Writer::new(Vec::new());

    match val {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                write_xml_element(&mut writer, key, value)
                    .map_err(|e| format!("XML write error: {}", e))?;
            }
        }
        _ => {
            write_xml_element(&mut writer, "root", val)
                .map_err(|e| format!("XML write error: {}", e))?;
        }
    }

    let bytes = writer.into_inner();
    String::from_utf8(bytes).map_err(|e| format!("XML encoding error: {}", e))
}

fn write_xml_element<W: io::Write>(
    writer: &mut quick_xml::Writer<W>,
    name: &str,
    val: &serde_json::Value,
) -> Result<(), quick_xml::Error> {
    use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};

    match val {
        serde_json::Value::Object(map) => {
            let mut elem = BytesStart::new(name);
            for (k, v) in map {
                if let Some(attr_name) = k.strip_prefix('@') {
                    if let serde_json::Value::String(s) = v {
                        elem.push_attribute((attr_name, s.as_str()));
                    }
                }
            }
            writer.write_event(Event::Start(elem))?;

            if let Some(serde_json::Value::String(text)) = map.get("#text") {
                writer.write_event(Event::Text(BytesText::new(text)))?;
            }

            for (k, v) in map {
                if k.starts_with('@') || k == "#text" {
                    continue;
                }
                match v {
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            write_xml_element(writer, k, item)?;
                        }
                    }
                    _ => write_xml_element(writer, k, v)?,
                }
            }

            writer.write_event(Event::End(BytesEnd::new(name)))?;
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                write_xml_element(writer, name, item)?;
            }
        }
        serde_json::Value::String(s) => {
            writer.write_event(Event::Start(BytesStart::new(name)))?;
            writer.write_event(Event::Text(BytesText::new(s)))?;
            writer.write_event(Event::End(BytesEnd::new(name)))?;
        }
        serde_json::Value::Number(n) => {
            let s = n.to_string();
            writer.write_event(Event::Start(BytesStart::new(name)))?;
            writer.write_event(Event::Text(BytesText::new(&s)))?;
            writer.write_event(Event::End(BytesEnd::new(name)))?;
        }
        serde_json::Value::Bool(b) => {
            let s = if *b { "true" } else { "false" };
            writer.write_event(Event::Start(BytesStart::new(name)))?;
            writer.write_event(Event::Text(BytesText::new(s)))?;
            writer.write_event(Event::End(BytesEnd::new(name)))?;
        }
        serde_json::Value::Null => {
            writer.write_event(Event::Empty(BytesStart::new(name)))?;
        }
    }
    Ok(())
}

// --- Output formatting ---

fn format_val_output(val: &Val, opts: &YqOptions, out_format: Format) -> Result<String, String> {
    let compact_str = format!("{}", val);

    // Raw output: unquote strings
    if opts.raw_output {
        if compact_str.starts_with('"') && compact_str.ends_with('"') && compact_str.len() >= 2 {
            if let Ok(unescaped) = serde_json::from_str::<String>(&compact_str) {
                return Ok(unescaped);
            }
        }
    }

    let json_val: serde_json::Value =
        serde_json::from_str(&compact_str).unwrap_or(serde_json::Value::String(compact_str));

    format_json_as(out_format, &json_val, opts.compact)
}

fn format_json_as(
    format: Format,
    val: &serde_json::Value,
    compact: bool,
) -> Result<String, String> {
    match format {
        Format::Json => {
            if compact {
                serde_json::to_string(val).map_err(|e| format!("JSON output error: {}", e))
            } else {
                serde_json::to_string_pretty(val).map_err(|e| format!("JSON output error: {}", e))
            }
        }
        Format::Yaml => {
            let s = serde_yaml::to_string(val).map_err(|e| format!("YAML output error: {}", e))?;
            // Strip leading "---\n" and trailing newline for cleaner output
            let s = s.strip_prefix("---\n").unwrap_or(&s);
            let s = s.strip_suffix('\n').unwrap_or(s);
            Ok(s.to_string())
        }
        Format::Toml => {
            let toml_val = json_to_toml(val)?;
            let s = toml::to_string_pretty(&toml_val)
                .map_err(|e| format!("TOML output error: {}", e))?;
            let s = s.strip_suffix('\n').unwrap_or(&s);
            Ok(s.to_string())
        }
        Format::Xml => json_to_xml(val),
    }
}

// --- Main logic ---

fn run_yq(args: &[String]) -> Result<i32, String> {
    let opts = parse_args(args)?;

    // Read input
    let mut stdin_data = String::new();
    if !opts.null_input {
        io::stdin()
            .read_to_string(&mut stdin_data)
            .map_err(|e| format!("failed to read stdin: {}", e))?;
    }

    // Determine input format
    let in_format = opts.input_format.unwrap_or_else(|| {
        if opts.null_input {
            Format::Yaml
        } else {
            detect_format(&stdin_data)
        }
    });

    // Default output format: YAML for YAML input, otherwise matches input
    let out_format = opts.output_format.unwrap_or(in_format);

    // Parse input to JSON, then convert to jaq Val
    let inputs = if opts.null_input {
        vec![Val::from(serde_json::Value::Null)]
    } else {
        let json_val = parse_input(&stdin_data, in_format)?;
        if opts.slurp {
            match json_val {
                serde_json::Value::Array(_) => vec![Val::from(json_val)],
                _ => vec![Val::from(serde_json::Value::Array(vec![json_val]))],
            }
        } else {
            vec![Val::from(json_val)]
        }
    };

    // Compile jaq filter (same engine as jq)
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let arena = Arena::default();
    let program = File {
        code: opts.filter.as_str(),
        path: (),
    };
    let modules = loader
        .load(&arena, program)
        .map_err(|errs| format!("parse error: {:?}", errs))?;

    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errs| format!("compile error: {:?}", errs))?;

    // Execute filter and output
    let empty_inputs = RcIter::new(core::iter::empty());
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for input in inputs {
        let ctx = Ctx::new(core::iter::empty(), &empty_inputs);
        let results = filter.run((ctx, input));

        for result in results {
            match result {
                Ok(val) => {
                    let s = format_val_output(&val, &opts, out_format)?;
                    writeln!(out, "{}", s).ok();
                }
                Err(e) => {
                    eprintln!("yq: error: {}", e);
                    return Ok(5);
                }
            }
        }
    }

    Ok(0)
}
