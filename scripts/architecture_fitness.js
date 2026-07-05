#!/usr/bin/env node
const fs = require("node:fs");
const path = require("node:path");

const DEFAULT_LARGE_THRESHOLD = 800;

const RULES = {
  lspBoundary: "lsp-boundary",
  ordinaryCompletionServiceLspBoundary: "ordinary-completion-service-lsp-boundary",
  sqliteBoundary: "sqlite-boundary",
  coreDirection: "core-dependency-direction",
  largeFile: "large-source-file",
};

const DEFAULT_ALLOWLIST = [
  {
    rule: RULES.lspBoundary,
    file: "crates/fossilsense/src/query/lsp_kinds.rs",
    reason:
      "Query currently contains a transitional LSP-kind adapter; move it under server/lsp_adapters.rs during a later behavior-preserving step.",
  },
];

function parseArgs(argv) {
  const args = {
    root: path.resolve(__dirname, ".."),
    format: "text",
    largeThreshold: DEFAULT_LARGE_THRESHOLD,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--root") {
      args.root = path.resolve(argv[++i]);
    } else if (arg === "--format") {
      args.format = argv[++i];
    } else if (arg === "--large-threshold") {
      args.largeThreshold = Number.parseInt(argv[++i], 10);
    } else if (arg === "--help" || arg === "-h") {
      args.help = true;
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  if (!Number.isInteger(args.largeThreshold) || args.largeThreshold < 1) {
    throw new Error("--large-threshold must be a positive integer");
  }

  if (!["text", "json"].includes(args.format)) {
    throw new Error("--format must be text or json");
  }

  return args;
}

function usage() {
  return [
    "Usage: node scripts/architecture_fitness.js [--root <path>] [--format text|json] [--large-threshold <lines>]",
    "",
    "Checks FossilSense architecture dependency boundaries and reports rule, severity, file, and allowlist reason.",
  ].join("\n");
}

function toRepoPath(root, filePath) {
  return path.relative(root, filePath).replace(/\\/g, "/");
}

function listFiles(dir, predicate, out = []) {
  if (!fs.existsSync(dir)) {
    return out;
  }

  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "target" || entry.name === "node_modules" || entry.name === ".git") {
        continue;
      }
      listFiles(fullPath, predicate, out);
    } else if (entry.isFile() && predicate(fullPath)) {
      out.push(fullPath);
    }
  }
  return out;
}

function stripBlockComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, "");
}

function stripLineComments(text) {
  return text
    .split(/\r?\n/)
    .map((line) => line.replace(/\/\/.*$/, ""))
    .join("\n");
}

function stripCfgTestSections(text) {
  let result = "";
  let index = 0;
  const marker = "#[cfg(test)]";

  while (index < text.length) {
    const markerIndex = text.indexOf(marker, index);
    if (markerIndex === -1) {
      result += text.slice(index);
      break;
    }

    result += text.slice(index, markerIndex);
    const modIndex = text.indexOf("mod tests", markerIndex);
    const braceIndex = modIndex === -1 ? -1 : text.indexOf("{", modIndex);
    if (modIndex === -1 || braceIndex === -1) {
      index = markerIndex + marker.length;
      continue;
    }

    let depth = 0;
    let end = braceIndex;
    for (; end < text.length; end += 1) {
      if (text[end] === "{") {
        depth += 1;
      } else if (text[end] === "}") {
        depth -= 1;
        if (depth === 0) {
          end += 1;
          break;
        }
      }
    }
    index = end;
  }

  return result;
}

function scanText(text) {
  return stripLineComments(stripBlockComments(stripCfgTestSections(text)));
}

function isRustSource(relPath) {
  return relPath.endsWith(".rs");
}

function isServerBoundary(relPath) {
  return relPath === "crates/fossilsense/src/server.rs" || relPath.startsWith("crates/fossilsense/src/server/");
}

function isStoreBoundary(relPath) {
  return relPath === "crates/fossilsense/src/store.rs" || relPath.startsWith("crates/fossilsense/src/store/");
}

function isOrdinaryCompletionService(relPath) {
  return relPath === "crates/fossilsense/src/completion/ordinary_service.rs";
}

function isModule(relPath, moduleName) {
  return (
    relPath === `crates/fossilsense/src/${moduleName}.rs` ||
    relPath.startsWith(`crates/fossilsense/src/${moduleName}/`)
  );
}

function usesCrateModule(text, moduleName) {
  const direct = new RegExp(`\\bcrate\\s*::\\s*${moduleName}\\b`);
  const grouped = new RegExp(`\\bcrate\\s*::\\s*\\{[^}]*\\b${moduleName}\\b`, "s");
  return direct.test(text) || grouped.test(text);
}

function addFinding(findings, severity, rule, file, detail) {
  findings.push({ severity, rule, file, detail, allowlistReason: null });
}

function applyAllowlist(findings, allowlist = DEFAULT_ALLOWLIST) {
  const byKey = new Map(allowlist.map((entry) => [`${entry.rule}\0${entry.file}`, entry.reason]));
  for (const finding of findings) {
    const reason = byKey.get(`${finding.rule}\0${finding.file}`);
    if (reason) {
      finding.allowlistReason = reason;
    }
  }
}

function checkCoreDirection(findings, relPath, text) {
  const directionRules = [];

  if (isModule(relPath, "parser")) {
    directionRules.push({
      owner: "parser",
      forbidden: ["store", "server", "indexer"],
      detail: "parser must not depend on store/server/indexer details",
    });
  }

  if (isModule(relPath, "resolver")) {
    directionRules.push({
      owner: "resolver",
      forbidden: ["parser", "store", "server", "indexer"],
      detail: "resolver must not depend on parser/store/server/indexer details",
    });
  }

  if (isModule(relPath, "model")) {
    directionRules.push({
      owner: "model",
      forbidden: ["store", "server", "indexer"],
      detail: "model must not depend on store/server/indexer details",
    });
  }

  if (isModule(relPath, "store")) {
    directionRules.push({
      owner: "store",
      forbidden: ["server"],
      detail: "store must not depend on server handler details",
    });
  }

  for (const rule of directionRules) {
    for (const moduleName of rule.forbidden) {
      if (usesCrateModule(text, moduleName)) {
        addFinding(
          findings,
          "ERROR",
          RULES.coreDirection,
          relPath,
          `${rule.detail}: crate::${moduleName}`
        );
      }
    }
  }
}

function collectFindings(root, options = {}) {
  const sourceRoots = [
    path.join(root, "crates", "fossilsense", "src"),
    path.join(root, "extensions", "vscode", "src"),
  ];
  const sourceFiles = sourceRoots
    .flatMap((sourceRoot) => listFiles(sourceRoot, (filePath) => /\.(rs|ts)$/.test(filePath)))
    .sort();
  const findings = [];
  const largeThreshold = options.largeThreshold ?? DEFAULT_LARGE_THRESHOLD;

  for (const filePath of sourceFiles) {
    const relPath = toRepoPath(root, filePath);
    const raw = fs.readFileSync(filePath, "utf8");
    const lines = raw.length === 0 ? [] : raw.split(/\r?\n/);
    if (lines.length > 0 && lines[lines.length - 1] === "") {
      lines.pop();
    }
    const lineCount = lines.length;

    if (lineCount > largeThreshold) {
      addFinding(
        findings,
        "WARN",
        RULES.largeFile,
        relPath,
        `${lineCount} lines exceeds warning threshold ${largeThreshold}`
      );
    }

    if (!isRustSource(relPath)) {
      continue;
    }

    const text = scanText(raw);
    if (/\btower_lsp\b/.test(text)) {
      if (isOrdinaryCompletionService(relPath)) {
        addFinding(
          findings,
          "ERROR",
          RULES.ordinaryCompletionServiceLspBoundary,
          relPath,
          "ordinary completion service must stay protocol-neutral and must not import tower_lsp"
        );
      } else if (!isServerBoundary(relPath)) {
        addFinding(
          findings,
          "ERROR",
          RULES.lspBoundary,
          relPath,
          "tower_lsp usage is limited to server and LSP adapter boundaries"
        );
      }
    }

    if (/\brusqlite\b/.test(text) && !isStoreBoundary(relPath)) {
      addFinding(
        findings,
        "ERROR",
        RULES.sqliteBoundary,
        relPath,
        "rusqlite usage is limited to store/persistence modules"
      );
    }

    checkCoreDirection(findings, relPath, text);
  }

  applyAllowlist(findings, options.allowlist);
  findings.sort((a, b) => {
    const rule = a.rule.localeCompare(b.rule);
    if (rule !== 0) return rule;
    const file = a.file.localeCompare(b.file);
    if (file !== 0) return file;
    return a.detail.localeCompare(b.detail);
  });
  return findings;
}

function statusOf(finding) {
  if (finding.allowlistReason) {
    return "ALLOWLISTED";
  }
  return finding.severity === "WARN" ? "WARN" : "FAIL";
}

function summarize(findings) {
  return findings.reduce(
    (summary, finding) => {
      const status = statusOf(finding);
      if (status === "FAIL") summary.fail += 1;
      if (status === "WARN") summary.warn += 1;
      if (status === "ALLOWLISTED") summary.allowlisted += 1;
      return summary;
    },
    { fail: 0, warn: 0, allowlisted: 0 }
  );
}

function formatText(findings) {
  const lines = ["Architecture fitness report", "status severity rule file detail allowlist"];
  for (const finding of findings) {
    lines.push(
      [
        statusOf(finding),
        finding.severity,
        finding.rule,
        finding.file,
        finding.detail,
        finding.allowlistReason ?? "-",
      ].join(" ")
    );
  }
  const summary = summarize(findings);
  lines.push(`Summary: fail=${summary.fail} warn=${summary.warn} allowlisted=${summary.allowlisted}`);
  return `${lines.join("\n")}\n`;
}

function formatJson(findings) {
  return `${JSON.stringify({ findings, summary: summarize(findings) }, null, 2)}\n`;
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

  const findings = collectFindings(args.root, { largeThreshold: args.largeThreshold });
  process.stdout.write(args.format === "json" ? formatJson(findings) : formatText(findings));
  return summarize(findings).fail > 0 ? 1 : 0;
}

if (require.main === module) {
  process.exitCode = main();
}

module.exports = {
  collectFindings,
  formatText,
  summarize,
};
