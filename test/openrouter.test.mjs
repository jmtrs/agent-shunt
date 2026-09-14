import assert from 'node:assert/strict';
import test from 'node:test';

import { callOpenRouter } from '../src/openrouter.mjs';

test('enforces private routing, structured output, and output limits', async () => {
  let request;
  const response = await callOpenRouter({
    apiKey: 'secret',
    baseUrl: 'https://example.com/v1',
    model: 'paid/model',
    question: 'Inspect',
    documents: [{ path: 'a.js', numberedContent: '1: x' }],
    timeoutMs: 100,
    maxOutputTokens: 123,
    maxResponseBytes: 1000,
    maxRequestBytes: 10_000,
    fetchImpl: async (url, options) => {
      request = { url, options, body: JSON.parse(options.body) };
      return new Response(JSON.stringify({ choices: [{ message: { content: '{}' } }] }), { status: 200 });
    }
  });
  assert.equal(response.content, '{}');
  assert.equal(request.body.provider.zdr, true);
  assert.equal(request.body.provider.data_collection, 'deny');
  assert.equal(request.body.provider.require_parameters, true);
  assert.equal(request.body.max_completion_tokens, 123);
  assert.equal(request.options.headers.authorization, 'Bearer secret');
});

test('blocks free model routes before network access', async () => {
  await assert.rejects(callOpenRouter({
    apiKey: 'secret',
    baseUrl: 'https://example.com/v1',
    model: 'vendor/model:free',
    question: 'Inspect',
    documents: [],
    timeoutMs: 100,
    maxOutputTokens: 10,
    maxResponseBytes: 1000,
    maxRequestBytes: 10_000,
    fetchImpl: async () => { throw new Error('must not be called'); }
  }), /free model routes are disabled/u);
});

test('rejects oversized HTTP responses while streaming', async () => {
  await assert.rejects(callOpenRouter({
    apiKey: 'secret',
    baseUrl: 'https://openrouter.ai/api/v1',
    model: 'paid/model',
    question: 'Inspect',
    documents: [],
    timeoutMs: 100,
    maxOutputTokens: 10,
    maxResponseBytes: 20,
    maxRequestBytes: 10_000,
    fetchImpl: async () => new Response('x'.repeat(21), { status: 200 })
  }), /response exceeds 20 bytes/u);
});
