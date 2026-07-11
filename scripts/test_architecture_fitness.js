#!/usr/bin/env node
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const repoRoot = path.resolve(__dirname, "..");
const script = path.join(repoRoot, "scripts", "architecture_fitness.js");
const fixtureRoot = path.join(repoRoot, "tests", "architecture_fitness", "fixtures");
const goldenRoot = path.join(repoRoot, "tests", "architecture_fitness", "golden");

const cases = [
  {
    name: "forbidden dependency",
    fixture: "forbidden_dependency",
    golden: "forbidden_dependency.txt",
    expectedStatus: 1,
    args: [],
  },
  {
    name: "large file warning",
    fixture: "large_file_warning",
    golden: "large_file_warning.txt",
    expectedStatus: 0,
    args: ["--large-threshold", "3"],
  },
  {
    name: "ordinary completion service rejects tower_lsp",
    fixture: "ordinary_completion_service_lsp",
    golden: "ordinary_completion_service_lsp.txt",
    expectedStatus: 1,
    args: [],
  },
  {
    name: "ordinary completion service rejects project discovery IO",
    fixture: "project_context_hot_path_io",
    golden: "project_context_hot_path_io.txt",
    expectedStatus: 1,
    args: [],
  },
  {
    name: "large test sources do not create production size warnings",
    fixture: "large_test_sources",
    golden: "large_test_sources.txt",
    expectedStatus: 0,
    args: ["--large-threshold", "3"],
  },
  {
    name: "cfg test helpers cannot hide production source size",
    fixture: "cfg_test_boundary",
    golden: "cfg_test_boundary.txt",
    expectedStatus: 0,
    args: ["--large-threshold", "6"],
  },
];

for (const testCase of cases) {
  const root = path.join(fixtureRoot, testCase.fixture);
  const result = spawnSync(
    process.execPath,
    [script, "--root", root, "--format", "text", ...testCase.args],
    {
      cwd: repoRoot,
      encoding: "utf8",
      windowsHide: true,
    }
  );

  const expected = fs.readFileSync(path.join(goldenRoot, testCase.golden), "utf8");
  assert.equal(result.status, testCase.expectedStatus, `${testCase.name} exit status\n${result.stderr}`);
  assert.equal(result.stdout.replace(/\r\n/g, "\n"), expected, `${testCase.name} stdout`);
  assert.equal(result.stderr.replace(/\r\n/g, "\n"), "", `${testCase.name} stderr`);
}

console.log(`architecture fitness golden tests passed (${cases.length} cases)`);
