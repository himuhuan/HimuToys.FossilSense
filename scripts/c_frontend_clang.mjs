import assert from 'node:assert/strict';
import { resolve } from 'node:path';

const normalizedPath = file => {
  const value = resolve(file).replaceAll('\\', '/');
  return process.platform === 'win32' ? value.toLowerCase() : value;
};
const location = value => value?.expansionLoc ?? value;

export function sourceRange(source, start, end) {
  assert.ok(Number.isInteger(start) && Number.isInteger(end) && start >= 0 && end >= start && end <= source.length);
  const position = offset => {
    const prefix = source.subarray(0, offset).toString('utf8');
    return { line: prefix.split('\n').length - 1, character: prefix.slice(prefix.lastIndexOf('\n') + 1).length };
  };
  return { start: position(start), end: position(end), startByte: start, endByte: end };
}

// Clang's per-declarator span omits the shared trailing declarators and ';'.
// Normalize only source extents here; declaration identity and kind come from
// Clang. Quotes/comments and nested delimiters cannot end the statement early.
function statementEnd(source, start) {
  let parentheses = 0, brackets = 0, braces = 0, quote = 0, comment = 0;
  for (let index = start; index < Math.min(source.length, start + 65536); index++) {
    const byte = source[index], next = source[index + 1];
    if (comment === 1) { if (byte === 10) comment = 0; continue; }
    if (comment === 2) { if (byte === 42 && next === 47) { comment = 0; index++; } continue; }
    if (quote) { if (byte === 92) index++; else if (byte === quote) quote = 0; continue; }
    if (byte === 47 && next === 47) { comment = 1; index++; continue; }
    if (byte === 47 && next === 42) { comment = 2; index++; continue; }
    if (byte === 34 || byte === 39) { quote = byte; continue; }
    if (byte === 40) parentheses++;
    else if (byte === 41) parentheses--;
    else if (byte === 91) brackets++;
    else if (byte === 93) brackets--;
    else if (byte === 123) braces++;
    else if (byte === 125) braces--;
    else if (byte === 59 && parentheses === 0 && brackets === 0 && braces === 0) return index + 1;
  }
  throw new Error('Clang declaration exceeds bounded statement normalization');
}

export function extractClang(ast, source, file, unsupportedRanges) {
  const target = normalizedPath(file);
  const observations = [], excluded = [];
  const stack = [{ node: ast, file, owner: null, function: null, depth: 0 }];
  let visits = 0;
  while (stack.length) {
    assert.ok(++visits <= 100000, 'Clang AST node limit');
    const context = stack.pop();
    const node = context.node;
    assert.ok(context.depth <= 128, 'Clang AST depth limit');
    const loc = location(node.loc);
    const currentFile = loc?.file ?? context.file;
    const inSource = Number.isInteger(loc?.offset) && normalizedPath(currentFile) === target;
    const implicit = node.isImplicit === true;
    let childOwner = context.owner, childFunction = context.function;
    let kind = null, role = 'definition', owner = context.owner;
    let scope = 'persistent', namespace = null, declarationRange = null;
    const body = node.inner?.find(child => child.kind === 'CompoundStmt');
    if (node.kind === 'FunctionDecl') childFunction = { name: node.name, defined: !!body };
    if (node.kind === 'RecordDecl' && node.name) childOwner = node.name;
    if (inSource && !implicit && node.name) {
      const local = context.function?.defined === true;
      if (node.kind === 'FunctionDecl') { kind = 'function'; role = body ? 'definition' : 'declaration'; }
      else if (node.kind === 'VarDecl') {
        kind = local ? 'local_variable' : 'object';
        role = local || node.init ? 'definition' : node.storageClass === 'extern' ? 'declaration' : 'tentative_definition';
      } else if (node.kind === 'TypedefDecl') kind = local ? 'local_type' : 'alias';
      else if (node.kind === 'RecordDecl' || node.kind === 'EnumDecl') {
        kind = local ? 'local_type' : 'type';
        role = local || node.completeDefinition || node.inner?.some(child => child.kind === 'EnumConstantDecl') ? 'definition' : 'declaration';
        if (local) namespace = 'tag';
      } else if (node.kind === 'EnumConstantDecl') kind = local ? 'local_constant' : 'enum_constant';
      else if (node.kind === 'ParmVarDecl') {
        if (local) kind = 'parameter';
        else excluded.push({ name: node.name, reason: 'prototype_parameter_scope' });
      } else if (node.kind === 'FieldDecl') {
        if (local) {
          assert.ok(unsupportedRanges.some(range => range.startByte <= loc.offset && loc.offset < range.endByte),
            'a local field may be omitted only inside an explicit unsupported region');
          excluded.push({ name: node.name, reason: 'scope_not_supported', offset: loc.offset });
        } else kind = 'field';
      } else if (node.kind?.endsWith('Decl')) throw new Error('Unsupported source declaration kind: ' + node.kind);
      if (kind) {
        const end = loc.offset + loc.tokLen;
        assert.equal(source.subarray(loc.offset, end).toString('utf8'), node.name, 'Clang name must match original source');
        const nameRange = sourceRange(source, loc.offset, end);
        if (local && node.kind !== 'FunctionDecl') {
          scope = 'request_local'; owner = context.function.name; namespace ??= 'ordinary';
        } else if (kind !== 'field') {
          const begin = location(node.range?.begin)?.offset;
          assert.ok(Number.isInteger(begin), 'source declaration beginning missing');
          if ((kind === 'type' && role === 'declaration') || node.kind === 'EnumDecl' || kind === 'enum_constant') {
            declarationRange = nameRange;
          } else if (body) {
            let end = location(body.range?.begin)?.offset;
            assert.ok(Number.isInteger(end), 'function body beginning missing');
            while (end > begin && [9, 10, 13, 32].includes(source[end - 1])) end--;
            declarationRange = sourceRange(source, begin, end);
          } else declarationRange = sourceRange(source, begin, statementEnd(source, begin));
        }
        observations.push({ name: node.name, kind, role, owner, scope, namespace, guard: null, nameRange, declarationRange });
      }
    } else if (node.kind?.endsWith('Decl') && node.name) excluded.push({ name: node.name,
      reason: implicit ? 'implicit' : 'external_or_no_source_location' });
    const children = node.inner ?? [];
    for (let index = children.length - 1; index >= 0; index--) stack.push({
      node: children[index], file: currentFile, owner: childOwner, function: childFunction, depth: context.depth + 1,
    });
  }
  return { observations, excluded, visits };
}
