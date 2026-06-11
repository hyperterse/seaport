#!/usr/bin/env bun
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const packageJson = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
const version = packageJson.version;

if (typeof version !== "string" || version.length === 0) {
  throw new Error("package.json must contain a non-empty string version");
}

const tag = `v${version}`;
const notes = latestReleaseNotes(version);

writeFileSync(resolve(root, "release-notes.md"), notes);
writeOutputs({ version, tag });

function latestReleaseNotes(releaseVersion) {
  const changelog = readFileSync(resolve(root, "CHANGELOG.md"), "utf8");
  const escapedVersion = releaseVersion.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const headingPattern = new RegExp(
    `^#{2,3}\\s+(?:\\[)?${escapedVersion}(?:\\])?(?:\\s|\\(|$)`,
    "m",
  );
  const heading = changelog.match(headingPattern);

  if (!heading || heading.index === undefined) {
    throw new Error(`could not find ${releaseVersion} in CHANGELOG.md`);
  }

  const start = heading.index;
  const rest = changelog.slice(start + heading[0].length);
  // Stop at the next *version* heading (level-2 `## `). `#{2,3}` also matched
  // the `### Features`/`### Bug Fixes` subsections, which truncated the notes
  // to just the version heading.
  const nextHeading = rest.search(/^## /m);
  const end = nextHeading === -1 ? changelog.length : start + heading[0].length + nextHeading;

  return `${changelog.slice(start, end).trim()}\n`;
}

function writeOutputs(values) {
  const lines = Object.entries(values).map(([key, value]) => `${key}=${value}`);

  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `${lines.join("\n")}\n`);
  }

  for (const line of lines) {
    console.log(line);
  }
}
