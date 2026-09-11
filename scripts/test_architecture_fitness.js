#!/usr/bin/env node
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const { collectFindings, formatText, summarize } = require("./architecture_fitness.js");

const repoRoot = path.resolve(__dirname, "..");
const script = path.join(repoRoot, "scripts", "architecture_fitness.js");
const fixtureRoot = path.join(repoRoot, "tests", "architecture_fitness", "fixtures");
const goldenRoot = path.join(repoRoot, "tests", "architecture_fitness", "golden");

const cases = [
  {
    name: "call relation domain boundaries",
    fixture: "call_domain_boundary",
    golden: "call_domain_boundary.txt",
    expectedStatus: 1,
    args: [],
    requiredOwners: ["call-service"],
  },
  {
    name: "forbidden dependency",
    fixture: "forbidden_dependency",
    golden: "forbidden_dependency.txt",
    expectedStatus: 1,
    args: [],
    requiredOwners: [],
  },
  {
    name: "large file warning",
    fixture: "large_file_warning",
    golden: "large_file_warning.txt",
    expectedStatus: 0,
    args: ["--large-threshold", "3"],
    requiredOwners: [],
  },
  {
    name: "ordinary completion service rejects tower_lsp",
    fixture: "ordinary_completion_service_lsp",
    golden: "ordinary_completion_service_lsp.txt",
    expectedStatus: 1,
    args: [],
    requiredOwners: [],
  },
  {
    name: "ordinary completion service rejects project discovery IO",
    fixture: "project_context_hot_path_io",
    golden: "project_context_hot_path_io.txt",
    expectedStatus: 1,
    args: [],
    requiredOwners: [],
  },
  {
    name: "large test sources do not create production size warnings",
    fixture: "large_test_sources",
    golden: "large_test_sources.txt",
    expectedStatus: 0,
    args: ["--large-threshold", "3"],
    requiredOwners: [],
  },
  {
    name: "cfg test helpers cannot hide production source size",
    fixture: "cfg_test_boundary",
    golden: "cfg_test_boundary.txt",
    expectedStatus: 0,
    args: ["--large-threshold", "6"],
    requiredOwners: [],
  },
  {
    name: "v1.4.2 semantic candidate and source excerpt boundaries",
    fixture: "semantic_candidate_boundary",
    golden: "semantic_candidate_boundary.txt",
    expectedStatus: 1,
    args: [],
    requiredOwners: [],
  },
];

for (const testCase of cases) {
  const root = path.join(fixtureRoot, testCase.fixture);
  const thresholdIndex = testCase.args.indexOf("--large-threshold");
  const findings = collectFindings(root, {
    largeThreshold: thresholdIndex === -1 ? undefined : Number.parseInt(testCase.args[thresholdIndex + 1], 10),
    requiredOwners: testCase.requiredOwners,
  });
  const actualStatus = summarize(findings).fail > 0 ? 1 : 0;
  const actualOutput = formatText(findings);

  const expected = fs.readFileSync(path.join(goldenRoot, testCase.golden), "utf8");
  assert.equal(actualStatus, testCase.expectedStatus, `${testCase.name} exit status`);
  assert.equal(
    actualOutput.replace(/\r\n/g, "\n"),
    expected.replace(/\r\n/g, "\n"),
    `${testCase.name} stdout`
  );
}

console.log(`architecture fitness golden tests passed (${cases.length} cases)`);

// Moving a hot path into a child module must retain its I/O constraint.
const tempRoot = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'fossilsense-architecture-'));
try {
  const target = path.join(tempRoot, 'crates/fossilsense/src/completion/ordinary_service/nested.rs');
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, 'use std::fs;\npub fn read() { let _ = fs::read_dir("."); }\n');
  const findings = collectFindings(tempRoot, { requiredOwners: [] });
  assert.equal(summarize(findings).fail, 1, 'ordinary completion child module must reject filesystem I/O');
} finally { fs.rmSync(tempRoot, { recursive: true, force: true }); }

// The call relation service boundary must follow the real production owner,
// including both its root module and extracted child modules.
const callServiceRoot = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'fossilsense-call-service-'));
try {
  const rootModule = path.join(callServiceRoot, 'crates/fossilsense/src/call_service.rs');
  const childModule = path.join(callServiceRoot, 'crates/fossilsense/src/call_service/nested.rs');
  const legacyAdapter = path.join(callServiceRoot, 'crates/fossilsense/src/server/call_hierarchy.rs');
  for (const target of [rootModule, childModule, legacyAdapter]) {
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, 'use std::fs;\npub fn scan() { let _ = fs::read_dir("."); }\n');
  }
  const findings = collectFindings(callServiceRoot, { requiredOwners: ['call-service'] });
  const callServiceFiles = findings
    .filter((finding) => finding.rule === 'call-service-io-boundary')
    .map((finding) => finding.file)
    .sort();
  assert.deepEqual(callServiceFiles, [
    'crates/fossilsense/src/call_service/nested.rs',
    'crates/fossilsense/src/call_service.rs',
  ].sort());
} finally { fs.rmSync(callServiceRoot, { recursive: true, force: true }); }

// Test-only helpers inside the owner do not become production dependency violations.
const callServiceTestsRoot = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'fossilsense-call-service-tests-'));
try {
  const rootModule = path.join(callServiceTestsRoot, 'crates/fossilsense/src/call_service.rs');
  fs.mkdirSync(path.dirname(rootModule), { recursive: true });
  fs.writeFileSync(rootModule, [
    'pub struct CallRelationService;',
    '#[cfg(test)]',
    'mod tests {',
    '  use std::fs;',
    '  fn fixture() { let _ = fs::read_dir("."); }',
    '}',
    '',
  ].join('\n'));
  assert.deepEqual(
    collectFindings(callServiceTestsRoot, { requiredOwners: ['call-service'] }),
    [],
    'cfg(test) helpers must not create production call-service violations'
  );
} finally { fs.rmSync(callServiceTestsRoot, { recursive: true, force: true }); }

// Explicit test owner sets and the production CLI default both reject zero matches.
const missingOwnerRoot = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'fossilsense-missing-owner-'));
try {
  const unrelated = path.join(missingOwnerRoot, 'crates/fossilsense/src/model.rs');
  fs.mkdirSync(path.dirname(unrelated), { recursive: true });
  fs.writeFileSync(unrelated, 'pub struct Model;\n');
  const explicitFindings = collectFindings(missingOwnerRoot, { requiredOwners: ['call-service'] });
  assert.equal(explicitFindings.length, 1);
  assert.equal(explicitFindings[0].rule, 'call-service-io-boundary');
  assert.match(explicitFindings[0].detail, /matched zero files.*call_service/);

  const productionDefault = spawnSync(process.execPath, [script, '--root', missingOwnerRoot], {
    cwd: repoRoot,
    encoding: 'utf8',
    windowsHide: true,
  });
  assert.equal(productionDefault.status, 1, 'production default must require the call-service owner');
  assert.match(productionDefault.stdout, /call-service-io-boundary.*matched zero files.*call_service/);
} finally { fs.rmSync(missingOwnerRoot, { recursive: true, force: true }); }
