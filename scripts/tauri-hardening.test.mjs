// Tests for tauri-hardening.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  CSP,
  checkCapabilities,
  checkCommands,
  checkConfig,
  checkFeatures,
  checkPermissions,
  checkRepoFiles,
} from "./tauri-hardening.mjs";

/** The hardened tauri.conf.json, as P8.3 sets it. */
function hardenedConfig() {
  return {
    productName: "afkfleet",
    build: { devUrl: "http://localhost:1420", frontendDist: "../dist" },
    app: {
      withGlobalTauri: false,
      windows: [{ label: "main", title: "afkfleet", width: 1200, height: 800 }],
      security: {
        csp: CSP,
        dangerousDisableAssetCspModification: true,
        freezePrototype: true,
        capabilities: ["main"],
      },
    },
  };
}

/** The one capability, as P8.3 writes it. */
function mainCapability() {
  return {
    $schema: "../gen/schemas/desktop-schema.json",
    identifier: "main",
    description: "The main window.",
    windows: ["main"],
    permissions: [],
  };
}

function capabilityFile(content) {
  return { name: "main.json", text: JSON.stringify(content) };
}

const BUILD_SCRIPT = `use tauri_build::{AppManifest, Attributes};

/// The app's own commands.
const COMMANDS: &[&str] = &[];

fn main() {}
`;

const DEFAULT_PERMISSION = `# Why this file exists.
[default]
description = "Grants nothing."
permissions = []
`;

const CARGO_TREE = "tauri v2.12.3|compression,tauri-runtime-wry,webkit2gtk,webview2-com,wry\n";

describe("checkConfig", () => {
  it("accepts the hardened configuration", () => {
    assert.deepEqual(checkConfig(hardenedConfig()), []);
  });

  it("fails on a changed CSP", () => {
    const config = hardenedConfig();
    config.app.security.csp = `${CSP}; script-src 'self' 'unsafe-eval'`;

    assert.match(checkConfig(config).join("\n"), /app\.security\.csp/);
  });

  it("fails on a global API that's on or left to its default", () => {
    const on = hardenedConfig();
    on.app.withGlobalTauri = true;
    const unset = hardenedConfig();
    delete unset.app.withGlobalTauri;

    assert.match(checkConfig(on).join("\n"), /withGlobalTauri/);
    assert.match(checkConfig(unset).join("\n"), /withGlobalTauri/);
  });

  it("fails on any security key that's added, removed or changed", () => {
    const devCsp = hardenedConfig();
    devCsp.app.security.devCsp = "default-src *";
    const modification = hardenedConfig();
    delete modification.app.security.dangerousDisableAssetCspModification;
    const prototype = hardenedConfig();
    prototype.app.security.freezePrototype = false;
    const capabilities = hardenedConfig();
    capabilities.app.security.capabilities = ["main", "extra"];
    const assets = hardenedConfig();
    assets.app.security.assetProtocol = { enable: true, scope: ["**"] };

    assert.match(checkConfig(devCsp).join("\n"), /devCsp/);
    assert.match(checkConfig(modification).join("\n"), /dangerousDisableAssetCspModification/);
    assert.match(checkConfig(prototype).join("\n"), /freezePrototype/);
    assert.match(checkConfig(capabilities).join("\n"), /capabilities/);
    assert.match(checkConfig(assets).join("\n"), /assetProtocol/);
  });

  it("fails on a window with devtools, its own URL or the https scheme", () => {
    for (const [key, value] of [
      ["devtools", true],
      ["url", "https://example.com"],
      ["useHttpsScheme", true],
    ]) {
      const config = hardenedConfig();
      config.app.windows[0][key] = value;

      assert.match(checkConfig(config).join("\n"), new RegExp(key));
    }
  });

  it("fails on another dev URL", () => {
    const config = hardenedConfig();
    config.build.devUrl = "https://example.com";

    assert.match(checkConfig(config).join("\n"), /devUrl/);
  });
});

describe("checkCapabilities", () => {
  it("accepts the one capability that grants nothing", () => {
    assert.deepEqual(checkCapabilities([capabilityFile(mainCapability())]), []);
  });

  it("fails on a core or plugin permission", () => {
    for (const permission of ["core:default", "core:event:allow-listen", "opener:default"]) {
      const capability = mainCapability();
      capability.permissions = [permission];

      assert.match(checkCapabilities([capabilityFile(capability)]).join("\n"), /permissions/);
    }
  });

  it("fails on a grant for a command build.rs doesn't list", () => {
    const capability = mainCapability();
    capability.permissions = ["allow-probe"];

    assert.match(checkCapabilities([capabilityFile(capability)]).join("\n"), /permissions/);
  });

  it("fails on another window, a remote origin or a platform limit", () => {
    const windows = mainCapability();
    windows.windows = ["*"];
    const remote = mainCapability();
    remote.remote = { urls: ["https://example.com"] };
    const platforms = mainCapability();
    platforms.platforms = ["windows"];

    assert.match(checkCapabilities([capabilityFile(windows)]).join("\n"), /windows/);
    assert.match(checkCapabilities([capabilityFile(remote)]).join("\n"), /remote/);
    assert.match(checkCapabilities([capabilityFile(platforms)]).join("\n"), /platforms/);
  });

  it("fails on a second capability file, or none", () => {
    const second = { name: "extra.json", text: JSON.stringify(mainCapability()) };

    assert.match(checkCapabilities([capabilityFile(mainCapability()), second]).join("\n"), /extra/);
    assert.match(checkCapabilities([]).join("\n"), /main\.json/);
  });

  it("fails on a capability that isn't JSON", () => {
    assert.match(checkCapabilities([{ name: "main.json", text: "{" }]).join("\n"), /JSON/);
  });
});

describe("checkCommands", () => {
  it("accepts build.rs's empty command list", () => {
    assert.deepEqual(checkCommands(BUILD_SCRIPT), []);
  });

  it("fails on a command the check doesn't expect", () => {
    const script = BUILD_SCRIPT.replace("&[];", '&["get_status"];');

    assert.match(checkCommands(script).join("\n"), /get_status/);
  });

  it("fails when build.rs has no command list", () => {
    assert.match(checkCommands("fn main() {}\n").join("\n"), /COMMANDS/);
  });
});

describe("checkPermissions", () => {
  it("accepts the empty default permission", () => {
    assert.deepEqual(checkPermissions([{ name: "default.toml", text: DEFAULT_PERMISSION }]), []);
  });

  it("accepts Windows line endings", () => {
    const text = DEFAULT_PERMISSION.replaceAll("\n", "\r\n");

    assert.deepEqual(checkPermissions([{ name: "default.toml", text }]), []);
  });

  it("fails when the default permission grants something", () => {
    const text = DEFAULT_PERMISSION.replace("permissions = []", 'permissions = ["allow-probe"]');

    assert.match(checkPermissions([{ name: "default.toml", text }]).join("\n"), /permissions/);
  });

  it("fails on another permission or set in the file", () => {
    const text = `${DEFAULT_PERMISSION}\n[[permission]]\nidentifier = "allow-all"\n`;

    assert.match(checkPermissions([{ name: "default.toml", text }]).join("\n"), /permission/);
  });

  it("fails on a missing or an extra permission file", () => {
    const extra = { name: "extra.toml", text: "" };

    assert.match(checkPermissions([]).join("\n"), /default\.toml/);
    assert.match(
      checkPermissions([{ name: "default.toml", text: DEFAULT_PERMISSION }, extra]).join("\n"),
      /extra\.toml/,
    );
  });
});

describe("checkFeatures", () => {
  it("accepts exactly the features wry and compression resolve to", () => {
    assert.deepEqual(checkFeatures(CARGO_TREE), []);
    assert.deepEqual(checkFeatures(CARGO_TREE.replace("\n", "\r\n")), []);
  });

  it("names devtools, dynamic-acl and custom-protocol as never allowed", () => {
    for (const feature of ["devtools", "dynamic-acl", "custom-protocol"]) {
      const output = CARGO_TREE.replace("compression,", `compression,${feature},`);

      assert.match(checkFeatures(output).join("\n"), new RegExp(`${feature}.*never`));
    }
  });

  it("fails on any other change to the feature set", () => {
    const added = CARGO_TREE.replace("compression,", "compression,common-controls-v6,");
    const removed = CARGO_TREE.replace("compression,", "");

    assert.match(checkFeatures(added).join("\n"), /common-controls-v6/);
    assert.match(checkFeatures(removed).join("\n"), /compression/);
  });

  it("fails on output it can't read", () => {
    assert.match(checkFeatures("").join("\n"), /can't be read/);
    assert.match(checkFeatures("warning: nothing to print.\n").join("\n"), /can't be read/);
    assert.match(checkFeatures(`${CARGO_TREE}${CARGO_TREE}`).join("\n"), /can't be read/);
  });
});

describe("the repository", () => {
  it("has the hardened desktop configuration", () => {
    const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

    assert.deepEqual(checkRepoFiles(repoRoot), []);
  });
});
