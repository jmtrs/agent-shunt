import assert from 'node:assert/strict';
import test from 'node:test';

import { scan } from '../src/scan.mjs';

test('dry run never resolves credentials or calls provider', async () => {
  let credentialCalled = false;
  let providerCalled = false;
  const result = await scan({ question: 'Where?', paths: ['a.js'], cwd: '/repo', dryRun: true }, {
    loadConfig: async () => ({ model: 'cheap', maxFiles: 2, maxFileBytes: 100, maxTotalBytes: 200 }),
    loadDocuments: async () => ({ documents: [{ path: 'a.js' }], totalBytes: 42 }),
    resolveApiKey: async () => { credentialCalled = true; },
    callOpenRouter: async () => { providerCalled = true; }
  });
  assert.equal(result.dryRun, true);
  assert.equal(credentialCalled, false);
  assert.equal(providerCalled, false);
});

test('scan validates provider output and adds usage', async () => {
  const result = await scan({ question: 'Where?', paths: ['a.js'], cwd: '/repo' }, {
    loadConfig: async () => ({ model: 'cheap', baseUrl: 'https://example.com', timeoutMs: 100, maxOutputTokens: 50, maxFiles: 2, maxFileBytes: 100, maxTotalBytes: 200 }),
    loadDocuments: async () => ({ documents: [{ path: 'a.js', lineCount: 2, numberedContent: '1: x\n2: y' }], totalBytes: 3 }),
    resolveApiKey: async () => ({ apiKey: 'secret' }),
    callOpenRouter: async () => ({
      content: JSON.stringify({ answer: 'At x', findings: [{ path: 'a.js', startLine: 1, endLine: 1, summary: 'x' }], uncertainties: [] }),
      responseModel: 'cheap',
      usage: { total_tokens: 10 }
    })
  });
  assert.equal(result.usage.total_tokens, 10);
  assert.equal(result.findings[0].path, 'a.js');
});
