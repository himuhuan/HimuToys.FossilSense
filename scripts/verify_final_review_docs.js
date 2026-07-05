#!/usr/bin/env node
const fs = require("node:fs");
const path = require("node:path");

function parseArgs(argv) {
  const args = {
    root: path.resolve(__dirname, ".."),
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--root") {
      args.root = path.resolve(argv[++i]);
    } else if (arg === "--help" || arg === "-h") {
      args.help = true;
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return args;
}

function usage() {
  return "Usage: node scripts/verify_final_review_docs.js [--root <path>]";
}

function readUtf8(root, relPath) {
  const fullPath = path.join(root, ...relPath.split("/"));
  if (!fs.existsSync(fullPath)) {
    return null;
  }
  return fs.readFileSync(fullPath, "utf8");
}

function includesAll(text, values) {
  return values.every((value) => text.includes(value));
}

function collectFindings(root) {
  const findings = [];
  const followUpsPath = "docs/architecture/follow-ups.md";
  const readmePath = "docs/architecture/README.md";
  const deliveryPath = "dist/DELIVERY-NOTE-1.2.2.md";
  const tasksPath = "openspec/changes/plan-healthy-v122-architecture-refactor/tasks.md";

  const followUps = readUtf8(root, followUpsPath);
  const readme = readUtf8(root, readmePath);
  const delivery = readUtf8(root, deliveryPath);
  const tasks = readUtf8(root, tasksPath);

  if (!followUps) {
    findings.push(`${followUpsPath} is missing`);
  } else {
    if (!followUps.includes("# v1.2.2 Architecture Follow-ups")) {
      findings.push(`${followUpsPath} must provide a stable v1.2.2 follow-up heading`);
    }
    if (!followUps.includes("## Final Review Record")) {
      findings.push(`${followUpsPath} must record the Final Review result`);
    }
    if (!includesAll(followUps, ["Phase A", "Phase B", "Phase C", "Phase D", "Phase H"])) {
      findings.push(`${followUpsPath} must confirm the required v1.2.2 scope is limited to phases A, B, C, D, and H`);
    }
    if (!includesAll(followUps, ["Phase E", "Phase F", "Phase G", "follow-up candidate"])) {
      findings.push(`${followUpsPath} must defer phases E, F, and G as follow-up candidates`);
    }
    if (!followUps.includes("plan-healthy-v122-architecture-refactor")) {
      findings.push(`${followUpsPath} must name the reviewed OpenSpec change`);
    }
    if (!followUps.includes("docs/research/healthy-fossilsense-dev-eval.md")) {
      findings.push(`${followUpsPath} must link the research evaluation used for scope review`);
    }
  }

  if (!readme || !readme.includes("docs/architecture/follow-ups.md")) {
    findings.push(`${readmePath} must link to ${followUpsPath}`);
  }

  if (!delivery || !delivery.includes("docs/architecture/follow-ups.md")) {
    findings.push(`${deliveryPath} must link to ${followUpsPath}`);
  }

  if (!tasks || !includesAll(tasks, ["- [x] 6.1", "- [x] 6.2", "- [x] 6.3"])) {
    findings.push(`${tasksPath} must mark Final Review tasks 6.1, 6.2, and 6.3 complete`);
  }

  return findings;
}

function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    console.error(usage());
    return 2;
  }

  if (args.help) {
    console.log(usage());
    return 0;
  }

  const findings = collectFindings(args.root);
  if (findings.length > 0) {
    console.error("Final Review documentation check failed:");
    for (const finding of findings) {
      console.error(`- ${finding}`);
    }
    return 1;
  }

  console.log("Final Review documentation check passed.");
  return 0;
}

if (require.main === module) {
  process.exitCode = main();
}

module.exports = {
  collectFindings,
};
