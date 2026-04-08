"use strict";

const knownFeatures = new Set(["crypto", "sqlite", "markAsUncloneable", "zstd"]);

class RuntimeFeatures {
  #map = new Map([
    ["crypto", true],
    ["sqlite", false],
    ["markAsUncloneable", false],
    ["zstd", false],
  ]);

  clear() {
    this.#map.clear();
  }

  has(feature) {
    if (!knownFeatures.has(feature)) {
      throw new TypeError(`unknown feature: ${feature}`);
    }
    return this.#map.get(feature) ?? false;
  }

  set(feature, value) {
    if (!knownFeatures.has(feature)) {
      throw new TypeError(`unknown feature: ${feature}`);
    }
    this.#map.set(feature, Boolean(value));
  }
}

const runtimeFeatures = new RuntimeFeatures();

module.exports = {
  runtimeFeatures,
  default: runtimeFeatures,
};
