use agentos_execution::javascript::ModuleResolutionTestHarness;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            temp: TempDir::new().expect("create temp dir"),
        }
    }

    fn host_path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.host_path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    fn write_json(&self, relative: &str, value: Value) {
        self.write(
            relative,
            &serde_json::to_string_pretty(&value).expect("serialize json"),
        );
    }

    fn mkdir(&self, relative: &str) {
        fs::create_dir_all(self.host_path(relative)).expect("create fixture dir");
    }

    fn symlink_dir(&self, target_relative: &str, link_relative: &str) {
        let target = self.host_path(target_relative);
        let link = self.host_path(link_relative);
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent).expect("create symlink parent");
        }
        symlink(target, link).expect("create directory symlink");
    }

    fn resolver(&self) -> ModuleResolutionTestHarness {
        ModuleResolutionTestHarness::new(self.temp.path())
    }
}

fn assert_import(fixture: &Fixture, specifier: &str, from_path: &str, expected: &str) {
    let mut resolver = fixture.resolver();
    assert_eq!(
        resolver.resolve_import(specifier, from_path),
        Some(String::from(expected))
    );
}

fn assert_require(fixture: &Fixture, specifier: &str, from_path: &str, expected: &str) {
    let mut resolver = fixture.resolver();
    assert_eq!(
        resolver.resolve_require(specifier, from_path),
        Some(String::from(expected))
    );
}

fn assert_require_missing(fixture: &Fixture, specifier: &str, from_path: &str) {
    let mut resolver = fixture.resolver();
    assert_eq!(resolver.resolve_require(specifier, from_path), None);
}

#[derive(Debug, Deserialize)]
struct SharedModuleResolutionFixture {
    cases: Vec<SharedModuleResolutionCase>,
    formats: Vec<SharedModuleFormatCase>,
}

#[derive(Debug, Deserialize)]
struct SharedModuleResolutionCase {
    name: String,
    files: std::collections::BTreeMap<String, String>,
    resolves: Vec<SharedModuleResolutionExpectation>,
}

#[derive(Debug, Deserialize)]
struct SharedModuleResolutionExpectation {
    specifier: String,
    from: String,
    mode: String,
    expected: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SharedModuleFormatCase {
    name: String,
    files: std::collections::BTreeMap<String, String>,
    path: String,
    expected: Option<String>,
}

#[test]
fn matches_shared_native_browser_conformance_fixture() {
    let fixture: SharedModuleResolutionFixture = serde_json::from_str(include_str!(
        "../../../tests/fixtures/module-resolution-conformance.json"
    ))
    .expect("parse shared module resolution fixture");

    for test_case in fixture.cases {
        let fs_fixture = Fixture::new();
        for (path, contents) in &test_case.files {
            fs_fixture.write(path, contents);
        }

        for resolution in &test_case.resolves {
            let mut resolver = fs_fixture.resolver();
            let actual = match resolution.mode.as_str() {
                "import" => resolver.resolve_import(&resolution.specifier, &resolution.from),
                "require" => resolver.resolve_require(&resolution.specifier, &resolution.from),
                other => panic!("unsupported shared fixture mode {other}"),
            };
            assert_eq!(
                actual, resolution.expected,
                "{}: {} from {} in {} mode",
                test_case.name, resolution.specifier, resolution.from, resolution.mode
            );
        }
    }

    for format_case in fixture.formats {
        let fs_fixture = Fixture::new();
        for (path, contents) in &format_case.files {
            fs_fixture.write(path, contents);
        }

        let mut resolver = fs_fixture.resolver();
        let actual = resolver.module_format(&format_case.path).map(String::from);
        assert_eq!(
            actual, format_case.expected,
            "{}: format for {}",
            format_case.name, format_case.path
        );
    }
}

#[test]
fn builtin_bare_fs_normalizes_to_node_prefix() {
    let fixture = Fixture::new();
    assert_import(&fixture, "fs", "/root/project/index.js", "node:fs");
}

#[test]
fn builtin_node_prefix_is_preserved_for_require() {
    let fixture = Fixture::new();
    assert_require(&fixture, "node:path", "/root/project/index.js", "node:path");
}

#[test]
fn builtin_subpath_normalizes_to_node_prefix() {
    let fixture = Fixture::new();
    assert_import(
        &fixture,
        "fs/promises",
        "/root/project/index.js",
        "node:fs/promises",
    );
}

#[test]
fn relative_import_probes_js_extension() {
    let fixture = Fixture::new();
    fixture.write("project/src/foo.js", "export default 1;");
    assert_import(
        &fixture,
        "./foo",
        "/root/project/src/index.js",
        "/root/project/src/foo.js",
    );
}

#[test]
fn relative_parent_import_probes_json_extension() {
    let fixture = Fixture::new();
    fixture.write("project/data/config.json", r#"{"ok":true}"#);
    assert_import(
        &fixture,
        "../data/config",
        "/root/project/src/index.js",
        "/root/project/data/config.json",
    );
}

#[test]
fn absolute_import_resolves_from_guest_root() {
    let fixture = Fixture::new();
    fixture.write("shared/util.mjs", "export const ok = true;");
    assert_import(
        &fixture,
        "/root/shared/util",
        "/root/project/src/index.js",
        "/root/shared/util.mjs",
    );
}

#[test]
fn directory_import_uses_package_main_field() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/pkg/package.json",
        serde_json::json!({ "main": "./dist/main.cjs" }),
    );
    fixture.write("project/pkg/dist/main.cjs", "module.exports = 1;");
    assert_require(
        &fixture,
        "./pkg",
        "/root/project/index.js",
        "/root/project/pkg/dist/main.cjs",
    );
}

#[test]
fn bare_package_main_only_type_module_resolves() {
    // Regression guard: a bare ESM package whose package.json has `main` but NO
    // `exports` map (e.g. @agentclientprotocol/sdk@0.16.1) must still resolve via
    // the `main` fallback. `exports` and `main` are separate code paths; this case
    // exercises the latter for a scoped package looked up through node_modules.
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/@scope/pkg/package.json",
        serde_json::json!({ "type": "module", "main": "dist/acp.js" }),
    );
    fixture.write("node_modules/@scope/pkg/dist/acp.js", "export default 1;");
    assert_import(
        &fixture,
        "@scope/pkg",
        "/root/project/src/adapter.js",
        "/root/node_modules/@scope/pkg/dist/acp.js",
    );
}

#[test]
fn directory_import_falls_back_to_index_file() {
    let fixture = Fixture::new();
    fixture.write("project/lib/index.cjs", "module.exports = 1;");
    assert_require(
        &fixture,
        "./lib",
        "/root/project/index.js",
        "/root/project/lib/index.cjs",
    );
}

#[test]
fn extension_probe_finds_existing_js_file_directly() {
    let fixture = Fixture::new();
    fixture.write("project/src/direct.js", "export default 1;");
    assert_import(
        &fixture,
        "./direct.js",
        "/root/project/src/index.js",
        "/root/project/src/direct.js",
    );
}

#[test]
fn extension_probe_finds_mjs_file() {
    let fixture = Fixture::new();
    fixture.write("project/src/mod.mjs", "export default 1;");
    assert_import(
        &fixture,
        "./mod",
        "/root/project/src/index.js",
        "/root/project/src/mod.mjs",
    );
}

#[test]
fn extension_probe_finds_cjs_file() {
    let fixture = Fixture::new();
    fixture.write("project/src/common.cjs", "module.exports = 1;");
    assert_require(
        &fixture,
        "./common",
        "/root/project/src/index.js",
        "/root/project/src/common.cjs",
    );
}

#[test]
fn extension_probe_finds_json_file() {
    let fixture = Fixture::new();
    fixture.write("project/src/data.json", r#"{"name":"fixture"}"#);
    assert_require(
        &fixture,
        "./data",
        "/root/project/src/index.js",
        "/root/project/src/data.json",
    );
}

#[test]
fn dot_specifier_resolves_current_package_directory() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/pkg/package.json",
        serde_json::json!({ "main": "./entry.js" }),
    );
    fixture.write("project/pkg/entry.js", "module.exports = 1;");
    assert_require(
        &fixture,
        ".",
        "/root/project/pkg/index.js",
        "/root/project/pkg/entry.js",
    );
}

#[test]
fn exports_string_shorthand_resolves_package_root() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({ "exports": "./dist/index.js" }),
    );
    fixture.write("node_modules/pkg/dist/index.js", "export default 1;");
    assert_import(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/dist/index.js",
    );
}

#[test]
fn exports_conditions_prefer_import_for_esm_resolution() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "exports": {
                ".": {
                    "import": "./dist/import.mjs",
                    "require": "./dist/require.cjs",
                    "default": "./dist/default.js"
                }
            }
        }),
    );
    fixture.write("node_modules/pkg/dist/import.mjs", "export default 1;");
    fixture.write("node_modules/pkg/dist/require.cjs", "module.exports = 1;");
    fixture.write("node_modules/pkg/dist/default.js", "export default 1;");
    assert_import(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/dist/import.mjs",
    );
}

#[test]
fn exports_conditions_prefer_require_for_cjs_resolution() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "exports": {
                ".": {
                    "import": "./dist/import.mjs",
                    "require": "./dist/require.cjs",
                    "default": "./dist/default.js"
                }
            }
        }),
    );
    fixture.write("node_modules/pkg/dist/import.mjs", "export default 1;");
    fixture.write("node_modules/pkg/dist/require.cjs", "module.exports = 1;");
    fixture.write("node_modules/pkg/dist/default.js", "export default 1;");
    assert_require(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/dist/require.cjs",
    );
}

#[test]
fn exports_nested_conditions_recurse_for_import_mode() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "exports": {
                ".": {
                    "import": {
                        "node": "./dist/node.mjs",
                        "default": "./dist/default.mjs"
                    },
                    "default": "./dist/fallback.js"
                }
            }
        }),
    );
    fixture.write("node_modules/pkg/dist/node.mjs", "export default 1;");
    fixture.write("node_modules/pkg/dist/default.mjs", "export default 1;");
    fixture.write("node_modules/pkg/dist/fallback.js", "export default 1;");
    assert_import(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/dist/node.mjs",
    );
}

#[test]
fn exports_wildcard_subpaths_expand_requested_segment() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "exports": {
                "./features/*": "./dist/features/*.mjs"
            }
        }),
    );
    fixture.write(
        "node_modules/pkg/dist/features/alpha.mjs",
        "export default 1;",
    );
    assert_import(
        &fixture,
        "pkg/features/alpha",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/dist/features/alpha.mjs",
    );
}

#[test]
fn exports_wildcard_subpaths_prefer_the_more_specific_pattern() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "exports": {
                "./*.js": "./esm/*.js",
                "./*": "./esm/*.js"
            }
        }),
    );
    fixture.write("node_modules/pkg/esm/sdk/user.js", "export class User {};");
    assert_import(
        &fixture,
        "pkg/sdk/user.js",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/esm/sdk/user.js",
    );
}

#[test]
fn exports_explicit_subpath_resolves_direct_mapping() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "exports": {
                "./feature": "./dist/feature.js"
            }
        }),
    );
    fixture.write("node_modules/pkg/dist/feature.js", "export default 1;");
    assert_import(
        &fixture,
        "pkg/feature",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/dist/feature.js",
    );
}

#[test]
fn exports_array_fallback_uses_first_resolvable_target() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "exports": [
                null,
                "./dist/index.js"
            ]
        }),
    );
    fixture.write("node_modules/pkg/dist/index.js", "export default 1;");
    assert_import(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/dist/index.js",
    );
}

#[test]
fn imports_exact_alias_resolves_relative_target() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/package.json",
        serde_json::json!({
            "imports": {
                "#alias": "./src/alias.js"
            }
        }),
    );
    fixture.write("project/src/alias.js", "export default 1;");
    assert_import(
        &fixture,
        "#alias",
        "/root/project/src/index.js",
        "/root/project/src/alias.js",
    );
}

#[test]
fn imports_condition_object_supports_require_mode() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/package.json",
        serde_json::json!({
            "imports": {
                "#config": {
                    "import": "./src/config.mjs",
                    "require": "./src/config.cjs"
                }
            }
        }),
    );
    fixture.write("project/src/config.mjs", "export default 1;");
    fixture.write("project/src/config.cjs", "module.exports = 1;");
    assert_require(
        &fixture,
        "#config",
        "/root/project/src/index.js",
        "/root/project/src/config.cjs",
    );
}

#[test]
fn imports_wildcard_subpaths_expand_requested_segment() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/package.json",
        serde_json::json!({
            "imports": {
                "#utils/*": "./src/utils/*.js"
            }
        }),
    );
    fixture.write("project/src/utils/math.js", "export default 1;");
    assert_import(
        &fixture,
        "#utils/math",
        "/root/project/src/index.js",
        "/root/project/src/utils/math.js",
    );
}

#[test]
fn imports_wildcard_subpaths_prefer_the_more_specific_pattern() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/package.json",
        serde_json::json!({
            "imports": {
                "#utils/*.js": "./src/utils/*.js",
                "#utils/*": "./src/utils/*.js"
            }
        }),
    );
    fixture.write("project/src/utils/math.js", "export default 1;");
    assert_import(
        &fixture,
        "#utils/math.js",
        "/root/project/src/index.js",
        "/root/project/src/utils/math.js",
    );
}

#[test]
fn imports_walk_up_to_nearest_package_json() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/package.json",
        serde_json::json!({
            "imports": {
                "#shared": "./src/shared.js"
            }
        }),
    );
    fixture.write("project/src/shared.js", "export default 1;");
    assert_import(
        &fixture,
        "#shared",
        "/root/project/src/nested/deeper/index.js",
        "/root/project/src/shared.js",
    );
}

#[test]
fn exports_take_priority_over_main_field() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "main": "./legacy.js",
            "exports": "./modern.js"
        }),
    );
    fixture.write("node_modules/pkg/legacy.js", "module.exports = 1;");
    fixture.write("node_modules/pkg/modern.js", "export default 1;");
    assert_import(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/modern.js",
    );
}

#[test]
fn type_module_directory_import_uses_index_js_for_import_mode() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/esm-dir/package.json",
        serde_json::json!({
            "type": "module"
        }),
    );
    fixture.write("project/esm-dir/index.js", "export default 1;");
    assert_import(
        &fixture,
        "./esm-dir",
        "/root/project/index.js",
        "/root/project/esm-dir/index.js",
    );
}

#[test]
fn main_field_still_beats_nonstandard_module_field() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        serde_json::json!({
            "main": "./main.cjs",
            "module": "./module.mjs"
        }),
    );
    fixture.write("node_modules/pkg/main.cjs", "module.exports = 1;");
    fixture.write("node_modules/pkg/module.mjs", "export default 1;");
    assert_require(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/node_modules/pkg/main.cjs",
    );
}

#[test]
fn pnpm_store_dir_is_not_checked_without_flattened_package_symlink() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/node_modules/.pnpm/node_modules/pkg/package.json",
        serde_json::json!({ "main": "./index.js" }),
    );
    fixture.write(
        "project/node_modules/.pnpm/node_modules/pkg/index.js",
        "module.exports = 1;",
    );
    assert_require_missing(&fixture, "pkg", "/root/project/src/index.js");
}

#[test]
fn npm_workspace_symlink_resolves_to_package_inside_project_root() {
    let fixture = Fixture::new();
    fixture.write_json(
        "packages/lib/package.json",
        serde_json::json!({ "name": "@workspace-test/lib", "main": "src/index.js" }),
    );
    fixture.write("packages/lib/src/index.js", "module.exports = 1;");
    fixture.symlink_dir("packages/lib", "node_modules/@workspace-test/lib");

    assert_require(
        &fixture,
        "@workspace-test/lib",
        "/root/packages/app/src/index.js",
        "/root/node_modules/@workspace-test/lib/src/index.js",
    );
}

#[test]
fn symlinked_package_escape_is_not_resolved() {
    let fixture = Fixture::new();
    let outside = TempDir::new().expect("create outside temp dir");
    fs::write(
        outside.path().join("secret.js"),
        "module.exports = 'secret';",
    )
    .expect("write outside file");
    fixture.mkdir("node_modules");
    symlink(outside.path(), fixture.host_path("node_modules/escape"))
        .expect("create escape symlink");

    let mut resolver = fixture.resolver();
    assert_eq!(
        resolver.resolve_require("escape/secret", "/root/project/index.js"),
        None
    );
}

#[test]
fn absolute_host_path_fallback_is_not_resolved() {
    let fixture = Fixture::new();
    let outside = TempDir::new().expect("create outside temp dir");
    let outside_module = outside.path().join("secret.js");
    fs::write(&outside_module, "module.exports = 'secret';").expect("write outside file");

    let mut resolver = fixture.resolver();
    assert_eq!(
        resolver.resolve_require(
            outside_module.to_string_lossy().as_ref(),
            "/root/project/index.js",
        ),
        None
    );
}

#[test]
fn pnpm_symlinked_referrer_can_resolve_sibling_dependency() {
    let fixture = Fixture::new();
    fixture.write(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-a/index.js",
        "module.exports = require('pkg-b');",
    );
    fixture.write_json(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-a/package.json",
        serde_json::json!({ "main": "./index.js" }),
    );
    fixture.write(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-b/index.js",
        "module.exports = 1;",
    );
    fixture.write_json(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-b/package.json",
        serde_json::json!({ "main": "./index.js" }),
    );
    fixture.symlink_dir(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-a",
        "node_modules/pkg-a",
    );

    assert_require(
        &fixture,
        "pkg-b",
        "/root/node_modules/pkg-a/index.js",
        "/root/node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-b/index.js",
    );
}

#[test]
fn pnpm_symlinked_referrer_prefers_package_store_dependency_over_generic_hoist() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/.pnpm/node_modules/dep/package.json",
        serde_json::json!({ "main": "./index.js" }),
    );
    fixture.write(
        "node_modules/.pnpm/node_modules/dep/index.js",
        "module.exports = 'generic';",
    );
    fixture.write(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-a/index.js",
        "import { named } from 'dep';\nexport default named;\n",
    );
    fixture.write_json(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-a/package.json",
        serde_json::json!({ "type": "module" }),
    );
    fixture.write(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/dep/index.js",
        "export const named = 1;",
    );
    fixture.write_json(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/dep/package.json",
        serde_json::json!({
            "type": "module",
            "exports": "./index.js",
        }),
    );
    fixture.symlink_dir(
        "node_modules/.pnpm/pkg-a@1.0.0/node_modules/pkg-a",
        "node_modules/pkg-a",
    );

    assert_import(
        &fixture,
        "dep",
        "/root/node_modules/pkg-a/index.js",
        "/root/node_modules/.pnpm/pkg-a@1.0.0/node_modules/dep/index.js",
    );
}

#[test]
fn root_node_modules_fallback_is_checked_last() {
    let fixture = Fixture::new();
    fixture.mkdir("project/src");
    fixture.write_json(
        "node_modules/shared-pkg/package.json",
        serde_json::json!({ "main": "./index.js" }),
    );
    fixture.write("node_modules/shared-pkg/index.js", "module.exports = 1;");
    assert_require(
        &fixture,
        "shared-pkg",
        "/root/project/src/index.js",
        "/root/node_modules/shared-pkg/index.js",
    );
}

// Reproduces the ~/r6 conflict: minimatch@10 (ESM) imports `brace-expansion`.
// Its OWN nested node_modules has the compatible brace-expansion@5 (named ESM
// `expand`), while a hoisted top-level brace-expansion@1 (CJS, no named export)
// exists too. Real Node resolves the nested @5; the VM must do the same.
#[test]
fn nested_dep_wins_over_hoisted_incompatible_version() {
    let fixture = Fixture::new();

    // minimatch@10: ESM package whose esm entry imports brace-expansion.
    fixture.write_json(
        "node_modules/minimatch/package.json",
        serde_json::json!({
            "version": "10.2.5",
            "type": "module",
            "main": "./dist/commonjs/index.js",
            "module": "./dist/esm/index.js",
            "exports": {
                ".": {
                    "import": "./dist/esm/index.js",
                    "require": "./dist/commonjs/index.js"
                }
            }
        }),
    );
    fixture.write(
        "node_modules/minimatch/dist/esm/index.js",
        "import { expand } from 'brace-expansion';\nexport const minimatch = () => {};",
    );

    // minimatch's OWN nested brace-expansion@5 (compatible, real ESM).
    fixture.write_json(
        "node_modules/minimatch/node_modules/brace-expansion/package.json",
        serde_json::json!({
            "version": "5.0.5",
            "type": "module",
            "main": "./dist/commonjs/index.js",
            "module": "./dist/esm/index.js",
            "exports": {
                ".": {
                    "import": "./dist/esm/index.js",
                    "require": "./dist/commonjs/index.js"
                }
            }
        }),
    );
    fixture.write(
        "node_modules/minimatch/node_modules/brace-expansion/dist/esm/index.js",
        "export function expand() {}",
    );

    // Hoisted top-level brace-expansion@1 (incompatible CJS).
    fixture.write_json(
        "node_modules/brace-expansion/package.json",
        serde_json::json!({ "version": "1.1.12", "main": "index.js" }),
    );
    fixture.write(
        "node_modules/brace-expansion/index.js",
        "module.exports = function () {};",
    );

    assert_import(
        &fixture,
        "brace-expansion",
        "/root/node_modules/minimatch/dist/esm/index.js",
        "/root/node_modules/minimatch/node_modules/brace-expansion/dist/esm/index.js",
    );
}

// Regression for the pnpm virtual-store scan shadowing hoisted resolution: a
// consumer imports `dep`; the correct version is hoisted at the top level, but
// an alphabetically-earlier `.pnpm/<other>@ver/node_modules/dep` holds an
// incompatible version. The standard ancestor walk (which finds the hoisted
// copy) must win over the store scan, matching real Node. Before the fix the
// scan ran while walking and returned the wrong store entry first.
#[test]
fn hoisted_dependency_wins_over_alphabetically_earlier_pnpm_store_entry() {
    let fixture = Fixture::new();

    // Consumer is hoisted at the top level (not under .pnpm).
    fixture.write(
        "node_modules/consumer/index.js",
        "import { wanted } from 'dep';\nexport default wanted;",
    );
    fixture.write_json(
        "node_modules/consumer/package.json",
        serde_json::json!({ "type": "module" }),
    );

    // Correct hoisted dep: real ESM with the named export.
    fixture.write_json(
        "node_modules/dep/package.json",
        serde_json::json!({
            "type": "module",
            "exports": { ".": { "import": "./index.mjs" } }
        }),
    );
    fixture.write("node_modules/dep/index.mjs", "export const wanted = 1;");

    // Incompatible dep nested in the pnpm store under an alphabetically-earlier
    // key (`aaa-pkg` sorts before any real consumer key). No named export.
    fixture.write_json(
        "node_modules/.pnpm/aaa-pkg@1.0.0/node_modules/dep/package.json",
        serde_json::json!({ "version": "0.0.1", "main": "index.js" }),
    );
    fixture.write(
        "node_modules/.pnpm/aaa-pkg@1.0.0/node_modules/dep/index.js",
        "module.exports = 1;",
    );

    assert_import(
        &fixture,
        "dep",
        "/root/node_modules/consumer/index.js",
        "/root/node_modules/dep/index.mjs",
    );
}

// Faithful pnpm layout (what pnpm + real Node actually rely on): every package
// lives in its own `.pnpm/<pkg>@<ver>/node_modules/<pkg>` entry, and a
// consumer's dependency is a *symlink* sibling inside the consumer's store dir
// pointing at the dep's own entry. Top-level `node_modules/<pkg>` is a symlink
// to the store entry. Resolution must work purely by realpath + the standard
// ancestor walk (Docker-style VFS), with NO `.pnpm` store scanning — and must
// pick the version the symlink points at, not an alphabetically-earlier entry.
#[test]
fn faithful_pnpm_symlink_layout_resolves_via_realpath_walk() {
    let fixture = Fixture::new();

    // consumer@1.0.0 in its store entry; imports `dep`.
    fixture.write(
        "node_modules/.pnpm/consumer@1.0.0/node_modules/consumer/index.mjs",
        "import { wanted } from 'dep';\nexport default wanted;",
    );
    fixture.write_json(
        "node_modules/.pnpm/consumer@1.0.0/node_modules/consumer/package.json",
        serde_json::json!({ "version": "1.0.0", "type": "module", "exports": { ".": "./index.mjs" } }),
    );

    // dep@2.0.0 — the correct version — in its own store entry.
    fixture.write(
        "node_modules/.pnpm/dep@2.0.0/node_modules/dep/index.mjs",
        "export const wanted = 2;",
    );
    fixture.write_json(
        "node_modules/.pnpm/dep@2.0.0/node_modules/dep/package.json",
        serde_json::json!({ "version": "2.0.0", "type": "module", "exports": { ".": "./index.mjs" } }),
    );

    // Decoy: an alphabetically-earlier store entry holding an incompatible dep@1.
    fixture.write(
        "node_modules/.pnpm/aaa-other@1.0.0/node_modules/dep/index.js",
        "module.exports = 1;",
    );
    fixture.write_json(
        "node_modules/.pnpm/aaa-other@1.0.0/node_modules/dep/package.json",
        serde_json::json!({ "version": "1.0.0", "main": "index.js" }),
    );

    // pnpm's sibling symlink: consumer's `dep` -> dep@2.0.0's store entry.
    fixture.symlink_dir(
        "node_modules/.pnpm/dep@2.0.0/node_modules/dep",
        "node_modules/.pnpm/consumer@1.0.0/node_modules/dep",
    );
    // Top-level symlink: node_modules/consumer -> consumer's store entry.
    fixture.symlink_dir(
        "node_modules/.pnpm/consumer@1.0.0/node_modules/consumer",
        "node_modules/consumer",
    );

    // Importer is the top-level symlink path. The standard ancestor walk finds
    // `dep` via pnpm's sibling symlink in consumer's store dir (which points at
    // dep@2.0.0) — no `.pnpm` store scan involved. The resolver returns that
    // symlink path; its realpath is dep@2.0.0's entry (the correct version, not
    // the alphabetically-earlier aaa-other dep@1).
    assert_import(
        &fixture,
        "dep",
        "/root/node_modules/consumer/index.mjs",
        "/root/node_modules/.pnpm/consumer@1.0.0/node_modules/dep/index.mjs",
    );
}
