#!/usr/bin/env bun
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const packageName = "seaport";
const version = readVersion();

updateCargoToml(version);
updateCargoLock(version);

function readVersion() {
  const packageJson = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));

  if (typeof packageJson.version !== "string" || packageJson.version.length === 0) {
    throw new Error("package.json must contain a non-empty string version");
  }

  return packageJson.version;
}

function updateCargoToml(nextVersion) {
  const path = resolve(root, "Cargo.toml");
  const lines = readFileSync(path, "utf8").split(/(?<=\n)/);
  let inPackage = false;
  let updated = false;

  for (const [index, line] of lines.entries()) {
    const trimmed = line.trim();

    if (trimmed === "[package]") {
      inPackage = true;
      continue;
    }

    if (inPackage && trimmed.startsWith("[")) {
      break;
    }

    if (inPackage && trimmed.startsWith("version")) {
      lines[index] = line.replace(/"[^"]+"/, `"${nextVersion}"`);
      updated = true;
      break;
    }
  }

  if (!updated) {
    throw new Error("could not update Cargo.toml package version");
  }

  writeFileSync(path, lines.join(""));
}

function updateCargoLock(nextVersion) {
  const path = resolve(root, "Cargo.lock");
  const lines = readFileSync(path, "utf8").split(/(?<=\n)/);
  let inPackage = false;
  let packageMatches = false;
  let updated = false;

  for (const [index, line] of lines.entries()) {
    const trimmed = line.trim();

    if (trimmed === "[[package]]") {
      inPackage = true;
      packageMatches = false;
      continue;
    }

    if (inPackage && trimmed.startsWith("name")) {
      packageMatches = trimmed === `name = "${packageName}"`;
      continue;
    }

    if (inPackage && packageMatches && trimmed.startsWith("version")) {
      lines[index] = line.replace(/"[^"]+"/, `"${nextVersion}"`);
      updated = true;
      break;
    }
  }

  if (!updated) {
    throw new Error("could not update Cargo.lock package version");
  }

  writeFileSync(path, lines.join(""));
}
