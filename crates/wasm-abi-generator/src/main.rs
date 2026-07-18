use agentos_wasm_abi_generator::AbiManifest;
use serde::Serialize;
use std::collections::BTreeMap;
use std::env;
use std::io::Read as _;
use std::path::PathBuf;
use witx::{Id, Layout, Type, WasmType};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoweredPreview1 {
    module: String,
    imports: Vec<LoweredImport>,
    layouts: BTreeMap<String, MemoryLayout>,
}

#[derive(Serialize)]
struct LoweredImport {
    name: String,
    params: Vec<&'static str>,
    results: Vec<&'static str>,
}

#[derive(Serialize)]
struct MemoryLayout {
    size: usize,
    align: usize,
    fields: BTreeMap<String, usize>,
}

fn wasm_type(value: WasmType) -> &'static str {
    match value {
        WasmType::I32 => "i32",
        WasmType::I64 => "i64",
        WasmType::F32 => "f32",
        WasmType::F64 => "f64",
    }
}

fn record_layout(document: &witx::Document, name: &str) -> Result<MemoryLayout, String> {
    let named = document
        .typename(&Id::new(name))
        .ok_or_else(|| format!("pinned WITX has no type {name}"))?;
    let size_align = named.mem_size_align();
    let Type::Record(record) = named.type_().as_ref() else {
        return Err(format!("pinned WITX type {name} is not a record"));
    };
    let fields = record
        .member_layout()
        .into_iter()
        .map(|member| (member.member.name.as_str().to_owned(), member.offset))
        .collect();
    Ok(MemoryLayout {
        size: size_align.size,
        align: size_align.align,
        fields,
    })
}

fn prestat_layout(document: &witx::Document) -> Result<MemoryLayout, String> {
    let named = document
        .typename(&Id::new("prestat"))
        .ok_or_else(|| String::from("pinned WITX has no type prestat"))?;
    let size_align = named.mem_size_align();
    let Type::Variant(variant) = named.type_().as_ref() else {
        return Err(String::from("pinned WITX prestat is not a variant"));
    };
    let payload_offset = variant.payload_offset();
    let dir = variant
        .cases
        .iter()
        .find(|case| case.name.as_str() == "dir")
        .and_then(|case| case.tref.as_ref())
        .ok_or_else(|| String::from("pinned WITX prestat has no dir payload"))?;
    let Type::Record(dir) = dir.type_().as_ref() else {
        return Err(String::from(
            "pinned WITX prestat dir payload is not a record",
        ));
    };
    let name_len = dir
        .member_layout()
        .into_iter()
        .find(|member| member.member.name.as_str() == "pr_name_len")
        .ok_or_else(|| String::from("pinned WITX prestat dir has no pr_name_len"))?;
    Ok(MemoryLayout {
        size: size_align.size,
        align: size_align.align,
        fields: BTreeMap::from([
            (String::from("tag"), 0),
            (String::from("name_len"), payload_offset + name_len.offset),
        ]),
    })
}

fn subscription_layout(document: &witx::Document) -> Result<MemoryLayout, String> {
    let named = document
        .typename(&Id::new("subscription"))
        .ok_or_else(|| String::from("pinned WITX has no type subscription"))?;
    let size_align = named.mem_size_align();
    let Type::Record(record) = named.type_().as_ref() else {
        return Err(String::from("pinned WITX subscription is not a record"));
    };
    let members = record.member_layout();
    let userdata = members
        .iter()
        .find(|member| member.member.name.as_str() == "userdata")
        .ok_or_else(|| String::from("pinned WITX subscription has no userdata"))?;
    let union = members
        .iter()
        .find(|member| member.member.name.as_str() == "u")
        .ok_or_else(|| String::from("pinned WITX subscription has no union"))?;
    let Type::Variant(variant) = union.member.tref.type_().as_ref() else {
        return Err(String::from(
            "pinned WITX subscription union is not a variant",
        ));
    };
    Ok(MemoryLayout {
        size: size_align.size,
        align: size_align.align,
        fields: BTreeMap::from([
            (String::from("userdata"), userdata.offset),
            (String::from("type"), union.offset),
            (
                String::from("clock_or_fd"),
                union.offset + variant.payload_offset(),
            ),
        ]),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = env::args_os().nth(1);
    if argument.as_deref() == Some(std::ffi::OsStr::new("--render-registry")) {
        let mut json = String::new();
        std::io::stdin().read_to_string(&mut json)?;
        let manifest = AbiManifest::parse(&json);
        let registry = manifest
            .render_rust_registry()
            .map_err(|error| format!("invalid AgentOS WASM ABI manifest: {error}"))?;
        print!("{registry}");
        return Ok(());
    }

    let path = argument.map(PathBuf::from).ok_or(
        "usage: agentos-wasm-abi-generator <wasi_snapshot_preview1.witx> | --render-registry",
    )?;
    let document = witx::load(&[path])?;
    let module = document
        .module(&Id::new("wasi_snapshot_preview1"))
        .ok_or("pinned WITX has no wasi_snapshot_preview1 module")?;
    let mut imports = module
        .funcs()
        .map(|function| {
            let (params, results) = function.wasm_signature();
            LoweredImport {
                name: function.name.as_str().to_owned(),
                params: params.into_iter().map(wasm_type).collect(),
                results: results.into_iter().map(wasm_type).collect(),
            }
        })
        .collect::<Vec<_>>();
    imports.sort_by(|left, right| left.name.cmp(&right.name));

    let mut layouts = BTreeMap::new();
    for name in ["ciovec", "iovec", "dirent", "event", "fdstat", "filestat"] {
        layouts.insert(name.to_owned(), record_layout(&document, name)?);
    }
    layouts.insert(String::from("prestat"), prestat_layout(&document)?);
    layouts.insert(
        String::from("subscription"),
        subscription_layout(&document)?,
    );

    serde_json::to_writer_pretty(
        std::io::stdout(),
        &LoweredPreview1 {
            module: module.name.as_str().to_owned(),
            imports,
            layouts,
        },
    )?;
    println!();
    Ok(())
}
