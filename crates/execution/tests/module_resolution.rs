use agent_os_execution::javascript::ModuleResolutionTestHarness;
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
fn pnpm_candidate_dir_is_checked_without_flattened_package_symlink() {
    let fixture = Fixture::new();
    fixture.write_json(
        "project/node_modules/.pnpm/node_modules/pkg/package.json",
        serde_json::json!({ "main": "./index.js" }),
    );
    fixture.write(
        "project/node_modules/.pnpm/node_modules/pkg/index.js",
        "module.exports = 1;",
    );
    assert_require(
        &fixture,
        "pkg",
        "/root/project/src/index.js",
        "/root/project/node_modules/.pnpm/node_modules/pkg/index.js",
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
fn pnpm_symlinked_referrer_can_resolve_virtual_store_dependency() {
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
        "node_modules/.pnpm/pkg-b@1.0.0/node_modules/pkg-b/index.js",
        "module.exports = 1;",
    );
    fixture.write_json(
        "node_modules/.pnpm/pkg-b@1.0.0/node_modules/pkg-b/package.json",
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
        "/root/node_modules/.pnpm/pkg-b@1.0.0/node_modules/pkg-b/index.js",
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
