import assert from 'node:assert/strict';
import test from 'node:test';

import { parseAndValidateResult } from '../src/result.mjs';

const documents = [{ path: 'src/a.js', lineCount: 3 }];

test('accepts findings grounded in loaded documents', () => {
  const result = parseAndValidateResult(JSON.stringify({
    answer: 'Found it',
    findings: [{ path: 'src/a.js', startLine: 1, endLine: 2, summary: 'Relevant code' }],
    uncertainties: []
  }), documents);
  assert.equal(result.findings[0].endLine, 2);
  assert.equal(result.trust, 'untrusted-model-output-with-validated-source-references');
});

test('rejects hallucinated paths', () => {
  assert.throws(() => parseAndValidateResult(JSON.stringify({
    answer: 'No',
    findings: [{ path: 'src/missing.js', startLine: 1, endLine: 1, summary: 'Fake' }],
    uncertainties: []
  }), documents), /unknown path/u);
});

test('rejects line ranges outside the document', () => {
  assert.throws(() => parseAndValidateResult(JSON.stringify({
    answer: 'No',
    findings: [{ path: 'src/a.js', startLine: 2, endLine: 4, summary: 'Too far' }],
    uncertainties: []
  }), documents), /invalid line range/u);
});
