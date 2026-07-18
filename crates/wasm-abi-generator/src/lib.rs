//! Generated raw-WASM proof fixtures for the checked-in AgentOS host ABI.
//!
//! The fixture builder consumes the same manifest as linker generation and
//! import auditing. Tests therefore cannot silently keep compiling an old,
//! hand-written signature after the owned ABI changes.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

const PERMISSION_TIERS: [(&str, u8); 4] = [
    ("isolated", 1 << 0),
    ("read-only", 1 << 1),
    ("read-write", 1 << 2),
    ("full", 1 << 3),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreSignature {
    pub id: String,
    pub params: Vec<String>,
    pub results: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbiBindingSemantics {
    pub handler: String,
    pub decode: String,
    pub encode: String,
    pub return_kind: String,
    pub execution_class: String,
    pub restartability: String,
    pub transactional: bool,
    pub prevalidate_outputs: bool,
    pub permission_tiers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbiBindingMetadata {
    pub id: String,
    pub status: String,
    pub core_signature: String,
    #[serde(flatten)]
    pub semantics: AbiBindingSemantics,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AbiImport {
    pub module: String,
    pub name: String,
    pub params: Vec<String>,
    pub results: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbiManifest {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub abi_version: String,
    pub module_aliases: BTreeMap<String, String>,
    pub module_policy: BTreeMap<String, Vec<String>>,
    pub import_policy_overrides: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub core_signatures: Vec<CoreSignature>,
    #[serde(default)]
    pub bindings: BTreeMap<String, AbiBindingMetadata>,
    pub imports: Vec<AbiImport>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ManifestCounts {
    pub imports: usize,
    pub signatures: usize,
    pub alias_bindings: usize,
    pub isolated_bindings: usize,
    pub read_only_bindings: usize,
    pub read_write_bindings: usize,
    pub full_bindings: usize,
    pub isolated_bindings_with_aliases: usize,
    pub read_only_bindings_with_aliases: usize,
    pub read_write_bindings_with_aliases: usize,
    pub full_bindings_with_aliases: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallArguments {
    /// Valid zero values, useful for terminal imports and narrow smoke cases.
    Zero,
    /// Invalid fds/scalars and OOB pointers. This reaches every host function
    /// without granting it a valid resource on which to perform a side effect.
    Hostile,
}

#[derive(Clone, Debug)]
pub struct RawCallAssertion {
    pub module: String,
    pub name: String,
    /// WAT constant expressions in parameter order.
    pub arguments: Vec<String>,
    pub expected_i32: i32,
}

impl RawCallAssertion {
    pub fn i32(
        module: impl Into<String>,
        name: impl Into<String>,
        arguments: impl IntoIterator<Item = impl Into<String>>,
        expected_i32: i32,
    ) -> Self {
        Self {
            module: module.into(),
            name: name.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
            expected_i32,
        }
    }
}

impl AbiManifest {
    pub fn parse(json: &str) -> Self {
        serde_json::from_str(json).expect("parse AgentOS WASM ABI manifest")
    }

    pub fn imports_with_aliases(&self) -> Vec<AbiImport> {
        let mut imports = self.imports.clone();
        for (alias, canonical) in &self.module_aliases {
            imports.extend(
                self.imports
                    .iter()
                    .filter(|import| import.module == *canonical)
                    .cloned()
                    .map(|mut import| {
                        import.module = alias.clone();
                        import
                    }),
            );
        }
        imports
            .sort_by(|left, right| (&left.module, &left.name).cmp(&(&right.module, &right.name)));
        imports
    }

    pub fn permits(&self, import: &AbiImport, tier: &str) -> bool {
        let key = format!("{}.{}", import.module, import.name);
        self.import_policy_overrides
            .get(&key)
            .or_else(|| self.module_policy.get(&import.module))
            .is_some_and(|tiers| tiers.iter().any(|candidate| candidate == tier))
    }

    pub fn permitted_imports(&self, tier: &str) -> Vec<AbiImport> {
        self.imports_with_aliases()
            .into_iter()
            .filter(|import| self.permits(import, tier))
            .collect()
    }

    pub fn validate_registry(&self) -> Result<ManifestCounts, String> {
        if self.schema_version != 2 {
            return Err(format!(
                "unsupported AgentOS WASM ABI schema version {}; expected 2",
                self.schema_version
            ));
        }
        if self.abi_version.is_empty() {
            return Err(String::from("ABI version must not be empty"));
        }

        let mut import_keys = BTreeSet::new();
        let mut import_ids = BTreeSet::new();
        let mut signature_ids = BTreeSet::new();
        let mut signature_shapes = BTreeSet::new();
        let mut referenced_signature_ids = BTreeSet::new();
        for signature in &self.core_signatures {
            validate_rust_identifier("core signature", &signature.id)?;
            if !signature_ids.insert(signature.id.as_str()) {
                return Err(format!("duplicate core signature id {}", signature.id));
            }
            validate_core_values(&signature.params)?;
            validate_core_values(&signature.results)?;
            let shape = signature_shape(&signature.params, &signature.results);
            if !signature_shapes.insert(shape) {
                return Err(format!(
                    "duplicate core signature shape for {}",
                    signature.id
                ));
            }
        }

        let signatures_by_id = self
            .core_signatures
            .iter()
            .map(|signature| (signature.id.as_str(), signature))
            .collect::<BTreeMap<_, _>>();
        let mut canonical_tier_counts = BTreeMap::from([
            ("isolated", 0usize),
            ("read-only", 0usize),
            ("read-write", 0usize),
            ("full", 0usize),
        ]);

        for import in &self.imports {
            let key = format!("{}.{}", import.module, import.name);
            if !import_keys.insert(key.clone()) {
                return Err(format!("duplicate ABI import {key}"));
            }
            let metadata = self
                .bindings
                .get(&key)
                .ok_or_else(|| format!("import {key} has no semantic binding"))?;
            validate_rust_identifier("import", &metadata.id)?;
            if !import_ids.insert(metadata.id.as_str()) {
                return Err(format!("duplicate import id {}", metadata.id));
            }
            if !matches!(metadata.status.as_str(), "canonical" | "compatibility") {
                return Err(format!(
                    "import {key} has unsupported status {}",
                    metadata.status
                ));
            }
            validate_core_values(&import.params)?;
            validate_core_values(&import.results)?;
            let signature = signatures_by_id
                .get(metadata.core_signature.as_str())
                .ok_or_else(|| {
                    format!(
                        "import {key} references unknown core signature {}",
                        metadata.core_signature
                    )
                })?;
            referenced_signature_ids.insert(metadata.core_signature.as_str());
            if signature.params != import.params || signature.results != import.results {
                return Err(format!(
                    "import {key} core signature {} does not match its params/results",
                    metadata.core_signature
                ));
            }

            let binding = &metadata.semantics;
            validate_rust_identifier("handler", &binding.handler)?;
            validate_rust_identifier("decoder", &binding.decode)?;
            validate_rust_identifier("encoder", &binding.encode)?;
            validate_enum_value(
                &key,
                "return kind",
                &binding.return_kind,
                &["WasiErrno", "ScalarI32", "ScalarI64", "Void"],
            )?;
            validate_enum_value(
                &key,
                "execution class",
                &binding.execution_class,
                &["Bootstrap", "Host", "Wait", "Local", "Terminal"],
            )?;
            validate_enum_value(
                &key,
                "restartability",
                &binding.restartability,
                &["Never", "SignalRestartable"],
            )?;
            match (binding.return_kind.as_str(), import.results.as_slice()) {
                ("Void", []) => {}
                ("WasiErrno" | "ScalarI32", [result]) if result == "i32" => {}
                ("ScalarI64", [result]) if result == "i64" => {}
                _ => {
                    return Err(format!(
                        "import {key} return kind {} does not match core results {:?}",
                        binding.return_kind, import.results
                    ));
                }
            }

            let effective_tiers = self.effective_tiers(import)?;
            if binding.permission_tiers != effective_tiers {
                return Err(format!(
                    "import {key} permission tiers {:?} do not match policy {:?}",
                    binding.permission_tiers, effective_tiers
                ));
            }
            for tier in effective_tiers {
                *canonical_tier_counts
                    .get_mut(tier.as_str())
                    .expect("validated permission tier") += 1;
            }
        }
        if self.bindings.len() != self.imports.len() {
            let extra = self
                .bindings
                .keys()
                .filter(|key| !import_keys.contains(*key))
                .cloned()
                .collect::<Vec<_>>();
            return Err(format!(
                "semantic binding count {} does not match import count {}; unmapped bindings: {extra:?}",
                self.bindings.len(),
                self.imports.len()
            ));
        }
        if referenced_signature_ids != signature_ids {
            let unused = signature_ids
                .difference(&referenced_signature_ids)
                .copied()
                .collect::<Vec<_>>();
            return Err(format!("unreferenced core signatures: {unused:?}"));
        }

        for (alias, canonical) in &self.module_aliases {
            if alias == canonical {
                return Err(format!("module alias {alias} points to itself"));
            }
            let alias_policy = self
                .module_policy
                .get(alias)
                .ok_or_else(|| format!("alias module {alias} has no permission policy"))?;
            validate_permission_tiers(alias_policy)?;
            if !self
                .imports
                .iter()
                .any(|import| import.module == *canonical)
            {
                return Err(format!(
                    "module alias {alias} references empty or unknown module {canonical}"
                ));
            }
        }

        let imports_with_aliases = self.imports_with_aliases();
        let alias_bindings = imports_with_aliases.len() - self.imports.len();
        let mut aliased_keys = BTreeSet::new();
        let mut all_tier_counts = BTreeMap::from([
            ("isolated", 0usize),
            ("read-only", 0usize),
            ("read-write", 0usize),
            ("full", 0usize),
        ]);
        for import in &imports_with_aliases {
            let key = format!("{}.{}", import.module, import.name);
            if !aliased_keys.insert(key.clone()) {
                return Err(format!("duplicate effective ABI import {key}"));
            }
            for (tier, count) in &mut all_tier_counts {
                if self.permits(import, tier) {
                    *count += 1;
                }
            }
        }

        Ok(ManifestCounts {
            imports: self.imports.len(),
            signatures: self.core_signatures.len(),
            alias_bindings,
            isolated_bindings: canonical_tier_counts["isolated"],
            read_only_bindings: canonical_tier_counts["read-only"],
            read_write_bindings: canonical_tier_counts["read-write"],
            full_bindings: canonical_tier_counts["full"],
            isolated_bindings_with_aliases: all_tier_counts["isolated"],
            read_only_bindings_with_aliases: all_tier_counts["read-only"],
            read_write_bindings_with_aliases: all_tier_counts["read-write"],
            full_bindings_with_aliases: all_tier_counts["full"],
        })
    }

    pub fn render_rust_registry(&self) -> Result<String, String> {
        self.validate_registry()?;

        let handler_ids = semantic_ids(&self.bindings, |binding| &binding.handler);
        let decode_ids = semantic_ids(&self.bindings, |binding| &binding.decode);
        let encode_ids = semantic_ids(&self.bindings, |binding| &binding.encode);
        let mut output = String::new();
        writeln!(
            output,
            "// @generated by scripts/generate-wasm-abi-manifest.mjs; do not edit."
        )
        .unwrap();
        writeln!(
            output,
            "// Source: crates/execution/assets/agentos-wasm-abi.json (schema {}).\n",
            self.schema_version
        )
        .unwrap();
        output.push_str("#![allow(clippy::too_many_lines)]\n\n");
        output.push_str(
            "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
             pub enum CoreValueType {\n    I32,\n    I64,\n}\n\n",
        );
        render_enum(
            &mut output,
            "CoreSignatureId",
            self.core_signatures
                .iter()
                .map(|signature| signature.id.as_str()),
            true,
        );
        render_enum(
            &mut output,
            "ImportId",
            self.imports.iter().map(|import| {
                self.binding_metadata(import)
                    .expect("validated binding")
                    .id
                    .as_str()
            }),
            true,
        );
        render_enum(
            &mut output,
            "HandlerId",
            handler_ids.iter().map(String::as_str),
            false,
        );
        render_enum(
            &mut output,
            "DecodeId",
            decode_ids.iter().map(String::as_str),
            false,
        );
        render_enum(
            &mut output,
            "EncodeId",
            encode_ids.iter().map(String::as_str),
            false,
        );
        output.push_str(
            "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
             pub enum ImportStatus {\n    Canonical,\n    Compatibility,\n}\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
             pub enum ReturnKind {\n    WasiErrno,\n    ScalarI32,\n    ScalarI64,\n    Void,\n}\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
             pub enum ExecutionClass {\n    Bootstrap,\n    Host,\n    Wait,\n    Local,\n    Terminal,\n}\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
             pub enum Restartability {\n    Never,\n    SignalRestartable,\n}\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
             pub enum PermissionTier {\n    Isolated,\n    ReadOnly,\n    ReadWrite,\n    Full,\n}\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n\
             pub struct PermissionTiers(u8);\n\n\
             impl PermissionTiers {\n\
                 pub const fn from_bits(bits: u8) -> Self {\n        Self(bits)\n    }\n\n\
                 pub const fn bits(self) -> u8 {\n        self.0\n    }\n\n\
                 pub const fn contains(self, tier: PermissionTier) -> bool {\n\
                     let bit = match tier {\n\
                         PermissionTier::Isolated => 1 << 0,\n\
                         PermissionTier::ReadOnly => 1 << 1,\n\
                         PermissionTier::ReadWrite => 1 << 2,\n\
                         PermissionTier::Full => 1 << 3,\n\
                     };\n\
                     self.0 & bit != 0\n\
                 }\n\
             }\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq)]\n\
             pub struct CoreSignature {\n\
                 pub id: CoreSignatureId,\n\
                 pub params: &'static [CoreValueType],\n\
                 pub results: &'static [CoreValueType],\n\
             }\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq)]\n\
             pub struct AbiBinding {\n\
                 pub id: ImportId,\n\
                 pub module: &'static str,\n\
                 pub name: &'static str,\n\
                 pub signature: CoreSignatureId,\n\
                 pub status: ImportStatus,\n\
                 pub handler: HandlerId,\n\
                 pub decode: DecodeId,\n\
                 pub encode: EncodeId,\n\
                 pub return_kind: ReturnKind,\n\
                 pub execution_class: ExecutionClass,\n\
                 pub restartability: Restartability,\n\
                 /// Submit the semantic action as one shared host operation; do not decompose it into adapter-side check/mutate steps.\n\
                 pub transactional: bool,\n\
                 /// Validate every guest output range before submitting the host operation.\n\
                 pub prevalidate_outputs: bool,\n\
                 pub permission_tiers: PermissionTiers,\n\
             }\n\n\
             #[derive(Clone, Copy, Debug, PartialEq, Eq)]\n\
             pub struct AliasBinding {\n\
                 pub alias_module: &'static str,\n\
                 pub canonical_module: &'static str,\n\
                 pub import: ImportId,\n\
                 pub permission_tiers: PermissionTiers,\n\
             }\n\n",
        );

        writeln!(
            output,
            "pub const ABI_SCHEMA_VERSION: u32 = {};",
            self.schema_version
        )
        .unwrap();
        writeln!(
            output,
            "pub const ABI_VERSION: &str = {:?};\n",
            self.abi_version
        )
        .unwrap();

        output.push_str("pub const CORE_SIGNATURES: &[CoreSignature] = &[\n");
        for signature in &self.core_signatures {
            writeln!(output, "    CoreSignature {{").unwrap();
            writeln!(output, "        id: CoreSignatureId::{},", signature.id).unwrap();
            render_value_type_slice(&mut output, "params", &signature.params);
            render_value_type_slice(&mut output, "results", &signature.results);
            writeln!(output, "    }},").unwrap();
        }
        output.push_str("];\n\n");

        output.push_str("pub const ABI_BINDINGS: &[AbiBinding] = &[\n");
        for import in &self.imports {
            let metadata = self.binding_metadata(import).expect("validated binding");
            let binding = &metadata.semantics;
            let status = if metadata.status == "canonical" {
                "Canonical"
            } else {
                "Compatibility"
            };
            writeln!(output, "    AbiBinding {{").unwrap();
            writeln!(output, "        id: ImportId::{},", metadata.id).unwrap();
            writeln!(output, "        module: {:?},", import.module).unwrap();
            writeln!(output, "        name: {:?},", import.name).unwrap();
            writeln!(
                output,
                "        signature: CoreSignatureId::{},",
                metadata.core_signature
            )
            .unwrap();
            writeln!(output, "        status: ImportStatus::{status},").unwrap();
            writeln!(output, "        handler: HandlerId::{},", binding.handler).unwrap();
            writeln!(output, "        decode: DecodeId::{},", binding.decode).unwrap();
            writeln!(output, "        encode: EncodeId::{},", binding.encode).unwrap();
            writeln!(
                output,
                "        return_kind: ReturnKind::{},",
                binding.return_kind
            )
            .unwrap();
            writeln!(
                output,
                "        execution_class: ExecutionClass::{},",
                binding.execution_class
            )
            .unwrap();
            writeln!(
                output,
                "        restartability: Restartability::{},",
                binding.restartability
            )
            .unwrap();
            writeln!(output, "        transactional: {},", binding.transactional).unwrap();
            writeln!(
                output,
                "        prevalidate_outputs: {},",
                binding.prevalidate_outputs
            )
            .unwrap();
            writeln!(
                output,
                "        permission_tiers: PermissionTiers::from_bits({}),",
                permission_bits(&binding.permission_tiers)?
            )
            .unwrap();
            writeln!(output, "    }},").unwrap();
        }
        output.push_str("];\n\n");

        output.push_str("pub const ALIAS_BINDINGS: &[AliasBinding] = &[\n");
        for (alias, canonical) in &self.module_aliases {
            let alias_tiers = self
                .module_policy
                .get(alias)
                .ok_or_else(|| format!("alias module {alias} has no permission policy"))?;
            for import in self
                .imports
                .iter()
                .filter(|import| import.module == *canonical)
            {
                writeln!(output, "    AliasBinding {{").unwrap();
                writeln!(output, "        alias_module: {alias:?},").unwrap();
                writeln!(output, "        canonical_module: {canonical:?},").unwrap();
                let metadata = self.binding_metadata(import).expect("validated binding");
                writeln!(output, "        import: ImportId::{},", metadata.id).unwrap();
                writeln!(
                    output,
                    "        permission_tiers: PermissionTiers::from_bits({}),",
                    permission_bits(alias_tiers)?
                )
                .unwrap();
                writeln!(output, "    }},").unwrap();
            }
        }
        output.push_str("];\n\n");

        output.push_str(
            "pub fn binding(id: ImportId) -> &'static AbiBinding {\n\
                 &ABI_BINDINGS[id as usize]\n\
             }\n\n\
             pub fn core_signature(id: CoreSignatureId) -> &'static CoreSignature {\n\
                 &CORE_SIGNATURES[id as usize]\n\
             }\n\n\
             pub fn find_binding(module: &str, name: &str) -> Option<&'static AbiBinding> {\n\
                 if let Some(binding) = ABI_BINDINGS\n\
                     .iter()\n\
                     .find(|binding| binding.module == module && binding.name == name)\n\
                 {\n\
                     return Some(binding);\n\
                 }\n\
                 let alias = ALIAS_BINDINGS\n\
                     .iter()\n\
                     .find(|alias| alias.alias_module == module && binding(alias.import).name == name)?;\n\
                 Some(binding(alias.import))\n\
             }\n",
        );
        Ok(output)
    }

    fn effective_tiers(&self, import: &AbiImport) -> Result<Vec<String>, String> {
        let key = format!("{}.{}", import.module, import.name);
        let tiers = self
            .import_policy_overrides
            .get(&key)
            .or_else(|| self.module_policy.get(&import.module))
            .ok_or_else(|| format!("import {key} has no permission policy"))?;
        validate_permission_tiers(tiers)?;
        Ok(tiers.clone())
    }

    fn binding_metadata(&self, import: &AbiImport) -> Result<&AbiBindingMetadata, String> {
        let key = format!("{}.{}", import.module, import.name);
        self.bindings
            .get(&key)
            .ok_or_else(|| format!("import {key} has no semantic binding"))
    }
}

fn validate_core_values(values: &[String]) -> Result<(), String> {
    for value in values {
        if !matches!(value.as_str(), "i32" | "i64") {
            return Err(format!("unsupported core ABI value type {value}"));
        }
    }
    Ok(())
}

fn validate_enum_value(
    import: &str,
    field: &str,
    value: &str,
    allowed: &[&str],
) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("import {import} has unsupported {field} {value}"))
    }
}

fn validate_rust_identifier(kind: &str, value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|character| character.is_ascii_uppercase())
        || !chars.all(|character| character.is_ascii_alphanumeric())
    {
        return Err(format!(
            "{kind} id {value:?} is not a PascalCase Rust identifier"
        ));
    }
    Ok(())
}

fn validate_permission_tiers(tiers: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for tier in tiers {
        if !PERMISSION_TIERS
            .iter()
            .any(|(candidate, _)| tier == candidate)
        {
            return Err(format!("unknown ABI permission tier {tier}"));
        }
        if !seen.insert(tier) {
            return Err(format!("duplicate ABI permission tier {tier}"));
        }
    }
    if tiers.is_empty() {
        return Err(String::from("ABI permission tier list must not be empty"));
    }
    Ok(())
}

fn permission_bits(tiers: &[String]) -> Result<u8, String> {
    validate_permission_tiers(tiers)?;
    Ok(PERMISSION_TIERS
        .iter()
        .filter(|(tier, _)| tiers.iter().any(|candidate| candidate == tier))
        .fold(0, |bits, (_, bit)| bits | bit))
}

fn signature_shape(params: &[String], results: &[String]) -> String {
    format!("{}->{}", params.join(","), results.join(","))
}

fn semantic_ids(
    bindings: &BTreeMap<String, AbiBindingMetadata>,
    id: impl Fn(&AbiBindingSemantics) -> &String,
) -> Vec<String> {
    let mut values = BTreeSet::new();
    for binding in bindings.values() {
        values.insert(id(&binding.semantics).clone());
    }
    values.into_iter().collect()
}

fn render_enum<'a>(
    output: &mut String,
    name: &str,
    variants: impl IntoIterator<Item = &'a str>,
    repr: bool,
) {
    output.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    if repr {
        output.push_str("#[repr(u16)]\n");
    }
    writeln!(output, "pub enum {name} {{").unwrap();
    for variant in variants {
        writeln!(output, "    {variant},").unwrap();
    }
    output.push_str("}\n\n");
}

fn render_value_type_slice(output: &mut String, field: &str, values: &[String]) {
    if values.is_empty() {
        writeln!(output, "        {field}: &[],").unwrap();
        return;
    }
    writeln!(output, "        {field}: &[").unwrap();
    for value in values {
        let value = match value.as_str() {
            "i32" => "I32",
            "i64" => "I64",
            _ => unreachable!("validated core value type"),
        };
        writeln!(output, "            CoreValueType::{value},").unwrap();
    }
    writeln!(output, "        ],").unwrap();
}

fn typed_argument(value_type: &str, arguments: CallArguments) -> &'static str {
    match (value_type, arguments) {
        ("i32", CallArguments::Zero) => "(i32.const 0)",
        ("i64", CallArguments::Zero) => "(i64.const 0)",
        ("i32", CallArguments::Hostile) => "(i32.const -1)",
        ("i64", CallArguments::Hostile) => "(i64.const -1)",
        (other, _) => panic!("unsupported ABI value type {other}"),
    }
}

fn import_arguments(import: &AbiImport, arguments: CallArguments) -> String {
    // These compatibility setters are the only one-scalar calls where -1 is
    // a valid, potentially long-lived request rather than an invalid fd/id.
    let arguments = if matches!(
        (import.module.as_str(), import.name.as_str()),
        ("host_fs", "set_open_mode" | "set_open_direct")
            | ("host_process", "sleep_ms")
            | ("host_tty", "set_raw_mode")
            | ("wasi_snapshot_preview1" | "wasi_unstable", "proc_exit")
    ) {
        CallArguments::Zero
    } else {
        arguments
    };
    import
        .params
        .iter()
        .map(|param| typed_argument(param, arguments))
        .collect::<Vec<_>>()
        .join(" ")
}

fn declare_import(wat: &mut String, import: &AbiImport, local_name: &str) {
    let params = import.params.join(" ");
    let results = import.results.join(" ");
    wat.push_str(&format!(
        "  (import \"{}\" \"{}\" (func ${local_name}",
        import.module, import.name
    ));
    if !params.is_empty() {
        wat.push_str(&format!(" (param {params})"));
    }
    if !results.is_empty() {
        wat.push_str(&format!(" (result {results})"));
    }
    wat.push_str("))\n");
}

/// Build one module which declares every import and optionally invokes all
/// non-terminal imports once. `proc_exit` is omitted from the combined caller
/// so it cannot hide later calls; use [`single_import_module`] for its proof.
pub fn imports_module(imports: &[AbiImport], invoke: bool, arguments: CallArguments) -> Vec<u8> {
    let mut wat = String::from("(module\n");
    for (index, import) in imports.iter().enumerate() {
        declare_import(&mut wat, import, &format!("abi_{index}"));
    }
    wat.push_str("  (memory (export \"memory\") 1)\n");
    wat.push_str("  (func (export \"_start\")\n");
    if invoke {
        for (index, import) in imports.iter().enumerate() {
            if matches!(
                (import.module.as_str(), import.name.as_str()),
                ("wasi_snapshot_preview1" | "wasi_unstable", "proc_exit")
            ) {
                continue;
            }
            let args = import_arguments(import, arguments);
            if import.results.is_empty() {
                wat.push_str(&format!("    (call $abi_{index} {args})\n"));
            } else {
                wat.push_str(&format!("    (drop (call $abi_{index} {args}))\n"));
            }
        }
    }
    wat.push_str("  )\n)\n");
    wat::parse_str(&wat)
        .unwrap_or_else(|error| panic!("compile generated ABI caller: {error}\n{wat}"))
}

pub fn single_import_module(import: &AbiImport, invoke: bool, arguments: CallArguments) -> Vec<u8> {
    let mut wat = String::from("(module\n");
    declare_import(&mut wat, import, "target");
    wat.push_str("  (memory (export \"memory\") 1)\n  (func (export \"_start\")\n");
    if invoke {
        let args = import_arguments(import, arguments);
        if import.results.is_empty() {
            wat.push_str(&format!("    (call $target {args})\n"));
        } else {
            wat.push_str(&format!("    (drop (call $target {args}))\n"));
        }
    }
    wat.push_str("  )\n)\n");
    wat::parse_str(&wat).expect("compile generated single-import ABI fixture")
}

/// Build a direct-WAT hostile-memory fixture from named manifest imports.
/// Each assertion is type-checked against the manifest signature and traps if
/// the import does not return the expected stable errno.
pub fn raw_call_assertion_module(
    manifest: &AbiManifest,
    assertions: &[RawCallAssertion],
    setup_wat: &str,
    postconditions_wat: &str,
) -> Vec<u8> {
    let imports = manifest.imports_with_aliases();
    let resolved = assertions
        .iter()
        .map(|assertion| {
            let import = imports
                .iter()
                .find(|import| import.module == assertion.module && import.name == assertion.name)
                .unwrap_or_else(|| {
                    panic!(
                        "raw assertion references undeclared import {}.{}",
                        assertion.module, assertion.name
                    )
                });
            assert_eq!(
                import.params.len(),
                assertion.arguments.len(),
                "raw assertion argument count for {}.{}",
                assertion.module,
                assertion.name
            );
            assert_eq!(
                import.results.as_slice(),
                ["i32"],
                "raw assertion requires one i32 result for {}.{}",
                assertion.module,
                assertion.name
            );
            import
        })
        .collect::<Vec<_>>();

    let mut wat = String::from("(module\n");
    for (index, import) in resolved.iter().enumerate() {
        declare_import(&mut wat, import, &format!("assert_{index}"));
    }
    let failure_exit = imports
        .iter()
        .find(|import| import.module == "wasi_snapshot_preview1" && import.name == "proc_exit")
        .expect("raw assertion fixture requires Preview1 proc_exit");
    declare_import(&mut wat, failure_exit, "assert_fail");
    wat.push_str("  (memory (export \"memory\") 1)\n  (func (export \"_start\")\n");
    wat.push_str(setup_wat);
    for (index, assertion) in assertions.iter().enumerate() {
        let arguments = assertion.arguments.join(" ");
        wat.push_str(&format!(
            "    (if (i32.ne (call $assert_{index} {arguments}) (i32.const {})) (then (call $assert_fail (i32.const {})) unreachable))\n",
            assertion.expected_i32,
            index + 1,
        ));
    }
    wat.push_str(postconditions_wat);
    wat.push_str("  )\n)\n");
    wat::parse_str(&wat).unwrap_or_else(|error| {
        panic!("compile generated raw-call assertion module: {error}\n{wat}")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        imports_module, raw_call_assertion_module, AbiManifest, CallArguments, RawCallAssertion,
    };

    const CHECKED_MANIFEST: &str = include_str!("../../execution/assets/agentos-wasm-abi.json");

    #[test]
    fn generated_fixture_calls_each_declared_signature() {
        let manifest = AbiManifest::parse(
            r#"{
              "moduleAliases": {"legacy": "canonical"},
              "modulePolicy": {
                "canonical": ["full"],
                "legacy": ["full"],
                "wasi_snapshot_preview1": ["full"]
              },
              "importPolicyOverrides": {},
              "imports": [
                {
                  "module": "canonical",
                  "name": "sample",
                  "params": ["i32", "i64"],
                  "results": ["i32"],
                  "status": "canonical"
                },
                {
                  "module": "wasi_snapshot_preview1",
                  "name": "proc_exit",
                  "params": ["i32"],
                  "results": [],
                  "status": "canonical"
                }
              ]
            }"#,
        );
        let fixture = imports_module(
            &manifest.permitted_imports("full"),
            true,
            CallArguments::Hostile,
        );
        assert!(fixture.starts_with(b"\0asm"));

        let assertion = raw_call_assertion_module(
            &manifest,
            &[RawCallAssertion::i32(
                "canonical",
                "sample",
                ["(i32.const 0)", "(i64.const 0)"],
                0,
            )],
            "",
            "",
        );
        assert!(assertion.starts_with(b"\0asm"));
    }

    #[test]
    fn checked_manifest_has_complete_unique_semantic_bindings() {
        let manifest = AbiManifest::parse(CHECKED_MANIFEST);
        let counts = manifest.validate_registry().expect("valid ABI registry");
        assert_eq!(counts.imports, 169);
        assert_eq!(counts.signatures, 29);
        assert_eq!(counts.alias_bindings, 40);
        assert_eq!(counts.isolated_bindings, 112);
        assert_eq!(counts.read_only_bindings, 121);
        assert_eq!(counts.read_write_bindings, 121);
        assert_eq!(counts.full_bindings, 169);
        assert_eq!(counts.isolated_bindings_with_aliases, 152);
        assert_eq!(counts.read_only_bindings_with_aliases, 161);
        assert_eq!(counts.read_write_bindings_with_aliases, 161);
        assert_eq!(counts.full_bindings_with_aliases, 209);
    }

    #[test]
    fn checked_manifest_renders_every_binding_and_alias() {
        let manifest = AbiManifest::parse(CHECKED_MANIFEST);
        let registry = manifest
            .render_rust_registry()
            .expect("render checked ABI registry");
        assert_eq!(registry.matches("    AbiBinding {").count(), 169);
        assert_eq!(registry.matches("    AliasBinding {").count(), 40);
        assert!(registry.contains("pub enum ImportId"));
        assert!(registry.contains("pub enum HandlerId"));
        assert!(registry.contains("pub enum DecodeId"));
        assert!(registry.contains("pub enum EncodeId"));
    }

    #[test]
    fn registry_validation_rejects_duplicate_and_unmapped_imports() {
        let mut unmapped = AbiManifest::parse(CHECKED_MANIFEST);
        unmapped.bindings.remove("host_fs.chmod");
        assert!(unmapped
            .validate_registry()
            .expect_err("missing binding must fail")
            .contains("has no semantic binding"));

        let mut duplicate = AbiManifest::parse(CHECKED_MANIFEST);
        duplicate.imports.push(duplicate.imports[0].clone());
        assert!(duplicate
            .validate_registry()
            .expect_err("duplicate import must fail")
            .contains("duplicate ABI import"));
    }

    #[test]
    fn aliases_and_versions_reuse_canonical_semantic_ids() {
        let manifest = AbiManifest::parse(CHECKED_MANIFEST);
        let find = |module: &str, name: &str| {
            manifest
                .bindings
                .get(&format!("{module}.{name}"))
                .unwrap_or_else(|| panic!("missing {module}.{name}"))
                .semantics
                .clone()
        };
        assert_eq!(
            find("host_process", "proc_spawn").handler,
            find("host_process", "proc_spawn_v4").handler
        );
        assert_ne!(
            find("host_process", "proc_spawn").decode,
            find("host_process", "proc_spawn_v4").decode
        );
        assert_eq!(
            find("host_fs", "fd_chown").decode,
            find("host_fs", "fchown").decode
        );
        assert_eq!(
            find("host_net", "net_close").handler,
            find("wasi_snapshot_preview1", "fd_close").handler
        );
    }
}
